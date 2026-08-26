//! Collection-scoped support for the mdbase typed Markdown specification.
//!
//! This module is intentionally separate from [`crate::config`]. `mdbase.yaml`
//! describes portable collection semantics, while `.vulcan/config*.toml`
//! configures Vulcan itself.

use crate::config::VaultConfig;
use crate::parser::parse_document;
use crate::paths::{normalize_relative_input_path, secure_read_to_string, RelativePathOptions};
use chrono::{DateTime, NaiveDate};
use chrono_tz::Tz;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

pub const MDBASE_CONFIG_FILE_NAME: &str = "mdbase.yaml";
pub const MDBASE_LOCK_FILE_NAME: &str = "mdbase.lock.yaml";
pub const MDBASE_SPEC_VERSION: &str = "0.3.0";
pub const SUPPORTED_MDBASE_SPEC_MINOR: &str = "0.3";
pub const MDBASE_SPEC_UPSTREAM_COMMIT: &str = "68b9a97969bf9472f0d42b8faf8a2e349553f4ea";
pub const MDBASE_SPEC_UPSTREAM_URL: &str = "https://github.com/mdbase-dev/mdbase-spec";
pub const MDBASE_BUNDLED_ASSET_DIGEST: &str =
    "9b4c7d477dc914099a5a40092d6543caca9c626d5ca0ff3ed5a4d47646c29e52";
pub const MDBASE_CANONICAL_SCHEMA_BASE: &str = "https://mdbase.dev/schemas/v0.3/";
const MDBASE_SCHEMA_MAX_FILES: usize = 64;
const MDBASE_SCHEMA_MAX_DEPTH: usize = 32;
const MDBASE_SCHEMA_MAX_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdbaseBundledSchema {
    pub canonical_id: &'static str,
    pub file_name: &'static str,
    pub json: &'static str,
}

macro_rules! bundled_schema {
    ($file_name:literal) => {
        MdbaseBundledSchema {
            canonical_id: concat!("https://mdbase.dev/schemas/v0.3/", $file_name),
            file_name: $file_name,
            json: include_str!(concat!(
                "../resources/mdbase/v0.3/upstream/schemas/",
                $file_name
            )),
        }
    };
}

pub const MDBASE_BUNDLED_SCHEMAS: [MdbaseBundledSchema; 12] = [
    bundled_schema!("config.schema.json"),
    bundled_schema!("conformance-claim.schema.json"),
    bundled_schema!("data-contract.schema.json"),
    bundled_schema!("diagnostic.schema.json"),
    bundled_schema!("operation-result.schema.json"),
    bundled_schema!("query-result.schema.json"),
    bundled_schema!("query.schema.json"),
    bundled_schema!("record-document.schema.json"),
    bundled_schema!("type-file.schema.json"),
    bundled_schema!("type-pack-lock.schema.json"),
    bundled_schema!("type-pack.schema.json"),
    bundled_schema!("view.schema.json"),
];

/// Resolve an exact canonical v0.3 schema identifier from Vulcan's offline
/// bundle. Mutable aliases such as `latest` and arbitrary network URLs are not
/// resolved by this registry.
#[must_use]
pub fn bundled_mdbase_schema(canonical_id: &str) -> Option<&'static MdbaseBundledSchema> {
    MDBASE_BUNDLED_SCHEMAS
        .iter()
        .find(|schema| schema.canonical_id == canonical_id)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseSchemaDiagnostic {
    pub code: String,
    pub message: String,
    pub instance_path: String,
    pub schema_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdbaseSchemaCompileError(pub String);

impl Display for MdbaseSchemaCompileError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for MdbaseSchemaCompileError {}

/// Validate a JSON-compatible frontmatter value with the mdbase Draft 2020-12
/// profile. Date, time, and date-time formats are assertions, as required by
/// mdbase rather than annotations.
pub fn validate_mdbase_schema_value(
    schema: &serde_json::Value,
    value: &serde_json::Value,
) -> Result<Vec<MdbaseSchemaDiagnostic>, MdbaseSchemaCompileError> {
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(schema)
        .map_err(|error| MdbaseSchemaCompileError(error.to_string()))?;
    Ok(schema_diagnostics(&validator, value))
}

/// Validate with offline canonical schemas and collection-confined local file
/// references. The base file establishes the location for relative `$ref`
/// values; it and every referenced schema must remain inside `collection_root`.
pub fn validate_mdbase_schema_value_with_local_refs(
    schema: &serde_json::Value,
    value: &serde_json::Value,
    base_file: &Path,
    collection_root: &Path,
) -> Result<Vec<MdbaseSchemaDiagnostic>, MdbaseSchemaCompileError> {
    let collection_root = fs::canonicalize(collection_root).map_err(|error| {
        MdbaseSchemaCompileError(format!(
            "failed to resolve mdbase collection root {}: {error}",
            collection_root.display()
        ))
    })?;
    let base_file = fs::canonicalize(base_file).map_err(|error| {
        MdbaseSchemaCompileError(format!(
            "failed to resolve schema base file {}: {error}",
            base_file.display()
        ))
    })?;
    ensure_schema_path_is_contained(&base_file, &collection_root)?;

    let base_uri = schema_file_uri(&base_file)?;
    let mut schemas = HashMap::new();
    for bundled in MDBASE_BUNDLED_SCHEMAS {
        let parsed = serde_json::from_str(bundled.json).map_err(|error| {
            MdbaseSchemaCompileError(format!(
                "bundled mdbase schema {} is invalid JSON: {error}",
                bundled.file_name
            ))
        })?;
        schemas.insert(bundled.canonical_id.to_string(), parsed);
    }

    let mut loader = LocalSchemaLoader {
        collection_root: &collection_root,
        schemas: &mut schemas,
        loaded_paths: BTreeSet::new(),
        visiting: vec![base_file.clone()],
    };
    loader.load_references(schema, &base_file, 0)?;

    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .with_base_uri(base_uri)
        .with_retriever(MdbaseSchemaRetriever { schemas })
        .build(schema)
        .map_err(|error| MdbaseSchemaCompileError(error.to_string()))?;
    Ok(schema_diagnostics(&validator, value))
}

fn schema_diagnostics(
    validator: &jsonschema::Validator,
    value: &serde_json::Value,
) -> Vec<MdbaseSchemaDiagnostic> {
    let mut diagnostics = validator
        .iter_errors(value)
        .map(|error| {
            let keyword = error.kind().keyword();
            MdbaseSchemaDiagnostic {
                code: if keyword == "format" {
                    "format_invalid".to_string()
                } else {
                    format!("schema_{}", camel_to_snake(keyword))
                },
                message: error.to_string(),
                instance_path: error.instance_path().to_string(),
                schema_path: error.schema_path().to_string(),
            }
        })
        .collect::<Vec<_>>();
    diagnostics.sort_by(|left, right| {
        left.instance_path
            .cmp(&right.instance_path)
            .then_with(|| left.schema_path.cmp(&right.schema_path))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
    diagnostics
}

struct MdbaseSchemaRetriever {
    schemas: HashMap<String, serde_json::Value>,
}

impl jsonschema::Retrieve for MdbaseSchemaRetriever {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        self.schemas
            .get(uri.as_str())
            .cloned()
            .ok_or_else(|| format!("schema reference is not available offline: {uri}").into())
    }
}

struct LocalSchemaLoader<'a> {
    collection_root: &'a Path,
    schemas: &'a mut HashMap<String, serde_json::Value>,
    loaded_paths: BTreeSet<PathBuf>,
    visiting: Vec<PathBuf>,
}

impl LocalSchemaLoader<'_> {
    fn load_references(
        &mut self,
        schema: &serde_json::Value,
        source_file: &Path,
        depth: usize,
    ) -> Result<(), MdbaseSchemaCompileError> {
        if depth > MDBASE_SCHEMA_MAX_DEPTH {
            return Err(MdbaseSchemaCompileError(format!(
                "schema reference depth exceeds {MDBASE_SCHEMA_MAX_DEPTH}"
            )));
        }
        let mut references = Vec::new();
        collect_external_schema_references(schema, &mut references);
        references.sort_unstable();
        references.dedup();

        for reference in references {
            let Some(resolved) = self.resolve_reference(reference, source_file)? else {
                continue;
            };
            if let Some(cycle_start) = self.visiting.iter().position(|path| path == &resolved) {
                return Err(self.cycle_error(cycle_start, &resolved));
            }
            if self.loaded_paths.contains(&resolved) {
                continue;
            }
            if self.loaded_paths.len() >= MDBASE_SCHEMA_MAX_FILES {
                return Err(MdbaseSchemaCompileError(format!(
                    "schema reference count exceeds {MDBASE_SCHEMA_MAX_FILES}"
                )));
            }
            let referenced_schema = read_local_schema(&resolved)?;
            let uri = schema_file_uri(&resolved)?;
            self.schemas.insert(uri, referenced_schema.clone());
            self.visiting.push(resolved.clone());
            self.load_references(&referenced_schema, &resolved, depth + 1)?;
            self.visiting.pop();
            self.loaded_paths.insert(resolved);
        }
        Ok(())
    }

    fn resolve_reference(
        &self,
        reference: &str,
        source_file: &Path,
    ) -> Result<Option<PathBuf>, MdbaseSchemaCompileError> {
        let reference = reference.split('#').next().unwrap_or_default();
        if reference.is_empty() || bundled_mdbase_schema(reference).is_some() {
            return Ok(None);
        }
        if reference.contains("://") || reference.starts_with("urn:") {
            return Err(MdbaseSchemaCompileError(format!(
                "remote schema reference is not allowed: {reference}"
            )));
        }
        if reference.contains('?') {
            return Err(MdbaseSchemaCompileError(format!(
                "schema reference queries are not supported: {reference}"
            )));
        }
        let parent = source_file.parent().ok_or_else(|| {
            MdbaseSchemaCompileError(format!(
                "schema base file has no parent: {}",
                source_file.display()
            ))
        })?;
        let resolved = fs::canonicalize(parent.join(reference)).map_err(|error| {
            MdbaseSchemaCompileError(format!(
                "failed to resolve schema reference {reference} from {}: {error}",
                source_file.display()
            ))
        })?;
        ensure_schema_path_is_contained(&resolved, self.collection_root)?;
        Ok(Some(resolved))
    }

    fn cycle_error(&self, cycle_start: usize, resolved: &Path) -> MdbaseSchemaCompileError {
        let mut cycle = self.visiting[cycle_start..]
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        cycle.push(resolved.display().to_string());
        MdbaseSchemaCompileError(format!(
            "schema reference cycle detected: {}",
            cycle.join(" -> ")
        ))
    }
}

