use super::{
    compose_mdbase_type_behavior, discover_mdbase_files, match_mdbase_record_types, mdbase_glob,
    resolve_match_field, validate_mdbase_schema_value_with_local_refs, MdbaseCollection,
    MdbaseTypeRegistry, MdbaseValidationLevel,
};
use crate::config::VaultConfig;
use crate::parser::{parse_document, ParseDiagnosticKind};
use crate::paths::secure_read_to_string;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const MDBASE_RECORD_MODEL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MdbaseRecordDiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseRecordDiagnostic {
    pub severity: MdbaseRecordDiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub path: String,
    pub field: String,
    pub type_name: Option<String>,
    pub schema_path: Option<String>,
    pub related_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseRecordFileMetadata {
    pub path: String,
    pub name: String,
    pub basename: String,
    pub ext: String,
    pub folder: String,
    pub size: u64,
    pub mtime: Option<String>,
    pub ctime: Option<String>,
}

/// One complete mdbase record, with persisted and derived values kept apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseRecordDocument {
    pub path: String,
    pub revision: String,
    pub types: Vec<String>,
    pub frontmatter: serde_json::Value,
    pub effective_frontmatter: serde_json::Value,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
    pub file: MdbaseRecordFileMetadata,
    /// Advisory metadata from the first matched type, never a validator.
    pub display: Option<serde_json::Value>,
    pub diagnostics: Vec<MdbaseRecordDiagnostic>,
}

impl MdbaseRecordDocument {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == MdbaseRecordDiagnosticSeverity::Error)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MdbaseRecordSet {
    pub records: Vec<MdbaseRecordDocument>,
}

impl MdbaseRecordSet {
    #[must_use]
    pub fn get(&self, path: &str) -> Option<&MdbaseRecordDocument> {
        self.records.iter().find(|record| record.path == path)
    }
}

