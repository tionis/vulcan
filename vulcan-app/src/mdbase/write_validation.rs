use super::{AppError, LoadedCollection, MdbaseManagedWriteMode, MdbaseWritePreview};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use vulcan_core::mdbase::{
    analyze_mdbase_record_set_sources, analyze_mdbase_record_set_with_cached,
    analyze_mdbase_record_source_with_clock, is_mdbase_record_path, mdbase_content_revision,
    MdbaseCelClock, MdbaseRecordDiagnostic,
};
use vulcan_core::paths::secure_read_to_string;

/// Load semantic controls between the initial and scoped revision snapshots.
/// Reusing registries loaded before the first snapshot could validate old rules
/// while binding the plan to newer control bytes.
pub(super) fn reload_controls(loaded: &mut LoadedCollection) -> Result<(), AppError> {
    if !super::allowed(loaded.control_filter.as_ref(), "mdbase.yaml") {
        return Err(super::control_permission_denied());
    }
    let collection = vulcan_core::mdbase::load_mdbase_collection(&loaded.collection.root)
        .map_err(AppError::operation)?
        .ok_or_else(|| {
            AppError::operation_with_code(
                "stale_state",
                "mdbase collection disappeared while planning",
            )
        })?;
    let (types, contracts) =
        super::load_control_registries(&collection, loaded.control_filter.as_ref())?;
    loaded.collection = collection;
    loaded.types = types;
    loaded.contracts = contracts;
    Ok(())
}

/// Derive authority from actual old/proposed sources, never caller type hints.
pub(super) fn affected_membership(
    loaded: &LoadedCollection,
    preview: &MdbaseWritePreview,
    clock: &MdbaseCelClock,
) -> Result<Vec<String>, AppError> {
    let mut names = BTreeSet::new();
    for change in &preview.changes {
        if !is_mdbase_record_path(&loaded.collection, &change.path).map_err(AppError::operation)? {
            continue;
        }
        for source in change.before.iter().chain(change.after.iter()) {
            let analysis = analyze_mdbase_record_source_with_clock(
                &loaded.collection,
                &loaded.types,
                &change.path,
                source,
                clock,
            );
            for name in analysis.types {
                if let Some(definition) = loaded.types.get(&name) {
                    names.insert(definition.name.clone());
                }
            }
        }
    }
    Ok(names.into_iter().collect())
}

pub(super) fn check_snapshot_stability(
    initial: &MdbaseWritePreview,
    scoped: &MdbaseWritePreview,
) -> Result<(), AppError> {
    if initial.changes.len() != scoped.changes.len()
        || initial
            .changes
            .iter()
            .zip(&scoped.changes)
            .any(|(left, right)| {
                left.path != right.path
                    || left.before != right.before
                    || left.before_revision != right.before_revision
                    || left.if_revision != right.if_revision
            })
    {
        return Err(AppError::operation_with_code(
            "concurrent_modification",
            "mdbase affected sources changed while planning",
        ));
    }
    if initial.control_revisions != scoped.control_revisions {
        return Err(AppError::operation_with_code(
            "stale_state",
            "mdbase controls changed while planning",
        ));
    }
    Ok(())
}

pub(super) fn validate_final_state(
    loaded: &LoadedCollection,
    preview: &MdbaseWritePreview,
    clock: &MdbaseCelClock,
    mode: MdbaseManagedWriteMode,
) -> Result<Vec<MdbaseRecordDiagnostic>, AppError> {
    let diagnostics = final_diagnostics(loaded, preview, clock)?;
    if mode == MdbaseManagedWriteMode::Validated {
        let blocking = blocking_diagnostics(loaded, preview, clock, &diagnostics)?;
        if !blocking.is_empty() {
            return Err(AppError::operation_with_code("validation_failed", format!(
                "mdbase validation rejected the managed note write: {}; use explicit raw repair only when preserving invalid source is intentional",
                super::validation_error_summary(&blocking)
            )));
        }
    }
    Ok(diagnostics)
}