fn read_local_schema(path: &Path) -> Result<serde_json::Value, MdbaseSchemaCompileError> {
    let metadata = fs::metadata(path).map_err(|error| {
        MdbaseSchemaCompileError(format!(
            "failed to inspect schema reference {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(MdbaseSchemaCompileError(format!(
            "schema reference is not a regular file: {}",
            path.display()
        )));
    }
    if metadata.len() > MDBASE_SCHEMA_MAX_BYTES {
        return Err(MdbaseSchemaCompileError(format!(
            "schema reference exceeds {MDBASE_SCHEMA_MAX_BYTES} bytes: {}",
            path.display()
        )));
    }
    let contents = fs::read(path).map_err(|error| {
        MdbaseSchemaCompileError(format!(
            "failed to read schema reference {}: {error}",
            path.display()
        ))
    })?;
    let yaml: serde_yaml::Value = serde_yaml::from_slice(&contents).map_err(|error| {
        MdbaseSchemaCompileError(format!(
            "failed to parse schema reference {}: {error}",
            path.display()
        ))
    })?;
    serde_json::to_value(yaml).map_err(|error| {
        MdbaseSchemaCompileError(format!(
            "schema reference {} is not JSON-compatible: {error}",
            path.display()
        ))
    })
}

fn collect_external_schema_references<'a>(
    value: &'a serde_json::Value,
    references: &mut Vec<&'a str>,
) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str) {
                references.push(reference);
            }
            for child in object.values() {
                collect_external_schema_references(child, references);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                collect_external_schema_references(child, references);
            }
        }
        _ => {}
    }
}

fn ensure_schema_path_is_contained(
    path: &Path,
    collection_root: &Path,
) -> Result<(), MdbaseSchemaCompileError> {
    if path.starts_with(collection_root) {
        Ok(())
    } else {
        Err(MdbaseSchemaCompileError(format!(
            "schema reference escapes collection root: {}",
            path.display()
        )))
    }
}

fn schema_file_uri(path: &Path) -> Result<String, MdbaseSchemaCompileError> {
    let path = path.to_str().ok_or_else(|| {
        MdbaseSchemaCompileError(format!(
            "schema path is not valid UTF-8: {}",
            path.display()
        ))
    })?;
    let normalized = path.replace('\\', "/");
    let mut encoded = String::with_capacity(normalized.len());
    for byte in normalized.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").expect("writing to a string cannot fail");
        }
    }
    if encoded.starts_with('/') {
        Ok(format!("file://{encoded}"))
    } else {
        Ok(format!("file:///{encoded}"))
    }
}