#[derive(Debug)]
pub enum MdbaseRecordError {
    Discovery(super::MdbaseDiscoveryError),
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbasePathDiagnostic {
    pub code: String,
    pub message: String,
    pub field: String,
}

impl Display for MdbaseRecordError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Discovery(error) => {
                write!(formatter, "failed to discover mdbase records: {error}")
            }
            Self::Read { path, source } => {
                write!(
                    formatter,
                    "failed to read mdbase record {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for MdbaseRecordError {}

/// Load every discovered record in deterministic collection-path order.
pub fn load_mdbase_records(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    include_source: bool,
) -> Result<MdbaseRecordSet, MdbaseRecordError> {
    let discovery = discover_mdbase_files(collection).map_err(MdbaseRecordError::Discovery)?;
    let mut records = discovery
        .records
        .iter()
        .map(|path| load_mdbase_record(collection, types, path, include_source))
        .collect::<Result<Vec<_>, _>>()?;
    if collection.config.settings.validation != MdbaseValidationLevel::Off {
        validate_cross_file_uniqueness(collection, types, &mut records);
    }
    Ok(MdbaseRecordSet { records })
}

/// Load one collection-relative record without consulting Dataview inline fields.
pub fn load_mdbase_record(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    path: &str,
    include_source: bool,
) -> Result<MdbaseRecordDocument, MdbaseRecordError> {
    let source = secure_read_to_string(&collection.root, Path::new(path)).map_err(|source| {
        MdbaseRecordError::Read {
            path: collection.root.join(path),
            source,
        }
    })?;
    let metadata =
        fs::metadata(collection.root.join(path)).map_err(|source| MdbaseRecordError::Read {
            path: collection.root.join(path),
            source,
        })?;
    Ok(build_mdbase_record(
        collection,
        types,
        path,
        source,
        &metadata,
        include_source,
    ))
}

fn build_mdbase_record(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    path: &str,
    source: String,
    metadata: &fs::Metadata,
    include_source: bool,
) -> MdbaseRecordDocument {
    let parse_source = source.strip_prefix('\u{feff}').unwrap_or(&source);
    let parsed = parse_document(parse_source, &VaultConfig::default());
    let severity = validation_severity(collection.config.settings.validation);
    let mut diagnostics = Vec::new();
    let frontmatter = if parsed.raw_frontmatter.is_some() {
        if let Some(frontmatter) = parsed.frontmatter.as_ref().and_then(yaml_mapping_to_json) {
            frontmatter
        } else {
            let message = parsed
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.kind == ParseDiagnosticKind::MalformedFrontmatter)
                .map_or_else(
                    || "frontmatter must be a YAML mapping".to_string(),
                    |diagnostic| diagnostic.message.clone(),
                );
            diagnostics.push(record_diagnostic(
                severity,
                "frontmatter_invalid",
                message,
                path,
                "",
                None,
            ));
            serde_json::json!({})
        }
    } else {
        serde_json::json!({})
    };

    let matched = match_mdbase_record_types(collection, types, path, &frontmatter);
    diagnostics.extend(matched.diagnostics.into_iter().map(|diagnostic| {
        record_diagnostic(
            severity,
            &diagnostic.code,
            diagnostic.message,
            &diagnostic.path,
            &diagnostic.field,
            diagnostic.type_name,
        )
    }));

    let behavior = compose_mdbase_type_behavior(types, &matched.types);
    diagnostics.extend(
        behavior
            .diagnostics
            .into_iter()
            .map(|diagnostic| MdbaseRecordDiagnostic {
                severity,
                code: diagnostic.code,
                message: diagnostic.message,
                path: path.to_string(),
                field: diagnostic.field,
                type_name: None,
                schema_path: None,
                related_paths: diagnostic.locations,
            }),
    );
    if collection.config.settings.validation != MdbaseValidationLevel::Off {
        validate_record_schemas(
            collection,
            types,
            path,
            &frontmatter,
            &matched.types,
            &mut diagnostics,
        );
    }
    let effective_frontmatter = apply_mdbase_read_defaults(&frontmatter, &behavior.read_defaults);
    let revision = format!("sha256:{:x}", Sha256::digest(source.as_bytes()));
    let body = record_body(&source).to_string();
    let file = file_metadata(path, metadata);
    sort_record_diagnostics(&mut diagnostics);
    MdbaseRecordDocument {
        path: path.to_string(),
        revision,
        types: matched.types,
        frontmatter,
        effective_frontmatter,
        body,
        document: include_source.then_some(source),
        file,
        display: behavior.display,
        diagnostics,
    }
}

/// Render and validate the portable `{field}` path-pattern grammar.
pub fn render_mdbase_path_pattern(
    pattern: &str,
    frontmatter: &serde_json::Value,
) -> Result<String, MdbasePathDiagnostic> {
    let object = frontmatter
        .as_object()
        .ok_or_else(|| MdbasePathDiagnostic {
            code: "frontmatter_invalid".to_string(),
            message: "path policy requires a frontmatter mapping".to_string(),
            field: String::new(),
        })?;
    if pattern.is_empty() || pattern.starts_with('/') || pattern.contains('\\') {
        return Err(path_diagnostic(
            "path_pattern_invalid",
            "path pattern must be a non-empty relative forward-slash path",
            "collection.path.pattern",
        ));
    }

    let mut output = String::new();
    let mut remaining = pattern;
    while let Some(open) = remaining.find('{') {
        output.push_str(&remaining[..open]);
        let after_open = &remaining[open + 1..];
        let Some(close) = after_open.find('}') else {
            return Err(path_diagnostic(
                "path_pattern_invalid",
                "path pattern contains an unclosed placeholder",
                "collection.path.pattern",
            ));
        };
        let field = &after_open[..close];
        if field.is_empty() || field.contains(['{', '}', '.', '/', '\\']) {
            return Err(path_diagnostic(
                "path_pattern_invalid",
                "path placeholders must name one top-level frontmatter field",
                "collection.path.pattern",
            ));
        }
        let value = object
            .get(field)
            .filter(|value| !value.is_null())
            .ok_or_else(|| {
                path_diagnostic(
                    "path_value_missing",
                    format!("path placeholder `{field}` has no persisted value"),
                    field,
                )
            })?;
        let value = path_value_string(value).ok_or_else(|| {
            path_diagnostic(
                "path_value_invalid",
                format!("path placeholder `{field}` must resolve to a scalar value"),
                field,
            )
        })?;
        if value.is_empty() || value == "." || value == ".." || value.contains(['/', '\\']) {
            return Err(path_diagnostic(
                "path_value_invalid",
                format!("path placeholder `{field}` produces an unsafe path component"),
                field,
            ));
        }
        output.push_str(&value);
        remaining = &after_open[close + 1..];
    }
    if remaining.contains('}') {
        return Err(path_diagnostic(
            "path_pattern_invalid",
            "path pattern contains an unmatched closing brace",
            "collection.path.pattern",
        ));
    }
    output.push_str(remaining);
    validate_portable_record_path(&output)?;
    Ok(output)
}

fn path_value_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            None
        }
    }
}

