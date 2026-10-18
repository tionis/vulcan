use crate::commit::AutoCommitPolicy;
use crate::AppError;
use serde_json::json;
use vulcan_core::Verbosity;
use vulcan_core::{
    scan_vault_with_progress, PluginEvent, ScanMode, ScanProgress, ScanSummary, VaultPaths,
};

/// Run a user-requested scan and its configured post-scan automation.
pub fn scan_vault_with_automation<F>(
    paths: &VaultPaths,
    mode: ScanMode,
    auto_commit: &AutoCommitPolicy,
    active_permission_profile: Option<&str>,
    verbosity: Verbosity,
    on_progress: F,
) -> Result<ScanSummary, AppError>
where
    F: FnMut(ScanProgress),
{
    let summary =
        scan_vault_with_progress(paths, mode, on_progress).map_err(AppError::operation)?;
    if summary.added + summary.updated + summary.deleted > 0 {
        auto_commit
            .commit(paths, "scan", &[], active_permission_profile, verbosity)
            .map_err(AppError::operation)?;
    }
    let _ = crate::plugins::dispatch_plugin_event(
        paths,
        active_permission_profile,
        PluginEvent::OnScanComplete,
        &json!({
            "kind": PluginEvent::OnScanComplete,
            "mode": if mode == ScanMode::Full { "full" } else { "incremental" },
            "summary": &summary,
        }),
        verbosity,
    );
    Ok(summary)
}

pub fn refresh_cache_incrementally(paths: &VaultPaths) -> Result<ScanSummary, AppError> {
    refresh_cache_incrementally_with_progress(paths, |_| {})
}

/// Refreshes the cache after a write that changed `changed` (vault-relative
/// paths). Updates of already indexed notes are rescanned directly, without
/// discovering every file in the vault; creates, deletes, renames, and
/// uncertain paths fall back to ordinary discovery inside the same scan, and
/// an empty list refreshes as [`refresh_cache_incrementally`] does. Takes the
/// vault write lock.
pub fn refresh_cache_for_paths(
    paths: &VaultPaths,
    changed: &[String],
) -> Result<ScanSummary, AppError> {
    if changed.is_empty() {
        return refresh_cache_incrementally(paths);
    }
    let _lock = vulcan_core::write_lock::acquire_write_lock(paths).map_err(AppError::operation)?;
    let changed = changed
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    vulcan_core::scan::scan_vault_paths_unlocked(paths, &changed).map_err(AppError::operation)
}

/// Refreshes while the caller retains the vault write lock across a larger
/// mutation transaction.
pub(crate) fn refresh_cache_incrementally_unlocked(
    paths: &VaultPaths,
) -> Result<ScanSummary, AppError> {
    vulcan_core::scan::scan_vault_unlocked(paths, ScanMode::Incremental)
        .map_err(AppError::operation)
}

pub fn refresh_cache_incrementally_with_progress<F>(
    paths: &VaultPaths,
    on_progress: F,
) -> Result<ScanSummary, AppError>
where
    F: FnMut(ScanProgress),
{
    scan_vault_with_progress(paths, ScanMode::Incremental, on_progress).map_err(AppError::operation)
}

#[cfg(test)]
mod tests {
    use super::{
        refresh_cache_incrementally, refresh_cache_incrementally_with_progress,
        scan_vault_with_automation,
    };
    use crate::commit::AutoCommitPolicy;
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::properties::load_note_index;
    use vulcan_core::Verbosity;
    use vulcan_core::{
        initialize_vulcan_dir, scan_vault_with_progress, ScanMode, ScanPhase, VaultPaths,
    };

    /// The cache as a full rescan would leave it: documents with their
    /// content, every link with how it resolved, and full-text rows.
    fn cache_state(paths: &VaultPaths) -> Vec<String> {
        let connection = rusqlite::Connection::open(paths.cache_db()).expect("cache");
        let mut rows = Vec::new();
        for (label, sql) in [
            ("doc", "SELECT path || ' ' || hex(content_hash) FROM documents ORDER BY path"),
            (
                "link",
                "SELECT source.path || ' ' || links.raw_text || ' -> ' || COALESCE(target.path, '?')
                 FROM links
                 JOIN documents source ON source.id = links.source_document_id
                 LEFT JOIN documents target ON target.id = links.resolved_target_id
                 ORDER BY 1",
            ),
            (
                "fts",
                "SELECT documents.path || ' ' || search_chunk_content.content
                 FROM search_chunk_content
                 JOIN documents ON documents.id = search_chunk_content.document_id
                 ORDER BY 1",
            ),
        ] {
            let mut statement = connection.prepare(sql).expect(label);
            let values = statement
                .query_map([], |row| row.get::<_, String>(0))
                .expect(label)
                .collect::<Result<Vec<_>, _>>()
                .expect(label);
            rows.extend(values.into_iter().map(|value| format!("{label}: {value}")));
        }
        rows
    }

