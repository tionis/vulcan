use crate::mdbase::mdbase_js_mutation_committer;
use crate::scan::refresh_cache_incrementally;
use crate::tools::{build_custom_tool_js_registry, CustomToolRegistryOptions};
use crate::{plugins, templates, tools, AppError};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::process::Command as ProcessCommand;
use vulcan_core::note_session::NoteStoreSession;
use vulcan_core::properties::load_note_index;
use vulcan_core::{
    doctor_vault as core_doctor_vault, evaluate_base_file as core_evaluate_base_file,
    evaluate_dataview_js_query as core_evaluate_dataview_js_query,
    evaluate_dataview_js_with_options as core_evaluate_dataview_js_with_options,
    evaluate_dql as core_evaluate_dql, evaluate_dql_with_filter as core_evaluate_dql_with_filter,
    evaluate_dql_with_guard as core_evaluate_dql_with_guard,
    evaluate_note_inline_expressions as core_evaluate_note_inline_expressions,
    git_log as core_git_log, git_status as core_git_status, inspect_cache as core_inspect_cache,
    is_git_repo as core_is_git_repo, list_assistant_skills,
    list_daily_note_events as core_list_daily_note_events,
    list_kanban_boards as core_list_kanban_boards,
    list_note_identities as core_list_note_identities, list_saved_reports,
    list_tagged_note_identities as core_list_tagged_note_identities, list_tags as core_list_tags,
    load_assistant_skill, load_dataview_blocks as core_load_dataview_blocks,
    load_kanban_board as core_load_kanban_board, load_permission_profiles, load_vault_config,
    move_note as core_move_note, query_backlinks as core_query_backlinks,
    query_graph_analytics as core_query_graph_analytics, query_links as core_query_links,
    query_notes as core_query_notes, resolve_note_reference, search_vault as core_search_vault,
    AutoScanMode, BacklinksReport, BasesEvalReport, CacheDatabase, DailyNoteEvents,
    DataviewBlockRecord, DataviewJsEvalOptions, DataviewJsResult, DoctorReport, DqlEvalError,
    DqlQueryResult, EvaluatedInlineExpression, GitLogEntry, GraphConfidenceBreakdown,
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
    /// Enclosing Git work tree, reported when the vault is nested below it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_repository_root: Option<String>,
    /// Vault root relative to `git_repository_root`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_vault_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mkdocs_config: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layout_hints: Vec<String>,
    pub graph_confidence: Option<GraphConfidenceBreakdown>,
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
    let _read_guard = vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    core_list_note_identities(paths).map_err(AppError::operation)
}

pub fn search_vault(paths: &VaultPaths, query: &SearchQuery) -> Result<SearchReport, AppError> {
    core_search_vault(paths, query).map_err(AppError::operation)
}

pub fn query_notes(paths: &VaultPaths, query: &NoteQuery) -> Result<NotesReport, AppError> {
    core_query_notes(paths, query).map_err(AppError::operation)
}

pub fn list_tags(paths: &VaultPaths) -> Result<Vec<NamedCount>, AppError> {
    let _read_guard = vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    core_list_tags(paths).map_err(AppError::operation)
}

pub fn list_tagged_note_identities(
    paths: &VaultPaths,
    tag: &str,
) -> Result<Vec<NoteIdentity>, AppError> {
    let _read_guard = vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    core_list_tagged_note_identities(paths, tag).map_err(AppError::operation)
}

pub fn evaluate_base_file(paths: &VaultPaths, path: &str) -> Result<BasesEvalReport, AppError> {
    core_evaluate_base_file(paths, path).map_err(AppError::operation)
}

/// Evaluate a base under the caller's read authority. The evaluation uses one
/// immutable policy snapshot and rechecks the profile, hook trust, and grants
/// before and after derivation so a changed authority never yields a report.
pub fn evaluate_base_file_with_guard(
    paths: &VaultPaths,
    path: &str,
    guard: &ProfilePermissionGuard,
) -> Result<BasesEvalReport, AppError> {
    evaluate_base_file_with_guard_and_plan(paths, path, guard, false)
}

