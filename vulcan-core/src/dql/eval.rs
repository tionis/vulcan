use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::cache::CacheDatabase;
use crate::config::load_vault_config;
use crate::expression::eval::{
    compare_values, evaluate, is_truthy, parse_wikilink_target,
    resolve_note_reference as resolve_lookup_note_reference, value_to_display, EvalContext,
};
use crate::expression::value::DataviewTimeZone;
use crate::file_metadata::FileMetadataResolver;
use crate::paths::VaultPaths;
use crate::properties::{
    build_note_filter_clause_from_expressions, load_note_index, FilterExpression, FilterField,
    FilterOperator, FilterValue, NoteRecord, ParsedFilter, PropertyError,
};
use crate::resolve_note_reference as resolve_vault_note_reference;

use super::ast::{DqlDataCommand, DqlLinkTarget, DqlNamedExpr, DqlProjection, DqlQuery};
use super::compile::{compile_dql, CompiledDqlCommand, CompiledDqlSourceExpr};
use super::{parse_dql, DqlDiagnostic};

#[derive(Debug)]
pub enum DqlEvalError {
    Parse(String),
    Property(PropertyError),
    Message(String),
}

impl Display for DqlEvalError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(error) | Self::Message(error) => f.write_str(error),
            Self::Property(error) => Display::fmt(error, f),
        }
    }
}

impl std::error::Error for DqlEvalError {}

