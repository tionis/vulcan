//! Effective schema reports for one matched type composition
//! (`docs/specs/mdb/IMPLEMENTATION_CONTRACTS.md` §5).
//!
//! A form or App asks "what does a record of these types look like": which
//! fields exist, which are required in persisted frontmatter, which receive
//! read defaults, which the write pipeline generates, and which conflicts or
//! features stand in the way. The report is derived only from the composed
//! type behavior, so it can never be less restrictive than validation.

use super::{
    compose_mdbase_type_behavior, MdbaseDeclaredTypeValue, MdbaseTypeCompositionDiagnostic,
    MdbaseTypeRegistry,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseSchemaReport {
    /// Source revisions of the composed type files, in request order.
    pub types: Vec<MdbaseSchemaTypeSource>,
    /// Each type's effective schema; a record must satisfy every one.
    pub schemas: Vec<MdbaseDeclaredTypeValue>,
    pub fields: Vec<MdbaseSchemaField>,
    pub links: BTreeMap<String, serde_json::Value>,
    pub unique: Vec<MdbaseDeclaredTypeValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<serde_json::Value>,
    /// Composition conflicts and unknown types. A conflicted behavior is not
    /// applied, so writes of this composition fail until it is resolved.
    pub conflicts: Vec<MdbaseTypeCompositionDiagnostic>,
    /// Profiles or features a client needs to honor this composition.
    pub required_features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseSchemaTypeSource {
    pub name: String,
    pub path: String,
    pub revision: String,
    pub schema_revision: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseSchemaField {
    pub name: String,
    /// Types whose schema declares the field as a property.
    pub declared_by: Vec<String>,
    /// Required in persisted frontmatter by at least one type; a read
    /// default does not satisfy this.
    pub required: bool,
    /// Read default applied to the effective record when the field is missing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// Lifecycle events whose policy assigns the field.
    pub generated_by: Vec<String>,
    /// False when the write pipeline assigns the field or a schema marks it
    /// `readOnly`: a form edit would be overwritten or is not meant to change.
    pub editable: bool,
}

/// Build the report for one matched composition. Unknown type names appear as
/// `type_not_found` conflicts, exactly as record composition reports them.
#[must_use]
pub fn build_mdbase_schema_report(
    registry: &MdbaseTypeRegistry,
    matched_types: &[String],
) -> MdbaseSchemaReport {
    let behavior = compose_mdbase_type_behavior(registry, matched_types);
    let definitions = behavior
        .types
        .iter()
        .filter_map(|name| registry.get(name))
        .collect::<Vec<_>>();
    let mut fields = BTreeMap::<String, MdbaseSchemaField>::new();
    for schema in &behavior.schemas {
        let properties = schema
            .value
            .get("properties")
            .and_then(serde_json::Value::as_object);
        for (name, property) in properties.into_iter().flatten() {
            let entry = entry(&mut fields, name);
            entry.declared_by.push(schema.type_name.clone());
            if property.get("readOnly") == Some(&serde_json::Value::Bool(true)) {
                entry.editable = false;
            }
        }
        let required = schema
            .value
            .get("required")
            .and_then(serde_json::Value::as_array);
        for name in required
            .into_iter()
            .flatten()
            .filter_map(|name| name.as_str())
        {
            entry(&mut fields, name).required = true;
        }
    }
    for (name, value) in &behavior.read_defaults {
        entry(&mut fields, name).default = Some(value.clone());
    }
    for (event, assignments) in &behavior.lifecycle {
        for name in assignments.keys() {
            let entry = entry(&mut fields, name);
            entry.generated_by.push(event.clone());
            entry.editable = false;
        }
    }
    let mut required_features = BTreeSet::new();
    if !behavior.lifecycle.is_empty() {
        required_features.insert("vulcan.lifecycle.v1");
    }
    if !behavior.links.is_empty() {
        required_features.insert("links");
    }
    if !behavior.projections.is_empty() {
        required_features.insert("cel");
    }
    if definitions
        .iter()
        .any(|definition| definition.frontmatter.pointer("/match/expr").is_some())
    {
        required_features.insert("cel_match");
    }
    MdbaseSchemaReport {
        types: definitions
            .iter()
            .map(|definition| MdbaseSchemaTypeSource {
                name: definition.name.clone(),
                path: definition.path.clone(),
                revision: definition.revision.clone(),
                schema_revision: definition.schema_revision.clone(),
            })
            .collect(),
        schemas: behavior.schemas,
        fields: fields.into_values().collect(),
        links: behavior.links,
        unique: behavior.unique,
        path: behavior.path,
        conflicts: behavior.diagnostics,
        required_features: required_features
            .into_iter()
            .map(ToString::to_string)
            .collect(),
    }
}

fn entry<'a>(
    fields: &'a mut BTreeMap<String, MdbaseSchemaField>,
    name: &str,
) -> &'a mut MdbaseSchemaField {
    fields
        .entry(name.to_string())
        .or_insert_with(|| MdbaseSchemaField {
            name: name.to_string(),
            declared_by: Vec::new(),
            required: false,
            default: None,
            generated_by: Vec::new(),
            editable: true,
        })
}

#[cfg(test)]
mod tests;
