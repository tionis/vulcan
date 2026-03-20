use crate::{query_notes, NoteQuery, NoteRecord, PropertyError, VaultPaths};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Component, Path};

#[derive(Debug)]
pub enum BasesError {
    InvalidPath(String),
    Io(std::io::Error),
    Property(PropertyError),
    Yaml(serde_yaml::Error),
}

impl Display for BasesError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath(path) => write!(formatter, "invalid base file path: {path}"),
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
            Self::InvalidPath(_) => None,
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BasesEvaluatedView {
    pub name: Option<String>,
    pub view_type: String,
    pub filters: Vec<String>,
    pub sort_by: Option<String>,
    pub sort_descending: bool,
    pub rows: Vec<BasesRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BasesRow {
    pub document_path: String,
    pub properties: Value,
    pub formulas: BTreeMap<String, Value>,
}

pub fn evaluate_base_file(
    paths: &VaultPaths,
    relative_path: &str,
) -> Result<BasesEvalReport, BasesError> {
    let normalized = normalize_base_path(relative_path)?;
    let source = fs::read_to_string(paths.vault_root().join(&normalized))?;
    let parsed = parse_base_file(&source)?;
    let mut diagnostics = parsed.diagnostics;
    let mut views = Vec::new();

    if parsed.source.as_deref() != Some("notes") {
        diagnostics.push(BasesDiagnostic {
            path: Some("source".to_string()),
            message: "unsupported base source; only `notes` is implemented".to_string(),
        });
    }

    for view in parsed.views {
        if view.view_type != "table" {
            diagnostics.push(BasesDiagnostic {
                path: view.name.as_ref().map(|name| format!("views.{name}.type")),
                message: format!("unsupported view type `{}`", view.view_type),
            });
            continue;
        }

        let notes = match query_notes(
            paths,
            &NoteQuery {
                filters: view.filters.clone(),
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
                continue;
            }
            Err(error) => return Err(BasesError::Property(error)),
        };

        let mut rows = Vec::new();
        for note in notes {
            let formulas = evaluate_formulas(
                &note,
                &view.formulas,
                &mut diagnostics,
                view.name.as_deref(),
            );
            rows.push(BasesRow {
                document_path: note.document_path.clone(),
                properties: note.properties.clone(),
                formulas,
            });
        }

        views.push(BasesEvaluatedView {
            name: view.name,
            view_type: view.view_type,
            filters: view.filters,
            sort_by: view.sort_by,
            sort_descending: view.sort_descending,
            rows,
        });
    }

    Ok(BasesEvalReport {
        file: normalized,
        views,
        diagnostics,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedBaseFile {
    source: Option<String>,
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
    formulas: BTreeMap<String, String>,
}

fn parse_base_file(source: &str) -> Result<ParsedBaseFile, BasesError> {
    let value = serde_yaml::from_str::<serde_yaml::Value>(source)?;
    let mut diagnostics = Vec::new();
    let Some(root) = value.as_mapping() else {
        return Ok(ParsedBaseFile {
            source: None,
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
        .map(ToOwned::to_owned);
    let views = root
        .get(serde_yaml::Value::String("views".to_string()))
        .and_then(serde_yaml::Value::as_sequence)
        .map_or_else(Vec::new, |entries| {
            entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| parse_view(index, entry, &mut diagnostics))
                .collect()
        });

    for key in root.keys().filter_map(serde_yaml::Value::as_str) {
        if !matches!(key, "source" | "views") {
            diagnostics.push(BasesDiagnostic {
                path: Some(key.to_string()),
                message: format!("unsupported top-level base field `{key}`"),
            });
        }
    }

    Ok(ParsedBaseFile {
        source,
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
    let formulas = parse_view_formulas(index, mapping, diagnostics);

    for key in mapping.keys().filter_map(serde_yaml::Value::as_str) {
        if !matches!(key, "name" | "type" | "filters" | "sort" | "formulas") {
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
        formulas,
    })
}

fn parse_view_filters(
    index: usize,
    mapping: &serde_yaml::Mapping,
    diagnostics: &mut Vec<BasesDiagnostic>,
) -> Vec<String> {
    let Some(filters) = mapping.get(serde_yaml::Value::String("filters".to_string())) else {
        return Vec::new();
    };
    let Some(filters) = filters.as_sequence() else {
        diagnostics.push(BasesDiagnostic {
            path: Some(format!("views[{index}].filters")),
            message: "filters must be a list of query strings".to_string(),
        });
        return Vec::new();
    };

    filters
        .iter()
        .enumerate()
        .filter_map(|(filter_index, filter)| {
            filter.as_str().map(ToOwned::to_owned).or_else(|| {
                diagnostics.push(BasesDiagnostic {
                    path: Some(format!("views[{index}].filters[{filter_index}]")),
                    message: "filter entries must be strings".to_string(),
                });
                None
            })
        })
        .collect()
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

    let Some(sort) = sort.as_mapping() else {
        diagnostics.push(BasesDiagnostic {
            path: Some(format!("views[{index}].sort")),
            message: "sort must be a string or object with `by`/`desc`".to_string(),
        });
        return (None, false);
    };

    let sort_by = sort
        .get(serde_yaml::Value::String("by".to_string()))
        .and_then(serde_yaml::Value::as_str)
        .map(ToOwned::to_owned);
    let sort_descending = sort
        .get(serde_yaml::Value::String("desc".to_string()))
        .and_then(serde_yaml::Value::as_bool)
        .unwrap_or(false);
    if sort_by.is_none() {
        diagnostics.push(BasesDiagnostic {
            path: Some(format!("views[{index}].sort.by")),
            message: "sort.by must be a string".to_string(),
        });
    }

    (sort_by, sort_descending)
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
        match evaluate_formula(note, expression) {
            Some(value) => {
                evaluated.insert(name.clone(), value);
            }
            None => diagnostics.push(BasesDiagnostic {
                path: Some(match view_name {
                    Some(view_name) => format!("views.{view_name}.formulas.{name}"),
                    None => format!("formulas.{name}"),
                }),
                message: format!("unsupported formula expression `{expression}`"),
            }),
        }
    }

    evaluated
}

fn evaluate_formula(note: &NoteRecord, expression: &str) -> Option<Value> {
    match expression {
        "file.path" => Some(Value::String(note.document_path.clone())),
        "file.name" => Some(Value::String(note.file_name.clone())),
        "file.ext" => Some(Value::String(note.file_ext.clone())),
        "file.mtime" => Some(Value::Number(note.file_mtime.into())),
        property if is_simple_property_expression(property) => Some(
            note.properties
                .get(property)
                .cloned()
                .unwrap_or(Value::Null),
        ),
        _ => None,
    }
}

fn is_simple_property_expression(expression: &str) -> bool {
    !expression.is_empty()
        && expression
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

fn normalize_base_path(path: &str) -> Result<String, BasesError> {
    if path.is_empty()
        || path.chars().any(char::is_control)
        || !Path::new(path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("base"))
        || Path::new(path)
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err(BasesError::InvalidPath(path.to_string()));
    }

    Ok(Path::new(path)
        .components()
        .filter_map(|component| match component {
            Component::CurDir => None,
            other => Some(other.as_os_str().to_string_lossy().into_owned()),
        })
        .collect::<Vec<_>>()
        .join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scan_vault, ScanMode};
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn parser_accepts_supported_views_and_flags_unsupported_fields() {
        let report = parse_base_file(
            "
            source: notes
            views:
              - name: Release
                type: table
                filters:
                  - status = backlog
                sort:
                  by: due
                formulas:
                  note_name: file.name
                  unsupported: concat(file.name, due)
              - name: Board
                type: board
                group_by: status
            ",
        )
        .expect("base parse should succeed");

        assert_eq!(report.views.len(), 2);
        assert!(report.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("unsupported view field `group_by`")));
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
        assert_eq!(report.views[0].rows[0].document_path, "Backlog.md");
        assert_eq!(
            report.views[0].rows[0].formulas.get("note_name"),
            Some(&Value::String("Backlog".to_string()))
        );
        assert!(report.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("unsupported formula expression")));
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
}
