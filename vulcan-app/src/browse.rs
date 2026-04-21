use crate::scan::refresh_cache_incrementally;
use crate::AppError;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::process::Command as ProcessCommand;
use vulcan_core::properties::load_note_index;
use vulcan_core::{
    doctor_vault as core_doctor_vault, evaluate_base_file as core_evaluate_base_file,
    evaluate_dataview_js_query as core_evaluate_dataview_js_query,
    evaluate_dataview_js_with_options as core_evaluate_dataview_js_with_options,
    evaluate_dql as core_evaluate_dql, evaluate_dql_with_filter as core_evaluate_dql_with_filter,
    evaluate_note_inline_expressions as core_evaluate_note_inline_expressions,
    git_log as core_git_log, git_status as core_git_status, inspect_cache as core_inspect_cache,
    is_git_repo as core_is_git_repo, list_daily_note_events as core_list_daily_note_events,
    list_kanban_boards as core_list_kanban_boards,
    list_note_identities as core_list_note_identities,
    list_tagged_note_identities as core_list_tagged_note_identities, list_tags as core_list_tags,
    load_dataview_blocks as core_load_dataview_blocks, load_kanban_board as core_load_kanban_board,
    load_vault_config, move_note as core_move_note, query_backlinks as core_query_backlinks,
    query_links as core_query_links, query_notes as core_query_notes, resolve_note_reference,
    search_vault as core_search_vault, AutoScanMode, BacklinksReport, BasesEvalReport,
    CacheDatabase, DailyNoteEvents, DataviewBlockRecord, DataviewJsEvalOptions, DataviewJsResult,
    DoctorReport, DqlEvalError, DqlQueryResult, EvaluatedInlineExpression, GitLogEntry,
    KanbanBoardRecord, KanbanBoardSummary, MoveSummary, NamedCount, NoteIdentity, NoteQuery,
    NoteRecord, NotesReport, OutgoingLinksReport, PeriodicConfig, PermissionFilter,
    PermissionGuard, ProfilePermissionGuard, ScanSummary, SearchQuery, SearchReport, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VaultStatusReport {
    pub vault_root: String,
    pub note_count: usize,
    pub attachment_count: usize,
    pub last_scan: Option<String>,
    pub cache_bytes: u64,
    pub git_branch: Option<String>,
    pub git_dirty: bool,
    pub git_staged: usize,
    pub git_unstaged: usize,
    pub git_untracked: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeriodicListItem {
    pub period_type: String,
    pub date: Option<String>,
    pub path: String,
    pub event_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DataviewInlineReport {
    pub file: String,
    pub results: Vec<EvaluatedInlineExpression>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DataviewEvalReport {
    pub file: String,
    pub blocks: Vec<DataviewBlockReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "engine", content = "data", rename_all = "snake_case")]
pub enum DataviewBlockResult {
    Dql(DqlQueryResult),
    Js(DataviewJsResult),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DataviewBlockReport {
    pub block_index: usize,
    pub line_number: i64,
    pub language: String,
    pub source: String,
    pub result: Option<DataviewBlockResult>,
    pub error: Option<String>,
}

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

pub fn build_dataview_inline_report(
    paths: &VaultPaths,
    file: &str,
    permissions: Option<&ProfilePermissionGuard>,
) -> Result<DataviewInlineReport, AppError> {
    let resolved = resolve_note_reference(paths, file).map_err(AppError::operation)?;
    if let Some(permissions) = permissions {
        permissions
            .check_read_path(&resolved.path)
            .map_err(AppError::operation)?;
    }
    let note_index = load_note_index(paths).map_err(AppError::operation)?;
    let note = note_index
        .values()
        .find(|note| note.document_path == resolved.path)
        .ok_or_else(|| AppError::operation(format!("note is not indexed: {}", resolved.path)))?;
    let results = core_evaluate_note_inline_expressions(note, &note_index);

    Ok(DataviewInlineReport {
        file: resolved.path,
        results,
    })
}

pub fn build_dataview_query_report(
    paths: &VaultPaths,
    source: &str,
    source_path: Option<&str>,
    filter: Option<&PermissionFilter>,
) -> Result<DqlQueryResult, AppError> {
    core_evaluate_dql_with_filter(paths, source, source_path, filter).map_err(AppError::operation)
}

pub fn build_dataview_query_js_report(
    paths: &VaultPaths,
    source: &str,
    source_path: Option<&str>,
    permission_profile: Option<&str>,
) -> Result<DataviewJsResult, AppError> {
    core_evaluate_dataview_js_with_options(
        paths,
        source,
        source_path,
        DataviewJsEvalOptions {
            timeout: None,
            sandbox: None,
            permission_profile: permission_profile.map(ToOwned::to_owned),
            ..DataviewJsEvalOptions::default()
        },
    )
    .map_err(AppError::operation)
}

pub fn build_dataview_eval_report(
    paths: &VaultPaths,
    file: &str,
    block: Option<usize>,
    permission_profile: Option<&str>,
    permissions: Option<&ProfilePermissionGuard>,
) -> Result<DataviewEvalReport, AppError> {
    let resolved = resolve_note_reference(paths, file).map_err(AppError::operation)?;
    if let Some(permissions) = permissions {
        permissions
            .check_read_path(&resolved.path)
            .map_err(AppError::operation)?;
    }
    let blocks = core_load_dataview_blocks(paths, file, block).map_err(AppError::operation)?;
    let file = blocks
        .first()
        .map_or_else(|| file.to_string(), |block| block.file.clone());
    let read_filter = permissions.map(PermissionGuard::read_filter);
    let mut reports = Vec::with_capacity(blocks.len());

    for block in blocks {
        let (result, error) = if block.language == "dataview" {
            match core_evaluate_dql_with_filter(
                paths,
                &block.source,
                Some(&block.file),
                read_filter.as_ref(),
            ) {
                Ok(result) => (Some(DataviewBlockResult::Dql(result)), None),
                Err(error) => (None, Some(error.to_string())),
            }
        } else if block.language == "dataviewjs" {
            match build_dataview_query_js_report(
                paths,
                &block.source,
                Some(&block.file),
                permission_profile,
            ) {
                Ok(result) => (Some(DataviewBlockResult::Js(result)), None),
                Err(error) => (None, Some(error.to_string())),
            }
        } else {
            (
                None,
                Some(format!(
                    "unsupported Dataview block language `{}`",
                    block.language
                )),
            )
        };

        reports.push(DataviewBlockReport {
            block_index: block.block_index,
            line_number: block.line_number,
            language: block.language,
            source: block.source,
            result,
            error,
        });
    }

    Ok(DataviewEvalReport {
        file,
        blocks: reports,
    })
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

pub fn build_vault_status_report(paths: &VaultPaths) -> Result<VaultStatusReport, AppError> {
    let cache = core_inspect_cache(paths).map_err(AppError::operation)?;
    let last_scan = CacheDatabase::open(paths).ok().and_then(|db| {
        db.connection()
            .query_row("SELECT MAX(indexed_at) FROM documents", [], |row| {
                row.get::<_, Option<String>>(0)
            })
            .ok()
            .flatten()
    });

    let (git_branch, git_dirty, git_staged, git_unstaged, git_untracked) =
        if core_is_git_repo(paths.vault_root()) {
            match core_git_status(paths.vault_root()) {
                Ok(status) => (
                    current_git_branch(paths.vault_root()),
                    !status.clean,
                    status.staged.len(),
                    status.unstaged.len(),
                    status.untracked.len(),
                ),
                Err(_) => (None, false, 0, 0, 0),
            }
        } else {
            (None, false, 0, 0, 0)
        };

    Ok(VaultStatusReport {
        vault_root: paths.vault_root().display().to_string(),
        note_count: cache.notes,
        attachment_count: cache.attachments,
        last_scan,
        cache_bytes: cache.database_bytes,
        git_branch,
        git_dirty,
        git_staged,
        git_unstaged,
        git_untracked,
    })
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

pub fn build_periodic_list_report(
    paths: &VaultPaths,
    period_type: Option<&str>,
) -> Result<Vec<PeriodicListItem>, AppError> {
    let config = load_vault_config(paths).config;
    if let Some(period_type) = period_type {
        if config.periodic.note(period_type).is_none() {
            return Err(AppError::operation(format!(
                "unknown periodic note type: {period_type}"
            )));
        }
    }

    let database = CacheDatabase::open(paths).map_err(AppError::operation)?;
    let mut statement = database
        .connection()
        .prepare(
            "
            SELECT
                documents.periodic_type,
                documents.periodic_date,
                documents.path,
                (
                    SELECT COUNT(*)
                    FROM events
                    WHERE events.document_id = documents.id
                ) AS event_count
            FROM documents
            WHERE documents.periodic_type IS NOT NULL
              AND (?1 IS NULL OR documents.periodic_type = ?1)
            ORDER BY documents.periodic_type, documents.periodic_date, documents.path
            ",
        )
        .map_err(AppError::operation)?;
    let rows = statement
        .query_map([period_type], |row| {
            Ok(PeriodicListItem {
                period_type: row.get(0)?,
                date: row.get(1)?,
                path: row.get(2)?,
                event_count: row.get::<_, i64>(3)?.try_into().unwrap_or(usize::MAX),
            })
        })
        .map_err(AppError::operation)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(AppError::operation)
}

pub fn collect_complete_candidates(
    paths: &VaultPaths,
    context: &str,
) -> Result<Vec<String>, AppError> {
    let candidates = match context {
        "note" => {
            let notes = list_note_identities(paths)?;
            let mut seen = BTreeSet::new();
            let mut candidates = Vec::new();
            for note in notes {
                if !note.filename.is_empty()
                    && note.filename != note.path
                    && seen.insert(note.filename.clone())
                {
                    candidates.push(note.filename);
                }
                if seen.insert(note.path.clone()) {
                    candidates.push(note.path);
                }
            }
            candidates
        }
        "daily-date" => {
            let mut dates: Vec<String> = load_note_index(paths)
                .map_err(AppError::operation)?
                .into_values()
                .filter(|note| note.periodic_type.as_deref() == Some("daily"))
                .filter_map(|note| note.periodic_date)
                .collect::<Vec<_>>();
            dates.sort_by(|left, right| right.cmp(left));
            dates.dedup();
            dates
        }
        "kanban-board" => list_kanban_boards(paths)?
            .into_iter()
            .map(|board| board.path)
            .collect(),
        "bases-file" | "bases-view" => {
            let mut paths: Vec<String> = load_note_index(paths)
                .map_err(AppError::operation)?
                .into_values()
                .map(|note| note.document_path)
                .filter(|path| {
                    std::path::Path::new(path)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("base"))
                })
                .collect::<Vec<_>>();
            paths.sort();
            paths.dedup();
            paths
        }
        "task-view" => {
            let mut out: Vec<String> = load_vault_config(paths)
                .config
                .tasknotes
                .saved_views
                .iter()
                .map(|view| view.id.clone())
                .collect();
            let mut base_files = collect_complete_candidates(paths, "bases-view")?;
            out.append(&mut base_files);
            dedupe_strings_preserve_order(out)
        }
        _ => Vec::new(),
    };
    Ok(candidates)
}

fn current_git_branch(vault_root: &std::path::Path) -> Option<String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(vault_root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn dedupe_strings_preserve_order(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            deduped.push(value);
        }
    }
    deduped
}

#[cfg(test)]
mod tests {
    use super::{
        build_dataview_eval_report, build_dataview_inline_report, build_periodic_list_report,
        build_vault_status_report, collect_complete_candidates, list_note_identities, move_note,
        prepare_browse_refresh,
    };
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
        fs::write(
            paths.vault_root().join("Inbox.md"),
            "# Inbox
",
        )
        .expect("seed note");
        fs::write(
            paths.vault_root().join("Projects.md"),
            "# Projects
",
        )
        .expect("seed note");
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let notes = list_note_identities(&paths).expect("notes should load");
        let paths = notes.into_iter().map(|note| note.path).collect::<Vec<_>>();
        assert!(paths.contains(&"Inbox.md".to_string()));
        assert!(paths.contains(&"Projects.md".to_string()));
    }

    #[test]
    fn browse_move_note_uses_shared_workflow_wrapper() {
        let (_dir, paths) = test_paths();
        fs::write(
            paths.vault_root().join("Old.md"),
            "# Old
",
        )
        .expect("seed note");
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
        fs::write(
            paths.vault_root().join("Inbox.md"),
            "# Inbox
",
        )
        .expect("seed note");

        let summary = prepare_browse_refresh(&paths, AutoScanMode::Off)
            .expect("refresh should succeed")
            .expect("missing cache should trigger an initial refresh");

        assert_eq!(summary.mode, ScanMode::Incremental);
        assert_eq!(summary.added, 1);
    }

    #[test]
    fn build_vault_status_report_reads_cache_metadata() {
        let (_dir, paths) = test_paths();
        fs::write(
            paths.vault_root().join("Inbox.md"),
            "# Inbox
",
        )
        .expect("seed note");
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let report = build_vault_status_report(&paths).expect("status report");
        assert_eq!(report.note_count, 1);
        assert_eq!(report.attachment_count, 0);
        assert!(report.cache_bytes > 0);
        assert!(report.last_scan.is_some());
    }

    #[test]
    fn build_periodic_list_report_reads_periodic_notes() {
        let (_dir, paths) = test_paths();
        fs::create_dir_all(paths.vault_root().join("Journal/Daily")).expect("daily dir");
        fs::write(
            paths.config_file(),
            r#"[periodic.daily]
folder = "Journal/Daily"
format = "YYYY-MM-DD"
"#,
        )
        .expect("config should write");
        fs::write(
            paths.vault_root().join("Journal/Daily/2026-04-20.md"),
            "# Day\n",
        )
        .expect("daily note");
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let items = build_periodic_list_report(&paths, Some("daily")).expect("periodic list");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].period_type, "daily");
        assert_eq!(items[0].date.as_deref(), Some("2026-04-20"));
        assert_eq!(items[0].path, "Journal/Daily/2026-04-20.md");
    }

    #[test]
    fn collect_complete_candidates_lists_daily_dates_and_base_files() {
        let (_dir, paths) = test_paths();
        fs::create_dir_all(paths.vault_root().join("Views")).expect("views dir");
        fs::create_dir_all(paths.vault_root().join("Journal/Daily")).expect("daily dir");
        fs::write(
            paths.config_file(),
            r#"[periodic.daily]
folder = "Journal/Daily"
format = "YYYY-MM-DD"

[[tasknotes.saved_views]]
id = "blocked"
name = "Blocked Tasks"

[tasknotes.saved_views.query]
type = "group"
id = "root"
conjunction = "and"
sortKey = "due"
sortDirection = "asc"

[[tasknotes.saved_views.query.children]]
type = "condition"
id = "status-filter"
property = "status"
operator = "is"
value = "blocked"
"#,
        )
        .expect("config should write");
        fs::write(
            paths.vault_root().join("Views/Tasks.base"),
            "views:\n  - type: table\n",
        )
        .expect("base view");
        fs::write(
            paths.vault_root().join("Journal/Daily/2026-04-20.md"),
            "# Day\n",
        )
        .expect("daily note");
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        assert_eq!(
            collect_complete_candidates(&paths, "daily-date").expect("daily dates"),
            vec!["2026-04-20".to_string()]
        );
        assert_eq!(
            collect_complete_candidates(&paths, "bases-view").expect("base files"),
            vec!["Views/Tasks.base".to_string()]
        );
        assert_eq!(
            collect_complete_candidates(&paths, "task-view").expect("task views"),
            vec!["blocked".to_string(), "Views/Tasks.base".to_string()]
        );
    }

    #[test]
    fn build_dataview_reports_use_shared_workflows() {
        let dir = tempdir().expect("temp dir");
        let vault_root = dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");
        let paths = VaultPaths::new(&vault_root);

        let inline = build_dataview_inline_report(&paths, "Dashboard", None).expect("inline");
        assert_eq!(inline.file, "Dashboard.md");
        assert_eq!(inline.results[0].value, serde_json::json!("draft"));

        let eval = build_dataview_eval_report(&paths, "Dashboard", None, None, None).expect("eval");
        assert_eq!(eval.file, "Dashboard.md");
        assert_eq!(eval.blocks.len(), 2);
    }

    fn copy_fixture_vault(name: &str, destination: &std::path::Path) {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);
        copy_dir_recursive(&source, destination);
        fs::create_dir_all(destination.join(".vulcan")).expect(".vulcan dir should be created");
    }

    fn copy_dir_recursive(source: &std::path::Path, destination: &std::path::Path) {
        fs::create_dir_all(destination).expect("destination directory should be created");

        for entry in fs::read_dir(source).expect("source directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            let file_type = entry.file_type().expect("file type should be readable");
            let target = destination.join(entry.file_name());

            if file_type.is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else if file_type.is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).expect("parent directory should exist");
                }
                fs::copy(entry.path(), target).expect("file should be copied");
            }
        }
    }
}
