use super::*;
use serde_json::json;
use std::cell::RefCell;
use tempfile::tempdir;

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
