use crate::AppError;
use vulcan_core::{scan_vault_with_progress, ScanMode, ScanProgress, ScanSummary, VaultPaths};

pub fn refresh_cache_incrementally(paths: &VaultPaths) -> Result<ScanSummary, AppError> {
    refresh_cache_incrementally_with_progress(paths, |_| {})
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
    use super::{refresh_cache_incrementally, refresh_cache_incrementally_with_progress};
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::properties::load_note_index;
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
}
