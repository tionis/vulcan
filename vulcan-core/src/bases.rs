use crate::expression::eval::EvalContext;
use crate::expression::parse_expression;
use crate::paths::{normalize_relative_input_path, RelativePathError, RelativePathOptions};
use crate::{query_notes, NoteQuery, NoteRecord, PropertyError, VaultPaths};
use serde::Serialize;
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;

#[derive(Debug)]
pub enum BasesError {
    InvalidPath(RelativePathError),
    Io(std::io::Error),
    Property(PropertyError),
    Yaml(serde_yaml::Error),
}

impl Display for BasesError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath(error) => write!(formatter, "invalid base file path: {error}"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Property(error) => write!(formatter, "{error}"),
            Self::Yaml(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for BasesError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Property(error) => Some(error),
            Self::Yaml(error) => Some(error),
            Self::InvalidPath(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for BasesError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<PropertyError> for BasesError {
    fn from(error: PropertyError) -> Self {
        Self::Property(error)
    }
}

impl From<serde_yaml::Error> for BasesError {
    fn from(error: serde_yaml::Error) -> Self {
        Self::Yaml(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BasesDiagnostic {
    pub path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BasesEvalReport {
    pub file: String,
    pub views: Vec<BasesEvaluatedView>,
    pub diagnostics: Vec<BasesDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BasesColumn {
    pub key: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BasesGroupBy {
    pub property: String,
    pub display_name: String,
    pub descending: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BasesEvaluatedView {
    pub name: Option<String>,
    pub view_type: String,
    pub filters: Vec<String>,
    pub sort_by: Option<String>,
    pub sort_descending: bool,
    pub columns: Vec<BasesColumn>,
    pub group_by: Option<BasesGroupBy>,
    pub rows: Vec<BasesRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BasesRow {
    pub document_path: String,
    pub file_name: String,
    pub file_ext: String,
    pub file_mtime: i64,
    pub properties: Value,
    pub formulas: BTreeMap<String, Value>,
    pub cells: BTreeMap<String, Value>,
    pub group_value: Option<Value>,
}

// ── View-spec public structs ────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize)]
pub struct BaseViewSpec {
    pub name: Option<String>,
    pub view_type: String,
    pub filters: Vec<String>,
    pub sort_by: Option<String>,
    pub sort_descending: bool,
    pub columns: Vec<String>,
    pub group_by: Option<BaseViewGroupBy>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BaseViewGroupBy {
    pub property: String,
    pub descending: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BaseViewPatch {
    pub set_filters: Option<Vec<String>>,
    pub add_filters: Vec<String>,
    pub remove_filters: Vec<String>,
    pub set_columns: Option<Vec<String>>,
    pub set_sort: Option<Option<String>>,
    pub set_sort_descending: Option<bool>,
    pub set_group_by: Option<Option<BaseViewGroupBy>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BasesViewEditReport {
    pub file: String,
    pub action: String,
    pub dry_run: bool,
    pub eval: BasesEvalReport,
}

// ── Serializer ───────────────────────────────────────────────────────────────

fn serialize_base_file(parsed: &ParsedBaseFile) -> Result<String, BasesError> {
    let mut root = serde_yaml::Mapping::new();

    // source: omit if "notes" and there are filters or views (default implied)
    // include if non-notes source
    if parsed.source != "notes" {
        root.insert(
            serde_yaml::Value::String("source".to_string()),
            serde_yaml::Value::String(parsed.source.clone()),
        );
    }

    // filters
    if !parsed.filters.is_empty() {
        let seq: serde_yaml::Sequence = parsed
            .filters
            .iter()
            .map(|f| serde_yaml::Value::String(f.clone()))
            .collect();
        root.insert(
            serde_yaml::Value::String("filters".to_string()),
            serde_yaml::Value::Sequence(seq),
        );
    }

    // properties
    if !parsed.property_display_names.is_empty() {
        let mut props = serde_yaml::Mapping::new();
        for (key, display_name) in &parsed.property_display_names {
            let mut def = serde_yaml::Mapping::new();
            def.insert(
                serde_yaml::Value::String("displayName".to_string()),
                serde_yaml::Value::String(display_name.clone()),
            );
            props.insert(
                serde_yaml::Value::String(key.clone()),
                serde_yaml::Value::Mapping(def),
            );
        }
        root.insert(
            serde_yaml::Value::String("properties".to_string()),
            serde_yaml::Value::Mapping(props),
        );
    }

    // views
    if !parsed.views.is_empty() {
        let seq: serde_yaml::Sequence = parsed.views.iter().map(serialize_view).collect();
        root.insert(
            serde_yaml::Value::String("views".to_string()),
            serde_yaml::Value::Sequence(seq),
        );
    }

    serde_yaml::to_string(&serde_yaml::Value::Mapping(root)).map_err(BasesError::Yaml)
}

fn serialize_view(view: &ParsedBaseView) -> serde_yaml::Value {
    let mut m = serde_yaml::Mapping::new();

    if let Some(name) = &view.name {
        m.insert(
            serde_yaml::Value::String("name".to_string()),
            serde_yaml::Value::String(name.clone()),
        );
    }

    m.insert(
        serde_yaml::Value::String("type".to_string()),
        serde_yaml::Value::String(view.view_type.clone()),
    );

    if !view.filters.is_empty() {
        let seq: serde_yaml::Sequence = view
            .filters
            .iter()
            .map(|f| serde_yaml::Value::String(f.clone()))
            .collect();
        m.insert(
            serde_yaml::Value::String("filters".to_string()),
            serde_yaml::Value::Sequence(seq),
        );
    }

    if !view.columns.is_empty() {
        let seq: serde_yaml::Sequence = view
            .columns
            .iter()
            .map(|c| serde_yaml::Value::String(c.clone()))
            .collect();
        m.insert(
            serde_yaml::Value::String("order".to_string()),
            serde_yaml::Value::Sequence(seq),
        );
    }

    if let Some(sort_by) = &view.sort_by {
        let sort_val = if view.sort_descending {
            let mut sort_map = serde_yaml::Mapping::new();
            sort_map.insert(
                serde_yaml::Value::String("by".to_string()),
                serde_yaml::Value::String(sort_by.clone()),
            );
            sort_map.insert(
                serde_yaml::Value::String("desc".to_string()),
                serde_yaml::Value::Bool(true),
            );
            serde_yaml::Value::Mapping(sort_map)
        } else {
            serde_yaml::Value::String(sort_by.clone())
        };
        m.insert(serde_yaml::Value::String("sort".to_string()), sort_val);
    }

    if let Some(group_by) = &view.group_by {
        let mut gb_map = serde_yaml::Mapping::new();
        gb_map.insert(
            serde_yaml::Value::String("property".to_string()),
            serde_yaml::Value::String(group_by.property.clone()),
        );
        gb_map.insert(
            serde_yaml::Value::String("direction".to_string()),
            serde_yaml::Value::String(if group_by.descending {
                "desc".to_string()
            } else {
                "asc".to_string()
            }),
        );
        m.insert(
            serde_yaml::Value::String("groupBy".to_string()),
            serde_yaml::Value::Mapping(gb_map),
        );
    }

    if !view.formulas.is_empty() {
        let mut formulas_map = serde_yaml::Mapping::new();
        for (name, expr) in &view.formulas {
            formulas_map.insert(
                serde_yaml::Value::String(name.clone()),
                serde_yaml::Value::String(expr.clone()),
            );
        }
        m.insert(
            serde_yaml::Value::String("formulas".to_string()),
            serde_yaml::Value::Mapping(formulas_map),
        );
    }

    serde_yaml::Value::Mapping(m)
}

// ── Conversion helpers ────────────────────────────────────────────────────────

fn spec_to_parsed_view(spec: BaseViewSpec) -> ParsedBaseView {
    let view_type = if spec.view_type.is_empty() {
        "table".to_string()
    } else {
        spec.view_type
    };
    ParsedBaseView {
        name: spec.name,
        view_type,
        filters: spec.filters,
        sort_by: spec.sort_by,
        sort_descending: spec.sort_descending,
        columns: spec.columns,
        group_by: spec.group_by.map(|g| ParsedBaseGroupBy {
            property: g.property,
            descending: g.descending,
        }),
        formulas: BTreeMap::new(),
    }
}

#[allow(dead_code)]
fn parsed_view_to_spec(view: &ParsedBaseView) -> BaseViewSpec {
    BaseViewSpec {
        name: view.name.clone(),
        view_type: view.view_type.clone(),
        filters: view.filters.clone(),
        sort_by: view.sort_by.clone(),
        sort_descending: view.sort_descending,
        columns: view.columns.clone(),
        group_by: view.group_by.as_ref().map(|g| BaseViewGroupBy {
            property: g.property.clone(),
            descending: g.descending,
        }),
    }
}

// ── Evaluate from string helper ───────────────────────────────────────────────

fn evaluate_base_from_yaml(
    paths: &VaultPaths,
    normalized: &str,
    yaml: &str,
) -> Result<BasesEvalReport, BasesError> {
    let parsed = parse_base_file(yaml)?;
    let ParsedBaseFile {
        source: parsed_source,
        filters: base_filters,
        property_display_names,
        views: parsed_views,
        diagnostics: parsed_diagnostics,
    } = parsed;
    let mut diagnostics = parsed_diagnostics;
    let mut views = Vec::new();

    if parsed_source != "notes" {
        diagnostics.push(BasesDiagnostic {
            path: Some("source".to_string()),
            message: "unsupported base source; only `notes` is implemented".to_string(),
        });
    }

    for view in parsed_views {
        if let Some(evaluated_view) = evaluate_base_view(
            paths,
            &base_filters,
            &property_display_names,
            view,
            &mut diagnostics,
        )? {
            views.push(evaluated_view);
        }
    }

    Ok(BasesEvalReport {
        file: normalized.to_string(),
        views,
        diagnostics,
    })
}

// ── Mutation functions ────────────────────────────────────────────────────────

pub fn bases_view_add(
    paths: &VaultPaths,
    relative_path: &str,
    spec: BaseViewSpec,
    dry_run: bool,
) -> Result<BasesViewEditReport, BasesError> {
    let normalized = normalize_base_path(relative_path)?;
    let file_path = paths.vault_root().join(&normalized);
    let source = fs::read_to_string(&file_path)?;
    let mut parsed = parse_base_file(&source)?;

    let view_name = spec.name.clone().unwrap_or_else(|| "(unnamed)".to_string());
    let action = format!("Added view '{view_name}' to {normalized}");
    parsed.views.push(spec_to_parsed_view(spec));

    let yaml = serialize_base_file(&parsed)?;

    if !dry_run {
        let _lock = crate::write_lock::acquire_write_lock(paths).map_err(BasesError::Io)?;
        fs::write(&file_path, &yaml)?;
    }

    let eval = evaluate_base_from_yaml(paths, &normalized, &yaml)?;
    Ok(BasesViewEditReport {
        file: normalized,
        action,
        dry_run,
        eval,
    })
}

pub fn bases_view_delete(
    paths: &VaultPaths,
    relative_path: &str,
    name: &str,
    dry_run: bool,
) -> Result<BasesViewEditReport, BasesError> {
    let normalized = normalize_base_path(relative_path)?;
    let file_path = paths.vault_root().join(&normalized);
    let source = fs::read_to_string(&file_path)?;
    let mut parsed = parse_base_file(&source)?;

    let pos = parsed
        .views
        .iter()
        .position(|v| v.name.as_deref() == Some(name))
        .ok_or_else(|| BasesError::Io(std::io::Error::other(format!("view not found: {name}"))))?;

    parsed.views.remove(pos);
    let action = format!("Deleted view '{name}' from {normalized}");
    let yaml = serialize_base_file(&parsed)?;

    if !dry_run {
        let _lock = crate::write_lock::acquire_write_lock(paths).map_err(BasesError::Io)?;
        fs::write(&file_path, &yaml)?;
    }

    let eval = evaluate_base_from_yaml(paths, &normalized, &yaml)?;
    Ok(BasesViewEditReport {
        file: normalized,
        action,
        dry_run,
        eval,
    })
}

pub fn bases_view_rename(
    paths: &VaultPaths,
    relative_path: &str,
    old_name: &str,
    new_name: &str,
    dry_run: bool,
) -> Result<BasesViewEditReport, BasesError> {
    let normalized = normalize_base_path(relative_path)?;
    let file_path = paths.vault_root().join(&normalized);
    let source = fs::read_to_string(&file_path)?;
    let mut parsed = parse_base_file(&source)?;

    let view = parsed
        .views
        .iter_mut()
        .find(|v| v.name.as_deref() == Some(old_name))
        .ok_or_else(|| {
            BasesError::Io(std::io::Error::other(format!("view not found: {old_name}")))
        })?;

    view.name = Some(new_name.to_string());
    let action = format!("Renamed view '{old_name}' to '{new_name}' in {normalized}");
    let yaml = serialize_base_file(&parsed)?;

    if !dry_run {
        let _lock = crate::write_lock::acquire_write_lock(paths).map_err(BasesError::Io)?;
        fs::write(&file_path, &yaml)?;
    }

    let eval = evaluate_base_from_yaml(paths, &normalized, &yaml)?;
    Ok(BasesViewEditReport {
        file: normalized,
        action,
        dry_run,
        eval,
    })
}

pub fn bases_view_edit(
    paths: &VaultPaths,
    relative_path: &str,
    name: &str,
    patch: BaseViewPatch,
    dry_run: bool,
) -> Result<BasesViewEditReport, BasesError> {
    let normalized = normalize_base_path(relative_path)?;
    let file_path = paths.vault_root().join(&normalized);
    let source = fs::read_to_string(&file_path)?;
    let mut parsed = parse_base_file(&source)?;

    let view = parsed
        .views
        .iter_mut()
        .find(|v| v.name.as_deref() == Some(name))
        .ok_or_else(|| BasesError::Io(std::io::Error::other(format!("view not found: {name}"))))?;

    if let Some(filters) = patch.set_filters {
        view.filters = filters;
    }
    if !patch.add_filters.is_empty() {
        view.filters.extend(patch.add_filters);
    }
    if !patch.remove_filters.is_empty() {
        view.filters.retain(|f| !patch.remove_filters.contains(f));
    }
    if let Some(columns) = patch.set_columns {
        view.columns = columns;
    }
    if let Some(sort) = patch.set_sort {
        view.sort_by = sort;
    }
    if let Some(sort_desc) = patch.set_sort_descending {
        view.sort_descending = sort_desc;
    }
    if let Some(group_by) = patch.set_group_by {
        view.group_by = group_by.map(|g| ParsedBaseGroupBy {
            property: g.property,
            descending: g.descending,
        });
    }

    let action = format!("Edited view '{name}' in {normalized}");
    let yaml = serialize_base_file(&parsed)?;

    if !dry_run {
        let _lock = crate::write_lock::acquire_write_lock(paths).map_err(BasesError::Io)?;
        fs::write(&file_path, &yaml)?;
    }

    let eval = evaluate_base_from_yaml(paths, &normalized, &yaml)?;
    Ok(BasesViewEditReport {
        file: normalized,
        action,
        dry_run,
        eval,
    })
}

pub fn evaluate_base_file(
    paths: &VaultPaths,
    relative_path: &str,
) -> Result<BasesEvalReport, BasesError> {
    let normalized = normalize_base_path(relative_path)?;
    let source = fs::read_to_string(paths.vault_root().join(&normalized))?;
    let parsed = parse_base_file(&source)?;
    let ParsedBaseFile {
        source: parsed_source,
        filters: base_filters,
        property_display_names,
        views: parsed_views,
        diagnostics: parsed_diagnostics,
    } = parsed;
    let mut diagnostics = parsed_diagnostics;
    let mut views = Vec::new();

    if parsed_source != "notes" {
        diagnostics.push(BasesDiagnostic {
            path: Some("source".to_string()),
            message: "unsupported base source; only `notes` is implemented".to_string(),
        });
    }

    for view in parsed_views {
        if let Some(evaluated_view) = evaluate_base_view(
            paths,
            &base_filters,
            &property_display_names,
            view,
            &mut diagnostics,
        )? {
            views.push(evaluated_view);
        }
    }

    Ok(BasesEvalReport {
        file: normalized,
        views,
        diagnostics,
    })
}

fn evaluate_base_view(
    paths: &VaultPaths,
    base_filters: &[String],
    property_display_names: &BTreeMap<String, String>,
    view: ParsedBaseView,
    diagnostics: &mut Vec<BasesDiagnostic>,
) -> Result<Option<BasesEvaluatedView>, BasesError> {
    let view_filters = combined_filters(base_filters, &view.filters);
    if view.view_type != "table" {
        diagnostics.push(BasesDiagnostic {
            path: view.name.as_ref().map(|name| format!("views.{name}.type")),
            message: format!("unsupported view type `{}`", view.view_type),
        });
        return Ok(None);
    }

    let notes = match query_notes(
        paths,
        &NoteQuery {
            filters: view_filters.clone(),
            sort_by: view.sort_by.clone(),
            sort_descending: view.sort_descending,
        },
    ) {
        Ok(report) => report.notes,
        Err(PropertyError::InvalidFilter(filter)) => {
            diagnostics.push(BasesDiagnostic {
                path: view
                    .name
                    .as_ref()
                    .map(|name| format!("views.{name}.filters")),
                message: format!("unsupported filter in base view: {filter}"),
            });
            return Ok(None);
        }
        Err(error) => return Err(BasesError::Property(error)),
    };

    let columns = build_view_columns(property_display_names, &view);
    let mut rows = Vec::new();
    for note in notes {
        let formulas = evaluate_formulas(&note, &view.formulas, diagnostics, view.name.as_deref());
        let cells = columns
            .iter()
            .map(|column| {
                (
                    column.key.clone(),
                    evaluate_base_cell(&note, &formulas, &column.key),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let group_value = view
            .group_by
            .as_ref()
            .map(|group_by| evaluate_base_cell(&note, &formulas, &group_by.property));
        rows.push(BasesRow {
            document_path: note.document_path.clone(),
            file_name: note.file_name.clone(),
            file_ext: note.file_ext.clone(),
            file_mtime: note.file_mtime,
            properties: note.properties.clone(),
            formulas,
            cells,
            group_value,
        });
    }
    sort_base_rows(&mut rows, &view);
    let ParsedBaseView {
        name,
        view_type,
        filters: _,
        sort_by,
        sort_descending,
        columns: _,
        group_by,
        formulas: _,
    } = view;

    Ok(Some(BasesEvaluatedView {
        name,
        view_type,
        filters: view_filters,
        sort_by,
        sort_descending,
        columns,
        group_by: group_by.map(|group_by| BasesGroupBy {
            display_name: column_display_name(&group_by.property, property_display_names),
            property: group_by.property,
            descending: group_by.descending,
        }),
        rows,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedBaseFile {
    source: String,
    filters: Vec<String>,
    property_display_names: BTreeMap<String, String>,
    views: Vec<ParsedBaseView>,
    diagnostics: Vec<BasesDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedBaseView {
    name: Option<String>,
    view_type: String,
    filters: Vec<String>,
    sort_by: Option<String>,
    sort_descending: bool,
    columns: Vec<String>,
    group_by: Option<ParsedBaseGroupBy>,
    formulas: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedBaseGroupBy {
    property: String,
    descending: bool,
}

fn parse_base_file(source: &str) -> Result<ParsedBaseFile, BasesError> {
    let value = serde_yaml::from_str::<serde_yaml::Value>(source)?;
    let mut diagnostics = Vec::new();
    let Some(root) = value.as_mapping() else {
        return Ok(ParsedBaseFile {
            source: "notes".to_string(),
            filters: Vec::new(),
            property_display_names: BTreeMap::new(),
            views: Vec::new(),
            diagnostics: vec![BasesDiagnostic {
                path: None,
                message: "base file must be a YAML object".to_string(),
            }],
        });
    };

    let source = root
        .get(serde_yaml::Value::String("source".to_string()))
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or("notes")
        .to_string();
    let filters = root
        .get(serde_yaml::Value::String("filters".to_string()))
        .map_or_else(Vec::new, |value| {
            parse_base_filters("filters", value, &mut diagnostics)
        });
    let property_display_names = root
        .get(serde_yaml::Value::String("properties".to_string()))
        .map_or_else(BTreeMap::new, |value| {
            parse_property_display_names(value, &mut diagnostics)
        });
    let mut views = root
        .get(serde_yaml::Value::String("views".to_string()))
        .and_then(serde_yaml::Value::as_sequence)
        .map_or_else(Vec::new, |entries| {
            entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| parse_view(index, entry, &mut diagnostics))
                .collect()
        });

    let base_formulas = root
        .get(serde_yaml::Value::String("formulas".to_string()))
        .map_or_else(BTreeMap::new, |value| {
            parse_top_level_formulas(value, &mut diagnostics)
        });

    // Merge top-level formulas into views (view formulas take precedence)
    for view in &mut views {
        for (key, expression) in &base_formulas {
            view.formulas.entry(key.clone()).or_insert_with(|| expression.clone());
        }
    }

    for key in root.keys().filter_map(serde_yaml::Value::as_str) {
        if !matches!(key, "source" | "views" | "filters" | "properties" | "formulas") {
            diagnostics.push(BasesDiagnostic {
                path: Some(key.to_string()),
                message: format!("unsupported top-level base field `{key}`"),
            });
        }
    }

    Ok(ParsedBaseFile {
        source,
        filters,
        property_display_names,
        views,
        diagnostics,
    })
}

fn parse_view(
    index: usize,
    entry: &serde_yaml::Value,
    diagnostics: &mut Vec<BasesDiagnostic>,
) -> Option<ParsedBaseView> {
    let Some(mapping) = entry.as_mapping() else {
        diagnostics.push(BasesDiagnostic {
            path: Some(format!("views[{index}]")),
            message: "view entry must be a YAML object".to_string(),
        });
        return None;
    };

    let name = mapping
        .get(serde_yaml::Value::String("name".to_string()))
        .and_then(serde_yaml::Value::as_str)
        .map(ToOwned::to_owned);
    let view_type = mapping
        .get(serde_yaml::Value::String("type".to_string()))
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or("table")
        .to_string();
    let filters = parse_view_filters(index, mapping, diagnostics);
    let (sort_by, sort_descending) = parse_view_sort(index, mapping, diagnostics);
    let columns = parse_view_columns(index, mapping, diagnostics);
    let group_by = parse_view_group_by(index, mapping, diagnostics);
    let formulas = parse_view_formulas(index, mapping, diagnostics);

    for key in mapping.keys().filter_map(serde_yaml::Value::as_str) {
        if !matches!(
            key,
            "name" | "type" | "filters" | "sort" | "formulas" | "order" | "groupBy"
                | "columnSize"
        ) {
            diagnostics.push(BasesDiagnostic {
                path: Some(format!("views[{index}].{key}")),
                message: format!("unsupported view field `{key}`"),
            });
        }
    }

    Some(ParsedBaseView {
        name,
        view_type,
        filters,
        sort_by,
        sort_descending,
        columns,
        group_by,
        formulas,
    })
}

fn parse_base_filters(
    path: &str,
    value: &serde_yaml::Value,
    diagnostics: &mut Vec<BasesDiagnostic>,
) -> Vec<String> {
    if let Some(sequence) = value.as_sequence() {
        return sequence
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                parse_base_filter_entry(&format!("{path}[{index}]"), entry, diagnostics)
            })
            .collect();
    }

    if let Some(mapping) = value.as_mapping() {
        if let Some(and_filters) = mapping.get(serde_yaml::Value::String("and".to_string())) {
            return parse_base_filters(&format!("{path}.and"), and_filters, diagnostics);
        }
    }

    diagnostics.push(BasesDiagnostic {
        path: Some(path.to_string()),
        message: "filters must be a list or an `and:` group".to_string(),
    });
    Vec::new()
}

fn parse_base_filter_entry(
    path: &str,
    value: &serde_yaml::Value,
    diagnostics: &mut Vec<BasesDiagnostic>,
) -> Option<String> {
    let expression = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    translate_base_filter_expression(expression).or_else(|| {
        diagnostics.push(BasesDiagnostic {
            path: Some(path.to_string()),
            message: format!("unsupported base filter expression `{expression}`"),
        });
        None
    })
}

fn translate_base_filter_expression(expression: &str) -> Option<String> {
    if let Some(folder) = parse_in_folder_expression(expression) {
        let prefix = folder.trim_end_matches('/');
        return Some(format!("file.path starts_with \"{prefix}/\""));
    }

    if let Some(tag) = parse_has_tag_expression(expression) {
        return Some(format!("tags has_tag \"{tag}\""));
    }

    if let Some((field, value)) = expression.split_once("!=") {
        return Some(format!("{} != {}", field.trim(), value.trim()));
    }

    if let Some((field, value)) = expression.split_once("==") {
        return Some(format!("{} = {}", field.trim(), value.trim()));
    }

    if expression.contains(" starts_with ")
        || expression.contains(" contains ")
        || expression.contains(" >= ")
        || expression.contains(" <= ")
        || expression.contains(" != ")
        || expression.contains(" = ")
        || expression.contains(" > ")
        || expression.contains(" < ")
    {
        return Some(expression.to_string());
    }

    None
}

fn parse_in_folder_expression(expression: &str) -> Option<String> {
    let trimmed = expression.trim();
    let argument = trimmed
        .strip_prefix("file.inFolder(")?
        .strip_suffix(')')?
        .trim();
    strip_matching_quotes(argument).map(ToOwned::to_owned)
}

fn parse_has_tag_expression(expression: &str) -> Option<String> {
    let trimmed = expression.trim();
    let argument = trimmed
        .strip_prefix("file.hasTag(")?
        .strip_suffix(')')?
        .trim();
    strip_matching_quotes(argument).map(ToOwned::to_owned)
}

fn strip_matching_quotes(value: &str) -> Option<&str> {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        Some(&value[1..value.len() - 1])
    } else {
        None
    }
}

fn parse_property_display_names(
    value: &serde_yaml::Value,
    diagnostics: &mut Vec<BasesDiagnostic>,
) -> BTreeMap<String, String> {
    let Some(mapping) = value.as_mapping() else {
        diagnostics.push(BasesDiagnostic {
            path: Some("properties".to_string()),
            message: "properties must be a mapping".to_string(),
        });
        return BTreeMap::new();
    };

    let mut names = BTreeMap::new();
    for (key, value) in mapping {
        let Some(key) = key.as_str() else {
            diagnostics.push(BasesDiagnostic {
                path: Some("properties".to_string()),
                message: "property keys must be strings".to_string(),
            });
            continue;
        };
        let Some(definition) = value.as_mapping() else {
            diagnostics.push(BasesDiagnostic {
                path: Some(format!("properties.{key}")),
                message: "property definitions must be objects".to_string(),
            });
            continue;
        };
        let display_name = definition
            .get(serde_yaml::Value::String("displayName".to_string()))
            .and_then(serde_yaml::Value::as_str)
            .map_or_else(|| key.to_string(), ToOwned::to_owned);
        names.insert(key.to_string(), display_name);
        for field in definition.keys().filter_map(serde_yaml::Value::as_str) {
            if field != "displayName" {
                diagnostics.push(BasesDiagnostic {
                    path: Some(format!("properties.{key}.{field}")),
                    message: format!("unsupported property field `{field}`"),
                });
            }
        }
    }

    names
}

fn parse_view_filters(
    index: usize,
    mapping: &serde_yaml::Mapping,
    diagnostics: &mut Vec<BasesDiagnostic>,
) -> Vec<String> {
    let Some(filters) = mapping.get(serde_yaml::Value::String("filters".to_string())) else {
        return Vec::new();
    };
    parse_base_filters(&format!("views[{index}].filters"), filters, diagnostics)
}

fn parse_view_sort(
    index: usize,
    mapping: &serde_yaml::Mapping,
    diagnostics: &mut Vec<BasesDiagnostic>,
) -> (Option<String>, bool) {
    let Some(sort) = mapping.get(serde_yaml::Value::String("sort".to_string())) else {
        return (None, false);
    };

    if let Some(sort_by) = sort.as_str() {
        return (Some(sort_by.to_string()), false);
    }

    // Support Obsidian's list format: [{property: "field", direction: "DESC"}]
    if let Some(sequence) = sort.as_sequence() {
        if let Some(first) = sequence.first().and_then(serde_yaml::Value::as_mapping) {
            let sort_by = first
                .get(serde_yaml::Value::String("property".to_string()))
                .and_then(serde_yaml::Value::as_str)
                .map(ToOwned::to_owned);
            let sort_descending = first
                .get(serde_yaml::Value::String("direction".to_string()))
                .and_then(serde_yaml::Value::as_str)
                .map(|direction| direction.eq_ignore_ascii_case("desc"))
                .unwrap_or(false);
            if sort_by.is_none() {
                diagnostics.push(BasesDiagnostic {
                    path: Some(format!("views[{index}].sort[0].property")),
                    message: "sort[0].property must be a string".to_string(),
                });
            }
            return (sort_by, sort_descending);
        }
    }

    let Some(sort) = sort.as_mapping() else {
        diagnostics.push(BasesDiagnostic {
            path: Some(format!("views[{index}].sort")),
            message: "sort must be a string, list, or object with `by`/`desc`".to_string(),
        });
        return (None, false);
    };

    let sort_by = sort
        .get(serde_yaml::Value::String("by".to_string()))
        .or_else(|| sort.get(serde_yaml::Value::String("property".to_string())))
        .and_then(serde_yaml::Value::as_str)
        .map(ToOwned::to_owned);
    let sort_descending = sort
        .get(serde_yaml::Value::String("desc".to_string()))
        .and_then(serde_yaml::Value::as_bool)
        .unwrap_or_else(|| {
            sort.get(serde_yaml::Value::String("direction".to_string()))
                .and_then(serde_yaml::Value::as_str)
                .map(|direction| direction.eq_ignore_ascii_case("desc"))
                .unwrap_or(false)
        });
    if sort_by.is_none() {
        diagnostics.push(BasesDiagnostic {
            path: Some(format!("views[{index}].sort.by")),
            message: "sort.by must be a string".to_string(),
        });
    }

    (sort_by, sort_descending)
}

fn parse_view_columns(
    index: usize,
    mapping: &serde_yaml::Mapping,
    diagnostics: &mut Vec<BasesDiagnostic>,
) -> Vec<String> {
    let Some(order) = mapping.get(serde_yaml::Value::String("order".to_string())) else {
        return Vec::new();
    };
    let Some(order) = order.as_sequence() else {
        diagnostics.push(BasesDiagnostic {
            path: Some(format!("views[{index}].order")),
            message: "order must be a list of field keys".to_string(),
        });
        return Vec::new();
    };

    order
        .iter()
        .enumerate()
        .filter_map(|(column_index, column)| {
            column.as_str().map(ToOwned::to_owned).or_else(|| {
                diagnostics.push(BasesDiagnostic {
                    path: Some(format!("views[{index}].order[{column_index}]")),
                    message: "order entries must be strings".to_string(),
                });
                None
            })
        })
        .collect()
}

fn parse_view_group_by(
    index: usize,
    mapping: &serde_yaml::Mapping,
    diagnostics: &mut Vec<BasesDiagnostic>,
) -> Option<ParsedBaseGroupBy> {
    let group_by = mapping.get(serde_yaml::Value::String("groupBy".to_string()))?;
    let Some(group_by) = group_by.as_mapping() else {
        diagnostics.push(BasesDiagnostic {
            path: Some(format!("views[{index}].groupBy")),
            message: "groupBy must be an object with `property` and optional `direction`"
                .to_string(),
        });
        return None;
    };

    let property = group_by
        .get(serde_yaml::Value::String("property".to_string()))
        .and_then(serde_yaml::Value::as_str)
        .map(ToOwned::to_owned);
    let descending = group_by
        .get(serde_yaml::Value::String("direction".to_string()))
        .and_then(serde_yaml::Value::as_str)
        .is_some_and(|direction| direction.eq_ignore_ascii_case("desc"));
    if property.is_none() {
        diagnostics.push(BasesDiagnostic {
            path: Some(format!("views[{index}].groupBy.property")),
            message: "groupBy.property must be a string".to_string(),
        });
    }

    property.map(|property| ParsedBaseGroupBy {
        property,
        descending,
    })
}

fn parse_top_level_formulas(
    value: &serde_yaml::Value,
    diagnostics: &mut Vec<BasesDiagnostic>,
) -> BTreeMap<String, String> {
    let Some(formulas) = value.as_mapping() else {
        diagnostics.push(BasesDiagnostic {
            path: Some("formulas".to_string()),
            message: "formulas must be a mapping of name -> expression".to_string(),
        });
        return BTreeMap::new();
    };

    formulas
        .iter()
        .filter_map(|(key, value)| {
            let Some(key) = key.as_str() else {
                diagnostics.push(BasesDiagnostic {
                    path: Some("formulas".to_string()),
                    message: "formula names must be strings".to_string(),
                });
                return None;
            };
            value
                .as_str()
                .map(|expression| (key.to_string(), expression.to_string()))
                .or_else(|| {
                    diagnostics.push(BasesDiagnostic {
                        path: Some(format!("formulas.{key}")),
                        message: "formula expressions must be strings".to_string(),
                    });
                    None
                })
        })
        .collect()
}

fn parse_view_formulas(
    index: usize,
    mapping: &serde_yaml::Mapping,
    diagnostics: &mut Vec<BasesDiagnostic>,
) -> BTreeMap<String, String> {
    let Some(formulas) = mapping.get(serde_yaml::Value::String("formulas".to_string())) else {
        return BTreeMap::new();
    };
    let Some(formulas) = formulas.as_mapping() else {
        diagnostics.push(BasesDiagnostic {
            path: Some(format!("views[{index}].formulas")),
            message: "formulas must be a mapping of name -> expression".to_string(),
        });
        return BTreeMap::new();
    };

    formulas
        .iter()
        .filter_map(|(key, value)| {
            let Some(key) = key.as_str() else {
                diagnostics.push(BasesDiagnostic {
                    path: Some(format!("views[{index}].formulas")),
                    message: "formula names must be strings".to_string(),
                });
                return None;
            };
            value
                .as_str()
                .map(|expression| (key.to_string(), expression.to_string()))
                .or_else(|| {
                    diagnostics.push(BasesDiagnostic {
                        path: Some(format!("views[{index}].formulas.{key}")),
                        message: "formula expressions must be strings".to_string(),
                    });
                    None
                })
        })
        .collect()
}

fn evaluate_formulas(
    note: &NoteRecord,
    formulas: &BTreeMap<String, String>,
    diagnostics: &mut Vec<BasesDiagnostic>,
    view_name: Option<&str>,
) -> BTreeMap<String, Value> {
    let mut evaluated = BTreeMap::new();

    for (name, expression) in formulas {
        match parse_expression(expression) {
            Ok(ast) => {
                let ctx = EvalContext::new(note, &evaluated);
                match crate::expression::eval::evaluate(&ast, &ctx) {
                    Ok(value) => {
                        evaluated.insert(name.clone(), value);
                    }
                    Err(err) => {
                        let diagnostic = BasesDiagnostic {
                            path: Some(match view_name {
                                Some(view_name) => {
                                    format!("views.{view_name}.formulas.{name}")
                                }
                                None => format!("formulas.{name}"),
                            }),
                            message: format!("formula evaluation error: {err}"),
                        };
                        if !diagnostics.contains(&diagnostic) {
                            diagnostics.push(diagnostic);
                        }
                    }
                }
            }
            Err(err) => {
                let diagnostic = BasesDiagnostic {
                    path: Some(match view_name {
                        Some(view_name) => format!("views.{view_name}.formulas.{name}"),
                        None => format!("formulas.{name}"),
                    }),
                    message: format!("formula parse error: {err}"),
                };
                if !diagnostics.contains(&diagnostic) {
                    diagnostics.push(diagnostic);
                }
            }
        }
    }

    evaluated
}

fn combined_filters(base_filters: &[String], view_filters: &[String]) -> Vec<String> {
    base_filters
        .iter()
        .chain(view_filters.iter())
        .cloned()
        .collect()
}

fn build_view_columns(
    property_display_names: &BTreeMap<String, String>,
    view: &ParsedBaseView,
) -> Vec<BasesColumn> {
    let column_keys = if view.columns.is_empty() {
        let mut keys = vec!["file.name".to_string()];
        if let Some(group_by) = view.group_by.as_ref() {
            if !keys.contains(&group_by.property) {
                keys.push(group_by.property.clone());
            }
        }
        if let Some(sort_by) = view.sort_by.as_ref() {
            if !keys.contains(sort_by) {
                keys.push(sort_by.clone());
            }
        }
        for key in view.formulas.keys() {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
        keys
    } else {
        view.columns.clone()
    };

    column_keys
        .into_iter()
        .map(|key| BasesColumn {
            display_name: column_display_name(&key, property_display_names),
            key,
        })
        .collect()
}

fn column_display_name(key: &str, property_display_names: &BTreeMap<String, String>) -> String {
    if let Some(display_name) = property_display_names.get(key) {
        return display_name.clone();
    }

    match key {
        "file.name" => "Name".to_string(),
        "file.path" => "Path".to_string(),
        "file.ext" => "Extension".to_string(),
        "file.mtime" => "Modified".to_string(),
        _ => key.to_string(),
    }
}

fn evaluate_base_cell(note: &NoteRecord, formulas: &BTreeMap<String, Value>, key: &str) -> Value {
    if let Some(value) = formulas.get(key) {
        return value.clone();
    }

    match parse_expression(key) {
        Ok(ast) => {
            let ctx = EvalContext::new(note, formulas);
            crate::expression::eval::evaluate(&ast, &ctx).unwrap_or(Value::Null)
        }
        Err(_) => Value::Null,
    }
}

fn sort_base_rows(rows: &mut [BasesRow], view: &ParsedBaseView) {
    let default_sort = view.columns.first().map(String::as_str);
    rows.sort_by(|left, right| {
        let group_ordering = view.group_by.as_ref().map_or(Ordering::Equal, |group_by| {
            let ordering = compare_json_values(
                left.group_value.as_ref().unwrap_or(&Value::Null),
                right.group_value.as_ref().unwrap_or(&Value::Null),
            );
            if group_by.descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        if group_ordering != Ordering::Equal {
            return group_ordering.then_with(|| left.document_path.cmp(&right.document_path));
        }

        let sort_ordering =
            view.sort_by
                .as_deref()
                .or(default_sort)
                .map_or(Ordering::Equal, |sort_by| {
                    compare_json_values(
                        &lookup_row_value(left, sort_by),
                        &lookup_row_value(right, sort_by),
                    )
                });
        let sort_ordering = if view.sort_descending {
            sort_ordering.reverse()
        } else {
            sort_ordering
        };

        sort_ordering.then_with(|| left.document_path.cmp(&right.document_path))
    });
}

fn lookup_row_value(row: &BasesRow, key: &str) -> Value {
    if let Some(value) = row.cells.get(key) {
        return value.clone();
    }
    if let Some(value) = row.formulas.get(key) {
        return value.clone();
    }

    match key {
        "file.path" => Value::String(row.document_path.clone()),
        "file.name" => Value::String(row.file_name.clone()),
        "file.ext" => Value::String(row.file_ext.clone()),
        "file.mtime" => Value::Number(row.file_mtime.into()),
        property => row.properties.get(property).cloned().unwrap_or(Value::Null),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum JsonSortKey {
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
}

fn compare_json_values(left: &Value, right: &Value) -> Ordering {
    let left_key = json_sort_key(left);
    let right_key = json_sort_key(right);
    json_sort_rank(&left_key)
        .cmp(&json_sort_rank(&right_key))
        .then_with(|| match (&left_key, &right_key) {
            (JsonSortKey::Bool(left), JsonSortKey::Bool(right)) => left.cmp(right),
            (JsonSortKey::Number(left), JsonSortKey::Number(right)) => {
                left.partial_cmp(right).unwrap_or(Ordering::Equal)
            }
            (JsonSortKey::Text(left), JsonSortKey::Text(right)) => left.cmp(right),
            _ => Ordering::Equal,
        })
}

fn json_sort_key(value: &Value) -> JsonSortKey {
    match value {
        Value::Null => JsonSortKey::Null,
        Value::Bool(value_bool) => JsonSortKey::Bool(*value_bool),
        Value::Number(value_number) => JsonSortKey::Number(value_number.as_f64().unwrap_or(0.0)),
        Value::String(value_text) => JsonSortKey::Text(value_text.clone()),
        Value::Array(_) | Value::Object(_) => JsonSortKey::Text(value.to_string()),
    }
}

fn json_sort_rank(value: &JsonSortKey) -> u8 {
    match value {
        JsonSortKey::Null => 0,
        JsonSortKey::Bool(_) => 1,
        JsonSortKey::Number(_) => 2,
        JsonSortKey::Text(_) => 3,
    }
}

fn normalize_base_path(path: &str) -> Result<String, BasesError> {
    normalize_relative_input_path(
        path,
        RelativePathOptions {
            expected_extension: Some("base"),
            append_extension_if_missing: false,
        },
    )
    .map_err(BasesError::InvalidPath)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scan_vault, ScanMode};
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn parser_accepts_real_world_base_fields() {
        let report = parse_base_file(
            "
            filters:
              and:
                - file.inFolder(\"Rules/Gear\")
                - 'file.ext == \"md\"'
            properties:
              category:
                displayName: Category
            views:
              - name: Gear Table
                type: table
                order:
                  - file.name
                  - category
                groupBy:
                  property: category
                  direction: ASC
            ",
        )
        .expect("base parse should succeed");

        assert_eq!(
            report.filters,
            vec![
                "file.path starts_with \"Rules/Gear/\"".to_string(),
                "file.ext = \"md\"".to_string()
            ]
        );
        assert_eq!(
            report.property_display_names.get("category"),
            Some(&"Category".to_string())
        );
        assert_eq!(report.views.len(), 1);
        assert_eq!(
            report.views[0].columns,
            vec!["file.name".to_string(), "category".to_string()]
        );
        assert_eq!(
            report.views[0].group_by,
            Some(ParsedBaseGroupBy {
                property: "category".to_string(),
                descending: false,
            })
        );
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn evaluates_supported_bases_view_and_reports_unsupported_features() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("bases", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = evaluate_base_file(&paths, "release.base").expect("base eval should succeed");

        assert_eq!(report.views.len(), 1);
        assert_eq!(report.views[0].rows.len(), 1);
        assert_eq!(
            report.views[0].filters,
            vec![
                "file.ext = \"md\"".to_string(),
                "status starts_with \"b\"".to_string(),
                "estimate > 2".to_string()
            ]
        );
        assert_eq!(
            report.views[0]
                .columns
                .iter()
                .map(|column| column.key.as_str())
                .collect::<Vec<_>>(),
            vec!["file.name", "due", "note_name"]
        );
        assert_eq!(
            report.views[0].group_by,
            Some(BasesGroupBy {
                property: "status".to_string(),
                display_name: "Status".to_string(),
                descending: false,
            })
        );
        assert_eq!(report.views[0].rows[0].document_path, "Backlog.md");
        assert_eq!(
            report.views[0].rows[0].group_value,
            Some(Value::String("backlog".to_string()))
        );
        assert_eq!(
            report.views[0].rows[0].formulas.get("note_name"),
            Some(&Value::String("Backlog".to_string()))
        );
        assert_eq!(
            report.views[0].rows[0].cells.get("due"),
            Some(&Value::String("2026-04-01".to_string()))
        );
        assert!(report.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("unknown function")));
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unsupported view type `board`")));
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

    #[test]
    fn serialize_and_round_trip_empty_file() {
        let minimal = "views:\n  - name: Simple\n    type: table\n";
        let parsed = parse_base_file(minimal).expect("parse should succeed");
        let yaml = serialize_base_file(&parsed).expect("serialize should succeed");
        let re_parsed = parse_base_file(&yaml).expect("re-parse should succeed");

        assert_eq!(parsed.source, re_parsed.source);
        assert_eq!(parsed.filters, re_parsed.filters);
        assert_eq!(parsed.views.len(), re_parsed.views.len());
        assert_eq!(parsed.views[0].name, re_parsed.views[0].name);
        assert_eq!(parsed.views[0].view_type, re_parsed.views[0].view_type);
    }

    #[test]
    fn serialize_and_round_trip_full_file() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults/bases/release.base");
        let source = fs::read_to_string(&fixture).expect("fixture should be readable");
        let parsed = parse_base_file(&source).expect("parse should succeed");
        let yaml = serialize_base_file(&parsed).expect("serialize should succeed");
        let re_parsed = parse_base_file(&yaml).expect("re-parse should succeed");

        assert_eq!(parsed.views.len(), re_parsed.views.len());
        for (original, roundtripped) in parsed.views.iter().zip(re_parsed.views.iter()) {
            assert_eq!(original.name, roundtripped.name);
            assert_eq!(original.filters, roundtripped.filters);
            assert_eq!(original.columns, roundtripped.columns);
            assert_eq!(original.group_by, roundtripped.group_by);
        }
        assert_eq!(parsed.filters, re_parsed.filters);
    }

    #[test]
    fn bases_view_add_creates_new_view() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("bases", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let spec = BaseViewSpec {
            name: Some("Sprint".to_string()),
            view_type: "table".to_string(),
            filters: vec!["status = backlog".to_string()],
            ..Default::default()
        };
        let report =
            bases_view_add(&paths, "release.base", spec, false).expect("view add should succeed");

        // The new view is evaluated
        assert!(report
            .eval
            .views
            .iter()
            .any(|v| v.name.as_deref() == Some("Sprint")));

        // The file on disk was written
        let on_disk =
            fs::read_to_string(vault_root.join("release.base")).expect("file should be readable");
        assert!(on_disk.contains("Sprint"));
    }

    #[test]
    fn bases_view_delete_removes_named_view() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("bases", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let report = bases_view_delete(&paths, "release.base", "Board", false)
            .expect("view delete should succeed");

        let on_disk =
            fs::read_to_string(vault_root.join("release.base")).expect("file should be readable");
        assert!(
            !on_disk.contains("Board"),
            "Board view should be removed from the file"
        );
        // Release Table view should still be in the eval (it's valid table type)
        assert!(
            report
                .eval
                .views
                .iter()
                .any(|v| v.name.as_deref() == Some("Release Table")),
            "Release Table should still exist"
        );
    }

    #[test]
    fn bases_view_rename_changes_view_name() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("bases", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let report = bases_view_rename(&paths, "release.base", "Release Table", "New Name", false)
            .expect("view rename should succeed");

        let on_disk =
            fs::read_to_string(vault_root.join("release.base")).expect("file should be readable");
        assert!(
            on_disk.contains("New Name"),
            "new name should appear in file"
        );
        assert!(
            !on_disk.contains("Release Table"),
            "old name should be gone"
        );
        assert!(
            report
                .eval
                .views
                .iter()
                .any(|v| v.name.as_deref() == Some("New Name")),
            "renamed view should appear in eval"
        );
    }

    #[test]
    fn bases_view_edit_patches_view_filters() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("bases", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let patch = BaseViewPatch {
            add_filters: vec!["reviewed = true".to_string()],
            ..Default::default()
        };
        let _report = bases_view_edit(&paths, "release.base", "Release Table", patch, false)
            .expect("view edit should succeed");

        let on_disk =
            fs::read_to_string(vault_root.join("release.base")).expect("file should be readable");
        assert!(
            on_disk.contains("reviewed = true"),
            "added filter should appear in file"
        );
    }
}
