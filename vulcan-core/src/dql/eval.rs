use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::cache::CacheDatabase;
use crate::config::{load_vault_config, VaultConfig};
use crate::expression::ast::Expr;
use crate::expression::eval::{
    compare_values, evaluate, is_truthy, parse_wikilink_target,
    resolve_note_reference as resolve_lookup_note_reference, value_to_display, EvalContext,
};
use crate::expression::value::DataviewTimeZone;
use crate::file_metadata::FileMetadataResolver;
use crate::paths::VaultPaths;
use crate::permissions::{PermissionFilter, PermissionGuard};
use crate::predicate::{Decision, Dialect, Predicate, RecordValues};
use crate::properties::{
    hydrate_note_index_entries, load_note_index_with_filter, load_note_index_with_guard,
    load_note_index_with_guard_deferring_hydration, NoteRecord, PropertyError,
};
use crate::resolve_note_reference as resolve_vault_note_reference;
use crate::source::{SourceColumns, SourceExpr};

use super::ast::{DqlDataCommand, DqlLinkTarget, DqlNamedExpr, DqlProjection, DqlQuery};
use super::compile::{compile_dql, CompiledDqlCommand, CompiledDqlSourceExpr, CompiledWhereClause};
use super::{parse_dql, DqlDiagnostic};
use crate::expression::eval::{canonical_file_field_name, normalize_field_name};
use std::borrow::Cow;

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
    evaluate_dql_with_filter(paths, source, current_file, None)
}

pub fn evaluate_dql_with_filter(
    paths: &VaultPaths,
    source: &str,
    current_file: Option<&str>,
    filter: Option<&PermissionFilter>,
) -> Result<DqlQueryResult, DqlEvalError> {
    let query = parse_dql(source).map_err(DqlEvalError::Parse)?;
    evaluate_parsed_dql_with_filter(paths, &query, current_file, filter)
}

#[allow(clippy::too_many_lines)]
pub fn evaluate_parsed_dql(
    paths: &VaultPaths,
    query: &DqlQuery,
    current_file: Option<&str>,
) -> Result<DqlQueryResult, DqlEvalError> {
    evaluate_parsed_dql_with_filter(paths, query, current_file, None)
}

#[allow(clippy::too_many_lines)]
pub fn evaluate_parsed_dql_with_filter(
    paths: &VaultPaths,
    query: &DqlQuery,
    current_file: Option<&str>,
    filter: Option<&PermissionFilter>,
) -> Result<DqlQueryResult, DqlEvalError> {
    let config = load_vault_config(paths).config;
    let note_lookup = load_note_index_with_filter(paths, filter)?;
    evaluate_parsed_dql_with_note_index_and_config(
        paths,
        query,
        current_file,
        filter,
        &config,
        &note_lookup,
        NoteIndexScope::Authorized,
    )
}

/// Evaluate DQL inside the caller's complete read authority. Path/tag grants
/// and policy decisions select one note universe before hydration, so rows,
/// `file.inlinks`, and linked-note lookups never include hidden notes. Policy
/// failures are errors, not empty results.
pub fn evaluate_dql_with_guard(
    paths: &VaultPaths,
    source: &str,
    current_file: Option<&str>,
    guard: &dyn PermissionGuard,
) -> Result<DqlQueryResult, DqlEvalError> {
    let config = load_vault_config(paths).config;
    let query = parse_dql(source).map_err(DqlEvalError::Parse)?;
    let filter = guard.read_filter();
    let note_lookup = load_scoped_note_index(paths, &query, current_file, guard, &filter)?;
    evaluate_parsed_dql_with_note_index_and_config(
        paths,
        &query,
        current_file,
        Some(&filter),
        &config,
        &note_lookup,
        NoteIndexScope::Authorized,
    )
}

/// Load the note index `query` needs. When no expression can reach another
/// note's file object, only `this` and the notes its `FROM` selects (every
/// note without `FROM`) that a leading `WHERE` does not decide as
/// non-matches are hydrated (tags, links, inlinks, tasks, list items, inline
/// expressions); every other note keeps its stored fields and the aliases
/// link resolution reads. Otherwise every note is hydrated.
fn load_scoped_note_index(
    paths: &VaultPaths,
    query: &DqlQuery,
    current_file: Option<&str>,
    guard: &dyn PermissionGuard,
    filter: &PermissionFilter,
) -> Result<HashMap<String, NoteRecord>, DqlEvalError> {
    let compiled = compile_dql(query);
    let mut sources = compiled
        .commands
        .iter()
        .filter_map(|command| match command {
            CompiledDqlCommand::From(source) => Some(source),
            _ => None,
        });
    let (source, None) = (sources.next(), sources.next()) else {
        return Ok(load_note_index_with_guard(paths, guard)?);
    };
    if query_reaches_other_file_objects(query) {
        return Ok(load_note_index_with_guard(paths, guard)?);
    }
    let mut note_lookup = load_note_index_with_guard_deferring_hydration(paths, guard)?;
    let all_notes = sorted_notes(&note_lookup);
    let mut selected = match source {
        Some(source) => source_paths(
            paths,
            source,
            current_file,
            &note_lookup,
            &all_notes,
            Some(filter),
        )?,
        None => all_notes
            .iter()
            .map(|note| note.document_path.clone())
            .collect(),
    };
    // Rows a leading `WHERE` decides as non-matches from stored fields are
    // removed before anything reads their file objects.
    if let Some(where_clause) = leading_page_where(query.query_type, &compiled) {
        let this = current_file
            .and_then(|path| note_lookup.values().find(|note| note.document_path == path));
        let predicate = where_predicate_with_this(where_clause, this);
        let decided_out = all_notes
            .iter()
            .filter(|note| {
                predicate.decide(
                    Dialect::Dataview,
                    &RecordValues {
                        properties: &note.properties,
                        path: &note.document_path,
                        name: &note.file_name,
                        ext: &note.file_ext,
                    },
                ) == Decision::NoMatch
            })
            .map(|note| note.document_path.clone())
            .collect::<Vec<_>>();
        for path in decided_out {
            selected.remove(&path);
        }
    }
    selected.extend(current_file.map(ToString::to_string));
    hydrate_note_index_entries(paths, guard, &mut note_lookup, &selected)?;
    Ok(note_lookup)
}

