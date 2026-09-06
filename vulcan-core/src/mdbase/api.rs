use super::{
    MdbaseConfigDiagnostic, MdbaseContractDiagnostic, MdbaseDiagnosticSeverity,
    MdbaseRecordDiagnostic, MdbaseRecordDiagnosticSeverity, MdbaseRecordDocument,
    MdbaseTypeDiagnostic,
};
use serde::{Deserialize, Serialize};

/// Canonical mdbase v0.3 machine-readable diagnostic shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseDiagnostic {
    pub severity: MdbaseDiagnosticLevel,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MdbaseDiagnosticLevel {
    Info,
    Warning,
    Error,
}

/// Normative mdbase v0.3 operation result envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseOperationResult<T> {
    pub valid: bool,
    pub result: T,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

impl<T> MdbaseOperationResult<T> {
    #[must_use]
    pub fn new(valid: bool, result: T, diagnostics: Vec<MdbaseDiagnostic>) -> Self {
        Self {
            valid,
            result,
            diagnostics,
        }
    }
}

/// Exact projection defined by `record-document.schema.json`.
///
/// Vulcan-only display, contract-view, and diagnostic data belongs outside
/// this structure in operation results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseCompleteRecord {
    pub path: String,
    pub revision: String,
    pub types: Vec<String>,
    pub frontmatter: serde_json::Value,
    pub effective_frontmatter: serde_json::Value,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
    pub file: MdbaseCompleteRecordFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseCompleteRecordFile {
    pub name: String,
    pub folder: String,
    pub size: u64,
    pub mtime: String,
}

impl From<&MdbaseRecordDocument> for MdbaseCompleteRecord {
    fn from(value: &MdbaseRecordDocument) -> Self {
        Self {
            path: value.path.clone(),
            revision: value.revision.clone(),
            types: value.types.clone(),
            frontmatter: value.frontmatter.clone(),
            effective_frontmatter: value.effective_frontmatter.clone(),
            body: value.body.clone(),
            document: value.document.clone(),
            file: MdbaseCompleteRecordFile {
                name: value.file.name.clone(),
                folder: value.file.folder.clone(),
                size: value.file.size,
                mtime: value.file.mtime.clone().unwrap_or_default(),
            },
        }
    }
}

impl MdbaseDiagnostic {
    #[must_use]
    pub fn from_record(value: &MdbaseRecordDiagnostic) -> Self {
        Self {
            severity: match value.severity {
                MdbaseRecordDiagnosticSeverity::Warning => MdbaseDiagnosticLevel::Warning,
                MdbaseRecordDiagnosticSeverity::Error => MdbaseDiagnosticLevel::Error,
            },
            code: value.code.clone(),
            message: value.message.clone(),
            path: nonempty(&value.path),
            field: nonempty(&value.field),
            type_name: value.type_name.clone(),
            schema_location: value.schema_path.clone(),
            details: (!value.related_paths.is_empty())
                .then(|| serde_json::json!({ "related_paths": value.related_paths })),
        }
    }

    #[must_use]
    pub fn from_config(value: &MdbaseConfigDiagnostic) -> Self {
        Self {
            severity: match value.severity {
                MdbaseDiagnosticSeverity::Warning => MdbaseDiagnosticLevel::Warning,
            },
            code: value.code.clone(),
            message: value.message.clone(),
            path: value
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            field: nonempty(&value.field),
            type_name: None,
            schema_location: None,
            details: None,
        }
    }

    #[must_use]
    pub fn from_type(value: &MdbaseTypeDiagnostic) -> Self {
        Self {
            severity: MdbaseDiagnosticLevel::Error,
            code: value.code.clone(),
            message: value.message.clone(),
            path: nonempty(&value.path),
            field: nonempty(&value.field),
            type_name: None,
            schema_location: None,
            details: (!value.related_paths.is_empty())
                .then(|| serde_json::json!({ "related_paths": value.related_paths })),
        }
    }

