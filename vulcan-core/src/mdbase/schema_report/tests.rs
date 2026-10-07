use super::*;
use crate::mdbase::{load_mdbase_collection, load_mdbase_type_registry};
use serde_json::json;
use std::fs;

fn registry(types: &[(&str, &str)]) -> (tempfile::TempDir, MdbaseTypeRegistry) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("mdbase.yaml"),
        "spec_version: \"0.3.0\"\n",
    )
    .unwrap();
    fs::create_dir_all(directory.path().join("_types")).unwrap();
    for (name, body) in types {
        fs::write(
            directory.path().join(format!("_types/{name}.md")),
            format!("---\nkind: mdbase.type\nname: {name}\n{body}---\n"),
        )
        .unwrap();
    }
    let collection = load_mdbase_collection(directory.path()).unwrap().unwrap();
    let registry = load_mdbase_type_registry(&collection).unwrap();
    assert!(
        registry.diagnostics.is_empty(),
        "{:?}",
        registry.diagnostics
    );
    (directory, registry)
}

const TASK: &str = "schema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [title]\n    properties:\n      title: {type: string}\n      status: {type: string}\n      id: {type: string}\n      created: {type: string, readOnly: true}\ncollection:\n  read_defaults: {status: open}\n  links:\n    project: {target_type: any}\nlifecycle:\n  on_create:\n    set:\n      id: {ulid: true}\n";
const TRACKED: &str = "match:\n  expr:\n    $expr: 'present.raw.estimate'\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [estimate]\n    properties:\n      estimate: {type: integer}\n      title: {type: string}\n";

#[test]
fn composed_fields_report_required_defaults_generation_and_editability() {
    let (_directory, registry) = registry(&[("task", TASK), ("tracked", TRACKED)]);
    let report =
        build_mdbase_schema_report(&registry, &["task".to_string(), "tracked".to_string()]);
    assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);
    assert_eq!(
        report
            .types
            .iter()
            .map(|source| (source.name.as_str(), source.path.as_str()))
            .collect::<Vec<_>>(),
        [("task", "_types/task.md"), ("tracked", "_types/tracked.md")]
    );
    assert_eq!(
        report.types[0].revision,
        registry.get("task").unwrap().revision
    );
    assert_eq!(report.schemas.len(), 2);
    let fields = report
        .fields
        .iter()
        .map(|field| (field.name.as_str(), serde_json::to_value(field).unwrap()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        fields["title"],
        json!({"name": "title", "declared_by": ["task", "tracked"], "required": true,
            "generated_by": [], "editable": true})
    );
    assert_eq!(
        fields["status"],
        json!({"name": "status", "declared_by": ["task"], "required": false,
            "default": "open", "generated_by": [], "editable": true})
    );
    assert_eq!(fields["id"]["generated_by"], json!(["on_create"]));
    assert_eq!(fields["id"]["editable"], false);
    assert_eq!(fields["created"]["editable"], false);
    assert_eq!(fields["estimate"]["required"], true);
    assert_eq!(fields["estimate"]["declared_by"], json!(["tracked"]));
    assert!(report.links.contains_key("project"));
    assert_eq!(
        report.required_features,
        ["cel_match", "links", "vulcan.lifecycle.v1"]
    );
}

#[test]
fn conflicts_and_unknown_types_are_reported_not_hidden() {
    let other = TASK.replace("status: open", "status: closed");
    let (_directory, registry) = registry(&[("task", TASK), ("other", &other)]);
    let report = build_mdbase_schema_report(
        &registry,
        &["task".to_string(), "other".to_string(), "ghost".to_string()],
    );
    let codes = report
        .conflicts
        .iter()
        .map(|conflict| conflict.code.as_str())
        .collect::<BTreeSet<_>>();
    assert!(codes.contains("type_not_found"), "{codes:?}");
    assert!(codes.contains("type_conflict"), "{codes:?}");
    // A conflicted default is not applied, so the report does not offer one.
    assert!(report
        .fields
        .iter()
        .find(|field| field.name == "status")
        .unwrap()
        .default
        .is_none());
    assert_eq!(report.types.len(), 2);
}
