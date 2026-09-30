use super::*;
use serde_json::json;
use std::cell::RefCell;
use tempfile::tempdir;

#[test]
fn type_registry_reuses_one_snapshot_and_keeps_nested_reference_bases() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("_types")).unwrap();
    fs::create_dir(dir.path().join("schemas")).unwrap();
    fs::write(dir.path().join("mdbase.yaml"), "spec_version: '0.3.0'\n").unwrap();
    fs::write(dir.path().join("_types/task.md"), "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  ref: ../schemas/task.yaml\n---\n").unwrap();
    fs::write(
        dir.path().join("schemas/task.yaml"),
        "type: object\nproperties:\n  id: {$ref: id.txt}\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("schemas/id.txt"),
        "type: string\nminLength: 3\n",
    )
    .unwrap();
    let collection = load_mdbase_collection(dir.path()).unwrap().unwrap();
    let types = load_mdbase_type_registry(&collection).unwrap();
    assert!(types.diagnostics.is_empty(), "{:?}", types.diagnostics);
    assert_eq!(
        types.compiled_schema("TASK").unwrap().dependencies().len(),
        2
    );
    let cloned = types.clone();
    assert!(std::ptr::eq(
        types.compiled_schema("task").unwrap(),
        cloned.compiled_schema("task").unwrap()
    ));
    let record = "---\ntype: task\nid: ab\n---\nBody\n";
    let before = analyze_mdbase_record_source(&collection, &types, "a.md", record);
    assert!(before
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "schema_min_length"));
    fs::write(
        dir.path().join("schemas/id.txt"),
        "type: string\nminLength: 1\n",
    )
    .unwrap();
    let fresh = load_mdbase_type_registry(&collection).unwrap();
    assert!(fresh.diagnostics.is_empty());
    assert!(
        analyze_mdbase_record_source(&collection, &fresh, "a.md", record)
            .diagnostics
            .is_empty()
    );
    assert_ne!(types, fresh);
    dir.close().unwrap();
    // Reusing the registry cannot silently read a different control revision,
    // and record validation cannot recompile/reopen schemas per candidate.
    for _ in 0..10 {
        assert_eq!(
            analyze_mdbase_record_source(&collection, &cloned, "a.md", record),
            before
        );
    }
}

#[test]
fn wrapper_introspection_and_validator_use_the_same_authorized_bytes() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("type.md");
    fs::write(&base, "placeholder").unwrap();
    fs::write(
        dir.path().join("schema.yaml"),
        "$defs:\n  id: {type: string, minLength: 3}\n",
    )
    .unwrap();
    let calls = RefCell::new(Vec::new());
    let (value, compiled) = compile_mdbase_schema_wrapper(
        &json!({"ref": "schema.yaml#/$defs/id"}),
        &base,
        dir.path(),
        &|path| {
            calls.borrow_mut().push(path.to_path_buf());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(value, json!({"type": "string", "minLength": 3}));
    assert_eq!(
        *calls.borrow(),
        [PathBuf::from("type.md"), PathBuf::from("schema.yaml")]
    );
    assert!(!compiled.validate(&json!("x")).is_empty());
    assert!(compiled.validate(&json!("valid")).is_empty());
    for invalid in [
        json!({"ref":"schema.yaml","value":{}}),
        json!({"ref": 7, "value": {}}),
        json!({"ref": 7}),
    ] {
        assert!(
            compile_mdbase_schema_wrapper(&invalid, &base, dir.path(), &|_| {
                panic!("malformed wrappers must fail before file access")
            })
            .is_err()
        );
    }
}

#[test]
fn wrapper_fragment_selection_matches_the_pinned_local_pointer_root() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("type.md");
    fs::write(&base, "placeholder").unwrap();
    fs::write(dir.path().join("schema.json"), r##"{"$defs":{"nested":{"$ref":"#/$defs/only_nested","$defs":{"only_nested":{"type":"string"}}}}}"##).unwrap();
    // MDB wrapper selection precedes compilation, unlike a JSON Schema $ref.
    let (_, compiled) = compile_mdbase_schema_wrapper(
        &json!({"ref":"schema.json#/$defs/nested"}),
        &base,
        dir.path(),
        &|_| Ok(()),
    )
    .unwrap();
    assert!(compiled.validate(&json!("ok")).is_empty());
    assert!(!compiled.validate(&json!(7)).is_empty());
    assert!(compile_mdbase_schema_with_local_refs(
        &json!({"$ref":"schema.json#/$defs/nested"}),
        &base,
        dir.path(),
        &|_| Ok(())
    )
    .is_err());
}

#[test]
fn fragment_schema_required_diagnostics_identify_the_missing_nested_property() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("_types")).unwrap();
    fs::write(dir.path().join("mdbase.yaml"), "spec_version: '0.3.0'\n").unwrap();
    fs::write(dir.path().join("_types/contact.md"), "---\nkind: mdbase.type\nname: contact\nschema:\n  dialect: json-schema-2020-12\n  ref: ../schema.json#/$defs/contact\n---\n").unwrap();
    fs::write(dir.path().join("schema.json"), r#"{"$defs":{"contact":{"type":"object","required":["name"],"properties":{"address":{"type":"object","required":["street"]}}}}}"#).unwrap();
    let collection = load_mdbase_collection(dir.path()).unwrap().unwrap();
    let types = load_mdbase_type_registry(&collection).unwrap();
    assert!(types.diagnostics.is_empty());
    let record = analyze_mdbase_record_source(
        &collection,
        &types,
        "a.md",
        "---\ntype: contact\nname: Name\naddress: {}\n---\n",
    );
    assert!(
        record
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "schema_required"
                && diagnostic.field == "/address/street"),
        "{:?}",
        record.diagnostics
    );
}