/// Records already invalid before a write must not block unrelated writes.
/// Errors block when they sit on a changed record or did not exist in the
/// pre-write snapshot (for example a link broken by deleting its target).
fn blocking_diagnostics(
    loaded: &LoadedCollection,
    preview: &MdbaseWritePreview,
    clock: &MdbaseCelClock,
    diagnostics: &[MdbaseRecordDiagnostic],
) -> Result<Vec<MdbaseRecordDiagnostic>, AppError> {
    let existing: BTreeSet<_> = analyze_scope(loaded, preview, clock, Scope::Before)?
        .records
        .into_iter()
        .flat_map(|record| record.diagnostics)
        .map(|diagnostic| diagnostic_key(&diagnostic))
        .collect();
    let changed: BTreeSet<&str> = preview
        .changes
        .iter()
        .map(|change| change.path.as_str())
        .collect();
    Ok(diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.severity == vulcan_core::mdbase::MdbaseRecordDiagnosticSeverity::Error
                && (changed.contains(diagnostic.path.as_str())
                    || !existing.contains(&diagnostic_key(diagnostic)))
        })
        .cloned()
        .collect())
}

fn diagnostic_key(diagnostic: &MdbaseRecordDiagnostic) -> (String, String, String, String) {
    (
        diagnostic.path.clone(),
        diagnostic.code.clone(),
        diagnostic.field.clone(),
        diagnostic.message.clone(),
    )
}

/// Assemble the complete authorized validation scope captured by the preview.
/// Each existing source must match its accepted revision. Overlays are applied
/// together, so no validator observes an intermediate batch state.
pub(super) fn final_diagnostics(
    loaded: &LoadedCollection,
    preview: &MdbaseWritePreview,
    clock: &MdbaseCelClock,
) -> Result<Vec<MdbaseRecordDiagnostic>, AppError> {
    Ok(analyze_scope(loaded, preview, clock, Scope::After)?
        .records
        .into_iter()
        .flat_map(|record| record.diagnostics)
        .collect())
}

/// Which state of the accepted validation scope to analyze.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    /// Every accepted record as it was when the preview was captured.
    Before,
    /// Every accepted record with the proposed changes applied.
    After,
    /// Only the changed records' proposed sources.
    ChangedAfter,
}

/// Analyze `scope`. Unchanged records come from cached local derivations
/// when they cover every one at its accepted revision (the revision the
/// preview's snapshot proved) under the preview's controls; otherwise every
/// source is read again and must still match its accepted revision.
pub(super) fn analyze_scope(
    loaded: &LoadedCollection,
    preview: &MdbaseWritePreview,
    clock: &MdbaseCelClock,
    scope: Scope,
) -> Result<vulcan_core::mdbase::MdbaseRecordSet, AppError> {
    let mut changed = BTreeMap::new();
    for change in &preview.changes {
        if !is_mdbase_record_path(&loaded.collection, &change.path).map_err(AppError::operation)? {
            continue;
        }
        let source = if scope == Scope::Before {
            &change.before
        } else {
            &change.after
        };
        if let Some(source) = source {
            changed.insert(change.path.clone(), source.clone());
        }
    }
    if scope == Scope::ChangedAfter {
        return Ok(analyze_mdbase_record_set_sources(
            &loaded.collection,
            &loaded.types,
            &changed,
            clock,
        ));
    }
    if let Some(local) = loaded
        .cached_local
        .as_deref()
        .filter(|local| *local.controls() == preview.control_revisions)
    {
        // Unchanged accepted paths are records by construction: the preview
        // snapshot takes them from collection discovery.
        let changed_paths = preview
            .changes
            .iter()
            .map(|change| change.path.as_str())
            .collect::<BTreeSet<_>>();
        let cached = preview
            .accepted_revisions
            .iter()
            .filter(|(path, _)| !changed_paths.contains(path.as_str()))
            .map(|(path, revision)| (path.clone(), revision.clone()))
            .collect::<BTreeMap<_, _>>();
        if let Some(set) = analyze_mdbase_record_set_with_cached(
            &loaded.collection,
            &loaded.types,
            local,
            &changed,
            &cached,
            clock,
        ) {
            return Ok(set);
        }
    }
    let sources = if scope == Scope::Before {
        accepted_sources(loaded, preview)?
    } else {
        proposed_sources(loaded, preview)?
    };
    Ok(analyze_mdbase_record_set_sources(
        &loaded.collection,
        &loaded.types,
        &sources,
        clock,
    ))
}

