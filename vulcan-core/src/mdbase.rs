//! Collection-scoped support for the mdbase typed Markdown specification.
//!
//! This module is intentionally separate from [`crate::config`]. `mdbase.yaml`
//! describes portable collection semantics, while `.vulcan/config*.toml`
//! configures Vulcan itself.

use crate::paths::{normalize_relative_input_path, RelativePathOptions};
use chrono_tz::Tz;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

pub const MDBASE_CONFIG_FILE_NAME: &str = "mdbase.yaml";
pub const MDBASE_LOCK_FILE_NAME: &str = "mdbase.lock.yaml";
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

/// Files that participate in mdbase semantics for one collection root.
///
/// Paths are collection-relative, use `/` separators, and are sorted. This
/// view is deliberately narrower than Vulcan's ordinary vault scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MdbaseDiscovery {
    pub records: Vec<String>,
    pub type_files: Vec<String>,
    pub contract_files: Vec<String>,
    pub nested_collections: Vec<String>,
}

#[derive(Debug)]
pub enum MdbaseDiscoveryError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    NonUtf8Path {
        path: PathBuf,
    },
}

impl Display for MdbaseDiscoveryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "failed to discover {}: {source}", path.display())
            }
            Self::NonUtf8Path { path } => write!(
                formatter,
                "mdbase collection contains a path that is not valid UTF-8: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for MdbaseDiscoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::NonUtf8Path { .. } => None,
        }
    }
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

/// Discover the collection's mdbase records and control files.
///
/// This does not affect the normal Vulcan scanner: type and contract Markdown
/// remains visible to ordinary vault browsing and is excluded only from this
/// collection-scoped record set.
pub fn discover_mdbase_files(
    collection: &MdbaseCollection,
) -> Result<MdbaseDiscovery, MdbaseDiscoveryError> {
    let excludes = compile_excludes(&collection.config.settings.exclude)
        .expect("exclusion globs are validated while loading mdbase.yaml");
    let mut discovery = MdbaseDiscovery::default();

    discover_control_files(
        &collection.root,
        &collection.config.settings.types_folder,
        &mut discovery.type_files,
    )?;
    discover_control_files(
        &collection.root,
        &collection.config.settings.contracts_folder,
        &mut discovery.contract_files,
    )?;
    discover_records(collection, &collection.root, "", &excludes, &mut discovery)?;

    discovery.records.sort();
    discovery.type_files.sort();
    discovery.contract_files.sort();
    discovery.nested_collections.sort();
    Ok(discovery)
}

fn discover_control_files(
    root: &Path,
    folder: &str,
    output: &mut Vec<String>,
) -> Result<(), MdbaseDiscoveryError> {
    let directory = root.join(folder);
    if !directory.exists() {
        return Ok(());
    }
    discover_markdown_files(root, &directory, output)
}

fn discover_markdown_files(
    root: &Path,
    directory: &Path,
    output: &mut Vec<String>,
) -> Result<(), MdbaseDiscoveryError> {
    for entry in sorted_directory_entries(directory)? {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| MdbaseDiscoveryError::Io {
                path: path.clone(),
                source,
            })?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            discover_markdown_files(root, &path, output)?;
        } else if file_type.is_file() && has_extension(&path, "md") {
            output.push(relative_utf8_path(root, &path)?);
        }
    }
    Ok(())
}

fn discover_records(
    collection: &MdbaseCollection,
    directory: &Path,
    relative_directory: &str,
    excludes: &GlobSet,
    discovery: &mut MdbaseDiscovery,
) -> Result<(), MdbaseDiscoveryError> {
    for entry in sorted_directory_entries(directory)? {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| MdbaseDiscoveryError::Io {
                path: path.clone(),
                source,
            })?;
        if file_type.is_symlink() {
            continue;
        }

        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| MdbaseDiscoveryError::NonUtf8Path { path: path.clone() })?;
        let relative = if relative_directory.is_empty() {
            name
        } else {
            format!("{relative_directory}/{name}")
        };

        if file_type.is_dir() {
            if is_control_directory(collection, &relative)
                || is_derived_directory(&relative)
                || is_excluded(excludes, &relative, true)
            {
                continue;
            }
            if path.join(MDBASE_CONFIG_FILE_NAME).is_file() {
                discovery.nested_collections.push(relative);
                continue;
            }
            if collection.config.settings.include_subfolders {
                discover_records(collection, &path, &relative, excludes, discovery)?;
            }
        } else if file_type.is_file()
            && !(relative_directory.is_empty()
                && matches!(
                    relative.as_str(),
                    MDBASE_CONFIG_FILE_NAME | MDBASE_LOCK_FILE_NAME
                ))
            && !is_excluded(excludes, &relative, false)
            && collection
                .config
                .settings
                .record_extensions
                .iter()
                .any(|extension| has_extension(&path, extension))
        {
            discovery.records.push(relative);
        }
    }
    Ok(())
}