fn validate_portable_record_path(path: &str) -> Result<(), MdbasePathDiagnostic> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(path_diagnostic(
            "path_traversal",
            "generated path must remain a normalized collection-relative path",
            "path",
        ));
    }
    Ok(())
}

fn path_diagnostic(code: &str, message: impl Into<String>, field: &str) -> MdbasePathDiagnostic {
    MdbasePathDiagnostic {
        code: code.to_string(),
        message: message.into(),
        field: field.to_string(),
    }
}

fn validate_cross_file_uniqueness(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    records: &mut [MdbaseRecordDocument],
) {
    let severity = validation_severity(collection.config.settings.validation);
    for definition in types.iter() {
        let Some(rules) = definition
            .frontmatter
            .pointer("/collection/unique")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for rule in rules {
            validate_uniqueness_rule(records, definition.name.as_str(), rule, severity);
        }
    }
    for record in records {
        sort_record_diagnostics(&mut record.diagnostics);
    }
}

fn validate_uniqueness_rule(
    records: &mut [MdbaseRecordDocument],
    declaring_type: &str,
    rule: &serde_json::Value,
    severity: MdbaseRecordDiagnosticSeverity,
) {
    let field = rule
        .get("field")
        .and_then(serde_json::Value::as_str)
        .expect("validated uniqueness rule has a field");
    let scope = rule
        .get("scope")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("collection");
    let path_glob = rule
        .get("path_glob")
        .and_then(serde_json::Value::as_str)
        .and_then(|pattern| mdbase_glob(pattern).ok())
        .map(|pattern| pattern.compile_matcher());

    let candidate_indices = records
        .iter()
        .enumerate()
        .filter(|(_, record)| match scope {
            "type" => record
                .types
                .iter()
                .any(|name| name.eq_ignore_ascii_case(declaring_type)),
            "path_glob" => path_glob
                .as_ref()
                .is_some_and(|matcher| matcher.is_match(&record.path)),
            _ => true,
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();

    let owners = candidate_indices
        .iter()
        .copied()
        .filter(|index| {
            records[*index]
                .types
                .iter()
                .any(|name| name.eq_ignore_ascii_case(declaring_type))
        })
        .collect::<Vec<_>>();
    for owner in owners {
        let owner_values = record_field_values(&records[owner], field);
        let mut related_paths = candidate_indices
            .iter()
            .copied()
            .filter(|candidate| *candidate != owner)
            .filter(|candidate| {
                let candidate_values = record_field_values(&records[*candidate], field);
                owner_values
                    .iter()
                    .any(|owner_value| candidate_values.contains(owner_value))
            })
            .map(|candidate| records[candidate].path.clone())
            .collect::<Vec<_>>();
        related_paths.sort();
        related_paths.dedup();
        if !related_paths.is_empty() {
            records[owner].diagnostics.push(MdbaseRecordDiagnostic {
                severity,
                code: "duplicate_value".to_string(),
                message: format!(
                    "field `{field}` duplicates another record in `{scope}` uniqueness scope"
                ),
                path: records[owner].path.clone(),
                field: field.to_string(),
                type_name: Some(declaring_type.to_string()),
                schema_path: None,
                related_paths,
            });
        }
    }
}

fn record_field_values<'a>(
    record: &'a MdbaseRecordDocument,
    field: &str,
) -> Vec<&'a serde_json::Value> {
    let Some(frontmatter) = record.frontmatter.as_object() else {
        return Vec::new();
    };
    let resolved = resolve_match_field(frontmatter, field);
    if !resolved.exists {
        return Vec::new();
    }
    resolved
        .values
        .into_iter()
        .filter(|value| !value.is_null())
        .collect()
}