#[test]
fn diagnostics_retain_distinct_missing_and_actual_unexpected_properties() {
    let diagnostics = validate_mdbase_schema_value(
        &json!({"type":"object", "required":["first", "second"],
            "patternProperties":{"^allowed_":{}}, "additionalProperties":false}),
        &json!({"allowed_name":"ok", "unexpected":"bad"}),
    )
    .unwrap();
    let missing = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "schema_required")
        .filter_map(|diagnostic| diagnostic.property.as_deref())
        .collect::<BTreeSet<_>>();
    assert_eq!(missing, BTreeSet::from(["first", "second"]));
    let additional = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "schema_additional_properties")
        .unwrap();
    assert_eq!(additional.property.as_deref(), Some("unexpected"));
    assert!(serde_json::to_value(additional)
        .unwrap()
        .get("property")
        .is_none());
}

#[test]
fn compiled_schema_retains_exact_transitive_sources_and_validates_without_io() {
    fn assert_shareable<T: Send + Sync>() {}
    assert_shareable::<MdbaseCompiledSchema>();
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("_types")).unwrap();
    let base = dir.path().join("_types/task.md");
    fs::write(&base, "placeholder").unwrap();
    let first = "properties:\n  id: {$ref: 'identifier.txt'}\n";
    let second = "type: string\nminLength: 3\n";
    fs::write(dir.path().join("record.yaml"), first).unwrap();
    fs::write(dir.path().join("identifier.txt"), second).unwrap();
    let reads = RefCell::new(Vec::new());
    let schema = compile_mdbase_schema_with_local_refs(
        &json!({"allOf": [{"$ref": "../record.yaml"}, {"$ref": "../record.yaml"}]}),
        &base,
        dir.path(),
        &|path| {
            reads.borrow_mut().push(path.to_path_buf());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        *reads.borrow(),
        [
            PathBuf::from("_types/task.md"),
            PathBuf::from("record.yaml"),
            PathBuf::from("identifier.txt")
        ]
    );
    assert_eq!(
        schema.dependencies(),
        &BTreeMap::from([
            (PathBuf::from("record.yaml"), first.as_bytes().to_vec()),
            (PathBuf::from("identifier.txt"), second.as_bytes().to_vec()),
        ])
    );
    // Once prepared, validation must not reopen either the base or a reference.
    dir.close().unwrap();
    for _ in 0..3 {
        assert!(schema.validate(&json!({"id": "valid"})).is_empty());
        assert!(schema
            .validate(&json!({"id": "x"}))
            .iter()
            .any(|diagnostic| diagnostic.code == "schema_min_length"));
    }
}