    #[must_use]
    pub fn from_contract(value: &MdbaseContractDiagnostic) -> Self {
        Self {
            severity: MdbaseDiagnosticLevel::Error,
            code: value.code.clone(),
            message: value.message.clone(),
            path: nonempty(&value.path),
            field: nonempty(&value.field),
            type_name: value.type_name.clone(),
            schema_location: None,
            details: Some(serde_json::json!({
                "contract_id": value.contract_id,
                "contract_version": value.contract_version,
                "related_paths": value.related_paths,
            })),
        }
    }
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_diagnostic_omits_absent_optional_members() {
        let diagnostic = MdbaseDiagnostic {
            severity: MdbaseDiagnosticLevel::Error,
            code: "schema_required".to_string(),
            message: "required property is missing".to_string(),
            path: Some("tasks/a.md".to_string()),
            field: Some("title".to_string()),
            type_name: None,
            schema_location: None,
            details: None,
        };
        assert_eq!(
            serde_json::to_value(diagnostic).expect("diagnostic serializes"),
            serde_json::json!({
                "severity": "error",
                "code": "schema_required",
                "message": "required property is missing",
                "path": "tasks/a.md",
                "field": "title"
            })
        );
    }

    #[test]
    fn operation_and_complete_record_match_bundled_canonical_schemas() {
        use crate::mdbase::{
            bundled_mdbase_schema, validate_mdbase_schema_value,
            validate_mdbase_schema_value_with_local_refs, MDBASE_CANONICAL_SCHEMA_BASE,
        };
        use std::fs;

        let record = MdbaseCompleteRecord {
            path: "tasks/a.md".to_string(),
            revision: "sha256:abc".to_string(),
            types: vec!["task".to_string()],
            frontmatter: serde_json::json!({"type": "task"}),
            effective_frontmatter: serde_json::json!({"type": "task", "status": "open"}),
            body: "Body\n".to_string(),
            document: None,
            file: MdbaseCompleteRecordFile {
                name: "a.md".to_string(),
                folder: "tasks".to_string(),
                size: 42,
                mtime: "2026-09-06T12:00:00Z".to_string(),
            },
        };
        let record_value = serde_json::to_value(&record).expect("record serializes");
        let record_schema = bundled_mdbase_schema(&format!(
            "{MDBASE_CANONICAL_SCHEMA_BASE}record-document.schema.json"
        ))
        .expect("record schema");
        let record_schema = serde_json::from_str(record_schema.json).expect("record schema parses");
        assert!(validate_mdbase_schema_value(&record_schema, &record_value)
            .expect("record schema compiles")
            .is_empty());

        let envelope = MdbaseOperationResult::new(true, record, Vec::new());
        let envelope_value = serde_json::to_value(envelope).expect("envelope serializes");
        let operation_schema = bundled_mdbase_schema(&format!(
            "{MDBASE_CANONICAL_SCHEMA_BASE}operation-result.schema.json"
        ))
        .expect("operation schema");
        let operation_schema: serde_json::Value =
            serde_json::from_str(operation_schema.json).expect("operation schema parses");
        let directory = tempfile::tempdir().expect("temporary schema root");
        let operation_path = directory.path().join("operation-result.schema.json");
        fs::write(&operation_path, operation_schema.to_string()).expect("schema file");
        let diagnostic_schema = bundled_mdbase_schema(&format!(
            "{MDBASE_CANONICAL_SCHEMA_BASE}diagnostic.schema.json"
        ))
        .expect("diagnostic schema");
        fs::write(
            directory.path().join("diagnostic.schema.json"),
            diagnostic_schema.json,
        )
        .expect("diagnostic schema file");
        assert!(validate_mdbase_schema_value_with_local_refs(
            &operation_schema,
            &envelope_value,
            &operation_path,
            directory.path(),
        )
        .expect("operation schema compiles")
        .is_empty());
    }
}