/// The predicate of a `WHERE` that sees the page rows exactly as `FROM`
/// produced them: the first data command, in a page (non-task) query.
fn leading_page_where(
    query_type: super::DqlQueryType,
    compiled: &super::compile::CompiledDqlQuery,
) -> Option<&CompiledWhereClause> {
    if query_type == super::DqlQueryType::Task {
        return None;
    }
    match compiled
        .commands
        .iter()
        .find(|command| !matches!(command, CompiledDqlCommand::From(_)))?
    {
        CompiledDqlCommand::Where(where_clause) => Some(where_clause),
        _ => None,
    }
}

/// `where_clause`'s predicate with `this` bound to what the evaluator reads
/// for it: the file path, name, and extension of the note containing the
/// query, or null when there is none. Conditions comparing rows with
/// `this`, such as `file.name != this.file.name`, then lower too; the
/// original expression is still what undecided rows evaluate.
fn where_predicate_with_this<'a>(
    where_clause: &'a CompiledWhereClause,
    this: Option<&NoteRecord>,
) -> Cow<'a, Predicate> {
    if !mentions_this(&where_clause.expr) {
        return Cow::Borrowed(&where_clause.predicate);
    }
    Cow::Owned(Predicate::lower_dataview(&bind_this(
        &where_clause.expr,
        this,
    )))
}

fn is_this(expr: &Expr) -> bool {
    matches!(expr, Expr::Identifier(name) if normalize_field_name(name) == "this")
}

fn mentions_this(expr: &Expr) -> bool {
    let mut found = false;
    let _ = map_expr(expr, &mut |expr| {
        found |= is_this(expr);
        None
    });
    found
}

fn bind_this(expr: &Expr, this: Option<&NoteRecord>) -> Expr {
    map_expr(expr, &mut |expr| {
        let Expr::FieldAccess(base, field) = expr else {
            return is_this(expr)
                .then_some(this.is_none())
                .and_then(|missing| missing.then_some(Expr::Null));
        };
        match (base.as_ref(), this) {
            // A missing `this` is null, and so is every field of it.
            (base, None) if is_this(base) => Some(Expr::Null),
            (Expr::FieldAccess(inner, file), None)
                if is_this(inner) && normalize_field_name(file) == "file" =>
            {
                Some(Expr::Null)
            }
            (Expr::FieldAccess(inner, file), Some(note))
                if is_this(inner) && normalize_field_name(file) == "file" =>
            {
                match canonical_file_field_name(field).as_str() {
                    "path" => Some(Expr::Str(note.document_path.clone())),
                    "name" | "basename" => Some(Expr::Str(note.file_name.clone())),
                    "ext" => Some(Expr::Str(note.file_ext.clone())),
                    _ => None,
                }
            }
            _ => None,
        }
    })
}

/// Rebuild `expr`, replacing each subexpression for which `replace` returns
/// a value; lambda bodies are kept as written, since they bind names.
fn map_expr(expr: &Expr, replace: &mut impl FnMut(&Expr) -> Option<Expr>) -> Expr {
    if let Some(replacement) = replace(expr) {
        return replacement;
    }
    let mut map = |expr: &Expr| Box::new(map_expr(expr, replace));
    match expr {
        Expr::Array(items) => Expr::Array(items.iter().map(|item| *map(item)).collect()),
        Expr::Object(fields) => Expr::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), *map(value)))
                .collect(),
        ),
        Expr::FieldAccess(base, field) => Expr::FieldAccess(map(base), field.clone()),
        Expr::IndexAccess(base, index) => Expr::IndexAccess(map(base), map(index)),
        Expr::BinaryOp(left, operator, right) => Expr::BinaryOp(map(left), *operator, map(right)),
        Expr::UnaryOp(operator, operand) => Expr::UnaryOp(*operator, map(operand)),
        Expr::FunctionCall(name, args) => {
            Expr::FunctionCall(name.clone(), args.iter().map(|arg| *map(arg)).collect())
        }
        Expr::MethodCall(base, method, args) => Expr::MethodCall(
            map(base),
            method.clone(),
            args.iter().map(|arg| *map(arg)).collect(),
        ),
        Expr::Lambda(..)
        | Expr::Null
        | Expr::Bool(_)
        | Expr::Number(_)
        | Expr::Str(_)
        | Expr::Regex { .. }
        | Expr::Identifier(_)
        | Expr::FormulaRef(_) => expr.clone(),
    }
}

/// Whether evaluating `query` can build the file object (and so read the
/// hydrated fields) of a note other than its rows and `this`: `.file` on anything but
/// `this`, `asFile()`, or indexing that could name `file`. Conservative.
fn query_reaches_other_file_objects(query: &DqlQuery) -> bool {
    let commands = query.commands.iter().flat_map(|command| match command {
        DqlDataCommand::Where(expr) => vec![expr],
        DqlDataCommand::Sort(keys) => keys.iter().map(|key| &key.expr).collect(),
        DqlDataCommand::GroupBy(named) | DqlDataCommand::Flatten(named) => vec![&named.expr],
        DqlDataCommand::From(_) | DqlDataCommand::Limit(_) => Vec::new(),
    });
    query
        .table_columns
        .iter()
        .map(|column| &column.expr)
        .chain(query.list_expression.iter())
        .chain(query.calendar_expression.iter())
        .chain(commands)
        .any(expr_reaches_other_file_objects)
}

