use crate::paths::ensure_vulcan_dir;
use crate::{search::SearchMode, VaultPaths};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

const REPORT_EXTENSION: &str = "toml";

#[derive(Debug)]
pub enum SavedReportError {
    InvalidName(String),
    Io(std::io::Error),
    TomlDeserialize(toml::de::Error),
    TomlSerialize(toml::ser::Error),
}

impl Display for SavedReportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName(name) => write!(
                formatter,
                "saved report names must be simple identifiers without separators or control characters: {name}"
            ),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::TomlDeserialize(error) => write!(formatter, "{error}"),
            Self::TomlSerialize(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for SavedReportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::TomlDeserialize(error) => Some(error),
            Self::TomlSerialize(error) => Some(error),
            Self::InvalidName(_) => None,
        }
    }
}

impl From<std::io::Error> for SavedReportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<toml::de::Error> for SavedReportError {
    fn from(error: toml::de::Error) -> Self {
        Self::TomlDeserialize(error)
    }
}

impl From<toml::ser::Error> for SavedReportError {
    fn from(error: toml::ser::Error) -> Self {
        Self::TomlSerialize(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedExportFormat {
    Csv,
    Jsonl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedExport {
    pub format: SavedExportFormat,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedReportKind {
    Search,
    Notes,
    Bases,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SavedReportQuery {
    Search {
        query: String,
        mode: SearchMode,
        tag: Option<String>,
        path_prefix: Option<String>,
        has_property: Option<String>,
        context_size: usize,
    },
    Notes {
        filters: Vec<String>,
        sort_by: Option<String>,
        sort_descending: bool,
    },
    Bases {
        file: String,
    },
}

impl SavedReportQuery {
    #[must_use]
    pub fn kind(&self) -> SavedReportKind {
        match self {
            Self::Search { .. } => SavedReportKind::Search,
            Self::Notes { .. } => SavedReportKind::Notes,
            Self::Bases { .. } => SavedReportKind::Bases,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedReportDefinition {
    pub name: String,
    pub description: Option<String>,
    pub fields: Option<Vec<String>>,
    pub limit: Option<usize>,
    pub export: Option<SavedExport>,
    #[serde(flatten)]
    pub query: SavedReportQuery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SavedReportSummary {
    pub name: String,
    pub description: Option<String>,
    pub kind: SavedReportKind,
    pub fields: Option<Vec<String>>,
    pub limit: Option<usize>,
    pub export: Option<SavedExport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SavedReportFile {
    pub description: Option<String>,
    pub fields: Option<Vec<String>>,
    pub limit: Option<usize>,
    pub export: Option<SavedExport>,
    #[serde(flatten)]
    pub query: SavedReportQuery,
}

pub fn save_saved_report(
    paths: &VaultPaths,
    definition: &SavedReportDefinition,
) -> Result<PathBuf, SavedReportError> {
    let report_path = report_definition_path(paths, &definition.name)?;
    ensure_vulcan_dir(paths)?;
    let payload = SavedReportFile {
        description: definition.description.clone(),
        fields: definition.fields.clone(),
        limit: definition.limit,
        export: definition.export.clone(),
        query: definition.query.clone(),
    };
    let mut rendered = toml::to_string_pretty(&payload)?;
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    fs::write(&report_path, rendered)?;
    Ok(report_path)
}

pub fn load_saved_report(
    paths: &VaultPaths,
    name: &str,
) -> Result<SavedReportDefinition, SavedReportError> {
    let report_path = report_definition_path(paths, name)?;
    let source = fs::read_to_string(report_path)?;
    let stored = toml::from_str::<SavedReportFile>(&source)?;

    Ok(SavedReportDefinition {
        name: normalize_saved_report_name(name)?,
        description: stored.description,
        fields: stored.fields,
        limit: stored.limit,
        export: stored.export,
        query: stored.query,
    })
}

pub fn list_saved_reports(paths: &VaultPaths) -> Result<Vec<SavedReportSummary>, SavedReportError> {
    if !paths.reports_dir().exists() {
        return Ok(Vec::new());
    }

    let mut names = fs::read_dir(paths.reports_dir())?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry.file_type().ok().filter(std::fs::FileType::is_file)?;
            report_name_from_path(&entry.path())
        })
        .collect::<Vec<_>>();
    names.sort();

    names
        .into_iter()
        .map(|name| {
            let definition = load_saved_report(paths, &name)?;
            Ok(SavedReportSummary {
                name: definition.name,
                description: definition.description,
                kind: definition.query.kind(),
                fields: definition.fields,
                limit: definition.limit,
                export: definition.export,
            })
        })
        .collect()
}

pub fn report_definition_path(paths: &VaultPaths, name: &str) -> Result<PathBuf, SavedReportError> {
    let normalized = normalize_saved_report_name(name)?;
    Ok(paths
        .reports_dir()
        .join(format!("{normalized}.{REPORT_EXTENSION}")))
}

pub fn normalize_saved_report_name(name: &str) -> Result<String, SavedReportError> {
    let trimmed = name.trim_end_matches(&format!(".{REPORT_EXTENSION}"));
    if trimmed.is_empty()
        || trimmed.chars().any(char::is_control)
        || trimmed.contains(['/', '\\'])
        || trimmed == "."
        || trimmed == ".."
    {
        return Err(SavedReportError::InvalidName(name.to_string()));
    }

    if !trimmed
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(SavedReportError::InvalidName(name.to_string()));
    }

    Ok(trimmed.to_string())
}

fn report_name_from_path(path: &Path) -> Option<String> {
    if path.extension().and_then(|value| value.to_str()) != Some(REPORT_EXTENSION) {
        return None;
    }

    path.file_stem()
        .and_then(|value| value.to_str())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn save_and_load_saved_report_round_trip() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        let definition = SavedReportDefinition {
            name: "weekly-search".to_string(),
            description: Some("dashboard hits".to_string()),
            fields: Some(vec!["document_path".to_string(), "rank".to_string()]),
            limit: Some(25),
            export: Some(SavedExport {
                format: SavedExportFormat::Csv,
                path: "exports/dashboard.csv".to_string(),
            }),
            query: SavedReportQuery::Search {
                query: "dashboard".to_string(),
                mode: SearchMode::Keyword,
                tag: Some("index".to_string()),
                path_prefix: Some("Projects/".to_string()),
                has_property: Some("status".to_string()),
                context_size: 24,
            },
        };

        let report_path =
            save_saved_report(&paths, &definition).expect("saved report should persist");
        assert_eq!(report_path, paths.reports_dir().join("weekly-search.toml"));

        let loaded =
            load_saved_report(&paths, "weekly-search").expect("saved report should load cleanly");
        assert_eq!(loaded, definition);
    }

    #[test]
    fn list_saved_reports_returns_sorted_summaries() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        save_saved_report(
            &paths,
            &SavedReportDefinition {
                name: "zeta".to_string(),
                description: None,
                fields: None,
                limit: None,
                export: None,
                query: SavedReportQuery::Bases {
                    file: "Indexes/Gear.base".to_string(),
                },
            },
        )
        .expect("zeta report should save");
        save_saved_report(
            &paths,
            &SavedReportDefinition {
                name: "alpha".to_string(),
                description: Some("property audit".to_string()),
                fields: Some(vec!["document_path".to_string()]),
                limit: Some(10),
                export: Some(SavedExport {
                    format: SavedExportFormat::Jsonl,
                    path: "exports/alpha.jsonl".to_string(),
                }),
                query: SavedReportQuery::Notes {
                    filters: vec!["status = active".to_string()],
                    sort_by: Some("priority".to_string()),
                    sort_descending: true,
                },
            },
        )
        .expect("alpha report should save");

        let summaries = list_saved_reports(&paths).expect("saved reports should list");
        assert_eq!(
            summaries,
            vec![
                SavedReportSummary {
                    name: "alpha".to_string(),
                    description: Some("property audit".to_string()),
                    kind: SavedReportKind::Notes,
                    fields: Some(vec!["document_path".to_string()]),
                    limit: Some(10),
                    export: Some(SavedExport {
                        format: SavedExportFormat::Jsonl,
                        path: "exports/alpha.jsonl".to_string(),
                    }),
                },
                SavedReportSummary {
                    name: "zeta".to_string(),
                    description: None,
                    kind: SavedReportKind::Bases,
                    fields: None,
                    limit: None,
                    export: None,
                },
            ]
        );
    }

    #[test]
    fn saved_report_names_reject_traversal_and_separators() {
        for invalid in ["", "../weekly", "weekly/search", "bad name", "bad\nname"] {
            assert!(normalize_saved_report_name(invalid).is_err());
        }
    }
}
