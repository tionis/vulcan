use crate::scan::refresh_cache_incrementally;
use crate::AppError;
use std::collections::HashMap;
use vulcan_core::{
    doctor_vault as core_doctor_vault, evaluate_base_file as core_evaluate_base_file,
    evaluate_dataview_js_query as core_evaluate_dataview_js_query,
    evaluate_dql as core_evaluate_dql,
    evaluate_note_inline_expressions as core_evaluate_note_inline_expressions,
    git_log as core_git_log, is_git_repo as core_is_git_repo,
    list_daily_note_events as core_list_daily_note_events,
    list_kanban_boards as core_list_kanban_boards,
    list_note_identities as core_list_note_identities,
    list_tagged_note_identities as core_list_tagged_note_identities, list_tags as core_list_tags,
    load_dataview_blocks as core_load_dataview_blocks, load_kanban_board as core_load_kanban_board,
    load_vault_config, move_note as core_move_note, query_backlinks as core_query_backlinks,
    query_links as core_query_links, query_notes as core_query_notes,
    search_vault as core_search_vault, AutoScanMode, BacklinksReport, BasesEvalReport,
    DailyNoteEvents, DataviewBlockRecord, DataviewJsResult, DoctorReport, DqlEvalError,
    DqlQueryResult, EvaluatedInlineExpression, GitLogEntry, KanbanBoardRecord, KanbanBoardSummary,
    MoveSummary, NamedCount, NoteIdentity, NoteQuery, NoteRecord, NotesReport, OutgoingLinksReport,
    PeriodicConfig, ScanSummary, SearchQuery, SearchReport, VaultPaths,
};

pub fn prepare_browse_refresh(
    paths: &VaultPaths,
    refresh_mode: AutoScanMode,
) -> Result<Option<ScanSummary>, AppError> {
    if !paths.cache_db().exists() {
        return refresh_cache_incrementally(paths).map(Some);
    }

    match refresh_mode {
        AutoScanMode::Off | AutoScanMode::Background => Ok(None),
        AutoScanMode::Blocking => refresh_cache_incrementally(paths).map(Some),
    }
}

pub fn refresh_browse_cache(paths: &VaultPaths) -> Result<ScanSummary, AppError> {
    refresh_cache_incrementally(paths)
}

pub fn list_note_identities(paths: &VaultPaths) -> Result<Vec<NoteIdentity>, AppError> {
    core_list_note_identities(paths).map_err(AppError::operation)
}

pub fn search_vault(paths: &VaultPaths, query: &SearchQuery) -> Result<SearchReport, AppError> {
    core_search_vault(paths, query).map_err(AppError::operation)
}

pub fn query_notes(paths: &VaultPaths, query: &NoteQuery) -> Result<NotesReport, AppError> {
    core_query_notes(paths, query).map_err(AppError::operation)
}

pub fn list_tags(paths: &VaultPaths) -> Result<Vec<NamedCount>, AppError> {
    core_list_tags(paths).map_err(AppError::operation)
}

pub fn list_tagged_note_identities(
    paths: &VaultPaths,
    tag: &str,
) -> Result<Vec<NoteIdentity>, AppError> {
    core_list_tagged_note_identities(paths, tag).map_err(AppError::operation)
}

pub fn evaluate_base_file(paths: &VaultPaths, path: &str) -> Result<BasesEvalReport, AppError> {
    core_evaluate_base_file(paths, path).map_err(AppError::operation)
}

pub fn load_dataview_blocks(
    paths: &VaultPaths,
    path: &str,
    block: Option<usize>,
) -> Result<Vec<DataviewBlockRecord>, AppError> {
    core_load_dataview_blocks(paths, path, block).map_err(AppError::operation)
}

pub fn evaluate_dql(
    paths: &VaultPaths,
    source: &str,
    source_path: Option<&str>,
) -> Result<DqlQueryResult, DqlEvalError> {
    core_evaluate_dql(paths, source, source_path)
}

pub fn evaluate_dataview_js_query(
    paths: &VaultPaths,
    source: &str,
    source_path: Option<&str>,
) -> Result<DataviewJsResult, AppError> {
    core_evaluate_dataview_js_query(paths, source, source_path).map_err(AppError::operation)
}