fn expr_reaches_other_file_objects(expr: &Expr) -> bool {
    match expr {
        Expr::FieldAccess(base, field) => {
            (field.eq_ignore_ascii_case("file")
                && !matches!(&**base, Expr::Identifier(name) if name.eq_ignore_ascii_case("this")))
                || expr_reaches_other_file_objects(base)
        }
        Expr::IndexAccess(base, key) => {
            !matches!(&**key, Expr::Number(_) | Expr::Str(_))
                || matches!(&**key, Expr::Str(name) if name.eq_ignore_ascii_case("file"))
                || expr_reaches_other_file_objects(base)
        }
        Expr::MethodCall(base, method, args) => {
            method.eq_ignore_ascii_case("asFile")
                || expr_reaches_other_file_objects(base)
                || args.iter().any(expr_reaches_other_file_objects)
        }
        Expr::FunctionCall(_, args) | Expr::Array(args) => {
            args.iter().any(expr_reaches_other_file_objects)
        }
        Expr::Object(fields) => fields
            .iter()
            .any(|(_, value)| expr_reaches_other_file_objects(value)),
        Expr::BinaryOp(left, _, right) => {
            expr_reaches_other_file_objects(left) || expr_reaches_other_file_objects(right)
        }
        Expr::UnaryOp(_, operand) => expr_reaches_other_file_objects(operand),
        Expr::Lambda(_, body) => expr_reaches_other_file_objects(body),
        Expr::FormulaRef(_) => true,
        Expr::Null
        | Expr::Bool(_)
        | Expr::Number(_)
        | Expr::Str(_)
        | Expr::Regex { .. }
        | Expr::Identifier(_) => false,
    }
}

pub(crate) fn evaluate_dql_with_note_index_and_config(
    paths: &VaultPaths,
    source: &str,
    current_file: Option<&str>,
    filter: Option<&PermissionFilter>,
    config: &VaultConfig,
    note_lookup: &HashMap<String, NoteRecord>,
) -> Result<DqlQueryResult, DqlEvalError> {
    let query = parse_dql(source).map_err(DqlEvalError::Parse)?;
    evaluate_parsed_dql_with_note_index_and_config(
        paths,
        &query,
        current_file,
        filter,
        config,
        note_lookup,
        NoteIndexScope::Unchecked,
    )
}

#[allow(clippy::too_many_lines)]
/// Whether a note index passed to DQL evaluation is already limited to the
/// read scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoteIndexScope {
    /// Loaded through the same filter or guard. Its notes may be partially
    /// hydrated (no tags), so they are not checked again.
    Authorized,
    /// Supplied by the caller; notes outside the filter are dropped, which
    /// needs their tags.
    Unchecked,
}

/// A copy of `note_lookup` without the notes outside `filter`, or `None`
/// when nothing needs removing. Tag-aware, matching the SQL scope; a
/// path-only check would drop notes granted by tag.
fn notes_outside_scope_removed(
    note_lookup: &HashMap<String, NoteRecord>,
    filter: Option<&PermissionFilter>,
    index_scope: NoteIndexScope,
) -> Option<HashMap<String, NoteRecord>> {
    let allowed = |note: &NoteRecord| {
        filter.is_none_or(|filter| {
            filter
                .path_permission()
                .is_allowed_with_tags(&note.document_path, &note.tags)
        })
    };
    (index_scope == NoteIndexScope::Unchecked && note_lookup.values().any(|note| !allowed(note)))
        .then(|| {
            note_lookup
                .iter()
                .filter(|(_, note)| allowed(note))
                .map(|(path, note)| (path.clone(), note.clone()))
                .collect()
        })
}