fn camel_to_snake(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii_uppercase() {
            if !output.is_empty() {
                output.push('_');
            }
            output.push(character.to_ascii_lowercase());
        } else {
            output.push(character);
        }
    }
    output
}

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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseTypeDefinition {
    pub name: String,
    pub normalized_name: String,
    pub path: String,
    pub version: Option<u64>,
    pub description: Option<String>,
    pub schema: serde_json::Value,
    pub schema_ref: Option<String>,
    pub frontmatter: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseTypeDiagnostic {
    pub code: String,
    pub message: String,
    pub path: String,
    pub field: String,
    pub related_paths: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MdbaseTypeRegistry {
    types: BTreeMap<String, MdbaseTypeDefinition>,
    pub diagnostics: Vec<MdbaseTypeDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MdbaseTypeMatchMode {
    Explicit,
    Inferred,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseTypeMatchDiagnostic {
    pub code: String,
    pub message: String,
    pub path: String,
    pub field: String,
    pub type_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseTypeMatchResult {
    pub types: Vec<String>,
    pub mode: MdbaseTypeMatchMode,
    pub diagnostics: Vec<MdbaseTypeMatchDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseDeclaredTypeValue {
    pub type_name: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseTypeCompositionDiagnostic {
    pub code: String,
    pub message: String,
    pub field: String,
    pub type_names: Vec<String>,
    pub locations: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MdbaseComposedTypeBehavior {
    pub types: Vec<String>,
    pub schemas: Vec<MdbaseDeclaredTypeValue>,
    pub read_defaults: BTreeMap<String, serde_json::Value>,
    pub links: BTreeMap<String, serde_json::Value>,
    pub unique: Vec<MdbaseDeclaredTypeValue>,
    pub path: Option<serde_json::Value>,
    pub lifecycle: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
    pub projections: BTreeMap<String, serde_json::Value>,
    pub display_by_type: BTreeMap<String, serde_json::Value>,
    pub display: Option<serde_json::Value>,
    pub diagnostics: Vec<MdbaseTypeCompositionDiagnostic>,
}

impl MdbaseTypeRegistry {
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&MdbaseTypeDefinition> {
        self.types.get(&normalize_type_name(name))
    }

    pub fn iter(&self) -> impl Iterator<Item = &MdbaseTypeDefinition> {
        self.types.values()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.types.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }
}

#[derive(Debug)]
pub enum MdbaseTypeRegistryError {
    Discovery(MdbaseDiscoveryError),
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    BundledSchema {
        source: serde_json::Error,
    },
}

impl Display for MdbaseTypeRegistryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Discovery(error) => error.fmt(formatter),
            Self::Read { path, source } => {
                write!(
                    formatter,
                    "failed to read mdbase type file {}: {source}",
                    path.display()
                )
            }
            Self::BundledSchema { source } => {
                write!(
                    formatter,
                    "bundled mdbase type-file schema is invalid: {source}"
                )
            }
        }
    }
}

impl std::error::Error for MdbaseTypeRegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Discovery(error) => Some(error),
            Self::Read { source, .. } => Some(source),
            Self::BundledSchema { source } => Some(source),
        }
    }
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

/// Load valid `kind: mdbase.type` files into a deterministic registry.
///
/// Names are matched case-insensitively while the authored spelling remains
/// available on each definition. Conflicting names are excluded rather than
/// selecting a filesystem-order-dependent winner.
pub fn load_mdbase_type_registry(
    collection: &MdbaseCollection,
) -> Result<MdbaseTypeRegistry, MdbaseTypeRegistryError> {
    let discovery =
        discover_mdbase_files(collection).map_err(MdbaseTypeRegistryError::Discovery)?;
    build_mdbase_type_registry(collection, &discovery.type_files)
}

/// Resolve the types for one record using raw persisted frontmatter.
/// Explicit declarations take precedence over all inferred match rules.
#[must_use]
pub fn match_mdbase_record_types(
    collection: &MdbaseCollection,
    registry: &MdbaseTypeRegistry,
    record_path: &str,
    frontmatter: &serde_json::Value,
) -> MdbaseTypeMatchResult {
    let Ok(record_path) = normalize_relative_input_path(
        record_path,
        RelativePathOptions {
            expected_extension: None,
            append_extension_if_missing: false,
        },
    ) else {
        return MdbaseTypeMatchResult {
            types: Vec::new(),
            mode: MdbaseTypeMatchMode::Inferred,
            diagnostics: vec![match_diagnostic(
                record_path,
                "record_path_invalid",
                "record path must be collection-relative and traversal-free",
                "path",
                None,
            )],
        };
    };
    let Some(frontmatter) = frontmatter.as_object() else {
        return MdbaseTypeMatchResult {
            types: Vec::new(),
            mode: MdbaseTypeMatchMode::Inferred,
            diagnostics: vec![match_diagnostic(
                &record_path,
                "frontmatter_invalid",
                "record frontmatter must be a mapping",
                "",
                None,
            )],
        };
    };
    if collection
        .config
        .settings
        .explicit_type_keys
        .iter()
        .any(|key| frontmatter.contains_key(key))
    {
        return match_explicit_types(collection, registry, &record_path, frontmatter);
    }
    match_inferred_types(registry, &record_path, frontmatter)
}

/// Compose collection behavior for an already ordered set of matched types.
///
/// Schemas and uniqueness rules remain associated with their declaring type.
/// Compatible derived behavior coalesces, while conflicted values are omitted
/// and reported before a caller can apply them.
#[must_use]
pub fn compose_mdbase_type_behavior(
    registry: &MdbaseTypeRegistry,
    matched_types: &[String],
) -> MdbaseComposedTypeBehavior {
    let mut behavior = MdbaseComposedTypeBehavior::default();
    let mut declarations = BTreeMap::<CompositionKey, Vec<CompositionDeclaration>>::new();
    for type_name in matched_types {
        let Some(definition) = registry.get(type_name) else {
            behavior.diagnostics.push(MdbaseTypeCompositionDiagnostic {
                code: "type_not_found".to_string(),
                message: format!("matched type `{type_name}` is not defined"),
                field: "types".to_string(),
                type_names: vec![type_name.clone()],
                locations: Vec::new(),
            });
            continue;
        };
        behavior.types.push(definition.name.clone());
        behavior.schemas.push(MdbaseDeclaredTypeValue {
            type_name: definition.name.clone(),
            value: definition.schema.clone(),
        });
        if behavior.types.len() == 1 {
            behavior.display = definition
                .frontmatter
                .pointer("/collection/display")
                .cloned();
        }
        collect_type_behavior(definition, &mut behavior, &mut declarations);
    }
    resolve_composition_declarations(declarations, &mut behavior);
    behavior.diagnostics.sort_by(|left, right| {
        left.field
            .cmp(&right.field)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.type_names.cmp(&right.type_names))
    });
    behavior
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum CompositionKey {
    ReadDefault(String),
    Link(String),
    Path,
    Lifecycle { event: String, field: String },
    Projection(String),
}

#[derive(Debug, Clone)]
struct CompositionDeclaration {
    type_name: String,
    location: String,
    value: serde_json::Value,
}

fn collect_type_behavior(
    definition: &MdbaseTypeDefinition,
    behavior: &mut MdbaseComposedTypeBehavior,
    declarations: &mut BTreeMap<CompositionKey, Vec<CompositionDeclaration>>,
) {
    let collection = definition
        .frontmatter
        .get("collection")
        .and_then(serde_json::Value::as_object);
    if let Some(display) = collection.and_then(|value| value.get("display")) {
        behavior
            .display_by_type
            .insert(definition.name.clone(), display.clone());
    }
    if let Some(rules) = collection
        .and_then(|value| value.get("unique"))
        .and_then(serde_json::Value::as_array)
    {
        behavior
            .unique
            .extend(rules.iter().cloned().map(|value| MdbaseDeclaredTypeValue {
                type_name: definition.name.clone(),
                value,
            }));
    }
    collect_object_declarations(
        definition,
        collection.and_then(|value| value.get("read_defaults")),
        "collection.read_defaults",
        CompositionKey::ReadDefault,
        declarations,
    );
    collect_object_declarations(
        definition,
        collection.and_then(|value| value.get("links")),
        "collection.links",
        CompositionKey::Link,
        declarations,
    );
    collect_object_declarations(
        definition,
        collection.and_then(|value| value.get("projections")),
        "collection.projections",
        CompositionKey::Projection,
        declarations,
    );
    if let Some(path) = collection.and_then(|value| value.get("path")) {
        push_composition_declaration(
            declarations,
            CompositionKey::Path,
            definition,
            "collection.path".to_string(),
            path.clone(),
        );
    }
    collect_lifecycle_declarations(definition, declarations);
}

fn collect_object_declarations(
    definition: &MdbaseTypeDefinition,
    object: Option<&serde_json::Value>,
    location: &str,
    key: fn(String) -> CompositionKey,
    declarations: &mut BTreeMap<CompositionKey, Vec<CompositionDeclaration>>,
) {
    let Some(object) = object.and_then(serde_json::Value::as_object) else {
        return;
    };
    for (field, value) in object {
        push_composition_declaration(
            declarations,
            key(field.clone()),
            definition,
            format!("{location}.{field}"),
            value.clone(),
        );
    }
}

fn collect_lifecycle_declarations(
    definition: &MdbaseTypeDefinition,
    declarations: &mut BTreeMap<CompositionKey, Vec<CompositionDeclaration>>,
) {
    let Some(lifecycle) = definition
        .frontmatter
        .get("lifecycle")
        .and_then(serde_json::Value::as_object)
    else {
        return;
    };
    for (event, actions) in lifecycle {
        let actions = actions.as_array().map_or_else(
            || vec![actions],
            |actions| actions.iter().collect::<Vec<_>>(),
        );
        let mut by_field = BTreeMap::<String, Vec<serde_json::Value>>::new();
        for action in actions {
            let action = action
                .as_object()
                .expect("validated lifecycle action should be an object");
            let guard = action.get("if");
            let assignments = action
                .get("set")
                .and_then(serde_json::Value::as_object)
                .expect("validated lifecycle action should contain set");
            for (field, value) in assignments {
                by_field.entry(field.clone()).or_default().push(
                    serde_json::json!({"if": guard.cloned().unwrap_or(serde_json::Value::Null), "value": value}),
                );
            }
        }
        for (field, assignments) in by_field {
            push_composition_declaration(
                declarations,
                CompositionKey::Lifecycle {
                    event: event.clone(),
                    field: field.clone(),
                },
                definition,
                format!("lifecycle.{event}.{field}"),
                serde_json::Value::Array(assignments),
            );
        }
    }
}

fn push_composition_declaration(
    declarations: &mut BTreeMap<CompositionKey, Vec<CompositionDeclaration>>,
    key: CompositionKey,
    definition: &MdbaseTypeDefinition,
    location: String,
    value: serde_json::Value,
) {
    declarations
        .entry(key)
        .or_default()
        .push(CompositionDeclaration {
            type_name: definition.name.clone(),
            location,
            value,
        });
}

fn resolve_composition_declarations(
    declarations: BTreeMap<CompositionKey, Vec<CompositionDeclaration>>,
    behavior: &mut MdbaseComposedTypeBehavior,
) {
    for (key, values) in declarations {
        let first = &values[0].value;
        if values.iter().all(|value| value.value == *first) {
            insert_composed_value(behavior, key, first.clone());
            continue;
        }
        let mut type_names = values
            .iter()
            .map(|value| value.type_name.clone())
            .collect::<Vec<_>>();
        type_names.sort_by_key(|name| normalize_type_name(name));
        type_names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        let mut locations = values
            .iter()
            .map(|value| format!("{}:{}", value.type_name, value.location))
            .collect::<Vec<_>>();
        locations.sort_by_key(|location| location.to_ascii_lowercase());
        let field = composition_field(&key);
        behavior.diagnostics.push(MdbaseTypeCompositionDiagnostic {
            code: "type_conflict".to_string(),
            message: format!(
                "matched types define incompatible values for `{field}`: {}",
                type_names.join(", ")
            ),
            field,
            type_names,
            locations,
        });
    }
}

fn insert_composed_value(
    behavior: &mut MdbaseComposedTypeBehavior,
    key: CompositionKey,
    value: serde_json::Value,
) {
    match key {
        CompositionKey::ReadDefault(field) => {
            behavior.read_defaults.insert(field, value);
        }
        CompositionKey::Link(field) => {
            behavior.links.insert(field, value);
        }
        CompositionKey::Path => behavior.path = Some(value),
        CompositionKey::Lifecycle { event, field } => {
            behavior
                .lifecycle
                .entry(event)
                .or_default()
                .insert(field, value);
        }
        CompositionKey::Projection(field) => {
            behavior.projections.insert(field, value);
        }
    }
}

fn composition_field(key: &CompositionKey) -> String {
    match key {
        CompositionKey::ReadDefault(field)
        | CompositionKey::Link(field)
        | CompositionKey::Projection(field)
        | CompositionKey::Lifecycle { field, .. } => field.clone(),
        CompositionKey::Path => "path".to_string(),
    }
}

fn match_explicit_types(
    collection: &MdbaseCollection,
    registry: &MdbaseTypeRegistry,
    record_path: &str,
    frontmatter: &serde_json::Map<String, serde_json::Value>,
) -> MdbaseTypeMatchResult {
    let mut types = Vec::new();
    let mut seen = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for key in &collection.config.settings.explicit_type_keys {
        let Some(value) = frontmatter.get(key) else {
            continue;
        };
        let Some(declarations) = explicit_type_declarations(value) else {
            diagnostics.push(match_diagnostic(
                record_path,
                "type_declaration_invalid",
                "explicit type declaration must be a type name or non-empty list of type names",
                key,
                None,
            ));
            continue;
        };
        for declaration in declarations {
            if !valid_type_name(declaration) {
                diagnostics.push(match_diagnostic(
                    record_path,
                    "type_declaration_invalid",
                    format!("`{declaration}` is not a valid mdbase type name"),
                    key,
                    Some(declaration.to_string()),
                ));
                continue;
            }
            let normalized = normalize_type_name(declaration);
            if !seen.insert(normalized) {
                continue;
            }
            if let Some(definition) = registry.get(declaration) {
                types.push(definition.name.clone());
            } else {
                diagnostics.push(match_diagnostic(
                    record_path,
                    "type_not_found",
                    format!("explicit type `{declaration}` is not defined"),
                    key,
                    Some(declaration.to_string()),
                ));
            }
        }
    }
    sort_match_diagnostics(&mut diagnostics);
    MdbaseTypeMatchResult {
        types,
        mode: MdbaseTypeMatchMode::Explicit,
        diagnostics,
    }
}

fn explicit_type_declarations(value: &serde_json::Value) -> Option<Vec<&str>> {
    if let Some(name) = value.as_str() {
        return Some(vec![name]);
    }
    let values = value.as_array()?;
    if values.is_empty() {
        return None;
    }
    values
        .iter()
        .map(serde_json::Value::as_str)
        .collect::<Option<Vec<_>>>()
}

fn valid_type_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        && name.len() <= 128
        && characters
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

fn match_inferred_types(
    registry: &MdbaseTypeRegistry,
    record_path: &str,
    frontmatter: &serde_json::Map<String, serde_json::Value>,
) -> MdbaseTypeMatchResult {
    let mut types = Vec::new();
    for definition in registry.iter() {
        let Some(rule) = definition.frontmatter.get("match") else {
            continue;
        };
        if inferred_rule_matches(rule, record_path, frontmatter) {
            types.push(definition.name.clone());
        }
    }
    MdbaseTypeMatchResult {
        types,
        mode: MdbaseTypeMatchMode::Inferred,
        diagnostics: Vec::new(),
    }
}

fn inferred_rule_matches(
    rule: &serde_json::Value,
    record_path: &str,
    frontmatter: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    let rule = rule
        .as_object()
        .expect("type-file validation guarantees a match object");
    if let Some(patterns) = rule.get("path_glob") {
        if !match_glob_patterns(patterns).iter().any(|pattern| {
            mdbase_glob(pattern)
                .expect("match globs are validated while loading types")
                .compile_matcher()
                .is_match(record_path)
        }) {
            return false;
        }
    }
    if let Some(fields) = rule.get("fields_present") {
        let fields = fields
            .as_array()
            .expect("type-file validation guarantees fields_present is an array");
        if !fields.iter().all(|field| {
            resolve_match_field(
                frontmatter,
                field
                    .as_str()
                    .expect("validated field reference should be a string"),
            )
            .values
            .iter()
            .any(|value| !value.is_null())
        }) {
            return false;
        }
    }
    rule.get("where")
        .is_none_or(|predicates| structured_where_matches(predicates, frontmatter))
}

#[derive(Debug)]
struct ResolvedMatchField<'a> {
    exists: bool,
    values: Vec<&'a serde_json::Value>,
}

fn resolve_match_field<'a>(
    frontmatter: &'a serde_json::Map<String, serde_json::Value>,
    selector: &str,
) -> ResolvedMatchField<'a> {
    if selector.starts_with('/') {
        return resolve_json_pointer(frontmatter, selector);
    }
    let mut values: Vec<&serde_json::Value> = Vec::new();
    let mut exists = false;
    for (index, component) in selector.split('.').enumerate() {
        let expand = component.ends_with("[]");
        let key = component.strip_suffix("[]").unwrap_or(component);
        let selected = if index == 0 {
            frontmatter.get(key).into_iter().collect::<Vec<_>>()
        } else {
            values
                .iter()
                .filter_map(|value| value.as_object().and_then(|object| object.get(key)))
                .collect::<Vec<_>>()
        };
        if selected.is_empty() {
            return ResolvedMatchField {
                exists: false,
                values: Vec::new(),
            };
        }
        exists = true;
        values = if expand {
            selected
                .into_iter()
                .filter_map(serde_json::Value::as_array)
                .flatten()
                .collect()
        } else {
            selected
        };
    }
    ResolvedMatchField { exists, values }
}