/// [`evaluate_base_file_with_guard`], reporting each view's note plan when
/// `explain` is set (QRY.5).
pub fn evaluate_base_file_with_guard_and_plan(
    paths: &VaultPaths,
    path: &str,
    guard: &ProfilePermissionGuard,
    explain: bool,
) -> Result<BasesEvalReport, AppError> {
    evaluate_base_file_in(None, paths, path, guard, explain)
}

/// [`evaluate_base_file_with_guard_and_plan`] through a host's retained note
/// session when one is attached (QRY.6). A session snapshot shows only
/// completed writes and needs no read lock; without one the evaluation
/// holds the shared vault lock as usual.
pub fn evaluate_base_file_in(
    session: Option<&NoteStoreSession>,
    paths: &VaultPaths,
    path: &str,
    guard: &ProfilePermissionGuard,
    explain: bool,
) -> Result<BasesEvalReport, AppError> {
    recheck_read_authority(paths, guard, "bases")?;
    let policy = guard.snapshot_read_policy().map_err(AppError::operation)?;
    recheck_read_authority(paths, &policy, "bases")?;
    let report = if let Some(store) = session.and_then(NoteStoreSession::snapshot) {
        vulcan_core::bases::evaluate_base_file_in(&store, paths, path, &policy, explain)
    } else {
        let _read_guard = vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
            .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
        vulcan_core::bases::evaluate_base_file_with_guard_and_plan(paths, path, &policy, explain)
    };
    recheck_read_authority(paths, &policy, "bases")?;
    report.map_err(AppError::operation)
}