/// Apply static read defaults only where the persisted top-level key is missing.
#[must_use]
pub fn apply_mdbase_read_defaults(
    persisted: &serde_json::Value,
    defaults: &std::collections::BTreeMap<String, serde_json::Value>,
) -> serde_json::Value {
    let mut effective = persisted.clone();
    let Some(object) = effective.as_object_mut() else {
        return effective;
    };
    for (field, value) in defaults {
        if !object.contains_key(field) {
            object.insert(field.clone(), value.clone());
        }
    }
    effective
}

fn validate_record_schemas(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    path: &str,
    frontmatter: &serde_json::Value,
    matched_types: &[String],
    diagnostics: &mut Vec<MdbaseRecordDiagnostic>,
) {
    let severity = validation_severity(collection.config.settings.validation);
    for type_name in matched_types {
        let Some(definition) = types.get(type_name) else {
            continue;
        };
        let type_path = collection.root.join(&definition.path);
        match validate_mdbase_schema_value_with_local_refs(
            &definition.schema,
            frontmatter,
            &type_path,
            &collection.root,
        ) {
            Ok(schema_diagnostics) => {
                diagnostics.extend(schema_diagnostics.into_iter().map(|diagnostic| {
                    MdbaseRecordDiagnostic {
                        severity,
                        code: diagnostic.code,
                        message: diagnostic.message,
                        path: path.to_string(),
                        field: diagnostic.instance_path,
                        type_name: Some(definition.name.clone()),
                        schema_path: Some(diagnostic.schema_path),
                        related_paths: Vec::new(),
                    }
                }));
            }
            Err(error) => diagnostics.push(MdbaseRecordDiagnostic {
                severity,
                code: "schema_invalid".to_string(),
                message: format!(
                    "failed to compile schema for type `{}`: {error}",
                    definition.name
                ),
                path: path.to_string(),
                field: String::new(),
                type_name: Some(definition.name.clone()),
                schema_path: None,
                related_paths: vec![definition.path.clone()],
            }),
        }
    }
}

fn yaml_mapping_to_json(value: &serde_yaml::Value) -> Option<serde_json::Value> {
    if !value.is_mapping() {
        return None;
    }
    serde_json::to_value(value)
        .ok()
        .filter(serde_json::Value::is_object)
}

fn record_body(source: &str) -> &str {
    let without_bom = source.strip_prefix('\u{feff}').unwrap_or(source);
    let opening_len = if without_bom.starts_with("---\r\n") {
        5
    } else if without_bom.starts_with("---\n") {
        4
    } else {
        return source;
    };
    let mut offset = opening_len;
    for line in without_bom[opening_len..].split_inclusive('\n') {
        let marker = line.trim_end_matches(['\r', '\n']);
        offset += line.len();
        if marker == "---" || marker == "..." {
            return &without_bom[offset..];
        }
    }
    ""
}

fn file_metadata(path: &str, metadata: &fs::Metadata) -> MdbaseRecordFileMetadata {
    let path_object = Path::new(path);
    let name = path_object
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let ext = path_object
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_string();
    let basename = path_object
        .file_stem()
        .and_then(|basename| basename.to_str())
        .unwrap_or_default()
        .to_string();
    let folder = path_object
        .parent()
        .and_then(|folder| folder.to_str())
        .unwrap_or_default()
        .replace('\\', "/");
    MdbaseRecordFileMetadata {
        path: path.to_string(),
        name,
        basename,
        ext,
        folder,
        size: metadata.len(),
        mtime: metadata.modified().ok().map(format_system_time),
        ctime: metadata.created().ok().map(format_system_time),
    }
}