fn resolve_json_pointer<'a>(
    frontmatter: &'a serde_json::Map<String, serde_json::Value>,
    selector: &str,
) -> ResolvedMatchField<'a> {
    let mut tokens = selector[1..].split('/').map(decode_json_pointer_token);
    let Some(first) = tokens.next() else {
        return ResolvedMatchField {
            exists: false,
            values: Vec::new(),
        };
    };
    let Some(mut value) = frontmatter.get(&first) else {
        return ResolvedMatchField {
            exists: false,
            values: Vec::new(),
        };
    };
    for token in tokens {
        let next = match value {
            serde_json::Value::Object(object) => object.get(&token),
            serde_json::Value::Array(values) => token
                .parse::<usize>()
                .ok()
                .and_then(|index| values.get(index)),
            _ => None,
        };
        let Some(next) = next else {
            return ResolvedMatchField {
                exists: false,
                values: Vec::new(),
            };
        };
        value = next;
    }
    ResolvedMatchField {
        exists: true,
        values: vec![value],
    }
}

fn decode_json_pointer_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

fn structured_where_matches(
    predicates: &serde_json::Value,
    frontmatter: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    predicates
        .as_object()
        .expect("type-file validation guarantees a where object")
        .iter()
        .all(|(selector, predicate)| {
            structured_predicate_matches(&resolve_match_field(frontmatter, selector), predicate)
        })
}

fn structured_predicate_matches(
    field: &ResolvedMatchField<'_>,
    predicate: &serde_json::Value,
) -> bool {
    let Some(operators) = predicate.as_object() else {
        return field.values.contains(&predicate);
    };
    if let Some(expected) = operators.get("exists").and_then(serde_json::Value::as_bool) {
        if field.exists != expected {
            return false;
        }
    }
    let value_operators = operators
        .iter()
        .filter(|(operator, _)| operator.as_str() != "exists")
        .collect::<Vec<_>>();
    value_operators.is_empty()
        || field.values.iter().any(|value| {
            !value.is_null()
                && value_operators
                    .iter()
                    .all(|(operator, operand)| match_operator(value, operator, operand))
        })
}

fn match_operator(value: &serde_json::Value, operator: &str, operand: &serde_json::Value) -> bool {
    match operator {
        "eq" => value == operand,
        "neq" => value != operand,
        "contains" => match value {
            serde_json::Value::String(value) => operand
                .as_str()
                .is_some_and(|operand| value.contains(operand)),
            serde_json::Value::Array(values) => values.contains(operand),
            _ => false,
        },
        "containsAll" => contains_requested_values(value, operand, true),
        "containsAny" => contains_requested_values(value, operand, false),
        "startsWith" => {
            string_operator(value, operand, |value, operand| value.starts_with(operand))
        }
        "endsWith" => string_operator(value, operand, |value, operand| value.ends_with(operand)),
        "matches" => value.as_str().is_some_and(|value| {
            Regex::new(
                operand
                    .as_str()
                    .expect("validated regex operand should be a string"),
            )
            .expect("match regexes are validated while loading types")
            .is_match(value)
        }),
        "gt" => compare_match_values(value, operand).is_some_and(std::cmp::Ordering::is_gt),
        "gte" => compare_match_values(value, operand).is_some_and(std::cmp::Ordering::is_ge),
        "lt" => compare_match_values(value, operand).is_some_and(std::cmp::Ordering::is_lt),
        "lte" => compare_match_values(value, operand).is_some_and(std::cmp::Ordering::is_le),
        "exists" => true,
        _ => unreachable!("type-file validation rejects unknown match operators"),
    }
}

fn contains_requested_values(
    value: &serde_json::Value,
    operand: &serde_json::Value,
    require_all: bool,
) -> bool {
    let (Some(values), Some(requested)) = (value.as_array(), operand.as_array()) else {
        return false;
    };
    if require_all {
        requested.iter().all(|item| values.contains(item))
    } else {
        requested.iter().any(|item| values.contains(item))
    }
}

fn string_operator(
    value: &serde_json::Value,
    operand: &serde_json::Value,
    operation: fn(&str, &str) -> bool,
) -> bool {
    value
        .as_str()
        .zip(operand.as_str())
        .is_some_and(|(value, operand)| operation(value, operand))
}

fn compare_match_values(
    left: &serde_json::Value,
    right: &serde_json::Value,
) -> Option<std::cmp::Ordering> {
    if let (Some(left), Some(right)) = (left.as_f64(), right.as_f64()) {
        return left.partial_cmp(&right);
    }
    let (Some(left), Some(right)) = (left.as_str(), right.as_str()) else {
        return None;
    };
    if let (Ok(left), Ok(right)) = (
        DateTime::parse_from_rfc3339(left),
        DateTime::parse_from_rfc3339(right),
    ) {
        return Some(left.cmp(&right));
    }
    if let (Ok(left), Ok(right)) = (
        NaiveDate::parse_from_str(left, "%Y-%m-%d"),
        NaiveDate::parse_from_str(right, "%Y-%m-%d"),
    ) {
        return Some(left.cmp(&right));
    }
    if let (Ok(left), Ok(right)) = (
        DateTime::parse_from_rfc3339(&format!("1970-01-01T{left}")),
        DateTime::parse_from_rfc3339(&format!("1970-01-01T{right}")),
    ) {
        return Some(left.cmp(&right));
    }
    Some(left.cmp(right))
}

