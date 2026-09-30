use super::*;
use crate::mdbase::{load_mdbase_collection, load_mdbase_type_registry};

fn fixture() -> (tempfile::TempDir, MdbaseCollection, MdbaseTypeRegistry) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("mdbase.yaml"),
        "spec_version: 0.3.0\n",
    )
    .unwrap();
    fs::create_dir(directory.path().join("_types")).unwrap();
    fs::write(directory.path().join("_types/task.md"), "---\nkind: mdbase.type\nname: task\nversion: 1\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [id]\ncollection:\n  read_defaults: {status: open}\n  unique:\n    - {field: id, scope: collection}\n  links:\n    parent: {target_type: task, validate_exists: true}\n---\n").unwrap();
    let collection = load_mdbase_collection(directory.path()).unwrap().unwrap();
    let types = load_mdbase_type_registry(&collection).unwrap();
    assert!(types.diagnostics.is_empty());
    (directory, collection, types)
}

fn analyze(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    sources: &BTreeMap<String, String>,
) -> MdbaseRecordSet {
    let clock = MdbaseCelClock::new("2026-09-08T12:00:00Z".parse().unwrap(), "UTC").unwrap();
    analyze_mdbase_record_set_sources(collection, types, sources, &clock)
}

fn source(id: &str) -> String {
    format!("---\ntype: task\nid: {id}\n---\nBody\n")
}

#[test]
fn single_source_analysis_uses_the_supplied_clock_for_inferred_membership() {
    let (directory, collection, _) = fixture();
    fs::write(directory.path().join("_types/task.md"), "---\nkind: mdbase.type\nname: task\nmatch:\n  expr: {$expr: \"today() == '2026-09-08'\"}\nschema:\n  dialect: json-schema-2020-12\n  value: {type: object, required: [id]}\n---\n").unwrap();
    let types = load_mdbase_type_registry(&collection).unwrap();
    assert!(types.diagnostics.is_empty());
    let clock = MdbaseCelClock::new("2026-09-08T12:00:00Z".parse().unwrap(), "UTC").unwrap();
    let matched =
        analyze_mdbase_record_source_with_clock(&collection, &types, "new.md", "Body\n", &clock);
    assert_eq!(matched.types, ["task"]);
    assert!(matched
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "schema_required"));
    let later = MdbaseCelClock::new("2026-09-09T12:00:00Z".parse().unwrap(), "UTC").unwrap();
    let unmatched =
        analyze_mdbase_record_source_with_clock(&collection, &types, "new.md", "Body\n", &later);
    assert!(unmatched.types.is_empty());
    assert!(unmatched.diagnostics.is_empty());
    assert!(!directory.path().join("new.md").exists());
}

#[test]
fn intra_batch_uniqueness_uses_only_the_supplied_snapshot() {
    let (directory, collection, types) = fixture();
    fs::write(directory.path().join("hidden.md"), source("same")).unwrap();
    let mut sources = BTreeMap::from([("a.md".to_string(), source("same"))]);
    assert!(analyze(&collection, &types, &sources).records[0].is_valid());
    sources.insert("b.md".to_string(), source("same"));
    let result = analyze(&collection, &types, &sources);
    assert_eq!(result.records.len(), 2);
    for record in &result.records {
        let diagnostic = record
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "duplicate_value")
            .unwrap();
        assert_eq!(diagnostic.related_paths.len(), 1);
        assert!(!diagnostic
            .related_paths
            .iter()
            .any(|path| path == "hidden.md"));
    }
    assert!(!directory.path().join("a.md").exists());
    assert!(!directory.path().join("b.md").exists());
}

#[test]
fn unique_value_swaps_and_deletions_do_not_validate_intermediate_states() {
    let (directory, collection, types) = fixture();
    fs::write(directory.path().join("a.md"), source("first")).unwrap();
    fs::write(directory.path().join("b.md"), source("second")).unwrap();
    let sources = BTreeMap::from([
        ("a.md".to_string(), source("second")),
        ("b.md".to_string(), source("first")),
    ]);
    assert!(analyze(&collection, &types, &sources)
        .records
        .iter()
        .all(MdbaseRecordDocument::is_valid));
    let remaining = BTreeMap::from([("a.md".to_string(), source("second"))]);
    assert!(analyze(&collection, &types, &remaining).records[0].is_valid());
    assert_eq!(
        fs::read_to_string(directory.path().join("a.md")).unwrap(),
        source("first")
    );
}

#[test]
fn proposed_creations_resolve_links_and_deleted_targets_break_them() {
    let (directory, collection, types) = fixture();
    let child = "---\ntype: task\nid: child\nparent: '[[parent]]'\n---\n[[parent|Parent]]\n";
    let mut sources = BTreeMap::from([
        ("child.md".to_string(), child.to_string()),
        ("parent.md".to_string(), source("parent")),
    ]);
    let created = analyze(&collection, &types, &sources);
    assert!(created.records.iter().all(MdbaseRecordDocument::is_valid));
    let child_record = created.get("child.md").unwrap();
    assert!(child_record
        .links
        .iter()
        .all(|link| link.resolved_path.as_deref() == Some("parent.md")));
    // A file remaining on disk must not revive a target deleted by the batch.
    fs::write(directory.path().join("parent.md"), source("parent")).unwrap();
    sources.remove("parent.md");
    let deleted = analyze(&collection, &types, &sources);
    assert!(deleted.records[0]
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "link_not_found" && diagnostic.field == "parent"));
    sources.insert(
        "parent.md".to_string(),
        "---\nid: parent\n---\nUntyped\n".to_string(),
    );
    assert!(analyze(&collection, &types, &sources)
        .get("child.md")
        .unwrap()
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "link_target_type_mismatch"));
}

#[test]
fn proposed_snapshot_matches_disk_semantics_except_unavailable_file_times() {
    let (directory, collection, types) = fixture();
    let sources = BTreeMap::from([
        (
            "a.md".to_string(),
            "\u{feff}---\r\ntype: task\r\nid: a\r\n---\r\nÉté [[b]] #tag\r\n".to_string(),
        ),
        ("b.md".to_string(), source("b")),
    ]);
    for (path, source) in &sources {
        fs::write(directory.path().join(path), source).unwrap();
    }
    let mut disk = load_mdbase_records(&collection, &types, true).unwrap();
    for record in &mut disk.records {
        record.file.mtime = None;
        record.file.ctime = None;
    }
    let snapshot = analyze(&collection, &types, &sources);
    assert_eq!(snapshot, disk);
    assert_eq!(snapshot.records[0].file.size, sources["a.md"].len() as u64);
    assert_eq!(snapshot.records[0].effective_frontmatter["status"], "open");
    assert!(snapshot.records[0].frontmatter.get("status").is_none());
}
