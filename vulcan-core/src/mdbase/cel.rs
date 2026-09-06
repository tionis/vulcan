use super::{MdbaseDiagnostic, MdbaseDiagnosticLevel, MdbaseRecordDocument};
use cel_interpreter::extractors::This;
use cel_interpreter::{Context, ExecutionError, Program, Value};
use cel_parser::ast::{EntryExpr, Expr, IdedEntryExpr, IdedExpr};
use chrono::{DateTime, FixedOffset, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

pub const MDBASE_CEL_RESERVED_BINDINGS: [&str; 16] = [
    "record",
    "raw",
    "present",
    "file",
    "note",
    "projection",
    "this",
    "values",
    "old",
    "operation",
    "event",
    "workflow",
    "trigger",
    "steps",
    "vars",
    "item",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseCelLimits {
    pub max_source_bytes: usize,
    pub max_lexical_tokens: usize,
    pub max_ast_depth: usize,
    pub max_ast_nodes: usize,
    pub max_evaluation_work: usize,
    pub max_value_bytes: usize,
    pub max_value_nodes: usize,
    pub max_collection_items: usize,
    pub max_link_traversal: usize,
}

impl Default for MdbaseCelLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 64 * 1024,
            max_lexical_tokens: 1_024,
            max_ast_depth: 100,
            max_ast_nodes: 1_024,
            max_evaluation_work: 100_000,
            max_value_bytes: 1024 * 1024,
            max_value_nodes: 50_000,
            max_collection_items: 2_048,
            max_link_traversal: 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MdbaseCelProgramStats {
    pub ast_depth: usize,
    pub ast_nodes: usize,
    pub estimated_work: usize,
}

#[derive(Debug)]
pub struct MdbaseCelProgram {
    source: String,
    program: Program,
    stats: MdbaseCelProgramStats,
    projection_dependencies: BTreeSet<String>,
}

impl MdbaseCelProgram {
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub const fn stats(&self) -> MdbaseCelProgramStats {
        self.stats
    }

    pub fn projection_dependencies(&self) -> impl Iterator<Item = &str> {
        self.projection_dependencies.iter().map(String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdbaseCelError {
    pub code: &'static str,
    pub message: String,
}

impl MdbaseCelError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn limit(name: &str, actual: usize, maximum: usize) -> Self {
        Self::new(
            "expression_limit_exceeded",
            format!("CEL {name} is {actual}, exceeding the limit of {maximum}"),
        )
    }
}

impl Display for MdbaseCelError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MdbaseCelError {}

#[derive(Debug, Clone)]
pub struct MdbaseCelEngine {
    limits: MdbaseCelLimits,
}

impl Default for MdbaseCelEngine {
    fn default() -> Self {
        Self::new(MdbaseCelLimits::default())
    }
}

impl MdbaseCelEngine {
    #[must_use]
    pub const fn new(limits: MdbaseCelLimits) -> Self {
        Self { limits }
    }

    #[must_use]
    pub const fn limits(&self) -> MdbaseCelLimits {
        self.limits
    }

    pub fn compile(&self, source: &str) -> Result<MdbaseCelProgram, MdbaseCelError> {
        if source.len() > self.limits.max_source_bytes {
            return Err(MdbaseCelError::limit(
                "source size",
                source.len(),
                self.limits.max_source_bytes,
            ));
        }
        let lexical = inspect_source(source)?;
        if lexical.tokens > self.limits.max_lexical_tokens {
            return Err(MdbaseCelError::limit(
                "lexical token count",
                lexical.tokens,
                self.limits.max_lexical_tokens,
            ));
        }
        if lexical.depth > self.limits.max_ast_depth {
            return Err(MdbaseCelError::limit(
                "syntactic depth",
                lexical.depth,
                self.limits.max_ast_depth,
            ));
        }

        let expression = catch_unwind(AssertUnwindSafe(|| cel_parser::Parser::new().parse(source)))
            .map_err(|_| {
                MdbaseCelError::new(
                    "expression_compile_error",
                    "CEL parser rejected malformed input",
                )
            })?
            .map_err(|error| MdbaseCelError::new("expression_compile_error", error.to_string()))?;
        let stats = inspect_ast(&expression, &self.limits)?;
        let mut projection_dependencies = BTreeSet::new();
        collect_projection_dependencies(&expression, &mut projection_dependencies);
        let program = catch_unwind(AssertUnwindSafe(|| Program::compile(source)))
            .map_err(|_| {
                MdbaseCelError::new(
                    "expression_compile_error",
                    "CEL compiler rejected malformed input",
                )
            })?
            .map_err(|error| MdbaseCelError::new("expression_compile_error", error.to_string()))?;
        Ok(MdbaseCelProgram {
            source: source.to_string(),
            program,
            stats,
            projection_dependencies,
        })
    }

    pub fn evaluate(
        &self,
        program: &MdbaseCelProgram,
        bindings: &BTreeMap<String, serde_json::Value>,
    ) -> Result<serde_json::Value, MdbaseCelError> {
        inspect_value(
            &serde_json::to_value(bindings).map_err(|error| {
                MdbaseCelError::new("expression_binding_error", error.to_string())
            })?,
            &self.limits,
            "input",
        )?;
        let mut context = Context::default();
        add_json_bindings(&mut context, bindings)?;
        let value = program.program.execute(&context).map_err(|error| {
            MdbaseCelError::new("expression_evaluation_error", error.to_string())
        })?;
        let value = value
            .json()
            .map_err(|error| MdbaseCelError::new("expression_result_error", error.to_string()))?;
        inspect_value(&value, &self.limits, "output")?;
        Ok(value)
    }

    pub fn evaluate_context(
        &self,
        program: &MdbaseCelProgram,
        evaluation: &MdbaseCelContext,
    ) -> Result<MdbaseCelEvaluation, MdbaseCelError> {
        evaluation.validate_program(program)?;
        inspect_value(
            &serde_json::to_value(&evaluation.bindings).map_err(|error| {
                MdbaseCelError::new("expression_binding_error", error.to_string())
            })?,
            &self.limits,
            "input",
        )?;

        let mut context = Context::default();
        add_json_bindings(&mut context, &evaluation.bindings)?;
        add_mdbase_functions(&mut context, &evaluation.clock);
        match program.program.execute(&context) {
            Ok(value) => {
                let value = value.json().map_err(|error| {
                    MdbaseCelError::new("expression_result_error", error.to_string())
                })?;
                inspect_value(&value, &self.limits, "output")?;
                Ok(MdbaseCelEvaluation {
                    value,
                    diagnostics: Vec::new(),
                })
            }
            Err(error) if evaluation.kind.yields_null_on_error() => Ok(MdbaseCelEvaluation {
                value: serde_json::Value::Null,
                diagnostics: vec![evaluation.diagnostic(
                    "expression_evaluation_error",
                    error.to_string(),
                    program.source(),
                )],
            }),
            Err(error) => Err(MdbaseCelError::new(
                evaluation.kind.failure_code(),
                format!("{} CEL expression failed: {error}", evaluation.kind.name()),
            )),
        }
    }

    pub fn evaluate_expression_value(
        &self,
        template: &serde_json::Value,
        evaluation: &MdbaseCelContext,
    ) -> Result<MdbaseCelEvaluation, MdbaseCelError> {
        let mut diagnostics = Vec::new();
        let value = self.evaluate_expression_value_inner(template, evaluation, &mut diagnostics)?;
        inspect_value(&value, &self.limits, "output")?;
        Ok(MdbaseCelEvaluation { value, diagnostics })
    }

    fn evaluate_expression_value_inner(
        &self,
        template: &serde_json::Value,
        evaluation: &MdbaseCelContext,
        diagnostics: &mut Vec<MdbaseDiagnostic>,
    ) -> Result<serde_json::Value, MdbaseCelError> {
        match template {
            serde_json::Value::Object(object)
                if object.len() == 1 && object.contains_key("$expr") =>
            {
                let source = object["$expr"].as_str().ok_or_else(|| {
                    MdbaseCelError::new(
                        "expression_compile_error",
                        "an expression object's `$expr` member must be a string",
                    )
                })?;
                let result = self.evaluate_context(&self.compile(source)?, evaluation)?;
                diagnostics.extend(result.diagnostics);
                Ok(result.value)
            }
            serde_json::Value::Object(object) => object
                .iter()
                .map(|(name, value)| {
                    self.evaluate_expression_value_inner(value, evaluation, diagnostics)
                        .map(|value| (name.clone(), value))
                })
                .collect::<Result<serde_json::Map<_, _>, _>>()
                .map(serde_json::Value::Object),
            serde_json::Value::Array(values) => values
                .iter()
                .map(|value| self.evaluate_expression_value_inner(value, evaluation, diagnostics))
                .collect::<Result<Vec<_>, _>>()
                .map(serde_json::Value::Array),
            value => Ok(value.clone()),
        }
    }

    #[must_use]
    pub const fn link_budget(&self) -> MdbaseCelLinkBudget {
        MdbaseCelLinkBudget {
            remaining: self.limits.max_link_traversal,
        }
    }
}

fn collect_projection_dependencies(expression: &IdedExpr, dependencies: &mut BTreeSet<String>) {
    match &expression.expr {
        Expr::Select(select) => {
            if matches!(&select.operand.expr, Expr::Ident(name) if name == "projection") {
                dependencies.insert(select.field.clone());
            }
            collect_projection_dependencies(&select.operand, dependencies);
        }
        Expr::Call(call) => {
            if let Some(target) = &call.target {
                collect_projection_dependencies(target, dependencies);
            }
            for argument in &call.args {
                collect_projection_dependencies(argument, dependencies);
            }
        }
        Expr::Comprehension(comprehension) => {
            for child in [
                &comprehension.iter_range,
                &comprehension.accu_init,
                &comprehension.loop_cond,
                &comprehension.loop_step,
                &comprehension.result,
            ] {
                collect_projection_dependencies(child, dependencies);
            }
        }
        Expr::List(list) => {
            for child in &list.elements {
                collect_projection_dependencies(child, dependencies);
            }
        }
        Expr::Map(map) => {
            for child in entry_children(&map.entries) {
                collect_projection_dependencies(child, dependencies);
            }
        }
        Expr::Struct(map) => {
            for child in entry_children(&map.entries) {
                collect_projection_dependencies(child, dependencies);
            }
        }
        Expr::Ident(_) | Expr::Literal(_) | Expr::Unspecified => {}
    }
}

fn add_json_bindings(
    context: &mut Context<'_>,
    bindings: &BTreeMap<String, serde_json::Value>,
) -> Result<(), MdbaseCelError> {
    for (name, value) in bindings {
        context
            .add_variable(name, value)
            .map_err(|error| MdbaseCelError::new("expression_binding_error", error.to_string()))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MdbaseCelContextKind {
    InferredMatch,
    QueryFilter,
    QueryProjection,
    QuerySummary,
    LifecycleGuard,
    WorkflowTrigger,
    WorkflowCondition,
    WorkflowStep,
    WorkflowRunPolicy,
}

impl MdbaseCelContextKind {
    const fn name(self) -> &'static str {
        match self {
            Self::InferredMatch => "inferred_match",
            Self::QueryFilter => "query_filter",
            Self::QueryProjection => "query_projection",
            Self::QuerySummary => "query_summary",
            Self::LifecycleGuard => "lifecycle_guard",
            Self::WorkflowTrigger => "workflow_trigger",
            Self::WorkflowCondition => "workflow_condition",
            Self::WorkflowStep => "workflow_step",
            Self::WorkflowRunPolicy => "workflow_run_policy",
        }
    }

    const fn yields_null_on_error(self) -> bool {
        matches!(
            self,
            Self::InferredMatch | Self::QueryFilter | Self::QueryProjection | Self::QuerySummary
        )
    }

    const fn failure_code(self) -> &'static str {
        match self {
            Self::LifecycleGuard => "lifecycle_expression_error",
            Self::WorkflowTrigger
            | Self::WorkflowCondition
            | Self::WorkflowStep
            | Self::WorkflowRunPolicy => "runtime_expression_error",
            Self::InferredMatch
            | Self::QueryFilter
            | Self::QueryProjection
            | Self::QuerySummary => "expression_evaluation_error",
        }
    }

    fn available_reserved(self) -> &'static [&'static str] {
        match self {
            Self::InferredMatch => &["record", "raw", "present", "file", "note"],
            Self::QueryFilter | Self::QueryProjection => &[
                "record",
                "raw",
                "present",
                "file",
                "note",
                "projection",
                "this",
            ],
            Self::QuerySummary => &["values"],
            Self::LifecycleGuard => &["record", "raw", "present", "old", "file", "operation"],
            Self::WorkflowTrigger | Self::WorkflowCondition | Self::WorkflowRunPolicy => {
                &["event", "workflow", "trigger", "vars"]
            }
            Self::WorkflowStep => &["event", "workflow", "trigger", "steps", "vars", "item"],
        }
    }
}

#[derive(Debug, Clone)]
pub struct MdbaseCelClock {
    now_utc: DateTime<Utc>,
    timezone: Tz,
}

impl MdbaseCelClock {
    pub fn new(now_utc: DateTime<Utc>, timezone: &str) -> Result<Self, MdbaseCelError> {
        let timezone = timezone.parse::<Tz>().map_err(|_| {
            MdbaseCelError::new(
                "expression_timezone_invalid",
                format!("`{timezone}` is not an IANA timezone identifier"),
            )
        })?;
        Ok(Self { now_utc, timezone })
    }

    #[must_use]
    pub const fn now_utc(&self) -> DateTime<Utc> {
        self.now_utc
    }

    #[must_use]
    pub fn timezone(&self) -> &str {
        self.timezone.name()
    }

    #[must_use]
    pub fn today(&self) -> String {
        self.now_utc
            .with_timezone(&self.timezone)
            .date_naive()
            .format("%Y-%m-%d")
            .to_string()
    }
}

#[derive(Debug, Clone)]
pub struct MdbaseCelContext {
    kind: MdbaseCelContextKind,
    bindings: BTreeMap<String, serde_json::Value>,
    clock: MdbaseCelClock,
    path: Option<String>,
}

impl MdbaseCelContext {
    pub fn query(
        kind: MdbaseCelContextKind,
        record: &MdbaseRecordDocument,
        known_fields: impl IntoIterator<Item = String>,
        projection: serde_json::Value,
        invocation_context: Option<&MdbaseRecordDocument>,
        clock: MdbaseCelClock,
    ) -> Result<Self, MdbaseCelError> {
        if !matches!(
            kind,
            MdbaseCelContextKind::QueryFilter | MdbaseCelContextKind::QueryProjection
        ) {
            return Err(MdbaseCelError::new(
                "expression_context_invalid",
                "query bindings require a query filter or projection context",
            ));
        }
        let known_fields = known_fields.into_iter().collect::<BTreeSet<_>>();
        let mut bindings = record_bindings(record, &known_fields, false);
        bindings.insert("projection".to_string(), projection);
        bindings.insert(
            "this".to_string(),
            invocation_context.map_or(serde_json::Value::Null, |record| {
                invocation_context_value(record, &known_fields)
            }),
        );
        Ok(Self {
            kind,
            bindings,
            clock,
            path: Some(record.path.clone()),
        })
    }

    pub fn inferred_match(
        record: &MdbaseRecordDocument,
        known_fields: impl IntoIterator<Item = String>,
        clock: MdbaseCelClock,
    ) -> Self {
        let known_fields = known_fields.into_iter().collect::<BTreeSet<_>>();
        Self {
            kind: MdbaseCelContextKind::InferredMatch,
            bindings: record_bindings(record, &known_fields, true),
            clock,
            path: Some(record.path.clone()),
        }
    }

    pub fn inferred_candidate(
        path: &str,
        frontmatter: &serde_json::Value,
        body: &str,
        known_fields: impl IntoIterator<Item = String>,
        clock: MdbaseCelClock,
    ) -> Self {
        let known_fields = known_fields.into_iter().collect::<BTreeSet<_>>();
        let raw = materialize_record_fields(frontmatter, &known_fields);
        let present = presence_map(frontmatter, &known_fields);
        let file = candidate_file_value(path, frontmatter, body);
        let mut bindings = BTreeMap::from([
            ("record".to_string(), raw.clone()),
            ("note".to_string(), raw.clone()),
            ("raw".to_string(), raw.clone()),
            (
                "present".to_string(),
                serde_json::json!({"raw": present.clone(), "record": present}),
            ),
            ("file".to_string(), file),
        ]);
        if let Some(fields) = raw.as_object() {
            for (name, value) in fields {
                if !MDBASE_CEL_RESERVED_BINDINGS.contains(&name.as_str()) {
                    bindings.insert(name.clone(), value.clone());
                }
            }
        }
        Self {
            kind: MdbaseCelContextKind::InferredMatch,
            bindings,
            clock,
            path: Some(path.to_string()),
        }
    }

    pub fn system(
        kind: MdbaseCelContextKind,
        bindings: BTreeMap<String, serde_json::Value>,
        clock: MdbaseCelClock,
    ) -> Result<Self, MdbaseCelError> {
        for name in bindings.keys() {
            if MDBASE_CEL_RESERVED_BINDINGS.contains(&name.as_str())
                && !kind.available_reserved().contains(&name.as_str())
            {
                return Err(MdbaseCelError::new(
                    "expression_context_binding_unavailable",
                    format!("`{name}` is unavailable in the {} context", kind.name()),
                ));
            }
        }
        Ok(Self {
            kind,
            bindings,
            clock,
            path: None,
        })
    }

    fn validate_program(&self, program: &MdbaseCelProgram) -> Result<(), MdbaseCelError> {
        let references = program.program.references();
        for name in MDBASE_CEL_RESERVED_BINDINGS {
            if references.has_variable(name) && !self.kind.available_reserved().contains(&name) {
                return Err(MdbaseCelError::new(
                    "expression_context_binding_unavailable",
                    format!(
                        "`{name}` is unavailable in the {} context",
                        self.kind.name()
                    ),
                ));
            }
        }
        Ok(())
    }

    fn diagnostic(&self, code: &str, message: String, source: &str) -> MdbaseDiagnostic {
        MdbaseDiagnostic {
            severity: MdbaseDiagnosticLevel::Error,
            code: code.to_string(),
            message,
            path: self.path.clone(),
            field: None,
            type_name: None,
            schema_location: None,
            details: Some(serde_json::json!({
                "context": self.kind.name(),
                "source": source,
            })),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseCelEvaluation {
    pub value: serde_json::Value,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

fn record_bindings(
    record: &MdbaseRecordDocument,
    known_fields: &BTreeSet<String>,
    raw_top_level: bool,
) -> BTreeMap<String, serde_json::Value> {
    let raw = materialize_record_fields(&record.frontmatter, known_fields);
    let effective = if raw_top_level {
        raw.clone()
    } else {
        materialize_record_fields(&record.effective_frontmatter, known_fields)
    };
    let present = serde_json::json!({
        "raw": presence_map(&record.frontmatter, known_fields),
        "record": if raw_top_level {
            presence_map(&record.frontmatter, known_fields)
        } else {
            presence_map(&record.effective_frontmatter, known_fields)
        },
    });
    let file = file_value(record);
    let mut bindings = BTreeMap::from([
        ("record".to_string(), effective.clone()),
        ("note".to_string(), effective.clone()),
        ("raw".to_string(), raw),
        ("present".to_string(), present),
        ("file".to_string(), file),
    ]);
    if let Some(fields) = effective.as_object() {
        for (name, value) in fields {
            if !MDBASE_CEL_RESERVED_BINDINGS.contains(&name.as_str()) {
                bindings.insert(name.clone(), value.clone());
            }
        }
    }
    bindings
}

fn invocation_context_value(
    record: &MdbaseRecordDocument,
    known_fields: &BTreeSet<String>,
) -> serde_json::Value {
    let mut value = materialize_record_fields(&record.effective_frontmatter, known_fields)
        .as_object()
        .cloned()
        .unwrap_or_default();
    let effective = materialize_record_fields(&record.effective_frontmatter, known_fields);
    value.insert("record".to_string(), effective.clone());
    value.insert("note".to_string(), effective);
    value.insert(
        "raw".to_string(),
        materialize_record_fields(&record.frontmatter, known_fields),
    );
    value.insert(
        "present".to_string(),
        serde_json::json!({
            "raw": presence_map(&record.frontmatter, known_fields),
            "record": presence_map(&record.effective_frontmatter, known_fields),
        }),
    );
    value.insert("file".to_string(), file_value(record));
    serde_json::Value::Object(value)
}

fn materialize_record_fields(
    value: &serde_json::Value,
    known_fields: &BTreeSet<String>,
) -> serde_json::Value {
    let mut fields = value.as_object().cloned().unwrap_or_default();
    for field in known_fields {
        fields
            .entry(field.clone())
            .or_insert(serde_json::Value::Null);
    }
    serde_json::Value::Object(fields)
}

fn presence_map(value: &serde_json::Value, known_fields: &BTreeSet<String>) -> serde_json::Value {
    let present = value.as_object();
    let mut fields = known_fields
        .iter()
        .map(|field| {
            (
                field.clone(),
                serde_json::Value::Bool(present.is_some_and(|value| value.contains_key(field))),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    if let Some(present) = present {
        for field in present.keys() {
            fields.insert(field.clone(), serde_json::Value::Bool(true));
        }
    }
    serde_json::Value::Object(fields)
}

fn file_value(record: &MdbaseRecordDocument) -> serde_json::Value {
    let tags = collect_tags(&record.frontmatter, &record.body);
    serde_json::json!({
        "path": record.path,
        "name": record.file.name,
        "basename": record.file.basename,
        "ext": record.file.ext,
        "folder": record.file.folder,
        "size": record.file.size,
        "mtime": record.file.mtime,
        "ctime": record.file.ctime,
        "body": record.body,
        "tags": tags,
        "links": [],
        "embeds": [],
    })
}

fn candidate_file_value(
    path: &str,
    frontmatter: &serde_json::Value,
    body: &str,
) -> serde_json::Value {
    let (folder, name) = path
        .rsplit_once('/')
        .map_or(("", path), |(folder, name)| (folder, name));
    let (basename, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    serde_json::json!({
        "path": path,
        "name": name,
        "basename": basename,
        "ext": ext,
        "folder": folder,
        "size": null,
        "mtime": null,
        "ctime": null,
        "body": body,
        "tags": collect_tags(frontmatter, body),
        "links": [],
        "embeds": [],
    })
}

fn collect_tags(frontmatter: &serde_json::Value, body: &str) -> BTreeSet<String> {
    let mut tags = BTreeSet::new();
    if let Some(value) = frontmatter.get("tags") {
        match value {
            serde_json::Value::String(value) => {
                tags.extend(value.split([',', ' ']).filter_map(normalize_tag));
            }
            serde_json::Value::Array(values) => {
                tags.extend(
                    values
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .filter_map(normalize_tag),
                );
            }
            _ => {}
        }
    }
    tags.extend(
        crate::parser::parse_document(body, &crate::config::VaultConfig::default())
            .tags
            .into_iter()
            .map(|tag| tag.tag_text),
    );
    tags
}

fn normalize_tag(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches('#');
    (!value.is_empty()).then(|| value.to_string())
}

fn add_mdbase_functions(context: &mut Context<'_>, clock: &MdbaseCelClock) {
    let now: DateTime<FixedOffset> = clock.now_utc.fixed_offset();
    let today = Arc::new(clock.today());
    context.add_function("now", move || now);
    context.add_function("today", move || Arc::clone(&today));
    context.add_function("duration", iso8601_duration);
    context.add_function("inFolder", file_in_folder);
    context.add_function("hasTag", file_has_tag);
}

fn file_in_folder(This(file): This<Value>, folder: Arc<String>) -> Result<bool, ExecutionError> {
    let path = file_member_string(&file, "path", "inFolder")?;
    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    let folder = take_arc_string(folder);
    let folder = folder.trim_matches('/');
    Ok(folder.is_empty() || parent == folder || parent.starts_with(&format!("{folder}/")))
}

fn file_has_tag(This(file): This<Value>, tag: Arc<String>) -> Result<bool, ExecutionError> {
    let Value::Map(file) = file else {
        return Err(ExecutionError::function_error(
            "hasTag",
            "target is not an mdbase file object",
        ));
    };
    let tag = take_arc_string(tag);
    let wanted = tag.trim().trim_start_matches('#');
    let Some(Value::List(tags)) = file.get(&"tags".to_string().into()) else {
        return Ok(false);
    };
    Ok(tags.iter().any(|value| {
        let Value::String(value) = value else {
            return false;
        };
        value.as_str() == wanted || value.starts_with(&format!("{wanted}/"))
    }))
}

fn file_member_string(
    file: &Value,
    member: &str,
    function: &str,
) -> Result<Arc<String>, ExecutionError> {
    let Value::Map(file) = file else {
        return Err(ExecutionError::function_error(
            function,
            "target is not an mdbase file object",
        ));
    };
    match file.get(&member.to_string().into()) {
        Some(Value::String(value)) => Ok(Arc::clone(value)),
        _ => Err(ExecutionError::function_error(
            function,
            format!("file.{member} is not a string"),
        )),
    }
}

fn iso8601_duration(value: Arc<String>) -> Result<chrono::Duration, ExecutionError> {
    let value = take_arc_string(value);
    parse_iso8601_duration(&value)
        .ok_or_else(|| ExecutionError::function_error("duration", "invalid ISO 8601 duration"))
}

fn take_arc_string(value: Arc<String>) -> String {
    Arc::try_unwrap(value).unwrap_or_else(|value| (*value).clone())
}

fn parse_iso8601_duration(source: &str) -> Option<chrono::Duration> {
    let (sign, source) = source
        .strip_prefix('-')
        .map_or((1_i64, source), |rest| (-1, rest));
    let source = source.strip_prefix('P')?;
    let mut in_time = false;
    let mut number_start = 0_usize;
    let mut total_nanos = 0_i128;
    let mut saw_component = false;
    let chars = source.char_indices().collect::<Vec<_>>();
    let mut index = 0_usize;
    while index < chars.len() {
        let (offset, character) = chars[index];
        if character == 'T' {
            if offset != number_start || in_time {
                return None;
            }
            in_time = true;
            number_start = offset + 1;
            index += 1;
            continue;
        }
        if character.is_ascii_digit() || character == '.' {
            index += 1;
            continue;
        }
        if offset == number_start {
            return None;
        }
        let number = &source[number_start..offset];
        let unit_nanos = match (in_time, character) {
            (false, 'W') => 7_i128 * 86_400 * 1_000_000_000,
            (false, 'D') => 86_400_i128 * 1_000_000_000,
            (true, 'H') => 3_600_i128 * 1_000_000_000,
            (true, 'M') => 60_i128 * 1_000_000_000,
            (true, 'S') => 1_000_000_000_i128,
            _ => return None,
        };
        let component = if character == 'S' {
            decimal_seconds_to_nanos(number)?
        } else {
            i128::from(number.parse::<u64>().ok()?).checked_mul(unit_nanos)?
        };
        total_nanos = total_nanos.checked_add(component)?;
        saw_component = true;
        number_start = offset + character.len_utf8();
        index += 1;
    }
    if !saw_component || number_start != source.len() {
        return None;
    }
    let total_nanos = i64::try_from(total_nanos).ok()?.checked_mul(sign)?;
    Some(chrono::Duration::nanoseconds(total_nanos))
}

fn decimal_seconds_to_nanos(source: &str) -> Option<i128> {
    let (seconds, fraction) = source.split_once('.').unwrap_or((source, ""));
    if seconds.is_empty() || fraction.len() > 9 || !fraction.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let seconds = i128::from(seconds.parse::<u64>().ok()?).checked_mul(1_000_000_000)?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        let value = fraction.parse::<u64>().ok()?;
        i128::from(value).checked_mul(10_i128.pow(u32::try_from(9 - fraction.len()).ok()?))?
    };
    seconds.checked_add(fraction)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdbaseCelLinkBudget {
    remaining: usize,
}

impl MdbaseCelLinkBudget {
    pub fn consume(&mut self) -> Result<(), MdbaseCelError> {
        self.remaining = self.remaining.checked_sub(1).ok_or_else(|| {
            MdbaseCelError::new(
                "expression_link_traversal_limit",
                "CEL link traversal budget is exhausted",
            )
        })?;
        Ok(())
    }

    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.remaining
    }
}

#[derive(Debug, Default)]
struct LexicalStats {
    tokens: usize,
    depth: usize,
}

fn inspect_source(source: &str) -> Result<LexicalStats, MdbaseCelError> {
    let mut stats = LexicalStats::default();
    let mut current_depth = 0_usize;
    let mut identifier = false;
    let mut quote = None;
    let mut escaped = false;
    for character in source.chars() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == delimiter {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            stats.tokens = stats.tokens.saturating_add(1);
            identifier = false;
        } else if character.is_alphanumeric() || character == '_' {
            if !identifier {
                stats.tokens = stats.tokens.saturating_add(1);
                identifier = true;
            }
        } else {
            identifier = false;
            if matches!(character, '(' | '[' | '{') {
                current_depth = current_depth.saturating_add(1);
                stats.depth = stats.depth.max(current_depth);
                stats.tokens = stats.tokens.saturating_add(1);
            } else if matches!(character, ')' | ']' | '}') {
                current_depth = current_depth.saturating_sub(1);
                stats.tokens = stats.tokens.saturating_add(1);
            } else if !character.is_whitespace() {
                stats.tokens = stats.tokens.saturating_add(1);
            }
        }
    }
    if quote.is_some() {
        return Err(MdbaseCelError::new(
            "expression_compile_error",
            "unterminated string literal",
        ));
    }
    Ok(stats)
}

fn inspect_ast(
    expression: &IdedExpr,
    limits: &MdbaseCelLimits,
) -> Result<MdbaseCelProgramStats, MdbaseCelError> {
    let (nodes, depth, work) = ast_cost(expression, 1, limits)?;
    if nodes > limits.max_ast_nodes {
        return Err(MdbaseCelError::limit(
            "AST node count",
            nodes,
            limits.max_ast_nodes,
        ));
    }
    if depth > limits.max_ast_depth {
        return Err(MdbaseCelError::limit(
            "AST depth",
            depth,
            limits.max_ast_depth,
        ));
    }
    if work > limits.max_evaluation_work {
        return Err(MdbaseCelError::limit(
            "estimated evaluation work",
            work,
            limits.max_evaluation_work,
        ));
    }
    Ok(MdbaseCelProgramStats {
        ast_depth: depth,
        ast_nodes: nodes,
        estimated_work: work,
    })
}

fn ast_cost(
    expression: &IdedExpr,
    depth: usize,
    limits: &MdbaseCelLimits,
) -> Result<(usize, usize, usize), MdbaseCelError> {
    if depth > limits.max_ast_depth {
        return Err(MdbaseCelError::limit(
            "AST depth",
            depth,
            limits.max_ast_depth,
        ));
    }
    let children = match &expression.expr {
        Expr::Call(call) => call
            .target
            .iter()
            .map(AsRef::as_ref)
            .chain(call.args.iter())
            .collect(),
        Expr::Comprehension(comprehension) => {
            let range = ast_cost(&comprehension.iter_range, depth + 1, limits)?;
            let init = ast_cost(&comprehension.accu_init, depth + 1, limits)?;
            let condition = ast_cost(&comprehension.loop_cond, depth + 1, limits)?;
            let step = ast_cost(&comprehension.loop_step, depth + 1, limits)?;
            let result = ast_cost(&comprehension.result, depth + 1, limits)?;
            let nodes = 1_usize
                .saturating_add(range.0)
                .saturating_add(init.0)
                .saturating_add(condition.0)
                .saturating_add(step.0)
                .saturating_add(result.0);
            let iteration = condition.2.saturating_add(step.2);
            let work = 1_usize
                .saturating_add(range.2)
                .saturating_add(init.2)
                .saturating_add(iteration.saturating_mul(limits.max_collection_items))
                .saturating_add(result.2);
            return Ok((
                nodes,
                [range.1, init.1, condition.1, step.1, result.1]
                    .into_iter()
                    .max()
                    .unwrap_or(depth),
                work,
            ));
        }
        Expr::List(list) => list.elements.iter().collect(),
        Expr::Map(map) => entry_children(&map.entries),
        Expr::Select(select) => vec![select.operand.as_ref()],
        Expr::Struct(structure) => entry_children(&structure.entries),
        Expr::Ident(_) | Expr::Literal(_) | Expr::Unspecified => Vec::new(),
    };
    let mut nodes = 1_usize;
    let mut maximum_depth = depth;
    let mut work = 1_usize;
    for child in children {
        let child = ast_cost(child, depth + 1, limits)?;
        nodes = nodes.saturating_add(child.0);
        maximum_depth = maximum_depth.max(child.1);
        work = work.saturating_add(child.2);
    }
    Ok((nodes, maximum_depth, work))
}

fn entry_children(entries: &[IdedEntryExpr]) -> Vec<&IdedExpr> {
    entries
        .iter()
        .flat_map(|entry| match &entry.expr {
            EntryExpr::StructField(field) => vec![&field.value],
            EntryExpr::MapEntry(entry) => vec![&entry.key, &entry.value],
        })
        .collect()
}

fn inspect_value(
    value: &serde_json::Value,
    limits: &MdbaseCelLimits,
    label: &str,
) -> Result<(), MdbaseCelError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| MdbaseCelError::new("expression_value_error", error.to_string()))?
        .len();
    if bytes > limits.max_value_bytes {
        return Err(MdbaseCelError::limit(
            &format!("{label} value size"),
            bytes,
            limits.max_value_bytes,
        ));
    }
    let mut nodes = 0_usize;
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        nodes = nodes.saturating_add(1);
        if nodes > limits.max_value_nodes {
            return Err(MdbaseCelError::limit(
                &format!("{label} value node count"),
                nodes,
                limits.max_value_nodes,
            ));
        }
        match value {
            serde_json::Value::Array(values) => {
                if values.len() > limits.max_collection_items {
                    return Err(MdbaseCelError::limit(
                        &format!("{label} list iteration width"),
                        values.len(),
                        limits.max_collection_items,
                    ));
                }
                pending.extend(values);
            }
            serde_json::Value::Object(values) => {
                if values.len() > limits.max_collection_items {
                    return Err(MdbaseCelError::limit(
                        &format!("{label} map iteration width"),
                        values.len(),
                        limits.max_collection_items,
                    ));
                }
                pending.extend(values.values());
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdbase::MdbaseRecordFileMetadata;
    use serde_json::json;

    fn engine_with(mut update: impl FnMut(&mut MdbaseCelLimits)) -> MdbaseCelEngine {
        let mut limits = MdbaseCelLimits::default();
        update(&mut limits);
        MdbaseCelEngine::new(limits)
    }

    fn fixed_clock() -> MdbaseCelClock {
        MdbaseCelClock::new(
            DateTime::parse_from_rfc3339("2026-06-14T22:30:00Z")
                .expect("valid time")
                .with_timezone(&Utc),
            "Europe/Berlin",
        )
        .expect("valid timezone")
    }

    fn record() -> MdbaseRecordDocument {
        MdbaseRecordDocument {
            path: "tasks/open.md".to_string(),
            revision: "sha256:test".to_string(),
            types: vec!["task".to_string()],
            frontmatter: json!({
                "title": "Open task",
                "status": null,
                "tags": ["project/alpha", "urgent"],
                "record": "frontmatter collision"
            }),
            effective_frontmatter: json!({
                "title": "Open task",
                "status": null,
                "priority": 3,
                "tags": ["project/alpha", "urgent"],
                "record": "frontmatter collision"
            }),
            body: "Body #runtime".to_string(),
            document: None,
            file: MdbaseRecordFileMetadata {
                path: "tasks/open.md".to_string(),
                name: "open.md".to_string(),
                basename: "open".to_string(),
                ext: "md".to_string(),
                folder: "tasks".to_string(),
                size: 100,
                mtime: Some("2026-06-14T08:00:00Z".to_string()),
                ctime: None,
            },
            display: None,
            contract_views: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn adapter_compiles_and_evaluates_without_exposing_engine_types() {
        let engine = MdbaseCelEngine::default();
        let program = engine
            .compile("status == 'open' && tags.exists(t, t == 'urgent')")
            .expect("expression compiles");
        let bindings = BTreeMap::from([
            ("status".to_string(), json!("open")),
            ("tags".to_string(), json!(["urgent", "project"])),
        ]);

        assert_eq!(
            engine.evaluate(&program, &bindings).expect("evaluates"),
            true
        );
        assert!(program.stats().ast_nodes > 1);
        assert!(program.stats().estimated_work > program.stats().ast_nodes);
    }

    #[test]
    fn default_limits_meet_the_portable_profile_minimums() {
        let limits = MdbaseCelLimits::default();

        assert!(limits.max_source_bytes >= 64 * 1024);
        assert!(limits.max_ast_depth >= 100);
        assert!(limits.max_link_traversal >= 10);
    }

    #[test]
    fn source_tokens_ast_and_work_are_bounded_before_evaluation() {
        let source = engine_with(|limits| limits.max_source_bytes = 4)
            .compile("true && true")
            .expect_err("source should be bounded");
        assert_eq!(source.code, "expression_limit_exceeded");

        let tokens = engine_with(|limits| limits.max_lexical_tokens = 2)
            .compile("a + b")
            .expect_err("tokens should be bounded");
        assert!(tokens.message.contains("token"));

        let depth = engine_with(|limits| limits.max_ast_depth = 2)
            .compile("(((true)))")
            .expect_err("depth should be bounded");
        assert!(depth.message.contains("depth"));

        let work = engine_with(|limits| limits.max_evaluation_work = 10)
            .compile("items.exists(item, item == 1)")
            .expect_err("comprehension work should be bounded");
        assert!(work.message.contains("work"));
    }

    #[test]
    fn input_output_memory_and_iteration_are_bounded() {
        let input_engine = engine_with(|limits| limits.max_collection_items = 1);
        let input_program = input_engine
            .compile("items.size()")
            .expect("expression compiles");
        let input = input_engine
            .evaluate(
                &input_program,
                &BTreeMap::from([("items".to_string(), json!([1, 2]))]),
            )
            .expect_err("input width should be bounded");
        assert!(input.message.contains("iteration width"));

        let output_engine = engine_with(|limits| limits.max_value_bytes = 8);
        let output_program = output_engine
            .compile("'long output'")
            .expect("expression compiles");
        let output = output_engine
            .evaluate(&output_program, &BTreeMap::new())
            .expect_err("output memory should be bounded");
        assert!(output.message.contains("output value size"));
    }

    #[test]
    fn link_traversal_budget_fails_closed() {
        let engine = engine_with(|limits| limits.max_link_traversal = 1);
        let mut budget = engine.link_budget();

        budget.consume().expect("first traversal is allowed");
        let error = budget.consume().expect_err("second traversal is denied");
        assert_eq!(error.code, "expression_link_traversal_limit");
        assert_eq!(budget.remaining(), 0);
    }

    #[test]
    fn query_context_preserves_raw_effective_presence_and_reserved_namespaces() {
        let engine = MdbaseCelEngine::default();
        let program = engine
            .compile(
                "status == null && missing == null && priority == 3 && \
                 present.raw.priority == false && present.record.priority && \
                 present.raw.status && record.record == 'frontmatter collision'",
            )
            .expect("expression compiles");
        let context = MdbaseCelContext::query(
            MdbaseCelContextKind::QueryFilter,
            &record(),
            ["title", "status", "priority", "missing", "record"]
                .into_iter()
                .map(str::to_string),
            json!({}),
            None,
            fixed_clock(),
        )
        .expect("query context");

        let result = engine
            .evaluate_context(&program, &context)
            .expect("expression evaluates");
        assert_eq!(result.value, true);
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn matching_context_uses_raw_values_and_file_helpers() {
        let engine = MdbaseCelEngine::default();
        let program = engine
            .compile(
                "priority == null && !present.raw.priority && \
                 file.inFolder('tasks') && file.hasTag('project') && \
                 !file.hasTag('proj') && file.hasTag('runtime')",
            )
            .expect("expression compiles");
        let context = MdbaseCelContext::inferred_match(
            &record(),
            ["priority"].into_iter().map(str::to_string),
            fixed_clock(),
        );

        assert_eq!(
            engine
                .evaluate_context(&program, &context)
                .expect("expression evaluates")
                .value,
            true
        );
    }

    #[test]
    fn operation_clock_timezone_and_iso_durations_are_fixed_and_typed() {
        let engine = MdbaseCelEngine::default();
        let program = engine
            .compile(
                "now() == timestamp('2026-06-14T22:30:00Z') && \
                 now() == now() && today() == '2026-06-15' && \
                 duration('PT1H30M') == duration('PT90M')",
            )
            .expect("expression compiles");
        let context = MdbaseCelContext::system(
            MdbaseCelContextKind::QuerySummary,
            BTreeMap::from([("values".to_string(), json!([]))]),
            fixed_clock(),
        )
        .expect("summary context");

        assert_eq!(context.clock.timezone(), "Europe/Berlin");
        assert_eq!(context.clock.today(), "2026-06-15");
        assert_eq!(
            engine
                .evaluate_context(&program, &context)
                .expect("expression evaluates")
                .value,
            true
        );
        assert!(MdbaseCelClock::new(Utc::now(), "+02:00").is_err());
    }

    #[test]
    fn evaluation_failures_follow_the_embedding_context_policy() {
        let engine = MdbaseCelEngine::default();
        let program = engine.compile("'due' + 1").expect("expression compiles");
        let query = MdbaseCelContext::query(
            MdbaseCelContextKind::QueryFilter,
            &record(),
            std::iter::empty(),
            json!({}),
            None,
            fixed_clock(),
        )
        .expect("query context");
        let result = engine
            .evaluate_context(&program, &query)
            .expect("query errors become diagnostics");
        assert_eq!(result.value, serde_json::Value::Null);
        assert_eq!(result.diagnostics[0].code, "expression_evaluation_error");
        assert_eq!(
            result.diagnostics[0].details.as_ref().unwrap()["context"],
            "query_filter"
        );

        let lifecycle = MdbaseCelContext::system(
            MdbaseCelContextKind::LifecycleGuard,
            BTreeMap::new(),
            fixed_clock(),
        )
        .expect("lifecycle context");
        let error = engine
            .evaluate_context(&program, &lifecycle)
            .expect_err("lifecycle errors fail the operation");
        assert_eq!(error.code, "lifecycle_expression_error");
    }

    #[test]
    fn reserved_bindings_are_context_checked_before_execution() {
        let engine = MdbaseCelEngine::default();
        let program = engine
            .compile("steps['finished'].status")
            .expect("expression compiles");
        let trigger = MdbaseCelContext::system(
            MdbaseCelContextKind::WorkflowTrigger,
            BTreeMap::from([("event".to_string(), json!({}))]),
            fixed_clock(),
        )
        .expect("trigger context");

        let error = engine
            .evaluate_context(&program, &trigger)
            .expect_err("steps are unavailable before workflow steps run");
        assert_eq!(error.code, "expression_context_binding_unavailable");
    }

    #[test]
    fn workflow_expression_objects_evaluate_recursively_but_strings_stay_literal() {
        let engine = MdbaseCelEngine::default();
        let context = MdbaseCelContext::system(
            MdbaseCelContextKind::WorkflowStep,
            BTreeMap::from([
                (
                    "event".to_string(),
                    json!({"data": {"file": {"path": "tasks/card-001.md"}}}),
                ),
                (
                    "steps".to_string(),
                    json!({"patch-task-status": {"status": "succeeded"}}),
                ),
            ]),
            fixed_clock(),
        )
        .expect("workflow context");
        let template = json!({
            "path": {"$expr": "event.data.file.path"},
            "state": {"$expr": "steps['patch-task-status'].status"},
            "literal": "event.data.file.path"
        });

        let result = engine
            .evaluate_expression_value(&template, &context)
            .expect("template evaluates");
        assert_eq!(
            result.value,
            json!({
                "path": "tasks/card-001.md",
                "state": "succeeded",
                "literal": "event.data.file.path"
            })
        );
    }
}