fn mdbase_glob(pattern: &str) -> Result<globset::Glob, globset::Error> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(false)
        .build()
}

fn match_glob_patterns(value: &serde_json::Value) -> Vec<&str> {
    match value {
        serde_json::Value::String(pattern) => vec![pattern],
        serde_json::Value::Array(patterns) => patterns
            .iter()
            .map(|pattern| {
                pattern
                    .as_str()
                    .expect("type-file validation guarantees path_glob strings")
            })
            .collect(),
        _ => unreachable!("type-file validation guarantees path_glob strings"),
    }
}

fn match_diagnostic(
    path: &str,
    code: &str,
    message: impl Into<String>,
    field: &str,
    type_name: Option<String>,
) -> MdbaseTypeMatchDiagnostic {
    MdbaseTypeMatchDiagnostic {
        code: code.to_string(),
        message: message.into(),
        path: path.to_string(),
        field: field.to_string(),
        type_name,
    }
}

fn sort_match_diagnostics(diagnostics: &mut [MdbaseTypeMatchDiagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.field
            .cmp(&right.field)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.type_name.cmp(&right.type_name))
            .then_with(|| left.message.cmp(&right.message))
    });
}

fn build_mdbase_type_registry(
    collection: &MdbaseCollection,
    type_files: &[String],
) -> Result<MdbaseTypeRegistry, MdbaseTypeRegistryError> {
    let type_schema = bundled_mdbase_schema(&format!(
        "{MDBASE_CANONICAL_SCHEMA_BASE}type-file.schema.json"
    ))
    .expect("the type-file schema is part of the pinned bundle");
    let type_schema: serde_json::Value = serde_json::from_str(type_schema.json)
        .map_err(|source| MdbaseTypeRegistryError::BundledSchema { source })?;
    let mut candidates = BTreeMap::<String, Vec<MdbaseTypeDefinition>>::new();
    let mut diagnostics = Vec::new();
    let mut paths = type_files.to_vec();
    paths.sort();
    paths.dedup();

    for path in paths {
        match load_mdbase_type_file(collection, &path, &type_schema)? {
            TypeFileLoad::Valid(definition) => candidates
                .entry(definition.normalized_name.clone())
                .or_default()
                .push(definition),
            TypeFileLoad::Invalid(mut file_diagnostics) => {
                diagnostics.append(&mut file_diagnostics);
            }
        }
    }

    let mut types = BTreeMap::new();
    for (normalized_name, mut definitions) in candidates {
        definitions.sort_by(|left, right| left.path.cmp(&right.path));
        if definitions.len() == 1 {
            let definition = definitions.pop().expect("one definition should remain");
            types.insert(normalized_name, definition);
            continue;
        }
        let all_paths = definitions
            .iter()
            .map(|definition| definition.path.clone())
            .collect::<Vec<_>>();
        for definition in definitions {
            diagnostics.push(MdbaseTypeDiagnostic {
                code: "type_name_conflict".to_string(),
                message: format!(
                    "type name `{}` conflicts case-insensitively with another type definition",
                    definition.name
                ),
                path: definition.path.clone(),
                field: "name".to_string(),
                related_paths: all_paths
                    .iter()
                    .filter(|path| *path != &definition.path)
                    .cloned()
                    .collect(),
            });
        }
    }
    sort_type_diagnostics(&mut diagnostics);
    Ok(MdbaseTypeRegistry { types, diagnostics })
}

enum TypeFileLoad {
    Valid(MdbaseTypeDefinition),
    Invalid(Vec<MdbaseTypeDiagnostic>),
}

fn load_mdbase_type_file(
    collection: &MdbaseCollection,
    path: &str,
    type_schema: &serde_json::Value,
) -> Result<TypeFileLoad, MdbaseTypeRegistryError> {
    let source = secure_read_to_string(&collection.root, Path::new(path)).map_err(|source| {
        MdbaseTypeRegistryError::Read {
            path: collection.root.join(path),
            source,
        }
    })?;
    let frontmatter = match parse_type_frontmatter(&source, path) {
        Ok(frontmatter) => frontmatter,
        Err(diagnostic) => return Ok(TypeFileLoad::Invalid(vec![diagnostic])),
    };
    let absolute_path = collection.root.join(path);
    let schema_diagnostics = match validate_mdbase_schema_value_with_local_refs(
        type_schema,
        &frontmatter,
        &absolute_path,
        &collection.root,
    ) {
        Ok(diagnostics) => diagnostics,
        Err(error) => {
            return Ok(TypeFileLoad::Invalid(vec![type_diagnostic(
                path,
                "schema_invalid",
                format!("failed to compile the mdbase type-file schema: {error}"),
                "schema",
            )]));
        }
    };
    if !schema_diagnostics.is_empty() {
        return Ok(TypeFileLoad::Invalid(
            schema_diagnostics
                .into_iter()
                .map(|diagnostic| type_schema_diagnostic(path, diagnostic))
                .collect(),
        ));
    }
    let match_diagnostics = validate_type_match_rule(&frontmatter, path);
    if !match_diagnostics.is_empty() {
        return Ok(TypeFileLoad::Invalid(match_diagnostics));
    }

    let wrapped_schema = frontmatter
        .get("schema")
        .expect("validated type frontmatter should contain schema");
    let (schema, schema_ref) = if let Some(value) = wrapped_schema.get("value") {
        (value.clone(), None)
    } else {
        let reference = wrapped_schema
            .get("ref")
            .and_then(serde_json::Value::as_str)
            .expect("validated schema wrapper should contain value or ref")
            .to_string();
        (serde_json::json!({"$ref": reference}), Some(reference))
    };
    if let Err(error) = validate_mdbase_schema_value_with_local_refs(
        &schema,
        &serde_json::Value::Null,
        &absolute_path,
        &collection.root,
    ) {
        return Ok(TypeFileLoad::Invalid(vec![type_diagnostic(
            path,
            "schema_invalid",
            format!("failed to compile type schema: {error}"),
            "schema",
        )]));
    }

    let name = frontmatter
        .get("name")
        .and_then(serde_json::Value::as_str)
        .expect("validated type frontmatter should contain a string name")
        .to_string();
    Ok(TypeFileLoad::Valid(MdbaseTypeDefinition {
        normalized_name: normalize_type_name(&name),
        name,
        path: path.to_string(),
        version: frontmatter
            .get("version")
            .and_then(serde_json::Value::as_u64),
        description: frontmatter
            .get("description")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        schema,
        schema_ref,
        frontmatter,
    }))
}

fn parse_type_frontmatter(
    source: &str,
    path: &str,
) -> Result<serde_json::Value, MdbaseTypeDiagnostic> {
    let parsed = parse_document(source, &VaultConfig::default());
    let raw_frontmatter = parsed.raw_frontmatter.ok_or_else(|| {
        type_diagnostic(
            path,
            "type_frontmatter_missing",
            "mdbase type files require leading YAML frontmatter",
            "",
        )
    })?;
    let yaml = serde_yaml::from_str::<serde_yaml::Value>(&raw_frontmatter).map_err(|error| {
        type_diagnostic(
            path,
            "type_frontmatter_invalid",
            format!("failed to parse type frontmatter: {error}"),
            "",
        )
    })?;
    serde_json::to_value(yaml).map_err(|error| {
        type_diagnostic(
            path,
            "type_frontmatter_invalid",
            format!("type frontmatter is not JSON-compatible: {error}"),
            "",
        )
    })
}

fn validate_type_match_rule(
    frontmatter: &serde_json::Value,
    path: &str,
) -> Vec<MdbaseTypeDiagnostic> {
    let Some(rule) = frontmatter
        .get("match")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    if rule.contains_key("expr") {
        diagnostics.push(type_diagnostic(
            path,
            "unsupported_profile",
            "match.expr requires the cel_match profile, which is not enabled",
            "/match/expr",
        ));
    }
    if let Some(patterns) = rule.get("path_glob") {
        for pattern in match_glob_patterns(patterns) {
            if let Err(error) = mdbase_glob(pattern) {
                diagnostics.push(type_diagnostic(
                    path,
                    "match_pattern_invalid",
                    format!("invalid match.path_glob pattern `{pattern}`: {error}"),
                    "/match/path_glob",
                ));
            }
        }
    }
    if let Some(predicates) = rule.get("where").and_then(serde_json::Value::as_object) {
        for (selector, predicate) in predicates {
            let Some(pattern) = predicate
                .as_object()
                .and_then(|operators| operators.get("matches"))
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            if let Err(error) = Regex::new(pattern) {
                diagnostics.push(type_diagnostic(
                    path,
                    "match_pattern_invalid",
                    format!("invalid match.where regex `{pattern}`: {error}"),
                    &format!("/match/where/{selector}/matches"),
                ));
            }
        }
    }
    sort_type_diagnostics(&mut diagnostics);
    diagnostics
}