fn format_system_time(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn validation_severity(level: MdbaseValidationLevel) -> MdbaseRecordDiagnosticSeverity {
    match level {
        MdbaseValidationLevel::Off | MdbaseValidationLevel::Warn => {
            MdbaseRecordDiagnosticSeverity::Warning
        }
        MdbaseValidationLevel::Error => MdbaseRecordDiagnosticSeverity::Error,
    }
}

fn record_diagnostic(
    severity: MdbaseRecordDiagnosticSeverity,
    code: &str,
    message: String,
    path: &str,
    field: &str,
    type_name: Option<String>,
) -> MdbaseRecordDiagnostic {
    MdbaseRecordDiagnostic {
        severity,
        code: code.to_string(),
        message,
        path: path.to_string(),
        field: field.to_string(),
        type_name,
        schema_path: None,
        related_paths: Vec::new(),
    }
}

fn sort_record_diagnostics(diagnostics: &mut [MdbaseRecordDiagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.field.cmp(&right.field))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.type_name.cmp(&right.type_name))
            .then_with(|| left.message.cmp(&right.message))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdbase::{load_mdbase_collection, load_mdbase_type_registry};
    use std::fs;
    use tempfile::tempdir;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture directory");
        fs::write(path, contents).expect("fixture file");
    }

    fn collection_with_type() -> (tempfile::TempDir, MdbaseCollection, MdbaseTypeRegistry) {
        let directory = tempdir().expect("collection directory");
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        write(
            &directory.path().join("_types/task.md"),
            "---\nkind: mdbase.type\nname: task\nversion: 1\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n---\n",
        );
        let collection = load_mdbase_collection(directory.path())
            .expect("collection should load")
            .expect("collection should exist");
        let types = load_mdbase_type_registry(&collection).expect("types should load");
        (directory, collection, types)
    }

    #[test]
    fn record_keeps_exact_source_body_persisted_values_and_metadata_distinct() {
        let (directory, collection, types) = collection_with_type();
        let source = "\u{feff}---\r\ntype: task\r\nstatus: null\r\nempty: \"\"\r\nitems: []\r\n---\r\n# Body\r\ninline:: excluded\r\n";
        write(&directory.path().join("tasks/item.md"), source);

        let record = load_mdbase_record(&collection, &types, "tasks/item.md", true)
            .expect("record should load");

        assert_eq!(record.document.as_deref(), Some(source));
        assert_eq!(record.body, "# Body\r\ninline:: excluded\r\n");
        assert_eq!(record.frontmatter["status"], serde_json::Value::Null);
        assert_eq!(record.frontmatter["empty"], "");
        assert_eq!(record.frontmatter["items"], serde_json::json!([]));
        assert!(!record
            .frontmatter
            .as_object()
            .unwrap()
            .contains_key("inline"));
        assert_eq!(record.effective_frontmatter, record.frontmatter);
        assert_eq!(record.types, vec!["task"]);
        assert_eq!(record.file.path, "tasks/item.md");
        assert_eq!(record.file.name, "item.md");
        assert_eq!(record.file.basename, "item");
        assert_eq!(record.file.ext, "md");
        assert_eq!(record.file.folder, "tasks");
        assert_eq!(record.file.size, source.len() as u64);
        assert!(record.revision.starts_with("sha256:"));
    }

    #[test]
    fn absent_frontmatter_is_empty_and_source_is_opt_in() {
        let (directory, collection, types) = collection_with_type();
        write(
            &directory.path().join("plain.md"),
            "Body before inline:: value\n",
        );

        let record =
            load_mdbase_record(&collection, &types, "plain.md", false).expect("record should load");

        assert_eq!(record.frontmatter, serde_json::json!({}));
        assert_eq!(record.body, "Body before inline:: value\n");
        assert!(record.document.is_none());
        assert!(record.types.is_empty());
    }

    #[test]
    fn records_are_loaded_in_collection_path_order() {
        let (directory, collection, types) = collection_with_type();
        write(&directory.path().join("z.md"), "z\n");
        write(&directory.path().join("a.md"), "a\n");

        let records =
            load_mdbase_records(&collection, &types, false).expect("record set should load");

        assert_eq!(
            records
                .records
                .iter()
                .map(|record| record.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.md", "z.md"]
        );
    }

    #[test]
    fn defaults_preserve_explicit_states_and_required_validates_persisted_values() {
        let directory = tempdir().expect("collection directory");
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        write(
            &directory.path().join("_types/task.md"),
            "---\nkind: mdbase.type\nname: task\nversion: 1\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [type, status]\n    properties:\n      type: {const: task}\n      status: {}\n      empty: {type: string}\n      items: {type: array}\ncollection:\n  read_defaults:\n    status: open\n    empty: default\n    items: [default]\n---\n",
        );
        write(
            &directory.path().join("missing.md"),
            "---\ntype: task\n---\n",
        );
        write(
            &directory.path().join("states.md"),
            "---\ntype: task\nstatus: null\nempty: \"\"\nitems: []\n---\n",
        );
        let collection = load_mdbase_collection(directory.path())
            .expect("collection should load")
            .expect("collection should exist");
        let types = load_mdbase_type_registry(&collection).expect("types should load");

        let missing = load_mdbase_record(&collection, &types, "missing.md", false)
            .expect("missing record should load");
        assert!(!missing
            .frontmatter
            .as_object()
            .unwrap()
            .contains_key("status"));
        assert_eq!(missing.effective_frontmatter["status"], "open");
        assert!(missing
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "schema_required"
                && diagnostic.type_name.as_deref() == Some("task")));

        let states = load_mdbase_record(&collection, &types, "states.md", false)
            .expect("states record should load");
        assert_eq!(
            states.effective_frontmatter["status"],
            serde_json::Value::Null
        );
        assert_eq!(states.effective_frontmatter["empty"], "");
        assert_eq!(states.effective_frontmatter["items"], serde_json::json!([]));
        assert!(!states
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "schema_required"));
    }

    #[test]
    fn uniqueness_display_and_portable_path_policy_are_deterministic() {
        let directory = tempdir().expect("collection directory");
        write(
            &directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        );
        write(
            &directory.path().join("_types/task.md"),
            "---\nkind: mdbase.type\nname: task\nversion: 1\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\ncollection:\n  display: {name_field: title, icon: check}\n  unique:\n    - {field: id, scope: type}\n    - {field: slug, scope: path_glob, path_glob: 'published/**/*.md'}\n  path:\n    pattern: 'tasks/{id}.md'\n---\n",
        );
        for (path, id, slug) in [
            ("published/b.md", "same", "public"),
            ("published/a.md", "same", "public"),
            ("drafts/c.md", "same", "public"),
        ] {
            write(
                &directory.path().join(path),
                &format!("---\ntype: task\nid: {id}\nslug: {slug}\ntitle: {path}\n---\n"),
            );
        }
        let collection = load_mdbase_collection(directory.path())
            .expect("collection should load")
            .expect("collection should exist");
        let types = load_mdbase_type_registry(&collection).expect("types should load");

        let records = load_mdbase_records(&collection, &types, false).expect("records should load");
        let first = records.get("published/a.md").expect("first record");
        let duplicate_diagnostics = first
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "duplicate_value")
            .collect::<Vec<_>>();
        assert_eq!(duplicate_diagnostics.len(), 2);
        assert_eq!(
            duplicate_diagnostics[0].related_paths,
            vec!["drafts/c.md", "published/b.md"]
        );
        assert_eq!(
            duplicate_diagnostics[1].related_paths,
            vec!["published/b.md"]
        );
        assert_eq!(first.display.as_ref().unwrap()["icon"], "check");

        assert_eq!(
            render_mdbase_path_pattern("tasks/{id}.md", &serde_json::json!({"id": "new-task"}))
                .expect("safe path"),
            "tasks/new-task.md"
        );
        assert_eq!(
            render_mdbase_path_pattern("tasks/{id}.md", &serde_json::json!({"id": "../escape"}))
                .expect_err("unsafe placeholder")
                .code,
            "path_value_invalid"
        );
        assert_eq!(
            render_mdbase_path_pattern("../{id}.md", &serde_json::json!({"id": "escape"}))
                .expect_err("traversing pattern")
                .code,
            "path_traversal"
        );
    }
}