/// Reject a guard whose profile selection or policy snapshot is no longer
/// current. `subject` names the read surface in the error message.
pub(crate) fn recheck_read_authority(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    subject: &str,
) -> Result<(), AppError> {
    let current = vulcan_core::resolve_permission_profile(paths, Some(guard.profile_name()))
        .map_err(AppError::operation)?;
    if &current != guard.selection() {
        return Err(AppError::operation_with_code(
            "permission_denied",
            format!("{subject} read authority changed; resolve a new guard"),
        ));
    }
    guard
        .recheck_read_policy_snapshot()
        .map_err(AppError::operation)
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
    // Resolve and evaluate inside the caller's universe so inline expressions
    // cannot read hidden linked notes or count hidden backlinks.
    let (resolved, note_index) = match permissions {
        Some(guard) => (
            vulcan_core::graph::resolve_note_reference_with_guard(paths, file, guard)
                .map_err(AppError::operation)?,
            vulcan_core::properties::load_note_index_with_guard(paths, guard)
                .map_err(AppError::operation)?,
        ),
        None => (
            resolve_note_reference(paths, file).map_err(AppError::operation)?,
            load_note_index(paths).map_err(AppError::operation)?,
        ),
    };
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

/// Evaluate DQL under the caller's read authority with one immutable policy
/// snapshot, rechecking profile, grants, and hook trust after derivation.
pub fn build_dataview_query_report_with_guard(
    paths: &VaultPaths,
    source: &str,
    source_path: Option<&str>,
    guard: &ProfilePermissionGuard,
) -> Result<DqlQueryResult, AppError> {
    build_dataview_query_report_with_guard_and_plan(paths, source, source_path, guard, false)
}

/// [`build_dataview_query_report_with_guard`], reporting the note plan in
/// `DqlQueryResult::plan` when `explain` is set (QRY.5).
pub fn build_dataview_query_report_with_guard_and_plan(
    paths: &VaultPaths,
    source: &str,
    source_path: Option<&str>,
    guard: &ProfilePermissionGuard,
    explain: bool,
) -> Result<DqlQueryResult, AppError> {
    build_dataview_query_report_in(None, paths, source, source_path, guard, explain)
}

/// [`build_dataview_query_report_with_guard_and_plan`] through a host's
/// retained note session when one is attached (QRY.6); see
/// [`evaluate_base_file_in`].
pub fn build_dataview_query_report_in(
    session: Option<&NoteStoreSession>,
    paths: &VaultPaths,
    source: &str,
    source_path: Option<&str>,
    guard: &ProfilePermissionGuard,
    explain: bool,
) -> Result<DqlQueryResult, AppError> {
    recheck_read_authority(paths, guard, "dataview")?;
    let policy = guard.snapshot_read_policy().map_err(AppError::operation)?;
    recheck_read_authority(paths, &policy, "dataview")?;
    let result = if let Some(store) = session.and_then(NoteStoreSession::snapshot) {
        vulcan_core::dql::evaluate_dql_in(&store, paths, source, source_path, &policy, explain)
    } else {
        let _read_guard = vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
            .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
        vulcan_core::dql::evaluate_dql_with_guard_and_plan(
            paths,
            source,
            source_path,
            &policy,
            explain,
        )
    };
    recheck_read_authority(paths, &policy, "dataview")?;
    result.map_err(AppError::operation)
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
            mutation_committer: Some(mdbase_js_mutation_committer(
                paths,
                permission_profile,
                true,
            )),
            tool_registry: Some(build_custom_tool_js_registry(
                paths,
                permission_profile,
                "dataview.query_js",
                &CustomToolRegistryOptions::default(),
            )),
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
    let mut reports = Vec::with_capacity(blocks.len());

    for block in blocks {
        let (result, error) = if block.language == "dataview" {
            let evaluated = match permissions {
                Some(guard) => {
                    core_evaluate_dql_with_guard(paths, &block.source, Some(&block.file), guard)
                }
                None => {
                    core_evaluate_dql_with_filter(paths, &block.source, Some(&block.file), None)
                }
            };
            match evaluated {
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
    move_note_with_profile(paths, source_path, destination, dry_run, None)
}

/// Move a note and rewrite references to it. When the note or any rewritten
/// reference is an mdbase record, the move, its reference rewrites, and any
/// ordinary companions commit as one validated mdbase rename under
/// `permission_profile`; validation or permission failures are returned,
/// never retried as an ordinary move. Other moves use the ordinary move
/// journal.
pub fn move_note_with_profile(
    paths: &VaultPaths,
    source_path: &str,
    destination: &str,
    dry_run: bool,
    permission_profile: Option<&str>,
) -> Result<MoveSummary, AppError> {
    use crate::mdbase::{
        apply_managed_mdbase_note_writes, MdbaseManagedNoteWriteBatchRequest,
        MdbaseManagedNoteWriteChange, MdbaseManagedWriteMode, MdbaseWriteOperation,
    };
    let plan = vulcan_core::move_rewrite::plan_ordinary_note_move(paths, source_path, destination)
        .map_err(AppError::operation)?;
    if !plan.changes.is_empty() {
        let changes = plan
            .changes
            .iter()
            .map(|change| MdbaseManagedNoteWriteChange {
                path: &change.path,
                before: change.before.as_deref(),
                after: change.after.as_deref(),
            })
            .collect::<Vec<_>>();
        let routed = apply_managed_mdbase_note_writes(
            paths,
            &MdbaseManagedNoteWriteBatchRequest {
                changes: &changes,
                operation: MdbaseWriteOperation::Rename {
                    from: plan.summary.source_path.clone(),
                    to: plan.summary.destination_path.clone(),
                },
                mode: MdbaseManagedWriteMode::Validated,
                allow_mixed_paths: true,
                dry_run,
                permission_profile,
                quiet: true,
            },
        )?;
        if routed.is_some() {
            let mut summary = plan.summary;
            summary.dry_run = dry_run;
            return Ok(summary);
        }
    }
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
    let _read_guard = vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
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

    let layout = crate::vault_layout::inspect_vault_layout(paths);
    let nested_repository = layout
        .repository
        .as_ref()
        .filter(|repository| repository.is_nested());

    Ok(VaultStatusReport {
        git_repository_root: nested_repository
            .map(|repository| repository.work_tree.display().to_string()),
        git_vault_prefix: nested_repository.map(|repository| repository.vault_prefix.clone()),
        mkdocs_config: layout
            .mkdocs
            .as_ref()
            .map(|mkdocs| mkdocs.config_file.display().to_string()),
        layout_hints: layout.hints,
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
        graph_confidence: core_query_graph_analytics(paths)
            .ok()
            .map(|report| report.confidence),
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

#[allow(clippy::too_many_lines)]
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
        "export-profile" => load_vault_config(paths)
            .config
            .export
            .profiles
            .into_keys()
            .collect(),
        "outline-profile" => load_vault_config(paths)
            .config
            .publish
            .outline
            .profiles
            .into_keys()
            .collect(),
        "integration-route" => load_vault_config(paths)
            .config
            .integrations
            .routes
            .into_keys()
            .collect(),
        "site-profile" => load_vault_config(paths)
            .config
            .site
            .profiles
            .into_keys()
            .collect(),
        "permission-profile" => load_permission_profiles(paths)
            .profiles
            .into_keys()
            .collect(),
        "config-alias" => load_vault_config(paths)
            .config
            .aliases
            .into_keys()
            .collect(),
        "plugin" => plugins::list_plugins(paths)
            .into_iter()
            .map(|plugin| plugin.name)
            .collect(),
        "saved-report" => list_saved_reports(paths)
            .map_err(AppError::operation)?
            .into_iter()
            .map(|report| report.name)
            .collect(),
        "template" => templates::build_template_list_report(paths)?
            .templates
            .into_iter()
            .map(|template| template.name.trim_end_matches(".md").to_string())
            .collect(),
        "skill" => list_assistant_skills(paths)
            .map_err(AppError::operation)?
            .into_iter()
            .map(|skill| skill.name)
            .collect(),
        context if context.starts_with("skill-command:") => {
            let skill = context.trim_start_matches("skill-command:");
            if skill.is_empty() {
                Vec::new()
            } else {
                load_assistant_skill(paths, skill)
                    .map_err(AppError::operation)?
                    .summary
                    .commands
                    .into_iter()
                    .map(|command| command.id)
                    .collect()
            }
        }
        "custom-tool" => tools::collect_custom_tool_cli_name_candidates(
            paths,
            &tools::CustomToolRegistryOptions::default(),
        )?,
        context if context.starts_with("custom-tool-flag:") => {
            let name = context.trim_start_matches("custom-tool-flag:");
            if name.is_empty() {
                Vec::new()
            } else {
                tools::collect_custom_tool_cli_flag_candidates(
                    paths,
                    name,
                    &tools::CustomToolRegistryOptions::default(),
                )?
            }
        }
        context if context.starts_with("custom-tool-value:") => {
            let Some((name, flag)) = context
                .trim_start_matches("custom-tool-value:")
                .rsplit_once(':')
            else {
                return Ok(Vec::new());
            };
            let mut candidates = tools::collect_custom_tool_cli_choice_candidates(
                paths,
                name,
                flag,
                &tools::CustomToolRegistryOptions::default(),
            )?;
            if candidates.is_empty() {
                if let Some(completion) = tools::custom_tool_cli_flag_completion_context(
                    paths,
                    name,
                    flag,
                    &tools::CustomToolRegistryOptions::default(),
                )? {
                    candidates = collect_complete_candidates(paths, &completion)?;
                }
            }
            candidates
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
        build_vault_status_report, collect_complete_candidates, list_note_identities,
        list_tagged_note_identities, list_tags, move_note, prepare_browse_refresh,
    };
    use serde::Serialize;
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::{initialize_vulcan_dir, scan_vault, AutoScanMode, ScanMode, VaultPaths};

    fn test_paths() -> (tempfile::TempDir, VaultPaths) {
        let dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        (dir, paths)
    }

    fn write_pending_journal(paths: &VaultPaths) {
        #[derive(Serialize)]
        struct JournalFixture<'a> {
            version: u32,
            transaction_id: &'a str,
            changes: Vec<vulcan_core::ordinary_write::OrdinaryWriteChange>,
            digest: String,
        }

        let directory = paths
            .operational_state_dir()
            .expect("operational state")
            .join("ordinary-write");
        fs::create_dir_all(&directory).expect("journal directory");
        let mut journal = JournalFixture {
            version: 1,
            transaction_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            changes: vec![vulcan_core::ordinary_write::OrdinaryWriteChange {
                path: "Inbox.md".to_string(),
                before: Some("# Original\n".to_string()),
                after: Some("# Updated\n".to_string()),
            }],
            digest: String::new(),
        };
        journal.digest = blake3::hash(&serde_json::to_vec(&journal).expect("journal bytes"))
            .to_hex()
            .to_string();
        let journal_path = directory.join("journal.json");
        fs::write(
            &journal_path,
            serde_json::to_vec(&journal).expect("sealed journal"),
        )
        .expect("pending journal");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o600))
                .expect("owner-only journal");
        }
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
    fn build_vault_status_report_refuses_pending_ordinary_write_journal() {
        let (_dir, paths) = test_paths();
        fs::write(paths.vault_root().join("Inbox.md"), "# Original\n").expect("seed note");
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        write_pending_journal(&paths);

        assert_eq!(
            build_vault_status_report(&paths)
                .expect_err("status must fail closed")
                .code(),
            Some("ordinary_write_pending")
        );
    }

    #[test]
    fn browse_identity_and_tag_lists_refuse_pending_ordinary_write_journal() {
        let (_dir, paths) = test_paths();
        fs::write(paths.vault_root().join("Inbox.md"), "# Original\n").expect("seed note");
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        write_pending_journal(&paths);

        for error in [
            list_note_identities(&paths).expect_err("identities must fail closed"),
            list_tags(&paths).expect_err("tags must fail closed"),
            list_tagged_note_identities(&paths, "work").expect_err("tagged notes must fail closed"),
        ] {
            assert_eq!(error.code(), Some("ordinary_write_pending"));
        }
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
    fn collect_complete_candidates_lists_configured_identifiers_and_resources() {
        let (_dir, paths) = test_paths();
        fs::write(
            paths.config_file(),
            r#"[export.profiles.book]
format = "epub"

[publish.outline.profiles.players]
collection_title = "Players"

[integrations.routes.campaign]
profile = "players"

[site.profiles.public]
title = "Public"

[plugins.calendar]
enabled = true
"#,
        )
        .expect("config should write");
        fs::write(paths.local_config_file(), "[aliases]\nqq = \"query\"\n")
            .expect("local config should write");
        fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
        fs::write(
            paths.vulcan_dir().join("templates/session.md"),
            "# Session\n",
        )
        .expect("template should write");
        fs::create_dir_all(paths.reports_dir()).expect("reports dir");
        fs::write(
            paths.reports_dir().join("open-quests.toml"),
            "kind = \"notes\"\nfilters = []\nsort_descending = false\n",
        )
        .expect("saved report should write");
        fs::create_dir_all(paths.vulcan_dir().join("plugins")).expect("plugins dir");
        fs::write(
            paths.vulcan_dir().join("plugins/weather.js"),
            "export function main() {}\n",
        )
        .expect("plugin should write");

        for (context, expected) in [
            ("export-profile", "book"),
            ("outline-profile", "players"),
            ("integration-route", "campaign"),
            ("site-profile", "public"),
            ("config-alias", "qq"),
            ("plugin", "calendar"),
            ("plugin", "weather"),
            ("saved-report", "open-quests"),
            ("template", "session"),
            ("permission-profile", "readonly"),
        ] {
            assert!(
                collect_complete_candidates(&paths, context)
                    .unwrap_or_else(|error| panic!("{context} candidates failed: {error}"))
                    .contains(&expected.to_string()),
                "{context} should include {expected}"
            );
        }
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
