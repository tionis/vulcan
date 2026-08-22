//! Collection-scoped support for the mdbase typed Markdown specification.
//!
//! This module is intentionally separate from [`crate::config`]. `mdbase.yaml`
//! describes portable collection semantics, while `.vulcan/config*.toml`
//! configures Vulcan itself.

use crate::paths::{normalize_relative_input_path, RelativePathOptions};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

pub const MDBASE_CONFIG_FILE_NAME: &str = "mdbase.yaml";
pub const SUPPORTED_MDBASE_SPEC_MINOR: &str = "0.3";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MdbaseValidationLevel {
    Off,
    Warn,
    #[default]
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseSettings {
    pub timezone: Option<String>,
    pub types_folder: String,
    pub contracts_folder: String,
    pub record_extensions: Vec<String>,
    pub validation: MdbaseValidationLevel,
    pub explicit_type_keys: Vec<String>,
    pub id_field: String,
    pub include_subfolders: bool,
    pub exclude: Vec<String>,
}

impl Default for MdbaseSettings {
    fn default() -> Self {
        Self {
            timezone: None,
            types_folder: "_types".to_string(),
            contracts_folder: "_contracts".to_string(),
            record_extensions: vec!["md".to_string()],
            validation: MdbaseValidationLevel::Error,
            explicit_type_keys: vec!["type".to_string(), "types".to_string()],
            id_field: "id".to_string(),
            include_subfolders: true,
            exclude: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseConfig {
    pub spec_version: String,
    pub settings: MdbaseSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseConfigDiagnostic {
    pub severity: MdbaseDiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub path: PathBuf,
    pub field: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MdbaseDiagnosticSeverity {
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdbaseCollection {
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub config: MdbaseConfig,
    pub diagnostics: Vec<MdbaseConfigDiagnostic>,
}

#[derive(Debug)]
pub enum MdbaseConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    InvalidYaml {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    InvalidConfig {
        path: PathBuf,
        field: String,
        message: String,
    },
}

impl Display for MdbaseConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "failed to read {}: {source}", path.display())
            }
            Self::InvalidYaml { path, source } => {
                write!(
                    formatter,
                    "invalid mdbase config {}: {source}",
                    path.display()
                )
            }
            Self::InvalidConfig {
                path,
                field,
                message,
            } => write!(
                formatter,
                "invalid mdbase config {} at `{field}`: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for MdbaseConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidYaml { source, .. } => Some(source),
            Self::InvalidConfig { .. } => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawMdbaseConfig {
    spec_version: Option<String>,
    #[serde(default)]
    settings: RawMdbaseSettings,
    #[serde(flatten)]
    unknown: BTreeMap<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
struct RawMdbaseSettings {
    timezone: Option<String>,
    types_folder: Option<String>,
    contracts_folder: Option<String>,
    record_extensions: Option<Vec<String>>,
    validation: Option<MdbaseValidationLevel>,
    explicit_type_keys: Option<Vec<String>>,
    id_field: Option<String>,
    include_subfolders: Option<bool>,
    exclude: Option<Vec<String>>,
    #[serde(flatten)]
    unknown: BTreeMap<String, Value>,
}

/// Load a root-level mdbase collection marker without changing ordinary
/// Vulcan vault behavior. A missing marker returns `Ok(None)`.
pub fn load_mdbase_collection(
    collection_root: &Path,
) -> Result<Option<MdbaseCollection>, MdbaseConfigError> {
    let config_path = collection_root.join(MDBASE_CONFIG_FILE_NAME);
    let contents = match fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(MdbaseConfigError::Io {
                path: config_path,
                source,
            })
        }
    };
    let raw: RawMdbaseConfig =
        serde_yaml::from_str(&contents).map_err(|source| MdbaseConfigError::InvalidYaml {
            path: config_path.clone(),
            source,
        })?;

    let spec_version = raw
        .spec_version
        .ok_or_else(|| MdbaseConfigError::InvalidConfig {
            path: config_path.clone(),
            field: "spec_version".to_string(),
            message: "required field is missing".to_string(),
        })?;
    validate_spec_version(&config_path, &spec_version)?;
    let settings = normalize_settings(&config_path, raw.settings)?;
    let mut diagnostics = unknown_key_diagnostics(&config_path, "", raw.unknown);
    diagnostics.extend(unknown_key_diagnostics(
        &config_path,
        "settings.",
        settings.1,
    ));

    Ok(Some(MdbaseCollection {
        root: collection_root.to_path_buf(),
        config_path,
        config: MdbaseConfig {
            spec_version,
            settings: settings.0,
        },
        diagnostics,
    }))
}

fn validate_spec_version(path: &Path, version: &str) -> Result<(), MdbaseConfigError> {
    let components = version.split('.').collect::<Vec<_>>();
    let supported = components.len() == 3
        && components[0] == "0"
        && components[1] == "3"
        && components[2]
            .chars()
            .all(|character| character.is_ascii_digit())
        && (components[2] == "0" || !components[2].starts_with('0'));
    if supported {
        return Ok(());
    }

    invalid_config(
        path,
        "spec_version",
        format!(
            "unsupported version `{version}`; Vulcan currently supports stable {SUPPORTED_MDBASE_SPEC_MINOR}.x collections"
        ),
    )
}

fn normalize_settings(
    path: &Path,
    raw: RawMdbaseSettings,
) -> Result<(MdbaseSettings, BTreeMap<String, Value>), MdbaseConfigError> {
    let defaults = MdbaseSettings::default();
    let timezone = raw.timezone;
    if let Some(value) = timezone.as_deref() {
        if value == "local" || value.parse::<Tz>().is_err() {
            return invalid_config(
                path,
                "settings.timezone",
                format!("`{value}` is not an IANA timezone identifier"),
            );
        }
    }

    let types_folder = normalize_control_folder(
        path,
        "settings.types_folder",
        &raw.types_folder.unwrap_or(defaults.types_folder),
    )?;
    let contracts_folder = normalize_control_folder(
        path,
        "settings.contracts_folder",
        &raw.contracts_folder.unwrap_or(defaults.contracts_folder),
    )?;
    if types_folder == contracts_folder {
        return invalid_config(
            path,
            "settings.contracts_folder",
            "types and contracts folders must be different normalized paths",
        );
    }

    let record_extensions = normalize_record_extensions(
        path,
        raw.record_extensions.unwrap_or(defaults.record_extensions),
    )?;
    let explicit_type_keys = raw
        .explicit_type_keys
        .unwrap_or(defaults.explicit_type_keys);
    validate_field_names(path, "settings.explicit_type_keys", &explicit_type_keys)?;
    let id_field = raw.id_field.unwrap_or(defaults.id_field);
    validate_field_names(path, "settings.id_field", std::slice::from_ref(&id_field))?;

    Ok((
        MdbaseSettings {
            timezone,
            types_folder,
            contracts_folder,
            record_extensions,
            validation: raw.validation.unwrap_or(defaults.validation),
            explicit_type_keys,
            id_field,
            include_subfolders: raw
                .include_subfolders
                .unwrap_or(defaults.include_subfolders),
            exclude: raw.exclude.unwrap_or(defaults.exclude),
        },
        raw.unknown,
    ))
}

fn normalize_control_folder(
    config_path: &Path,
    field: &str,
    value: &str,
) -> Result<String, MdbaseConfigError> {
    normalize_relative_input_path(
        value,
        RelativePathOptions {
            expected_extension: None,
            append_extension_if_missing: false,
        },
    )
    .map_err(|_| MdbaseConfigError::InvalidConfig {
        path: config_path.to_path_buf(),
        field: field.to_string(),
        message: format!("`{value}` must be a safe collection-relative path"),
    })
}

fn normalize_record_extensions(
    path: &Path,
    extensions: Vec<String>,
) -> Result<Vec<String>, MdbaseConfigError> {
    if extensions.is_empty() {
        return invalid_config(
            path,
            "settings.record_extensions",
            "at least one record extension is required",
        );
    }

    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    for extension in extensions {
        let canonical = extension.to_ascii_lowercase();
        if extension.is_empty()
            || extension.starts_with('.')
            || !extension
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
        {
            return invalid_config(
                path,
                "settings.record_extensions",
                format!("`{extension}` must contain only ASCII letters or digits and no dot"),
            );
        }
        if seen.insert(canonical.clone()) {
            normalized.push(canonical);
        }
    }
    Ok(normalized)
}

fn validate_field_names(
    path: &Path,
    field: &str,
    values: &[String],
) -> Result<(), MdbaseConfigError> {
    if let Some(value) = values
        .iter()
        .find(|value| value.is_empty() || value.chars().any(char::is_control))
    {
        return invalid_config(
            path,
            field,
            format!("field name `{value}` must be non-empty and contain no control characters"),
        );
    }
    Ok(())
}

fn unknown_key_diagnostics(
    path: &Path,
    prefix: &str,
    unknown: BTreeMap<String, Value>,
) -> Vec<MdbaseConfigDiagnostic> {
    unknown
        .into_keys()
        .map(|key| {
            let field = format!("{prefix}{key}");
            MdbaseConfigDiagnostic {
                severity: MdbaseDiagnosticSeverity::Warning,
                code: "unknown_config_key".to_string(),
                message: format!("unknown mdbase config key `{field}`"),
                path: path.to_path_buf(),
                field,
            }
        })
        .collect()
}

fn invalid_config<T>(
    path: &Path,
    field: &str,
    message: impl Into<String>,
) -> Result<T, MdbaseConfigError> {
    Err(MdbaseConfigError::InvalidConfig {
        path: path.to_path_buf(),
        field: field.to_string(),
        message: message.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_config(root: &Path, content: &str) {
        fs::write(root.join(MDBASE_CONFIG_FILE_NAME), content).expect("config should be written");
    }

    #[test]
    fn missing_marker_is_not_an_mdbase_collection() {
        let directory = tempdir().expect("temporary directory should exist");
        assert_eq!(
            load_mdbase_collection(directory.path()).expect("detection should succeed"),
            None
        );
    }

    #[test]
    fn minimal_config_uses_v03_defaults() {
        let directory = tempdir().expect("temporary directory should exist");
        write_config(directory.path(), "spec_version: \"0.3.0\"\n");

        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");

        assert_eq!(collection.config.spec_version, "0.3.0");
        assert_eq!(collection.config.settings, MdbaseSettings::default());
        assert!(collection.diagnostics.is_empty());
    }

    #[test]
    fn custom_settings_are_normalized_and_unknown_keys_warn() {
        let directory = tempdir().expect("temporary directory should exist");
        write_config(
            directory.path(),
            r#"spec_version: "0.3.7"
name: Example
settings:
  timezone: Europe/Berlin
  types_folder: ./Schema/Types
  contracts_folder: Schema/Contracts
  record_extensions: [MD, md, markdown]
  validation: warn
  explicit_type_keys: []
  id_field: "@id"
  include_subfolders: false
  exclude: [archive/**]
  future_setting: true
"#,
        );

        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");

        assert_eq!(
            collection.config.settings.timezone.as_deref(),
            Some("Europe/Berlin")
        );
        assert_eq!(collection.config.settings.types_folder, "Schema/Types");
        assert_eq!(
            collection.config.settings.contracts_folder,
            "Schema/Contracts"
        );
        assert_eq!(
            collection.config.settings.record_extensions,
            ["md", "markdown"]
        );
        assert_eq!(
            collection.config.settings.validation,
            MdbaseValidationLevel::Warn
        );
        assert!(collection.config.settings.explicit_type_keys.is_empty());
        assert_eq!(collection.config.settings.id_field, "@id");
        assert!(!collection.config.settings.include_subfolders);
        assert_eq!(collection.config.settings.exclude, ["archive/**"]);
        assert_eq!(
            collection
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.field.as_str())
                .collect::<Vec<_>>(),
            ["name", "settings.future_setting"]
        );
    }

    #[test]
    fn malformed_yaml_is_reported() {
        let directory = tempdir().expect("temporary directory should exist");
        write_config(directory.path(), "spec_version: [\n");
        assert!(matches!(
            load_mdbase_collection(directory.path()),
            Err(MdbaseConfigError::InvalidYaml { .. })
        ));
    }

    #[test]
    fn missing_spec_version_is_a_config_error() {
        let directory = tempdir().expect("temporary directory should exist");
        write_config(directory.path(), "settings: {}\n");
        assert!(matches!(
            load_mdbase_collection(directory.path()),
            Err(MdbaseConfigError::InvalidConfig { ref field, .. }) if field == "spec_version"
        ));
    }

    #[test]
    fn unsupported_or_prerelease_versions_are_rejected() {
        for version in ["0.2.1", "0.4.0", "0.3.00", "0.3.0-rc.1", "1.0.0"] {
            let directory = tempdir().expect("temporary directory should exist");
            write_config(directory.path(), &format!("spec_version: \"{version}\"\n"));
            assert!(matches!(
                load_mdbase_collection(directory.path()),
                Err(MdbaseConfigError::InvalidConfig { ref field, .. }) if field == "spec_version"
            ));
        }
    }

    #[test]
    fn unsafe_or_colliding_control_folders_are_rejected() {
        for settings in [
            "types_folder: ../types\n",
            "types_folder: /types\n",
            "types_folder: schema\n  contracts_folder: ./schema\n",
        ] {
            let directory = tempdir().expect("temporary directory should exist");
            write_config(
                directory.path(),
                &format!("spec_version: \"0.3.0\"\nsettings:\n  {settings}"),
            );
            assert!(matches!(
                load_mdbase_collection(directory.path()),
                Err(MdbaseConfigError::InvalidConfig { .. })
            ));
        }
    }

    #[test]
    fn timezone_must_be_a_durable_iana_identifier() {
        for timezone in ["local", "+02:00", "Mars/Olympus_Mons"] {
            let directory = tempdir().expect("temporary directory should exist");
            write_config(
                directory.path(),
                &format!("spec_version: \"0.3.0\"\nsettings:\n  timezone: \"{timezone}\"\n"),
            );
            assert!(matches!(
                load_mdbase_collection(directory.path()),
                Err(MdbaseConfigError::InvalidConfig { ref field, .. })
                    if field == "settings.timezone"
            ));
        }
    }

    #[test]
    fn record_extensions_must_be_nonempty_names_without_dots() {
        for extensions in ["[]", "[.md]", "[md/text]"] {
            let directory = tempdir().expect("temporary directory should exist");
            write_config(
                directory.path(),
                &format!("spec_version: \"0.3.0\"\nsettings:\n  record_extensions: {extensions}\n"),
            );
            assert!(matches!(
                load_mdbase_collection(directory.path()),
                Err(MdbaseConfigError::InvalidConfig { ref field, .. })
                    if field == "settings.record_extensions"
            ));
        }
    }
}
