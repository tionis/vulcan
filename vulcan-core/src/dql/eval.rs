use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::config::load_vault_config;
use crate::expression::eval::{
    compare_values, evaluate, is_truthy, parse_wikilink_target, resolve_note_reference,
    value_to_display, EvalContext,
};
use crate::file_metadata::FileMetadataResolver;
use crate::paths::VaultPaths;
use crate::properties::{load_note_index, NoteRecord, PropertyError};

use super::ast::{DqlDataCommand, DqlLinkTarget, DqlNamedExpr, DqlProjection, DqlQuery};
use super::parse_dql;

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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DqlQueryResult {
    pub query_type: super::DqlQueryType,
    pub columns: Vec<String>,
    pub rows: Vec<Value>,
    pub result_count: usize,
}

pub fn evaluate_dql(
    paths: &VaultPaths,
    source: &str,
    current_file: Option<&str>,
) -> Result<DqlQueryResult, DqlEvalError> {
    let query = parse_dql(source).map_err(DqlEvalError::Parse)?;
    evaluate_parsed_dql(paths, &query, current_file)
}

pub fn evaluate_parsed_dql(
    paths: &VaultPaths,
    query: &DqlQuery,
    current_file: Option<&str>,
) -> Result<DqlQueryResult, DqlEvalError> {
    let config = load_vault_config(paths).config;
    let note_lookup = load_note_index(paths)?;
    let all_notes = sorted_notes(&note_lookup);
    let from_sources = query
        .commands
        .iter()
        .filter_map(|command| match command {
            DqlDataCommand::From(source) => Some(source),
            _ => None,
        })
        .collect::<Vec<_>>();

    if from_sources.len() > 1 {
        return Err(DqlEvalError::Message(
            "DQL queries may contain at most one FROM clause".to_string(),
        ));
    }

    let mut rows = if let Some(source) = from_sources.first() {
        rows_for_source(query, source, current_file, &note_lookup, &all_notes)?
    } else {
        default_rows(query, &all_notes)
    };

    for command in &query.commands {
        match command {
            DqlDataCommand::From(_) => {}
            DqlDataCommand::Where(expr) => {
                let mut filtered = Vec::with_capacity(rows.len());
                for row in rows {
                    let value = row.evaluate(expr, &note_lookup).map_err(|error| {
                        DqlEvalError::Message(format!(
                            "failed to evaluate WHERE for {}: {error}",
                            row.note.document_path
                        ))
                    })?;
                    if is_truthy(&value) {
                        filtered.push(row);
                    }
                }
                rows = filtered;
            }
            DqlDataCommand::Sort(keys) => {
                let mut decorated = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut values = Vec::with_capacity(keys.len());
                    for key in keys {
                        let value = row.evaluate(&key.expr, &note_lookup).map_err(|error| {
                            DqlEvalError::Message(format!(
                                "failed to evaluate SORT key for {}: {error}",
                                row.note.document_path
                            ))
                        })?;
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
            DqlDataCommand::Limit(limit) => rows.truncate(*limit),
            DqlDataCommand::GroupBy(DqlNamedExpr { .. }) => {
                return Err(DqlEvalError::Message(
                    "GROUP BY evaluation is not implemented yet".to_string(),
                ));
            }
            DqlDataCommand::Flatten(DqlNamedExpr { .. }) => {
                return Err(DqlEvalError::Message(
                    "FLATTEN evaluation is not implemented yet".to_string(),
                ));
            }
        }
    }

    render_result(
        query,
        &config.dataview.primary_column_name,
        rows,
        &note_lookup,
    )
}

#[derive(Debug, Clone)]
struct ExecutionRow {
    note: NoteRecord,
    fields: Map<String, Value>,
    ordinal: i64,
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
        }
    }

    fn task(note: &NoteRecord, fields: Map<String, Value>) -> Self {
        let ordinal = fields.get("line").and_then(Value::as_i64).unwrap_or(0);
        Self {
            note: note.clone(),
            fields,
            ordinal,
        }
    }

    fn evaluate(
        &self,
        expr: &crate::expression::ast::Expr,
        note_lookup: &HashMap<String, NoteRecord>,
    ) -> Result<Value, String> {
        let mut note = self.note.clone();
        note.properties = Value::Object(self.fields.clone());
        let formulas = BTreeMap::new();
        let ctx = EvalContext::new(&note, &formulas).with_note_lookup(note_lookup);
        evaluate(expr, &ctx)
    }

    fn identity(&self) -> (&str, i64) {
        (self.note.document_path.as_str(), self.ordinal)
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
    query: &DqlQuery,
    source: &super::DqlSourceExpr,
    current_file: Option<&str>,
    note_lookup: &HashMap<String, NoteRecord>,
    all_notes: &[NoteRecord],
) -> Result<Vec<ExecutionRow>, DqlEvalError> {
    let source_paths = source_paths(source, current_file, note_lookup, all_notes)?;
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
            .filter_map(|task| match task {
                Value::Object(fields) => Some(ExecutionRow::task(note, fields)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn source_paths(
    source: &super::DqlSourceExpr,
    current_file: Option<&str>,
    note_lookup: &HashMap<String, NoteRecord>,
    all_notes: &[NoteRecord],
) -> Result<HashSet<String>, DqlEvalError> {
    Ok(match source {
        super::DqlSourceExpr::Tag(tag) => all_notes
            .iter()
            .filter(|note| {
                note.tags
                    .iter()
                    .any(|candidate| candidate == tag || candidate.starts_with(&format!("{tag}/")))
            })
            .map(|note| note.document_path.clone())
            .collect(),
        super::DqlSourceExpr::Path(path) => all_notes
            .iter()
            .filter(|note| matches_path_source(note, path, all_notes))
            .map(|note| note.document_path.clone())
            .collect(),
        super::DqlSourceExpr::IncomingLink(target) => {
            let target_note = resolve_source_target(target, current_file, note_lookup)?;
            incoming_link_sources(target_note, note_lookup, all_notes)
        }
        super::DqlSourceExpr::OutgoingLink(target) => {
            let target_note = resolve_source_target(target, current_file, note_lookup)?;
            outgoing_link_sources(target_note, note_lookup)
        }
        super::DqlSourceExpr::Not(inner) => {
            let inner_paths = source_paths(inner, current_file, note_lookup, all_notes)?;
            all_notes
                .iter()
                .filter(|note| !inner_paths.contains(note.document_path.as_str()))
                .map(|note| note.document_path.clone())
                .collect()
        }
        super::DqlSourceExpr::And(left, right) => {
            let left_paths = source_paths(left, current_file, note_lookup, all_notes)?;
            let right_paths = source_paths(right, current_file, note_lookup, all_notes)?;
            left_paths.intersection(&right_paths).cloned().collect()
        }
        super::DqlSourceExpr::Or(left, right) => {
            let mut left_paths = source_paths(left, current_file, note_lookup, all_notes)?;
            left_paths.extend(source_paths(right, current_file, note_lookup, all_notes)?);
            left_paths
        }
    })
}

fn matches_path_source(note: &NoteRecord, path: &str, all_notes: &[NoteRecord]) -> bool {
    if Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return note.document_path == path;
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
        note.document_path == normalized || note.document_path == exact_file
    } else if folder_exists {
        note.document_path.starts_with(&folder_prefix)
    } else {
        note.document_path == normalized || note.document_path == exact_file
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
            resolve_note_reference(note_lookup, source_path, &target).ok_or_else(|| {
                DqlEvalError::Message(format!("could not resolve DQL source target {raw}"))
            })
        }
    }
}

fn incoming_link_sources(
    target_note: &NoteRecord,
    note_lookup: &HashMap<String, NoteRecord>,
    all_notes: &[NoteRecord],
) -> HashSet<String> {
    all_notes
        .iter()
        .filter(|note| {
            note.links.iter().any(|link| {
                let target = parse_wikilink_target(link);
                resolve_note_reference(note_lookup, &note.document_path, &target)
                    .is_some_and(|resolved| resolved.document_path == target_note.document_path)
            })
        })
        .map(|note| note.document_path.clone())
        .collect()
}

fn outgoing_link_sources(
    target_note: &NoteRecord,
    note_lookup: &HashMap<String, NoteRecord>,
) -> HashSet<String> {
    target_note
        .links
        .iter()
        .filter_map(|link| {
            let target = parse_wikilink_target(link);
            resolve_note_reference(note_lookup, &target_note.document_path, &target)
                .map(|note| note.document_path.clone())
        })
        .collect()
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

fn render_result(
    query: &DqlQuery,
    primary_column_name: &str,
    rows: Vec<ExecutionRow>,
    note_lookup: &HashMap<String, NoteRecord>,
) -> Result<DqlQueryResult, DqlEvalError> {
    match query.query_type {
        super::DqlQueryType::Table => {
            render_table_result(query, primary_column_name, rows, note_lookup)
        }
        super::DqlQueryType::List => {
            render_list_result(query, primary_column_name, rows, note_lookup)
        }
        super::DqlQueryType::Task => Ok(render_task_result(query, primary_column_name, rows)),
        super::DqlQueryType::Calendar => {
            render_calendar_result(query, primary_column_name, rows, note_lookup)
        }
    }
}

fn render_table_result(
    query: &DqlQuery,
    primary_column_name: &str,
    rows: Vec<ExecutionRow>,
    note_lookup: &HashMap<String, NoteRecord>,
) -> Result<DqlQueryResult, DqlEvalError> {
    let mut columns = Vec::new();
    if !query.without_id {
        columns.push(primary_column_name.to_string());
    }
    columns.extend(query.table_columns.iter().map(projection_label));

    let rendered_rows = rows
        .into_iter()
        .map(|row| render_table_row(&row, query, primary_column_name, note_lookup))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(DqlQueryResult {
        query_type: query.query_type,
        result_count: rendered_rows.len(),
        columns,
        rows: rendered_rows,
    })
}

fn render_table_row(
    row: &ExecutionRow,
    query: &DqlQuery,
    primary_column_name: &str,
    note_lookup: &HashMap<String, NoteRecord>,
) -> Result<Value, DqlEvalError> {
    let mut object = Map::new();
    if !query.without_id {
        object.insert(
            primary_column_name.to_string(),
            FileMetadataResolver::field(&row.note, "link"),
        );
    }

    for projection in &query.table_columns {
        let label = projection_label(projection);
        let value = row
            .evaluate(&projection.expr, note_lookup)
            .map_err(|error| {
                DqlEvalError::Message(format!(
                    "failed to evaluate TABLE column `{label}` for {}: {error}",
                    row.note.document_path
                ))
            })?;
        object.insert(label, value);
    }

    Ok(Value::Object(object))
}

fn render_list_result(
    query: &DqlQuery,
    primary_column_name: &str,
    rows: Vec<ExecutionRow>,
    note_lookup: &HashMap<String, NoteRecord>,
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
                object.insert(
                    primary_column_name.to_string(),
                    FileMetadataResolver::field(&row.note, "link"),
                );
            }
            if let Some(expr) = &query.list_expression {
                let value = row.evaluate(expr, note_lookup).map_err(|error| {
                    DqlEvalError::Message(format!(
                        "failed to evaluate LIST expression for {}: {error}",
                        row.note.document_path
                    ))
                })?;
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
            object.insert(
                primary_column_name.to_string(),
                FileMetadataResolver::field(&row.note, "link"),
            );
            Value::Object(object)
        })
        .collect::<Vec<_>>();

    DqlQueryResult {
        query_type: query.query_type,
        result_count: rendered_rows.len(),
        columns,
        rows: rendered_rows,
    }
}

fn render_calendar_result(
    query: &DqlQuery,
    primary_column_name: &str,
    rows: Vec<ExecutionRow>,
    note_lookup: &HashMap<String, NoteRecord>,
) -> Result<DqlQueryResult, DqlEvalError> {
    let expr = query.calendar_expression.as_ref().ok_or_else(|| {
        DqlEvalError::Message("CALENDAR queries require a date expression".to_string())
    })?;
    let mut rendered_rows = Vec::new();

    for row in rows {
        let value = row.evaluate(expr, note_lookup).map_err(|error| {
            DqlEvalError::Message(format!(
                "failed to evaluate CALENDAR expression for {}: {error}",
                row.note.document_path
            ))
        })?;
        if value.is_null() {
            continue;
        }

        let mut object = Map::new();
        object.insert("date".to_string(), value);
        object.insert(
            primary_column_name.to_string(),
            FileMetadataResolver::field(&row.note, "link"),
        );
        rendered_rows.push(Value::Object(object));
    }

    Ok(DqlQueryResult {
        query_type: query.query_type,
        result_count: rendered_rows.len(),
        columns: vec!["date".to_string(), primary_column_name.to_string()],
        rows: rendered_rows,
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