/// Accepted record sources as captured, skipping any that no longer match:
/// such a record cannot have pre-existing diagnostics to excuse.
fn accepted_sources(
    loaded: &LoadedCollection,
    preview: &MdbaseWritePreview,
) -> Result<BTreeMap<String, String>, AppError> {
    let mut before = BTreeMap::new();
    for (path, revision) in &preview.accepted_revisions {
        if !is_mdbase_record_path(&loaded.collection, path).map_err(AppError::operation)? {
            continue;
        }
        let source = match preview.changes.iter().find(|change| change.path == *path) {
            Some(change) => change.before.clone(),
            None => secure_read_to_string(&loaded.collection.root, Path::new(path)).ok(),
        };
        if let Some(source) = source.filter(|source| mdbase_content_revision(source) == *revision) {
            before.insert(path.clone(), source);
        }
    }
    Ok(before)
}

pub(super) fn proposed_sources(
    loaded: &LoadedCollection,
    preview: &MdbaseWritePreview,
) -> Result<BTreeMap<String, String>, AppError> {
    let mut sources = BTreeMap::new();
    for (path, revision) in &preview.accepted_revisions {
        if !is_mdbase_record_path(&loaded.collection, path).map_err(AppError::operation)? {
            continue;
        }
        let source =
            if let Some(change) = preview.changes.iter().find(|change| change.path == *path) {
                change
                    .before
                    .clone()
                    .ok_or_else(|| snapshot_changed(path))?
            } else {
                secure_read_to_string(&loaded.collection.root, Path::new(path))
                    .map_err(|_| snapshot_changed(path))?
            };
        if mdbase_content_revision(&source) != *revision {
            return Err(snapshot_changed(path));
        }
        sources.insert(path.clone(), source);
    }
    for change in &preview.changes {
        if !is_mdbase_record_path(&loaded.collection, &change.path).map_err(AppError::operation)? {
            continue;
        }
        if let Some(source) = &change.after {
            sources.insert(change.path.clone(), source.clone());
        } else {
            sources.remove(&change.path);
        }
    }
    Ok(sources)
}