#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn evaluate_note_inline_expressions(
    note: &NoteRecord,
    note_index: &HashMap<String, NoteRecord>,
) -> Vec<EvaluatedInlineExpression> {
    core_evaluate_note_inline_expressions(note, note_index)
}

pub fn move_note(
    paths: &VaultPaths,
    source_path: &str,
    destination: &str,
    dry_run: bool,
) -> Result<MoveSummary, AppError> {
    core_move_note(paths, source_path, destination, dry_run).map_err(AppError::operation)
}

pub fn load_kanban_board(
    paths: &VaultPaths,
    board: &str,
    include_archive: bool,
) -> Result<KanbanBoardRecord, AppError> {
    core_load_kanban_board(paths, board, include_archive).map_err(AppError::operation)
}

pub fn list_kanban_boards(paths: &VaultPaths) -> Result<Vec<KanbanBoardSummary>, AppError> {
    core_list_kanban_boards(paths).map_err(AppError::operation)
}

pub fn git_log(
    vault_root: &std::path::Path,
    path: &str,
    limit: usize,
) -> Result<Vec<GitLogEntry>, AppError> {
    core_git_log(vault_root, path, limit).map_err(AppError::operation)
}

#[must_use]
pub fn is_git_repo(vault_root: &std::path::Path) -> bool {
    core_is_git_repo(vault_root)
}

pub fn doctor_vault(paths: &VaultPaths) -> Result<DoctorReport, AppError> {
    core_doctor_vault(paths).map_err(AppError::operation)
}

pub fn query_backlinks(paths: &VaultPaths, path: &str) -> Result<BacklinksReport, AppError> {
    core_query_backlinks(paths, path).map_err(AppError::operation)
}

pub fn query_links(paths: &VaultPaths, path: &str) -> Result<OutgoingLinksReport, AppError> {
    core_query_links(paths, path).map_err(AppError::operation)
}

#[must_use]
pub fn load_periodic_config(paths: &VaultPaths) -> PeriodicConfig {
    load_vault_config(paths).config.periodic
}

pub fn list_daily_note_events(
    paths: &VaultPaths,
    start: &str,
    end: &str,
) -> Result<Vec<DailyNoteEvents>, AppError> {
    core_list_daily_note_events(paths, start, end).map_err(AppError::operation)
}

#[cfg(test)]
mod tests {
    use super::{list_note_identities, move_note, prepare_browse_refresh};
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::{initialize_vulcan_dir, scan_vault, AutoScanMode, ScanMode, VaultPaths};

    fn test_paths() -> (tempfile::TempDir, VaultPaths) {
        let dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        (dir, paths)
    }

    #[test]
    fn list_note_identities_reads_indexed_notes() {
        let (_dir, paths) = test_paths();
        fs::write(paths.vault_root().join("Inbox.md"), "# Inbox\n").expect("seed note");
        fs::write(paths.vault_root().join("Projects.md"), "# Projects\n").expect("seed note");
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let notes = list_note_identities(&paths).expect("notes should load");
        let paths = notes.into_iter().map(|note| note.path).collect::<Vec<_>>();
        assert!(paths.contains(&"Inbox.md".to_string()));
        assert!(paths.contains(&"Projects.md".to_string()));
    }

    #[test]
    fn browse_move_note_uses_shared_workflow_wrapper() {
        let (_dir, paths) = test_paths();
        fs::write(paths.vault_root().join("Old.md"), "# Old\n").expect("seed note");
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let summary =
            move_note(&paths, "Old.md", "Archive/New.md", false).expect("move should succeed");

        assert_eq!(summary.source_path, "Old.md");
        assert_eq!(summary.destination_path, "Archive/New.md");
        assert!(!paths.vault_root().join("Old.md").exists());
        assert!(paths.vault_root().join("Archive/New.md").exists());
    }

    #[test]
    fn prepare_browse_refresh_runs_blocking_refresh_when_cache_is_missing() {
        let (_dir, paths) = test_paths();
        fs::write(paths.vault_root().join("Inbox.md"), "# Inbox\n").expect("seed note");

        let summary = prepare_browse_refresh(&paths, AutoScanMode::Off)
            .expect("refresh should succeed")
            .expect("missing cache should trigger an initial refresh");

        assert_eq!(summary.mode, ScanMode::Incremental);
        assert_eq!(summary.added, 1);
    }
}