impl From<PropertyError> for DqlEvalError {
    fn from(error: PropertyError) -> Self {
        Self::Property(error)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DqlQueryResult {
    pub query_type: super::DqlQueryType,
    pub columns: Vec<String>,
    pub rows: Vec<Value>,
    pub result_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<DqlDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataviewBlockRecord {
    pub file: String,
    pub language: String,
    pub block_index: usize,
    pub line_number: i64,
    pub source: String,
}

#[derive(Debug, Default)]
struct DqlDiagnosticCollector {
    messages: BTreeSet<String>,
}

impl DqlDiagnosticCollector {
    fn push(&mut self, message: impl Into<String>) {
        self.messages.insert(message.into());
    }

    fn into_vec(self) -> Vec<DqlDiagnostic> {
        self.messages
            .into_iter()
            .map(|message| DqlDiagnostic { message })
            .collect()
    }
}

fn is_unsupported_dql_feature(error: &str) -> bool {
    error.starts_with("unknown function `")
        || error.starts_with("unknown method `")
        || error.starts_with("unknown file method `")
}

fn recover_unsupported_feature<T>(
    diagnostics: &mut DqlDiagnosticCollector,
    error: &str,
    diagnostic_message: impl FnOnce(&str) -> String,
    error_message: impl FnOnce(&str) -> String,
    fallback: T,
) -> Result<T, DqlEvalError> {
    if is_unsupported_dql_feature(error) {
        diagnostics.push(diagnostic_message(error));
        Ok(fallback)
    } else {
        Err(DqlEvalError::Message(error_message(error)))
    }
}

pub fn evaluate_dql(
    paths: &VaultPaths,
    source: &str,
    current_file: Option<&str>,
) -> Result<DqlQueryResult, DqlEvalError> {
    let query = parse_dql(source).map_err(DqlEvalError::Parse)?;
    evaluate_parsed_dql(paths, &query, current_file)
}

#[allow(clippy::too_many_lines)]
pub fn evaluate_parsed_dql(
    paths: &VaultPaths,
    query: &DqlQuery,
    current_file: Option<&str>,
) -> Result<DqlQueryResult, DqlEvalError> {
    let config = load_vault_config(paths).config;
    let time_zone = DataviewTimeZone::parse(config.dataview.timezone.as_deref());
    let compiled = compile_dql(query);
    let mut diagnostics = DqlDiagnosticCollector::default();
    let note_lookup = load_note_index(paths)?;
    let all_notes = sorted_notes(&note_lookup);
    let from_sources = compiled
        .commands
        .iter()
        .filter_map(|command| match command {
            CompiledDqlCommand::From(source) => Some(source),
            _ => None,
        })
        .collect::<Vec<_>>();

    if from_sources.len() > 1 {
        return Err(DqlEvalError::Message(
            "DQL queries may contain at most one FROM clause".to_string(),
        ));
    }

    let mut rows = if let Some(source) = from_sources.first() {
        rows_for_source(paths, query, source, current_file, &note_lookup, &all_notes)?
    } else {
        default_rows(query, &all_notes)
    };
    // Resolve the note that contains the query, used as the `this` reference in expressions.
    // When the query is embedded in a note (e.g. a Dataview code block), `current_file` names
    // that note so `WHERE file.name != this.file.name` can filter it out.
    let source_note: Option<NoteRecord> = current_file.and_then(|path| {
        note_lookup
            .values()
            .find(|n| n.document_path == path)
            .cloned()
    });
    let mut page_rows_are_pristine = query.query_type != super::DqlQueryType::Task;

    for command in &compiled.commands {
        match command {
            CompiledDqlCommand::From(_) => {}
            CompiledDqlCommand::Where(where_clause) => {
                // Only use the fast SQL filter path when there is no `this` reference in the WHERE
                // expression; SQL filtering has no knowledge of the source note so `this.*` fields
                // would evaluate incorrectly there.
                let has_this_reference =
                    source_note.is_some() && where_clause_uses_this(&where_clause.expr);
                if page_rows_are_pristine
                    && !has_this_reference
                    && query.query_type != super::DqlQueryType::Task
                    && where_clause.filters.is_some()
                {
                    let matching_paths = matching_note_paths_for_filters(
                        paths,
                        where_clause
                            .filters
                            .as_deref()
                            .expect("filters presence checked above"),
                    )?;
                    rows.retain(|row| matching_paths.contains(row.note.document_path.as_str()));
                } else {
                    rows = apply_where_expression(
                        rows,
                        &where_clause.expr,
                        query,
                        &note_lookup,
                        source_note.as_ref(),
                        time_zone,
                        &mut diagnostics,
                    )?;
                }
            }
            CompiledDqlCommand::Sort(keys) => {
                let mut decorated = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut values = Vec::with_capacity(keys.len());
                    for key in keys {
                        let value = match row.evaluate_with_source(
                            &key.expr,
                            &note_lookup,
                            time_zone,
                            source_note.as_ref(),
                        ) {
                            Ok(value) => value,
                            Err(error) => recover_unsupported_feature(
                                &mut diagnostics,
                                &error,
                                |error| {
                                    format!(
                                        "unsupported DQL feature in SORT for {}: {error}; using null sort key",
                                        row.note.document_path
                                    )
                                },
                                |error| {
                                    format!(
                                        "failed to evaluate SORT key for {}: {error}",
                                        row.note.document_path
                                    )
                                },
                                Value::Null,
                            )?,
                        };
                        values.push(value);
                    }
                    decorated.push((values, row));
                }

                decorated.sort_by(|left, right| {
                    compare_sort_key_lists(&left.0, &right.0, keys)
                        .then_with(|| left.1.identity().cmp(&right.1.identity()))
                });
                rows = decorated.into_iter().map(|(_, row)| row).collect();
            }
            CompiledDqlCommand::Limit(limit) => rows.truncate(*limit),
            CompiledDqlCommand::GroupBy(named_expr) => {
                rows = apply_group_by(rows, named_expr, &note_lookup, time_zone, &mut diagnostics)?;
                page_rows_are_pristine = false;
            }
            CompiledDqlCommand::Flatten(named_expr) => {
                rows = apply_flatten(rows, named_expr, &note_lookup, time_zone, &mut diagnostics)?;
                page_rows_are_pristine = false;
            }
        }
    }

    let mut result = render_result(
        query,
        &config.dataview.primary_column_name,
        &config.dataview.group_column_name,
        rows,
        &note_lookup,
        time_zone,
        &mut diagnostics,
    )?;
    result.diagnostics = diagnostics.into_vec();
    Ok(result)
}

pub fn load_dataview_blocks(
    paths: &VaultPaths,
    file: &str,
    block: Option<usize>,
) -> Result<Vec<DataviewBlockRecord>, DqlEvalError> {
    let resolved = resolve_vault_note_reference(paths, file)
        .map_err(|error| DqlEvalError::Message(error.to_string()))?;
    let database =
        CacheDatabase::open(paths).map_err(|error| DqlEvalError::Message(error.to_string()))?;
    let connection = database.connection();
    let mut statement = connection
        .prepare(
            "SELECT dataview_blocks.language, dataview_blocks.block_index, \
             dataview_blocks.line_number, dataview_blocks.raw_text
             FROM dataview_blocks
             JOIN documents ON documents.id = dataview_blocks.document_id
             WHERE documents.path = ?1
             ORDER BY dataview_blocks.block_index",
        )
        .map_err(|error| DqlEvalError::Message(error.to_string()))?;
    let rows = statement
        .query_map([resolved.path.as_str()], |row| {
            let block_index = row.get::<_, i64>(1)?;
            Ok(DataviewBlockRecord {
                file: resolved.path.clone(),
                language: row.get(0)?,
                block_index: usize::try_from(block_index).unwrap_or_default(),
                line_number: row.get(2)?,
                source: row.get(3)?,
            })
        })
        .map_err(|error| DqlEvalError::Message(error.to_string()))?;

    let mut blocks = Vec::new();
    for row in rows {
        blocks.push(row.map_err(|error| DqlEvalError::Message(error.to_string()))?);
    }

    if let Some(requested_block) = block {
        return blocks
            .into_iter()
            .find(|candidate| candidate.block_index == requested_block)
            .map(|candidate| vec![candidate])
            .ok_or_else(|| {
                DqlEvalError::Message(format!(
                    "no Dataview block {requested_block} found in {}",
                    resolved.path
                ))
            });
    }

    if blocks.is_empty() {
        return Err(DqlEvalError::Message(format!(
            "no Dataview blocks found in {}",
            resolved.path
        )));
    }

    Ok(blocks)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Page,
    Task,
    Group,
}

#[derive(Debug, Clone)]
struct ExecutionRow {
    note: NoteRecord,
    fields: Map<String, Value>,
    ordinal: i64,
    kind: RowKind,
    task_id: Option<String>,
    parent_task_id: Option<String>,
}

impl ExecutionRow {
    fn page(note: &NoteRecord) -> Self {
        Self {
            note: note.clone(),
            fields: note
                .properties
                .as_object()
                .cloned()
                .unwrap_or_else(Map::new),
            ordinal: 0,
            kind: RowKind::Page,
            task_id: None,
            parent_task_id: None,
        }
    }

    fn task(
        note: &NoteRecord,
        task_id: String,
        parent_task_id: Option<String>,
        fields: Map<String, Value>,
    ) -> Self {
        let ordinal = fields.get("line").and_then(Value::as_i64).unwrap_or(0);
        Self {
            note: note.clone(),
            fields,
            ordinal,
            kind: RowKind::Task,
            task_id: Some(task_id),
            parent_task_id,
        }
    }

    fn group(fields: Map<String, Value>, ordinal: i64) -> Self {
        Self {
            note: synthetic_group_note(&fields),
            fields,
            ordinal,
            kind: RowKind::Group,
            task_id: None,
            parent_task_id: None,
        }
    }

    fn evaluate(
        &self,
        expr: &crate::expression::ast::Expr,
        note_lookup: &HashMap<String, NoteRecord>,
        time_zone: DataviewTimeZone,
    ) -> Result<Value, String> {
        self.evaluate_with_source(expr, note_lookup, time_zone, None)
    }

    fn evaluate_with_source(
        &self,
        expr: &crate::expression::ast::Expr,
        note_lookup: &HashMap<String, NoteRecord>,
        time_zone: DataviewTimeZone,
        source_note: Option<&NoteRecord>,
    ) -> Result<Value, String> {
        let mut note = self.note.clone();
        note.properties = Value::Object(self.fields.clone());
        let formulas = BTreeMap::new();
        let mut ctx = EvalContext::new(&note, &formulas)
            .with_note_lookup(note_lookup)
            .with_time_zone(time_zone);
        if let Some(sn) = source_note {
            ctx = ctx.with_this_note(sn);
        }
        evaluate(expr, &ctx)
    }

    fn identity(&self) -> (&str, i64) {
        (self.note.document_path.as_str(), self.ordinal)
    }

    fn data_object(&self) -> Value {
        let mut object = self.fields.clone();
        if self.kind != RowKind::Group {
            object.insert("file".to_string(), FileMetadataResolver::object(&self.note));
        }
        Value::Object(object)
    }

    fn primary_value(&self) -> Value {
        match self.kind {
            RowKind::Group => self.fields.get("key").cloned().unwrap_or(Value::Null),
            RowKind::Page | RowKind::Task => FileMetadataResolver::field(&self.note, "link"),
        }
    }
}

fn sorted_notes(note_lookup: &HashMap<String, NoteRecord>) -> Vec<NoteRecord> {
    let mut notes = note_lookup.values().cloned().collect::<Vec<_>>();
    notes.sort_by(|left, right| left.document_path.cmp(&right.document_path));
    notes
}

fn default_rows(query: &DqlQuery, notes: &[NoteRecord]) -> Vec<ExecutionRow> {
    match query.query_type {
        super::DqlQueryType::Task => notes.iter().flat_map(task_rows_for_note).collect(),
        _ => notes.iter().map(ExecutionRow::page).collect(),
    }
}

fn rows_for_source(
    paths: &VaultPaths,
    query: &DqlQuery,
    source: &CompiledDqlSourceExpr,
    current_file: Option<&str>,
    note_lookup: &HashMap<String, NoteRecord>,
    all_notes: &[NoteRecord],
) -> Result<Vec<ExecutionRow>, DqlEvalError> {
    let source_paths = source_paths(paths, source, current_file, note_lookup, all_notes)?;
    let mut notes = all_notes
        .iter()
        .filter(|note| source_paths.contains(note.document_path.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    notes.sort_by(|left, right| left.document_path.cmp(&right.document_path));

    Ok(match query.query_type {
        super::DqlQueryType::Task => notes.iter().flat_map(task_rows_for_note).collect(),
        _ => notes.iter().map(ExecutionRow::page).collect(),
    })
}

fn task_rows_for_note(note: &NoteRecord) -> Vec<ExecutionRow> {
    match FileMetadataResolver::field(note, "tasks") {
        Value::Array(tasks) => tasks
            .into_iter()
            .zip(note.tasks.iter())
            .filter_map(|(task, record)| match task {
                Value::Object(fields) => Some(ExecutionRow::task(
                    note,
                    record.id.clone(),
                    record.parent_task_id.clone(),
                    fields,
                )),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Returns true if the expression tree contains a reference to `this` (the Dataview identifier
/// for the note that contains the query).  Used to decide whether to fall back from the fast SQL
/// filter path to the full expression evaluator, which can properly resolve `this.*`.
fn where_clause_uses_this(expr: &crate::expression::ast::Expr) -> bool {
    use crate::expression::ast::Expr;
    match expr {
        Expr::Identifier(name) => crate::expression::eval::normalize_field_name(name) == "this",
        Expr::FieldAccess(receiver, _) => where_clause_uses_this(receiver),
        Expr::IndexAccess(receiver, index) => {
            where_clause_uses_this(receiver) || where_clause_uses_this(index)
        }
        Expr::FunctionCall(_, args) => args.iter().any(where_clause_uses_this),
        Expr::MethodCall(receiver, _, args) => {
            where_clause_uses_this(receiver) || args.iter().any(where_clause_uses_this)
        }
        Expr::BinaryOp(left, _, right) => {
            where_clause_uses_this(left) || where_clause_uses_this(right)
        }
        Expr::UnaryOp(_, operand) => where_clause_uses_this(operand),
        Expr::Lambda(_, body) => where_clause_uses_this(body),
        Expr::Array(elements) => elements.iter().any(where_clause_uses_this),
        Expr::Object(entries) => entries.iter().any(|(_, v)| where_clause_uses_this(v)),
        Expr::Null
        | Expr::Bool(_)
        | Expr::Number(_)
        | Expr::Str(_)
        | Expr::Regex { .. }
        | Expr::FormulaRef(_) => false,
    }
}

fn apply_where_expression(
    rows: Vec<ExecutionRow>,
    expr: &crate::expression::ast::Expr,
    query: &DqlQuery,
    note_lookup: &HashMap<String, NoteRecord>,
    source_note: Option<&NoteRecord>,
    time_zone: DataviewTimeZone,
    diagnostics: &mut DqlDiagnosticCollector,
) -> Result<Vec<ExecutionRow>, DqlEvalError> {
    let mut decorated = Vec::with_capacity(rows.len());
    let mut directly_matched_task_ids = HashSet::new();

    for row in rows {
        let value = match row.evaluate_with_source(expr, note_lookup, time_zone, source_note) {
            Ok(value) => value,
            Err(error) => recover_unsupported_feature(
                diagnostics,
                &error,
                |error| {
                    format!(
                        "unsupported DQL feature in WHERE for {}: {error}; treating the row as non-matching",
                        row.note.document_path
                    )
                },
                |error| {
                    format!(
                        "failed to evaluate WHERE for {}: {error}",
                        row.note.document_path
                    )
                },
                Value::Bool(false),
            )?,
        };
        let matched = is_truthy(&value);
        if matched && query.query_type == super::DqlQueryType::Task {
            if let Some(task_id) = row.task_id.as_ref() {
                directly_matched_task_ids.insert(task_id.clone());
            }
        }
        decorated.push((matched, row));
    }

    if query.query_type != super::DqlQueryType::Task || directly_matched_task_ids.is_empty() {
        return Ok(decorated
            .into_iter()
            .filter_map(|(matched, row)| matched.then_some(row))
            .collect());
    }

    let descendant_task_ids = descendant_task_ids(&decorated, &directly_matched_task_ids);
    Ok(decorated
        .into_iter()
        .filter_map(|(matched, row)| {
            if matched {
                return Some(row);
            }
            row.task_id
                .as_ref()
                .is_some_and(|task_id| descendant_task_ids.contains(task_id))
                .then_some(row)
        })
        .collect())
}

fn descendant_task_ids(rows: &[(bool, ExecutionRow)], roots: &HashSet<String>) -> HashSet<String> {
    let mut children_by_parent = HashMap::<&str, Vec<&str>>::new();
    for (_, row) in rows {
        if let (Some(task_id), Some(parent_task_id)) =
            (row.task_id.as_deref(), row.parent_task_id.as_deref())
        {
            children_by_parent
                .entry(parent_task_id)
                .or_default()
                .push(task_id);
        }
    }

    let mut included = HashSet::new();
    let mut stack = roots.iter().map(String::as_str).collect::<Vec<_>>();
    while let Some(task_id) = stack.pop() {
        if let Some(children) = children_by_parent.get(task_id) {
            for child in children {
                if included.insert((*child).to_string()) {
                    stack.push(child);
                }
            }
        }
    }
    included
}

fn source_paths(
    paths: &VaultPaths,
    source: &CompiledDqlSourceExpr,
    current_file: Option<&str>,
    note_lookup: &HashMap<String, NoteRecord>,
    all_notes: &[NoteRecord],
) -> Result<HashSet<String>, DqlEvalError> {
    Ok(match source {
        CompiledDqlSourceExpr::Filter(filter) => {
            matching_note_paths_for_filters(paths, std::slice::from_ref(filter))?
        }
        CompiledDqlSourceExpr::Path(path) => {
            matching_note_paths_for_filters(paths, &[path_source_filter(path, all_notes)])?
        }
        CompiledDqlSourceExpr::IncomingLink(target) => {
            let target_note = resolve_source_target(target, current_file, note_lookup)?;
            incoming_link_sources(paths, target_note)?
        }
        CompiledDqlSourceExpr::OutgoingLink(target) => {
            let target_note = resolve_source_target(target, current_file, note_lookup)?;
            outgoing_link_sources(paths, target_note)?
        }
        CompiledDqlSourceExpr::Not(inner) => {
            let inner_paths = source_paths(paths, inner, current_file, note_lookup, all_notes)?;
            all_notes
                .iter()
                .filter(|note| !inner_paths.contains(note.document_path.as_str()))
                .map(|note| note.document_path.clone())
                .collect()
        }
        CompiledDqlSourceExpr::And(left, right) => {
            let left_paths = source_paths(paths, left, current_file, note_lookup, all_notes)?;
            let right_paths = source_paths(paths, right, current_file, note_lookup, all_notes)?;
            left_paths.intersection(&right_paths).cloned().collect()
        }
        CompiledDqlSourceExpr::Or(left, right) => {
            let mut left_paths = source_paths(paths, left, current_file, note_lookup, all_notes)?;
            left_paths.extend(source_paths(
                paths,
                right,
                current_file,
                note_lookup,
                all_notes,
            )?);
            left_paths
        }
    })
}

fn path_source_filter(path: &str, all_notes: &[NoteRecord]) -> FilterExpression {
    if Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return FilterExpression::Condition(ParsedFilter {
            field: FilterField::FilePath,
            operator: FilterOperator::Eq,
            value: FilterValue::Text(path.to_string()),
        });
    }

    let normalized = path.trim_end_matches('/');
    let folder_prefix = format!("{normalized}/");
    let exact_file = format!("{normalized}.md");
    let folder_exists = all_notes
        .iter()
        .any(|candidate| candidate.document_path.starts_with(&folder_prefix));
    let file_exists = all_notes.iter().any(|candidate| {
        candidate.document_path == normalized || candidate.document_path == exact_file
    });

    if path.contains('/') && file_exists {
        exact_path_filter(normalized, &exact_file)
    } else if folder_exists {
        FilterExpression::Condition(ParsedFilter {
            field: FilterField::FilePath,
            operator: FilterOperator::StartsWith,
            value: FilterValue::Text(folder_prefix),
        })
    } else {
        exact_path_filter(normalized, &exact_file)
    }
}

fn exact_path_filter(normalized: &str, exact_file: &str) -> FilterExpression {
    if normalized == exact_file {
        FilterExpression::Condition(ParsedFilter {
            field: FilterField::FilePath,
            operator: FilterOperator::Eq,
            value: FilterValue::Text(normalized.to_string()),
        })
    } else {
        FilterExpression::Any(vec![
            ParsedFilter {
                field: FilterField::FilePath,
                operator: FilterOperator::Eq,
                value: FilterValue::Text(normalized.to_string()),
            },
            ParsedFilter {
                field: FilterField::FilePath,
                operator: FilterOperator::Eq,
                value: FilterValue::Text(exact_file.to_string()),
            },
        ])
    }
}

fn resolve_source_target<'a>(
    target: &DqlLinkTarget,
    current_file: Option<&str>,
    note_lookup: &'a HashMap<String, NoteRecord>,
) -> Result<&'a NoteRecord, DqlEvalError> {
    match target {
        DqlLinkTarget::SelfReference => {
            let current_file = current_file.ok_or_else(|| {
                DqlEvalError::Message(
                    "self-referential FROM sources require a current note context".to_string(),
                )
            })?;
            note_lookup
                .values()
                .find(|note| note.document_path == current_file)
                .ok_or_else(|| {
                    DqlEvalError::Message(format!("current note is not indexed: {current_file}"))
                })
        }
        DqlLinkTarget::Wikilink(raw) => {
            let source_path = current_file.unwrap_or_default();
            let target = parse_wikilink_target(raw);
            resolve_lookup_note_reference(note_lookup, source_path, &target).ok_or_else(|| {
                DqlEvalError::Message(format!("could not resolve DQL source target {raw}"))
            })
        }
    }
}

fn incoming_link_sources(
    paths: &VaultPaths,
    target_note: &NoteRecord,
) -> Result<HashSet<String>, DqlEvalError> {
    let database =
        CacheDatabase::open(paths).map_err(|error| DqlEvalError::Message(error.to_string()))?;
    let mut statement = database
        .connection()
        .prepare(
            "
            SELECT source.path
            FROM links
            JOIN documents AS source ON source.id = links.source_document_id
            WHERE links.resolved_target_id = ?1
            ORDER BY source.path
            ",
        )
        .map_err(|error| DqlEvalError::Message(error.to_string()))?;
    let rows = statement
        .query_map([target_note.document_id.as_str()], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| DqlEvalError::Message(error.to_string()))?;
    rows.collect::<Result<HashSet<_>, _>>()
        .map_err(|error| DqlEvalError::Message(error.to_string()))
}

fn outgoing_link_sources(
    paths: &VaultPaths,
    target_note: &NoteRecord,
) -> Result<HashSet<String>, DqlEvalError> {
    let database =
        CacheDatabase::open(paths).map_err(|error| DqlEvalError::Message(error.to_string()))?;
    let mut statement = database
        .connection()
        .prepare(
            "
            SELECT target.path
            FROM links
            JOIN documents AS target ON target.id = links.resolved_target_id
            WHERE links.source_document_id = ?1
            ORDER BY target.path
            ",
        )
        .map_err(|error| DqlEvalError::Message(error.to_string()))?;
    let rows = statement
        .query_map([target_note.document_id.as_str()], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| DqlEvalError::Message(error.to_string()))?;
    rows.collect::<Result<HashSet<_>, _>>()
        .map_err(|error| DqlEvalError::Message(error.to_string()))
}

fn matching_note_paths_for_filters(
    paths: &VaultPaths,
    filters: &[FilterExpression],
) -> Result<HashSet<String>, DqlEvalError> {
    if filters.is_empty() {
        return Ok(HashSet::new());
    }

    let database =
        CacheDatabase::open(paths).map_err(|error| DqlEvalError::Message(error.to_string()))?;
    let filter_sql = build_note_filter_clause_from_expressions(filters)?;
    let mut sql = filter_sql.cte;
    sql.push_str(
        "SELECT documents.path
        FROM documents
        LEFT JOIN properties ON properties.document_id = documents.id
        WHERE documents.extension = 'md'",
    );
    sql.push_str(&filter_sql.clause);
    let mut statement = database
        .connection()
        .prepare(&sql)
        .map_err(|error| DqlEvalError::Message(error.to_string()))?;
    let rows = statement
        .query_map(
            rusqlite::params_from_iter(filter_sql.params.iter()),
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| DqlEvalError::Message(error.to_string()))?;
    rows.collect::<Result<HashSet<_>, _>>()
        .map_err(|error| DqlEvalError::Message(error.to_string()))
}

fn compare_sort_key_lists(left: &[Value], right: &[Value], keys: &[super::DqlSortKey]) -> Ordering {
    for (index, key) in keys.iter().enumerate() {
        let ordering = compare_sort_values(&left[index], &right[index]);
        let ordering = match key.direction {
            super::DqlSortDirection::Asc => ordering,
            super::DqlSortDirection::Desc => ordering.reverse(),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

fn compare_sort_values(left: &Value, right: &Value) -> Ordering {
    compare_values(left, right)
        .unwrap_or_else(|| value_to_display(left).cmp(&value_to_display(right)))
}

fn apply_group_by(
    rows: Vec<ExecutionRow>,
    named_expr: &DqlNamedExpr,
    note_lookup: &HashMap<String, NoteRecord>,
    time_zone: DataviewTimeZone,
    diagnostics: &mut DqlDiagnosticCollector,
) -> Result<Vec<ExecutionRow>, DqlEvalError> {
    let group_name = named_expr
        .alias
        .clone()
        .unwrap_or_else(|| expression_label(&named_expr.expr));
    let mut decorated = Vec::with_capacity(rows.len());

    for row in rows {
        let key = match row.evaluate(&named_expr.expr, note_lookup, time_zone) {
            Ok(value) => value,
            Err(error) => recover_unsupported_feature(
                diagnostics,
                &error,
                |error| {
                    format!(
                        "unsupported DQL feature in GROUP BY for {}: {error}; grouping under null",
                        row.note.document_path
                    )
                },
                |error| {
                    format!(
                        "failed to evaluate GROUP BY key for {}: {error}",
                        row.note.document_path
                    )
                },
                Value::Null,
            )?,
        };
        decorated.push((key, row));
    }

    decorated.sort_by(|left, right| {
        compare_sort_values(&left.0, &right.0)
            .then_with(|| left.1.identity().cmp(&right.1.identity()))
    });

    let mut grouped_rows: Vec<ExecutionRow> = Vec::new();
    let mut group_index = 0_i64;
    for (key, row) in decorated {
        let row_data = row.data_object();
        if let Some(last_group) = grouped_rows.last_mut() {
            let same_key = last_group
                .fields
                .get("key")
                .is_some_and(|last_key| group_keys_equal(last_key, &key));
            if same_key {
                if let Some(Value::Array(items)) = last_group.fields.get_mut("rows") {
                    items.push(row_data);
                    continue;
                }
            }
        }

        let mut fields = Map::new();
        fields.insert("key".to_string(), key.clone());
        fields.insert(group_name.clone(), key);
        fields.insert("rows".to_string(), Value::Array(vec![row_data]));
        grouped_rows.push(ExecutionRow::group(fields, group_index));
        group_index += 1;
    }

    Ok(grouped_rows)
}

fn apply_flatten(
    rows: Vec<ExecutionRow>,
    named_expr: &DqlNamedExpr,
    note_lookup: &HashMap<String, NoteRecord>,
    time_zone: DataviewTimeZone,
    diagnostics: &mut DqlDiagnosticCollector,
) -> Result<Vec<ExecutionRow>, DqlEvalError> {
    let field_name = named_expr
        .alias
        .clone()
        .unwrap_or_else(|| expression_label(&named_expr.expr));
    let mut flattened_rows = Vec::new();

    for row in rows {
        let value = match row.evaluate(&named_expr.expr, note_lookup, time_zone) {
            Ok(value) => value,
            Err(error) => recover_unsupported_feature(
                diagnostics,
                &error,
                |error| {
                    format!(
                        "unsupported DQL feature in FLATTEN for {}: {error}; flattening a single null value",
                        row.note.document_path
                    )
                },
                |error| {
                    format!(
                        "failed to evaluate FLATTEN expression for {}: {error}",
                        row.note.document_path
                    )
                },
                Value::Null,
            )?,
        };

        let datapoints = match value {
            Value::Array(values) => values,
            other => vec![other],
        };

        for datapoint in datapoints {
            let mut flattened = row.clone();
            flattened.fields.insert(field_name.clone(), datapoint);
            flattened_rows.push(flattened);
        }
    }

    Ok(flattened_rows)
}

fn group_keys_equal(left: &Value, right: &Value) -> bool {
    compare_values(left, right) == Some(Ordering::Equal) || left == right
}

fn synthetic_group_note(fields: &Map<String, Value>) -> NoteRecord {
    NoteRecord {
        document_id: String::new(),
        document_path: String::new(),
        file_name: String::new(),
        file_ext: "md".to_string(),
        file_mtime: 0,
        file_ctime: 0,
        file_size: 0,
        properties: Value::Object(fields.clone()),
        tags: Vec::new(),
        links: Vec::new(),
        starred: false,
        inlinks: Vec::new(),
        aliases: Vec::new(),
        frontmatter: Value::Object(Map::new()),
        periodic_type: None,
        periodic_date: None,
        list_items: Vec::new(),
        tasks: Vec::new(),
        raw_inline_expressions: Vec::new(),
        inline_expressions: Vec::new(),
    }
}

fn render_result(
    query: &DqlQuery,
    primary_column_name: &str,
    group_column_name: &str,
    rows: Vec<ExecutionRow>,
    note_lookup: &HashMap<String, NoteRecord>,
    time_zone: DataviewTimeZone,
    diagnostics: &mut DqlDiagnosticCollector,
) -> Result<DqlQueryResult, DqlEvalError> {
    let first_column_name = result_first_column_name(query, primary_column_name, group_column_name);
    match query.query_type {
        super::DqlQueryType::Table => render_table_result(
            query,
            first_column_name,
            rows,
            note_lookup,
            time_zone,
            diagnostics,
        ),
        super::DqlQueryType::List => render_list_result(
            query,
            first_column_name,
            rows,
            note_lookup,
            time_zone,
            diagnostics,
        ),
        super::DqlQueryType::Task => Ok(render_task_result(query, first_column_name, rows)),
        super::DqlQueryType::Calendar => render_calendar_result(
            query,
            first_column_name,
            rows,
            note_lookup,
            time_zone,
            diagnostics,
        ),
    }
}

fn result_first_column_name<'a>(
    query: &DqlQuery,
    primary_column_name: &'a str,
    group_column_name: &'a str,
) -> &'a str {
    if query_has_group_by(query) {
        group_column_name
    } else {
        primary_column_name
    }
}

fn query_has_group_by(query: &DqlQuery) -> bool {
    query
        .commands
        .iter()
        .any(|command| matches!(command, DqlDataCommand::GroupBy(_)))
}

fn render_table_result(
    query: &DqlQuery,
    primary_column_name: &str,
    rows: Vec<ExecutionRow>,
    note_lookup: &HashMap<String, NoteRecord>,
    time_zone: DataviewTimeZone,
    diagnostics: &mut DqlDiagnosticCollector,
) -> Result<DqlQueryResult, DqlEvalError> {
    let mut columns = Vec::new();
    if !query.without_id {
        columns.push(primary_column_name.to_string());
    }
    columns.extend(query.table_columns.iter().map(projection_label));

    let rendered_rows = rows
        .into_iter()
        .map(|row| {
            render_table_row(
                &row,
                query,
                primary_column_name,
                note_lookup,
                time_zone,
                diagnostics,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(DqlQueryResult {
        query_type: query.query_type,
        result_count: rendered_rows.len(),
        columns,
        rows: rendered_rows,
        diagnostics: Vec::new(),
    })
}

fn render_table_row(
    row: &ExecutionRow,
    query: &DqlQuery,
    primary_column_name: &str,
    note_lookup: &HashMap<String, NoteRecord>,
    time_zone: DataviewTimeZone,
    diagnostics: &mut DqlDiagnosticCollector,
) -> Result<Value, DqlEvalError> {
    let mut object = Map::new();
    if !query.without_id {
        object.insert(primary_column_name.to_string(), row.primary_value());
    }

    for projection in &query.table_columns {
        let label = projection_label(projection);
        let value = match row.evaluate(&projection.expr, note_lookup, time_zone) {
            Ok(value) => value,
            Err(error) => recover_unsupported_feature(
                diagnostics,
                &error,
                |error| {
                    format!(
                        "unsupported DQL feature in TABLE column `{label}` for {}: {error}; rendered as null",
                        row.note.document_path
                    )
                },
                |error| {
                    format!(
                        "failed to evaluate TABLE column `{label}` for {}: {error}",
                        row.note.document_path
                    )
                },
                Value::Null,
            )?,
        };
        object.insert(label, value);
    }

    Ok(Value::Object(object))
}

fn render_list_result(
    query: &DqlQuery,
    primary_column_name: &str,
    rows: Vec<ExecutionRow>,
    note_lookup: &HashMap<String, NoteRecord>,
    time_zone: DataviewTimeZone,
    diagnostics: &mut DqlDiagnosticCollector,
) -> Result<DqlQueryResult, DqlEvalError> {
    let mut columns = Vec::new();
    if !query.without_id {
        columns.push(primary_column_name.to_string());
    }
    if query.list_expression.is_some() || query.without_id {
        columns.push("value".to_string());
    }

    let rendered_rows = rows
        .into_iter()
        .map(|row| {
            let mut object = Map::new();
            if !query.without_id {
                object.insert(primary_column_name.to_string(), row.primary_value());
            }
            if let Some(expr) = &query.list_expression {
                let value = match row.evaluate(expr, note_lookup, time_zone) {
                    Ok(value) => value,
                    Err(error) => recover_unsupported_feature(
                        diagnostics,
                        &error,
                        |error| {
                            format!(
                                "unsupported DQL feature in LIST for {}: {error}; rendered as null",
                                row.note.document_path
                            )
                        },
                        |error| {
                            format!(
                                "failed to evaluate LIST expression for {}: {error}",
                                row.note.document_path
                            )
                        },
                        Value::Null,
                    )?,
                };
                object.insert("value".to_string(), value);
            } else if query.without_id {
                object.insert(
                    "value".to_string(),
                    FileMetadataResolver::field(&row.note, "link"),
                );
            }
            Ok(Value::Object(object))
        })
        .collect::<Result<Vec<_>, DqlEvalError>>()?;

    Ok(DqlQueryResult {
        query_type: query.query_type,
        result_count: rendered_rows.len(),
        columns,
        rows: rendered_rows,
        diagnostics: Vec::new(),
    })
}

fn render_task_result(
    query: &DqlQuery,
    primary_column_name: &str,
    rows: Vec<ExecutionRow>,
) -> DqlQueryResult {
    let mut columns = vec![primary_column_name.to_string()];
    columns.extend(
        [
            "status",
            "text",
            "visual",
            "checked",
            "completed",
            "fullyCompleted",
        ]
        .into_iter()
        .map(ToOwned::to_owned),
    );

    let rendered_rows = rows
        .into_iter()
        .map(|row| {
            let mut object = row.fields.clone();
            object.insert(primary_column_name.to_string(), row.primary_value());
            Value::Object(object)
        })
        .collect::<Vec<_>>();

    DqlQueryResult {
        query_type: query.query_type,
        result_count: rendered_rows.len(),
        columns,
        rows: rendered_rows,
        diagnostics: Vec::new(),
    }
}

fn render_calendar_result(
    query: &DqlQuery,
    primary_column_name: &str,
    rows: Vec<ExecutionRow>,
    note_lookup: &HashMap<String, NoteRecord>,
    time_zone: DataviewTimeZone,
    diagnostics: &mut DqlDiagnosticCollector,
) -> Result<DqlQueryResult, DqlEvalError> {
    let expr = query.calendar_expression.as_ref().ok_or_else(|| {
        DqlEvalError::Message("CALENDAR queries require a date expression".to_string())
    })?;
    let mut rendered_rows = Vec::new();

    for row in rows {
        let value = match row.evaluate(expr, note_lookup, time_zone) {
            Ok(value) => value,
            Err(error) => recover_unsupported_feature(
                diagnostics,
                &error,
                |error| {
                    format!(
                        "unsupported DQL feature in CALENDAR for {}: {error}; skipping the row",
                        row.note.document_path
                    )
                },
                |error| {
                    format!(
                        "failed to evaluate CALENDAR expression for {}: {error}",
                        row.note.document_path
                    )
                },
                Value::Null,
            )?,
        };
        if value.is_null() {
            continue;
        }

        let mut object = Map::new();
        object.insert("date".to_string(), value);
        object.insert(primary_column_name.to_string(), row.primary_value());
        rendered_rows.push(Value::Object(object));
    }

    Ok(DqlQueryResult {
        query_type: query.query_type,
        result_count: rendered_rows.len(),
        columns: vec!["date".to_string(), primary_column_name.to_string()],
        rows: rendered_rows,
        diagnostics: Vec::new(),
    })
}

fn projection_label(projection: &DqlProjection) -> String {
    projection
        .alias
        .clone()
        .unwrap_or_else(|| expression_label(&projection.expr))
}

fn expression_label(expr: &crate::expression::ast::Expr) -> String {
    use crate::expression::ast::{BinOp, Expr, UnOp};

    match expr {
        Expr::Identifier(name) | Expr::FunctionCall(name, _) => name.clone(),
        Expr::FieldAccess(receiver, field) => format!("{}.{}", expression_label(receiver), field),
        Expr::IndexAccess(receiver, index) => {
            format!(
                "{}[{}]",
                expression_label(receiver),
                expression_label(index)
            )
        }
        Expr::FormulaRef(name) => format!("${name}"),
        Expr::Str(text) => text.clone(),
        Expr::Number(number) => value_to_display(&Value::from(*number)),
        Expr::Bool(value) => value.to_string(),
        Expr::Null => "null".to_string(),
        Expr::Array(_) | Expr::Object(_) | Expr::Regex { .. } => format!("{expr:?}"),
        Expr::Lambda(_, _) => "lambda".to_string(),
        Expr::MethodCall(receiver, method, _) => {
            format!("{}.{}", expression_label(receiver), method)
        }
        Expr::UnaryOp(UnOp::Not, operand) => format!("!{}", expression_label(operand)),
        Expr::UnaryOp(UnOp::Neg, operand) => format!("-{}", expression_label(operand)),
        Expr::BinaryOp(left, op, right) => {
            format!(
                "{} {} {}",
                expression_label(left),
                match op {
                    BinOp::And => "&&",
                    BinOp::Or => "||",
                    BinOp::Eq => "=",
                    BinOp::Ne => "!=",
                    BinOp::Gt => ">",
                    BinOp::Lt => "<",
                    BinOp::Ge => ">=",
                    BinOp::Le => "<=",
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Mod => "%",
                },
                expression_label(right)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use crate::{scan_vault, ScanMode, VaultPaths};

    use super::*;

    #[test]
    fn evaluates_table_queries_against_dataview_fixture() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let result = evaluate_dql(
            &paths,
            r#"TABLE status, priority
FROM "Projects"
WHERE priority >= 1
SORT file.name DESC
LIMIT 1"#,
            None,
        )
        .expect("DQL should evaluate");

        assert_eq!(result.query_type, super::super::DqlQueryType::Table);
        assert_eq!(result.columns, vec!["File", "status", "priority"]);
        assert_eq!(result.result_count, 1);
        assert_eq!(
            result.rows[0]["File"],
            Value::String("[[Projects/Beta]]".to_string())
        );
        assert_eq!(
            result.rows[0]["status"],
            Value::String("backlog".to_string())
        );
        assert_eq!(result.rows[0]["priority"].as_f64(), Some(5.0));
    }

    #[test]
    fn evaluates_list_queries_with_expression_values() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let result = evaluate_dql(
            &paths,
            r#"LIST choice(reviewed, status, "skip")
FROM "Projects"
SORT file.name ASC"#,
            None,
        )
        .expect("DQL should evaluate");

        assert_eq!(result.query_type, super::super::DqlQueryType::List);
        assert_eq!(result.columns, vec!["File", "value"]);
        assert_eq!(result.result_count, 2);
        assert_eq!(
            result.rows[0]["File"],
            Value::String("[[Projects/Alpha]]".to_string())
        );
        assert_eq!(result.rows[0]["value"], Value::String("active".to_string()));
        assert_eq!(
            result.rows[1]["File"],
            Value::String("[[Projects/Beta]]".to_string())
        );
        assert_eq!(result.rows[1]["value"], Value::String("skip".to_string()));
    }

    #[test]
    fn evaluates_link_indexing_inside_dql_expressions() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let result = evaluate_dql(
            &paths,
            r#"TABLE [[People/Bob]].role AS editor_role
FROM "Dashboard"
WHERE [[People/Bob]].role = "editor""#,
            None,
        )
        .expect("DQL should evaluate");

        assert_eq!(result.query_type, super::super::DqlQueryType::Table);
        assert_eq!(result.result_count, 1);
        assert_eq!(
            result.rows[0]["editor_role"],
            Value::String("editor".to_string())
        );
    }

    #[test]
    fn from_tag_sources_include_subtags() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should be created");
        fs::write(
            vault_root.join("Projects/Alpha.md"),
            "Tag #project/subtag\n",
        )
        .expect("alpha note should be written");
        fs::write(vault_root.join("Projects/Beta.md"), "Tag #project\n")
            .expect("beta note should be written");

        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let result = evaluate_dql(
            &paths,
            r"TABLE WITHOUT ID file.path AS path
FROM #project
SORT path ASC",
            None,
        )
        .expect("DQL should evaluate");

        assert_eq!(
            result.rows,
            vec![
                serde_json::json!({ "path": "Projects/Alpha.md" }),
                serde_json::json!({ "path": "Projects/Beta.md" }),
            ]
        );
    }

    #[test]
    fn evaluates_task_queries_using_inherited_page_fields() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let result = evaluate_dql(
            &paths,
            r#"TASK
FROM "Projects"
WHERE !completed AND file.name = "Alpha"
SORT due ASC"#,
            None,
        )
        .expect("DQL should evaluate");

        assert_eq!(result.query_type, super::super::DqlQueryType::Task);
        assert_eq!(result.result_count, 1);
        assert_eq!(
            result.rows[0]["File"],
            Value::String("[[Projects/Alpha]]".to_string())
        );
        assert_eq!(
            result.rows[0]["text"],
            Value::String("Follow up [due:: 2026-04-02]".to_string())
        );
        assert_eq!(
            result.rows[0]["due"],
            Value::String("2026-04-02".to_string())
        );
        assert_eq!(
            result.rows[0]["visual"],
            Value::String("Follow up [due:: 2026-04-02]".to_string())
        );
        assert_eq!(
            result.rows[0]["path"],
            Value::String("Projects/Alpha.md".to_string())
        );
    }

    #[test]
    fn task_queries_include_child_tasks_when_parent_matches() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let result = evaluate_dql(
            &paths,
            r#"TASK
FROM "Dashboard"
WHERE text = "Write docs [due:: 2026-04-01]""#,
            None,
        )
        .expect("DQL should evaluate");

        assert_eq!(result.query_type, super::super::DqlQueryType::Task);
        assert_eq!(result.result_count, 2);
        assert_eq!(
            result.rows[0]["text"],
            Value::String("Write docs [due:: 2026-04-01]".to_string())
        );
        assert_eq!(
            result.rows[1]["text"],
            Value::String("Ship release [owner:: [[People/Bob]]]".to_string())
        );
    }

    #[test]
    fn evaluates_calendar_queries_from_expression_values() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join("Daily")).expect("daily dir should be created");
        fs::write(
            vault_root.join("Daily/2026-04-01.md"),
            "kind:: daily\nstatus:: planned\n",
        )
        .expect("first note should be written");
        fs::write(
            vault_root.join("Daily/2026-04-03.md"),
            "kind:: daily\nstatus:: shipped\n",
        )
        .expect("second note should be written");

        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let result = evaluate_dql(
            &paths,
            r#"CALENDAR file.day
FROM "Daily"
SORT file.name ASC"#,
            None,
        )
        .expect("DQL should evaluate");

        assert_eq!(result.query_type, super::super::DqlQueryType::Calendar);
        assert_eq!(result.columns, vec!["date", "File"]);
        assert_eq!(result.result_count, 2);
        assert_eq!(
            result.rows[0]["date"],
            Value::String("2026-04-01".to_string())
        );
        assert_eq!(
            result.rows[0]["File"],
            Value::String("[[Daily/2026-04-01]]".to_string())
        );
        assert_eq!(
            result.rows[1]["date"],
            Value::String("2026-04-03".to_string())
        );
    }

    #[test]
    fn evaluates_group_by_queries_with_null_keys_and_row_swizzling() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join("Notes")).expect("notes dir should be created");
        fs::write(vault_root.join("Notes/A.md"), "category:: alpha\n")
            .expect("first note should be written");
        fs::write(vault_root.join("Notes/B.md"), "category:: alpha\n")
            .expect("second note should be written");
        fs::write(vault_root.join("Notes/C.md"), "reviewed:: true\n")
            .expect("third note should be written");

        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let result = evaluate_dql(
            &paths,
            r#"TABLE rows.file.link AS pages, length(rows) AS count
FROM "Notes"
GROUP BY category
SORT key ASC"#,
            None,
        )
        .expect("DQL should evaluate");

        assert_eq!(result.query_type, super::super::DqlQueryType::Table);
        assert_eq!(result.columns, vec!["Group", "pages", "count"]);
        assert_eq!(result.result_count, 2);
        assert_eq!(result.rows[0]["Group"], Value::Null);
        assert_eq!(
            result.rows[0]["pages"],
            Value::Array(vec![Value::String("[[Notes/C]]".to_string())])
        );
        assert_eq!(result.rows[0]["count"].as_f64(), Some(1.0));
        assert_eq!(result.rows[1]["Group"], Value::String("alpha".to_string()));
        assert_eq!(
            result.rows[1]["pages"],
            Value::Array(vec![
                Value::String("[[Notes/A]]".to_string()),
                Value::String("[[Notes/B]]".to_string()),
            ])
        );
        assert_eq!(result.rows[1]["count"].as_f64(), Some(2.0));
    }

    #[test]
    fn evaluates_flatten_queries_for_arrays_scalars_and_sequential_composition() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let flattened_choices = evaluate_dql(
            &paths,
            r#"TABLE WITHOUT ID variant
FROM "Dashboard"
FLATTEN choices AS choice
FLATTEN list(choice, upper(choice)) AS variant
SORT variant ASC"#,
            None,
        )
        .expect("array flatten should evaluate");

        assert_eq!(flattened_choices.columns, vec!["variant"]);
        assert_eq!(flattened_choices.result_count, 4);
        assert_eq!(
            flattened_choices.rows,
            vec![
                serde_json::json!({ "variant": "ALPHA" }),
                serde_json::json!({ "variant": "BETA" }),
                serde_json::json!({ "variant": "alpha" }),
                serde_json::json!({ "variant": "beta" }),
            ]
        );

        let flattened_scalar = evaluate_dql(
            &paths,
            r#"TABLE WITHOUT ID plain
FROM "Dashboard"
FLATTEN plain"#,
            None,
        )
        .expect("scalar flatten should evaluate");

        assert_eq!(flattened_scalar.columns, vec!["plain"]);
        assert_eq!(flattened_scalar.result_count, 1);
        assert_eq!(
            flattened_scalar.rows,
            vec![serde_json::json!({ "plain": "alpha, beta" })]
        );
    }

    #[test]
    fn reports_unsupported_function_and_method_diagnostics_without_aborting_query() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let result = evaluate_dql(
            &paths,
            r#"TABLE status.slugify() AS slug, mystery(status) AS surprise
FROM "Projects"
SORT file.name ASC"#,
            None,
        )
        .expect("unsupported features should surface as diagnostics");

        assert_eq!(result.result_count, 2);
        assert_eq!(result.rows[0]["slug"], Value::Null);
        assert_eq!(result.rows[0]["surprise"], Value::Null);
        assert_eq!(result.rows[1]["slug"], Value::Null);
        assert_eq!(result.rows[1]["surprise"], Value::Null);
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unknown method `slugify`")));
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unknown function `mystery`")));
    }

    #[test]
    fn fixture_queries_cover_tags_regex_date_math_and_missing_links() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let tags = evaluate_dql(
            &paths,
            r"TABLE WITHOUT ID file.name AS page
FROM #project/list",
            None,
        )
        .expect("tag expansion query should evaluate");

        assert_eq!(tags.rows, vec![serde_json::json!({ "page": "Dashboard" })]);

        let computed = evaluate_dql(
            &paths,
            r#"TABLE WITHOUT ID
regexreplace(owner, "\[\[(.+)\]\]", "$1") AS owner_path,
dateformat(date("2026-04-03") - dur("1d"), "yyyy-MM-dd") AS previous_day,
[[Missing Person]].role AS missing_role
FROM "Dashboard""#,
            None,
        )
        .expect("computed query should evaluate");

        assert_eq!(
            computed.rows,
            vec![serde_json::json!({
                "owner_path": "People/Bob",
                "previous_day": "2026-04-02",
                "missing_role": Value::Null,
            })]
        );
    }