fn snapshot_changed(path: &str) -> AppError {
    AppError::operation_with_code(
        "stale_state",
        format!("mdbase validation dependency changed while planning: {path}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdbase::*;
    use chrono::{TimeZone, Utc};
    use std::fs;
    use vulcan_core::mdbase::list_mdbase_write_outbox;
    use vulcan_core::VaultPaths;

    fn fixture(rules: &str) -> (tempfile::TempDir, VaultPaths) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("mdbase.yaml"), "spec_version: '0.3.0'\n").unwrap();
        fs::create_dir(dir.path().join("_types")).unwrap();
        fs::write(dir.path().join("_types/task.md"), format!("---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  value: {{type: object, required: [id]}}\ncollection:\n{rules}---\n")).unwrap();
        let paths = VaultPaths::new(dir.path());
        (dir, paths)
    }

    fn source(id: &str) -> String {
        format!("---\ntype: task\nid: {id}\n---\nBody\n")
    }

    fn request(changes: &[(&str, Option<String>)]) -> MdbaseWritePlanRequest {
        MdbaseWritePlanRequest {
            caller_id: "test".to_string(),
            instance_id: "test".to_string(),
            operation: MdbaseWriteOperation::Batch,
            changes: changes
                .iter()
                .map(|(path, after)| MdbaseWriteChangeRequest {
                    path: (*path).to_string(),
                    after: after.clone(),
                    if_revision: None,
                })
                .collect(),
            matched_types: Vec::new(),
            generated_values: BTreeMap::new(),
            permission_profile: None,
            ttl_seconds: None,
        }
    }

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap()
    }

    #[test]
    fn cached_planning_equals_source_planning() {
        let (dir, paths) = fixture(
            "  unique: [{field: id, scope: collection}]\n  links:\n    parent: {target_type: task, validate_exists: true}\n",
        );
        for (name, id) in [("a.md", "one"), ("b.md", "two"), ("c.md", "two")] {
            fs::write(dir.path().join(name), source(id)).unwrap();
        }
        fs::write(
            dir.path().join("d.md"),
            "---\ntype: task\nid: four\nparent: '[[b]]'\n---\nBody\n",
        )
        .unwrap();
        fs::write(dir.path().join("e.md"), "---\ntitle: untyped\n---\n").unwrap();
        vulcan_core::initialize_vulcan_dir(&paths).unwrap();
        // Populate the record cache through the ordinary unrestricted service.
        build_mdbase_query_report(&paths, &serde_json::json!({}), None).unwrap();
        let cache = paths.cache_db();
        let hidden = cache.with_extension("hidden");
        let outcome = |plan: Result<MdbaseWritePlanReport, AppError>| match plan {
            Ok(plan) => Ok((
                plan.diagnostics,
                plan.preview.accepted_revisions,
                plan.preview.matched_types,
            )),
            Err(error) => Err((error.code().map(str::to_string), error.to_string())),
        };
        let scenarios = [
            vec![("a.md", Some(source("five")))],
            vec![("a.md", Some(source("two")))],
            vec![("b.md", None)],
            vec![("f.md", Some(source("six")))],
            vec![("e.md", Some(source("seven"))), ("c.md", None)],
        ];
        for changes in scenarios {
            let with = outcome(plan_mdbase_write(&paths, &request(&changes), now()));
            fs::rename(cache, &hidden).unwrap();
            let without = outcome(plan_mdbase_write(&paths, &request(&changes), now()));
            fs::rename(&hidden, cache).unwrap();
            assert_eq!(with, without, "{changes:?}");
        }
    }

    #[test]
    fn apply_rejects_transitive_text_schema_drift_before_changing_records() {
        let (dir, paths) = fixture("  unique: [{field: id, scope: type}]\n");
        let type_path = dir.path().join("_types/task.md");
        let definition = fs::read_to_string(&type_path).unwrap();
        fs::write(
            type_path,
            definition.replace(
                "value: {type: object, required: [id]}",
                "ref: ../schema.yaml",
            ),
        )
        .unwrap();
        fs::write(
            dir.path().join("schema.yaml"),
            "type: object\nproperties:\n  id: {$ref: id.txt}\n",
        )
        .unwrap();
        fs::write(dir.path().join("id.txt"), "type: string\n").unwrap();
        let plan =
            plan_mdbase_write(&paths, &request(&[("a.md", Some(source("one")))]), now()).unwrap();
        fs::write(dir.path().join("id.txt"), "type: string\nminLength: 2\n").unwrap();
        let error = apply_mdbase_write(
            &paths,
            &plan,
            &MdbaseWriteExecutionOptions {
                idempotency_key: "schema-drift".to_string(),
                no_commit: true,
                verbosity: Verbosity::Quiet,
            },
            now(),
        )
        .unwrap_err();
        assert_eq!(error.code(), Some("stale_state"));
        assert!(!dir.path().join("a.md").exists());
        assert!(list_mdbase_write_outbox(&paths).unwrap().is_empty());
    }

    #[test]
    fn control_reload_keeps_the_original_read_ceiling() {
        use vulcan_core::permissions::{PathPermission, PermissionFilter, ResourceSpecifier};
        let (dir, paths) = fixture("");
        let filter = PermissionFilter::new(PathPermission {
            allow: vec![ResourceSpecifier::All],
            deny: vec![ResourceSpecifier::Note("hidden.txt".into())],
        });
        let mut loaded = super::super::load_collection_authorized(&paths, Some(&filter)).unwrap();
        fs::write(dir.path().join("_types/task.md"), "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  ref: ../hidden.txt\n---\n").unwrap();
        for contents in [None, Some("invalid: [SECRET"), Some("type: object\n")] {
            if let Some(contents) = contents {
                fs::write(dir.path().join("hidden.txt"), contents).unwrap();
            }
            let error = reload_controls(&mut loaded).unwrap_err();
            assert_eq!(error.code(), Some("permission_denied"));
            assert_eq!(
                error.message(),
                "permission denied for required mdbase controls"
            );
        }
    }

    #[test]
    fn controls_are_reloaded_inside_the_revision_capture_window() {
        let (dir, paths) = fixture("  unique: [{field: id, scope: collection}]\n");
        let mut loaded = super::super::load_collection(&paths).unwrap();
        assert!(loaded.types.get("task").is_some());
        let type_path = dir.path().join("_types/task.md");
        let definition = fs::read_to_string(&type_path).unwrap();
        fs::write(type_path, definition.replace("name: task", "name: renamed")).unwrap();
        reload_controls(&mut loaded).unwrap();
        assert!(loaded.types.get("task").is_none());
        assert!(loaded.types.get("renamed").is_some());
        fs::remove_file(dir.path().join("mdbase.yaml")).unwrap();
        assert_eq!(
            reload_controls(&mut loaded).unwrap_err().code(),
            Some("stale_state")
        );
    }

    #[test]
    fn membership_and_final_validation_share_the_fixed_operation_clock() {
        let (dir, paths) = fixture("  unique: [{field: id, scope: type}]\n");
        let path = dir.path().join("_types/task.md");
        let definition = fs::read_to_string(&path).unwrap();
        fs::write(
            path,
            definition.replace(
                "schema:\n",
                "match:\n  expr: {$expr: \"today() == '2026-09-13'\"}\nschema:\n",
            ),
        )
        .unwrap();
        let draft = request(&[("a.md", Some("No persisted id\n".to_string()))]);
        let error = plan_mdbase_write(&paths, &draft, now()).unwrap_err();
        assert!(error.message().contains("schema_required"));
        let plan = plan_mdbase_write(&paths, &draft, now() + chrono::Duration::days(1)).unwrap();
        assert!(plan.preview.matched_types.is_empty());
        assert!(plan.diagnostics.is_empty());
    }

    #[test]
    fn bounded_scope_does_not_validate_unreadable_unrelated_records() {
        let (dir, paths) =
            fixture("  unique: [{field: id, scope: path_glob, path_glob: 'published/**'}]\n");
        fs::create_dir(dir.path().join("published")).unwrap();
        fs::write(dir.path().join("published/one.md"), source("one")).unwrap();
        fs::write(
            dir.path().join("hidden.md"),
            "---\ntype: task\n---\nInvalid hidden record\n",
        )
        .unwrap();
        fs::create_dir(dir.path().join(".vulcan")).unwrap();
        fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"folder:published/**\", \"folder:_types/**\", \"note:mdbase.yaml\", \"note:mdbase.lock.yaml\", \"folder:_contracts/**\"] }\nwrite = { allow = [\"folder:published/**\"] }\n").unwrap();
        let mut draft = request(&[("published/two.md", Some(source("two")))]);
        draft.permission_profile = Some("scoped".to_string());
        let plan = plan_mdbase_write(&paths, &draft, now()).unwrap();
        assert_eq!(
            plan.authorization.collection_record_namespaces,
            ["published/**"]
        );
        assert!(plan.diagnostics.is_empty());
        assert!(!plan.preview.accepted_revisions.contains_key("hidden.md"));
    }

    #[test]
    fn preexisting_invalid_records_do_not_block_unrelated_valid_writes() {
        let (dir, paths) = fixture("  unique: [{field: id, scope: collection}]\n");
        fs::write(
            dir.path().join("legacy.md"),
            "---\ntype: task\n---\nAlready invalid\n",
        )
        .unwrap();
        plan_mdbase_write(&paths, &request(&[("new.md", Some(source("new")))]), now()).unwrap();
        let error = plan_mdbase_write(
            &paths,
            &request(&[("bad.md", Some("---\ntype: task\n---\n".to_string()))]),
            now(),
        )
        .unwrap_err();
        assert_eq!(error.code(), Some("validation_failed"));
    }

    #[test]
    fn direct_planner_rejects_intra_batch_duplicates_and_invalid_schemas_without_state() {
        let (dir, paths) = fixture("  unique: [{field: id, scope: collection}]\n");
        let error = plan_mdbase_write(
            &paths,
            &request(&[
                ("a.md", Some(source("same"))),
                ("b.md", Some(source("same"))),
            ]),
            now(),
        )
        .unwrap_err();
        assert_eq!(error.code(), Some("validation_failed"));
        assert!(error.message().contains("duplicate_value"));
        let error = plan_mdbase_write(
            &paths,
            &request(&[("a.md", Some("---\ntype: task\n---\n".to_string()))]),
            now(),
        )
        .unwrap_err();
        assert!(error.message().contains("schema_required"));
        assert!(!dir.path().join(".vulcan").exists());
        assert!(!dir.path().join("a.md").exists());
        assert!(!dir.path().join("b.md").exists());
    }

    #[test]
    fn final_set_accepts_unique_swaps_and_commits_one_transaction() {
        let (dir, paths) = fixture("  unique: [{field: id, scope: collection}]\n");
        fs::write(dir.path().join("a.md"), source("one")).unwrap();
        fs::write(dir.path().join("b.md"), source("two")).unwrap();
        let plan = plan_mdbase_write(
            &paths,
            &request(&[("a.md", Some(source("two"))), ("b.md", Some(source("one")))]),
            now(),
        )
        .unwrap();
        assert_eq!(plan.preview.matched_types, ["task"]);
        assert!(plan.diagnostics.is_empty());
        apply_mdbase_write(
            &paths,
            &plan,
            &MdbaseWriteExecutionOptions {
                idempotency_key: "swap".to_string(),
                no_commit: true,
                verbosity: Verbosity::Quiet,
            },
            now(),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.md")).unwrap(),
            source("two")
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("b.md")).unwrap(),
            source("one")
        );
        assert_eq!(list_mdbase_write_outbox(&paths).unwrap().len(), 1);
    }

    #[test]
    fn incoming_links_validate_created_and_deleted_targets_in_the_final_set() {
        let (dir, paths) =
            fixture("  links:\n    related: {target_type: any, validate_exists: true}\n");
        fs::write(dir.path().join("target.md"), "Untyped target\n").unwrap();
        let owner = "---\ntype: task\nid: owner\nrelated: '[[target]]'\n---\n".to_string();
        fs::write(dir.path().join("owner.md"), &owner).unwrap();
        let error = plan_mdbase_write(&paths, &request(&[("target.md", None)]), now()).unwrap_err();
        assert_eq!(error.code(), Some("validation_failed"));
        assert!(error.message().contains("link"));
        plan_mdbase_write(
            &paths,
            &request(&[("target.md", None), ("owner.md", None)]),
            now(),
        )
        .unwrap();
        fs::remove_file(dir.path().join("target.md")).unwrap();
        fs::remove_file(dir.path().join("owner.md")).unwrap();
        plan_mdbase_write(
            &paths,
            &request(&[
                ("owner.md", Some(owner)),
                ("target.md", Some("new target".to_string())),
            ]),
            now(),
        )
        .unwrap();
        assert!(!dir.path().join(".vulcan").exists());
    }

    #[test]
    fn missing_caller_type_hints_cannot_hide_old_or_proposed_membership() {
        let (dir, paths) = fixture("  unique: [{field: id, scope: type}]\n");
        fs::create_dir(dir.path().join(".vulcan")).unwrap();
        fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"note:a.md\", \"folder:_types/**\", \"note:mdbase.yaml\", \"note:mdbase.lock.yaml\", \"folder:_contracts/**\"] }\nwrite = { allow = [\"note:a.md\"] }\n").unwrap();
        let mut proposed = request(&[("a.md", Some(source("one")))]);
        proposed.permission_profile = Some("scoped".to_string());
        assert_eq!(
            plan_mdbase_write(&paths, &proposed, now())
                .unwrap_err()
                .code(),
            Some("permission_denied")
        );
        fs::write(dir.path().join("a.md"), source("one")).unwrap();
        proposed.changes[0].after = Some("Untyped replacement\n".to_string());
        assert_eq!(
            plan_mdbase_write(&paths, &proposed, now())
                .unwrap_err()
                .code(),
            Some("permission_denied")
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("a.md")).unwrap(),
            source("one")
        );
        assert!(list_mdbase_write_outbox(&paths).unwrap().is_empty());
    }

    #[test]
    fn validation_never_uses_a_dependency_different_from_the_preview_revision() {
        let (dir, paths) = fixture("  unique: [{field: id, scope: collection}]\n");
        fs::write(dir.path().join("other.md"), source("other")).unwrap();
        let plan =
            plan_mdbase_write(&paths, &request(&[("a.md", Some(source("one")))]), now()).unwrap();
        let loaded = super::super::load_collection(&paths).unwrap();
        let clock = MdbaseCelClock::new(now(), "UTC").unwrap();
        fs::write(dir.path().join("other.md"), source("one")).unwrap();
        assert_eq!(
            final_diagnostics(&loaded, &plan.preview, &clock)
                .unwrap_err()
                .code(),
            Some("stale_state")
        );
        let mut changed = plan.preview.clone();
        changed.changes[0].before = Some("concurrent creation".to_string());
        assert_eq!(
            check_snapshot_stability(&plan.preview, &changed)
                .unwrap_err()
                .code(),
            Some("concurrent_modification")
        );
        changed = plan.preview.clone();
        changed.control_revisions.combined = "changed".to_string();
        assert_eq!(
            check_snapshot_stability(&plan.preview, &changed)
                .unwrap_err()
                .code(),
            Some("stale_state")
        );
    }

    #[test]
    fn explicit_raw_repair_reports_collection_errors_without_silent_fallback() {
        let (dir, paths) = fixture("  unique: [{field: id, scope: collection}]\n");
        fs::write(dir.path().join("other.md"), source("same")).unwrap();
        let after = source("same");
        let mut request = MdbaseManagedNoteWriteRequest {
            path: "a.md",
            before: None,
            after: Some(&after),
            operation: MdbaseWriteOperation::Create,
            mode: MdbaseManagedWriteMode::Validated,
            dry_run: false,
            permission_profile: None,
            verbosity: Verbosity::Quiet,
        };
        assert!(apply_managed_mdbase_note_write(&paths, &request).is_err());
        assert!(!dir.path().join("a.md").exists());
        request.mode = MdbaseManagedWriteMode::RawRepair;
        let report = apply_managed_mdbase_note_write(&paths, &request)
            .unwrap()
            .unwrap();
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "duplicate_value"));
        assert_eq!(fs::read_to_string(dir.path().join("a.md")).unwrap(), after);
    }
}
