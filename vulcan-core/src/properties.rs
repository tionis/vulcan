use crate::parser::parse_document;
use crate::{CacheDatabase, CacheError, VaultConfig, VaultPaths};
use rusqlite::params_from_iter;
use rusqlite::types::Value as SqlValue;
use serde::Serialize;
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fmt::{Display, Formatter};

const PROPERTY_NAMESPACE_FRONTMATTER: &str = "frontmatter";

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

#[derive(Debug)]
pub enum PropertyError {
    Cache(CacheError),
    CacheMissing,
    InvalidFilter(String),
    Json(serde_json::Error),
    Sqlite(rusqlite::Error),
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
        }
    }
}

impl Error for PropertyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cache(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Sqlite(error) => Some(error),
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
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoteRecord {
    pub document_path: String,
    pub file_name: String,
    pub file_ext: String,
    pub file_mtime: i64,
    pub file_size: i64,
    pub properties: Value,
    pub tags: Vec<String>,
    pub links: Vec<String>,
}

pub fn extract_indexed_properties(
    raw_frontmatter: Option<&str>,
    frontmatter: Option<&serde_yaml::Value>,
    config: &VaultConfig,
) -> Result<Option<IndexedProperties>, serde_json::Error> {
    let Some(frontmatter) = frontmatter else {
        return Ok(None);
    };

    let raw_yaml = raw_frontmatter.unwrap_or_default().to_string();
    let canonical_json = serde_json::to_string(&yaml_to_json(frontmatter))?;
    let mut values = Vec::new();
    let mut list_items = Vec::new();
    let mut diagnostics = Vec::new();

    if let Some(mapping) = frontmatter.as_mapping() {
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

    Ok(Some(IndexedProperties {
        raw_yaml,
        canonical_json,
        values,
        list_items,
        diagnostics,
    }))
}

#[allow(clippy::too_many_lines)]
pub fn query_notes(paths: &VaultPaths, query: &NoteQuery) -> Result<NotesReport, PropertyError> {
    let database = open_existing_cache(paths)?;
    let connection = database.connection();

    let NoteFilterSql {
        cte,
        clause: filter_clause,
        params,
    } = build_note_filter_clause(&query.filters)?;

    let mut sql = cte;
    sql.push_str(
        "SELECT
            documents.id,
            documents.path,
            documents.filename,
            documents.extension,
            documents.file_mtime,
            documents.file_size,
            COALESCE(properties.canonical_json, '{}')
        FROM documents
        LEFT JOIN properties ON properties.document_id = documents.id
        WHERE documents.extension = 'md'",
    );
    sql.push_str(&filter_clause);
    sql.push_str(" ORDER BY documents.path ASC");

    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(
        params_from_iter(params.iter()),
        |row| -> Result<(String, NoteRecord), rusqlite::Error> {
            let doc_id: String = row.get(0)?;
            let canonical_json: String = row.get(6)?;
            let properties = serde_json::from_str::<Value>(&canonical_json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    6,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok((
                doc_id,
                NoteRecord {
                    document_path: row.get(1)?,
                    file_name: row.get(2)?,
                    file_ext: row.get(3)?,
                    file_mtime: row.get(4)?,
                    file_size: row.get(5)?,
                    properties,
                    tags: Vec::new(),
                    links: Vec::new(),
                },
            ))
        },
    )?;
    let mut doc_ids_and_notes: Vec<(String, NoteRecord)> = rows.collect::<Result<Vec<_>, _>>()?;

    // Batch-load tags and links for only the matching documents
    if !doc_ids_and_notes.is_empty() {
        let doc_ids: Vec<&str> = doc_ids_and_notes
            .iter()
            .map(|(id, _)| id.as_str())
            .collect();
        let placeholders = doc_ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");

        let mut tag_map: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
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

        let mut link_map: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let link_sql = format!(
            "SELECT source_document_id, raw_text FROM links WHERE link_kind = 'wikilink' AND source_document_id IN ({placeholders})"
        );
        let mut link_stmt = connection.prepare(&link_sql)?;
        let link_rows = link_stmt.query_map(params_from_iter(doc_ids.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for link_row in link_rows {
            let (doc_id, raw_text) = link_row?;
            link_map.entry(doc_id).or_default().push(raw_text);
        }

        for (doc_id, note) in &mut doc_ids_and_notes {
            if let Some(tags) = tag_map.remove(doc_id.as_str()) {
                note.tags = tags;
            }
            if let Some(links) = link_map.remove(doc_id.as_str()) {
                note.links = links;
            }
        }
    }

    let mut notes: Vec<NoteRecord> = doc_ids_and_notes
        .into_iter()
        .map(|(_, note)| note)
        .collect();

    if let Some(sort_by) = query.sort_by.as_deref() {
        notes.sort_by(|left, right| {
            let ordering = compare_sort_keys(
                &sort_key_for_note(left, sort_by),
                &sort_key_for_note(right, sort_by),
            );
            let ordering = if query.sort_descending {
                ordering.reverse()
            } else {
                ordering
            };
            ordering.then_with(|| left.document_path.cmp(&right.document_path))
        });
    }

    Ok(NotesReport {
        filters: query.filters.clone(),
        sort_by: query.sort_by.clone(),
        sort_descending: query.sort_descending,
        notes,
    })
}

/// Load a lightweight index of all notes keyed by `file_name` (basename without extension).
/// Loads only core document fields and frontmatter properties; tags and links are left empty
/// since they require expensive batch queries. Notes in the current view should be overlaid
/// on top of this index so their tags/links are available.
pub fn load_note_index(paths: &VaultPaths) -> Result<HashMap<String, NoteRecord>, PropertyError> {
    let database = open_existing_cache(paths)?;
    let connection = database.connection();
    let mut stmt = connection.prepare(
        "SELECT d.path, d.filename, d.extension, d.file_mtime, d.file_size, \
         COALESCE(p.canonical_json, '{}') \
         FROM documents d LEFT JOIN properties p ON p.document_id = d.id",
    )?;
    let rows = stmt.query_map([], |row| {
        let props_json: String = row.get(5)?;
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            props_json,
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (path, file_name, file_ext, file_mtime, file_size, props_json) = row?;
        let properties =
            serde_json::from_str(&props_json).unwrap_or(Value::Object(serde_json::Map::default()));
        map.insert(
            file_name.clone(),
            NoteRecord {
                document_path: path,
                file_name,
                file_ext,
                file_mtime,
                file_size,
                properties,
                tags: vec![],
                links: vec![],
            },
        );
    }
    Ok(map)
}

/// The result of building a filter clause.
/// `cte` is an optional `WITH ...` prefix to prepend before `SELECT`.
/// `clause` is an `AND ...` fragment appended to the WHERE clause.
pub(crate) struct NoteFilterSql {
    pub cte: String,
    pub clause: String,
    pub params: Vec<SqlValue>,
}

pub(crate) fn build_note_filter_clause(filters: &[String]) -> Result<NoteFilterSql, PropertyError> {
    let parsed = filters
        .iter()
        .map(|filter| parse_filter_expression(filter).map(FilterExpression::Condition))
        .collect::<Result<Vec<_>, _>>()?;

    build_note_filter_clause_from_expressions(&parsed)
}

pub(crate) fn build_note_filter_clause_from_expressions(
    filters: &[FilterExpression],
) -> Result<NoteFilterSql, PropertyError> {
    // Separate has_tag filters (grouped by property key) from all other filters.
    // Multiple has_tag filters on the same key are combined via INTERSECT on
    // property_list_items — much faster than correlated EXISTS for large result sets.
    let mut has_tag_by_key: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut other_filters: Vec<&FilterExpression> = Vec::new();

    for filter in filters {
        if let FilterExpression::Condition(ParsedFilter {
            field: FilterField::Property(key),
            operator: FilterOperator::HasTag,
            value: FilterValue::Text(value),
        }) = filter
        {
            has_tag_by_key
                .entry(key.clone())
                .or_default()
                .push(value.clone());
        } else {
            other_filters.push(filter);
        }
    }

    let mut cte = String::new();
    let mut clause = String::new();
    let mut params = Vec::<SqlValue>::new();

    // Generate CTEs + IN(INTERSECT) for has_tag groups
    if !has_tag_by_key.is_empty() {
        let mut cte_names: Vec<String> = Vec::new();
        let mut cte_index = 0usize;

        cte.push_str("WITH ");
        for (key_index, (key, tags)) in has_tag_by_key.iter().enumerate() {
            for tag in tags {
                let cte_name = format!("_hts{cte_index}");
                cte_names.push(cte_name.clone());
                if cte_index > 0 {
                    cte.push_str(", ");
                }
                cte.push_str(&cte_name);
                cte.push_str(" AS (SELECT document_id FROM property_list_items WHERE key = ? AND value_text = ?");
                params.push(SqlValue::Text(key.clone()));
                params.push(SqlValue::Text(tag.clone()));
                // Also capture nested subtags via range (e.g. "Femdom/" .. "Femdom0")
                cte.push_str(" UNION ALL SELECT document_id FROM property_list_items WHERE key = ? AND value_text >= ? AND value_text < ?)");
                params.push(SqlValue::Text(key.clone()));
                params.push(SqlValue::Text(format!("{tag}/")));
                params.push(SqlValue::Text(format!("{tag}0")));
                cte_index += 1;
            }
            let _ = key_index; // suppress unused warning
        }
        cte.push(' ');

        // IN (INTERSECT of all CTEs)
        clause.push_str(" AND documents.id IN (");
        for (i, name) in cte_names.iter().enumerate() {
            if i > 0 {
                clause.push_str(" INTERSECT ");
            }
            clause.push_str("SELECT document_id FROM ");
            clause.push_str(name);
        }
        clause.push(')');
    }

    // Regular WHERE fragments for non-has_tag filters
    for filter in &other_filters {
        clause.push_str(" AND ");
        clause.push_str(&filter_expression_sql_clause(filter, &mut params)?);
    }

    Ok(NoteFilterSql {
        cte,
        clause,
        params,
    })
}

pub(crate) fn rebuild_property_catalog(
    transaction: &rusqlite::Transaction<'_>,
    configured_types: &BTreeMap<String, String>,
) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM property_catalog", [])?;
    transaction.execute(
        "
        INSERT INTO property_catalog (key, observed_type, usage_count, namespace)
        SELECT key, value_type, COUNT(*), ?1
        FROM property_values
        GROUP BY key, value_type
        ",
        [PROPERTY_NAMESPACE_FRONTMATTER],
    )?;

    insert_configured_property_types(transaction, configured_types)?;

    Ok(())
}

/// Incrementally refresh the property catalog for only the given document IDs.
/// Instead of a full rebuild, deletes stale catalog entries for keys that appear in the
/// changed documents and recomputes counts only for those keys.
pub(crate) fn refresh_property_catalog_for_documents(
    transaction: &rusqlite::Transaction<'_>,
    changed_document_ids: &[String],
    configured_types: &BTreeMap<String, String>,
) -> Result<(), rusqlite::Error> {
    if changed_document_ids.is_empty() {
        return Ok(());
    }

    let placeholders = changed_document_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");

    // Collect all property keys that appear in the changed documents (before or after update).
    // We need to refresh counts for these keys.
    let sql =
        format!("SELECT DISTINCT key FROM property_values WHERE document_id IN ({placeholders})");
    let mut statement = transaction.prepare(&sql)?;
    let rows = statement.query_map(
        rusqlite::params_from_iter(changed_document_ids.iter()),
        |row| row.get::<_, String>(0),
    )?;
    let affected_keys: Vec<String> = rows.collect::<Result<Vec<_>, _>>()?;

    // Also include keys from the catalog that might now have zero usage
    // (if the changed documents were the only ones using them).
    // The simplest correct approach: delete catalog entries for affected keys,
    // then reinsert with fresh counts.
    if affected_keys.is_empty() {
        return Ok(());
    }

    // Use numbered params: ?1..?N for keys, ?{N+1} for namespace.
    let key_placeholders: Vec<String> =
        (1..=affected_keys.len()).map(|i| format!("?{i}")).collect();
    let key_list = key_placeholders.join(", ");
    let ns_param = affected_keys.len() + 1;

    let mut params: Vec<String> = affected_keys.clone();
    params.push(PROPERTY_NAMESPACE_FRONTMATTER.to_string());

    let delete_sql = format!(
        "DELETE FROM property_catalog WHERE key IN ({key_list}) AND namespace = ?{ns_param}"
    );
    transaction.execute(&delete_sql, rusqlite::params_from_iter(params.iter()))?;

    let insert_sql = format!(
        "INSERT INTO property_catalog (key, observed_type, usage_count, namespace)
         SELECT key, value_type, COUNT(*), ?{ns_param}
         FROM property_values
         WHERE key IN ({key_list})
         GROUP BY key, value_type"
    );
    transaction.execute(&insert_sql, rusqlite::params_from_iter(params.iter()))?;

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
        _ => None,
    }
}

fn property_type_matches(expected_type: &str, actual_type: &str) -> bool {
    expected_type == actual_type || (expected_type == "text" && actual_type == "link")
}

fn canonical_property_type(raw_type: &str) -> &str {
    match raw_type.to_ascii_lowercase().as_str() {
        "bool" | "boolean" | "checkbox" => "boolean",
        "date" | "datetime" => "date",
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
    match value.trim().to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn normalize_date_string(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if matches_iso_date(trimmed) || matches_iso_datetime(trimmed) {
        Some(trimmed)
    } else {
        None
    }
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterOperator {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    Exists,
    StartsWith,
    Contains,
    HasTag,
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

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FilterExpression {
    Condition(ParsedFilter),
    Any(Vec<ParsedFilter>),
}

pub(crate) fn parse_note_filter_expression(filter: &str) -> Result<ParsedFilter, PropertyError> {
    parse_filter_expression(filter)
}

fn parse_filter_expression(filter: &str) -> Result<ParsedFilter, PropertyError> {
    for (separator, operator) in [
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
            return Ok(ParsedFilter {
                field: parse_filter_field(field.trim()),
                operator,
                value: parse_filter_value(value.trim()),
            });
        }
    }

    Err(PropertyError::InvalidFilter(filter.to_string()))
}

fn parse_filter_field(field: &str) -> FilterField {
    match field {
        "file.path" => FilterField::FilePath,
        "file.name" => FilterField::FileName,
        "file.ext" => FilterField::FileExt,
        "file.mtime" => FilterField::FileMtime,
        other => FilterField::Property(other.to_string()),
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

fn strip_quotes(value: &str) -> Option<&str> {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        Some(&value[1..value.len() - 1])
    } else {
        None
    }
}

fn filter_sql_clause(
    filter: &ParsedFilter,
    params: &mut Vec<SqlValue>,
) -> Result<String, PropertyError> {
    match (&filter.field, filter.operator, &filter.value) {
        (FilterField::Property(key), FilterOperator::Exists, _) => {
            params.push(SqlValue::Text(key.clone()));
            Ok(
                "EXISTS (SELECT 1 FROM property_values WHERE property_values.document_id = documents.id AND property_values.key = ?)"
                    .to_string(),
            )
        }
        (FilterField::Property(key), FilterOperator::Contains, FilterValue::Text(value)) => {
            params.push(SqlValue::Text(key.clone()));
            params.push(SqlValue::Text(value.clone()));
            Ok(
                "EXISTS (SELECT 1 FROM property_list_items WHERE property_list_items.document_id = documents.id AND property_list_items.key = ? AND property_list_items.value_text = ?)".to_string(),
            )
        }
        (FilterField::Property(key), FilterOperator::HasTag, FilterValue::Text(value)) => {
            // Use UNION ALL so SQLite seeks the (key, value_text) index for
            // both the exact match and the nested-tag prefix range, instead of
            // falling back to a per-document tag scan.
            // Range: value_text >= "tag/" AND value_text < "tag0"  ('0' is the
            // character after '/' in ASCII, so this captures all "tag/…" entries)
            params.push(SqlValue::Text(key.clone()));
            params.push(SqlValue::Text(value.clone()));
            params.push(SqlValue::Text(key.clone()));
            params.push(SqlValue::Text(format!("{value}/")));
            params.push(SqlValue::Text(format!("{value}0")));
            Ok("EXISTS (\
                    SELECT 1 FROM property_list_items \
                    WHERE property_list_items.document_id = documents.id \
                    AND property_list_items.key = ? AND property_list_items.value_text = ? \
                    UNION ALL \
                    SELECT 1 FROM property_list_items \
                    WHERE property_list_items.document_id = documents.id \
                    AND property_list_items.key = ? \
                    AND property_list_items.value_text >= ? AND property_list_items.value_text < ? \
                )"
            .to_string())
        }
        (FilterField::Property(key), FilterOperator::StartsWith, FilterValue::Text(value)) => {
            params.push(SqlValue::Text(key.clone()));
            params.push(SqlValue::Text(format!("{value}%")));
            Ok(
                "EXISTS (SELECT 1 FROM property_values WHERE property_values.document_id = documents.id AND property_values.key = ? AND property_values.value_text LIKE ?)".to_string(),
            )
        }
        (FilterField::Property(key), operator, value) => {
            property_scalar_clause(key, operator, value, params)
        }
        (field, FilterOperator::Contains | FilterOperator::HasTag | FilterOperator::Exists, _) => {
            Err(PropertyError::InvalidFilter(match field {
                FilterField::Property(key) => key.clone(),
                FilterField::FilePath => "file.path".to_string(),
                FilterField::FileName => "file.name".to_string(),
                FilterField::FileExt => "file.ext".to_string(),
                FilterField::FileMtime => "file.mtime".to_string(),
            }))
        }
        (field, operator, value) => Ok(file_field_clause(field, operator, value, params)?),
    }
}

fn filter_expression_sql_clause(
    filter: &FilterExpression,
    params: &mut Vec<SqlValue>,
) -> Result<String, PropertyError> {
    match filter {
        FilterExpression::Condition(condition) => filter_sql_clause(condition, params),
        FilterExpression::Any(filters) => {
            let mut clauses = Vec::new();
            for condition in filters {
                clauses.push(filter_sql_clause(condition, params)?);
            }
            Ok(format!("({})", clauses.join(" OR ")))
        }
    }
}

fn property_scalar_clause(
    key: &str,
    operator: FilterOperator,
    value: &FilterValue,
    params: &mut Vec<SqlValue>,
) -> Result<String, PropertyError> {
    match value {
        FilterValue::Null => {
            let _ = sql_comparator(operator)?;
            params.push(SqlValue::Text(key.to_string()));
            Ok(
                "EXISTS (SELECT 1 FROM property_values WHERE property_values.document_id = documents.id AND property_values.key = ? AND property_values.value_type = 'null')".to_string(),
            )
        }
        FilterValue::Bool(value_bool) => {
            let comparator = sql_comparator(operator)?;
            params.push(SqlValue::Text(key.to_string()));
            params.push(SqlValue::Integer(i64::from(*value_bool)));
            Ok(format!(
                "EXISTS (SELECT 1 FROM property_values WHERE property_values.document_id = documents.id AND property_values.key = ? AND property_values.value_bool {comparator} ?)"
            ))
        }
        FilterValue::Number(value_number) => {
            let comparator = sql_comparator(operator)?;
            params.push(SqlValue::Text(key.to_string()));
            params.push(SqlValue::Real(*value_number));
            Ok(format!(
                "EXISTS (SELECT 1 FROM property_values WHERE property_values.document_id = documents.id AND property_values.key = ? AND property_values.value_number {comparator} ?)"
            ))
        }
        FilterValue::Date(value_date) => {
            let comparator = sql_comparator(operator)?;
            params.push(SqlValue::Text(key.to_string()));
            params.push(SqlValue::Text(value_date.clone()));
            Ok(format!(
                "EXISTS (SELECT 1 FROM property_values WHERE property_values.document_id = documents.id AND property_values.key = ? AND property_values.value_date {comparator} ?)"
            ))
        }
        FilterValue::Text(value_text) => {
            params.push(SqlValue::Text(key.to_string()));
            if operator == FilterOperator::StartsWith {
                params.push(SqlValue::Text(format!("{value_text}%")));
                Ok(
                    "EXISTS (SELECT 1 FROM property_values WHERE property_values.document_id = documents.id AND property_values.key = ? AND property_values.value_text LIKE ?)".to_string(),
                )
            } else {
                let comparator = sql_comparator(operator)?;
                params.push(SqlValue::Text(value_text.clone()));
                Ok(format!(
                    "EXISTS (SELECT 1 FROM property_values WHERE property_values.document_id = documents.id AND property_values.key = ? AND property_values.value_text {comparator} ?)"
                ))
            }
        }
    }
}

fn file_field_clause(
    field: &FilterField,
    operator: FilterOperator,
    value: &FilterValue,
    params: &mut Vec<SqlValue>,
) -> Result<String, PropertyError> {
    let (column, sql_value) = match (field, value) {
        (FilterField::FilePath, FilterValue::Text(value)) => {
            ("documents.path", SqlValue::Text(value.clone()))
        }
        (FilterField::FileName, FilterValue::Text(value)) => {
            ("documents.filename", SqlValue::Text(value.clone()))
        }
        (FilterField::FileExt, FilterValue::Text(value)) => {
            ("documents.extension", SqlValue::Text(value.clone()))
        }
        (FilterField::FileMtime, FilterValue::Number(value)) => (
            "documents.file_mtime",
            SqlValue::Integer(number_to_i64(*value).ok_or_else(|| {
                PropertyError::InvalidFilter("file.mtime expects an integer value".to_string())
            })?),
        ),
        _ => {
            return Err(PropertyError::InvalidFilter(match field {
                FilterField::Property(key) => key.clone(),
                FilterField::FilePath => "file.path".to_string(),
                FilterField::FileName => "file.name".to_string(),
                FilterField::FileExt => "file.ext".to_string(),
                FilterField::FileMtime => "file.mtime".to_string(),
            }))
        }
    };
    params.push(sql_value);
    match operator {
        FilterOperator::StartsWith => {
            let SqlValue::Text(value) = params.pop().expect("starts_with param should exist")
            else {
                unreachable!("starts_with only accepts text values");
            };
            params.push(SqlValue::Text(format!("{value}%")));
            Ok(format!("{column} LIKE ?"))
        }
        _ => Ok(format!("{column} {} ?", sql_comparator(operator)?)),
    }
}

fn sql_comparator(operator: FilterOperator) -> Result<&'static str, PropertyError> {
    match operator {
        FilterOperator::Eq => Ok("="),
        FilterOperator::Ne => Ok("<>"),
        FilterOperator::Gt => Ok(">"),
        FilterOperator::Gte => Ok(">="),
        FilterOperator::Lt => Ok("<"),
        FilterOperator::Lte => Ok("<="),
        FilterOperator::Exists => Err(PropertyError::InvalidFilter(
            "exists is an internal-only filter operator".to_string(),
        )),
        FilterOperator::StartsWith => Err(PropertyError::InvalidFilter(
            "starts_with only supports text fields".to_string(),
        )),
        FilterOperator::Contains => Err(PropertyError::InvalidFilter(
            "contains only supports property lists".to_string(),
        )),
        FilterOperator::HasTag => Err(PropertyError::InvalidFilter(
            "has_tag only supports property lists".to_string(),
        )),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum SortKey {
    Missing,
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
        "file.ext" => SortKey::Text(note.file_ext.clone()),
        "file.mtime" => SortKey::Integer(note.file_mtime),
        key => match note.properties.get(key) {
            Some(Value::Null) => SortKey::Null,
            Some(Value::Bool(value_bool)) => SortKey::Bool(*value_bool),
            Some(Value::Number(value_number)) => {
                SortKey::Number(value_number.as_f64().unwrap_or_default())
            }
            Some(Value::String(value_text)) => SortKey::Text(value_text.clone()),
            Some(other) => SortKey::Text(other.to_string()),
            None => SortKey::Missing,
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
        SortKey::Missing => 0,
        SortKey::Null => 1,
        SortKey::Bool(_) => 2,
        SortKey::Integer(_) => 3,
        SortKey::Number(_) => 4,
        SortKey::Text(_) => 5,
    }
}

fn number_to_i64(value: f64) -> Option<i64> {
    if !value.is_finite() || value.fract() != 0.0 {
        return None;
    }

    format!("{value:.0}").parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_document, scan_vault, ScanMode};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

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
        let indexed = extract_indexed_properties(
            parsed.raw_frontmatter.as_deref(),
            parsed.frontmatter.as_ref(),
            &config,
        )
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
    fn query_notes_filters_and_sorts_using_property_tables() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
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

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);

        copy_dir_recursive(&source, destination);
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