    #[test]
    fn evaluates_incoming_and_outgoing_from_sources_via_link_joins() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join("Notes")).expect("notes dir should be created");
        fs::write(vault_root.join("Notes/A.md"), "[[Notes/B]]\n")
            .expect("note A should be written");
        fs::write(vault_root.join("Notes/B.md"), "[[Notes/C]]\n")
            .expect("note B should be written");
        fs::write(vault_root.join("Notes/C.md"), "done\n").expect("note C should be written");

        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let incoming = evaluate_dql(
            &paths,
            r"TABLE WITHOUT ID file.name AS page
FROM [[Notes/B]]
SORT page ASC",
            None,
        )
        .expect("incoming source query should evaluate");
        assert_eq!(incoming.rows, vec![serde_json::json!({ "page": "A" })]);

        let outgoing = evaluate_dql(
            &paths,
            r"TABLE WITHOUT ID file.name AS page
FROM outgoing([[Notes/B]])
SORT page ASC",
            None,
        )
        .expect("outgoing source query should evaluate");
        assert_eq!(outgoing.rows, vec![serde_json::json!({ "page": "C" })]);
    }

    #[test]
    fn respects_configured_primary_and_group_column_names() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
        fs::create_dir_all(vault_root.join("Notes")).expect("notes dir should be created");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            "[dataview]\nprimary_column_name = \"Document\"\ngroup_column_name = \"Bucket\"\n",
        )
        .expect("config should be written");
        fs::write(vault_root.join("Notes/A.md"), "category:: alpha\n")
            .expect("note should be written");

        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let plain = evaluate_dql(&paths, r#"TABLE category FROM "Notes""#, None)
            .expect("plain query should evaluate");
        assert_eq!(plain.columns, vec!["Document", "category"]);

        let grouped = evaluate_dql(
            &paths,
            r#"TABLE length(rows) AS count
FROM "Notes"
GROUP BY category"#,
            None,
        )
        .expect("grouped query should evaluate");
        assert_eq!(grouped.columns, vec!["Bucket", "count"]);
    }

    #[test]
    fn respects_configured_timezone_in_dql_expressions() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should be created");
        fs::create_dir_all(vault_root.join("Notes")).expect("notes dir should be created");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            "[dataview]\ntimezone = \"+02:00\"\n",
        )
        .expect("config should be written");
        fs::write(vault_root.join("Notes/A.md"), "status:: draft\n")
            .expect("note should be written");

        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let result = evaluate_dql(
            &paths,
            r#"TABLE WITHOUT ID
dateformat(localtime(date("2026-04-17T22:00:00Z")), "yyyy-MM-dd HH:mm") AS local
FROM "Notes""#,
            None,
        )
        .expect("timezone query should evaluate");

        assert_eq!(
            result.rows,
            vec![serde_json::json!({ "local": "2026-04-18 00:00" })]
        );
    }

    #[test]
    fn this_file_name_resolves_to_source_note_not_current_row() {
        // `this.file.name` should reference the note *containing* the query, not each row being
        // evaluated.  `WHERE file.name != this.file.name` must exclude only the source note.
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir");
        // Three notes: the query lives in "Dashboard.md" (the source note).
        // The WHERE clause should return only the two non-Dashboard notes.
        fs::write(vault_root.join("Dashboard.md"), "# Dashboard\n").expect("Dashboard note");
        fs::write(vault_root.join("Alpha.md"), "# Alpha\n").expect("Alpha note");
        fs::write(vault_root.join("Beta.md"), "# Beta\n").expect("Beta note");

        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let result = evaluate_dql(
            &paths,
            "TABLE WITHOUT ID file.name AS Name\nWHERE file.name != this.file.name\nSORT file.name ASC",
            Some("Dashboard.md"),
        )
        .expect("DQL with this.file.name should evaluate");

        let names: Vec<&str> = result
            .rows
            .iter()
            .filter_map(|row| row["Name"].as_str())
            .collect();
        assert_eq!(names, vec!["Alpha", "Beta"], "Dashboard should be excluded");
    }

    #[test]
    fn this_file_name_without_current_file_falls_back_to_row() {
        // When no current_file is provided, `this` falls back to the current row (prior behaviour).
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir");
        fs::write(vault_root.join("Alpha.md"), "# Alpha\n").expect("Alpha note");

        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        // Without a source file, `this.file.name != file.name` is always false.
        let result = evaluate_dql(
            &paths,
            "TABLE WITHOUT ID file.name AS Name\nWHERE file.name != this.file.name",
            None,
        )
        .expect("DQL should evaluate");

        assert_eq!(
            result.result_count, 0,
            "without source note, this == current row so WHERE is always false"
        );
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);
        copy_dir_all(&source, destination);
    }

    fn copy_dir_all(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("destination should be created");
        for entry in fs::read_dir(source).expect("fixture dir should be readable") {
            let entry = entry.expect("fixture entry should load");
            let path = entry.path();
            let target = destination.join(entry.file_name());
            if path.is_dir() {
                copy_dir_all(&path, &target);
            } else {
                fs::copy(&path, &target).expect("fixture file should copy");
            }
        }
    }
}
