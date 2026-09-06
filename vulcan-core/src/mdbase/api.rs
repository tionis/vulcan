use super::{
    MdbaseConfigDiagnostic, MdbaseContractDiagnostic, MdbaseDiagnosticSeverity,
    MdbaseRecordDiagnostic, MdbaseRecordDiagnosticSeverity, MdbaseTypeDiagnostic,
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
}