fn normalize_type_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn type_schema_diagnostic(path: &str, diagnostic: MdbaseSchemaDiagnostic) -> MdbaseTypeDiagnostic {
    MdbaseTypeDiagnostic {
        code: diagnostic.code,
        message: diagnostic.message,
        path: path.to_string(),
        field: diagnostic.instance_path,
        related_paths: Vec::new(),
    }
}

fn type_diagnostic(
    path: &str,
    code: &str,
    message: impl Into<String>,
    field: &str,
) -> MdbaseTypeDiagnostic {
    MdbaseTypeDiagnostic {
        code: code.to_string(),
        message: message.into(),
        path: path.to_string(),
        field: field.to_string(),
        related_paths: Vec::new(),
    }
}

fn sort_type_diagnostics(diagnostics: &mut [MdbaseTypeDiagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.field.cmp(&right.field))
            .then_with(|| left.message.cmp(&right.message))
            .then_with(|| left.related_paths.cmp(&right.related_paths))
    });
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

    fn write_type_file(root: &Path, relative: &str, frontmatter: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("type file should have a parent"))
            .expect("type directory should be created");
        fs::write(path, format!("---\n{frontmatter}---\n")).expect("type file should be written");
    }

    fn write_matching_type(root: &Path, relative: &str, name: &str, match_rule: &str) {
        write_type_file(
            root,
            relative,
            &format!(
                "kind: mdbase.type\nname: {name}\nmatch:\n{match_rule}schema:\n  dialect: json-schema-2020-12\n  value: {{type: object}}\n"
            ),
        );
    }

    fn write_behavior_type(root: &Path, relative: &str, name: &str, behavior: &str) {
        write_type_file(
            root,
            relative,
            &format!(
                "kind: mdbase.type\nname: {name}\nschema:\n  dialect: json-schema-2020-12\n  value: {{type: object}}\n{behavior}"
            ),
        );
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

    fn bundled_resource_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/mdbase/v0.3")
    }

    fn collect_files(root: &Path, directory: &Path, output: &mut Vec<(String, PathBuf)>) {
        for entry in fs::read_dir(directory).expect("bundled resource directory should be readable")
        {
            let entry = entry.expect("bundled resource entry should be readable");
            let file_type = entry
                .file_type()
                .expect("bundled resource file type should be readable");
            assert!(
                !file_type.is_symlink(),
                "bundled resources must not be symlinks"
            );
            if file_type.is_dir() {
                collect_files(root, &entry.path(), output);
            } else {
                let relative = relative_utf8_path(root, &entry.path())
                    .expect("bundled resource paths should be UTF-8");
                output.push((format!("./{relative}"), entry.path()));
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
    fn type_registry_loads_inline_and_referenced_schemas_case_insensitively() {
        let directory = tempdir().expect("temporary collection should exist");
        write_config(directory.path(), "spec_version: \"0.3.0\"\n");
        write_type_file(
            directory.path(),
            "_types/Task.md",
            r"kind: mdbase.type
name: Task
version: 2
description: Work item
schema:
  dialect: json-schema-2020-12
  value:
    type: object
    required: [title]
    properties:
      title: { type: string }
",
        );
        write_type_file(
            directory.path(),
            "_types/Contact.md",
            r"kind: mdbase.type
name: Contact
schema:
  dialect: json-schema-2020-12
  ref: ./contact.schema.json#/$defs/contact
",
        );
        fs::write(
            directory.path().join("_types/contact.schema.json"),
            r#"{"$defs":{"contact":{"type":"object","required":["name"]}}}"#,
        )
        .expect("referenced schema should be written");
        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");

        let registry = load_mdbase_type_registry(&collection).expect("types should load");

        assert_eq!(registry.len(), 2);
        assert!(registry.diagnostics.is_empty());
        let task = registry
            .get("tAsK")
            .expect("lookup should ignore ASCII case");
        assert_eq!(task.name, "Task");
        assert_eq!(task.normalized_name, "task");
        assert_eq!(task.version, Some(2));
        assert_eq!(task.description.as_deref(), Some("Work item"));
        let contact = registry
            .get("CONTACT")
            .expect("referenced type should load");
        assert_eq!(
            contact.schema_ref.as_deref(),
            Some("./contact.schema.json#/$defs/contact")
        );
        assert_eq!(
            registry
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            ["Contact", "Task"]
        );
    }

    #[test]
    fn type_registry_excludes_case_conflicts_independently_of_input_order() {
        let directory = tempdir().expect("temporary collection should exist");
        write_config(directory.path(), "spec_version: \"0.3.0\"\n");
        for (path, name) in [("_types/z.md", "Person"), ("_types/a.md", "person")] {
            write_type_file(
                directory.path(),
                path,
                &format!(
                    "kind: mdbase.type\nname: {name}\nschema:\n  dialect: json-schema-2020-12\n  value: {{type: object}}\n"
                ),
            );
        }
        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");

        let forward = build_mdbase_type_registry(
            &collection,
            &["_types/a.md".to_string(), "_types/z.md".to_string()],
        )
        .expect("registry should load");
        let reverse = build_mdbase_type_registry(
            &collection,
            &["_types/z.md".to_string(), "_types/a.md".to_string()],
        )
        .expect("registry should load");

        assert_eq!(forward, reverse);
        assert!(forward.is_empty());
        assert_eq!(forward.diagnostics.len(), 2);
        assert!(forward
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == "type_name_conflict"));
        assert_eq!(forward.diagnostics[0].path, "_types/a.md");
        assert_eq!(forward.diagnostics[0].related_paths, ["_types/z.md"]);
    }

    #[test]
    fn type_registry_reports_invalid_files_without_hiding_valid_types() {
        let directory = tempdir().expect("temporary collection should exist");
        write_config(directory.path(), "spec_version: \"0.3.0\"\n");
        write_file(directory.path(), "_types/missing-frontmatter.md");
        fs::write(
            directory.path().join("_types/missing-frontmatter.md"),
            "# Not a type\n",
        )
        .expect("invalid type file should be written");
        write_type_file(
            directory.path(),
            "_types/wrong-kind.md",
            "kind: note\nname: Wrong\nschema:\n  dialect: json-schema-2020-12\n  value: {type: object}\n",
        );
        write_type_file(
            directory.path(),
            "_types/broken-schema.md",
            "kind: mdbase.type\nname: Broken\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: string\n    pattern: '['\n",
        );
        write_type_file(
            directory.path(),
            "_types/valid.md",
            "kind: mdbase.type\nname: Valid\nschema:\n  dialect: json-schema-2020-12\n  value: {type: object}\n",
        );
        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");

        let registry = load_mdbase_type_registry(&collection).expect("registry should load");

        assert_eq!(registry.len(), 1);
        assert!(registry.get("valid").is_some());
        assert_eq!(
            registry
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["schema_const", "schema_invalid", "type_frontmatter_missing"])
        );
    }

    #[test]
    fn type_registry_accepts_the_pinned_data_contract_type_fixture() {
        let directory = tempdir().expect("temporary collection should exist");
        write_config(directory.path(), "spec_version: \"0.3.0\"\n");
        let type_directory = directory.path().join("_types");
        fs::create_dir_all(&type_directory).expect("type directory should exist");
        fs::write(
            type_directory.join("contact.md"),
            include_str!(
                "../resources/mdbase/v0.3/upstream/tests/fixtures/data-contracts/json-pointer-contact-type.md"
            ),
        )
        .expect("pinned type fixture should be written");
        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");

        let registry = load_mdbase_type_registry(&collection).expect("fixture type should load");

        assert!(registry.diagnostics.is_empty());
        let contact = registry
            .get("CONTACT_CARD")
            .expect("authored type name should be registered case-insensitively");
        assert_eq!(contact.name, "contact_card");
        assert!(contact.frontmatter.get("implements").is_some());
    }

    #[test]
    fn explicit_type_declarations_override_inference_and_preserve_key_order() {
        let directory = tempdir().expect("temporary collection should exist");
        write_config(
            directory.path(),
            "spec_version: \"0.3.0\"\nsettings:\n  explicit_type_keys: [kind, type, types]\n",
        );
        write_matching_type(
            directory.path(),
            "_types/publishable.md",
            "Publishable",
            "  fields_present: [title]\n",
        );
        write_matching_type(
            directory.path(),
            "_types/review.md",
            "Review",
            "  fields_present: [review]\n",
        );
        write_matching_type(
            directory.path(),
            "_types/task.md",
            "Task",
            "  fields_present: [task]\n",
        );
        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");
        let registry = load_mdbase_type_registry(&collection).expect("types should load");

        let result = match_mdbase_record_types(
            &collection,
            &registry,
            "notes/work.md",
            &serde_json::json!({
                "kind": ["review", "TASK"],
                "type": "task",
                "types": ["Missing"],
                "title": "Would otherwise infer Publishable"
            }),
        );

        assert_eq!(result.mode, MdbaseTypeMatchMode::Explicit);
        assert_eq!(result.types, ["Review", "Task"]);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].code, "type_not_found");
        assert_eq!(result.diagnostics[0].field, "types");
        assert_eq!(result.diagnostics[0].type_name.as_deref(), Some("Missing"));
    }

    #[test]
    fn invalid_explicit_declaration_still_suppresses_inference() {
        let directory = tempdir().expect("temporary collection should exist");
        write_config(directory.path(), "spec_version: \"0.3.0\"\n");
        write_matching_type(
            directory.path(),
            "_types/publishable.md",
            "Publishable",
            "  fields_present: [title]\n",
        );
        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");
        let registry = load_mdbase_type_registry(&collection).expect("types should load");

        let result = match_mdbase_record_types(
            &collection,
            &registry,
            "note.md",
            &serde_json::json!({"type": [], "title": "Present"}),
        );

        assert_eq!(result.mode, MdbaseTypeMatchMode::Explicit);
        assert!(result.types.is_empty());
        assert_eq!(result.diagnostics[0].code, "type_declaration_invalid");
    }

    #[test]
    fn inferred_types_match_conjunctively_in_canonical_name_order() {
        let directory = tempdir().expect("temporary collection should exist");
        write_config(directory.path(), "spec_version: \"0.3.0\"\n");
        write_matching_type(
            directory.path(),
            "_types/reviewable.md",
            "reviewable",
            "  path_glob: ['notes/review*.md', 'drafts/*.md']\n  where:\n    reviewStatus: {neq: done}\n",
        );
        write_matching_type(
            directory.path(),
            "_types/publishable.md",
            "Publishable",
            "  fields_present: [title, enabled, items]\n",
        );
        write_matching_type(
            directory.path(),
            "_types/jsonld.md",
            "jsonld_contact",
            "  fields_present: ['/metadata/@type']\n  where:\n    '/metadata/@type': Contact\n",
        );
        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");
        let registry = load_mdbase_type_registry(&collection).expect("types should load");

        let result = match_mdbase_record_types(
            &collection,
            &registry,
            "notes/review-one.md",
            &serde_json::json!({
                "title": "Review",
                "enabled": false,
                "items": [],
                "reviewStatus": "pending",
                "metadata": {"@type": "Contact"}
            }),
        );

        assert_eq!(result.mode, MdbaseTypeMatchMode::Inferred);
        assert_eq!(
            result.types,
            ["jsonld_contact", "Publishable", "reviewable"]
        );
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn structured_where_supports_match_operators_and_expanded_fields() {
        let directory = tempdir().expect("temporary collection should exist");
        write_config(directory.path(), "spec_version: \"0.3.0\"\n");
        write_matching_type(
            directory.path(),
            "_types/rich.md",
            "Rich",
            r"  where:
    status: {eq: open, neq: closed}
    score: {gte: 2, lt: 3}
    due: {gt: 2026-01-01, lte: 2026-12-31}
    title: {startsWith: Plan, endsWith: v1, matches: '^Plan.*v1$'}
    tags: {contains: rust, containsAll: [rust, markdown], containsAny: [other, rust]}
    authors[].name: {eq: Ada}
    missing: {exists: false}
",
        );
        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");
        let registry = load_mdbase_type_registry(&collection).expect("types should load");
        let matching = serde_json::json!({
            "status": "open",
            "score": 2.5,
            "due": "2026-08-26",
            "title": "Plan mdbase v1",
            "tags": ["rust", "markdown"],
            "authors": [{"name": "Grace"}, {"name": "Ada"}]
        });

        assert_eq!(
            match_mdbase_record_types(&collection, &registry, "rich.md", &matching).types,
            ["Rich"]
        );
        let null_field = serde_json::json!({
            "status": null,
            "score": 2.5,
            "due": "2026-08-26",
            "title": "Plan mdbase v1",
            "tags": ["rust", "markdown"],
            "authors": [{"name": "Ada"}]
        });
        assert!(
            match_mdbase_record_types(&collection, &registry, "rich.md", &null_field)
                .types
                .is_empty()
        );
    }

    #[test]
    fn invalid_match_patterns_and_unsupported_expr_exclude_types() {
        let directory = tempdir().expect("temporary collection should exist");
        write_config(directory.path(), "spec_version: \"0.3.0\"\n");
        write_matching_type(
            directory.path(),
            "_types/glob.md",
            "BadGlob",
            "  path_glob: '[invalid'\n",
        );
        write_matching_type(
            directory.path(),
            "_types/regex.md",
            "BadRegex",
            "  where:\n    title: {matches: '[invalid'}\n",
        );
        write_matching_type(
            directory.path(),
            "_types/expr.md",
            "NeedsCel",
            "  expr: {$expr: 'true'}\n",
        );
        write_matching_type(
            directory.path(),
            "_types/valid.md",
            "Valid",
            "  fields_present: [title]\n",
        );
        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");

        let registry = load_mdbase_type_registry(&collection).expect("registry should load");

        assert_eq!(registry.len(), 1);
        assert!(registry.get("valid").is_some());
        assert_eq!(
            registry
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["match_pattern_invalid", "unsupported_profile"])
        );
    }

    #[test]
    fn compatible_type_behavior_coalesces_with_declaring_type_provenance() {
        let directory = tempdir().expect("temporary collection should exist");
        write_config(directory.path(), "spec_version: \"0.3.0\"\n");
        let shared = r"collection:
  read_defaults: {status: open}
  links:
    owner: {target_type: Person, validate_exists: true}
  path: {pattern: 'notes/{id}.md'}
  projections:
    label: {expr: title}
lifecycle:
  on_update:
    set:
      updated: {now: true}
";
        write_behavior_type(directory.path(), "_types/zeta.md", "Zeta", shared);
        write_behavior_type(
            directory.path(),
            "_types/alpha.md",
            "Alpha",
            r"collection:
  read_defaults: {status: open}
  links:
    owner: {target_type: Person, validate_exists: true}
  unique:
    - {field: id, scope: type}
  path: {pattern: 'notes/{id}.md'}
  projections:
    label: {expr: title}
lifecycle:
  on_update:
    set:
      updated: {now: true}
",
        );
        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");
        let registry = load_mdbase_type_registry(&collection).expect("types should load");

        let behavior =
            compose_mdbase_type_behavior(&registry, &["Alpha".to_string(), "Zeta".to_string()]);

        assert!(behavior.diagnostics.is_empty());
        assert_eq!(behavior.types, ["Alpha", "Zeta"]);
        assert_eq!(behavior.schemas.len(), 2);
        assert_eq!(behavior.read_defaults["status"], "open");
        assert_eq!(behavior.links["owner"]["target_type"], "Person");
        assert_eq!(
            behavior.path.as_ref().expect("path should compose")["pattern"],
            "notes/{id}.md"
        );
        assert_eq!(behavior.projections["label"]["expr"], "title");
        assert_eq!(
            behavior.lifecycle["on_update"]["updated"]
                .as_array()
                .expect("assignments should be normalized")
                .len(),
            1
        );
        assert_eq!(behavior.unique.len(), 1);
        assert_eq!(behavior.unique[0].type_name, "Alpha");
    }

    #[test]
    fn incompatible_type_behavior_is_reported_and_left_unavailable() {
        let directory = tempdir().expect("temporary collection should exist");
        write_config(directory.path(), "spec_version: \"0.3.0\"\n");
        write_behavior_type(
            directory.path(),
            "_types/a.md",
            "A",
            r"collection:
  read_defaults: {status: open}
  links:
    owner: {target_type: Person}
  path: {pattern: 'a/{id}.md'}
  projections:
    label: {expr: title}
lifecycle:
  on_update:
    set:
      stamp: {literal: a}
",
        );
        write_behavior_type(
            directory.path(),
            "_types/b.md",
            "B",
            r"collection:
  read_defaults: {status: done}
  links:
    owner: {target_type: Team}
  path: {pattern: 'b/{id}.md'}
  projections:
    label: {expr: summary}
lifecycle:
  on_update:
    set:
      stamp: {literal: b}
",
        );
        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");
        let registry = load_mdbase_type_registry(&collection).expect("types should load");

        let behavior = compose_mdbase_type_behavior(&registry, &["B".to_string(), "A".to_string()]);

        assert_eq!(behavior.diagnostics.len(), 5);
        assert!(behavior
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == "type_conflict"));
        assert_eq!(
            behavior
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.field.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["label", "owner", "path", "stamp", "status"])
        );
        assert!(!behavior.read_defaults.contains_key("status"));
        assert!(!behavior.links.contains_key("owner"));
        assert!(behavior.path.is_none());
        assert!(!behavior.projections.contains_key("label"));
        assert!(!behavior.lifecycle.contains_key("on_update"));
        assert_eq!(behavior.diagnostics[0].type_names, ["A", "B"]);
    }

    #[test]
    fn flattened_display_comes_only_from_the_first_matched_type() {
        let directory = tempdir().expect("temporary collection should exist");
        write_config(directory.path(), "spec_version: \"0.3.0\"\n");
        write_behavior_type(directory.path(), "_types/plain.md", "Plain", "");
        write_behavior_type(
            directory.path(),
            "_types/visual.md",
            "Visual",
            "collection:\n  display: {name_field: title, icon: note}\n",
        );
        let collection = load_mdbase_collection(directory.path())
            .expect("config should load")
            .expect("collection should be detected");
        let registry = load_mdbase_type_registry(&collection).expect("types should load");

        let plain_first =
            compose_mdbase_type_behavior(&registry, &["Plain".to_string(), "Visual".to_string()]);
        let visual_first =
            compose_mdbase_type_behavior(&registry, &["Visual".to_string(), "Plain".to_string()]);

        assert!(plain_first.display.is_none());
        assert_eq!(
            visual_first.display.expect("display should flatten")["icon"],
            "note"
        );
        assert_eq!(plain_first.display_by_type.len(), 1);
        assert!(plain_first.display_by_type.contains_key("Visual"));
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

    #[test]
    fn bundled_upstream_assets_match_the_pinned_revision_digest() {
        let upstream = bundled_resource_root().join("upstream");
        let mut files = Vec::new();
        collect_files(&upstream, &upstream, &mut files);
        files.sort_by(|left, right| left.0.cmp(&right.0));

        let mut hasher = blake3::Hasher::new();
        for (relative, path) in &files {
            hasher.update(relative.as_bytes());
            hasher.update(&[0]);
            hasher.update(&fs::read(path).expect("bundled resource should be readable"));
        }

        assert_eq!(files.len(), 48);
        assert_eq!(
            hasher.finalize().to_hex().as_str(),
            MDBASE_BUNDLED_ASSET_DIGEST
        );
    }

    #[test]
    fn bundled_canonical_schemas_are_json_with_stable_mdbase_ids() {
        let schemas = bundled_resource_root().join("upstream/schemas");
        let mut files = Vec::new();
        collect_files(&schemas, &schemas, &mut files);
        let schema_files = files
            .into_iter()
            .filter(|(relative, _)| relative.ends_with(".schema.json"))
            .collect::<Vec<_>>();

        assert_eq!(schema_files.len(), 12);
        for (_, path) in schema_files {
            let schema: serde_json::Value = serde_json::from_slice(
                &fs::read(&path).expect("canonical schema should be readable"),
            )
            .expect("canonical schema should be valid JSON");
            assert!(schema
                .get("$id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| id.starts_with(MDBASE_CANONICAL_SCHEMA_BASE)));
        }
    }

    #[test]
    fn canonical_schema_registry_resolves_exact_ids_offline() {
        assert_eq!(MDBASE_BUNDLED_SCHEMAS.len(), 12);
        for bundled in MDBASE_BUNDLED_SCHEMAS {
            let parsed: serde_json::Value =
                serde_json::from_str(bundled.json).expect("bundled schema should be valid JSON");
            assert_eq!(
                parsed.get("$id").and_then(serde_json::Value::as_str),
                Some(bundled.canonical_id)
            );
            assert_eq!(bundled_mdbase_schema(bundled.canonical_id), Some(&bundled));
        }

        assert!(
            bundled_mdbase_schema("https://mdbase.dev/schemas/latest/config.schema.json").is_none()
        );
        assert!(bundled_mdbase_schema("https://example.com/schema.json").is_none());
    }

    #[test]
    fn schema_validation_asserts_formats_and_emits_canonical_codes() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["due", "created"],
            "properties": {
                "due": {"type": "string", "format": "date"},
                "created": {"type": "string", "format": "date-time"}
            },
            "additionalProperties": false
        });
        let value = serde_json::json!({"due": "2026-02-30", "created": "2026-08-22T10:00:00"});

        let diagnostics =
            validate_mdbase_schema_value(&schema, &value).expect("schema should compile");
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>(),
            ["format_invalid", "format_invalid"]
        );
    }

    #[test]
    fn schema_validation_resolves_fragment_references() {
        let schema = serde_json::json!({
            "$defs": {"identifier": {"type": "string", "minLength": 2}},
            "type": "object",
            "properties": {"id": {"$ref": "#/$defs/identifier"}}
        });
        let diagnostics = validate_mdbase_schema_value(&schema, &serde_json::json!({"id": "x"}))
            .expect("fragment reference should compile");

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "schema_min_length");
        assert_eq!(diagnostics[0].instance_path, "/id");
    }

    #[test]
    fn schema_validation_resolves_bounded_local_file_references() {
        let temporary = tempdir().expect("temporary collection should exist");
        let schema_directory = temporary.path().join("_types/schemas");
        fs::create_dir_all(&schema_directory).expect("schema directory should exist");
        let base_file = temporary.path().join("_types/contact.md");
        fs::write(&base_file, "type definition placeholder").expect("base file should be written");
        fs::write(
            schema_directory.join("identifier.json"),
            r#"{"type":"string","minLength":3}"#,
        )
        .expect("referenced schema should be written");
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"id": {"$ref": "schemas/identifier.json"}}
        });

        let diagnostics = validate_mdbase_schema_value_with_local_refs(
            &schema,
            &serde_json::json!({"id": "x"}),
            &base_file,
            temporary.path(),
        )
        .expect("local reference should compile");

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "schema_min_length");
        assert_eq!(diagnostics[0].instance_path, "/id");
    }

    #[test]
    fn schema_validation_resolves_canonical_schemas_offline() {
        let temporary = tempdir().expect("temporary collection should exist");
        let base_file = temporary.path().join("type.md");
        fs::write(&base_file, "type definition placeholder").expect("base file should be written");
        let schema = serde_json::json!({
            "$ref": "https://mdbase.dev/schemas/v0.3/diagnostic.schema.json"
        });

        let diagnostics = validate_mdbase_schema_value_with_local_refs(
            &schema,
            &serde_json::json!({}),
            &base_file,
            temporary.path(),
        )
        .expect("canonical reference should resolve from the bundle");

        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "schema_required"));
    }

    #[test]
    fn schema_validation_rejects_reference_cycles() {
        let temporary = tempdir().expect("temporary collection should exist");
        let base_file = temporary.path().join("base.json");
        let other_file = temporary.path().join("other.json");
        fs::write(&base_file, r#"{"$ref":"other.json"}"#).expect("base schema should write");
        fs::write(&other_file, r#"{"$ref":"base.json"}"#).expect("other schema should write");
        let schema = serde_json::json!({"$ref": "other.json"});

        let error = validate_mdbase_schema_value_with_local_refs(
            &schema,
            &serde_json::json!({}),
            &base_file,
            temporary.path(),
        )
        .expect_err("reference cycle should fail");

        assert!(error
            .to_string()
            .contains("schema reference cycle detected"));
    }

    #[test]
    fn schema_validation_rejects_references_outside_collection() {
        let temporary = tempdir().expect("temporary directory should exist");
        let collection = temporary.path().join("collection");
        fs::create_dir_all(&collection).expect("collection should exist");
        let base_file = collection.join("base.json");
        fs::write(&base_file, r#"{"$ref":"../outside.json"}"#).expect("base schema should write");
        fs::write(
            temporary.path().join("outside.json"),
            r#"{"type":"object"}"#,
        )
        .expect("outside schema should write");
        let schema = serde_json::json!({"$ref": "../outside.json"});

        let error = validate_mdbase_schema_value_with_local_refs(
            &schema,
            &serde_json::json!({}),
            &base_file,
            &collection,
        )
        .expect_err("escaping reference should fail");

        assert!(error.to_string().contains("escapes collection root"));
    }

    #[test]
    fn schema_validation_rejects_remote_and_oversized_references() {
        let temporary = tempdir().expect("temporary collection should exist");
        let base_file = temporary.path().join("base.json");
        fs::write(&base_file, "{}\n").expect("base schema should write");

        let remote_error = validate_mdbase_schema_value_with_local_refs(
            &serde_json::json!({"$ref": "https://example.com/schema.json"}),
            &serde_json::json!({}),
            &base_file,
            temporary.path(),
        )
        .expect_err("remote reference should fail offline");
        assert!(remote_error
            .to_string()
            .contains("remote schema reference is not allowed"));

        let oversized_file = temporary.path().join("oversized.json");
        fs::write(
            &oversized_file,
            vec![
                b' ';
                usize::try_from(MDBASE_SCHEMA_MAX_BYTES)
                    .expect("schema byte limit should fit usize")
                    + 1
            ],
        )
        .expect("oversized schema should write");
        let oversized_error = validate_mdbase_schema_value_with_local_refs(
            &serde_json::json!({"$ref": "oversized.json"}),
            &serde_json::json!({}),
            &base_file,
            temporary.path(),
        )
        .expect_err("oversized reference should fail before parsing");
        assert!(oversized_error
            .to_string()
            .contains("schema reference exceeds"));
    }

    #[test]
    fn provenance_records_the_source_pin_and_license() {
        let provenance = fs::read_to_string(bundled_resource_root().join("PROVENANCE.md"))
            .expect("provenance should be readable");
        let license = fs::read_to_string(bundled_resource_root().join("upstream/LICENSE"))
            .expect("license should be readable");

        assert!(provenance.contains(MDBASE_SPEC_UPSTREAM_URL));
        assert!(provenance.contains(MDBASE_SPEC_UPSTREAM_COMMIT));
        assert!(provenance.contains(MDBASE_SPEC_VERSION));
        assert!(provenance.contains(MDBASE_BUNDLED_ASSET_DIGEST));
        assert!(license.starts_with("MIT License\n"));
        assert!(license.contains("Copyright (c) 2025 Callum Alpass"));
    }
}
