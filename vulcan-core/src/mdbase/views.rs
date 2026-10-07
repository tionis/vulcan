//! mdbase v0.3 saved view records (Chapter 11 "Saved View Records" and the
//! Chapter 12 `list_views`/`execute_view` operations).
//!
//! A saved view is an ordinary record matched by the `view` type. Discovery and
//! resolution operate on one caller-authorized record snapshot; resolution
//! derives a canonical query object, so execution reuses the canonical query
//! engine rather than a separate view evaluator. Presentation is advisory and
//! never changes the headless result.

use super::{
    bundled_mdbase_schema, compile_mdbase_prepared_query, validate_mdbase_schema_value,
    MdbaseDiagnostic, MdbaseDiagnosticLevel, MdbaseQueryError, MdbaseQueryResult,
    MdbaseQueryViewMeta, MdbaseRecordDocument, MdbaseRecordSet, MdbaseTypeRegistry,
    MDBASE_CANONICAL_SCHEMA_BASE,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Source format identifier for canonical view records.
pub const MDBASE_VIEW_SOURCE_FORMAT: &str = "mdbase.view";
/// Source format identifier for Obsidian `.base` files (Chapter 15).
pub const MDBASE_OBSIDIAN_BASE_SOURCE_FORMAT: &str = "obsidian.base";
/// The type name that marks a record as a saved view.
pub const MDBASE_VIEW_TYPE: &str = "view";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseViewList {
    pub views: Vec<MdbaseViewSource>,
    pub meta: MdbaseViewListMeta,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseViewListMeta {
    pub total_count: usize,
}

/// One discovered saved-view source and its named views.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseViewSource {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: MdbaseViewSourceRef,
    pub views: Vec<MdbaseNamedViewDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseViewSourceRef {
    pub path: String,
    pub format: String,
    pub revision: String,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseNamedViewDescriptor {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Selected result values in display order, with property metadata.
    pub properties: Vec<MdbaseViewProperty>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseViewProperty {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
}

/// How the caller supplied the invocation context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MdbaseViewContextArg {
    /// No context member: the view's `on_missing` policy applies.
    #[default]
    Absent,
    /// An explicit null context, which binds `this` to null.
    Null,
    /// A collection-relative record path.
    Path(String),
}

/// One `execute_view` invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MdbaseViewInvocation {
    /// View record path or stable record ID.
    pub source: String,
    /// Named-view ID.
    pub view: String,
    pub context: MdbaseViewContextArg,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub timezone: Option<String>,
    /// Request rendered output instead of the headless result.
    pub render: bool,
}

/// A resolved named view: the derived canonical query plus its identity.
#[derive(Debug, Clone, PartialEq)]
pub struct MdbaseResolvedView {
    /// Resolved view-record path.
    pub path: String,
    /// Named-view ID.
    pub id: String,
    /// The canonical query object for this invocation, context bound.
    pub query: serde_json::Value,
}

/// Stable named-view IDs for a source format without IDs, such as `.base`:
/// each derives from the view's name (lowercase ASCII letters and digits,
/// other runs collapsed to `-`; `view` when nothing remains), and a repeated
/// ID takes a source-order suffix (`-2`, `-3`, ...).
#[must_use]
pub fn derive_mdbase_view_ids(names: &[Option<&str>]) -> Vec<String> {
    let mut used = BTreeSet::new();
    names
        .iter()
        .map(|name| {
            let mut base = String::new();
            for character in name.unwrap_or_default().chars() {
                if character.is_ascii_alphanumeric() {
                    base.push(character.to_ascii_lowercase());
                } else if !base.is_empty() && !base.ends_with('-') {
                    base.push('-');
                }
            }
            let base = match base.trim_end_matches('-') {
                "" => "view".to_string(),
                trimmed if trimmed.starts_with(|c: char| c.is_ascii_digit()) => {
                    format!("view-{trimmed}")
                }
                trimmed => trimmed.to_string(),
            };
            let mut id = base.clone();
            let mut suffix = 2;
            while !used.insert(id.clone()) {
                id = format!("{base}-{suffix}");
                suffix += 1;
            }
            id
        })
        .collect()
}

/// Whether a record is a saved view: it is matched by the `view` type.
#[must_use]
pub fn is_mdbase_view_record(record: &MdbaseRecordDocument) -> bool {
    record
        .types
        .iter()
        .any(|name| name.eq_ignore_ascii_case(MDBASE_VIEW_TYPE))
}

/// Discover every saved-view source in an authorized record snapshot, in
/// ascending source-path order. Malformed sources are omitted and reported as
/// warnings, as `list_views` requires.
#[must_use]
pub fn list_mdbase_views(records: &MdbaseRecordSet) -> MdbaseViewList {
    let mut sources = records
        .records
        .iter()
        .filter(|record| is_mdbase_view_record(record))
        .collect::<Vec<_>>();
    sources.sort_by(|left, right| left.path.cmp(&right.path));
    let mut views = Vec::new();
    let mut diagnostics = Vec::new();
    for record in sources {
        match parse_view_record(record) {
            Ok(view) => views.push(describe_view(record, &view)),
            Err(errors) => diagnostics.extend(errors.into_iter().map(|mut diagnostic| {
                diagnostic.severity = MdbaseDiagnosticLevel::Warning;
                diagnostic
            })),
        }
    }
    MdbaseViewList {
        meta: MdbaseViewListMeta {
            total_count: views.len(),
        },
        views,
        diagnostics,
    }
}

/// Resolve a named view and bind its invocation context, producing the
/// canonical query to execute. Every failure here happens before any
/// candidate is evaluated.
pub fn resolve_mdbase_view(
    records: &MdbaseRecordSet,
    invocation: &MdbaseViewInvocation,
) -> Result<MdbaseResolvedView, MdbaseQueryError> {
    let record = find_view_record(records, &invocation.source)?;
    let view = parse_view_record(record).map_err(|diagnostics| MdbaseQueryError { diagnostics })?;
    let named = view
        .views
        .iter()
        .find(|named| named.id == invocation.view)
        .ok_or_else(|| {
            view_error(
                "view_not_found",
                format!(
                    "view record `{}` has no named view `{}`",
                    record.path, invocation.view
                ),
                &record.path,
            )
        })?;
    if invocation.render {
        // Vulcan executes views headlessly and ships no renderers, so neither
        // a presentation type nor its fallback is ever available.
        return Err(view_error(
            "unsupported_presentation",
            "rendered view output is not supported; request the headless result",
            &record.path,
        ));
    }
    let mut query = derive_query(&view, named);
    let policy = named.context.as_ref().or(view.query.context.as_ref());
    if let Some(path) = bind_context(records, record, policy, &invocation.context)? {
        query.insert(
            "context".to_string(),
            serde_json::json!({"this": {"path": path}}),
        );
    }
    if let Some(limit) = invocation.limit {
        query.insert("limit".to_string(), limit.into());
    }
    if let Some(offset) = invocation.offset {
        query.insert("offset".to_string(), offset.into());
    }
    if let Some(timezone) = &invocation.timezone {
        query.insert("timezone".to_string(), timezone.clone().into());
    }
    Ok(MdbaseResolvedView {
        path: record.path.clone(),
        id: named.id.clone(),
        query: serde_json::Value::Object(query),
    })
}

/// Resolve and execute a named view over one authorized record snapshot,
/// returning the canonical query envelope with `meta.view`. Context and the
/// candidates come from the same snapshot, so the context is fixed for the
/// whole execution.
pub fn execute_mdbase_view(
    records: &MdbaseRecordSet,
    types: &MdbaseTypeRegistry,
    invocation: &MdbaseViewInvocation,
    id_field: &str,
    collection_timezone: Option<&str>,
    now: DateTime<Utc>,
) -> Result<MdbaseQueryResult, MdbaseQueryError> {
    let resolved = resolve_mdbase_view(records, invocation)?;
    let mut result = compile_mdbase_prepared_query(&resolved.query)?.execute(
        records,
        types,
        id_field,
        collection_timezone,
        now,
    )?;
    result.meta.view = Some(MdbaseQueryViewMeta {
        path: resolved.path,
        id: resolved.id,
    });
    Ok(result)
}

/// Validate a complete proposed view-record source before it is written:
/// it must be matched by the `view` type at `path` and satisfy the view
/// schema and the named-view rules. Record schema validation stays with the
/// managed write pipeline. Returns the proposed record's stable view ID.
pub fn validate_mdbase_view_source(
    collection: &super::MdbaseCollection,
    types: &MdbaseTypeRegistry,
    path: &str,
    source: &str,
) -> Result<String, MdbaseQueryError> {
    let record = super::records::build_mdbase_record(
        collection,
        types,
        path,
        source.to_string(),
        None,
        false,
        &super::records::operation_clock(collection),
    );
    if !is_mdbase_view_record(&record) {
        return Err(view_error(
            "invalid_view",
            format!("`{path}` would not be matched by the `view` type"),
            path,
        ));
    }
    parse_view_record(&record)
        .map(|view| view.id)
        .map_err(|diagnostics| MdbaseQueryError { diagnostics })
}

#[derive(Debug, Deserialize)]
struct ViewRecord {
    id: String,
    name: String,
    description: Option<String>,
    #[serde(default)]
    query: SharedQuery,
    #[serde(default)]
    properties: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    summary_functions: serde_json::Map<String, serde_json::Value>,
    views: Vec<NamedView>,
}

#[derive(Debug, Default, Deserialize)]
struct SharedQuery {
    types: Option<Vec<String>>,
    #[serde(rename = "where")]
    filter: Option<String>,
    context: Option<ViewContext>,
    #[serde(default)]
    projections: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ViewContext {
    this: ThisContext,
}

#[derive(Debug, Default, Deserialize)]
struct ThisContext {
    #[serde(default)]
    on_missing: OnMissing,
    types: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OnMissing {
    #[default]
    View,
    Null,
    Error,
}

#[derive(Debug, Deserialize)]
struct NamedView {
    id: String,
    name: String,
    description: Option<String>,
    types: Option<Vec<String>>,
    #[serde(rename = "where")]
    filter: Option<String>,
    context: Option<ViewContext>,
    #[serde(default)]
    projections: serde_json::Map<String, serde_json::Value>,
    select: Option<Vec<serde_json::Value>>,
    order_by: Option<serde_json::Value>,
    group_by: Option<serde_json::Value>,
    summaries: Option<serde_json::Value>,
    limit: Option<usize>,
    offset: Option<usize>,
    include_body: Option<bool>,
    frontmatter_mode: Option<String>,
    presentation: Option<serde_json::Value>,
}

/// Validate persisted frontmatter against the canonical view schema, then the
/// rules the schema cannot express: unique named-view IDs and structurally
/// equal shared/named projections.
fn parse_view_record(record: &MdbaseRecordDocument) -> Result<ViewRecord, Vec<MdbaseDiagnostic>> {
    let schema = bundled_mdbase_schema(&format!("{MDBASE_CANONICAL_SCHEMA_BASE}view.schema.json"))
        .expect("view schema is bundled");
    let schema = serde_json::from_str(schema.json).expect("bundled view schema is valid JSON");
    let issues = validate_mdbase_schema_value(&schema, &record.frontmatter)
        .expect("bundled view schema compiles");
    if !issues.is_empty() {
        return Err(issues
            .into_iter()
            .map(|issue| MdbaseDiagnostic {
                severity: MdbaseDiagnosticLevel::Error,
                code: "invalid_view".to_string(),
                message: format!("view record `{}`: {}", record.path, issue.message),
                path: Some(record.path.clone()),
                field: Some(issue.instance_path),
                type_name: None,
                schema_location: Some(issue.schema_path),
                details: Some(serde_json::json!({"schema_code": issue.code})),
            })
            .collect());
    }
    let view =
        serde_json::from_value::<ViewRecord>(record.frontmatter.clone()).map_err(|error| {
            vec![view_diagnostic(
                "invalid_view",
                format!("view record `{}`: {error}", record.path),
                &record.path,
            )]
        })?;
    let mut seen = BTreeSet::new();
    let mut errors = Vec::new();
    for named in &view.views {
        if !seen.insert(named.id.as_str()) {
            errors.push(view_diagnostic(
                "invalid_view",
                format!(
                    "view record `{}` declares named view `{}` more than once",
                    record.path, named.id
                ),
                &record.path,
            ));
        }
        for (name, definition) in &named.projections {
            if view
                .query
                .projections
                .get(name)
                .is_some_and(|shared| shared != definition)
            {
                errors.push(view_diagnostic(
                    "invalid_view",
                    format!(
                        "named view `{}` in `{}` redefines shared projection `{name}` differently",
                        named.id, record.path
                    ),
                    &record.path,
                ));
            }
        }
    }
    if errors.is_empty() {
        Ok(view)
    } else {
        Err(errors)
    }
}

fn describe_view(record: &MdbaseRecordDocument, view: &ViewRecord) -> MdbaseViewSource {
    MdbaseViewSource {
        id: view.id.clone(),
        name: view.name.clone(),
        description: view.description.clone(),
        source: MdbaseViewSourceRef {
            path: record.path.clone(),
            format: MDBASE_VIEW_SOURCE_FORMAT.to_string(),
            revision: record.revision.clone(),
            writable: true,
        },
        views: view
            .views
            .iter()
            .map(|named| MdbaseNamedViewDescriptor {
                id: named.id.clone(),
                name: named.name.clone(),
                description: named.description.clone(),
                properties: named
                    .select
                    .iter()
                    .flatten()
                    .filter_map(|selection| view_property(&view.properties, selection))
                    .collect(),
                presentation: named.presentation.clone(),
            })
            .collect(),
    }
}

/// One selected output as a property descriptor. Metadata keys may name the
/// selected field (`projection.x`, `file.name`, `title`) or its output key;
/// an expression selection's own label and description take precedence.
fn view_property(
    metadata: &serde_json::Map<String, serde_json::Value>,
    selection: &serde_json::Value,
) -> Option<MdbaseViewProperty> {
    let (field, key, own) = match selection {
        serde_json::Value::String(field) => (
            field.as_str(),
            field.rsplit('.').next().unwrap_or(field).to_string(),
            None,
        ),
        serde_json::Value::Object(expression) => {
            let name = expression.get("name")?.as_str()?;
            (name, name.to_string(), Some(expression))
        }
        _ => return None,
    };
    let entry = metadata
        .get(field)
        .or_else(|| metadata.get(&key))
        .and_then(serde_json::Value::as_object);
    let text = |member: &str| {
        own.and_then(|own| own.get(member))
            .or_else(|| entry.and_then(|entry| entry.get(member)))
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string)
    };
    Some(MdbaseViewProperty {
        label: text("label"),
        description: text("description"),
        format: entry
            .and_then(|entry| entry.get("format"))
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        hidden: entry
            .and_then(|entry| entry.get("hidden"))
            .and_then(serde_json::Value::as_bool),
        key,
    })
}

/// Address a view record by path, then by its stable record ID.
fn find_view_record<'a>(
    records: &'a MdbaseRecordSet,
    source: &str,
) -> Result<&'a MdbaseRecordDocument, MdbaseQueryError> {
    if let Some(record) = records
        .get(source)
        .filter(|record| is_mdbase_view_record(record))
    {
        return Ok(record);
    }
    let mut matches = records.records.iter().filter(|record| {
        is_mdbase_view_record(record)
            && record
                .frontmatter
                .get("id")
                .and_then(serde_json::Value::as_str)
                == Some(source)
    });
    match (matches.next(), matches.next()) {
        (Some(record), None) => Ok(record),
        (Some(first), Some(second)) => Err(view_error(
            "invalid_view",
            format!(
                "view ID `{source}` is ambiguous: `{}` and `{}` both declare it; address the view by path",
                first.path, second.path
            ),
            source,
        )),
        (None, _) => Err(view_error(
            "view_not_found",
            format!("no view record has path or ID `{source}`"),
            source,
        )),
    }
}

/// Apply Chapter 11 named-view resolution steps 1-3 and 5. Context (step 4)
/// and binding (step 6) are handled by the caller.
fn derive_query(
    view: &ViewRecord,
    named: &NamedView,
) -> serde_json::Map<String, serde_json::Value> {
    let mut query = serde_json::Map::new();
    if let Some(types) = named.types.as_ref().or(view.query.types.as_ref()) {
        query.insert("types".to_string(), types.clone().into());
    }
    let filter = match (&view.query.filter, &named.filter) {
        (Some(shared), Some(own)) => Some(format!("({shared}) && ({own})")),
        (shared, own) => shared.clone().or_else(|| own.clone()),
    };
    if let Some(filter) = filter {
        query.insert("where".to_string(), filter.into());
    }
    let mut projections = view.query.projections.clone();
    projections.extend(named.projections.clone());
    if !projections.is_empty() {
        query.insert(
            "projections".to_string(),
            serde_json::Value::Object(projections),
        );
    }
    if !view.summary_functions.is_empty() {
        query.insert(
            "summary_functions".to_string(),
            serde_json::Value::Object(view.summary_functions.clone()),
        );
    }
    let copies = [
        ("select", named.select.clone().map(serde_json::Value::from)),
        ("order_by", named.order_by.clone()),
        ("group_by", named.group_by.clone()),
        ("summaries", named.summaries.clone()),
        ("limit", named.limit.map(serde_json::Value::from)),
        ("offset", named.offset.map(serde_json::Value::from)),
        (
            "include_body",
            named.include_body.map(serde_json::Value::from),
        ),
        (
            "frontmatter_mode",
            named.frontmatter_mode.clone().map(serde_json::Value::from),
        ),
    ];
    query.extend(
        copies
            .into_iter()
            .filter_map(|(key, value)| Some((key.to_string(), value?))),
    );
    query
}

/// Choose the record bound to `this`: an explicit context always wins; with
/// none, `on_missing` binds the view record, null, or fails. A non-null
/// context must match a declared context type.
fn bind_context(
    records: &MdbaseRecordSet,
    view_record: &MdbaseRecordDocument,
    policy: Option<&ViewContext>,
    argument: &MdbaseViewContextArg,
) -> Result<Option<String>, MdbaseQueryError> {
    let this = policy.map(|policy| &policy.this);
    let record = match argument {
        MdbaseViewContextArg::Null => return Ok(None),
        MdbaseViewContextArg::Path(path) => records.get(path).ok_or_else(|| {
            view_error(
                "context_not_found",
                format!("view context record was not found: {path}"),
                &view_record.path,
            )
        })?,
        MdbaseViewContextArg::Absent => {
            match this.map_or(OnMissing::View, |this| this.on_missing) {
                OnMissing::View => view_record,
                OnMissing::Null => return Ok(None),
                OnMissing::Error => {
                    return Err(view_error(
                        "context_required",
                        "this view requires an invocation context record",
                        &view_record.path,
                    ))
                }
            }
        }
    };
    if let Some(types) = this.and_then(|this| this.types.as_ref()) {
        let matches = record.types.iter().any(|actual| {
            types
                .iter()
                .any(|expected| expected.eq_ignore_ascii_case(actual))
        });
        if !matches {
            return Err(view_error(
                "context_type_mismatch",
                format!(
                    "context record `{}` is not one of the view's context types: {}",
                    record.path,
                    types.join(", ")
                ),
                &view_record.path,
            ));
        }
    }
    Ok(Some(record.path.clone()))
}

fn view_diagnostic(code: &str, message: impl Into<String>, path: &str) -> MdbaseDiagnostic {
    MdbaseDiagnostic {
        severity: MdbaseDiagnosticLevel::Error,
        code: code.to_string(),
        message: message.into(),
        path: Some(path.to_string()),
        field: None,
        type_name: None,
        schema_location: None,
        details: None,
    }
}

fn view_error(code: &str, message: impl Into<String>, path: &str) -> MdbaseQueryError {
    MdbaseQueryError {
        diagnostics: vec![view_diagnostic(code, message, path)],
    }
}

#[cfg(test)]
mod tests;