#[allow(clippy::too_many_lines)]
pub(crate) fn evaluate_parsed_dql_with_note_index_and_config(
    paths: &VaultPaths,
    query: &DqlQuery,
    current_file: Option<&str>,
    filter: Option<&PermissionFilter>,
    config: &VaultConfig,
    note_lookup: &HashMap<String, NoteRecord>,
    index_scope: NoteIndexScope,
) -> Result<DqlQueryResult, DqlEvalError> {
    let time_zone = DataviewTimeZone::parse(config.dataview.timezone.as_deref());
    let compiled = compile_dql(query);
    let mut diagnostics = DqlDiagnosticCollector::default();
    let filtered_note_lookup = notes_outside_scope_removed(note_lookup, filter, index_scope);
    let note_lookup = filtered_note_lookup.as_ref().unwrap_or(note_lookup);
    let all_notes = sorted_notes(note_lookup);
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
        rows_for_source(
            paths,
            query,
            source,
            current_file,
            note_lookup,
            &all_notes,
            filter,
        )?
    } else {
        default_rows(query, &all_notes)
    };
    // Resolve the note that contains the query, used as the `this` reference in expressions.
    // When the query is embedded in a note (e.g. a Dataview code block), `current_file` names
    // that note so `WHERE file.name != this.file.name` can filter it out.
    let source_note =
        current_file.and_then(|path| note_lookup.values().find(|n| n.document_path == path));
    let mut page_rows_are_pristine = query.query_type != super::DqlQueryType::Task;

    for command in &compiled.commands {
        match command {
            CompiledDqlCommand::From(_) => {}
            CompiledDqlCommand::Where(where_clause) => {
                // Page rows still hold exactly their note's properties, so the
                // shared Dataview predicate decides what it can; every other
                // row is evaluated (QRY.1).
                let bound = where_predicate_with_this(where_clause, source_note);
                let predicate = (page_rows_are_pristine && bound.is_useful()).then_some(&*bound);
                rows = apply_where_expression(
                    rows,
                    &where_clause.expr,
                    predicate,
                    query,
                    note_lookup,
                    source_note,
                    time_zone,
                    &mut diagnostics,
                )?;
            }
            CompiledDqlCommand::Sort(keys) => {
                let mut decorated = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut values = Vec::with_capacity(keys.len());
                    for key in keys {
                        let value = match row.evaluate_with_source(
                            &key.expr,
                            note_lookup,
                            time_zone,
                            source_note,
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
                rows = apply_group_by(rows, named_expr, note_lookup, time_zone, &mut diagnostics)?;
                page_rows_are_pristine = false;
            }
            CompiledDqlCommand::Flatten(named_expr) => {
                rows = apply_flatten(rows, named_expr, note_lookup, time_zone, &mut diagnostics)?;
                page_rows_are_pristine = false;
            }
        }
    }

    let mut result = render_result(
        query,
        &config.dataview.primary_column_name,
        &config.dataview.group_column_name,
        rows,
        note_lookup,
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
        } else {
            // No source note (e.g. CLI invocation) — `this` should resolve to null so that
            // `file.name != this.file.name` is vacuously true rather than always false.
            ctx = ctx.with_this_null_when_missing();
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

fn sorted_notes(note_lookup: &HashMap<String, NoteRecord>) -> Vec<&NoteRecord> {
    let mut notes = note_lookup.values().collect::<Vec<_>>();
    notes.sort_by(|left, right| left.document_path.cmp(&right.document_path));
    notes
}

fn default_rows(query: &DqlQuery, notes: &[&NoteRecord]) -> Vec<ExecutionRow> {
    match query.query_type {
        super::DqlQueryType::Task => notes
            .iter()
            .flat_map(|note| task_rows_for_note(note))
            .collect(),
        _ => notes.iter().map(|note| ExecutionRow::page(note)).collect(),
    }
}

fn rows_for_source(
    paths: &VaultPaths,
    query: &DqlQuery,
    source: &CompiledDqlSourceExpr,
    current_file: Option<&str>,
    note_lookup: &HashMap<String, NoteRecord>,
    all_notes: &[&NoteRecord],
    filter: Option<&PermissionFilter>,
) -> Result<Vec<ExecutionRow>, DqlEvalError> {
    let source_paths = source_paths(paths, source, current_file, note_lookup, all_notes, filter)?;
    // `all_notes` is already sorted by path.
    let notes = all_notes
        .iter()
        .copied()
        .filter(|note| source_paths.contains(note.document_path.as_str()))
        .collect::<Vec<_>>();
    Ok(default_rows(query, &notes))
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
#[allow(clippy::too_many_arguments)]
fn apply_where_expression(
    rows: Vec<ExecutionRow>,
    expr: &crate::expression::ast::Expr,
    predicate: Option<&Predicate>,
    query: &DqlQuery,
    note_lookup: &HashMap<String, NoteRecord>,
    source_note: Option<&NoteRecord>,
    time_zone: DataviewTimeZone,
    diagnostics: &mut DqlDiagnosticCollector,
) -> Result<Vec<ExecutionRow>, DqlEvalError> {
    let mut decorated = Vec::with_capacity(rows.len());
    let mut directly_matched_task_ids = HashSet::new();

    for row in rows {
        // A decided row is exact: deciding evaluates nothing that could
        // report a diagnostic.
        let decision = predicate.map_or(Decision::Undecided, |predicate| {
            predicate.decide(
                Dialect::Dataview,
                &RecordValues {
                    properties: &row.note.properties,
                    path: &row.note.document_path,
                    name: &row.note.file_name,
                    ext: &row.note.file_ext,
                },
            )
        });
        if decision != Decision::Undecided {
            decorated.push((decision == Decision::Match, row));
            continue;
        }
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

/// The readable notes a `FROM` clause selects, in one query.
fn source_paths(
    paths: &VaultPaths,
    source: &CompiledDqlSourceExpr,
    current_file: Option<&str>,
    note_lookup: &HashMap<String, NoteRecord>,
    all_notes: &[&NoteRecord],
    permission_filter: Option<&PermissionFilter>,
) -> Result<HashSet<String>, DqlEvalError> {
    let source = resolve_source(source, current_file, note_lookup, all_notes)?;
    let database =
        CacheDatabase::open(paths).map_err(|error| DqlEvalError::Message(error.to_string()))?;
    let permission_sql =
        permission_filter.map(|filter| filter.document_scope_sql("_permission_documents"));
    let mut params = permission_sql
        .as_ref()
        .map(|sql| {
            sql.params
                .iter()
                .cloned()
                .map(rusqlite::types::Value::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut sql = permission_sql
        .as_ref()
        .map_or_else(String::new, |sql| sql.cte.clone());
    sql.push_str("SELECT documents.path FROM documents WHERE documents.extension = 'md' AND ");
    sql.push_str(&source.render_sql(&SourceColumns::DOCUMENTS, &mut params));
    if let Some(permission_sql) = permission_sql.as_ref() {
        sql.push_str(&permission_sql.clause);
    }
    let mut statement = database
        .connection()
        .prepare(&sql)
        .map_err(|error| DqlEvalError::Message(error.to_string()))?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| DqlEvalError::Message(error.to_string()))?;
    rows.collect::<Result<HashSet<_>, _>>()
        .map_err(|error| DqlEvalError::Message(error.to_string()))
}

/// Resolve DQL's vault-dependent sources: a path names a folder or a file
/// depending on what exists, and link sources name resolved notes.
fn resolve_source(
    source: &CompiledDqlSourceExpr,
    current_file: Option<&str>,
    note_lookup: &HashMap<String, NoteRecord>,
    all_notes: &[&NoteRecord],
) -> Result<SourceExpr, DqlEvalError> {
    let resolve = |inner| resolve_source(inner, current_file, note_lookup, all_notes);
    Ok(match source {
        CompiledDqlSourceExpr::Tag(tag) => SourceExpr::Tag(tag.clone()),
        CompiledDqlSourceExpr::Path(path) => path_source(path, all_notes),
        CompiledDqlSourceExpr::IncomingLink(target) => SourceExpr::LinksTo(
            resolve_source_target(target, current_file, note_lookup)?
                .document_id
                .clone(),
        ),
        CompiledDqlSourceExpr::OutgoingLink(target) => SourceExpr::LinkedFrom(
            resolve_source_target(target, current_file, note_lookup)?
                .document_id
                .clone(),
        ),
        CompiledDqlSourceExpr::Not(inner) => SourceExpr::Not(Box::new(resolve(inner)?)),
        CompiledDqlSourceExpr::And(left, right) => {
            SourceExpr::And(vec![resolve(left)?, resolve(right)?])
        }
        CompiledDqlSourceExpr::Or(left, right) => {
            SourceExpr::Or(vec![resolve(left)?, resolve(right)?])
        }
    })
}

fn path_source(path: &str, all_notes: &[&NoteRecord]) -> SourceExpr {
    if Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return SourceExpr::Path(path.to_string());
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

    if (path.contains('/') && file_exists) || !folder_exists {
        SourceExpr::Or(vec![
            SourceExpr::Path(normalized.to_string()),
            SourceExpr::Path(exact_file),
        ])
    } else {
        SourceExpr::Folder(normalized.to_string())
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

    use crate::config::load_vault_config;
    use crate::permissions::{PathPermission, PermissionFilter, ResourceSpecifier};
    use crate::properties::load_note_index;
    use crate::{scan_vault, ScanMode, VaultPaths};

    use super::*;

    #[test]
    fn evaluates_table_queries_against_dataview_fixture() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
    #[allow(clippy::too_many_lines)]
    fn where_predicates_match_full_evaluation() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".vulcan")).unwrap();
        for (path, contents) in [
            ("A.md", "---\nstatus: done\npriority: 3\n---\n"),
            ("B.md", "---\nstatus: open\npriority: \"3\"\n---\n"),
            ("C.md", "no frontmatter\n"),
            ("D.md", "---\nstatus:\npriority: 1\n---\n"),
            ("E.md", "---\nstatus: [done]\nflag: true\n---\n"),
            ("Daily/2026-01-01.md", "---\nstatus: 2026-01-02\n---\n"),
        ] {
            let target = root.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, contents).unwrap();
        }
        let paths = VaultPaths::new(root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        let names = |source: &str| {
            let mut names = evaluate_dql(&paths, source, None)
                .expect("query should evaluate")
                .rows
                .into_iter()
                .map(|row| row["File"].as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>();
            names.sort();
            names
        };
        // Cases the former exact SQL path got wrong, against the evaluator.
        for (clause, expected) in [
            (
                "status != \"done\"",
                vec!["B", "C", "D", "E", "Daily/2026-01-01"],
            ),
            ("status != null", vec!["A", "B", "E", "Daily/2026-01-01"]),
            ("status = null", vec!["C", "D"]),
            ("priority > 2", vec!["A"]),
        ] {
            let source = format!("TABLE WITHOUT ID file.path AS File WHERE {clause}");
            let mut expected = expected
                .into_iter()
                .map(|name| format!("{name}.md"))
                .collect::<Vec<_>>();
            expected.sort();
            let table = |source: &str| {
                let mut paths = evaluate_dql(&paths, source, None)
                    .unwrap()
                    .rows
                    .into_iter()
                    .map(|row| row["File"].as_str().unwrap().to_string())
                    .collect::<Vec<_>>();
                paths.sort();
                paths
            };
            assert_eq!(table(&source), expected, "{clause}");
        }
        // Lowered and fully evaluated forms agree on every shape.
        for clause in [
            "status = \"done\"",
            "status != \"done\"",
            "status < \"p\"",
            "status >= \"done\"",
            "priority = 3",
            "priority != 3",
            "priority <= 1",
            "flag = true",
            "status = null OR priority > 2",
            "status = \"open\" AND priority = \"3\"",
            "file.name = \"2026-01-01\"",
            "file.path != \"A.md\" AND missing = 1",
            "status = \"done\" AND length(file.name) > 0",
        ] {
            let lowered = format!("TABLE WITHOUT ID file.path AS File WHERE {clause}");
            let evaluated = format!("TABLE WITHOUT ID file.path AS File WHERE true AND ({clause})");
            assert_eq!(names(&lowered), names(&evaluated), "{clause}");
        }
        // `this` binds to the note containing the query, or null without one.
        let with_this = |source: &str, this: Option<&str>| {
            let mut names = evaluate_dql(&paths, source, this)
                .expect("query should evaluate")
                .rows
                .into_iter()
                .map(|row| row["File"].as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>();
            names.sort();
            names
        };
        for clause in [
            "file.name != this.file.name",
            "file.name = this.file.name",
            "file.path != this.file.path AND status = \"done\"",
            "file.ext = this.file.ext AND priority > 2",
            "this.status = null AND status = \"open\"",
            "this = null OR status = \"open\"",
            "file.name != this.file.name AND this.file.mtime > 0",
        ] {
            for this in [None, Some("B.md"), Some("Daily/2026-01-01.md")] {
                let lowered = format!("TABLE WITHOUT ID file.path AS File WHERE {clause}");
                let evaluated =
                    format!("TABLE WITHOUT ID file.path AS File WHERE true AND ({clause})");
                assert_eq!(
                    with_this(&lowered, this),
                    with_this(&evaluated, this),
                    "{clause} in {this:?}"
                );
            }
        }
        // The bound condition decides rows without evaluation.
        let compiled = compile_dql(&parse_dql("LIST WHERE file.name != this.file.name").unwrap());
        let Some(CompiledDqlCommand::Where(where_clause)) = compiled.commands.first() else {
            panic!("expected WHERE");
        };
        assert!(!where_clause.predicate.is_useful());
        assert!(where_predicate_with_this(where_clause, None).is_useful());
    }

    #[test]
    fn non_ascii_sources_and_literals_select_their_notes() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".vulcan")).unwrap();
        fs::create_dir_all(root.join("Café")).unwrap();
        fs::write(root.join("Café/Crème.md"), "---\nstatus: brûlée\n---\n").unwrap();
        fs::write(root.join("Other.md"), "---\nstatus: brûlée\n---\n").unwrap();
        let paths = VaultPaths::new(root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        for source in [
            "LIST FROM \"Café\"",
            "LIST FROM \"Café\" WHERE status = \"brûlée\"",
            "LIST WHERE file.folder = \"Café\"",
        ] {
            let result = evaluate_dql(&paths, source, None).expect("query should evaluate");
            assert_eq!(result.result_count, 1, "{source}");
        }
    }

    #[test]
    fn from_sources_select_byte_exact_folders_and_nested_tags() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".vulcan")).unwrap();
        for (path, contents) in [
            ("Projects/A.md", "#project\n[[B]]\n"),
            ("projects/B.md", "#project/sub\n"),
            ("A_b/C.md", "#projects\n[[A]]\n"),
            ("Axb/D.md", "[[A]]\n"),
        ] {
            let target = root.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, contents).unwrap();
        }
        let paths = VaultPaths::new(root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        let count = |source: &str| {
            evaluate_dql(&paths, source, None)
                .expect("query should evaluate")
                .result_count
        };
        // Folders are byte-exact: no case folding, and `_` is not a wildcard.
        assert_eq!(count("LIST FROM \"projects\""), 1);
        assert_eq!(count("LIST FROM \"Projects\""), 1);
        assert_eq!(count("LIST FROM \"A_b\""), 1);
        // Tags include nested tags but not tags sharing a prefix.
        assert_eq!(count("LIST FROM #project"), 2);
        // Combinations and link sources resolve in one query.
        assert_eq!(count("LIST FROM #project AND -\"projects\""), 1);
        assert_eq!(count("LIST FROM [[A]]"), 2);
        assert_eq!(count("LIST FROM outgoing([[A]])"), 1);
        assert_eq!(count("LIST FROM [[A]] AND -#projects"), 1);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn scoped_hydration_equals_full_hydration() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".vulcan")).unwrap();
        for (path, contents) in [
            (
                "A/One.md",
                "---\ntags: [t]\nstatus: open\nparent: '[[Two]]'\n---\n- item [[Two]]\n- [ ] task one\n  - child\n",
            ),
            ("A/Three.md", "---\nstatus: open\n---\n- a\n- [x] done\n"),
            (
                "B/Two.md",
                "---\ntags: [t]\naliases: [Deux]\nstatus: closed\n---\n- b item\n- [ ] task two\nx:: `= this.status`\n",
            ),
            ("Here.md", "---\ntags: [here]\n---\n- here\n- [ ] here task\n[[One]]\n"),
        ] {
            let target = root.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, contents).unwrap();
        }
        let paths = VaultPaths::new(root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        let guard = crate::permissions::ProfilePermissionGuard::new(
            &paths,
            crate::permissions::resolve_permission_profile(&paths, None).unwrap(),
        );
        let filter = guard.read_filter();
        let config = load_vault_config(&paths).config;
        let full = load_note_index_with_guard(&paths, &guard).unwrap();
        for (source, reaches) in [
            (
                "TABLE length(file.lists) AS lists, length(file.tasks) AS tasks FROM \"A\"",
                false,
            ),
            ("TASK FROM \"A\"", false),
            ("LIST length(file.lists) FROM #t", false),
            ("TABLE length(this.file.lists) AS here FROM \"A\"", false),
            ("TABLE length(parent.file.lists) AS parent FROM \"A\"", true),
            (
                "TABLE map(file.outlinks, (l) => length(l.file.lists)) AS out FROM \"A\"",
                true,
            ),
            (
                "TABLE rows.file.lists AS lists FROM \"A\" GROUP BY status",
                true,
            ),
            ("TABLE length(file.lists) AS lists", false),
            (
                "TABLE file.tags AS tags, file.inlinks AS inlinks, file.outlinks AS out, \
                 file.etags AS etags, file.aliases AS aliases FROM \"A\" OR \"B\"",
                false,
            ),
            (
                "TABLE this.file.tags AS here, this.file.inlinks AS inl FROM \"A\"",
                false,
            ),
            (
                "TABLE parent.status AS ps, [[Deux]].status AS alias FROM \"A\"",
                false,
            ),
            ("LIST FROM [[Deux]]", false),
            ("LIST FROM outgoing([[Deux]])", false),
            (
                "TABLE file.tasks.text AS tasks FROM #t WHERE file.hasTag(\"t\")",
                false,
            ),
            ("TABLE parent.file.tags AS ptags FROM \"A\"", true),
            // Without FROM, a leading WHERE bounds what is hydrated.
            (
                "TABLE file.tags AS tags, file.inlinks AS inl WHERE status = \"open\"",
                false,
            ),
            ("TABLE file.lists AS lists WHERE status != \"open\"", false),
            ("LIST WHERE contains(file.tags, \"#t\")", false),
            (
                "TABLE file.tasks.text AS tasks FROM #t WHERE status = \"closed\" GROUP BY status",
                false,
            ),
            // A WHERE after SORT/LIMIT or FLATTEN, and task queries, do not.
            (
                "TABLE file.tags AS tags SORT file.name DESC LIMIT 2 WHERE status = \"open\"",
                false,
            ),
            (
                "TABLE x FLATTEN file.tags AS x WHERE status = \"open\"",
                false,
            ),
            ("TASK WHERE status = \"open\"", false),
            (
                "TABLE file.tags AS tags WHERE file.name != this.file.name AND status = \"open\"",
                false,
            ),
        ] {
            let query = parse_dql(source).unwrap();
            assert_eq!(
                query_reaches_other_file_objects(&query),
                reaches,
                "{source}"
            );
            let scoped = evaluate_dql_with_guard(&paths, source, Some("Here.md"), &guard).unwrap();
            let expected = evaluate_parsed_dql_with_note_index_and_config(
                &paths,
                &query,
                Some("Here.md"),
                Some(&filter),
                &config,
                &full,
                NoteIndexScope::Unchecked,
            )
            .unwrap();
            assert_eq!(
                serde_json::to_value(&scoped).unwrap(),
                serde_json::to_value(&expected).unwrap(),
                "{source}"
            );
        }
        // Lists load only for the FROM selection and `this`.
        let query = parse_dql("LIST FROM \"A\"").unwrap();
        let scoped =
            load_scoped_note_index(&paths, &query, Some("Here.md"), &guard, &filter).unwrap();
        let lists = |name: &str| {
            scoped
                .values()
                .find(|note| note.document_path == name)
                .unwrap()
                .list_items
                .len()
        };
        assert_eq!(lists("A/One.md"), 3);
        assert_eq!(lists("Here.md"), 2);
        assert_eq!(lists("B/Two.md"), 0);
    }

    #[test]
    fn guarded_dql_applies_tag_grants_policy_and_hidden_backlinks() {
        struct Guard {
            grant: crate::permissions::PermissionGrant,
            fail: bool,
        }
        impl PermissionGuard for Guard {
            fn profile_name(&self) -> &'static str {
                "test"
            }
            fn grant(&self) -> &crate::permissions::PermissionGrant {
                &self.grant
            }
            fn has_policy_hook(&self) -> bool {
                true
            }
            fn check_policy_decision(
                &self,
                action: &'static str,
                resource: Option<&str>,
            ) -> Result<(), crate::permissions::PermissionError> {
                let resource = resource.map(ToOwned::to_owned);
                if resource.as_deref() != Some("Policy.md") {
                    return Ok(());
                }
                Err(if self.fail {
                    crate::permissions::PermissionError::PolicyHookFailed {
                        profile: "test".into(),
                        action,
                        resource,
                        reason: "broken".into(),
                    }
                } else {
                    crate::permissions::PermissionError::PolicyHookDenied {
                        profile: "test".into(),
                        action,
                        resource,
                        reason: "denied".into(),
                    }
                })
            }
        }
        let temp_dir = tempdir().expect("temp dir should be created");
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".vulcan")).unwrap();
        for (path, contents) in [
            ("Target.md", "---\ntags: [visible]\n---\n"),
            ("Linker.md", "---\ntags: [visible]\n---\n[[Target]]\n"),
            ("Policy.md", "---\ntags: [visible]\n---\n[[Target]]\n"),
            (
                "Secret.md",
                "---\ntags: [visible, secret]\n---\n[[Target]]\n",
            ),
            ("Untagged.md", "[[Target]]\n"),
        ] {
            fs::write(root.join(path), contents).unwrap();
        }
        let paths = VaultPaths::new(root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        let mut grant = crate::permissions::resolve_permission_profile(&paths, None)
            .unwrap()
            .grant;
        grant.read = PathPermission {
            allow: vec![ResourceSpecifier::Tag("visible".into())],
            deny: vec![ResourceSpecifier::Tag("secret".into())],
        };
        let guard = Guard { grant, fail: false };

        let result = evaluate_dql_with_guard(
            &paths,
            "TABLE length(file.inlinks) AS inlinks SORT file.name ASC",
            None,
            &guard,
        )
        .expect("guarded DQL should evaluate");
        let rows = result
            .rows
            .iter()
            .map(|row| (row["File"].clone(), row["inlinks"].clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![
                (Value::String("[[Linker]]".into()), Value::from(0)),
                (Value::String("[[Target]]".into()), Value::from(1)),
            ]
        );

        let failing = Guard {
            grant: guard.grant.clone(),
            fail: true,
        };
        let error = evaluate_dql_with_guard(&paths, "LIST", None, &failing)
            .expect_err("a broken policy must fail");
        assert!(error.to_string().contains("broken"), "{error}");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn guarded_from_sources_select_the_readable_part_of_their_notes() {
        struct Guard {
            grant: crate::permissions::PermissionGrant,
        }
        impl PermissionGuard for Guard {
            fn profile_name(&self) -> &'static str {
                "test"
            }
            fn grant(&self) -> &crate::permissions::PermissionGrant {
                &self.grant
            }
            fn has_policy_hook(&self) -> bool {
                true
            }
            fn check_policy_decision(
                &self,
                action: &'static str,
                resource: Option<&str>,
            ) -> Result<(), crate::permissions::PermissionError> {
                if resource != Some("Folder/Policy.md") {
                    return Ok(());
                }
                Err(crate::permissions::PermissionError::PolicyHookDenied {
                    profile: "test".into(),
                    action,
                    resource: resource.map(ToOwned::to_owned),
                    reason: "denied".into(),
                })
            }
        }
        let temp_dir = tempdir().expect("temp dir should be created");
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".vulcan")).unwrap();
        for (path, contents) in [
            ("Target.md", "---\ntags: [visible]\n---\n[[Folder/A]]\n"),
            (
                "Linker.md",
                "---\ntags: [visible]\n---\n[[Target]] [[Folder/A]] [[Secret]]\n",
            ),
            (
                "Secret.md",
                "---\ntags: [visible, secret]\n---\n[[Target]]\n",
            ),
            ("Untagged.md", "[[Target]]\n"),
            (
                "Folder/A.md",
                "---\ntags: [visible, project/x]\n---\n[[Linker]]\n",
            ),
            (
                "Folder/B.md",
                "---\ntags: [visible, secret, project]\n---\n",
            ),
            (
                "Folder/Policy.md",
                "---\ntags: [visible, project]\n---\n[[Target]]\n",
            ),
            ("Folder/C.md", "---\ntags: [visible]\n---\n[[Target]]\n"),
        ] {
            let target = root.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, contents).unwrap();
        }
        let paths = VaultPaths::new(root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        let mut grant = crate::permissions::resolve_permission_profile(&paths, None)
            .unwrap()
            .grant;
        grant.read = PathPermission {
            allow: vec![ResourceSpecifier::Tag("visible".into())],
            deny: vec![ResourceSpecifier::Tag("secret".into())],
        };
        let guard = Guard { grant };
        let selected = |source: &str, guarded: bool| {
            let query = format!("TABLE WITHOUT ID file.path AS p FROM {source}");
            let result = if guarded {
                evaluate_dql_with_guard(&paths, &query, Some("Target.md"), &guard)
            } else {
                evaluate_dql(&paths, &query, Some("Target.md"))
            }
            .unwrap_or_else(|error| panic!("{source}: {error}"));
            result
                .rows
                .iter()
                .map(|row| row["p"].as_str().unwrap().to_string())
                .collect::<BTreeSet<_>>()
        };
        let readable = selected("\"\" OR -\"\"", true);
        assert_eq!(
            readable,
            ["Folder/A.md", "Folder/C.md", "Linker.md", "Target.md"]
                .map(String::from)
                .into()
        );
        for source in [
            "\"Folder\"",
            "#project",
            "#visible",
            "[[Target]]",
            "[[]]",
            "outgoing([[Linker]])",
            "outgoing([[]])",
            "-\"Folder\"",
            "-[[Target]]",
            "[[Target]] OR #project",
            "\"Folder\" AND -#project",
            "-(#project OR [[Linker]])",
        ] {
            let unrestricted = selected(source, false);
            let expected = unrestricted
                .intersection(&readable)
                .cloned()
                .collect::<BTreeSet<_>>();
            assert_eq!(selected(source, true), expected, "{source}");
        }
        // A hidden target cannot be named as a link source.
        let error = evaluate_dql_with_guard(&paths, "LIST FROM [[Secret]]", None, &guard)
            .expect_err("a hidden link target must not resolve");
        assert!(error.to_string().contains("could not resolve"), "{error}");
    }

    #[test]
    fn evaluate_dql_with_filter_restricts_visible_notes() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let filter = PermissionFilter::new(PathPermission {
            allow: vec![ResourceSpecifier::Note("Projects/Alpha.md".to_string())],
            deny: Vec::new(),
        });
        let result = evaluate_dql_with_filter(
            &paths,
            r"LIST file.name SORT file.name ASC",
            None,
            Some(&filter),
        )
        .expect("filtered DQL should evaluate");

        assert_eq!(result.result_count, 1);
        assert_eq!(
            result.rows[0]["File"],
            Value::String("[[Projects/Alpha]]".to_string())
        );
    }

    #[test]
    fn evaluate_dql_with_filter_denies_linked_note_property_lookups() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        std::fs::create_dir_all(vault_root.join("Public")).expect("public directory");
        std::fs::create_dir_all(vault_root.join("Private")).expect("private directory");
        std::fs::write(
            vault_root.join("Public/Dashboard.md"),
            "---\ntarget: \"[[Private/Secret]]\"\n---\n# Dashboard\n",
        )
        .expect("public note");
        std::fs::write(
            vault_root.join("Private/Secret.md"),
            "---\nsecret: classified\n---\n# Secret\n",
        )
        .expect("private note");
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        let filter = PermissionFilter::new(PathPermission {
            allow: vec![ResourceSpecifier::Folder("Public/**".to_string())],
            deny: Vec::new(),
        });

        let result = evaluate_dql_with_filter(
            &paths,
            "LIST file.name WHERE target.secret = \"classified\"",
            None,
            Some(&filter),
        )
        .expect("filtered DQL should evaluate");

        assert_eq!(result.result_count, 0);
    }

    #[test]
    fn cached_dql_context_matches_one_shot_evaluation() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let source = r"LIST file.name