    /// Refreshing only the written paths leaves the cache exactly as a full
    /// rescan would, for every kind of note write: an update (and a link it
    /// adds or breaks), a create that resolves a dangling link, and a delete
    /// that leaves links dangling.
    #[test]
    fn refreshing_written_paths_matches_a_full_rescan() {
        use crate::notes::{
            apply_note_append, apply_note_create, apply_note_delete, apply_note_set,
            NoteAppendMode, NoteAppendRequest, NoteCreateRequest, NoteDeleteRequest,
            NoteSetRequest,
        };
        use std::collections::HashMap;
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path().join("vault");
        crate::routed::test_support::copy_fixture("basic", &root);
        let paths = VaultPaths::new(&root);
        initialize_vulcan_dir(&paths).expect("init");
        scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("initial scan");
        let verify = |label: &str, changed: &[String]| {
            super::refresh_cache_for_paths(&paths, changed).expect("targeted refresh");
            let targeted = cache_state(&paths);
            scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("full rescan");
            assert_eq!(targeted, cache_state(&paths), "{label}");
        };

        let appended = apply_note_append(
            &paths,
            &NoteAppendRequest {
                note: Some("Home".to_string()),
                text: "See [[Projects/Beta]] and searchable-zebra.".to_string(),
                mode: NoteAppendMode::Append,
                heading: None,
                periodic: None,
                date: None,
                vars: HashMap::new(),
            },
            None,
            Verbosity::Quiet,
        )
        .expect("append");
        verify("append adding a dangling link", &appended.changed_paths);

        let created = apply_note_create(
            &paths,
            &NoteCreateRequest {
                path: "Projects/Beta.md".to_string(),
                template: None,
                frontmatter: None,
                body: "Beta links back to [[Home]].\n".to_string(),
            },
            None,
            Verbosity::Quiet,
        )
        .expect("create");
        verify("create resolving it", &created.changed_paths);

        let set = apply_note_set(
            &paths,
            &NoteSetRequest {
                note: "People/Bob".to_string(),
                replacement: "Bob now links [[Projects/Beta]].\n".to_string(),
                preserve_frontmatter: true,
            },
            None,
            Verbosity::Quiet,
        )
        .expect("set");
        verify("set replacing links", &set.changed_paths);

        let deleted = apply_note_delete(
            &paths,
            &NoteDeleteRequest {
                note: "Projects/Beta".to_string(),
                dry_run: false,
            },
            None,
            Verbosity::Quiet,
        )
        .expect("delete");
        verify("delete leaving links dangling", &deleted.changed_paths);
        assert!(!cache_state(&paths)
            .iter()
            .any(|row| row.starts_with("doc: Projects/Beta.md")));
    }

    #[test]
    fn refresh_cache_incrementally_updates_cache_and_emits_progress() {
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path();
        let paths = VaultPaths::new(root);
        initialize_vulcan_dir(&paths).expect("init should succeed");
        scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("initial scan");
        fs::write(root.join("Inbox.md"), "# Inbox\n").expect("seed note");

        let mut events = Vec::new();
        let summary = refresh_cache_incrementally_with_progress(&paths, |event| events.push(event))
            .expect("incremental refresh");

        assert_eq!(summary.mode, ScanMode::Incremental);
        assert_eq!(summary.added, 1);
        assert_eq!(summary.updated, 0);
        assert_eq!(summary.deleted, 0);
        assert!(!events.is_empty());
        assert_eq!(
            events.last().map(|event| event.phase),
            Some(ScanPhase::Completed)
        );
        assert!(events
            .iter()
            .all(|event| event.mode == ScanMode::Incremental));

        let index = load_note_index(&paths).expect("note index");
        assert!(index
            .values()
            .any(|record| record.document_path == "Inbox.md"));
    }

    #[test]
    fn refresh_cache_incrementally_supports_non_reporting_callers() {
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path();
        let paths = VaultPaths::new(root);
        initialize_vulcan_dir(&paths).expect("init should succeed");
        fs::write(root.join("Inbox.md"), "# Inbox\n").expect("seed note");
        scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("initial scan");

        let summary = refresh_cache_incrementally(&paths).expect("incremental refresh");

        assert_eq!(summary.mode, ScanMode::Incremental);
        assert_eq!(summary.added, 0);
        assert_eq!(summary.updated, 0);
        assert_eq!(summary.deleted, 0);
        assert_eq!(summary.unchanged, 1);
    }

    #[test]
    fn requested_scan_reports_changes_and_progress_without_auto_commit() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Inbox.md"), "# Inbox\n").expect("seed note");
        let mut events = Vec::new();
        let summary = scan_vault_with_automation(
            &paths,
            ScanMode::Full,
            &AutoCommitPolicy::for_scan(&paths, false),
            None,
            Verbosity::Quiet,
            |event| events.push(event),
        )
        .expect("requested scan");

        assert_eq!(summary.added, 1);
        assert_eq!(summary.mode, ScanMode::Full);
        assert_eq!(
            events.last().map(|event| event.phase),
            Some(ScanPhase::Completed)
        );
        let second = scan_vault_with_automation(
            &paths,
            ScanMode::Incremental,
            &AutoCommitPolicy::for_scan(&paths, false),
            None,
            Verbosity::Quiet,
            |_| {},
        )
        .expect("unchanged scan");
        assert_eq!(second.unchanged, 1);
    }
}