#[test]
fn reference_file_limit_counts_in_progress_transitive_dependencies() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("task.md");
    fs::write(&base, "placeholder").unwrap();
    let mut references = Vec::new();
    for index in 0..MDBASE_SCHEMA_MAX_FILES - 1 {
        let path = format!("a{index:02}.yaml");
        fs::write(dir.path().join(&path), "type: string").unwrap();
        references.push(json!({"$ref": path}));
    }
    fs::write(dir.path().join("z1.yaml"), "$ref: z2.yaml").unwrap();
    fs::write(dir.path().join("z2.yaml"), "type: string").unwrap();
    references.push(json!({"$ref": "z1.yaml"}));
    let error = compile_mdbase_schema_with_local_refs(
        &json!({"allOf": references}),
        &base,
        dir.path(),
        &|_| Ok(()),
    )
    .err()
    .unwrap();
    assert!(error
        .to_string()
        .contains("schema reference count exceeds 64"));
}

#[test]
fn denied_schema_paths_are_not_probed_for_existence_or_parsed() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("task.md");
    let schema = json!({"$ref": "hidden.yaml"});
    let denied = || MdbaseSchemaCompileError("permission_denied".into());
    // Base authority is checked before even checking that the base exists.
    assert_eq!(
        compile_mdbase_schema_with_local_refs(&schema, &base, dir.path(), &|_| Err(denied())).err(),
        Some(denied())
    );
    fs::write(&base, "placeholder").unwrap();
    for contents in [None, Some("malformed: ["), Some("type: object")] {
        if let Some(contents) = contents {
            fs::write(dir.path().join("hidden.yaml"), contents).unwrap();
        }
        assert_eq!(
            compile_mdbase_schema_with_local_refs(&schema, &base, dir.path(), &|path| {
                if path == Path::new("task.md") {
                    Ok(())
                } else {
                    Err(denied())
                }
            })
            .err(),
            Some(denied())
        );
    }
}

#[test]
fn compiled_schema_is_immutable_when_a_reference_changes() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("task.md");
    fs::write(&base, "placeholder").unwrap();
    fs::write(dir.path().join("schema.yaml"), "type: string").unwrap();
    let source = json!({"$ref": "schema.yaml"});
    let old =
        compile_mdbase_schema_with_local_refs(&source, &base, dir.path(), &|_| Ok(())).unwrap();
    fs::write(dir.path().join("schema.yaml"), "type: number").unwrap();
    let new =
        compile_mdbase_schema_with_local_refs(&source, &base, dir.path(), &|_| Ok(())).unwrap();
    assert_ne!(old.dependencies(), new.dependencies());
    assert!(old.validate(&json!("text")).is_empty());
    assert!(!new.validate(&json!("text")).is_empty());
}

#[cfg(unix)]
#[test]
fn absolute_in_collection_references_keep_relative_dependency_keys() {
    let dir = tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let base = root.join("task.md");
    fs::write(&base, "placeholder").unwrap();
    let reference = root.join("schema.yaml");
    fs::write(&reference, "type: string").unwrap();
    let compiled =
        compile_mdbase_schema_with_local_refs(&json!({"$ref": reference}), &base, &root, &|_| {
            Ok(())
        })
        .unwrap();
    assert!(compiled.validate(&json!("text")).is_empty());
    assert!(compiled
        .dependencies()
        .contains_key(Path::new("schema.yaml")));
}

#[cfg(unix)]
#[test]
fn schema_snapshot_rejects_symlinked_files_and_parent_directories() {
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let base = dir.path().join("task.md");
    fs::write(&base, "placeholder").unwrap();
    fs::create_dir(dir.path().join("schemas")).unwrap();
    fs::write(dir.path().join("schemas/id.yaml"), "type: string").unwrap();
    symlink("schemas/id.yaml", dir.path().join("alias.yaml")).unwrap();
    symlink("schemas", dir.path().join("alias")).unwrap();
    symlink("task.md", dir.path().join("alias.md")).unwrap();
    for reference in ["alias.yaml", "alias/id.yaml"] {
        assert!(compile_mdbase_schema_with_local_refs(
            &json!({"$ref": reference}),
            &base,
            dir.path(),
            &|_| Ok(())
        )
        .is_err());
        assert!(
            contracts::resolve_schema_wrapper(&json!({"ref": reference}), &base, dir.path(),)
                .is_err()
        );
    }
    assert!(compile_mdbase_schema_with_local_refs(
        &json!({}),
        &dir.path().join("alias.md"),
        dir.path(),
        &|_| Ok(())
    )
    .is_err());
}