WHERE file.name != this.file.name
SORT file.name ASC";
        let config = load_vault_config(&paths).config;
        let note_index = load_note_index(&paths).expect("note index should load");

        let one_shot =
            evaluate_dql(&paths, source, Some("Dashboard.md")).expect("DQL should evaluate");
        let cached = evaluate_dql_with_note_index_and_config(
            &paths,
            source,
            Some("Dashboard.md"),
            None,
            &config,
            &note_index,
        )
        .expect("cached DQL should evaluate");

        assert_eq!(cached, one_shot);
    }

    #[test]
    fn evaluates_list_queries_with_expression_values() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
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
    fn this_file_name_without_current_file_resolves_to_null() {
        // When no current_file is provided (e.g. CLI invocation), `this` resolves to null so
        // that `file.name != this.file.name` is vacuously true — all notes pass the filter.
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir");
        fs::write(vault_root.join("Alpha.md"), "# Alpha\n").expect("Alpha note");

        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let result = evaluate_dql(
            &paths,
            "TABLE WITHOUT ID file.name AS Name\nWHERE file.name != this.file.name",
            None,
        )
        .expect("DQL should evaluate");

        assert_eq!(
            result.result_count, 1,
            "without source note, this resolves to null so all notes pass the filter"
        );
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);
        copy_dir_all(&source, destination);
        fs::create_dir_all(destination.join(".vulcan")).expect(".vulcan dir should be created");
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
