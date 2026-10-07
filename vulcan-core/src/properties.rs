use crate::expression::ast::Expr;
use crate::expression::eval::{evaluate, is_truthy, EvalContext};
use crate::expression::functions::parse_duration_string;
use crate::expression::parse::Parser;
use crate::expression::value::DataviewTimeZone;
use crate::file_metadata::synthetic_file_link;
use crate::note_lookup::NoteLookup as _;
use crate::parser::{parse_document, types::InlineFieldKind};
use crate::permissions::{PermissionError, PermissionFilter, PermissionGuard};
use crate::tasknotes::{extract_tasknote, tasknotes_priority_weight, tasknotes_status_state};
use crate::{CacheDatabase, CacheError, VaultConfig, VaultPaths};
use regex::Regex;
use rusqlite::params_from_iter;
use rusqlite::types::Type as SqlType;
use rusqlite::types::Value as SqlValue;
use serde::Serialize;
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter, Write as _};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const PROPERTY_NAMESPACE_FRONTMATTER: &str = "frontmatter";
const PROPERTY_NAMESPACE_INLINE: &str = "inline";

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedProperties {
    pub raw_yaml: String,
    pub canonical_json: String,
    pub values: Vec<IndexedPropertyValue>,
    pub list_items: Vec<IndexedPropertyListItem>,
    pub diagnostics: Vec<PropertyTypeDiagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedPropertyValue {
    pub key: String,
    pub value_text: Option<String>,
    pub value_number: Option<f64>,
    pub value_bool: Option<bool>,
    pub value_date: Option<String>,
    pub value_type: String,
    pub origin: PropertyValueOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedPropertyListItem {
    pub key: String,
    pub item_index: usize,
    pub value_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyTypeDiagnostic {
    pub key: String,
    pub expected_type: String,
    pub actual_type: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PropertyCatalogEntry {
    pub key: String,
    pub count: usize,
    pub types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryFieldCatalogEntry {
    pub field: String,
    pub kind: String,
    pub supports: Vec<String>,
    pub types: Vec<String>,
    pub example: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyValueOrigin {
    Frontmatter,
    Inline,
    InlineParen,
    InlineBracket,
}

impl PropertyValueOrigin {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Frontmatter => "frontmatter",
            Self::Inline => "inline",
            Self::InlineParen => "inline_paren",
            Self::InlineBracket => "inline_bracket",
        }
    }
}

#[derive(Debug)]
pub enum PropertyError {
    Cache(CacheError),
    CacheMissing,
    InvalidFilter(String),
    Json(serde_json::Error),
    Sqlite(rusqlite::Error),
    Permission(PermissionError),
}

impl Display for PropertyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cache(error) => write!(formatter, "{error}"),
            Self::CacheMissing => {
                formatter.write_str("cache is missing; run `vulcan scan` before querying notes")
            }
            Self::InvalidFilter(filter) => write!(formatter, "invalid property filter: {filter}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::Sqlite(error) => write!(formatter, "{error}"),
            Self::Permission(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for PropertyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cache(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Permission(error) => Some(error),
            Self::CacheMissing | Self::InvalidFilter(_) => None,
        }
    }
}

impl From<CacheError> for PropertyError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<serde_json::Error> for PropertyError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<rusqlite::Error> for PropertyError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteQuery {
    pub filters: Vec<String>,
    pub sort_by: Option<String>,
    pub sort_descending: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NotesReport {
    pub filters: Vec<String>,
    pub sort_by: Option<String>,
    pub sort_descending: bool,
    pub notes: Vec<NoteRecord>,
    /// The note plan that produced `notes` (QRY.5); surfaced by `--explain`.
    #[serde(skip)]
    pub plan: Option<crate::plan::QueryPlanExplain>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoteRecord {
    #[serde(skip)]
    pub document_id: String,
    pub document_path: String,
    pub file_name: String,
    pub file_ext: String,
    pub file_mtime: i64,
    pub file_ctime: i64,
    pub file_size: i64,
    pub properties: Value,
    pub tags: Vec<String>,
    pub links: Vec<String>,
    #[serde(skip)]
    pub starred: bool,
    #[serde(skip)]
    pub inlinks: Vec<String>,
    #[serde(skip)]
    pub aliases: Vec<String>,
    #[serde(skip)]
    pub frontmatter: Value,
    #[serde(skip)]
    pub periodic_type: Option<String>,
    #[serde(skip)]
    pub periodic_date: Option<String>,
    #[serde(skip)]
    pub list_items: Vec<NoteListItemRecord>,
    #[serde(skip)]
    pub tasks: Vec<NoteTaskRecord>,
    #[serde(skip)]
    pub raw_inline_expressions: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub inline_expressions: Vec<EvaluatedInlineExpression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteListItemRecord {
    pub id: String,
    pub text: String,
    pub tags: Vec<String>,
    pub outlinks: Vec<String>,
    pub line_number: i64,
    pub line_count: i64,
    pub byte_offset: i64,
    pub section_heading: Option<String>,
    pub parent_item_id: Option<String>,
    pub is_task: bool,
    pub block_id: Option<String>,
    pub annotated: bool,
    pub symbol: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NoteTaskRecord {
    pub id: String,
    pub list_item_id: String,
    pub status_char: String,
    pub status_name: String,
    pub status_type: String,
    pub status_next_symbol: Option<String>,
    pub checked: bool,
    pub completed: bool,
    pub text: String,
    pub byte_offset: i64,
    pub parent_task_id: Option<String>,
    pub section_heading: Option<String>,
    pub line_number: i64,
    pub properties: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvaluatedInlineExpression {
    pub expression: String,
    pub value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn extract_indexed_properties(
    parsed: &crate::ParsedDocument,
    config: &VaultConfig,
) -> Result<Option<IndexedProperties>, serde_json::Error> {
    if parsed.frontmatter.is_none() && parsed.inline_fields.is_empty() {
        return Ok(None);
    }

    let raw_yaml = parsed.raw_frontmatter.clone().unwrap_or_default();
    let mut values = Vec::new();
    let mut list_items = Vec::new();
    let mut diagnostics = Vec::new();
    let mut merged_properties = parsed
        .frontmatter
        .as_ref()
        .map(yaml_to_json_object)
        .unwrap_or_default();
    let mut inline_json_values: BTreeMap<String, Vec<Value>> = BTreeMap::new();

    if let Some(mapping) = parsed
        .frontmatter
        .as_ref()
        .and_then(serde_yaml::Value::as_mapping)
    {
        for (key, value) in mapping {
            let Some(key) = key.as_str() else {
                continue;
            };
            let expected_type = config
                .property_types
                .get(key)
                .map(String::as_str)
                .map(canonical_property_type);
            let extracted = extract_property_value(key, value, config, expected_type);
            if let Some(diagnostic) = extracted.diagnostic {
                diagnostics.push(diagnostic);
            }
            values.push(extracted.value);
            list_items.extend(extracted.list_items);
        }
    }

    for inline_field in &parsed.inline_fields {
        let expected_type = config
            .property_types
            .get(&inline_field.key)
            .map(String::as_str)
            .map(canonical_property_type);
        let extracted = extract_inline_property_value(
            &inline_field.key,
            &inline_field.value_text,
            inline_field.kind,
            config,
            expected_type,
        );
        if let Some(diagnostic) = extracted.diagnostic.clone() {
            diagnostics.push(diagnostic);
        }
        inline_json_values
            .entry(inline_field.key.clone())
            .or_default()
            .push(extracted_property_json_value(&extracted));
        values.push(extracted.value);
        list_items.extend(extracted.list_items);
    }

    for (key, inline_values) in inline_json_values {
        merge_property_json_values(&mut merged_properties, key, inline_values);
    }

    Ok(Some(IndexedProperties {
        raw_yaml,
        canonical_json: serde_json::to_string(&Value::Object(merged_properties))?,
        values,
        list_items,
        diagnostics,
    }))
}

fn extract_inline_property_value(
    key: &str,
    value_text: &str,
    kind: InlineFieldKind,
    config: &VaultConfig,
    expected_type: Option<&str>,
) -> ExtractedPropertyValue {
    let yaml_value = inline_text_to_yaml_value(value_text, config);
    let mut extracted = extract_property_value(key, &yaml_value, config, expected_type);
    extracted.value.origin = match kind {
        InlineFieldKind::Bare => PropertyValueOrigin::Inline,
        InlineFieldKind::Parenthesized => PropertyValueOrigin::InlineParen,
        InlineFieldKind::Bracket => PropertyValueOrigin::InlineBracket,
    };
    extracted
}

pub(crate) fn indexed_inline_property_value(
    key: &str,
    value_text: &str,
    kind: InlineFieldKind,
    config: &VaultConfig,
    expected_type: Option<&str>,
) -> IndexedPropertyValue {
    extract_inline_property_value(key, value_text, kind, config, expected_type).value
}

fn inline_text_to_yaml_value(value_text: &str, config: &VaultConfig) -> serde_yaml::Value {
    let trimmed = value_text.trim();

    if is_internal_link_value(trimmed, config) {
        return serde_yaml::Value::String(trimmed.to_string());
    }

    if let Some(values) = parse_inline_quoted_list(trimmed) {
        return serde_yaml::Value::Sequence(
            values.into_iter().map(serde_yaml::Value::String).collect(),
        );
    }

    match serde_yaml::from_str::<serde_yaml::Value>(trimmed) {
        Ok(serde_yaml::Value::Bool(value_bool)) if parse_boolean(trimmed).is_some() => {
            serde_yaml::Value::Bool(value_bool)
        }
        Ok(serde_yaml::Value::Number(number)) if matches_number_literal(trimmed) => {
            serde_yaml::Value::Number(number)
        }
        Ok(serde_yaml::Value::String(text)) => serde_yaml::Value::String(text),
        Ok(serde_yaml::Value::Null) => serde_yaml::Value::Null,
        Ok(_) | Err(_) => serde_yaml::Value::String(trimmed.to_string()),
    }
}

fn merge_property_json_values(
    merged_properties: &mut Map<String, Value>,
    key: String,
    mut additional_values: Vec<Value>,
) {
    if let Some(existing_value) = merged_properties.remove(&key) {
        let mut values = match existing_value {
            Value::Array(values) => values,
            other => vec![other],
        };
        values.append(&mut additional_values);
        merged_properties.insert(key, Value::Array(values));
    } else {
        let merged_value = if additional_values.len() == 1 {
            additional_values.into_iter().next().unwrap_or(Value::Null)
        } else {
            Value::Array(additional_values)
        };
        merged_properties.insert(key, merged_value);
    }
}

fn parse_inline_quoted_list(value_text: &str) -> Option<Vec<String>> {
    if !value_text.contains(',') {
        return None;
    }
    if !value_text.split(',').all(is_quoted_inline_list_segment) {
        return None;
    }

    let parsed = serde_yaml::from_str::<serde_yaml::Value>(&format!("[{value_text}]")).ok()?;
    let serde_yaml::Value::Sequence(values) = parsed else {
        return None;
    };
    values
        .into_iter()
        .map(|value| match value {
            serde_yaml::Value::String(text) => Some(text),
            _ => None,
        })
        .collect()
}

fn is_quoted_inline_list_segment(segment: &str) -> bool {
    let trimmed = segment.trim();
    trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
}

fn matches_number_literal(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.parse::<f64>().is_ok_and(f64::is_finite)
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.' | 'e' | 'E'))
        && trimmed.chars().any(|ch| ch.is_ascii_digit())
}

fn property_json_value(value: &IndexedPropertyValue) -> Value {
    match value.value_type.as_str() {
        "null" => Value::Null,
        "boolean" => value.value_bool.map_or(Value::Null, Value::Bool),
        "number" => value
            .value_number
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Null, Value::Number),
        "date" => value
            .value_date
            .as_ref()
            .map_or(Value::Null, |value_date| Value::String(value_date.clone())),
        _ => value
            .value_text
            .as_ref()
            .map_or(Value::Null, |value_text| Value::String(value_text.clone())),
    }
}

fn extracted_property_json_value(value: &ExtractedPropertyValue) -> Value {
    if value.value.value_type == "list" {
        return Value::Array(
            value
                .list_items
                .iter()
                .map(|item| Value::String(item.value_text.clone()))
                .collect(),
        );
    }

    property_json_value(&value.value)
}

fn yaml_to_json_object(value: &serde_yaml::Value) -> Map<String, Value> {
    match yaml_to_json(value) {
        Value::Object(object) => object,
        _ => Map::new(),
    }
}

fn parse_frontmatter_json_object(raw_yaml: &str) -> Value {
    if raw_yaml.trim().is_empty() {
        return Value::Object(Map::new());
    }

    serde_yaml::from_str::<serde_yaml::Value>(raw_yaml)
        .ok()
        .map_or_else(
            || Value::Object(Map::new()),
            |value| Value::Object(yaml_to_json_object(&value)),
        )
}

#[allow(clippy::too_many_lines)]
pub fn query_notes(paths: &VaultPaths, query: &NoteQuery) -> Result<NotesReport, PropertyError> {
    query_notes_with_filter(paths, query, None)
}

pub fn query_notes_with_filter(
    paths: &VaultPaths,
    query: &NoteQuery,
    filter: Option<&PermissionFilter>,
) -> Result<NotesReport, PropertyError> {
    query_notes_with_scope(paths, query, filter, None)
}

/// Query notes inside an already-authorized lookup universe, such as one from
/// [`load_note_index_with_guard`]. Rows, incoming-link sources, expression
/// filters, and inline expressions are limited to that universe, so policy
/// decisions are reused rather than repeated per query.
#[allow(clippy::implicit_hasher)]
pub fn query_notes_in_authorized_scope(
    paths: &VaultPaths,
    query: &NoteQuery,
    filter: Option<&PermissionFilter>,
    authorized_index: &HashMap<String, NoteRecord>,
) -> Result<NotesReport, PropertyError> {
    query_notes_with_scope(paths, query, filter, Some(authorized_index))
}

pub(crate) fn query_notes_with_scope(
    paths: &VaultPaths,
    query: &NoteQuery,
    filter: Option<&PermissionFilter>,
    authorized_index: Option<&HashMap<String, NoteRecord>>,
) -> Result<NotesReport, PropertyError> {
    match query_notes_core(
        paths,
        query,
        filter,
        authorized_index,
        NoteQueryOutput::Notes,
    )? {
        NoteQueryOutcome::Notes(shared) => Ok(shared.into_report(query)),
        NoteQueryOutcome::Paths(_) => unreachable!("notes output yields notes"),
    }
}

/// Paths of the readable notes matching `filters`, with the semantics of
/// [`query_notes`]. Decided matches are never hydrated.
pub(crate) fn note_paths_matching_filters(
    paths: &VaultPaths,
    filters: &[String],
    filter: Option<&PermissionFilter>,
) -> Result<HashSet<String>, PropertyError> {
    let query = NoteQuery {
        filters: filters.to_vec(),
        sort_by: None,
        sort_descending: false,
    };
    match query_notes_core(paths, &query, filter, None, NoteQueryOutput::Paths)? {
        NoteQueryOutcome::Paths(paths) => Ok(paths),
        NoteQueryOutcome::Notes(shared) => Ok(shared
            .notes
            .into_iter()
            .map(|note| note.document_path.clone())
            .collect()),
    }
}

/// The plan's rows that the filters' expressions keep: decided matches as
/// they are, undecided rows evaluated against `lookup`. Paths-only output
/// keeps decided matches as paths without loading them.
fn evaluate_planned_rows(
    lookup: &crate::note_lookup::IndexedNoteLookup<'_>,
    planned: &crate::plan::PlannedRows,
    compiled: &CompiledNoteFilters,
    config: &VaultConfig,
    output: NoteQueryOutput,
) -> Result<(Vec<Arc<NoteRecord>>, Vec<String>), PropertyError> {
    let formulas = BTreeMap::new();
    let time_zone = DataviewTimeZone::parse(config.dataview.timezone.as_deref());
    let mut notes = Vec::with_capacity(planned.rows.len());
    let mut matched_paths = Vec::new();
    for path in &planned.rows {
        if output == NoteQueryOutput::Paths && !planned.undecided.contains(path) {
            // A decided match needs nothing but its path.
            matched_paths.push(path.clone());
            continue;
        }
        let note = if output == NoteQueryOutput::StoredNotes {
            lookup.note_arc_at(path)
        } else {
            lookup.hydrated_arc_at(path)
        };
        let Some(note) = note else {
            continue;
        };
        if planned.undecided.contains(path) {
            let ctx = EvalContext::new(&note, &formulas)
                .with_note_lookup(lookup)
                .with_time_zone(time_zone);
            let mut keep = true;
            for expression in &compiled.expressions {
                let value = evaluate(&expression.expr, &ctx)
                    .map_err(|_| PropertyError::InvalidFilter(expression.filter.clone()))?;
                if !expression_filter_matches(&value) {
                    keep = false;
                    break;
                }
            }
            if !keep {
                continue;
            }
        }
        notes.push(note);
    }
    Ok((notes, matched_paths))
}

/// Matching notes shared with the lookup that loaded them, and their plan.
pub(crate) struct SharedNotes {
    pub notes: Vec<Arc<NoteRecord>>,
    pub plan: crate::plan::QueryPlanExplain,
}

impl SharedNotes {
    fn into_report(self, query: &NoteQuery) -> NotesReport {
        NotesReport {
            filters: query.filters.clone(),
            sort_by: query.sort_by.clone(),
            sort_descending: query.sort_descending,
            notes: self
                .notes
                .into_iter()
                .map(|note| Arc::try_unwrap(note).unwrap_or_else(|shared| (*shared).clone()))
                .collect(),
            plan: Some(self.plan),
        }
    }
}

/// What [`query_notes_core`] produced.
enum NoteQueryOutcome {
    /// Matching notes in query order, shared with the lookup.
    Notes(SharedNotes),
    Paths(HashSet<String>),
}

/// What [`query_notes_core`] must produce for each matching note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteQueryOutput {
    /// Fully hydrated records in query order.
    Notes,
    /// Records with stored fields only, in query order: the caller reads
    /// no tags, links, tasks, or lists of the rows, and neither do the
    /// filters.
    StoredNotes,
    /// Only `document_path` is meaningful; rows are unsorted.
    Paths,
}

/// The note filter frontend over the shared planner (QRY.5): tag and
/// folder sources select candidates, the filters' predicate atoms decide
/// them on stored fields, and only undecided rows evaluate their
/// expressions, against a lookup that loads other notes on demand.
fn query_notes_core(
    paths: &VaultPaths,
    query: &NoteQuery,
    filter: Option<&PermissionFilter>,
    authorized_index: Option<&HashMap<String, NoteRecord>>,
    output: NoteQueryOutput,
) -> Result<NoteQueryOutcome, PropertyError> {
    query_notes_core_in(
        &crate::note_store::DirectNoteStore::new(paths),
        paths,
        query,
        filter,
        authorized_index,
        output,
        None,
    )
}

fn query_notes_core_in(
    store: &dyn crate::note_store::NoteStore,
    paths: &VaultPaths,
    query: &NoteQuery,
    filter: Option<&PermissionFilter>,
    authorized_index: Option<&HashMap<String, NoteRecord>>,
    output: NoteQueryOutput,
    page: Option<NotePage>,
) -> Result<NoteQueryOutcome, PropertyError> {
    let config = crate::load_vault_config(paths).config;
    let within = authorized_index.map(|index| {
        index
            .values()
            .map(|note| note.document_path.clone())
            .collect::<HashSet<_>>()
    });
    let lookup = store.lookup(NoteIndexReadScope::Filter(filter), within.as_ref())?;
    query_notes_over(paths, &lookup, &config, query, filter, output, page)
}

/// [`query_notes_with_filter`] reading notes from `store` (QRY.6).
pub fn query_notes_in(
    store: &dyn crate::note_store::NoteStore,
    paths: &VaultPaths,
    query: &NoteQuery,
    filter: Option<&PermissionFilter>,
) -> Result<NotesReport, PropertyError> {
    query_notes_page_in(store, paths, query, filter, None)
}

/// [`query_notes_in`] returning only `page` of the ordered notes. When the
/// filters read no hydrated fields only the page is hydrated.
pub fn query_notes_page_in(
    store: &dyn crate::note_store::NoteStore,
    paths: &VaultPaths,
    query: &NoteQuery,
    filter: Option<&PermissionFilter>,
    page: Option<NotePage>,
) -> Result<NotesReport, PropertyError> {
    match query_notes_core_in(
        store,
        paths,
        query,
        filter,
        None,
        NoteQueryOutput::Notes,
        page,
    )? {
        NoteQueryOutcome::Notes(shared) => Ok(shared.into_report(query)),
        NoteQueryOutcome::Paths(_) => unreachable!("notes output yields notes"),
    }
}

/// [`query_notes_with_filter`] over `lookup`, an already loaded universe
/// (for example a Bases evaluation's), with rows hydrated or carrying
/// stored fields only.
pub(crate) fn query_notes_shared_over(
    paths: &VaultPaths,
    lookup: &crate::note_lookup::IndexedNoteLookup<'_>,
    config: &VaultConfig,
    query: &NoteQuery,
    filter: Option<&PermissionFilter>,
    stored_only: bool,
) -> Result<SharedNotes, PropertyError> {
    let output = if stored_only {
        NoteQueryOutput::StoredNotes
    } else {
        NoteQueryOutput::Notes
    };
    match query_notes_over(paths, lookup, config, query, filter, output, None)? {
        NoteQueryOutcome::Notes(shared) => Ok(shared),
        NoteQueryOutcome::Paths(_) => unreachable!("notes output yields notes"),
    }
}

/// A window of a note query's ordered results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NotePage {
    pub offset: usize,
    pub limit: Option<usize>,
}

fn query_notes_over(
    paths: &VaultPaths,
    lookup: &crate::note_lookup::IndexedNoteLookup<'_>,
    config: &VaultConfig,
    query: &NoteQuery,
    filter: Option<&PermissionFilter>,
    output: NoteQueryOutput,
    page: Option<NotePage>,
) -> Result<NoteQueryOutcome, PropertyError> {
    let compiled = compile_note_filters(&query.filters)?;
    // A page of hydrated notes needs only the page hydrated when filters
    // and the sort read stored fields (sort keys always do).
    let page_first = page.is_some()
        && output == NoteQueryOutput::Notes
        && !compiled.expressions.iter().any(|expression| {
            crate::expression::analysis::reads_row_file_fields(
                &expression.expr,
                crate::expression::analysis::RowBindings {
                    whole_rows: &[],
                    this_is_row: true,
                },
            )
        });
    let rows_output = if page_first {
        NoteQueryOutput::StoredNotes
    } else {
        output
    };
    let plan = crate::plan::NotePlan {
        frontend: "notes",
        source: match compiled.sources.len() {
            0 => None,
            1 => compiled.sources.first().cloned(),
            _ => Some(crate::source::SourceExpr::And(compiled.sources.clone())),
        },
        markdown_only: true,
        // The filters form one ordered conjunction, so nothing after an
        // undecided filter excludes a row.
        predicate: crate::predicate::Predicate::All(
            compiled
                .expressions
                .iter()
                .map(|expression| expression.predicate.clone())
                .collect(),
        ),
        hydration: match rows_output {
            NoteQueryOutput::Notes => crate::plan::Hydration::Rows,
            NoteQueryOutput::StoredNotes => crate::plan::Hydration::Stored,
            NoteQueryOutput::Paths => crate::plan::Hydration::Undecided,
        },
        also_hydrate: Vec::new(),
    };
    let planned = crate::plan::execute_note_plan(paths, lookup, &plan, filter)?;

    let (mut notes, mut matched_paths) =
        evaluate_planned_rows(lookup, &planned, &compiled, config, rows_output)?;
    if let Some(error) = lookup.take_error() {
        return Err(error);
    }

    if output == NoteQueryOutput::Paths {
        matched_paths.extend(notes.iter().map(|note| note.document_path.clone()));
        return Ok(NoteQueryOutcome::Paths(matched_paths.into_iter().collect()));
    }

    if let Some(sort_by) = query.sort_by.as_deref() {
        let mut keyed = notes
            .into_iter()
            .map(|note| (sort_key_for_note(&note, sort_by), note))
            .collect::<Vec<_>>();
        keyed.sort_by(|(left_key, left), (right_key, right)| {
            let ordering = compare_sort_keys(left_key, right_key);
            let ordering = if query.sort_descending {
                ordering.reverse()
            } else {
                ordering
            };
            ordering.then_with(|| left.document_path.cmp(&right.document_path))
        });
        notes = keyed.into_iter().map(|(_, note)| note).collect();
    }
    if let Some(page) = page {
        let start = page.offset.min(notes.len());
        let end = page.limit.map_or(notes.len(), |limit| {
            start.saturating_add(limit).min(notes.len())
        });
        notes.truncate(end);
        notes.drain(..start);
    }
    if page_first {
        lookup.prefetch_hydrated(notes.iter().map(|note| note.document_path.as_str()));
        notes = notes
            .iter()
            .filter_map(|note| lookup.hydrated_arc_at(&note.document_path))
            .collect();
    }
    for note in &mut notes {
        if !note.raw_inline_expressions.is_empty() {
            let evaluated = evaluate_note_inline_expressions(note, lookup);
            Arc::make_mut(note).inline_expressions = evaluated;
        }
    }
    if let Some(error) = lookup.take_error() {
        return Err(error);
    }

    Ok(NoteQueryOutcome::Notes(SharedNotes {
        notes,
        plan: planned.explain,
    }))
}

/// Load all notes using collision-safe expression lookup keys.
/// Unique basenames retain their historical keys; duplicate basenames use
/// slash-prefixed document paths. Enumerate values or resolve references rather
/// than assuming that a basename identifies every note.
/// This includes enough derived metadata for expression evaluation on linked notes.
pub fn load_note_index(paths: &VaultPaths) -> Result<HashMap<String, NoteRecord>, PropertyError> {
    load_note_index_with_filter(paths, None)
}

/// Load the expression lookup index after applying the caller's read scope.
pub fn load_note_index_with_filter(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
) -> Result<HashMap<String, NoteRecord>, PropertyError> {
    load_note_index_with_read_scope(paths, filter, None, NoteIndexHydration::Full)
}

/// Apply static document grants and per-path policy before constructing the
/// expression lookup universe, including incoming-link sources.
pub fn load_note_index_with_guard(
    paths: &VaultPaths,
    guard: &dyn PermissionGuard,
) -> Result<HashMap<String, NoteRecord>, PropertyError> {
    load_note_index_with_read_scope(
        paths,
        Some(&guard.read_filter()),
        Some(guard),
        NoteIndexHydration::Full,
    )
}

/// How much of each note [`load_note_index_with_read_scope`] hydrates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteIndexHydration {
    /// Every field.
    Full,
    /// Stored fields and aliases, which link resolution reads for every
    /// note. Tags, links, inlinks, tasks, list items, and inline expressions
    /// are reachable only through a note's file object and stay empty until
    /// [`hydrate_note_index_entries`] fills them.
    AliasesOnly,
}

/// [`load_note_index_with_guard`] hydrating only what resolving links needs.
/// Callers that can bound whose file objects they read (QRY.3) complete
/// those notes with [`hydrate_note_index_entries`].
pub fn load_note_index_with_guard_deferring_hydration(
    paths: &VaultPaths,
    guard: &dyn PermissionGuard,
) -> Result<HashMap<String, NoteRecord>, PropertyError> {
    load_note_index_with_read_scope(
        paths,
        Some(&guard.read_filter()),
        Some(guard),
        NoteIndexHydration::AliasesOnly,
    )
}

/// [`load_note_index_with_filter`] hydrating only what resolving links
/// needs; see [`load_note_index_with_guard_deferring_hydration`].
pub fn load_note_index_with_filter_deferring_hydration(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
) -> Result<HashMap<String, NoteRecord>, PropertyError> {
    load_note_index_with_read_scope(paths, filter, None, NoteIndexHydration::AliasesOnly)
}

/// The note index a Tasks query reads: every readable note with stored
/// fields and aliases, fully hydrated only where it carries tasks
/// (Markdown tasks, or a `TaskNotes` task derived from its properties). Task
/// rows, their notes' fields, and `blocked` dependencies come only from
/// those notes.
pub fn load_task_note_index(
    paths: &VaultPaths,
    scope: NoteIndexReadScope<'_>,
) -> Result<HashMap<String, NoteRecord>, PropertyError> {
    load_task_note_index_with_plan(paths, scope).map(|(index, _)| index)
}

/// [`load_task_note_index`] with the note plan that built it (QRY.5): every
/// readable document as a candidate, nothing decided, and hydration of the
/// task-bearing notes.
pub fn load_task_note_index_with_plan(
    paths: &VaultPaths,
    scope: NoteIndexReadScope<'_>,
) -> Result<(HashMap<String, NoteRecord>, crate::plan::QueryPlanExplain), PropertyError> {
    let lookup = load_indexed_note_lookup(paths, scope)?;
    let mut task_paths = {
        let database = lookup
            .database()
            .expect("the indexed lookup shares its connection");
        let mut statement = database.connection().prepare(
            "SELECT DISTINCT documents.path FROM tasks \
             JOIN documents ON documents.id = tasks.document_id",
        )?;
        let task_paths = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<HashSet<_>, _>>()?;
        task_paths
    };
    let config = crate::load_vault_config(paths).config;
    task_paths.extend(
        lookup
            .notes()
            .filter(|note| {
                extract_tasknote(
                    &note.document_path,
                    &note.file_name,
                    &note.properties,
                    &config.tasknotes,
                )
                .is_some()
            })
            .map(|note| note.document_path.clone()),
    );
    let filter = match scope {
        NoteIndexReadScope::Filter(filter) => filter.cloned(),
        NoteIndexReadScope::Guard(guard) => Some(guard.read_filter()),
    };
    let planned = crate::plan::execute_note_plan(
        paths,
        &lookup,
        &crate::plan::NotePlan {
            frontend: "tasks",
            source: None,
            markdown_only: false,
            predicate: crate::predicate::Predicate::Unknown,
            hydration: crate::plan::Hydration::Paths(&task_paths),
            also_hydrate: Vec::new(),
        },
        filter.as_ref(),
    )?;
    Ok((lookup.into_index()?, planned.explain))
}

/// The read scope an index was loaded with, for hydrating its entries.
#[derive(Clone, Copy)]
pub enum NoteIndexReadScope<'a> {
    Filter(Option<&'a PermissionFilter>),
    Guard(&'a dyn PermissionGuard),
}

/// Fully hydrate the notes at `note_paths` in an index from a deferring
/// loader, with the read scope it was loaded with: incoming links come only
/// from notes in the index's universe.
#[allow(clippy::implicit_hasher)]
pub fn hydrate_note_index_entries(
    paths: &VaultPaths,
    scope: NoteIndexReadScope<'_>,
    note_index: &mut HashMap<String, NoteRecord>,
    note_paths: &HashSet<String>,
) -> Result<(), PropertyError> {
    let keys = note_index
        .iter()
        .filter(|(_, note)| note_paths.contains(&note.document_path))
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    if keys.is_empty() {
        return Ok(());
    }
    let (filter, policy_scoped) = match scope {
        NoteIndexReadScope::Filter(filter) => (filter.cloned(), false),
        NoteIndexReadScope::Guard(guard) => (Some(guard.read_filter()), guard.has_policy_hook()),
    };
    let readable_sources = policy_scoped.then(|| {
        note_index
            .values()
            .map(|note| note.document_path.clone())
            .collect::<HashSet<_>>()
    });
    let (keys, mut doc_ids_and_notes): (Vec<_>, Vec<_>) = keys
        .into_iter()
        .filter_map(|key| {
            note_index
                .remove(&key)
                .map(|note| (key, (note.document_id.clone(), note)))
        })
        .unzip();
    let database = open_existing_cache(paths)?;
    let config = crate::load_vault_config(paths).config;
    hydrate_note_records(
        database.connection(),
        &config,
        &mut doc_ids_and_notes,
        filter.as_ref(),
        readable_sources.as_ref(),
        true,
    )?;
    for (key, (_, note)) in keys.into_iter().zip(doc_ids_and_notes) {
        note_index.insert(key, note);
    }
    Ok(())
}

/// The readable identities (path, lookup key, file name, aliases) under
/// `filter`, the policy hook, and `within`, in path order.
fn load_note_identities(
    database: &CacheDatabase,
    filter: Option<&PermissionFilter>,
    guard: Option<&dyn PermissionGuard>,
    within: Option<&HashSet<String>>,
) -> Result<Vec<crate::note_lookup::IndexedIdentity>, PropertyError> {
    // Identity facts come from the narrow table alone, through its covering
    // identity index (schema v29); the read scope restricts its document ids.
    let permission_sql = filter
        .map(|filter| {
            filter.document_scope_sql_for("_note_identity_permission", "note_query.document_id")
        })
        .unwrap_or_default();
    let mut sql = permission_sql.cte;
    sql.push_str(
        "SELECT note_query.path, note_query.filename, note_query.aliases, \
         note_query.row_version FROM note_query WHERE 1 = 1",
    );
    sql.push_str(&permission_sql.clause);
    sql.push_str(" ORDER BY 1");
    let mut statement = database.connection().prepare_cached(&sql)?;
    let rows = statement.query_map(params_from_iter(permission_sql.params.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut admitted = Vec::new();
    for row in rows {
        let (path, file_name, aliases, row_version) = row?;
        if within.is_some_and(|within| !within.contains(&path)) {
            continue;
        }
        if policy_allows_indexed_note(guard, &path)? {
            let aliases = if aliases == "[]" {
                Vec::new()
            } else {
                serde_json::from_str::<Vec<String>>(&aliases).unwrap_or_default()
            };
            admitted.push((path, file_name, aliases, row_version));
        }
    }
    drop(statement);
    let mut counts = HashMap::<&str, usize>::new();
    for (_, file_name, _, _) in &admitted {
        *counts.entry(file_name.as_str()).or_default() += 1;
    }
    let duplicates = counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name.to_string())
        .collect::<HashSet<_>>();
    Ok(admitted
        .into_iter()
        .map(
            |(path, file_name, aliases, row_version)| crate::note_lookup::IndexedIdentity {
                key: if duplicates.contains(&file_name) {
                    format!("/{path}")
                } else {
                    file_name.clone()
                },
                path,
                file_name,
                aliases,
                row_version,
            },
        )
        .collect::<Vec<_>>())
}

/// A note lookup over identity facts (QRY.4) for the readable universe of
/// `scope`: grants and the policy hook select identities exactly as
/// [`load_note_index_with_guard`] selects notes, keys are the same, and no
/// note's fields load until a query prefetches, reads, or dereferences it.
pub fn load_indexed_note_lookup<'a>(
    paths: &'a VaultPaths,
    scope: NoteIndexReadScope<'_>,
) -> Result<crate::note_lookup::IndexedNoteLookup<'a>, PropertyError> {
    load_indexed_note_lookup_within(paths, scope, None)
}

/// [`load_indexed_note_lookup`] restricted to `within`, an already
/// authorized universe (for example a Bases evaluation's), which also
/// bounds incoming links.
pub(crate) fn load_indexed_note_lookup_within<'a>(
    paths: &'a VaultPaths,
    scope: NoteIndexReadScope<'_>,
    within: Option<&HashSet<String>>,
) -> Result<crate::note_lookup::IndexedNoteLookup<'a>, PropertyError> {
    let database = std::rc::Rc::new(open_existing_cache(paths)?);
    let readable = load_readable_identities(&database, scope, within)?;
    let bookmarked_paths = load_bookmarked_paths(paths.vault_root());
    // One connection and one configuration serve every load of this lookup.
    let config = std::rc::Rc::new(crate::load_vault_config(paths).config);
    let stored_database = std::rc::Rc::clone(&database);
    let hydrate_database = std::rc::Rc::clone(&database);
    let ReadableIdentities {
        identities,
        filter,
        readable_sources,
    } = readable;
    Ok(crate::note_lookup::IndexedNoteLookup::new(
        identities,
        Box::new(move |wanted: Option<&[&str]>| {
            Ok(load_stored_notes(
                stored_database.connection(),
                paths.vault_root(),
                &bookmarked_paths,
                wanted,
            )?
            .into_iter()
            .map(|stored| Arc::new(stored.record))
            .collect())
        }),
        Box::new(move |notes| {
            hydrate_shared_notes(
                hydrate_database.connection(),
                &config,
                filter.as_ref(),
                readable_sources.as_ref(),
                notes,
            )
        }),
    )
    .with_database(database))
}

/// A scope's readable identities and what hydration needs to keep
/// incoming links inside it.
pub(crate) struct ReadableIdentities {
    pub identities: Vec<crate::note_lookup::IndexedIdentity>,
    /// The scope's read filter.
    pub filter: Option<PermissionFilter>,
    /// The universe's paths when a policy hook or an authorized universe
    /// scopes incoming links; grants alone are applied in SQL.
    pub readable_sources: Option<HashSet<String>>,
}

/// The readable identities of `scope` (restricted to `within`): grants and
/// the policy hook select identities exactly as
/// [`load_note_index_with_guard`] selects notes.
pub(crate) fn load_readable_identities(
    database: &CacheDatabase,
    scope: NoteIndexReadScope<'_>,
    within: Option<&HashSet<String>>,
) -> Result<ReadableIdentities, PropertyError> {
    let (filter, guard) = match scope {
        NoteIndexReadScope::Filter(filter) => (filter.cloned(), None),
        NoteIndexReadScope::Guard(guard) => (Some(guard.read_filter()), Some(guard)),
    };
    let identities = load_note_identities(database, filter.as_ref(), guard, within)?;
    let policy_scoped = guard.is_some_and(PermissionGuard::has_policy_hook) || within.is_some();
    let readable_sources = policy_scoped.then(|| {
        identities
            .iter()
            .map(|identity| identity.path.clone())
            .collect::<HashSet<_>>()
    });
    Ok(ReadableIdentities {
        identities,
        filter,
        readable_sources,
    })
}

/// A note's stored fields as loaded from the cache.
pub(crate) struct StoredNote {
    pub record: NoteRecord,
    /// Whether `file.ctime` came from the cache rather than a filesystem
    /// fallback that can change without a cache write.
    pub ctime_recorded: bool,
}

/// Stored-field records for `document_paths` (`None`: every document, in
/// one scan), without aliases.
pub(crate) fn load_stored_notes(
    connection: &rusqlite::Connection,
    vault_root: &Path,
    bookmarked_paths: &HashSet<String>,
    document_paths: Option<&[&str]>,
) -> Result<Vec<StoredNote>, PropertyError> {
    use rayon::prelude::*;
    let rows = if let Some(document_paths) = document_paths {
        let mut statement = connection.prepare_cached(&format!(
            "SELECT {STORED_NOTE_COLUMNS} \
                 FROM documents LEFT JOIN properties \
                 ON properties.document_id = documents.id \
                 WHERE documents.path IN (SELECT value FROM json_each(?1))"
        ))?;
        let wanted = serde_json::to_string(document_paths).expect("paths serialize");
        let rows = statement
            .query_map([wanted], stored_note_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    } else {
        let mut statement = connection.prepare_cached(&format!(
            "SELECT {STORED_NOTE_COLUMNS} \
                 FROM documents LEFT JOIN properties \
                 ON properties.document_id = documents.id"
        ))?;
        let rows = statement
            .query_map([], stored_note_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    Ok(rows
        .into_par_iter()
        .map(|row| {
            let ctime_recorded = row.10.is_some();
            StoredNote {
                record: stored_note_record(row, vault_root, bookmarked_paths).1,
                ctime_recorded,
            }
        })
        .collect())
}

/// [`hydrate_note_copies`] over shared records.
pub(crate) fn hydrate_shared_notes(
    connection: &rusqlite::Connection,
    config: &VaultConfig,
    filter: Option<&PermissionFilter>,
    readable_sources: Option<&HashSet<String>>,
    notes: Vec<Arc<NoteRecord>>,
) -> Result<Vec<Arc<NoteRecord>>, PropertyError> {
    Ok(hydrate_note_copies(
        connection,
        config,
        filter,
        readable_sources,
        notes
            .into_iter()
            .map(|note| Arc::try_unwrap(note).unwrap_or_else(|shared| (*shared).clone()))
            .collect(),
    )?
    .into_iter()
    .map(Arc::new)
    .collect())
}

/// Hydrated copies of `notes` under `filter`; incoming links come only from
/// `readable_sources` when a policy hook or an authorized universe scopes
/// them.
pub(crate) fn hydrate_note_copies(
    connection: &rusqlite::Connection,
    config: &VaultConfig,
    filter: Option<&PermissionFilter>,
    readable_sources: Option<&HashSet<String>>,
    notes: Vec<NoteRecord>,
) -> Result<Vec<NoteRecord>, PropertyError> {
    if notes.is_empty() {
        return Ok(notes);
    }
    let mut doc_ids_and_notes = notes
        .into_iter()
        .map(|note| (note.document_id.clone(), note))
        .collect::<Vec<_>>();
    hydrate_note_records(
        connection,
        config,
        &mut doc_ids_and_notes,
        filter,
        readable_sources,
        true,
    )?;
    Ok(doc_ids_and_notes
        .into_iter()
        .map(|(_, note)| note)
        .collect())
}

/// A `documents` row joined with its stored properties, in the column
/// order of [`STORED_NOTE_COLUMNS`].
type StoredNoteRow = (
    String,
    String,
    String,
    String,
    i64,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
);

const STORED_NOTE_COLUMNS: &str = "documents.id, documents.path, documents.filename, \
     documents.extension, documents.file_mtime, documents.file_size, \
     COALESCE(properties.canonical_json, '{}'), COALESCE(properties.raw_yaml, ''), \
     documents.periodic_type, documents.periodic_date, documents.file_ctime";

fn stored_note_row(row: &rusqlite::Row<'_>) -> Result<StoredNoteRow, rusqlite::Error> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    ))
}

/// A note with its stored fields only: properties, frontmatter, and file
/// metadata, but no tags, links, inlinks, aliases, tasks, or lists.
fn stored_note_record(
    row: StoredNoteRow,
    vault_root: &Path,
    bookmarked_paths: &HashSet<String>,
) -> (String, NoteRecord) {
    let (
        document_id,
        path,
        file_name,
        file_ext,
        file_mtime,
        file_size,
        props_json,
        raw_yaml,
        periodic_type,
        periodic_date,
        recorded_ctime,
    ) = row;
    let properties =
        serde_json::from_str(&props_json).unwrap_or(Value::Object(serde_json::Map::default()));
    (
        document_id.clone(),
        NoteRecord {
            document_id,
            document_path: path.clone(),
            file_name,
            file_ext,
            file_mtime,
            file_ctime: recorded_ctime
                .unwrap_or_else(|| file_ctime_for_document(vault_root, &path, file_mtime)),
            file_size,
            properties,
            tags: vec![],
            links: vec![],
            starred: bookmarked_paths.contains(&path),
            inlinks: vec![],
            aliases: vec![],
            frontmatter: parse_frontmatter_json_object(&raw_yaml),
            periodic_type,
            periodic_date,
            list_items: vec![],
            tasks: vec![],
            raw_inline_expressions: vec![],
            inline_expressions: vec![],
        },
    )
}

#[allow(clippy::too_many_lines)]
fn load_note_index_with_read_scope(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
    guard: Option<&dyn PermissionGuard>,
    hydration: NoteIndexHydration,
) -> Result<HashMap<String, NoteRecord>, PropertyError> {
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let bookmarked_paths = load_bookmarked_paths(paths.vault_root());
    let vault_root = paths.vault_root().to_path_buf();
    let config = crate::load_vault_config(paths).config;
    let permission_sql = filter
        .map(|filter| filter.document_scope_sql("_note_index_permission"))
        .unwrap_or_default();
    let mut sql = permission_sql.cte;
    let _ = write!(
        sql,
        "SELECT {STORED_NOTE_COLUMNS} \
         FROM documents LEFT JOIN properties ON properties.document_id = documents.id \
         WHERE 1 = 1"
    );
    sql.push_str(&permission_sql.clause);
    let params = permission_sql
        .params
        .into_iter()
        .map(SqlValue::Text)
        .collect::<Vec<_>>();
    let mut stmt = connection.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params.iter()), stored_note_row)?;
    // Policy hooks may be stateful, so they are consulted in order; parsing
    // stored JSON and YAML and reading creation times run in parallel.
    let mut admitted = Vec::new();
    for row in rows {
        let row = row?;
        // SQL already checked document tags as well as paths. Repeating
        // check_read_path here would incorrectly reject tag-only grants.
        if policy_allows_indexed_note(guard, &row.1)? {
            admitted.push(row);
        }
    }
    let mut doc_ids_and_notes = {
        use rayon::prelude::*;
        admitted
            .into_par_iter()
            .map(|row| stored_note_record(row, &vault_root, &bookmarked_paths))
            .collect::<Vec<_>>()
    };

    // Preserve the complete authorized source universe, not only task-bearing
    // notes or the eventual query results. Reuse the same policy decisions for
    // backlinks rather than calling a potentially stateful hook again.
    let readable_sources = guard.filter(|guard| guard.has_policy_hook()).map(|_| {
        doc_ids_and_notes
            .iter()
            .map(|(_, note)| note.document_path.clone())
            .collect::<HashSet<_>>()
    });
    match hydration {
        NoteIndexHydration::Full => hydrate_note_records(
            connection,
            &config,
            &mut doc_ids_and_notes,
            filter,
            readable_sources.as_ref(),
            true,
        )?,
        NoteIndexHydration::AliasesOnly => {
            hydrate_note_aliases(connection, &mut doc_ids_and_notes)?;
        }
    }

    Ok(build_note_lookup_index(
        doc_ids_and_notes.into_iter().map(|(_, note)| note),
    ))
}

/// Build a complete expression lookup, retaining distinct paths with identical
/// basenames. Later records replace earlier records only at the same path (for
/// overlays). Unique basename keys remain compatible with existing callers.
/// Collision keys start with `/`, which cannot occur in a file basename.
pub fn build_note_lookup_index(
    notes: impl IntoIterator<Item = NoteRecord>,
) -> HashMap<String, NoteRecord> {
    let by_path = notes
        .into_iter()
        .map(|note| (note.document_path.clone(), note))
        .collect::<HashMap<_, _>>();
    let mut counts = HashMap::<&str, usize>::new();
    for note in by_path.values() {
        *counts.entry(&note.file_name).or_default() += 1;
    }
    let duplicate_names = counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name.to_string())
        .collect::<HashSet<_>>();
    by_path
        .into_values()
        .map(|note| {
            let key = if duplicate_names.contains(&note.file_name) {
                format!("/{}", note.document_path)
            } else {
                note.file_name.clone()
            };
            (key, note)
        })
        .collect()
}

fn policy_allows_indexed_note(
    guard: Option<&dyn PermissionGuard>,
    path: &str,
) -> Result<bool, PropertyError> {
    let Some(guard) = guard.filter(|guard| guard.has_policy_hook()) else {
        return Ok(true);
    };
    match guard.check_policy_decision("read", Some(path)) {
        Ok(()) => Ok(true),
        Err(PermissionError::PolicyHookDenied { .. } | PermissionError::PathDenied { .. }) => {
            Ok(false)
        }
        Err(error) => Err(PropertyError::Permission(error)),
    }
}

pub(crate) fn load_bookmarked_paths(vault_root: &Path) -> HashSet<String> {
    let path = vault_root.join(".obsidian/bookmarks.json");
    let Ok(contents) = fs::read_to_string(path) else {
        return HashSet::new();
    };
    let Ok(bookmarks) = serde_json::from_str::<Value>(&contents) else {
        return HashSet::new();
    };
    bookmarked_paths_from_value(&bookmarks)
}

fn file_ctime_for_document(vault_root: &Path, document_path: &str, fallback_mtime: i64) -> i64 {
    fs::metadata(vault_root.join(document_path))
        .ok()
        .and_then(|metadata| metadata.created().ok().or_else(|| metadata.modified().ok()))
        .and_then(system_time_to_millis)
        .unwrap_or(fallback_mtime)
}

fn system_time_to_millis(time: SystemTime) -> Option<i64> {
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_millis()).ok()
}

fn bookmarked_paths_from_value(bookmarks: &Value) -> HashSet<String> {
    let mut paths = HashSet::new();
    collect_bookmarked_paths(bookmarks, &mut paths);
    paths
}

fn collect_bookmarked_paths(value: &Value, paths: &mut HashSet<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_bookmarked_paths(item, paths);
            }
        }
        Value::Object(object) => {
            if object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|item_type| matches!(item_type, "file" | "markdown" | "canvas"))
            {
                if let Some(path) = object.get("path").and_then(Value::as_str) {
                    paths.insert(path.to_string());
                }
            }
            if let Some(items) = object.get("items") {
                collect_bookmarked_paths(items, paths);
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_lines)]
fn hydrate_note_records(
    connection: &rusqlite::Connection,
    config: &VaultConfig,
    doc_ids_and_notes: &mut Vec<(String, NoteRecord)>,
    filter: Option<&PermissionFilter>,
    readable_sources: Option<&HashSet<String>>,
    include_lists: bool,
) -> Result<(), rusqlite::Error> {
    if doc_ids_and_notes.is_empty() {
        return Ok(());
    }

    let doc_ids: Vec<&str> = doc_ids_and_notes
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    let placeholders = doc_ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");

    let mut tag_map: HashMap<String, Vec<String>> = HashMap::new();
    let tag_sql =
        format!("SELECT document_id, tag_text FROM tags WHERE document_id IN ({placeholders})");
    let mut tag_stmt = connection.prepare(&tag_sql)?;
    let tag_rows = tag_stmt.query_map(params_from_iter(doc_ids.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for tag_row in tag_rows {
        let (doc_id, tag_text) = tag_row?;
        tag_map.entry(doc_id).or_default().push(tag_text);
    }

    let mut link_map: HashMap<String, Vec<String>> = HashMap::new();
    let link_sql = format!(
        "SELECT source_document_id, raw_text
         FROM links
         WHERE link_kind = 'wikilink' AND source_document_id IN ({placeholders})"
    );
    let mut link_stmt = connection.prepare(&link_sql)?;
    let link_rows = link_stmt.query_map(params_from_iter(doc_ids.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for link_row in link_rows {
        let (doc_id, raw_text) = link_row?;
        link_map.entry(doc_id).or_default().push(raw_text);
    }

    let mut alias_map: HashMap<String, Vec<String>> = HashMap::new();
    let alias_sql = format!(
        "SELECT document_id, alias_text FROM aliases WHERE document_id IN ({placeholders})"
    );
    let mut alias_stmt = connection.prepare(&alias_sql)?;
    let alias_rows = alias_stmt.query_map(params_from_iter(doc_ids.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for alias_row in alias_rows {
        let (doc_id, alias_text) = alias_row?;
        alias_map.entry(doc_id).or_default().push(alias_text);
    }

    let mut inlink_map: HashMap<String, Vec<String>> = HashMap::new();
    // Incoming sources must be readable, but need not match the user's query.
    let permission_sql = filter
        .map(|filter| filter.document_scope_sql("_inlink_source_permission"))
        .unwrap_or_default();
    let inlink_sql = format!(
        "{}SELECT links.resolved_target_id, documents.path, documents.extension
         FROM links
         JOIN documents ON documents.id = links.source_document_id
         WHERE links.link_kind = 'wikilink'
           AND links.resolved_target_id IN ({placeholders}){}",
        permission_sql.cte, permission_sql.clause,
    );
    let inlink_params = permission_sql
        .params
        .into_iter()
        .chain(doc_ids.iter().map(|id| (*id).to_string()))
        .collect::<Vec<_>>();
    let mut inlink_stmt = connection.prepare(&inlink_sql)?;
    let inlink_rows = inlink_stmt.query_map(params_from_iter(inlink_params.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for inlink_row in inlink_rows {
        let (doc_id, source_path, source_ext) = inlink_row?;
        if readable_sources.is_some_and(|sources| !sources.contains(&source_path)) {
            continue;
        }
        inlink_map
            .entry(doc_id)
            .or_default()
            .push(synthetic_file_link(&source_path, &source_ext));
    }

    let mut list_item_map = if include_lists {
        load_list_item_map(connection, &doc_ids)?
    } else {
        HashMap::new()
    };

    let mut task_ids_by_doc: HashMap<String, Vec<String>> = HashMap::new();
    let mut task_records: HashMap<String, NoteTaskRecord> = HashMap::new();
    let task_sql = format!(
        "SELECT id, document_id, list_item_id, status_char, text, byte_offset, parent_task_id, \
         section_heading, line_number \
         FROM tasks WHERE document_id IN ({placeholders})"
    );
    let mut task_stmt = connection.prepare(&task_sql)?;
    let task_rows = task_stmt.query_map(params_from_iter(doc_ids.iter()), |row| {
        let status_char: String = row.get(3)?;
        let status_state = config.tasks.statuses.status_state(&status_char);
        Ok((
            row.get::<_, String>(1)?,
            NoteTaskRecord {
                id: row.get(0)?,
                list_item_id: row.get(2)?,
                status_char,
                status_name: status_state.name,
                status_type: status_state.status_type,
                status_next_symbol: status_state.next_symbol,
                checked: status_state.checked,
                completed: status_state.completed,
                text: row.get(4)?,
                byte_offset: row.get(5)?,
                parent_task_id: row.get(6)?,
                section_heading: row.get(7)?,
                line_number: row.get(8)?,
                properties: Map::new(),
            },
        ))
    })?;
    for task_row in task_rows {
        let (doc_id, task) = task_row?;
        task_ids_by_doc
            .entry(doc_id)
            .or_default()
            .push(task.id.clone());
        task_records.insert(task.id.clone(), task);
    }

    if !task_records.is_empty() {
        let task_ids: Vec<&str> = task_records.keys().map(String::as_str).collect();
        let task_placeholders = task_ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let task_property_sql = format!(
            "SELECT task_id, key, value_text, value_number, value_bool, value_date, value_type
             FROM task_properties
             WHERE task_id IN ({task_placeholders})"
        );
        let mut task_property_stmt = connection.prepare(&task_property_sql)?;
        let task_property_rows =
            task_property_stmt.query_map(params_from_iter(task_ids.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    typed_property_json_value(
                        row.get(2)?,
                        row.get(3)?,
                        row.get::<_, Option<i64>>(4)?.map(|value| value != 0),
                        row.get(5)?,
                        row.get::<_, String>(6)?.as_str(),
                    ),
                ))
            })?;

        let mut property_map: HashMap<String, BTreeMap<String, Vec<Value>>> = HashMap::new();
        for property_row in task_property_rows {
            let (task_id, key, value) = property_row?;
            property_map
                .entry(task_id)
                .or_default()
                .entry(key)
                .or_default()
                .push(value);
        }

        for (task_id, grouped_properties) in property_map {
            let Some(task) = task_records.get_mut(&task_id) else {
                continue;
            };
            for (key, values) in grouped_properties {
                merge_property_json_values(&mut task.properties, key, values);
            }
        }
    }

    let mut inline_expression_map: HashMap<String, Vec<String>> = HashMap::new();
    let inline_expression_sql = format!(
        "SELECT document_id, expression
         FROM inline_expressions
         WHERE document_id IN ({placeholders})
         ORDER BY line_number ASC, byte_offset_start ASC"
    );
    let mut inline_expression_stmt = connection.prepare(&inline_expression_sql)?;
    let inline_expression_rows = inline_expression_stmt
        .query_map(params_from_iter(doc_ids.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
    for inline_expression_row in inline_expression_rows {
        let (doc_id, expression) = inline_expression_row?;
        inline_expression_map
            .entry(doc_id)
            .or_default()
            .push(expression);
    }

    for (doc_id, note) in doc_ids_and_notes {
        if let Some(tags) = tag_map.remove(doc_id.as_str()) {
            note.tags = tags;
        }
        if let Some(links) = link_map.remove(doc_id.as_str()) {
            note.links = links;
        }
        if let Some(aliases) = alias_map.remove(doc_id.as_str()) {
            note.aliases = aliases;
        }
        if let Some(inlinks) = inlink_map.remove(doc_id.as_str()) {
            note.inlinks = inlinks;
        }
        if let Some(mut list_items) = list_item_map.remove(doc_id.as_str()) {
            list_items.sort_by_key(|item| (item.line_number, item.byte_offset));
            note.list_items = list_items;
        }
        if let Some(task_ids) = task_ids_by_doc.remove(doc_id.as_str()) {
            let mut tasks = task_ids
                .into_iter()
                .filter_map(|task_id| task_records.remove(&task_id))
                .collect::<Vec<_>>();
            tasks.sort_by_key(|task| (task.line_number, task.byte_offset));
            note.tasks = tasks;
        }
        if let Some(expressions) = inline_expression_map.remove(doc_id.as_str()) {
            note.raw_inline_expressions = expressions;
        }
        if let Some(tasknote) = extract_tasknote(
            &note.document_path,
            &note.file_name,
            &note.properties,
            &config.tasknotes,
        ) {
            note.tasks
                .push(build_tasknote_task_record(note, config, &tasknote));
            note.tasks
                .sort_by_key(|task| (task.line_number, task.byte_offset));
        }
    }

    Ok(())
}

/// Aliases only; see [`NoteIndexHydration::AliasesOnly`].
fn hydrate_note_aliases(
    connection: &rusqlite::Connection,
    doc_ids_and_notes: &mut [(String, NoteRecord)],
) -> Result<(), rusqlite::Error> {
    let mut aliases: HashMap<String, Vec<String>> = HashMap::new();
    let mut statement = connection.prepare("SELECT document_id, alias_text FROM aliases")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (doc_id, alias) = row?;
        aliases.entry(doc_id).or_default().push(alias);
    }
    for (doc_id, note) in doc_ids_and_notes {
        if let Some(note_aliases) = aliases.remove(doc_id.as_str()) {
            note.aliases = note_aliases;
        }
    }
    Ok(())
}

/// List items of the given documents, unsorted, keyed by document id.
fn load_list_item_map(
    connection: &rusqlite::Connection,
    doc_ids: &[&str],
) -> Result<HashMap<String, Vec<NoteListItemRecord>>, rusqlite::Error> {
    let placeholders = doc_ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let mut list_item_map: HashMap<String, Vec<NoteListItemRecord>> = HashMap::new();
    let list_item_sql = format!(
        "SELECT id, document_id, text, tags_json, outlinks_json, line_number, line_count, \
         byte_offset, section_heading, parent_item_id, is_task, block_id, annotated, symbol \
         FROM list_items WHERE document_id IN ({placeholders})"
    );
    let mut list_item_stmt = connection.prepare(&list_item_sql)?;
    let list_item_rows = list_item_stmt.query_map(params_from_iter(doc_ids.iter()), |row| {
        Ok((
            row.get::<_, String>(1)?,
            (
                row.get::<_, String>(0)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, i64>(10)? != 0,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, i64>(12)? != 0,
                row.get::<_, String>(13)?,
            ),
        ))
    })?;
    for list_item_row in list_item_rows {
        let (
            doc_id,
            (
                id,
                text,
                tags_json,
                outlinks_json,
                line_number,
                line_count,
                byte_offset,
                section_heading,
                parent_item_id,
                is_task,
                block_id,
                annotated,
                symbol,
            ),
        ) = list_item_row?;
        let list_item = NoteListItemRecord {
            id,
            text,
            tags: parse_json_string_array(&tags_json)?,
            outlinks: parse_json_string_array(&outlinks_json)?,
            line_number,
            line_count,
            byte_offset,
            section_heading,
            parent_item_id,
            is_task,
            block_id,
            annotated,
            symbol,
        };
        list_item_map.entry(doc_id).or_default().push(list_item);
    }
    Ok(list_item_map)
}

fn build_tasknote_task_record(
    note: &NoteRecord,
    config: &VaultConfig,
    tasknote: &crate::IndexedTaskNote,
) -> NoteTaskRecord {
    let status_state = tasknotes_status_state(&config.tasknotes, &tasknote.status);
    let task_id = synthetic_file_link(&note.document_path, &note.file_ext);

    NoteTaskRecord {
        id: format!("tasknote:{}", note.document_id),
        list_item_id: format!("tasknote:{}", note.document_id),
        status_char: tasknote.status.clone(),
        status_name: status_state.name,
        status_type: status_state.status_type,
        status_next_symbol: None,
        checked: status_state.completed,
        completed: status_state.completed,
        text: tasknote.title.clone(),
        byte_offset: 0,
        parent_task_id: None,
        section_heading: None,
        line_number: 1,
        properties: tasknote_properties(note, config, tasknote, &task_id),
    }
}

fn tasknote_properties(
    note: &NoteRecord,
    config: &VaultConfig,
    tasknote: &crate::IndexedTaskNote,
    task_id: &str,
) -> Map<String, Value> {
    let mut properties = tasknote.custom_fields.clone();
    properties.insert("id".to_string(), Value::String(task_id.to_string()));
    properties.insert("title".to_string(), Value::String(tasknote.title.clone()));
    properties.insert("status".to_string(), Value::String(tasknote.status.clone()));
    properties.insert(
        "priority".to_string(),
        Value::String(tasknote.priority.clone()),
    );
    properties.insert("archived".to_string(), Value::Bool(tasknote.archived));
    properties.insert(
        "contexts".to_string(),
        json_string_array(tasknote.contexts.clone()),
    );
    properties.insert(
        "projects".to_string(),
        json_string_array(tasknote.projects.clone()),
    );
    properties.insert("tags".to_string(), json_string_array(tasknote.tags.clone()));
    properties.insert(
        "blockedBy".to_string(),
        Value::Array(tasknote.blocked_by.clone()),
    );
    properties.insert(
        "blocked-by".to_string(),
        json_string_array(tasknote_dependency_ids(tasknote)),
    );
    properties.insert(
        "reminders".to_string(),
        Value::Array(tasknote.reminders.clone()),
    );
    properties.insert(
        "timeEntries".to_string(),
        Value::Array(tasknote.time_entries.clone()),
    );
    properties.insert(
        "complete_instances".to_string(),
        json_string_array(tasknote.complete_instances.clone()),
    );
    properties.insert(
        "skipped_instances".to_string(),
        json_string_array(tasknote.skipped_instances.clone()),
    );

    if let Some(due) = &tasknote.due {
        properties.insert("due".to_string(), Value::String(due.clone()));
    }
    if let Some(scheduled) = &tasknote.scheduled {
        properties.insert("scheduled".to_string(), Value::String(scheduled.clone()));
    }
    if let Some(completed_date) = &tasknote.completed_date {
        properties.insert(
            "completedDate".to_string(),
            Value::String(completed_date.clone()),
        );
        properties.insert("done".to_string(), Value::String(completed_date.clone()));
    }
    if let Some(date_created) = &tasknote.date_created {
        properties.insert(
            "dateCreated".to_string(),
            Value::String(date_created.clone()),
        );
        properties.insert("created".to_string(), Value::String(date_created.clone()));
    }
    if let Some(date_modified) = &tasknote.date_modified {
        properties.insert(
            "dateModified".to_string(),
            Value::String(date_modified.clone()),
        );
        properties.insert("modified".to_string(), Value::String(date_modified.clone()));
    }
    if let Some(time_estimate) = tasknote
        .time_estimate
        .and_then(serde_json::Number::from_f64)
    {
        properties.insert("timeEstimate".to_string(), Value::Number(time_estimate));
    }
    if let Some(recurrence) = &tasknote.recurrence {
        properties.insert("recurrence".to_string(), Value::String(recurrence.clone()));
    }
    if let Some(recurrence_anchor) = &tasknote.recurrence_anchor {
        properties.insert(
            "recurrenceAnchor".to_string(),
            Value::String(recurrence_anchor.clone()),
        );
    }
    if let Some(priority_weight) = tasknotes_priority_weight(&config.tasknotes, &tasknote.priority)
        .and_then(serde_json::Number::from_f64)
    {
        properties.insert("priorityWeight".to_string(), Value::Number(priority_weight));
    }

    if let Some(completion) = tasknote_completion_anchor(tasknote) {
        properties.insert("completion".to_string(), Value::String(completion));
    }

    properties.insert(
        "path".to_string(),
        Value::String(note.document_path.clone()),
    );
    properties.insert("taskSource".to_string(), Value::String("file".to_string()));

    properties
}

fn tasknote_dependency_ids(tasknote: &crate::IndexedTaskNote) -> Vec<String> {
    tasknote
        .blocked_by
        .iter()
        .filter_map(|dependency| {
            dependency
                .as_object()
                .and_then(|object| object.get("uid"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|uid| !uid.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn tasknote_completion_anchor(tasknote: &crate::IndexedTaskNote) -> Option<String> {
    tasknote
        .completed_date
        .clone()
        .or_else(|| tasknote.complete_instances.iter().max().cloned())
}

#[must_use]
pub fn evaluate_note_inline_expressions(
    note: &NoteRecord,
    note_lookup: &dyn crate::note_lookup::NoteLookup,
) -> Vec<EvaluatedInlineExpression> {
    let formulas = BTreeMap::new();
    note.raw_inline_expressions
        .iter()
        .map(|expression| {
            let (value, error) = match Parser::new(expression) {
                Ok(parser) => match parser.parse() {
                    Ok(expr) => {
                        let ctx = EvalContext::new(note, &formulas).with_note_lookup(note_lookup);
                        match evaluate(&expr, &ctx) {
                            Ok(value) => (value, None),
                            Err(error) => (Value::Null, Some(error)),
                        }
                    }
                    Err(error) => (Value::Null, Some(error.clone())),
                },
                Err(error) => (Value::Null, Some(error.clone())),
            };

            EvaluatedInlineExpression {
                expression: expression.clone(),
                value,
                error,
            }
        })
        .collect()
}

fn parse_json_string_array(value: &str) -> Result<Vec<String>, rusqlite::Error> {
    serde_json::from_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(value.len(), SqlType::Text, Box::new(error))
    })
}

fn json_string_array(values: Vec<String>) -> Value {
    Value::Array(values.into_iter().map(Value::String).collect())
}

fn typed_property_json_value(
    value_text: Option<String>,
    value_number: Option<f64>,
    value_bool: Option<bool>,
    value_date: Option<String>,
    value_type: &str,
) -> Value {
    match value_type {
        "null" => Value::Null,
        "boolean" => value_bool.map_or(Value::Null, Value::Bool),
        "number" => value_number
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Null, Value::Number),
        "date" => value_date.map_or(Value::Null, Value::String),
        _ => value_text.map_or(Value::Null, Value::String),
    }
}

pub(crate) fn rebuild_property_catalog(
    transaction: &rusqlite::Transaction<'_>,
    configured_types: &BTreeMap<String, String>,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM property_catalog", [])?;
    transaction.execute(
        "
        INSERT INTO property_catalog (key, observed_type, usage_count, namespace)
        SELECT
            key,
            value_type,
            COUNT(*),
            CASE
                WHEN origin = 'frontmatter' THEN ?1
                ELSE ?2
            END
        FROM property_values
        GROUP BY
            key,
            value_type,
            CASE
                WHEN origin = 'frontmatter' THEN ?1
                ELSE ?2
            END
        ",
        [PROPERTY_NAMESPACE_FRONTMATTER, PROPERTY_NAMESPACE_INLINE],
    )?;

    insert_configured_property_types(transaction, configured_types)?;

    Ok(())
}

/// Refresh keys whose catalog membership changed. Callers must include keys
/// removed by the update, which cannot be recovered from the new property rows.
pub(crate) fn refresh_property_catalog_for_keys(
    transaction: &rusqlite::Transaction<'_>,
    affected_keys: &[String],
    configured_types: &BTreeMap<String, String>,
) -> Result<(), rusqlite::Error> {
    if affected_keys.is_empty() {
        return Ok(());
    }

    let key_placeholders: Vec<String> =
        (1..=affected_keys.len()).map(|i| format!("?{i}")).collect();
    let key_list = key_placeholders.join(", ");

    let params: Vec<String> = affected_keys.to_vec();
    let delete_sql = format!("DELETE FROM property_catalog WHERE key IN ({key_list})");
    transaction.execute(&delete_sql, rusqlite::params_from_iter(params.iter()))?;

    let insert_sql = format!(
        "INSERT INTO property_catalog (key, observed_type, usage_count, namespace)
         SELECT
             key,
             value_type,
             COUNT(*),
             CASE
                 WHEN origin = 'frontmatter' THEN ?{{frontmatter_param}}
                 ELSE ?{{inline_param}}
             END
         FROM property_values
         WHERE key IN ({key_list})
         GROUP BY
             key,
             value_type,
             CASE
                 WHEN origin = 'frontmatter' THEN ?{{frontmatter_param}}
                 ELSE ?{{inline_param}}
             END"
    );
    let mut insert_params: Vec<String> = affected_keys.to_vec();
    let frontmatter_param = insert_params.len() + 1;
    insert_params.push(PROPERTY_NAMESPACE_FRONTMATTER.to_string());
    let inline_param = insert_params.len() + 1;
    insert_params.push(PROPERTY_NAMESPACE_INLINE.to_string());
    let insert_sql = insert_sql
        .replace("{frontmatter_param}", &frontmatter_param.to_string())
        .replace("{inline_param}", &inline_param.to_string());
    transaction.execute(
        &insert_sql,
        rusqlite::params_from_iter(insert_params.iter()),
    )?;

    insert_configured_property_types(transaction, configured_types)?;

    Ok(())
}

fn insert_configured_property_types(
    transaction: &rusqlite::Transaction<'_>,
    configured_types: &BTreeMap<String, String>,
) -> Result<(), rusqlite::Error> {
    for (key, value_type) in configured_types {
        transaction.execute(
            "
            INSERT INTO property_catalog (key, observed_type, usage_count, namespace)
            VALUES (?1, ?2, 0, ?3)
            ON CONFLICT(key, observed_type, namespace) DO NOTHING
            ",
            (
                key,
                canonical_property_type(value_type),
                PROPERTY_NAMESPACE_FRONTMATTER,
            ),
        )?;
    }
    Ok(())
}

pub fn list_properties(paths: &VaultPaths) -> Result<Vec<PropertyCatalogEntry>, PropertyError> {
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let mut statement = connection.prepare(
        "
        SELECT key, observed_type, usage_count
        FROM property_catalog
        ORDER BY key ASC, observed_type ASC
        ",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;

    let mut aggregated = BTreeMap::<String, (usize, Vec<String>)>::new();
    for row in rows {
        let (key, observed_type, usage_count) = row?;
        let entry = aggregated.entry(key).or_insert_with(|| (0, Vec::new()));
        entry.0 = entry
            .0
            .saturating_add(usize::try_from(usage_count).unwrap_or(usize::MAX));
        if !entry.1.iter().any(|value| value == &observed_type) {
            entry.1.push(observed_type);
        }
    }

    Ok(aggregated
        .into_iter()
        .map(|(key, (count, mut types))| {
            types.sort();
            PropertyCatalogEntry { key, count, types }
        })
        .collect())
}

pub fn list_query_fields(paths: &VaultPaths) -> Result<Vec<QueryFieldCatalogEntry>, PropertyError> {
    let notes = query_notes(
        paths,
        &NoteQuery {
            filters: Vec::new(),
            sort_by: Some("file.path".to_string()),
            sort_descending: false,
        },
    )?;
    let properties = list_properties(paths)?;
    let note_records = notes.notes;

    let mut fields = builtin_query_fields(&note_records);
    for property in properties {
        fields.push(QueryFieldCatalogEntry {
            field: property.key.clone(),
            kind: "property".to_string(),
            supports: vec![
                "where".to_string(),
                "sort".to_string(),
                "fields".to_string(),
            ],
            types: property.types,
            example: property_example_value(&note_records, &property.key),
        });
    }
    Ok(fields)
}

fn builtin_query_fields(notes: &[NoteRecord]) -> Vec<QueryFieldCatalogEntry> {
    [
        ("file.path", vec!["where", "sort", "fields"], vec!["text"]),
        ("file.name", vec!["where", "sort", "fields"], vec!["text"]),
        ("file.ext", vec!["where", "sort", "fields"], vec!["text"]),
        (
            "file.mtime",
            vec!["where", "sort", "fields"],
            vec!["number"],
        ),
        ("file.tags", vec!["where", "fields"], vec!["list"]),
        ("file.starred", vec!["where", "fields"], vec!["boolean"]),
    ]
    .into_iter()
    .map(|(field, supports, types)| QueryFieldCatalogEntry {
        field: field.to_string(),
        kind: "builtin".to_string(),
        supports: supports.into_iter().map(str::to_string).collect(),
        types: types.into_iter().map(str::to_string).collect(),
        example: builtin_field_example_value(notes, field),
    })
    .collect()
}

fn builtin_field_example_value(notes: &[NoteRecord], field: &str) -> Value {
    notes
        .iter()
        .map(|note| match field {
            "file.path" => Value::String(note.document_path.clone()),
            "file.name" => Value::String(note.file_name.clone()),
            "file.ext" => Value::String(note.file_ext.clone()),
            "file.mtime" => Value::Number(note.file_mtime.into()),
            "file.tags" => Value::Array(note.tags.iter().cloned().map(Value::String).collect()),
            "file.starred" => Value::Bool(note.starred),
            _ => Value::Null,
        })
        .find(|value| !value.is_null())
        .unwrap_or(Value::Null)
}

fn property_example_value(notes: &[NoteRecord], key: &str) -> Value {
    notes
        .iter()
        .filter_map(|note| note.properties.get(key))
        .find(|value| !value.is_null())
        .cloned()
        .unwrap_or(Value::Null)
}

fn open_existing_cache(paths: &VaultPaths) -> Result<CacheDatabase, PropertyError> {
    if !paths.cache_db().exists() {
        return Err(PropertyError::CacheMissing);
    }

    CacheDatabase::open(paths).map_err(PropertyError::from)
}

#[derive(Debug, Clone, PartialEq)]
struct ExtractedPropertyValue {
    value: IndexedPropertyValue,
    list_items: Vec<IndexedPropertyListItem>,
    diagnostic: Option<PropertyTypeDiagnostic>,
}

fn extract_property_value(
    key: &str,
    value: &serde_yaml::Value,
    config: &VaultConfig,
    expected_type: Option<&str>,
) -> ExtractedPropertyValue {
    let normalized = normalize_property_value(value, config, expected_type);
    let diagnostic = expected_type.and_then(|expected| {
        if property_type_matches(expected, &normalized.value_type) {
            None
        } else {
            Some(PropertyTypeDiagnostic {
                key: key.to_string(),
                expected_type: expected.to_string(),
                actual_type: normalized.value_type.clone(),
                message: format!(
                    "Property '{key}' expected type {expected} but observed {}",
                    normalized.value_type
                ),
            })
        }
    });

    ExtractedPropertyValue {
        value: IndexedPropertyValue {
            key: key.to_string(),
            value_text: normalized.value_text,
            value_number: normalized.value_number,
            value_bool: normalized.value_bool,
            value_date: normalized.value_date,
            value_type: normalized.value_type,
            origin: PropertyValueOrigin::Frontmatter,
        },
        list_items: normalized
            .list_items
            .into_iter()
            .enumerate()
            .map(|(item_index, value_text)| IndexedPropertyListItem {
                key: key.to_string(),
                item_index,
                value_text,
            })
            .collect(),
        diagnostic,
    }
}

#[derive(Debug, Clone, PartialEq)]
struct NormalizedPropertyValue {
    value_text: Option<String>,
    value_number: Option<f64>,
    value_bool: Option<bool>,
    value_date: Option<String>,
    value_type: String,
    list_items: Vec<String>,
}

fn normalize_property_value(
    value: &serde_yaml::Value,
    config: &VaultConfig,
    expected_type: Option<&str>,
) -> NormalizedPropertyValue {
    if let serde_yaml::Value::String(text) = value {
        if let Some(expected_type) = expected_type {
            if let Some(coerced) = coerce_string_to_expected_type(text, expected_type, config) {
                return coerced;
            }
        }
    }

    match value {
        serde_yaml::Value::Null => NormalizedPropertyValue {
            value_text: None,
            value_number: None,
            value_bool: None,
            value_date: None,
            value_type: "null".to_string(),
            list_items: Vec::new(),
        },
        serde_yaml::Value::Bool(value_bool) => NormalizedPropertyValue {
            value_text: None,
            value_number: None,
            value_bool: Some(*value_bool),
            value_date: None,
            value_type: "boolean".to_string(),
            list_items: Vec::new(),
        },
        serde_yaml::Value::Number(number) => NormalizedPropertyValue {
            value_text: None,
            value_number: number.as_f64(),
            value_bool: None,
            value_date: None,
            value_type: "number".to_string(),
            list_items: Vec::new(),
        },
        serde_yaml::Value::String(text) => {
            if is_internal_link_value(text, config) {
                return NormalizedPropertyValue {
                    value_text: Some(text.clone()),
                    value_number: None,
                    value_bool: None,
                    value_date: None,
                    value_type: "link".to_string(),
                    list_items: Vec::new(),
                };
            }
            if let Some(normalized_date) = normalize_date_string(text) {
                return NormalizedPropertyValue {
                    value_text: None,
                    value_number: None,
                    value_bool: None,
                    value_date: Some(normalized_date.to_string()),
                    value_type: "date".to_string(),
                    list_items: Vec::new(),
                };
            }
            if let Some(normalized_duration) = normalize_duration_string(text) {
                return NormalizedPropertyValue {
                    value_text: Some(normalized_duration.to_string()),
                    value_number: None,
                    value_bool: None,
                    value_date: None,
                    value_type: "duration".to_string(),
                    list_items: Vec::new(),
                };
            }

            NormalizedPropertyValue {
                value_text: Some(text.clone()),
                value_number: None,
                value_bool: None,
                value_date: None,
                value_type: "text".to_string(),
                list_items: Vec::new(),
            }
        }
        serde_yaml::Value::Sequence(values) => NormalizedPropertyValue {
            value_text: None,
            value_number: None,
            value_bool: None,
            value_date: None,
            value_type: "list".to_string(),
            list_items: values.iter().map(yaml_scalar_to_text).collect(),
        },
        serde_yaml::Value::Mapping(_) => NormalizedPropertyValue {
            value_text: None,
            value_number: None,
            value_bool: None,
            value_date: None,
            value_type: "object".to_string(),
            list_items: Vec::new(),
        },
        serde_yaml::Value::Tagged(tagged) => {
            normalize_property_value(&tagged.value, config, expected_type)
        }
    }
}

fn coerce_string_to_expected_type(
    text: &str,
    expected_type: &str,
    config: &VaultConfig,
) -> Option<NormalizedPropertyValue> {
    match expected_type {
        "boolean" => parse_boolean(text).map(|value_bool| NormalizedPropertyValue {
            value_text: None,
            value_number: None,
            value_bool: Some(value_bool),
            value_date: None,
            value_type: "boolean".to_string(),
            list_items: Vec::new(),
        }),
        "number" => text
            .parse::<f64>()
            .ok()
            .map(|value_number| NormalizedPropertyValue {
                value_text: None,
                value_number: Some(value_number),
                value_bool: None,
                value_date: None,
                value_type: "number".to_string(),
                list_items: Vec::new(),
            }),
        "date" => normalize_date_string(text).map(|value_date| NormalizedPropertyValue {
            value_text: None,
            value_number: None,
            value_bool: None,
            value_date: Some(value_date.to_string()),
            value_type: "date".to_string(),
            list_items: Vec::new(),
        }),
        "duration" => {
            normalize_duration_string(text).map(|value_duration| NormalizedPropertyValue {
                value_text: Some(value_duration.to_string()),
                value_number: None,
                value_bool: None,
                value_date: None,
                value_type: "duration".to_string(),
                list_items: Vec::new(),
            })
        }
        "link" => {
            if is_internal_link_value(text, config) {
                Some(NormalizedPropertyValue {
                    value_text: Some(text.to_string()),
                    value_number: None,
                    value_bool: None,
                    value_date: None,
                    value_type: "link".to_string(),
                    list_items: Vec::new(),
                })
            } else {
                None
            }
        }
        "list" => parse_inline_quoted_list(text).map(|list_items| NormalizedPropertyValue {
            value_text: None,
            value_number: None,
            value_bool: None,
            value_date: None,
            value_type: "list".to_string(),
            list_items,
        }),
        _ => None,
    }
}

fn property_type_matches(expected_type: &str, actual_type: &str) -> bool {
    expected_type == actual_type || (expected_type == "text" && actual_type == "link")
}

pub(crate) fn canonical_property_type(raw_type: &str) -> &str {
    match raw_type.to_ascii_lowercase().as_str() {
        "bool" | "boolean" | "checkbox" => "boolean",
        "date" | "datetime" => "date",
        "duration" | "interval" => "duration",
        "list" | "multitext" | "tags" => "list",
        "link" | "file" => "link",
        "number" => "number",
        "object" => "object",
        "null" => "null",
        _ => "text",
    }
}

fn yaml_to_json(value: &serde_yaml::Value) -> Value {
    match value {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(value_bool) => Value::Bool(*value_bool),
        serde_yaml::Value::Number(number) => {
            if let Some(value_i64) = number.as_i64() {
                Value::Number(value_i64.into())
            } else if let Some(value_u64) = number.as_u64() {
                Value::Number(value_u64.into())
            } else if let Some(value_f64) = number.as_f64() {
                serde_json::Number::from_f64(value_f64).map_or(Value::Null, Value::Number)
            } else {
                Value::Null
            }
        }
        serde_yaml::Value::String(text) => Value::String(text.clone()),
        serde_yaml::Value::Sequence(values) => {
            Value::Array(values.iter().map(yaml_to_json).collect::<Vec<_>>())
        }
        serde_yaml::Value::Mapping(values) => {
            let mut object = Map::new();
            for (key, value) in values {
                let key = key
                    .as_str()
                    .map_or_else(|| yaml_scalar_to_text(key), ToOwned::to_owned);
                object.insert(key, yaml_to_json(value));
            }
            Value::Object(object)
        }
        serde_yaml::Value::Tagged(tagged) => yaml_to_json(&tagged.value),
    }
}

fn yaml_scalar_to_text(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::Null => "null".to_string(),
        serde_yaml::Value::Bool(value_bool) => value_bool.to_string(),
        serde_yaml::Value::Number(number) => number.to_string(),
        serde_yaml::Value::String(text) => text.clone(),
        other => serde_json::to_string(&yaml_to_json(other)).unwrap_or_default(),
    }
}

fn parse_boolean(value: &str) -> Option<bool> {
    match value.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn normalize_date_string(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if matches_iso_year_month(trimmed) || matches_iso_date(trimmed) || matches_iso_datetime(trimmed)
    {
        Some(trimmed)
    } else {
        None
    }
}

fn normalize_duration_string(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    parse_duration_string(trimmed).map(|_| trimmed)
}

fn matches_iso_year_month(value: &str) -> bool {
    value.len() == 7
        && value.as_bytes()[4] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || byte.is_ascii_digit())
}

fn matches_iso_date(value: &str) -> bool {
    value.len() == 10
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

fn matches_iso_datetime(value: &str) -> bool {
    let Some((date, time)) = value.split_once('T') else {
        return false;
    };
    matches_iso_date(date)
        && time
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b':' | b'.' | b'Z' | b'+' | b'-'))
}

fn is_internal_link_value(value: &str, config: &VaultConfig) -> bool {
    let trimmed = value.trim();
    if !(trimmed.contains("[[") || trimmed.contains("](")) {
        return false;
    }

    let parsed = parse_document(trimmed, config);
    parsed.links.len() == 1
        && parsed.links[0].raw_text == trimmed
        && parsed.links[0].target_path_candidate.is_some()
        && parsed.links[0].link_kind != crate::LinkKind::External
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FilterField {
    Property(String),
    FilePath,
    FileName,
    FileExt,
    FileMtime,
    FileCtime,
    FileTags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterOperator {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    StartsWith,
    Contains,
    HasTag,
    Matches,
    MatchesI,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FilterValue {
    Null,
    Bool(bool),
    Number(f64),
    Date(String),
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParsedFilter {
    pub field: FilterField,
    pub operator: FilterOperator,
    pub value: FilterValue,
}

pub(crate) fn parse_note_filter_expression(filter: &str) -> Result<ParsedFilter, PropertyError> {
    parse_filter_expression(filter)
}

fn parse_filter_expression(filter: &str) -> Result<ParsedFilter, PropertyError> {
    for (separator, operator) in [
        (" matches_i ", FilterOperator::MatchesI),
        (" matches ", FilterOperator::Matches),
        (" has_tag ", FilterOperator::HasTag),
        (" contains ", FilterOperator::Contains),
        (" starts_with ", FilterOperator::StartsWith),
        (" >= ", FilterOperator::Gte),
        (" <= ", FilterOperator::Lte),
        (" != ", FilterOperator::Ne),
        (" = ", FilterOperator::Eq),
        (" > ", FilterOperator::Gt),
        (" < ", FilterOperator::Lt),
    ] {
        if let Some((field, value)) = filter.split_once(separator) {
            let field = field.trim();
            if !is_legacy_filter_field(field) {
                break;
            }
            let value = value.trim();
            if legacy_filter_needs_expression_fallback(operator, value) {
                break;
            }
            return Ok(ParsedFilter {
                field: parse_filter_field(field),
                operator,
                value: parse_filter_value(value),
            });
        }
    }

    Err(PropertyError::InvalidFilter(filter.to_string()))
}

fn legacy_filter_needs_expression_fallback(operator: FilterOperator, value: &str) -> bool {
    matches!(
        operator,
        FilterOperator::Eq
            | FilterOperator::Ne
            | FilterOperator::Gt
            | FilterOperator::Gte
            | FilterOperator::Lt
            | FilterOperator::Lte
    ) && !is_sql_literal_filter_value(value)
}

fn is_legacy_filter_field(field: &str) -> bool {
    matches!(
        field,
        "file.path"
            | "file.name"
            | "file.ext"
            | "file.extension"
            | "file.mtime"
            | "file.ctime"
            | "file.tags"
    ) || (!field.starts_with("file.")
        && !field.is_empty()
        && field
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')))
}

/// [`NoteQuery`] filters compiled per query-architecture §4.7: tag sources
/// run exactly in SQL, and every other filter is a Vulcan expression.
struct CompiledNoteFilters {
    sources: Vec<crate::source::SourceExpr>,
    expressions: Vec<CompiledExpressionFilter>,
}

#[derive(Debug)]
struct CompiledExpressionFilter {
    filter: String,
    expr: Expr,
    predicate: crate::predicate::Predicate,
}

fn compile_note_filters(filters: &[String]) -> Result<CompiledNoteFilters, PropertyError> {
    let mut sources = Vec::new();
    let mut expressions = Vec::new();
    for filter in filters {
        if let Ok(parsed) = parse_filter_expression(filter) {
            if let Some(source) = filter_source(&parsed) {
                sources.push(source);
                continue;
            }
        }
        let source = note_filter_expression_source(filter)?;
        let expr = Parser::new(&source)
            .and_then(Parser::parse)
            .map_err(|_| PropertyError::InvalidFilter(filter.clone()))?;
        let predicate = crate::predicate::Predicate::lower_dataview(&expr);
        expressions.push(CompiledExpressionFilter {
            filter: filter.clone(),
            expr,
            predicate,
        });
    }
    Ok(CompiledNoteFilters {
        sources,
        expressions,
    })
}

/// The source a filter is, if any: tags, and folder prefixes such as Bases'
/// `file.inFolder("x")` (`file.path starts_with "x/"`), whose expression
/// meaning is the same byte-exact selection.
fn filter_source(parsed: &ParsedFilter) -> Option<crate::source::SourceExpr> {
    let FilterValue::Text(value) = &parsed.value else {
        return None;
    };
    if is_tag_source(parsed) {
        let tag = value.strip_prefix('#').unwrap_or(value);
        return Some(crate::source::SourceExpr::Tag(tag.to_string()));
    }
    let folder = value.strip_suffix('/')?;
    (parsed.field == FilterField::FilePath
        && parsed.operator == FilterOperator::StartsWith
        && !folder.is_empty())
    .then(|| crate::source::SourceExpr::Folder(folder.to_string()))
}

/// `file.tags has_tag t` and `file.tags contains t` select notes tagged `t`
/// or a tag nested under it.
fn is_tag_source(parsed: &ParsedFilter) -> bool {
    parsed.field == FilterField::FileTags
        && matches!(
            parsed.operator,
            FilterOperator::HasTag | FilterOperator::Contains
        )
        && matches!(parsed.value, FilterValue::Text(_))
}

/// The Vulcan expression a note filter means (query-architecture §4.7):
/// `key op value` filters are surface syntax for an expression, and any
/// other filter already is one.
pub(crate) fn note_filter_expression_source(filter: &str) -> Result<String, PropertyError> {
    let Ok(parsed) = parse_filter_expression(filter) else {
        return Ok(filter.to_string());
    };
    let field = match &parsed.field {
        FilterField::Property(key) => property_expression_source(key),
        FilterField::FilePath => "file.path".to_string(),
        FilterField::FileName => "file.name".to_string(),
        FilterField::FileExt => "file.ext".to_string(),
        FilterField::FileMtime => "file.mtime".to_string(),
        FilterField::FileCtime => "file.ctime".to_string(),
        FilterField::FileTags => "file.tags".to_string(),
    };
    let value = filter_value_source(&parsed.value);
    let comparison = |operator: &str| Ok(format!("{field} {operator} {value}"));
    match parsed.operator {
        FilterOperator::Eq => comparison("=="),
        FilterOperator::Ne => comparison("!="),
        FilterOperator::Gt => comparison(">"),
        FilterOperator::Gte => comparison(">="),
        FilterOperator::Lt => comparison("<"),
        FilterOperator::Lte => comparison("<="),
        FilterOperator::StartsWith => Ok(format!("startswith({field}, {value})")),
        FilterOperator::Contains | FilterOperator::HasTag if is_tag_source(&parsed) => {
            Ok(format!("file.hasTag({value})"))
        }
        FilterOperator::Contains => Ok(format!("contains({field}, {value})")),
        FilterOperator::HasTag => Err(PropertyError::InvalidFilter(format!(
            "{filter} (has_tag selects file.tags; use contains for list properties)"
        ))),
        FilterOperator::Matches | FilterOperator::MatchesI => {
            let FilterValue::Text(pattern) = &parsed.value else {
                return Err(PropertyError::InvalidFilter(format!(
                    "{filter} (regex filters require a text pattern)"
                )));
            };
            let pattern = if parsed.operator == FilterOperator::MatchesI {
                format!("(?i:{pattern})")
            } else {
                pattern.clone()
            };
            Regex::new(&pattern)
                .map_err(|error| PropertyError::InvalidFilter(format!("{filter} ({error})")))?;
            Ok(format!(
                "regextest({}, {field})",
                serde_json::to_string(&pattern).expect("strings serialize")
            ))
        }
    }
}

fn filter_value_source(value: &FilterValue) -> String {
    match value {
        FilterValue::Null => "null".to_string(),
        FilterValue::Bool(value) => value.to_string(),
        FilterValue::Number(value) => serde_json::Number::from_f64(*value)
            .map_or_else(|| value.to_string(), |value| value.to_string()),
        FilterValue::Date(value) | FilterValue::Text(value) => {
            serde_json::to_string(value).expect("strings serialize")
        }
    }
}

/// `key == value` for any property key spelling, with the value read like a
/// note filter value.
pub(crate) fn property_equals_source(key: &str, value: &str) -> String {
    format!(
        "{} == {}",
        property_expression_source(key),
        filter_value_source(&parse_filter_value(value))
    )
}

/// A property reference in expression syntax: the bare key when it reads
/// back as that identifier, else `note["key"]`, which resolves the same way.
pub(crate) fn property_expression_source(key: &str) -> String {
    let bare = Parser::new(key)
        .and_then(Parser::parse)
        .is_ok_and(|expr| matches!(&expr, Expr::Identifier(name) if name == key))
        && !matches!(
            crate::expression::eval::normalize_field_name(key).as_str(),
            "this" | "file" | "note"
        );
    if bare {
        key.to_string()
    } else {
        format!(
            "note[{}]",
            serde_json::to_string(key).expect("strings serialize")
        )
    }
}

fn expression_filter_matches(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(expression_filter_matches),
        value => is_truthy(value),
    }
}

fn parse_filter_field(field: &str) -> FilterField {
    match field {
        "file.path" => FilterField::FilePath,
        "file.name" => FilterField::FileName,
        "file.ext" | "file.extension" => FilterField::FileExt,
        "file.mtime" => FilterField::FileMtime,
        "file.ctime" => FilterField::FileCtime,
        "file.tags" => FilterField::FileTags,
        other => FilterField::Property(
            other
                .strip_prefix("properties.")
                .unwrap_or(other)
                .to_string(),
        ),
    }
}

fn parse_filter_value(value: &str) -> FilterValue {
    if let Some(unquoted) = strip_quotes(value) {
        return FilterValue::Text(unquoted.to_string());
    }

    match value.trim().to_ascii_lowercase().as_str() {
        "null" => FilterValue::Null,
        "true" => FilterValue::Bool(true),
        "false" => FilterValue::Bool(false),
        _ => {
            if let Ok(number) = value.trim().parse::<f64>() {
                return FilterValue::Number(number);
            }
            if let Some(date) = normalize_date_string(value) {
                return FilterValue::Date(date.to_string());
            }

            FilterValue::Text(value.trim().to_string())
        }
    }
}

/// The text of one quoted literal; `"a" || b = "c"` is not one.
fn strip_quotes(value: &str) -> Option<&str> {
    let quote = value
        .chars()
        .next()
        .filter(|quote| matches!(quote, '"' | '\''))?;
    let inner = value.strip_prefix(quote)?.strip_suffix(quote)?;
    (!inner.contains(quote)).then_some(inner)
}

fn is_sql_literal_filter_value(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }
    if strip_quotes(trimmed).is_some() {
        return true;
    }
    if matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "null" | "true" | "false"
    ) {
        return true;
    }
    if trimmed.parse::<f64>().is_ok() || normalize_date_string(trimmed).is_some() {
        return true;
    }
    if is_wikilink_literal(trimmed) {
        return true;
    }
    if normalize_duration_string(trimmed).is_some() {
        return false;
    }
    trimmed.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/' | b'#')
    })
}

fn is_wikilink_literal(value: &str) -> bool {
    (value.starts_with("[[") || value.starts_with("![[")) && value.ends_with("]]")
}

#[derive(Debug, Clone, PartialEq)]
enum SortKey {
    Null,
    Bool(bool),
    Integer(i64),
    Number(f64),
    Text(String),
}

fn sort_key_for_note(note: &NoteRecord, sort_by: &str) -> SortKey {
    match sort_by {
        "file.path" => SortKey::Text(note.document_path.clone()),
        "file.name" => SortKey::Text(note.file_name.clone()),
        "file.ext" | "file.extension" => SortKey::Text(note.file_ext.clone()),
        "file.ctime" => SortKey::Integer(note.file_ctime),
        "file.mtime" => SortKey::Integer(note.file_mtime),
        key => match note
            .properties
            .get(key.strip_prefix("properties.").unwrap_or(key))
        {
            Some(Value::Null) | None => SortKey::Null,
            Some(Value::Bool(value_bool)) => SortKey::Bool(*value_bool),
            Some(Value::Number(value_number)) => {
                SortKey::Number(value_number.as_f64().unwrap_or_default())
            }
            Some(Value::String(value_text)) => SortKey::Text(value_text.clone()),
            Some(other) => SortKey::Text(other.to_string()),
        },
    }
}

fn compare_sort_keys(left: &SortKey, right: &SortKey) -> Ordering {
    let left_rank = sort_key_rank(left);
    let right_rank = sort_key_rank(right);
    left_rank
        .cmp(&right_rank)
        .then_with(|| match (left, right) {
            (SortKey::Bool(left), SortKey::Bool(right)) => left.cmp(right),
            (SortKey::Integer(left), SortKey::Integer(right)) => left.cmp(right),
            (SortKey::Number(left), SortKey::Number(right)) => {
                left.partial_cmp(right).unwrap_or(Ordering::Equal)
            }
            (SortKey::Text(left), SortKey::Text(right)) => left.cmp(right),
            _ => Ordering::Equal,
        })
}

fn sort_key_rank(key: &SortKey) -> u8 {
    match key {
        SortKey::Null => 0,
        SortKey::Bool(_) => 1,
        SortKey::Integer(_) => 2,
        SortKey::Number(_) => 3,
        SortKey::Text(_) => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{PathPermission, ResourceSpecifier};
    use crate::{file_metadata::FileMetadataResolver, parse_document, scan_vault, ScanMode};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn public_only_filter() -> PermissionFilter {
        PermissionFilter::new(PathPermission {
            allow: vec![ResourceSpecifier::Folder("Public/**".to_string())],
            deny: Vec::new(),
        })
    }

    #[test]
    fn note_lookup_preserves_duplicate_names_and_path_overlays() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".vulcan")).unwrap();
        for (path, score) in [
            ("Public/One/Item.md", 1),
            ("Public/Two/Item.md", 2),
            ("Private/Item.md", 3),
        ] {
            fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            fs::write(
                root.join(path),
                format!("---\nscore: {score}\naliases: [Shared]\n---\n"),
            )
            .unwrap();
        }
        fs::write(root.join("Public/Unique.md"), "# Unique\n").unwrap();
        let paths = VaultPaths::new(root);
        scan_vault(&paths, ScanMode::Full).unwrap();
        let index = load_note_index_with_filter(&paths, Some(&public_only_filter())).unwrap();
        assert_eq!(index.len(), 3);
        assert!(index.contains_key("Unique"));
        assert!(!index.contains_key("Item"));
        assert!(index
            .values()
            .all(|note| note.document_path.starts_with("Public/")));
        for (target, expected) in [
            ("Public/One/Item", "Public/One/Item.md"),
            ("Public/Two/Item.md", "Public/Two/Item.md"),
            ("Item", "Public/Two/Item.md"),
            ("Shared", "Public/Two/Item.md"),
        ] {
            let resolved = crate::expression::eval::resolve_note_reference(
                &index,
                "Public/Two/Source.md",
                target,
            )
            .unwrap();
            assert_eq!(resolved.document_path, expected);
        }
        for (target, expected) in [
            ("One/Item", Some("Public/One/Item.md")),
            ("Private/Item", None),
            ("Elsewhere/Item", None),
        ] {
            assert_eq!(
                crate::expression::eval::resolve_note_reference(
                    &index,
                    "Public/Two/Source.md",
                    target,
                )
                .map(|note| note.document_path.as_str()),
                expected
            );
        }
        let mut replacement = index
            .values()
            .find(|note| note.document_path == "Public/One/Item.md")
            .unwrap()
            .clone();
        replacement.properties = serde_json::json!({"score": 9});
        let overlay = build_note_lookup_index(index.values().cloned().chain([replacement]));
        assert_eq!(overlay.len(), 3);
        assert_eq!(overlay["/Public/One/Item.md"].properties["score"], 9);
        assert_eq!(overlay["/Public/Two/Item.md"].properties["score"], 2);
        let mut records = overlay.values().cloned().collect::<Vec<_>>();
        records.reverse();
        let reversed = build_note_lookup_index(records);
        assert_eq!(
            reversed.keys().collect::<HashSet<_>>(),
            overlay.keys().collect::<HashSet<_>>()
        );
        assert_eq!(load_note_index(&paths).unwrap().len(), 4);
    }

    #[test]
    fn guarded_note_index_applies_policy_before_tasks_and_backlinks() {
        struct Guard {
            grant: crate::permissions::PermissionGrant,
            calls: std::cell::RefCell<Vec<String>>,
            deny: std::cell::Cell<bool>,
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
                assert_eq!(action, "read");
                let path = resource.unwrap();
                self.calls.borrow_mut().push(path.to_string());
                if self.deny.get() && path == "BPolicy.md" {
                    Err(crate::permissions::PermissionError::PathDenied {
                        profile: "test".into(),
                        action,
                        path: path.into(),
                    })
                } else {
                    Ok(())
                }
            }
        }
        let temp = TempDir::new().unwrap();
        let paths = VaultPaths::new(temp.path());
        fs::create_dir_all(paths.vulcan_dir()).unwrap();
        for (path, source) in [
            ("AHidden.md", "- [ ] Hidden\n[[CTarget]]\n"),
            (
                "BPolicy.md",
                "---\ntags: [visible]\n---\n- [ ] Policy denied\n[[CTarget]]\n",
            ),
            (
                "CTarget.md",
                "---\ntags: [visible]\n---\n- [ ] Visible\n[[AHidden]]\n",
            ),
            ("DSource.md", "---\ntags: [visible]\n---\n[[CTarget]]\n"),
            (
                "EStaticDenied.md",
                "---\ntags: [visible, secret]\n---\n[[CTarget]]\n",
            ),
        ] {
            fs::write(temp.path().join(path), source).unwrap();
        }
        scan_vault(&paths, ScanMode::Full).unwrap();
        let mut grant = crate::permissions::resolve_permission_profile(&paths, None)
            .unwrap()
            .grant;
        grant.read = PathPermission {
            allow: vec![ResourceSpecifier::Tag("visible".into())],
            deny: vec![ResourceSpecifier::Tag("secret".into())],
        };
        let guard = Guard {
            grant,
            calls: std::cell::RefCell::default(),
            deny: std::cell::Cell::new(true),
        };
        let index = load_note_index_with_guard(&paths, &guard).unwrap();
        assert_eq!(index.len(), 2);
        assert_eq!(index["CTarget"].inlinks, vec!["[[DSource]]"]);
        assert_eq!(index["CTarget"].links, vec!["[[AHidden]]"]);
        let result = crate::tasks::evaluate_tasks_query_with_note_index(
            "not done\ngroup by path\nlimit 1",
            &index,
        )
        .unwrap();
        assert_eq!(result.result_count, 1);
        assert_eq!(result.tasks[0]["path"], "CTarget.md");
        assert_eq!(result.groups[0].tasks[0]["path"], "CTarget.md");
        let mut calls = guard.calls.borrow().clone();
        calls.sort();
        assert_eq!(calls, vec!["BPolicy.md", "CTarget.md", "DSource.md"]);
        // A fresh read must not reuse decisions from the previous operation.
        guard.deny.set(false);
        let index = load_note_index_with_guard(&paths, &guard).unwrap();
        assert_eq!(index.len(), 3);
        assert_eq!(index["CTarget"].inlinks.len(), 2);
    }

    #[test]
    fn scoped_hydration_filters_backlink_sources_not_query_results() {
        let temp = TempDir::new().unwrap();
        let paths = VaultPaths::new(temp.path());
        fs::create_dir_all(paths.vulcan_dir()).unwrap();
        fs::create_dir_all(temp.path().join("Public")).unwrap();
        fs::create_dir_all(temp.path().join("Hidden")).unwrap();
        fs::write(
            temp.path().join("Public/Target.md"),
            "---\ntags: [visible]\ncategories: [project]\n---\n[[Hidden/Secret]]\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("Public/Source.md"),
            "#visible\n[[Public/Target]]\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("Public/Denied.md"),
            "#visible\n[[Public/Target]]\n",
        )
        .unwrap();
        fs::write(temp.path().join("Hidden/Secret.md"), "[[Public/Target]]\n").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let query = NoteQuery {
            filters: vec!["file.path = \"Public/Target.md\"".into()],
            sort_by: None,
            sort_descending: false,
        };
        for allow in [
            ResourceSpecifier::Folder("Public/**".into()),
            ResourceSpecifier::Tag("visible".into()),
        ] {
            let filter = PermissionFilter::new(PathPermission {
                allow: vec![allow],
                deny: vec![ResourceSpecifier::Note("Public/Denied.md".into())],
            });
            for with_filter in [false, true] {
                let mut scoped_query = query.clone();
                if with_filter {
                    // Predicate bindings follow the permission bindings.
                    scoped_query.filters.push("file.name != \"Other\"".into());
                }
                let scoped = query_notes_with_filter(&paths, &scoped_query, Some(&filter)).unwrap();
                assert_eq!(scoped.notes.len(), 1);
                assert_eq!(scoped.notes[0].inlinks, vec!["[[Public/Source]]"]);
                assert_eq!(scoped.notes[0].links, vec!["[[Hidden/Secret]]"]);
            }
            let index = load_note_index_with_filter(&paths, Some(&filter)).unwrap();
            assert_eq!(index["Target"].inlinks, vec!["[[Public/Source]]"]);
        }
        let unrestricted = query_notes(&paths, &query).unwrap();
        assert_eq!(unrestricted.notes[0].inlinks.len(), 3);
    }

    #[test]
    fn filtered_inline_expressions_cannot_read_denied_linked_note_properties() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("cache directory");
        fs::create_dir_all(vault_root.join("Public")).expect("public directory");
        fs::create_dir_all(vault_root.join("Private")).expect("private directory");
        fs::write(
            vault_root.join("Public/Dashboard.md"),
            "---\ntarget: \"[[Private/Secret]]\"\n---\n# Dashboard\n\n`= target.secret`\n",
        )
        .expect("public note");
        fs::write(
            vault_root.join("Private/Secret.md"),
            "---\nsecret: classified\n---\n# Secret\n",
        )
        .expect("private note");
        assert_eq!(
            parse_document(
                "---\ntarget: \"[[Private/Secret]]\"\n---\n# Dashboard\n\n`= target.secret`\n",
                &VaultConfig::default(),
            )
            .inline_expressions
            .len(),
            1
        );
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let report = query_notes_with_filter(
            &paths,
            &NoteQuery {
                filters: Vec::new(),
                sort_by: None,
                sort_descending: false,
            },
            Some(&public_only_filter()),
        )
        .expect("filtered query should succeed");

        assert_eq!(report.notes.len(), 1);
        assert_eq!(report.notes[0].document_path, "Public/Dashboard.md");
        assert_eq!(report.notes[0].inline_expressions.len(), 1);
        assert_eq!(report.notes[0].inline_expressions[0].value, Value::Null);
    }

    #[test]
    fn filtered_expression_filters_cannot_use_denied_linked_note_properties() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("cache directory");
        fs::create_dir_all(vault_root.join("Public")).expect("public directory");
        fs::create_dir_all(vault_root.join("Private")).expect("private directory");
        fs::write(
            vault_root.join("Public/Dashboard.md"),
            "---\ntarget: \"[[Private/Secret]]\"\n---\n# Dashboard\n",
        )
        .expect("public note");
        fs::write(
            vault_root.join("Private/Secret.md"),
            "---\nsecret: classified\n---\n# Secret\n",
        )
        .expect("private note");
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let report = query_notes_with_filter(
            &paths,
            &NoteQuery {
                filters: vec!["target.secret = classified".to_string()],
                sort_by: None,
                sort_descending: false,
            },
            Some(&public_only_filter()),
        )
        .expect("filtered query should succeed");

        assert!(report.notes.is_empty());
    }

    #[test]
    fn file_ctime_uses_filesystem_metadata_and_missing_files_fall_back_to_mtime() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::write(vault_root.join("Note.md"), "# Note\n").expect("note should be written");

        let existing = file_ctime_for_document(vault_root, "Note.md", -1);
        let missing = file_ctime_for_document(vault_root, "Missing.md", 123);

        assert_ne!(existing, -1);
        assert_eq!(missing, 123);
    }

    #[test]
    fn extracts_property_rows_with_coercion_and_diagnostics() {
        let mut config = VaultConfig::default();
        config
            .property_types
            .insert("due".to_string(), "date".to_string());
        config
            .property_types
            .insert("reviewed".to_string(), "checkbox".to_string());
        config
            .property_types
            .insert("status".to_string(), "text".to_string());
        let parsed = parse_document(
            "---\ndue: 2026-03-01\nreviewed: \"true\"\nrelated:\n  - \"[[Backlog]]\"\n  - sprint\nstatus:\n  - done\n---\n",
            &config,
        );
        let indexed = extract_indexed_properties(&parsed, &config)
            .expect("property extraction should succeed")
            .expect("frontmatter should produce properties");

        assert_eq!(indexed.values.len(), 4);
        assert_eq!(
            indexed
                .values
                .iter()
                .find(|value| value.key == "due")
                .and_then(|value| value.value_date.as_deref()),
            Some("2026-03-01")
        );
        assert_eq!(
            indexed
                .values
                .iter()
                .find(|value| value.key == "reviewed")
                .and_then(|value| value.value_bool),
            Some(true)
        );
        assert_eq!(
            indexed
                .values
                .iter()
                .find(|value| value.key == "related")
                .map(|value| value.value_type.as_str()),
            Some("list")
        );
        assert_eq!(
            indexed
                .list_items
                .iter()
                .filter(|item| item.key == "related")
                .count(),
            2
        );
        assert_eq!(indexed.diagnostics.len(), 1);
        assert_eq!(indexed.diagnostics[0].key, "status");
    }

    #[test]
    fn inline_fields_infer_dataview_types() {
        let config = VaultConfig::default();
        let parsed = parse_document(
            concat!(
                "month:: 2026-04\n",
                "duration:: 1d 3h\n",
                "flag:: true\n",
                "estimate:: -7\n",
                "choices:: \"alpha\", \"beta\"\n",
                "plain:: alpha, beta\n",
            ),
            &config,
        );
        let indexed = extract_indexed_properties(&parsed, &config)
            .expect("property extraction should succeed")
            .expect("inline fields should produce properties");

        let month = indexed
            .values
            .iter()
            .find(|value| value.key == "month")
            .expect("month property should exist");
        assert_eq!(month.value_type, "date");
        assert_eq!(month.value_date.as_deref(), Some("2026-04"));

        let duration = indexed
            .values
            .iter()
            .find(|value| value.key == "duration")
            .expect("duration property should exist");
        assert_eq!(duration.value_type, "duration");
        assert_eq!(duration.value_text.as_deref(), Some("1d 3h"));

        let flag = indexed
            .values
            .iter()
            .find(|value| value.key == "flag")
            .expect("flag property should exist");
        assert_eq!(flag.value_type, "boolean");
        assert_eq!(flag.value_bool, Some(true));

        let estimate = indexed
            .values
            .iter()
            .find(|value| value.key == "estimate")
            .expect("estimate property should exist");
        assert_eq!(estimate.value_type, "number");
        assert_eq!(estimate.value_number, Some(-7.0));

        let choices = indexed
            .values
            .iter()
            .find(|value| value.key == "choices")
            .expect("choices property should exist");
        assert_eq!(choices.value_type, "list");
        assert_eq!(
            indexed
                .list_items
                .iter()
                .filter(|item| item.key == "choices")
                .map(|item| item.value_text.clone())
                .collect::<Vec<_>>(),
            vec!["alpha".to_string(), "beta".to_string()]
        );

        let plain = indexed
            .values
            .iter()
            .find(|value| value.key == "plain")
            .expect("plain property should exist");
        assert_eq!(plain.value_type, "text");
        assert_eq!(plain.value_text.as_deref(), Some("alpha, beta"));

        let canonical_json = serde_json::from_str::<Value>(&indexed.canonical_json)
            .expect("canonical json should deserialize");
        assert_eq!(
            canonical_json["month"],
            Value::String("2026-04".to_string())
        );
        assert_eq!(
            canonical_json["duration"],
            Value::String("1d 3h".to_string())
        );
        assert_eq!(canonical_json["flag"], Value::Bool(true));
        assert_eq!(
            canonical_json["estimate"],
            Value::Number(serde_json::Number::from_f64(-7.0).expect("finite"))
        );
        assert_eq!(
            canonical_json["choices"],
            Value::Array(vec![
                Value::String("alpha".to_string()),
                Value::String("beta".to_string()),
            ])
        );
        assert_eq!(
            canonical_json["plain"],
            Value::String("alpha, beta".to_string())
        );
    }

    #[test]
    fn duplicate_frontmatter_and_inline_keys_merge_into_lists() {
        let config = VaultConfig::default();
        let parsed = parse_document(
            concat!(
                "---\n",
                "status: draft\n",
                "reviewed: true\n",
                "---\n",
                "status:: done\n",
                "reviewed:: false\n",
            ),
            &config,
        );
        let indexed = extract_indexed_properties(&parsed, &config)
            .expect("property extraction should succeed")
            .expect("mixed properties should produce values");
        let canonical_json = serde_json::from_str::<Value>(&indexed.canonical_json)
            .expect("canonical json should deserialize");

        assert_eq!(
            canonical_json["status"],
            Value::Array(vec![
                Value::String("draft".to_string()),
                Value::String("done".to_string()),
            ])
        );
        assert_eq!(
            canonical_json["reviewed"],
            Value::Array(vec![Value::Bool(true), Value::Bool(false)])
        );
    }

    #[test]
    fn query_notes_filters_and_sorts_using_property_tables() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("mixed-properties", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let done = query_notes(
            &paths,
            &NoteQuery {
                filters: vec!["status = done".to_string()],
                sort_by: None,
                sort_descending: false,
            },
        )
        .expect("property query should succeed");
        assert_eq!(
            done.notes
                .iter()
                .map(|note| note.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Done.md".to_string()]
        );

        let sprint = query_notes(
            &paths,
            &NoteQuery {
                filters: vec!["related contains sprint".to_string()],
                sort_by: Some("due".to_string()),
                sort_descending: false,
            },
        )
        .expect("list query should succeed");
        assert_eq!(
            sprint
                .notes
                .iter()
                .map(|note| note.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Done.md".to_string()]
        );

        let sorted = query_notes(
            &paths,
            &NoteQuery {
                filters: vec!["estimate > 2".to_string()],
                sort_by: Some("due".to_string()),
                sort_descending: false,
            },
        )
        .expect("sorted property query should succeed");
        assert_eq!(
            sorted
                .notes
                .iter()
                .map(|note| note.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Done.md".to_string(), "Backlog.md".to_string()]
        );

        let prefixed = query_notes(
            &paths,
            &NoteQuery {
                filters: vec!["file.path starts_with \"Back\"".to_string()],
                sort_by: None,
                sort_descending: false,
            },
        )
        .expect("prefix property query should succeed");
        assert_eq!(
            prefixed
                .notes
                .iter()
                .map(|note| note.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Backlog.md".to_string()]
        );
    }

    #[test]
    fn query_notes_supports_file_tag_filters_with_subtags() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
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
        fs::write(vault_root.join("Projects/Gamma.md"), "Tag #other\n")
            .expect("gamma note should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let report = query_notes(
            &paths,
            &NoteQuery {
                filters: vec!["file.tags has_tag #project".to_string()],
                sort_by: Some("file.path".to_string()),
                sort_descending: false,
            },
        )
        .expect("tag filter query should succeed");

        assert_eq!(
            report
                .notes
                .iter()
                .map(|note| note.document_path.as_str())
                .collect::<Vec<_>>(),
            vec!["Projects/Alpha.md", "Projects/Beta.md"]
        );
    }

    #[test]
    fn query_notes_expression_filters_support_regex_functions() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let release = query_notes(
            &paths,
            &NoteQuery {
                filters: vec![r#"regextest("release", file.tasks.text)"#.to_string()],
                sort_by: Some("file.path".to_string()),
                sort_descending: false,
            },
        )
        .expect("regex test filter should succeed");
        assert_eq!(
            release
                .notes
                .iter()
                .map(|note| note.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Dashboard.md".to_string()]
        );

        let exact = query_notes(
            &paths,
            &NoteQuery {
                filters: vec![r#"regexmatch("draft", status)"#.to_string()],
                sort_by: Some("file.path".to_string()),
                sort_descending: false,
            },
        )
        .expect("regex match filter should succeed");
        assert_eq!(
            exact
                .notes
                .iter()
                .map(|note| note.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Dashboard.md".to_string()]
        );

        let capture = query_notes(
            &paths,
            &NoteQuery {
                filters: vec![
                    r#"regexreplace(owner, "\[\[(.+)\]\]", "$1") == "People/Bob""#.to_string(),
                ],
                sort_by: Some("file.path".to_string()),
                sort_descending: false,
            },
        )
        .expect("regex replace filter should succeed");
        assert_eq!(
            capture
                .notes
                .iter()
                .map(|note| note.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Dashboard.md".to_string()]
        );

        let combined = query_notes(
            &paths,
            &NoteQuery {
                filters: vec![
                    "reviewed = true".to_string(),
                    r#"regextest("^(Dashboard|Alpha)$", file.name)"#.to_string(),
                ],
                sort_by: Some("file.path".to_string()),
                sort_descending: false,
            },
        )
        .expect("combined sql and regex filters should succeed");
        assert_eq!(
            combined
                .notes
                .iter()
                .map(|note| note.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Projects/Alpha.md".to_string()]
        );
    }

    #[test]
    fn note_pages_equal_slices_of_the_full_ordered_result() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).unwrap();
        for index in 0..12 {
            fs::write(
                vault_root.join(format!("N{index:02}.md")),
                format!(
                    "---\nstatus: {}\nrank: {}\ntags: [t{}]\n---\n[[N{:02}]]\n- [ ] task\nx:: `= this.rank`\n",
                    if index % 3 == 0 { "done" } else { "open" },
                    (index * 5) % 7,
                    index % 2,
                    (index + 1) % 12,
                ),
            )
            .unwrap();
        }
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let store = crate::note_store::DirectNoteStore::new(&paths);
        for (filters, sort_by, descending) in [
            (vec!["status = open"], Some("rank"), false),
            (vec!["status = open"], None, false),
            (vec!["rank > 1"], Some("rank"), true),
            // Filters that read hydrated fields hydrate every row first.
            (vec!["file.tags has_tag t1"], Some("file.name"), true),
            (vec!["length(file.outlinks) > 0"], Some("rank"), false),
        ] {
            let query = NoteQuery {
                filters: filters.iter().map(ToString::to_string).collect(),
                sort_by: sort_by.map(ToString::to_string),
                sort_descending: descending,
            };
            let full = query_notes_in(&store, &paths, &query, None).unwrap().notes;
            for (offset, limit) in [(0, Some(3)), (2, Some(4)), (5, None), (20, Some(2))] {
                let page = query_notes_page_in(
                    &store,
                    &paths,
                    &query,
                    None,
                    Some(NotePage { offset, limit }),
                )
                .unwrap()
                .notes;
                let expected = full
                    .iter()
                    .skip(offset)
                    .take(limit.unwrap_or(usize::MAX))
                    .cloned()
                    .collect::<Vec<_>>();
                assert_eq!(page, expected, "{filters:?} {offset} {limit:?}");
            }
        }
    }

    #[test]
    fn note_filters_mean_their_expressions() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        fs::create_dir_all(vault_root.join("Folder%")).expect("folder should be created");
        for (path, contents) in [
            ("None.md", "# None\n"),
            ("Done.md", "---\nstatus: done\ntags: [a/b]\n---\n"),
            ("Open.md", "---\nstatus: open\nrank: 2\n---\n"),
            ("Upper.md", "---\nstatus: Open\nrank: null\n---\n"),
            ("List.md", "---\nstatus: [open, done]\nrank: 10\n---\n"),
            ("Folder%/Inner.md", "---\nstatus: openly\n---\n#a\n"),
        ] {
            fs::write(vault_root.join(path), contents).expect("note should be written");
        }
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let run = |filter: &str| {
            query_notes(
                &paths,
                &NoteQuery {
                    filters: vec![filter.to_string()],
                    sort_by: None,
                    sort_descending: false,
                },
            )
            .unwrap_or_else(|error| panic!("{filter}: {error}"))
            .notes
            .into_iter()
            .map(|note| note.document_path)
            .collect::<Vec<_>>()
        };
        for (filter, expected) in [
            // A missing property is null.
            (
                "status != done",
                vec![
                    "Folder%/Inner.md",
                    "List.md",
                    "None.md",
                    "Open.md",
                    "Upper.md",
                ],
            ),
            ("status = open", vec!["Open.md"]),
            ("rank > 1", vec!["List.md", "Open.md"]),
            (
                "rank = null",
                vec!["Done.md", "Folder%/Inner.md", "None.md", "Upper.md"],
            ),
            ("rank != null", vec!["List.md", "Open.md"]),
            // Prefixes are byte-exact and case-sensitive, with no wildcards.
            (
                "status starts_with open",
                vec!["Folder%/Inner.md", "List.md", "Open.md"],
            ),
            ("file.path starts_with Folder%/", vec!["Folder%/Inner.md"]),
            (
                "file.path starts_with \"Folder%/\"",
                vec!["Folder%/Inner.md"],
            ),
            ("file.path starts_with folder%/", vec![]),
            ("file.path starts_with Folder_/", vec![]),
            ("file.path starts_with Fold", vec!["Folder%/Inner.md"]),
            ("file.path starts_with folder", vec![]),
            ("file.path starts_with F_lder", vec![]),
            (
                "status contains open",
                vec!["Folder%/Inner.md", "List.md", "Open.md"],
            ),
            ("status matches ^open$", vec!["List.md", "Open.md"]),
            (
                "status matches_i ^open$",
                vec!["List.md", "Open.md", "Upper.md"],
            ),
            // Tags are a source: the tag or one nested under it.
            ("file.tags has_tag a", vec!["Done.md", "Folder%/Inner.md"]),
            ("file.tags contains #a/b", vec!["Done.md"]),
            // A value is one quoted literal; anything else is an expression.
            ("status = \"done\" || rank = 2", vec!["Done.md", "Open.md"]),
        ] {
            let mut actual = run(filter);
            actual.sort();
            assert_eq!(actual, expected, "{filter}");
            // The lowered and SQL-narrowed form equals full evaluation.
            let source = note_filter_expression_source(filter).expect("filter compiles");
            let mut evaluated = run(&format!("({source}) && length(\"x\") > 0"));
            evaluated.sort();
            assert_eq!(evaluated, expected, "{filter} as {source}");
        }
        assert!(matches!(
            query_notes(
                &paths,
                &NoteQuery {
                    filters: vec!["status has_tag open".to_string()],
                    sort_by: None,
                    sort_descending: false,
                },
            ),
            Err(PropertyError::InvalidFilter(_))
        ));
    }

    #[test]
    fn query_notes_sorts_missing_properties_like_nulls() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        fs::create_dir_all(&vault_root).expect("vault directory should be created");
        fs::write(vault_root.join("A.md"), "# A\n").expect("note should be written");
        fs::write(vault_root.join("B.md"), "---\nrank: null\n---\n# B\n")
            .expect("note should be written");
        fs::write(vault_root.join("C.md"), "---\nrank: 1\n---\n# C\n")
            .expect("note should be written");
        fs::write(vault_root.join("D.md"), "---\nrank: 2\n---\n# D\n")
            .expect("note should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let ascending = query_notes(
            &paths,
            &NoteQuery {
                filters: Vec::new(),
                sort_by: Some("rank".to_string()),
                sort_descending: false,
            },
        )
        .expect("ascending sort query should succeed");
        assert_eq!(
            ascending
                .notes
                .iter()
                .map(|note| note.document_path.clone())
                .collect::<Vec<_>>(),
            vec![
                "A.md".to_string(),
                "B.md".to_string(),
                "C.md".to_string(),
                "D.md".to_string(),
            ]
        );

        let descending = query_notes(
            &paths,
            &NoteQuery {
                filters: Vec::new(),
                sort_by: Some("rank".to_string()),
                sort_descending: true,
            },
        )
        .expect("descending sort query should succeed");
        assert_eq!(
            descending
                .notes
                .iter()
                .map(|note| note.document_path.clone())
                .collect::<Vec<_>>(),
            vec![
                "D.md".to_string(),
                "C.md".to_string(),
                "A.md".to_string(),
                "B.md".to_string(),
            ]
        );
    }

    #[test]
    fn bookmarked_paths_include_nested_file_items_only() {
        let bookmarks = serde_json::json!({
            "items": [
                {
                    "type": "group",
                    "items": [
                        {"type": "file", "path": "Inbox.md"},
                        {"type": "markdown", "path": "Projects/Alpha.md"},
                        {"type": "search", "query": "tag:#project"},
                        {"type": "folder", "path": "Projects"}
                    ]
                },
                {"type": "canvas", "path": "Boards/Roadmap.canvas"}
            ]
        });

        assert_eq!(
            bookmarked_paths_from_value(&bookmarks),
            std::collections::HashSet::from_iter([
                "Inbox.md".to_string(),
                "Projects/Alpha.md".to_string(),
                "Boards/Roadmap.canvas".to_string(),
            ])
        );
    }

    #[test]
    fn query_notes_exposes_bookmarked_notes_via_file_starred() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        fs::create_dir_all(vault_root.join(".obsidian"))
            .expect("obsidian directory should be created");
        fs::create_dir_all(vault_root.join("Projects"))
            .expect("project directory should be created");
        fs::write(vault_root.join("Inbox.md"), "# Inbox\n").expect("note should be written");
        fs::write(vault_root.join("Projects/Alpha.md"), "# Alpha\n")
            .expect("note should be written");
        fs::write(vault_root.join("Projects/Beta.md"), "# Beta\n").expect("note should be written");
        fs::write(
            vault_root.join(".obsidian/bookmarks.json"),
            serde_json::json!({
                "items": [
                    {
                        "type": "group",
                        "items": [
                            {"type": "file", "path": "Inbox.md"},
                            {"type": "file", "path": "Projects/Alpha.md", "subpath": "#Overview"}
                        ]
                    },
                    {"type": "search", "query": "tag:#project"}
                ]
            })
            .to_string(),
        )
        .expect("bookmarks should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let note_index = load_note_index(&paths).expect("note index should load");
        assert_eq!(
            FileMetadataResolver::field(note_index.get("Inbox").expect("Inbox note"), "starred"),
            Value::Bool(true)
        );
        assert_eq!(
            FileMetadataResolver::field(note_index.get("Alpha").expect("Alpha note"), "starred"),
            Value::Bool(true)
        );
        assert_eq!(
            FileMetadataResolver::field(note_index.get("Beta").expect("Beta note"), "starred"),
            Value::Bool(false)
        );

        let starred = query_notes(
            &paths,
            &NoteQuery {
                filters: vec!["file.starred == true".to_string()],
                sort_by: Some("file.path".to_string()),
                sort_descending: false,
            },
        )
        .expect("file.starred query should succeed");
        assert_eq!(
            starred
                .notes
                .iter()
                .map(|note| note.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Inbox.md".to_string(), "Projects/Alpha.md".to_string()]
        );
    }

    #[test]
    fn inline_fields_merge_into_properties_and_filters_see_the_merged_value() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let query_paths = |filters: &[&str], sort_by: Option<&str>| {
            query_notes(
                &paths,
                &NoteQuery {
                    filters: filters.iter().map(|filter| (*filter).to_string()).collect(),
                    sort_by: sort_by.map(str::to_string),
                    sort_descending: false,
                },
            )
            .expect("note query should succeed")
            .notes
            .into_iter()
            .map(|note| note.document_path)
            .collect::<Vec<_>>()
        };
        assert_eq!(
            query_paths(&["status = draft"], None),
            vec!["Dashboard.md".to_string()]
        );
        assert!(query_paths(&["status = done"], None).is_empty());
        // `priority` is `[2, 3]`: filters compare the merged value, which
        // contains 2 but does not equal it.
        assert!(query_paths(&["priority = 2"], None).is_empty());
        assert_eq!(
            query_paths(&["priority contains 2"], None),
            vec!["Dashboard.md".to_string()]
        );
        assert_eq!(
            query_paths(&["month = 2026-04"], None),
            vec!["Dashboard.md".to_string()]
        );
        // Dashboard's `reviewed: true` and `reviewed:: false` make a list.
        assert_eq!(
            query_paths(&["reviewed = true"], None),
            vec!["Projects/Alpha.md".to_string()]
        );

        let all_notes = query_notes(
            &paths,
            &NoteQuery {
                filters: Vec::new(),
                sort_by: Some("file.path".to_string()),
                sort_descending: false,
            },
        )
        .expect("full note query should succeed");
        let dashboard = all_notes
            .notes
            .iter()
            .find(|note| note.document_path == "Dashboard.md")
            .expect("dashboard note should be present");

        assert_eq!(
            dashboard.properties["status"],
            Value::String("draft".to_string())
        );
        assert_eq!(
            dashboard.properties["priority"],
            Value::Array(vec![
                Value::Number(serde_json::Number::from_f64(2.0).expect("finite")),
                Value::Number(serde_json::Number::from_f64(3.0).expect("finite")),
            ])
        );
        assert_eq!(
            dashboard.properties["owner"],
            Value::String("[[People/Bob]]".to_string())
        );
        assert_eq!(
            dashboard.properties["reviewed"],
            Value::Array(vec![Value::Bool(true), Value::Bool(false)])
        );
        assert_eq!(dashboard.inline_expressions.len(), 1);
        assert_eq!(
            dashboard.inline_expressions[0].expression,
            "this.status".to_string()
        );
        assert_eq!(
            dashboard.inline_expressions[0].value,
            Value::String("draft".to_string())
        );
        assert_eq!(dashboard.inline_expressions[0].error, None);
    }

    #[test]
    fn query_notes_expression_filters_support_date_and_duration_rhs_functions() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        fs::create_dir_all(&vault_root).expect("vault directory should be created");
        fs::write(
            vault_root.join("Long.md"),
            "due:: 2020-01-01\nduration:: 1d 3h\n",
        )
        .expect("long note should be written");
        fs::write(
            vault_root.join("Short.md"),
            "due:: 2099-01-01\nduration:: 2h\n",
        )
        .expect("short note should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let overdue = query_notes(
            &paths,
            &NoteQuery {
                filters: vec!["due < date(2099-01-01)".to_string()],
                sort_by: Some("file.path".to_string()),
                sort_descending: false,
            },
        )
        .expect("date function filter should succeed");
        assert_eq!(
            overdue
                .notes
                .iter()
                .map(|note| note.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Long.md".to_string()]
        );

        let long_duration = query_notes(
            &paths,
            &NoteQuery {
                filters: vec!["duration > dur(1d)".to_string()],
                sort_by: Some("file.path".to_string()),
                sort_descending: false,
            },
        )
        .expect("duration function filter should succeed");
        assert_eq!(
            long_duration
                .notes
                .iter()
                .map(|note| note.document_path.clone())
                .collect::<Vec<_>>(),
            vec!["Long.md".to_string()]
        );
    }

    #[test]
    fn load_note_index_applies_custom_task_status_config() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join("Tasks.md"),
            "- [!] Important todo\n- [v] Custom done\n",
        )
        .expect("note should be written");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[tasks.statuses]
todo = [" ", "!"]
completed = ["x", "v"]
"#,
        )
        .expect("config should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let note_index = load_note_index(&paths).expect("note index should load");
        let note = note_index.get("Tasks").expect("task note should exist");
        let tasks = FileMetadataResolver::field(note, "tasks");
        let tasks = tasks.as_array().expect("tasks should be an array");

        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["status"], Value::String("!".to_string()));
        assert_eq!(tasks[0]["statusName"], Value::String("Todo".to_string()));
        assert_eq!(tasks[0]["statusType"], Value::String("TODO".to_string()));
        assert_eq!(tasks[0]["checked"], Value::Bool(false));
        assert_eq!(tasks[0]["completed"], Value::Bool(false));
        assert_eq!(tasks[1]["status"], Value::String("v".to_string()));
        assert_eq!(tasks[1]["statusName"], Value::String("Done".to_string()));
        assert_eq!(tasks[1]["statusType"], Value::String("DONE".to_string()));
        assert_eq!(tasks[1]["checked"], Value::Bool(true));
        assert_eq!(tasks[1]["completed"], Value::Bool(true));
        assert_eq!(tasks[1]["fullyCompleted"], Value::Bool(true));
    }

    #[test]
    fn load_note_index_imports_tasks_plugin_status_definitions() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        fs::create_dir_all(vault_root.join(".obsidian/plugins/obsidian-tasks-plugin"))
            .expect("tasks plugin dir should be created");
        fs::write(
            vault_root.join("Tasks.md"),
            "- [>] Waiting review\n- [~] Parked for later\n",
        )
        .expect("note should be written");
        fs::write(
            vault_root.join(".obsidian/plugins/obsidian-tasks-plugin/data.json"),
            r#"{
              "statusSettings": {
                "coreStatuses": [
                  { "symbol": " ", "name": "Todo", "type": "TODO", "nextStatusSymbol": ">" },
                  { "symbol": "x", "name": "Done", "type": "DONE", "nextStatusSymbol": " " }
                ],
                "customStatuses": [
                  { "symbol": ">", "name": "Waiting", "type": "IN_PROGRESS", "nextStatusSymbol": "x" },
                  { "symbol": "~", "name": "Parked", "type": "NON_TASK", "nextStatusSymbol": null }
                ]
              }
            }"#,
        )
        .expect("tasks plugin config should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let note_index = load_note_index(&paths).expect("note index should load");
        let note = note_index.get("Tasks").expect("task note should exist");
        let tasks = FileMetadataResolver::field(note, "tasks");
        let tasks = tasks.as_array().expect("tasks should be an array");

        assert_eq!(tasks[0]["status"], Value::String(">".to_string()));
        assert_eq!(tasks[0]["statusName"], Value::String("Waiting".to_string()));
        assert_eq!(
            tasks[0]["statusType"],
            Value::String("IN_PROGRESS".to_string())
        );
        assert_eq!(tasks[0]["statusNext"], Value::String("x".to_string()));
        assert_eq!(tasks[1]["status"], Value::String("~".to_string()));
        assert_eq!(tasks[1]["statusName"], Value::String("Parked".to_string()));
        assert_eq!(
            tasks[1]["statusType"],
            Value::String("NON_TASK".to_string())
        );
        assert_eq!(tasks[1]["checked"], Value::Bool(true));
        assert_eq!(tasks[1]["completed"], Value::Bool(false));
    }

    #[test]
    fn load_note_index_persists_derived_task_recurrence_properties() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should be created");
        fs::write(
            vault_root.join("Tasks.md"),
            "- [ ] Review sprint ⏳ 2026-03-27 🔁 every weekday\n",
        )
        .expect("note should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let note_index = load_note_index(&paths).expect("note index should load");
        let note = note_index.get("Tasks").expect("task note should exist");
        let task = note.tasks.first().expect("task should exist");

        assert_eq!(
            task.properties.get("recurrence"),
            Some(&Value::String("every weekday".to_string()))
        );
        assert_eq!(
            task.properties.get("recurrenceRaw"),
            Some(&Value::String("every weekday".to_string()))
        );
        assert_eq!(
            task.properties.get("recurrenceRule"),
            Some(&Value::String(
                "FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,TU,WE,TH,FR".to_string()
            ))
        );
        assert_eq!(
            task.properties.get("recurrenceFrequency"),
            Some(&Value::String("weekly".to_string()))
        );
        assert_eq!(
            task.properties.get("recurrenceInterval"),
            Some(&serde_json::json!(1.0))
        );
        assert_eq!(
            task.properties.get("recurrenceWeekdays"),
            Some(&serde_json::json!([
                "monday",
                "tuesday",
                "wednesday",
                "thursday",
                "friday"
            ]))
        );
        assert_eq!(
            task.properties.get("recurrenceAnchor"),
            Some(&Value::String("2026-03-27".to_string()))
        );
    }

    #[test]
    fn load_note_index_exposes_tasknotes_as_synthetic_file_tasks() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("tasknotes", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let note_index = load_note_index(&paths).expect("note index should load");
        let note = note_index
            .get("Write Docs")
            .expect("tasknote should be indexed");
        let tasks = FileMetadataResolver::field(note, "tasks");
        let tasks = tasks.as_array().expect("tasks should be an array");

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["text"], Value::String("Write docs".to_string()));
        assert_eq!(
            tasks[0]["id"],
            Value::String("[[TaskNotes/Tasks/Write Docs]]".to_string())
        );
        assert_eq!(tasks[0]["status"], Value::String("in-progress".to_string()));
        assert_eq!(
            tasks[0]["statusType"],
            Value::String("IN_PROGRESS".to_string())
        );
        assert_eq!(tasks[0]["taskSource"], Value::String("file".to_string()));
        assert_eq!(
            tasks[0]["priorityWeight"],
            Value::Number(serde_json::Number::from_f64(3.0).expect("number"))
        );
        assert_eq!(
            tasks[0]["blocked-by"],
            serde_json::json!(["[[TaskNotes/Tasks/Prep Outline]]"])
        );
        assert_eq!(
            tasks[0]["recurrenceAnchor"],
            Value::String("2026-04-04".to_string())
        );
        assert_eq!(
            tasks[0]["completion"],
            Value::String("2026-04-04".to_string())
        );
        assert_eq!(
            tasks[0]["path"],
            Value::String("TaskNotes/Tasks/Write Docs.md".to_string())
        );
    }

    #[test]
    fn list_properties_reports_counts_and_observed_types() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("mixed-properties", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let properties = list_properties(&paths).expect("property listing should succeed");

        assert_eq!(
            properties,
            vec![
                PropertyCatalogEntry {
                    key: "due".to_string(),
                    count: 3,
                    types: vec!["date".to_string(), "text".to_string()],
                },
                PropertyCatalogEntry {
                    key: "empty_list".to_string(),
                    count: 1,
                    types: vec!["list".to_string()],
                },
                PropertyCatalogEntry {
                    key: "empty_text".to_string(),
                    count: 1,
                    types: vec!["text".to_string()],
                },
                PropertyCatalogEntry {
                    key: "estimate".to_string(),
                    count: 3,
                    types: vec!["number".to_string(), "text".to_string()],
                },
                PropertyCatalogEntry {
                    key: "notes".to_string(),
                    count: 1,
                    types: vec!["null".to_string()],
                },
                PropertyCatalogEntry {
                    key: "related".to_string(),
                    count: 3,
                    types: vec!["link".to_string(), "list".to_string()],
                },
                PropertyCatalogEntry {
                    key: "reviewed".to_string(),
                    count: 3,
                    types: vec!["boolean".to_string(), "text".to_string()],
                },
                PropertyCatalogEntry {
                    key: "status".to_string(),
                    count: 3,
                    types: vec!["list".to_string(), "text".to_string()],
                },
            ]
        );
    }

    #[test]
    fn list_query_fields_reports_builtin_supports_and_property_examples() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("mixed-properties", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let fields = list_query_fields(&paths).expect("query field listing should succeed");

        let path_field = fields
            .iter()
            .find(|entry| entry.field == "file.path")
            .expect("file.path should be present");
        assert_eq!(path_field.kind, "builtin");
        assert_eq!(path_field.supports, vec!["where", "sort", "fields"]);
        assert_eq!(path_field.types, vec!["text"]);
        assert_eq!(path_field.example, Value::String("Backlog.md".to_string()));

        let status_field = fields
            .iter()
            .find(|entry| entry.field == "status")
            .expect("status should be present");
        assert_eq!(status_field.kind, "property");
        assert_eq!(status_field.supports, vec!["where", "sort", "fields"]);
        assert_eq!(
            status_field.types,
            vec!["list".to_string(), "text".to_string()]
        );
        assert_eq!(status_field.example, Value::String("backlog".to_string()));
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);

        copy_dir_recursive(&source, destination);
        fs::create_dir_all(destination.join(".vulcan")).expect(".vulcan dir should be created");
    }

    fn copy_dir_recursive(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("destination directory should be created");

        for entry in fs::read_dir(source).expect("source directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            let file_type = entry.file_type().expect("file type should be readable");
            let target = destination.join(entry.file_name());

            if file_type.is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else if file_type.is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).expect("parent directory should exist");
                }
                fs::copy(entry.path(), target).expect("file should be copied");
            }
        }
    }
}
