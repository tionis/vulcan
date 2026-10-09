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