fn sorted_directory_entries(directory: &Path) -> Result<Vec<fs::DirEntry>, MdbaseDiscoveryError> {
    let entries = fs::read_dir(directory).map_err(|source| MdbaseDiscoveryError::Io {
        path: directory.to_path_buf(),
        source,
    })?;
    let mut entries = entries
        .map(|entry| {
            entry.map_err(|source| MdbaseDiscoveryError::Io {
                path: directory.to_path_buf(),
                source,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn is_control_directory(collection: &MdbaseCollection, relative: &str) -> bool {
    [
        collection.config.settings.types_folder.as_str(),
        collection.config.settings.contracts_folder.as_str(),
    ]
    .into_iter()
    .any(|folder| relative == folder)
}

fn is_derived_directory(relative: &str) -> bool {
    relative
        .split('/')
        .any(|component| matches!(component, ".git" | ".mdbase" | ".vulcan" | "node_modules"))
}

fn is_excluded(excludes: &GlobSet, relative: &str, directory: bool) -> bool {
    excludes.is_match(relative)
        || (directory && excludes.is_match(format!("{relative}/.mdbase-discovery-probe")))
}

fn has_extension(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

fn relative_utf8_path(root: &Path, path: &Path) -> Result<String, MdbaseDiscoveryError> {
    let relative = path
        .strip_prefix(root)
        .expect("discovered path should remain below collection root");
    let mut components = Vec::new();
    for component in relative.components() {
        let component =
            component
                .as_os_str()
                .to_str()
                .ok_or_else(|| MdbaseDiscoveryError::NonUtf8Path {
                    path: path.to_path_buf(),
                })?;
        components.push(component);
    }
    Ok(components.join("/"))
}

fn compile_excludes(patterns: &[String]) -> Result<GlobSet, globset::Error> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            GlobBuilder::new(pattern)
                .literal_separator(true)
                .backslash_escape(false)
                .build()?,
        );
    }
    builder.build()
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
    let exclude = raw.exclude.unwrap_or(defaults.exclude);
    if let Err(error) = compile_excludes(&exclude) {
        return invalid_config(
            path,
            "settings.exclude",
            format!("contains an invalid collection-relative glob: {error}"),
        );
    }

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
            exclude,
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
    use crate::{scan_vault, CacheDatabase, ScanMode, VaultPaths};
    use tempfile::tempdir;

    fn write_config(root: &Path, content: &str) {
        fs::write(root.join(MDBASE_CONFIG_FILE_NAME), content).expect("config should be written");
    }

    fn write_file(root: &Path, relative: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("file should have a parent"))
            .expect("parent should be created");
        fs::write(path, "---\ntitle: Fixture\n---\n").expect("file should be written");
    }

    fn fixture_collection(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults/mdbase")
            .join(name)
    }

    fn copy_directory(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("destination should be created");
        for entry in fs::read_dir(source).expect("fixture should be readable") {
            let entry = entry.expect("fixture entry should be readable");
            let destination_path = destination.join(entry.file_name());
            if entry
                .file_type()
                .expect("fixture file type should be readable")
                .is_dir()
            {
                copy_directory(&entry.path(), &destination_path);
            } else {
                fs::copy(entry.path(), destination_path).expect("fixture file should be copied");
            }
        }
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

    #[test]
    fn invalid_exclusion_glob_is_rejected_during_config_load() {
        let directory = tempdir().expect("temporary directory should exist");
        write_config(
            directory.path(),
            "spec_version: \"0.3.0\"\nsettings:\n  exclude: ['[invalid']\n",
        );
        assert!(matches!(
            load_mdbase_collection(directory.path()),
            Err(MdbaseConfigError::InvalidConfig { ref field, .. })
                if field == "settings.exclude"
        ));
    }

    #[test]
    fn discovery_separates_records_controls_exclusions_and_nested_collections() {
        let directory = tempdir().expect("temporary directory should exist");
        write_config(
            directory.path(),
            r#"spec_version: "0.3.0"
settings:
  types_folder: Schema/Types
  contracts_folder: Schema/Contracts
  record_extensions: [md, markdown]
  exclude: [Archive/**, "**/*.draft.md"]
"#,
        );
        for relative in [
            "Root.md",
            "Notes/Record.markdown",
            "Notes/Ignored.draft.md",
            "Schema/Types/Person.md",
            "Schema/Types/README.txt",
            "Schema/Contracts/People.md",
            "Schema/Other.md",
            "Archive/Old.md",
            ".mdbase/state.md",
            ".vulcan/internal.md",
            ".git/internal.md",
            "node_modules/package.md",
            MDBASE_LOCK_FILE_NAME,
            "Nested/mdbase.yaml",
            "Nested/Child.md",
        ] {
            write_file(directory.path(), relative);
        }

        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");
        let discovered = discover_mdbase_files(&collection).expect("discovery should succeed");

        assert_eq!(
            discovered.records,
            ["Notes/Record.markdown", "Root.md", "Schema/Other.md"]
        );
        assert_eq!(discovered.type_files, ["Schema/Types/Person.md"]);
        assert_eq!(discovered.contract_files, ["Schema/Contracts/People.md"]);
        assert_eq!(discovered.nested_collections, ["Nested"]);
    }

    #[test]
    fn disabling_subfolders_only_limits_record_discovery() {
        let directory = tempdir().expect("temporary directory should exist");
        write_config(
            directory.path(),
            "spec_version: \"0.3.0\"\nsettings:\n  include_subfolders: false\n",
        );
        for relative in ["Root.md", "Notes/Child.md", "_types/Nested/Person.md"] {
            write_file(directory.path(), relative);
        }

        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");
        let discovered = discover_mdbase_files(&collection).expect("discovery should succeed");

        assert_eq!(discovered.records, ["Root.md"]);
        assert_eq!(discovered.type_files, ["_types/Nested/Person.md"]);
        assert!(discovered.nested_collections.is_empty());
    }

    #[test]
    fn fixture_suite_covers_valid_and_invalid_collection_configs() {
        let minimal = load_mdbase_collection(&fixture_collection("minimal"))
            .expect("minimal fixture should load")
            .expect("minimal fixture should be detected");
        assert_eq!(minimal.config.settings, MdbaseSettings::default());

        let custom = load_mdbase_collection(&fixture_collection("custom"))
            .expect("custom fixture should load")
            .expect("custom fixture should be detected");
        assert_eq!(custom.diagnostics.len(), 2);
        assert_eq!(
            discover_mdbase_files(&custom).expect("custom fixture should be discovered"),
            MdbaseDiscovery {
                records: vec![
                    "People/Ada.markdown".to_string(),
                    "Schema/Other.md".to_string(),
                ],
                type_files: vec!["Schema/Types/Person.md".to_string()],
                contract_files: vec!["Schema/Contracts/People.md".to_string()],
                nested_collections: vec!["Nested".to_string()],
            }
        );

        assert!(matches!(
            load_mdbase_collection(&fixture_collection("malformed")),
            Err(MdbaseConfigError::InvalidYaml { .. })
        ));
        for name in ["unsupported", "unsafe-path", "invalid-timezone"] {
            assert!(matches!(
                load_mdbase_collection(&fixture_collection(name)),
                Err(MdbaseConfigError::InvalidConfig { .. })
            ));
        }
    }

    #[test]
    fn ordinary_vulcan_scan_keeps_mdbase_control_markdown_visible() {
        let directory = tempdir().expect("temporary directory should exist");
        let vault_root = directory.path().join("vault");
        copy_directory(&fixture_collection("custom"), &vault_root);
        fs::create_dir_all(vault_root.join(".vulcan")).expect("Vulcan directory should exist");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("ordinary scan should succeed");
        let database = CacheDatabase::open(&paths).expect("cache should open");
        let mut statement = database
            .connection()
            .prepare("SELECT path FROM documents ORDER BY path")
            .expect("document query should prepare");
        let paths = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("document query should execute")
            .collect::<Result<Vec<_>, _>>()
            .expect("document paths should deserialize");

        assert!(paths.contains(&"Schema/Types/Person.md".to_string()));
        assert!(paths.contains(&"Schema/Contracts/People.md".to_string()));
    }
}
