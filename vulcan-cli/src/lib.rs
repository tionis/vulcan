mod bases_tui;
mod cli;
mod note_picker;
mod serve;

pub use cli::{
    AutomationCommand, BasesCommand, CacheCommand, CheckpointCommand, Cli, Command, ExportArgs,
    ExportCommand, ExportFormat, GraphCommand, OutputFormat, RepairCommand, SavedCommand,
    SearchMode, SuggestCommand, VectorQueueCommand, VectorsCommand,
};

use clap::{CommandFactory, Parser};
use clap_complete::generate;
use serde::Serialize;
use serde_json::{Map, Value};
use serve::{serve_forever, ServeOptions};
use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vulcan_core::{
    bases_view_add, bases_view_delete, bases_view_edit, bases_view_rename, bulk_replace,
    bulk_set_property, cache_vacuum, cluster_vectors, create_checkpoint, doctor_fix, doctor_vault,
    drop_vector_model, evaluate_base_file, execute_query_report, export_static_search_index,
    index_vectors_with_progress, initialize_vault, inspect_cache, inspect_vector_queue,
    link_mentions, list_checkpoints, list_saved_reports, list_vector_models, load_saved_report,
    merge_tags, move_note,
    query_backlinks, query_change_report, query_graph_analytics, query_graph_components,
    query_graph_dead_ends, query_graph_hubs, query_graph_moc_candidates, query_graph_path,
    query_graph_trends, query_links, query_notes, query_related_notes, query_vector_neighbors,
    rebuild_vault_with_progress, rebuild_vectors_with_progress, rename_alias, rename_block_ref,
    rename_heading, rename_property, repair_fts, repair_vectors_with_progress,
    resolve_note_reference, save_saved_report, scan_vault_with_progress, search_vault,
    suggest_duplicates, suggest_mentions, vector_duplicates, verify_cache, watch_vault,
    BacklinkRecord, BacklinksReport, BaseViewGroupBy, BaseViewPatch, BaseViewSpec, BasesEvalReport,
    BasesViewEditReport, BulkMutationReport, CacheInspectReport, CacheVacuumQuery,
    CacheVacuumReport, CacheVerifyReport, ChangeAnchor, ChangeItem, ChangeKind, ChangeReport,
    CheckpointRecord, ClusterQuery, ClusterReport, DoctorDiagnosticIssue, DoctorFixReport,
    DoctorLinkIssue, DoctorReport, DuplicateSuggestionsReport, GraphAnalyticsReport,
    GraphComponentsReport, GraphDeadEndsReport, GraphHubsReport, GraphMocCandidate, GraphMocReport,
    GraphPathReport, GraphQueryError, GraphTrendsReport, InitSummary, MentionSuggestion,
    MentionSuggestionsReport, MergeCandidate, MoveSummary, NamedCount, NoteQuery, NoteRecord,
    NotesReport, OutgoingLinkRecord, OutgoingLinksReport, QueryAst, QueryReport, RebuildQuery,
    RebuildReport, RefactorReport, RelatedNoteHit, RelatedNotesQuery, RelatedNotesReport,
    RepairFtsQuery, RepairFtsReport, SavedExport, SavedExportFormat, SavedReportDefinition,
    SavedReportKind, SavedReportQuery, SavedReportSummary, ScanMode, ScanPhase, ScanProgress,
    ScanSummary, SearchHit, SearchQuery, SearchReport, VaultPaths, VectorDuplicatePair,
    VectorDuplicatesQuery, VectorDuplicatesReport, VectorIndexPhase, VectorIndexProgress,
    VectorIndexQuery, VectorIndexReport, VectorNeighborHit, VectorNeighborsQuery,
    VectorNeighborsReport, VectorQueueReport, VectorRebuildQuery, VectorRepairQuery,
    VectorRepairReport, StoredModelInfo, WatchOptions, WatchReport,
};

#[derive(Debug)]
pub struct CliError {
    exit_code: u8,
    message: String,
}

impl CliError {
    pub(crate) fn io(error: &io::Error) -> Self {
        Self {
            exit_code: 1,
            message: format!("failed to read current working directory: {error}"),
        }
    }

    pub(crate) fn operation(error: impl Display) -> Self {
        Self {
            exit_code: 1,
            message: error.to_string(),
        }
    }

    pub(crate) fn issues(message: impl Into<String>) -> Self {
        Self {
            exit_code: 2,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn exit_code(&self) -> u8 {
        self.exit_code
    }
}

impl Display for CliError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

const SCAN_PROGRESS_STEP: usize = 250;
const BASES_MAX_COLUMN_WIDTH: usize = 28;

#[derive(Clone, Copy)]
struct AnsiPalette {
    enabled: bool,
}

impl AnsiPalette {
    fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    fn bold(self, text: &str) -> String {
        self.wrap("1", text)
    }

    fn cyan(self, text: &str) -> String {
        self.wrap("36", text)
    }

    fn green(self, text: &str) -> String {
        self.wrap("32", text)
    }

    fn yellow(self, text: &str) -> String {
        self.wrap("33", text)
    }

    fn red(self, text: &str) -> String {
        self.wrap("31", text)
    }

    fn dim(self, text: &str) -> String {
        self.wrap("2", text)
    }

    fn wrap(self, code: &str, text: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
}

struct ScanProgressReporter {
    palette: AnsiPalette,
    started_at: Instant,
    last_phase: Option<ScanPhase>,
    next_checkpoint: usize,
}

impl ScanProgressReporter {
    fn new(use_color: bool) -> Self {
        Self {
            palette: AnsiPalette::new(use_color),
            started_at: Instant::now(),
            last_phase: None,
            next_checkpoint: SCAN_PROGRESS_STEP,
        }
    }

    fn record(&mut self, progress: &ScanProgress) {
        match progress.phase {
            ScanPhase::PreparingFiles => {
                if self.last_phase != Some(progress.phase) {
                    eprintln!(
                        "{} {} files for a {} scan...",
                        self.palette.cyan("Preparing"),
                        progress.discovered,
                        match progress.mode {
                            ScanMode::Full => "full",
                            ScanMode::Incremental => "incremental",
                        }
                    );
                    self.last_phase = Some(progress.phase);
                }
            }
            ScanPhase::ScanningFiles => {
                if progress.processed == 0 {
                    eprintln!(
                        "{} {} files; running {} scan...",
                        self.palette.cyan("Discovered"),
                        progress.discovered,
                        self.palette.bold(match progress.mode {
                            ScanMode::Full => "full",
                            ScanMode::Incremental => "incremental",
                        })
                    );
                    self.last_phase = Some(progress.phase);
                    self.next_checkpoint = SCAN_PROGRESS_STEP.min(progress.discovered.max(1));
                    return;
                }

                if progress.processed >= self.next_checkpoint
                    || progress.processed == progress.discovered
                {
                    let elapsed = self.started_at.elapsed();
                    let rate =
                        count_as_f64(progress.processed) / elapsed.as_secs_f64().max(f64::EPSILON);
                    let remaining = progress.discovered.saturating_sub(progress.processed);
                    eprintln!(
                        "{} {}/{} files: {} added, {} updated, {} unchanged, {} deleted | {} | {}",
                        self.palette.cyan("Scanned"),
                        self.palette.bold(&progress.processed.to_string()),
                        progress.discovered,
                        self.palette.green(&progress.added.to_string()),
                        self.palette.yellow(&progress.updated.to_string()),
                        progress.unchanged,
                        self.palette.red(&progress.deleted.to_string()),
                        self.palette.dim(&format!("{rate:.0} files/s")),
                        self.palette
                            .dim(&format!("ETA {}", format_eta(remaining, rate)))
                    );
                    while self.next_checkpoint <= progress.processed {
                        self.next_checkpoint += SCAN_PROGRESS_STEP;
                    }
                }
            }
            ScanPhase::RefreshingPropertyCatalog | ScanPhase::ResolvingLinks => {
                if self.last_phase != Some(progress.phase) {
                    eprintln!(
                        "{}...",
                        self.palette.cyan(match progress.phase {
                            ScanPhase::RefreshingPropertyCatalog => "Refreshing property catalog",
                            ScanPhase::ResolvingLinks => "Resolving links",
                            ScanPhase::PreparingFiles
                            | ScanPhase::ScanningFiles
                            | ScanPhase::Completed => unreachable!(),
                        })
                    );
                    self.last_phase = Some(progress.phase);
                }
            }
            ScanPhase::Completed => {}
        }
    }
}

struct VectorIndexProgressReporter {
    palette: AnsiPalette,
    started_at: Instant,
    last_batches_completed: usize,
    prepared: bool,
    verbose: bool,
}

impl VectorIndexProgressReporter {
    fn new(use_color: bool, verbose: bool) -> Self {
        Self {
            palette: AnsiPalette::new(use_color),
            started_at: Instant::now(),
            last_batches_completed: 0,
            prepared: false,
            verbose,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn record(&mut self, progress: &VectorIndexProgress) {
        match progress.phase {
            VectorIndexPhase::Preparing => {
                if self.prepared {
                    return;
                }
                self.prepared = true;
                if self.verbose {
                    eprintln!(
                        "{} {}:{}",
                        self.palette.dim("Provider:"),
                        progress.provider_name,
                        progress.model_name
                    );
                    eprintln!(
                        "{} {}",
                        self.palette.dim("Endpoint:"),
                        progress.endpoint_url
                    );
                    let key_status = match (&progress.api_key_env, progress.api_key_set) {
                        (Some(env_var), true) => format!("set (from ${env_var})"),
                        (Some(env_var), false) => format!("NOT SET (expected ${env_var})"),
                        (None, _) => "none configured".to_string(),
                    };
                    eprintln!(
                        "{} {}",
                        self.palette.dim("API key: "),
                        if progress.api_key_set {
                            key_status
                        } else {
                            self.palette.red(&key_status).clone()
                        }
                    );
                }
                if progress.pending == 0 {
                    eprintln!(
                        "{} for {}:{} {}",
                        self.palette.cyan("Vector index is up to date"),
                        progress.provider_name,
                        progress.model_name,
                        self.palette.dim(&format!(
                            "(batch size {}, concurrency {}, {} skipped)",
                            progress.batch_size, progress.max_concurrency, progress.skipped
                        ))
                    );
                } else {
                    eprintln!(
                        "{} {} vector chunks with {}:{} {}",
                        self.palette.cyan("Indexing"),
                        self.palette.bold(&progress.pending.to_string()),
                        progress.provider_name,
                        progress.model_name,
                        self.palette.dim(&format!(
                            "(batch size {}, concurrency {}, {} batches)",
                            progress.batch_size, progress.max_concurrency, progress.total_batches
                        ))
                    );
                }
            }
            VectorIndexPhase::Embedding => {
                if progress.batches_completed > self.last_batches_completed {
                    let elapsed = self.started_at.elapsed();
                    let rate =
                        count_as_f64(progress.processed) / elapsed.as_secs_f64().max(f64::EPSILON);
                    let remaining = progress.pending.saturating_sub(progress.processed);
                    eprintln!(
                        "{} {}/{}: {} indexed, {} failed, {} remaining | {} | {}",
                        self.palette.cyan("Completed batch"),
                        self.palette.bold(&progress.batches_completed.to_string()),
                        progress.total_batches.max(1),
                        self.palette.green(&progress.indexed.to_string()),
                        self.palette.red(&progress.failed.to_string()),
                        remaining,
                        self.palette.dim(&format!("{rate:.1} chunks/s")),
                        self.palette
                            .dim(&format!("ETA {}", format_eta(remaining, rate)))
                    );
                    self.last_batches_completed = progress.batches_completed;
                }
                if !progress.batch_failures.is_empty() {
                    let deduped = dedup_failure_messages(&progress.batch_failures);
                    if self.verbose {
                        for (message, count) in &deduped {
                            eprintln!(
                                "  {} {} {}",
                                self.palette.red("FAIL"),
                                message,
                                self.palette.dim(&format!("({count} chunks)"))
                            );
                        }
                    } else if progress.batches_completed == 1 {
                        let (message, count) = &deduped[0];
                        eprintln!(
                            "  {} {} {}",
                            self.palette.red("FAIL"),
                            message,
                            self.palette.dim(&format!(
                                "({count} chunks, use --verbose to see all failures)"
                            ))
                        );
                    }
                }
            }
            VectorIndexPhase::Completed => {
                if progress.dry_run {
                    eprintln!(
                        "{} {} vector chunks across {} batches",
                        self.palette.cyan("Dry run planned"),
                        self.palette.bold(&progress.pending.to_string()),
                        progress.total_batches
                    );
                } else if !self.prepared {
                    eprintln!(
                        "{} {} indexed, {} failed, {} skipped {}",
                        self.palette.cyan("Vector index complete"),
                        self.palette.green(&progress.indexed.to_string()),
                        self.palette.red(&progress.failed.to_string()),
                        progress.skipped,
                        self.palette.dim(&format!(
                            "in {:.3}s",
                            self.started_at.elapsed().as_secs_f64()
                        ))
                    );
                }
            }
        }
    }
}

fn dedup_failure_messages(failures: &[(String, String, String)]) -> Vec<(&str, usize)> {
    let mut seen = Vec::new();
    for (_, _, message) in failures {
        if let Some((_, count)) = seen
            .iter_mut()
            .find(|(m, _): &&mut (&str, usize)| *m == message)
        {
            *count += 1;
        } else {
            seen.push((message.as_str(), 1));
        }
    }
    seen
}

fn color_enabled_for_terminal(is_tty: bool) -> bool {
    is_tty
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map_or(true, |value| value != "dumb")
}

fn format_eta(remaining_units: usize, rate_per_second: f64) -> String {
    if remaining_units == 0 || rate_per_second <= f64::EPSILON {
        return "0s".to_string();
    }

    format_duration(Duration::from_secs_f64(
        count_as_f64(remaining_units) / rate_per_second,
    ))
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 1.0 {
        "<1s".to_string()
    } else if seconds < 60.0 {
        format!("{seconds:.1}s")
    } else if seconds < 3_600.0 {
        let minutes = (seconds / 60.0).floor();
        let remaining = seconds - (minutes * 60.0);
        format!("{minutes:.0}m {remaining:.0}s")
    } else {
        let hours = (seconds / 3_600.0).floor();
        let minutes = ((seconds - (hours * 3_600.0)) / 60.0).floor();
        format!("{hours:.0}h {minutes:.0}m")
    }
}

fn count_as_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedExport {
    format: SavedExportFormat,
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BatchRunItemReport {
    name: String,
    kind: Option<SavedReportKind>,
    ok: bool,
    row_count: Option<usize>,
    export_format: Option<SavedExportFormat>,
    export_path: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BatchRunReport {
    total: usize,
    succeeded: usize,
    failed: usize,
    items: Vec<BatchRunItemReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AutomationRunReport {
    actions: Vec<String>,
    reports: Option<BatchRunReport>,
    scan: Option<ScanSummary>,
    doctor_issues: Option<vulcan_core::DoctorSummary>,
    doctor_fix: Option<DoctorFixReport>,
    cache_verify: Option<CacheVerifyReport>,
    repair_fts: Option<RepairFtsReport>,
    issues_detected: bool,
}

#[allow(clippy::large_enum_variant)]
enum SavedExecution {
    Search(SearchReport),
    Notes(NotesReport),
    Bases(BasesEvalReport),
}

impl SavedExecution {
    fn kind(&self) -> SavedReportKind {
        match self {
            Self::Search(_) => SavedReportKind::Search,
            Self::Notes(_) => SavedReportKind::Notes,
            Self::Bases(_) => SavedReportKind::Bases,
        }
    }
}

fn stored_export_from_args(export: &ExportArgs) -> Result<Option<SavedExport>, CliError> {
    match (export.export, export.export_path.as_ref()) {
        (Some(format), Some(path)) => Ok(Some(SavedExport {
            format: saved_export_format(format),
            path: path_to_string(path)?,
        })),
        (None, None) => Ok(None),
        _ => Err(CliError::operation(
            "export flags require both --export and --export-path",
        )),
    }
}

fn resolve_cli_export(export: &ExportArgs) -> Result<Option<ResolvedExport>, CliError> {
    match (export.export, export.export_path.as_ref()) {
        (Some(format), Some(path)) => Ok(Some(ResolvedExport {
            format: saved_export_format(format),
            path: resolve_relative_output_path(
                path,
                &std::env::current_dir().map_err(|error| CliError::io(&error))?,
            ),
        })),
        (None, None) => Ok(None),
        _ => Err(CliError::operation(
            "export flags require both --export and --export-path",
        )),
    }
}

fn resolve_saved_export(paths: &VaultPaths, export: &SavedExport) -> ResolvedExport {
    ResolvedExport {
        format: export.format,
        path: resolve_relative_output_path(Path::new(&export.path), paths.vault_root()),
    }
}

fn resolve_runtime_export(
    paths: &VaultPaths,
    definition: &SavedReportDefinition,
    override_export: &ExportArgs,
) -> Result<Option<ResolvedExport>, CliError> {
    if let Some(export) = resolve_cli_export(override_export)? {
        return Ok(Some(export));
    }

    definition
        .export
        .as_ref()
        .map(|export| Ok(resolve_saved_export(paths, export)))
        .transpose()
}

fn resolve_relative_output_path(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn path_to_string(path: &Path) -> Result<String, CliError> {
    path.to_str()
        .map(ToString::to_string)
        .ok_or_else(|| CliError::operation("export paths must be valid UTF-8"))
}

fn interactive_note_selection_allowed(cli: &Cli, stdout_is_tty: bool) -> bool {
    cli.output == OutputFormat::Human && stdout_is_tty && io::stdin().is_terminal()
}

fn resolve_note_argument(
    paths: &VaultPaths,
    identifier: Option<&str>,
    interactive: bool,
    prompt: &str,
) -> Result<String, CliError> {
    match identifier {
        Some(identifier) => match resolve_note_reference(paths, identifier) {
            Ok(_) => Ok(identifier.to_string()),
            Err(GraphQueryError::AmbiguousIdentifier { matches, .. }) if interactive => {
                note_picker::pick_note(paths, Some(identifier), Some(&matches))
                    .map_err(CliError::operation)?
                    .ok_or_else(|| CliError::operation(format!("cancelled {prompt} selection")))
            }
            Err(GraphQueryError::NoteNotFound { .. }) if interactive => {
                note_picker::pick_note(paths, Some(identifier), None)
                    .map_err(CliError::operation)?
                    .ok_or_else(|| CliError::operation(format!("cancelled {prompt} selection")))
            }
            Err(error) => Err(CliError::operation(error)),
        },
        None if interactive => note_picker::pick_note(paths, None, None)
            .map_err(CliError::operation)?
            .ok_or_else(|| CliError::operation(format!("cancelled {prompt} selection"))),
        None => Err(CliError::operation(format!(
            "missing {prompt}; provide a note identifier or run interactively"
        ))),
    }
}

fn saved_export_format(format: ExportFormat) -> SavedExportFormat {
    match format {
        ExportFormat::Csv => SavedExportFormat::Csv,
        ExportFormat::Jsonl => SavedExportFormat::Jsonl,
    }
}

fn export_rows(
    rows: &[Value],
    fields: Option<&[String]>,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let Some(export) = export else {
        return Ok(());
    };

    if let Some(parent) = export.path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }

    match export.format {
        SavedExportFormat::Jsonl => {
            let rendered = rows
                .iter()
                .map(|row| {
                    serde_json::to_string(&select_fields(row.clone(), fields))
                        .map_err(CliError::operation)
                })
                .collect::<Result<Vec<_>, _>>()?
                .join("\n");
            let mut payload = rendered;
            if !payload.is_empty() {
                payload.push('\n');
            }
            fs::write(&export.path, payload).map_err(CliError::operation)?;
        }
        SavedExportFormat::Csv => {
            let mut writer = csv::Writer::from_path(&export.path).map_err(CliError::operation)?;
            let headers = csv_headers(rows, fields);
            writer
                .write_record(headers.iter().map(String::as_str))
                .map_err(CliError::operation)?;
            for row in rows {
                let selected = select_fields(row.clone(), fields);
                let record = headers
                    .iter()
                    .map(|header| csv_cell_for_value(selected.get(header)))
                    .collect::<Vec<_>>();
                writer.write_record(record).map_err(CliError::operation)?;
            }
            writer.flush().map_err(CliError::operation)?;
        }
    }

    Ok(())
}

fn csv_headers(rows: &[Value], fields: Option<&[String]>) -> Vec<String> {
    if let Some(fields) = fields {
        return fields.to_vec();
    }

    let mut headers = rows
        .iter()
        .filter_map(Value::as_object)
        .flat_map(|object| object.keys().cloned())
        .collect::<Vec<_>>();
    headers.sort();
    headers.dedup();
    if headers.is_empty() {
        headers.push("value".to_string());
    }
    headers
}

fn csv_cell_for_value(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(other) => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn execute_saved_report(
    paths: &VaultPaths,
    definition: &SavedReportDefinition,
    provider: Option<String>,
    controls: &ListOutputControls,
) -> Result<SavedExecution, CliError> {
    match &definition.query {
        SavedReportQuery::Search {
            query,
            mode,
            tag,
            path_prefix,
            has_property,
            filters,
            context_size,
            raw_query,
            fuzzy,
        } => Ok(SavedExecution::Search(
            search_vault(
                paths,
                &SearchQuery {
                    text: query.clone(),
                    tag: tag.clone(),
                    path_prefix: path_prefix.clone(),
                    has_property: has_property.clone(),
                    filters: filters.clone(),
                    provider,
                    mode: *mode,
                    limit: controls.requested_result_limit(),
                    context_size: *context_size,
                    raw_query: *raw_query,
                    fuzzy: *fuzzy,
                    explain: false,
                },
            )
            .map_err(CliError::operation)?,
        )),
        SavedReportQuery::Notes {
            filters,
            sort_by,
            sort_descending,
        } => Ok(SavedExecution::Notes(
            query_notes(
                paths,
                &NoteQuery {
                    filters: filters.clone(),
                    sort_by: sort_by.clone(),
                    sort_descending: *sort_descending,
                },
            )
            .map_err(CliError::operation)?,
        )),
        SavedReportQuery::Bases { file } => Ok(SavedExecution::Bases(
            evaluate_base_file(paths, file).map_err(CliError::operation)?,
        )),
    }
}

fn saved_execution_rows(execution: &SavedExecution, controls: &ListOutputControls) -> Vec<Value> {
    match execution {
        SavedExecution::Search(report) => {
            search_hit_rows(report, paginated_items(&report.hits, controls))
        }
        SavedExecution::Notes(report) => {
            note_rows(report, paginated_items(&report.notes, controls))
        }
        SavedExecution::Bases(report) => {
            let rows = bases_rows(report);
            let start = controls.offset.min(rows.len());
            let end = controls.limit.map_or(rows.len(), |limit| {
                start.saturating_add(limit).min(rows.len())
            });
            rows[start..end].to_vec()
        }
    }
}

fn run_saved_reports_batch(
    paths: &VaultPaths,
    provider: Option<&String>,
    controls: &ListOutputControls,
    names: &[String],
    all: bool,
) -> Result<BatchRunReport, CliError> {
    if all && !names.is_empty() {
        return Err(CliError::operation(
            "batch accepts either explicit report names or --all, not both",
        ));
    }

    let selected_names = if all {
        list_saved_reports(paths)
            .map_err(CliError::operation)?
            .into_iter()
            .map(|report| report.name)
            .collect::<Vec<_>>()
    } else {
        names.to_vec()
    };

    if selected_names.is_empty() {
        return Err(CliError::operation(
            "no saved reports selected; pass names or use --all",
        ));
    }

    let mut items = Vec::new();
    let mut succeeded = 0_usize;
    for name in selected_names {
        match load_saved_report(paths, &name).map_err(CliError::operation) {
            Ok(definition) => {
                let effective_controls =
                    controls.with_saved_defaults(definition.fields.clone(), definition.limit);
                let result = match definition.export.as_ref() {
                    Some(saved_export) => {
                        let resolved_export = resolve_saved_export(paths, saved_export);
                        execute_saved_report(
                            paths,
                            &definition,
                            provider.cloned(),
                            &effective_controls,
                        )
                        .and_then(|execution| {
                            let rows = saved_execution_rows(&execution, &effective_controls);
                            export_rows(
                                &rows,
                                effective_controls.fields.as_deref(),
                                Some(&resolved_export),
                            )?;
                            Ok(BatchRunItemReport {
                                name: definition.name.clone(),
                                kind: Some(execution.kind()),
                                ok: true,
                                row_count: Some(rows.len()),
                                export_format: Some(resolved_export.format),
                                export_path: Some(resolved_export.path.display().to_string()),
                                error: None,
                            })
                        })
                    }
                    None => Err(CliError::operation(
                        "batch mode requires each saved report to define an export target",
                    )),
                };

                match result {
                    Ok(item) => {
                        succeeded += 1;
                        items.push(item);
                    }
                    Err(error) => {
                        items.push(BatchRunItemReport {
                            name: definition.name,
                            kind: Some(definition.query.kind()),
                            ok: false,
                            row_count: None,
                            export_format: None,
                            export_path: None,
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
            Err(error) => {
                items.push(BatchRunItemReport {
                    name,
                    kind: None,
                    ok: false,
                    row_count: None,
                    export_format: None,
                    export_path: None,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(BatchRunReport {
        total: items.len(),
        succeeded,
        failed: items.len().saturating_sub(succeeded),
        items,
    })
}

fn doctor_summary_has_issues(summary: &vulcan_core::DoctorSummary) -> bool {
    summary.unresolved_links > 0
        || summary.ambiguous_links > 0
        || summary.broken_embeds > 0
        || summary.parse_failures > 0
        || summary.stale_index_rows > 0
        || summary.missing_index_rows > 0
        || summary.orphan_notes > 0
        || summary.orphan_assets > 0
        || summary.html_links > 0
}

fn execute_automation_run(
    paths: &VaultPaths,
    provider: Option<&String>,
    output: OutputFormat,
    use_stderr_color: bool,
    controls: &ListOutputControls,
    command: &AutomationCommand,
) -> Result<AutomationRunReport, CliError> {
    let AutomationCommand::Run {
        reports,
        all_reports,
        scan,
        doctor,
        doctor_fix: doctor_fix_requested,
        verify_cache: verify_cache_requested,
        repair_fts: repair_fts_requested,
        fail_on_issues: _,
    } = command;

    if !*scan
        && !*doctor
        && !*doctor_fix_requested
        && !*verify_cache_requested
        && !*repair_fts_requested
        && !*all_reports
        && reports.is_empty()
    {
        return Err(CliError::operation(
            "automation run requires at least one action",
        ));
    }

    let mut actions = Vec::new();
    let mut scan_report = None;
    if *scan {
        actions.push("scan".to_string());
        let mut progress =
            (output == OutputFormat::Human).then(|| ScanProgressReporter::new(use_stderr_color));
        scan_report = Some(
            scan_vault_with_progress(paths, ScanMode::Incremental, |event| {
                if let Some(progress) = progress.as_mut() {
                    progress.record(&event);
                }
            })
            .map_err(CliError::operation)?,
        );
    }

    let mut doctor_issues = None;
    let mut doctor_fix_report = None;
    if *doctor {
        actions.push("doctor".to_string());
        doctor_issues = Some(doctor_vault(paths).map_err(CliError::operation)?.summary);
    } else if *doctor_fix_requested {
        actions.push("doctor_fix".to_string());
        doctor_fix_report = Some(doctor_fix(paths, false).map_err(CliError::operation)?);
    }

    let mut cache_verify_report = None;
    if *verify_cache_requested {
        actions.push("cache_verify".to_string());
        cache_verify_report = Some(verify_cache(paths).map_err(CliError::operation)?);
    }

    let mut repair_fts_report = None;
    if *repair_fts_requested {
        actions.push("repair_fts".to_string());
        repair_fts_report = Some(
            repair_fts(paths, &RepairFtsQuery { dry_run: false }).map_err(CliError::operation)?,
        );
    }

    let batch_report = if *all_reports || !reports.is_empty() {
        actions.push(if *all_reports {
            "saved_reports_all".to_string()
        } else {
            "saved_reports".to_string()
        });
        Some(run_saved_reports_batch(
            paths,
            provider,
            controls,
            reports,
            *all_reports,
        )?)
    } else {
        None
    };

    let issues_detected = doctor_issues
        .as_ref()
        .is_some_and(doctor_summary_has_issues)
        || doctor_fix_report
            .as_ref()
            .and_then(|report| report.issues_after.as_ref())
            .is_some_and(doctor_summary_has_issues)
        || cache_verify_report
            .as_ref()
            .is_some_and(|report| !report.healthy);

    Ok(AutomationRunReport {
        actions,
        reports: batch_report,
        scan: scan_report,
        doctor_issues,
        doctor_fix: doctor_fix_report,
        cache_verify: cache_verify_report,
        repair_fts: repair_fts_report,
        issues_detected,
    })
}

pub fn run() -> Result<(), CliError> {
    run_from(std::env::args_os())
}

pub fn run_from<I, T>(args: I) -> Result<(), CliError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    dispatch(&cli)
}

#[allow(clippy::too_many_lines)]
fn dispatch(cli: &Cli) -> Result<(), CliError> {
    let paths = VaultPaths::new(resolve_vault_root(&cli.vault)?);
    let list_controls = ListOutputControls::from_cli(cli);
    let stdout_is_tty = io::stdout().is_terminal();
    let stderr_is_tty = io::stderr().is_terminal();
    let use_stdout_color = color_enabled_for_terminal(stdout_is_tty);
    let use_stderr_color = color_enabled_for_terminal(stderr_is_tty);
    let interactive_note_selection = interactive_note_selection_allowed(cli, stdout_is_tty);

    match cli.command {
        Command::Backlinks {
            ref note,
            ref export,
        } => {
            let note =
                resolve_note_argument(&paths, note.as_deref(), interactive_note_selection, "note")?;
            let report = query_backlinks(&paths, &note).map_err(CliError::operation)?;
            let export = resolve_cli_export(export)?;
            print_backlinks_report(
                cli.output,
                &report,
                &list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )?;
            Ok(())
        }
        Command::Graph { ref command } => match command {
            GraphCommand::Path { from, to } => {
                let from = resolve_note_argument(
                    &paths,
                    from.as_deref(),
                    interactive_note_selection,
                    "from note",
                )?;
                let to = resolve_note_argument(
                    &paths,
                    to.as_deref(),
                    interactive_note_selection,
                    "to note",
                )?;
                let report = query_graph_path(&paths, &from, &to).map_err(CliError::operation)?;
                print_graph_path_report(cli.output, &report)
            }
            GraphCommand::Hubs { export } => {
                let report = query_graph_hubs(&paths).map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_graph_hubs_report(
                    cli.output,
                    &report,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )
            }
            GraphCommand::Moc { export } => {
                let report = query_graph_moc_candidates(&paths).map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_graph_moc_report(
                    cli.output,
                    &report,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )
            }
            GraphCommand::DeadEnds { export } => {
                let report = query_graph_dead_ends(&paths).map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_graph_dead_ends_report(
                    cli.output,
                    &report,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )
            }
            GraphCommand::Components { export } => {
                let report = query_graph_components(&paths).map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_graph_components_report(
                    cli.output,
                    &report,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )
            }
            GraphCommand::Stats { export } => {
                let report = query_graph_analytics(&paths).map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_graph_analytics_report(cli.output, &report, export.as_ref())
            }
            GraphCommand::Trends { limit, export } => {
                let report = query_graph_trends(&paths, *limit).map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_graph_trends_report(cli.output, &report, &list_controls, export.as_ref())
            }
        },
        Command::Completions { shell } => {
            let mut command = Cli::command();
            generate(shell, &mut command, "vulcan", &mut io::stdout());
            Ok(())
        }
        Command::Bases { ref command } => match command {
            BasesCommand::Eval { file, export } => {
                let report = evaluate_base_file(&paths, file).map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_bases_report(
                    cli.output,
                    &report,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )?;
                Ok(())
            }
            BasesCommand::Tui { file } => {
                let report = evaluate_base_file(&paths, file).map_err(CliError::operation)?;
                if cli.output == OutputFormat::Human && stdout_is_tty && io::stdin().is_terminal() {
                    bases_tui::run_bases_tui(&paths, file, &report).map_err(CliError::operation)
                } else {
                    print_bases_report(
                        cli.output,
                        &report,
                        &list_controls,
                        stdout_is_tty,
                        use_stdout_color,
                        None,
                    )
                }
            }
            BasesCommand::ViewAdd {
                ref file,
                ref name,
                ref filters,
                ref column,
                ref sort,
                sort_desc,
                ref group_by,
                group_desc,
                dry_run,
            } => {
                let spec = BaseViewSpec {
                    name: Some(name.clone()),
                    view_type: "table".to_string(),
                    filters: filters.clone(),
                    sort_by: sort.clone(),
                    sort_descending: *sort_desc,
                    columns: column.clone(),
                    group_by: group_by.as_deref().map(|p| BaseViewGroupBy {
                        property: p.to_string(),
                        descending: *group_desc,
                    }),
                };
                let report =
                    bases_view_add(&paths, file, spec, *dry_run).map_err(CliError::operation)?;
                print_bases_view_edit_report(cli.output, &report)
            }
            BasesCommand::ViewDelete {
                ref file,
                ref name,
                dry_run,
            } => {
                let report =
                    bases_view_delete(&paths, file, name, *dry_run).map_err(CliError::operation)?;
                print_bases_view_edit_report(cli.output, &report)
            }
            BasesCommand::ViewRename {
                ref file,
                ref old_name,
                ref new_name,
                dry_run,
            } => {
                let report = bases_view_rename(&paths, file, old_name, new_name, *dry_run)
                    .map_err(CliError::operation)?;
                print_bases_view_edit_report(cli.output, &report)
            }
            BasesCommand::ViewEdit {
                ref file,
                ref name,
                ref add_filters,
                ref remove_filters,
                ref column,
                ref sort,
                sort_desc,
                ref group_by,
                group_desc,
                dry_run,
            } => {
                let patch = BaseViewPatch {
                    add_filters: add_filters.clone(),
                    remove_filters: remove_filters.clone(),
                    set_columns: if column.is_empty() {
                        None
                    } else {
                        Some(column.clone())
                    },
                    set_sort: sort.as_deref().map(|s| {
                        if s.is_empty() {
                            None
                        } else {
                            Some(s.to_string())
                        }
                    }),
                    set_sort_descending: if sort.is_some() {
                        Some(*sort_desc)
                    } else {
                        None
                    },
                    set_group_by: group_by.as_deref().map(|p| {
                        if p.is_empty() {
                            None
                        } else {
                            Some(BaseViewGroupBy {
                                property: p.to_string(),
                                descending: *group_desc,
                            })
                        }
                    }),
                    ..Default::default()
                };
                let report = bases_view_edit(&paths, file, name, patch, *dry_run)
                    .map_err(CliError::operation)?;
                print_bases_view_edit_report(cli.output, &report)
            }
        },
        Command::Cluster {
            clusters,
            dry_run,
            ref export,
        } => {
            let report = cluster_vectors(
                &paths,
                &ClusterQuery {
                    provider: cli.provider.clone(),
                    clusters,
                    dry_run,
                },
            )
            .map_err(CliError::operation)?;
            let export = resolve_cli_export(export)?;
            print_cluster_report(
                cli.output,
                &report,
                &list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )?;
            Ok(())
        }
        Command::Related {
            ref note,
            ref export,
        } => {
            let note =
                resolve_note_argument(&paths, note.as_deref(), interactive_note_selection, "note")?;
            let report = query_related_notes(
                &paths,
                &RelatedNotesQuery {
                    provider: cli.provider.clone(),
                    note,
                    limit: cli.limit.unwrap_or(10).saturating_add(cli.offset),
                },
            )
            .map_err(CliError::operation)?;
            let export = resolve_cli_export(export)?;
            print_related_notes_report(
                cli.output,
                &report,
                &list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )?;
            Ok(())
        }
        Command::Describe => print_describe_report(cli.output),
        Command::Doctor {
            fix,
            dry_run,
            fail_on_issues,
        } => {
            if fix {
                let report = doctor_fix(&paths, dry_run).map_err(CliError::operation)?;
                print_doctor_fix_report(cli.output, &paths, &report)?;
                if fail_on_issues {
                    let summary = report
                        .issues_after
                        .as_ref()
                        .unwrap_or(&report.issues_before);
                    if doctor_summary_has_issues(summary) {
                        return Err(CliError::issues("doctor found remaining issues"));
                    }
                }
            } else {
                let report = doctor_vault(&paths).map_err(CliError::operation)?;
                print_doctor_report(cli.output, &paths, &report)?;
                if fail_on_issues && doctor_summary_has_issues(&report.summary) {
                    return Err(CliError::issues("doctor found issues"));
                }
            }
            Ok(())
        }
        Command::Init => {
            let summary = initialize_vault(&paths).map_err(CliError::operation)?;
            print_init_summary(cli.output, &summary)?;
            Ok(())
        }
        Command::Rebuild { dry_run } => {
            let mut progress = (cli.output == OutputFormat::Human)
                .then(|| ScanProgressReporter::new(use_stderr_color));
            let report = rebuild_vault_with_progress(&paths, &RebuildQuery { dry_run }, |event| {
                if let Some(progress) = progress.as_mut() {
                    progress.record(&event);
                }
            })
            .map_err(CliError::operation)?;
            print_rebuild_report(cli.output, &report, use_stdout_color)
        }
        Command::Move {
            ref source,
            ref dest,
            dry_run,
        } => {
            let summary = move_note(&paths, source, dest, dry_run).map_err(CliError::operation)?;
            print_move_summary(cli.output, &summary)?;
            Ok(())
        }
        Command::RenameProperty {
            ref old,
            ref new,
            dry_run,
        } => {
            let report = rename_property(&paths, old, new, dry_run).map_err(CliError::operation)?;
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::MergeTags {
            ref source,
            ref dest,
            dry_run,
        } => {
            let report = merge_tags(&paths, source, dest, dry_run).map_err(CliError::operation)?;
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::RenameAlias {
            ref note,
            ref old,
            ref new,
            dry_run,
        } => {
            let note = resolve_note_argument(
                &paths,
                Some(note.as_str()),
                interactive_note_selection,
                "note to update",
            )?;
            let report =
                rename_alias(&paths, &note, old, new, dry_run).map_err(CliError::operation)?;
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::RenameHeading {
            ref note,
            ref old,
            ref new,
            dry_run,
        } => {
            let note = resolve_note_argument(
                &paths,
                Some(note.as_str()),
                interactive_note_selection,
                "note containing heading",
            )?;
            let report =
                rename_heading(&paths, &note, old, new, dry_run).map_err(CliError::operation)?;
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::RenameBlockRef {
            ref note,
            ref old,
            ref new,
            dry_run,
        } => {
            let note = resolve_note_argument(
                &paths,
                Some(note.as_str()),
                interactive_note_selection,
                "note containing block ref",
            )?;
            let report =
                rename_block_ref(&paths, &note, old, new, dry_run).map_err(CliError::operation)?;
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::Cache { ref command } => match command {
            CacheCommand::Inspect => {
                let report = inspect_cache(&paths).map_err(CliError::operation)?;
                print_cache_inspect_report(cli.output, &report)
            }
            CacheCommand::Verify { fail_on_errors } => {
                let report = verify_cache(&paths).map_err(CliError::operation)?;
                print_cache_verify_report(cli.output, &report)?;
                if *fail_on_errors && !report.healthy {
                    Err(CliError::issues("cache verification failed"))
                } else {
                    Ok(())
                }
            }
            CacheCommand::Vacuum { dry_run } => {
                let report = cache_vacuum(&paths, &CacheVacuumQuery { dry_run: *dry_run })
                    .map_err(CliError::operation)?;
                print_cache_vacuum_report(cli.output, &report)
            }
        },
        Command::Repair { ref command } => match command {
            RepairCommand::Fts { dry_run } => {
                let report = repair_fts(&paths, &RepairFtsQuery { dry_run: *dry_run })
                    .map_err(CliError::operation)?;
                print_repair_fts_report(cli.output, &report)
            }
        },
        Command::Serve {
            ref bind,
            no_watch,
            debounce_ms,
            ref auth_token,
        } => serve_forever(
            &paths,
            &ServeOptions {
                bind: bind.clone(),
                watch: !no_watch,
                debounce_ms,
                auth_token: auth_token.clone(),
            },
        ),
        Command::Watch { debounce_ms } => {
            if cli.output == OutputFormat::Human && stdout_is_tty {
                println!(
                    "Watching {} (debounce {}ms)",
                    paths.vault_root().display(),
                    debounce_ms
                );
            }
            watch_vault(&paths, &WatchOptions { debounce_ms }, |report| {
                print_watch_report(cli.output, &report)
            })
            .map_err(CliError::operation)
        }
        Command::Links {
            ref note,
            ref export,
        } => {
            let note =
                resolve_note_argument(&paths, note.as_deref(), interactive_note_selection, "note")?;
            let report = query_links(&paths, &note).map_err(CliError::operation)?;
            let export = resolve_cli_export(export)?;
            print_links_report(
                cli.output,
                &report,
                &list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )?;
            Ok(())
        }
        Command::Query {
            ref dsl,
            ref json,
            explain,
            ref export,
        } => {
            let ast = match (dsl.as_deref(), json.as_deref()) {
                (Some(_), Some(_)) => {
                    return Err(CliError::operation(
                        "provide either a DSL argument or --json, not both",
                    ))
                }
                (Some(dsl), None) => QueryAst::from_dsl(dsl).map_err(CliError::operation)?,
                (None, Some(json)) => QueryAst::from_json(json).map_err(CliError::operation)?,
                (None, None) => {
                    return Err(CliError::operation(
                        "provide a DSL query argument or --json payload",
                    ))
                }
            };
            let report = execute_query_report(&paths, ast).map_err(CliError::operation)?;
            // Merge DSL-embedded limit/offset with global list controls; global flags win.
            let effective_controls = ListOutputControls {
                limit: list_controls.limit.or(report.query.limit),
                offset: if list_controls.offset > 0 {
                    list_controls.offset
                } else {
                    report.query.offset
                },
                fields: list_controls.fields.clone(),
            };
            let export = resolve_cli_export(export)?;
            print_query_report(
                cli.output,
                &report,
                explain,
                &effective_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )?;
            Ok(())
        }
        Command::Update {
            ref filters,
            ref key,
            ref value,
            dry_run,
        } => {
            let report = bulk_set_property(&paths, filters, key, Some(value.as_str()), dry_run)
                .map_err(CliError::operation)?;
            print_bulk_mutation_report(cli.output, &report)
        }
        Command::Unset {
            ref filters,
            ref key,
            dry_run,
        } => {
            let report = bulk_set_property(&paths, filters, key, None, dry_run)
                .map_err(CliError::operation)?;
            print_bulk_mutation_report(cli.output, &report)
        }
        Command::Notes {
            ref filters,
            ref sort,
            desc,
            ref export,
        } => {
            let report = query_notes(
                &paths,
                &NoteQuery {
                    filters: filters.clone(),
                    sort_by: sort.clone(),
                    sort_descending: desc,
                },
            )
            .map_err(CliError::operation)?;
            let export = resolve_cli_export(export)?;
            print_notes_report(
                cli.output,
                &report,
                &list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )?;
            Ok(())
        }
        Command::Search {
            ref query,
            ref filters,
            mode,
            ref tag,
            ref path_prefix,
            ref has_property,
            context_size,
            raw_query,
            fuzzy,
            explain,
            ref export,
        } => {
            let report = search_vault(
                &paths,
                &SearchQuery {
                    text: query.clone(),
                    tag: tag.clone(),
                    path_prefix: path_prefix.clone(),
                    has_property: has_property.clone(),
                    filters: filters.clone(),
                    provider: cli.provider.clone(),
                    mode: match mode {
                        SearchMode::Keyword => vulcan_core::search::SearchMode::Keyword,
                        SearchMode::Hybrid => vulcan_core::search::SearchMode::Hybrid,
                    },
                    limit: cli.limit.map(|limit| limit.saturating_add(cli.offset)),
                    context_size,
                    raw_query,
                    fuzzy,
                    explain,
                },
            )
            .map_err(CliError::operation)?;
            let export = resolve_cli_export(export)?;
            print_search_report(
                cli.output,
                &report,
                &list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )?;
            Ok(())
        }
        Command::Suggest { ref command } => match command {
            SuggestCommand::Mentions { note, export } => {
                let note = if note.is_none() && interactive_note_selection {
                    note_picker::pick_note(&paths, None, None).map_err(CliError::operation)?
                } else {
                    note.clone()
                };
                let report =
                    suggest_mentions(&paths, note.as_deref()).map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_mention_suggestions_report(
                    cli.output,
                    &report,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )
            }
            SuggestCommand::Duplicates { export } => {
                let report = suggest_duplicates(&paths).map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_duplicate_suggestions_report(
                    cli.output,
                    &report,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )
            }
        },
        Command::Saved { ref command } => match command {
            SavedCommand::List => {
                let reports = list_saved_reports(&paths).map_err(CliError::operation)?;
                print_saved_report_list(
                    cli.output,
                    &reports,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                )
            }
            SavedCommand::Show { name } => {
                let definition = load_saved_report(&paths, name).map_err(CliError::operation)?;
                print_saved_report_definition(cli.output, &definition)
            }
            SavedCommand::Search {
                name,
                query,
                filters,
                mode,
                tag,
                path_prefix,
                has_property,
                context_size,
                raw_query,
                fuzzy,
                description,
                export,
            } => {
                let definition = SavedReportDefinition {
                    name: name.clone(),
                    description: description.clone(),
                    fields: cli.fields.clone(),
                    limit: cli.limit,
                    export: stored_export_from_args(export)?,
                    query: SavedReportQuery::Search {
                        query: query.clone(),
                        mode: match mode {
                            SearchMode::Keyword => vulcan_core::search::SearchMode::Keyword,
                            SearchMode::Hybrid => vulcan_core::search::SearchMode::Hybrid,
                        },
                        tag: tag.clone(),
                        path_prefix: path_prefix.clone(),
                        has_property: has_property.clone(),
                        filters: filters.clone(),
                        context_size: *context_size,
                        raw_query: *raw_query,
                        fuzzy: *fuzzy,
                    },
                };
                save_saved_report(&paths, &definition).map_err(CliError::operation)?;
                print_saved_report_definition(cli.output, &definition)
            }
            SavedCommand::Notes {
                name,
                filters,
                sort,
                desc,
                description,
                export,
            } => {
                let definition = SavedReportDefinition {
                    name: name.clone(),
                    description: description.clone(),
                    fields: cli.fields.clone(),
                    limit: cli.limit,
                    export: stored_export_from_args(export)?,
                    query: SavedReportQuery::Notes {
                        filters: filters.clone(),
                        sort_by: sort.clone(),
                        sort_descending: *desc,
                    },
                };
                save_saved_report(&paths, &definition).map_err(CliError::operation)?;
                print_saved_report_definition(cli.output, &definition)
            }
            SavedCommand::Bases {
                name,
                file,
                description,
                export,
            } => {
                let definition = SavedReportDefinition {
                    name: name.clone(),
                    description: description.clone(),
                    fields: cli.fields.clone(),
                    limit: cli.limit,
                    export: stored_export_from_args(export)?,
                    query: SavedReportQuery::Bases { file: file.clone() },
                };
                save_saved_report(&paths, &definition).map_err(CliError::operation)?;
                print_saved_report_definition(cli.output, &definition)
            }
            SavedCommand::Run { name, export } => {
                let definition = load_saved_report(&paths, name).map_err(CliError::operation)?;
                let effective_controls =
                    list_controls.with_saved_defaults(definition.fields.clone(), definition.limit);
                let resolved_export = resolve_runtime_export(&paths, &definition, export)?;
                let execution = execute_saved_report(
                    &paths,
                    &definition,
                    cli.provider.clone(),
                    &effective_controls,
                )?;
                match execution {
                    SavedExecution::Search(report) => print_search_report(
                        cli.output,
                        &report,
                        &effective_controls,
                        stdout_is_tty,
                        use_stdout_color,
                        resolved_export.as_ref(),
                    ),
                    SavedExecution::Notes(report) => print_notes_report(
                        cli.output,
                        &report,
                        &effective_controls,
                        stdout_is_tty,
                        use_stdout_color,
                        resolved_export.as_ref(),
                    ),
                    SavedExecution::Bases(report) => print_bases_report(
                        cli.output,
                        &report,
                        &effective_controls,
                        stdout_is_tty,
                        use_stdout_color,
                        resolved_export.as_ref(),
                    ),
                }
            }
        },
        Command::Checkpoint { ref command } => match command {
            CheckpointCommand::Create { name } => {
                let record = create_checkpoint(&paths, name).map_err(CliError::operation)?;
                print_checkpoint_record(cli.output, &record)
            }
            CheckpointCommand::List { export } => {
                let records = list_checkpoints(&paths).map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_checkpoint_list(
                    cli.output,
                    &records,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )
            }
        },
        Command::Export { ref command } => match command {
            ExportCommand::SearchIndex { path, pretty } => {
                let report = export_static_search_index(&paths).map_err(CliError::operation)?;
                print_static_search_index_report(cli.output, &report, path.as_ref(), *pretty)
            }
        },
        Command::Changes {
            ref checkpoint,
            ref export,
        } => {
            let report = query_change_report(
                &paths,
                &checkpoint.as_ref().map_or(ChangeAnchor::LastScan, |name| {
                    ChangeAnchor::Checkpoint(name.clone())
                }),
            )
            .map_err(CliError::operation)?;
            let export = resolve_cli_export(export)?;
            print_change_report(
                cli.output,
                &report,
                &list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
        Command::LinkMentions { ref note, dry_run } => {
            let report =
                link_mentions(&paths, note.as_deref(), dry_run).map_err(CliError::operation)?;
            print_refactor_report(cli.output, &report)
        }
        Command::Rewrite {
            ref filters,
            ref find,
            ref replace,
            dry_run,
        } => {
            let report = bulk_replace(&paths, filters, find, replace, dry_run)
                .map_err(CliError::operation)?;
            print_refactor_report(cli.output, &report)
        }
        Command::Batch { ref names, all } => {
            let report =
                run_saved_reports_batch(&paths, cli.provider.as_ref(), &list_controls, names, all)?;
            let has_failures = report.failed > 0;
            print_batch_run_report(cli.output, &report)?;
            if has_failures {
                Err(CliError {
                    exit_code: 1,
                    message: "one or more saved reports failed".to_string(),
                })
            } else {
                Ok(())
            }
        }
        Command::Automation { ref command } => {
            let report = execute_automation_run(
                &paths,
                cli.provider.as_ref(),
                cli.output,
                use_stderr_color,
                &list_controls,
                command,
            )?;
            let fail_on_issues = match command {
                AutomationCommand::Run { fail_on_issues, .. } => *fail_on_issues,
            };
            let report_failures = report
                .reports
                .as_ref()
                .is_some_and(|batch| batch.failed > 0);
            print_automation_run_report(cli.output, &report)?;
            if report_failures {
                Err(CliError::operation(
                    "one or more automation report actions failed",
                ))
            } else if fail_on_issues && report.issues_detected {
                Err(CliError::issues("automation detected issues"))
            } else {
                Ok(())
            }
        }
        Command::Vectors { ref command } => match command {
            VectorsCommand::Index { dry_run } => {
                let verbose = cli.verbose;
                let mut progress = (cli.output == OutputFormat::Human)
                    .then(|| VectorIndexProgressReporter::new(use_stderr_color, verbose));
                let report = index_vectors_with_progress(
                    &paths,
                    &VectorIndexQuery {
                        provider: cli.provider.clone(),
                        dry_run: *dry_run,
                        verbose,
                    },
                    |event| {
                        if let Some(progress) = progress.as_mut() {
                            progress.record(&event);
                        }
                    },
                )
                .map_err(CliError::operation)?;
                print_vector_index_report(cli.output, &report, use_stdout_color)?;
                Ok(())
            }
            VectorsCommand::Repair { dry_run } => {
                let mut progress = (cli.output == OutputFormat::Human)
                    .then(|| VectorIndexProgressReporter::new(use_stderr_color, false));
                let report = repair_vectors_with_progress(
                    &paths,
                    &VectorRepairQuery {
                        provider: cli.provider.clone(),
                        dry_run: *dry_run,
                    },
                    |event| {
                        if let Some(progress) = progress.as_mut() {
                            progress.record(&event);
                        }
                    },
                )
                .map_err(CliError::operation)?;
                print_vector_repair_report(cli.output, &report, use_stdout_color)
            }
            VectorsCommand::Rebuild { dry_run } => {
                let mut progress = (cli.output == OutputFormat::Human)
                    .then(|| VectorIndexProgressReporter::new(use_stderr_color, false));
                let report = rebuild_vectors_with_progress(
                    &paths,
                    &VectorRebuildQuery {
                        provider: cli.provider.clone(),
                        dry_run: *dry_run,
                    },
                    |event| {
                        if let Some(progress) = progress.as_mut() {
                            progress.record(&event);
                        }
                    },
                )
                .map_err(CliError::operation)?;
                print_vector_index_report(cli.output, &report, use_stdout_color)?;
                Ok(())
            }
            VectorsCommand::Queue { ref command } => match command {
                VectorQueueCommand::Status => {
                    let report = inspect_vector_queue(&paths, cli.provider.as_deref())
                        .map_err(CliError::operation)?;
                    print_vector_queue_report(cli.output, &report)
                }
                VectorQueueCommand::Run { dry_run } => {
                    let verbose = cli.verbose;
                    let mut progress = (cli.output == OutputFormat::Human)
                        .then(|| VectorIndexProgressReporter::new(use_stderr_color, verbose));
                    let report = index_vectors_with_progress(
                        &paths,
                        &VectorIndexQuery {
                            provider: cli.provider.clone(),
                            dry_run: *dry_run,
                            verbose,
                        },
                        |event| {
                            if let Some(progress) = progress.as_mut() {
                                progress.record(&event);
                            }
                        },
                    )
                    .map_err(CliError::operation)?;
                    print_vector_index_report(cli.output, &report, use_stdout_color)?;
                    Ok(())
                }
            },
            VectorsCommand::Neighbors {
                query,
                note,
                export,
            } => {
                let resolved_note = if note.is_some() || query.is_none() {
                    Some(resolve_note_argument(
                        &paths,
                        note.as_deref(),
                        interactive_note_selection && query.is_none(),
                        "note",
                    )?)
                } else {
                    None
                };
                let report = query_vector_neighbors(
                    &paths,
                    &VectorNeighborsQuery {
                        provider: cli.provider.clone(),
                        text: query.clone(),
                        note: resolved_note,
                        limit: cli.limit.unwrap_or(10).saturating_add(cli.offset),
                    },
                )
                .map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_vector_neighbors_report(
                    cli.output,
                    &report,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )?;
                Ok(())
            }
            VectorsCommand::Related { note, export } => {
                let note = resolve_note_argument(
                    &paths,
                    note.as_deref(),
                    interactive_note_selection,
                    "note",
                )?;
                let report = query_related_notes(
                    &paths,
                    &RelatedNotesQuery {
                        provider: cli.provider.clone(),
                        note,
                        limit: cli.limit.unwrap_or(10).saturating_add(cli.offset),
                    },
                )
                .map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_related_notes_report(
                    cli.output,
                    &report,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )?;
                Ok(())
            }
            VectorsCommand::Duplicates { threshold, export } => {
                let report = vector_duplicates(
                    &paths,
                    &VectorDuplicatesQuery {
                        provider: cli.provider.clone(),
                        threshold: *threshold,
                        limit: cli.limit.unwrap_or(10).saturating_add(cli.offset),
                    },
                )
                .map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_vector_duplicates_report(
                    cli.output,
                    &report,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )?;
                Ok(())
            }
            VectorsCommand::Models { export } => {
                let models =
                    list_vector_models(&paths).map_err(CliError::operation)?;
                let export = resolve_cli_export(export)?;
                print_vector_models_report(
                    cli.output,
                    &models,
                    stdout_is_tty,
                    use_stdout_color,
                    export.as_ref(),
                )?;
                Ok(())
            }
            VectorsCommand::DropModel { key } => {
                let dropped =
                    drop_vector_model(&paths, key).map_err(CliError::operation)?;
                if dropped {
                    if cli.output == OutputFormat::Json {
                        println!(
                            "{}",
                            serde_json::json!({"dropped": true, "cache_key": key})
                        );
                    } else {
                        eprintln!("Dropped model: {key}");
                    }
                } else if cli.output == OutputFormat::Json {
                    println!(
                        "{}",
                        serde_json::json!({"dropped": false, "cache_key": key})
                    );
                } else {
                    eprintln!("No model found with cache key: {key}");
                }
                Ok(())
            }
        },
        Command::Scan { full } => {
            let mut progress = (cli.output == OutputFormat::Human)
                .then(|| ScanProgressReporter::new(use_stderr_color));
            let summary = scan_vault_with_progress(
                &paths,
                if full {
                    ScanMode::Full
                } else {
                    ScanMode::Incremental
                },
                |event| {
                    if let Some(progress) = progress.as_mut() {
                        progress.record(&event);
                    }
                },
            )
            .map_err(CliError::operation)?;
            print_scan_summary(cli.output, &summary, use_stdout_color);
            Ok(())
        }
    }
}

fn print_search_report(
    output: OutputFormat,
    report: &SearchReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_hits = paginated_items(&report.hits, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = search_hit_rows(report, visible_hits);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!(
                    "{} {} {}",
                    palette.cyan("Search hits for"),
                    palette.bold(&report.query),
                    palette.dim(match report.mode {
                        vulcan_core::search::SearchMode::Keyword => "keyword",
                        vulcan_core::search::SearchMode::Hybrid => "hybrid",
                    }),
                );
            }
            if let Some(plan) = report.plan.as_ref() {
                print_search_plan(plan, palette);
            }
            if visible_hits.is_empty() {
                println!("No search hits.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for (index, hit) in visible_hits.iter().enumerate() {
                    print_search_hit(index, hit, palette);
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_describe_report(output: OutputFormat) -> Result<(), CliError> {
    let report = describe_cli();
    match output {
        OutputFormat::Human => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(CliError::operation)?
            );
            Ok(())
        }
        OutputFormat::Json => print_json(&report),
    }
}

fn print_saved_report_list(
    output: OutputFormat,
    reports: &[SavedReportSummary],
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    let visible_reports = paginated_items(reports, list_controls);
    let rows = saved_report_summary_rows(visible_reports);
    let palette = AnsiPalette::new(use_color);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Saved reports"));
            }
            if visible_reports.is_empty() {
                println!("No saved reports.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for report in visible_reports {
                    let description = report
                        .description
                        .as_deref()
                        .map(|description| format!(": {description}"))
                        .unwrap_or_default();
                    let export = report
                        .export
                        .as_ref()
                        .map(|export| format!(" -> {}", export.path))
                        .unwrap_or_default();
                    println!(
                        "- {} [{:?}]{}{}",
                        report.name, report.kind, description, export
                    );
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
    }
}

fn print_saved_report_definition(
    output: OutputFormat,
    definition: &SavedReportDefinition,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("Saved report: {}", definition.name);
            println!("Kind: {:?}", definition.query.kind());
            if let Some(description) = definition.description.as_deref() {
                println!("Description: {description}");
            }
            if let Some(fields) = definition.fields.as_deref() {
                println!("Fields: {}", fields.join(", "));
            }
            if let Some(limit) = definition.limit {
                println!("Limit: {limit}");
            }
            if let Some(export) = definition.export.as_ref() {
                println!("Export: {:?} -> {}", export.format, export.path);
            }
            match &definition.query {
                SavedReportQuery::Search {
                    query,
                    mode,
                    tag,
                    path_prefix,
                    has_property,
                    filters,
                    context_size,
                    raw_query,
                    fuzzy,
                } => {
                    println!("Query: {query}");
                    println!("Mode: {mode:?}");
                    if let Some(tag) = tag.as_deref() {
                        println!("Tag: {tag}");
                    }
                    if let Some(path_prefix) = path_prefix.as_deref() {
                        println!("Path prefix: {path_prefix}");
                    }
                    if let Some(has_property) = has_property.as_deref() {
                        println!("Has property: {has_property}");
                    }
                    if !filters.is_empty() {
                        println!("Filters: {}", filters.join(" | "));
                    }
                    println!("Context size: {context_size}");
                    if *raw_query {
                        println!("Raw query: true");
                    }
                    if *fuzzy {
                        println!("Fuzzy fallback: true");
                    }
                }
                SavedReportQuery::Notes {
                    filters,
                    sort_by,
                    sort_descending,
                } => {
                    if !filters.is_empty() {
                        println!("Filters: {}", filters.join(" | "));
                    }
                    if let Some(sort_by) = sort_by.as_deref() {
                        println!(
                            "Sort: {}{}",
                            sort_by,
                            if *sort_descending { " desc" } else { "" }
                        );
                    }
                }
                SavedReportQuery::Bases { file } => {
                    println!("Base file: {file}");
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(definition),
    }
}

fn print_batch_run_report(output: OutputFormat, report: &BatchRunReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!(
                "Batch completed: {} succeeded, {} failed",
                report.succeeded, report.failed
            );
            for item in &report.items {
                if item.ok {
                    let export = item
                        .export_path
                        .as_deref()
                        .map(|path| format!(" -> {path}"))
                        .unwrap_or_default();
                    println!(
                        "- {} [{}] {} rows{}",
                        item.name,
                        item.kind
                            .map_or_else(|| "unknown".to_string(), |kind| format!("{kind:?}")),
                        item.row_count.unwrap_or_default(),
                        export
                    );
                } else if let Some(error) = item.error.as_deref() {
                    println!(
                        "- {} [{}] failed: {}",
                        item.name,
                        item.kind
                            .map_or_else(|| "unknown".to_string(), |kind| format!("{kind:?}")),
                        error
                    );
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_notes_report(
    output: OutputFormat,
    report: &NotesReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_notes = paginated_items(&report.notes, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = note_rows(report, visible_notes);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Notes query"));
            }
            if visible_notes.is_empty() {
                println!("No notes matched.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for note in visible_notes {
                    print_note(note);
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn query_report_rows(report: &QueryReport, notes: &[NoteRecord]) -> Vec<Value> {
    let query_value = serde_json::to_value(&report.query).unwrap_or(Value::Null);
    notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "document_path": note.document_path,
                "file_name": note.file_name,
                "file_ext": note.file_ext,
                "file_mtime": note.file_mtime,
                "properties": note.properties,
                "query": query_value,
            })
        })
        .collect()
}

fn print_query_report(
    output: OutputFormat,
    report: &QueryReport,
    explain: bool,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_notes = paginated_items(&report.notes, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = query_report_rows(report, visible_notes);

    match output {
        OutputFormat::Human => {
            if explain || stdout_is_tty {
                let ast_json = serde_json::to_string_pretty(&report.query)
                    .unwrap_or_else(|_| "{}".to_string());
                println!("{}", palette.cyan("Query AST:"));
                println!("{ast_json}");
                println!();
            }
            if visible_notes.is_empty() {
                println!("No notes matched.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for note in visible_notes {
                    print_note(note);
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            if explain {
                let payload = serde_json::json!({
                    "query": report.query,
                    "notes": rows,
                });
                export_rows(
                    std::slice::from_ref(&payload),
                    list_controls.fields.as_deref(),
                    export,
                )?;
                print_json(&payload)
            } else {
                export_rows(&rows, list_controls.fields.as_deref(), export)?;
                print_json_lines(rows, list_controls.fields.as_deref())
            }
        }
    }
}

fn print_rebuild_report(
    output: OutputFormat,
    report: &RebuildReport,
    use_color: bool,
) -> Result<(), CliError> {
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human => {
            if report.dry_run {
                println!(
                    "{}: would rebuild {} discovered files with {} cached documents",
                    palette.cyan("Dry run"),
                    report.discovered,
                    report.existing_documents
                );
            } else if let Some(summary) = report.summary.as_ref() {
                println!(
                    "{} from {} files: {} added, {} updated, {} unchanged, {} deleted",
                    palette.cyan("Rebuilt cache"),
                    summary.discovered,
                    palette.green(&summary.added.to_string()),
                    palette.yellow(&summary.updated.to_string()),
                    summary.unchanged,
                    palette.red(&summary.deleted.to_string())
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_repair_fts_report(output: OutputFormat, report: &RepairFtsReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.dry_run {
                println!(
                    "Dry run: would rebuild FTS rows for {} chunks across {} documents",
                    report.indexed_chunks, report.indexed_documents
                );
            } else {
                println!(
                    "Rebuilt FTS rows for {} chunks across {} documents",
                    report.indexed_chunks, report.indexed_documents
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_watch_report(output: OutputFormat, report: &WatchReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.startup {
                println!(
                    "Initial scan: {} added, {} updated, {} unchanged, {} deleted",
                    report.summary.added,
                    report.summary.updated,
                    report.summary.unchanged,
                    report.summary.deleted
                );
            } else {
                println!(
                    "Watch update ({} events, {} paths): {} added, {} updated, {} unchanged, {} deleted",
                    report.event_count,
                    report.paths.len(),
                    report.summary.added,
                    report.summary.updated,
                    report.summary.unchanged,
                    report.summary.deleted
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_vector_index_report(
    output: OutputFormat,
    report: &VectorIndexReport,
    use_color: bool,
) -> Result<(), CliError> {
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human => {
            println!(
                "{} vectors with {}:{} {}: {} indexed, {} skipped, {} failed {}",
                if report.dry_run {
                    palette.cyan("Dry run for")
                } else {
                    palette.cyan("Indexed")
                },
                report.provider_name,
                report.model_name,
                palette.dim(&format!(
                    "(dims {}, batch size {}, concurrency {})",
                    report.dimensions, report.batch_size, report.max_concurrency
                )),
                palette.green(&report.indexed.to_string()),
                report.skipped,
                palette.red(&report.failed.to_string()),
                palette.dim(&format!("in {:.3}s", report.elapsed_seconds))
            );
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_vector_queue_report(
    output: OutputFormat,
    report: &VectorQueueReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!(
                "Vector queue {}:{}: {} pending, {} indexed, {} stale{}",
                report.provider_name,
                report.model_name,
                report.pending_chunks,
                report.indexed_chunks,
                report.stale_vectors,
                if report.model_mismatch {
                    " (model mismatch)"
                } else {
                    ""
                }
            );
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_vector_repair_report(
    output: OutputFormat,
    report: &VectorRepairReport,
    use_color: bool,
) -> Result<(), CliError> {
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human => {
            let status = if report.dry_run {
                palette.cyan("Dry run")
            } else if report.repaired {
                palette.green("Repaired")
            } else {
                palette.cyan("Checked")
            };
            println!(
                "{} vectors for {}:{}: {} pending, {} stale{}",
                status,
                report.provider_name,
                report.model_name,
                report.pending_chunks,
                report.stale_vectors,
                if report.model_mismatch {
                    " (model mismatch)"
                } else {
                    ""
                }
            );
            if let Some(index_report) = report.index_report.as_ref() {
                println!(
                    "{} indexed, {} skipped, {} failed",
                    index_report.indexed, index_report.skipped, index_report.failed
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_vector_neighbors_report(
    output: OutputFormat,
    report: &VectorNeighborsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_hits = paginated_items(&report.hits, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = vector_neighbor_rows(report, visible_hits);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                if let Some(query_text) = report.query_text.as_deref() {
                    println!(
                        "{} {}",
                        palette.cyan("Vector neighbors for"),
                        palette.bold(query_text)
                    );
                } else if let Some(note_path) = report.note_path.as_deref() {
                    println!(
                        "{} {}",
                        palette.cyan("Vector neighbors for note"),
                        palette.bold(note_path)
                    );
                }
            }
            if visible_hits.is_empty() {
                println!("No vector neighbors.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for (index, hit) in visible_hits.iter().enumerate() {
                    print_vector_neighbor(index, hit, palette);
                }
            }

            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_related_notes_report(
    output: OutputFormat,
    report: &RelatedNotesReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_hits = paginated_items(&report.hits, list_controls);
    let rows = related_note_rows(report, visible_hits);
    let palette = AnsiPalette::new(use_color);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!(
                    "{} {}",
                    palette.cyan("Related notes for"),
                    palette.bold(&report.note_path)
                );
            }
            if visible_hits.is_empty() {
                println!("No related notes.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for (index, hit) in visible_hits.iter().enumerate() {
                    println!(
                        "{}. {} ({:.3}, {} chunks)",
                        index + 1,
                        hit.document_path,
                        hit.similarity,
                        hit.matched_chunks
                    );
                    if !hit.heading_path.is_empty() {
                        println!("   {}", hit.heading_path.join(" > "));
                    }
                    println!("   {}", hit.snippet);
                }
            }

            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_vector_duplicates_report(
    output: OutputFormat,
    report: &VectorDuplicatesReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_pairs = paginated_items(&report.pairs, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = vector_duplicate_rows(report, visible_pairs);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Vector duplicates"));
            }
            if visible_pairs.is_empty() {
                println!("No duplicate pairs.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for pair in visible_pairs {
                    print_vector_duplicate(pair);
                }
            }

            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_vector_models_report(
    output: OutputFormat,
    models: &[StoredModelInfo],
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows: Vec<Value> = models
        .iter()
        .map(|m| {
            serde_json::json!({
                "cache_key": m.cache_key,
                "provider": m.provider_name,
                "model": m.model_name,
                "dimensions": m.dimensions,
                "normalized": m.normalized,
                "chunks": m.chunk_count,
                "active": m.is_active,
            })
        })
        .collect();

    match output {
        OutputFormat::Human => {
            let palette = AnsiPalette::new(use_color);
            if stdout_is_tty {
                println!("{}", palette.cyan("Vector models"));
            }
            if models.is_empty() {
                println!("No stored models.");
                return Ok(());
            }
            for model in models {
                let active_marker = if model.is_active { " (active)" } else { "" };
                println!(
                    "{}{}\n  provider: {}  model: {}  dimensions: {}  normalized: {}  chunks: {}",
                    palette.bold(&model.cache_key),
                    active_marker,
                    model.provider_name,
                    model.model_name,
                    model.dimensions,
                    model.normalized,
                    model.chunk_count,
                );
            }
            export_rows(&rows, None, export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, None, export)?;
            print_json_lines(rows, None)
        }
    }
}

fn print_cluster_report(
    output: OutputFormat,
    report: &ClusterReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_clusters = paginated_items(&report.clusters, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = cluster_rows(report, visible_clusters);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                if report.dry_run {
                    println!("Vector clusters (dry run)");
                } else {
                    println!("Vector clusters");
                }
            }
            if visible_clusters.is_empty() {
                println!("No vector clusters.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for (index, cluster) in visible_clusters.iter().enumerate() {
                    print_cluster_summary(index, cluster, palette);
                }
            }

            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_bases_report(
    output: OutputFormat,
    report: &BasesEvalReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows = bases_rows(report);
    let visible_rows = paginated_items(&rows, list_controls);
    let palette = AnsiPalette::new(use_color);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!(
                    "{} {}",
                    palette.cyan("Bases eval"),
                    palette.bold(&report.file)
                );
            }
            if rows.is_empty() {
                println!("No bases rows.");
            } else if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible_rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                print_bases_human(report, list_controls, palette);
            }

            if !report.diagnostics.is_empty() {
                println!("{}:", palette.yellow("Diagnostics"));
                for diagnostic in &report.diagnostics {
                    if let Some(path) = diagnostic.path.as_deref() {
                        println!("- {path}: {}", diagnostic.message);
                    } else {
                        println!("- {}", diagnostic.message);
                    }
                }
            }

            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            if list_controls.fields.is_some() {
                print_json_lines(visible_rows.to_vec(), list_controls.fields.as_deref())
            } else {
                print_json(report)
            }
        }
    }
}

fn print_bases_view_edit_report(
    output: OutputFormat,
    report: &BasesViewEditReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.dry_run {
                println!("Dry run: {}", report.action);
            } else {
                println!("{}", report.action);
            }
            println!(
                "{} views, {} diagnostics",
                report.eval.views.len(),
                report.eval.diagnostics.len()
            );
            for diag in &report.eval.diagnostics {
                let path = diag.path.as_deref().unwrap_or("(root)");
                println!("  warning [{path}]: {}", diag.message);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_mention_suggestions_report(
    output: OutputFormat,
    report: &MentionSuggestionsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_suggestions = paginated_items(&report.suggestions, list_controls);
    let rows = mention_suggestion_rows(visible_suggestions);
    let palette = AnsiPalette::new(use_color);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Mention suggestions"));
            }
            if visible_suggestions.is_empty() {
                println!("No mention suggestions.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for suggestion in visible_suggestions {
                    print_mention_suggestion(suggestion, palette);
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_duplicate_suggestions_report(
    output: OutputFormat,
    report: &DuplicateSuggestionsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows = duplicate_suggestion_rows(report);
    let visible_rows = paginated_items(&rows, list_controls);
    let palette = AnsiPalette::new(use_color);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Duplicate suggestions"));
            }
            if rows.is_empty() {
                println!("No duplicate suggestions.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible_rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                print_duplicate_groups("Duplicate titles", &report.duplicate_titles);
                print_duplicate_groups("Alias collisions", &report.alias_collisions);
                print_merge_candidates(&report.merge_candidates, palette);
            }
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(visible_rows.to_vec(), list_controls.fields.as_deref())
        }
    }
}

fn print_links_report(
    output: OutputFormat,
    report: &OutgoingLinksReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_links = paginated_items(&report.links, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = outgoing_link_rows(report, visible_links);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!(
                    "{} {} {}",
                    palette.cyan("Links for"),
                    palette.bold(&report.note_path),
                    palette.dim(&format!("({:?})", report.matched_by))
                );
            }
            if visible_links.is_empty() {
                println!("No outgoing links.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for link in visible_links {
                    print_outgoing_link(link);
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_backlinks_report(
    output: OutputFormat,
    report: &BacklinksReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_backlinks = paginated_items(&report.backlinks, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = backlink_rows(report, visible_backlinks);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!(
                    "{} {} {}",
                    palette.cyan("Backlinks for"),
                    palette.bold(&report.note_path),
                    palette.dim(&format!("({:?})", report.matched_by))
                );
            }
            if visible_backlinks.is_empty() {
                println!("No backlinks.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for backlink in visible_backlinks {
                    print_backlink(backlink);
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_init_summary(output: OutputFormat, summary: &InitSummary) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!(
                "Initialized {} (config {}, cache {})",
                summary.vault_root.display(),
                if summary.created_config {
                    "created"
                } else {
                    "existing"
                },
                if summary.created_cache {
                    "created"
                } else {
                    "existing"
                },
            );
            Ok(())
        }
        OutputFormat::Json => print_json(summary),
    }
}

fn print_scan_summary(output: OutputFormat, summary: &ScanSummary, use_color: bool) {
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human => {
            println!(
                "{} {} files: {} added, {} updated, {} unchanged, {} deleted",
                palette.cyan("Scanned"),
                summary.discovered,
                palette.green(&summary.added.to_string()),
                palette.yellow(&summary.updated.to_string()),
                summary.unchanged,
                palette.red(&summary.deleted.to_string())
            );
        }
        OutputFormat::Json => {
            print_json(summary).expect("scan summary JSON serialization should succeed");
        }
    }
}

fn print_move_summary(output: OutputFormat, summary: &MoveSummary) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if summary.dry_run {
                println!(
                    "Dry run: move {} -> {}",
                    summary.source_path, summary.destination_path
                );
            } else {
                println!(
                    "Moved {} -> {}",
                    summary.source_path, summary.destination_path
                );
            }

            if summary.rewritten_files.is_empty() {
                println!("No link rewrites.");
                return Ok(());
            }

            for file in &summary.rewritten_files {
                println!("- {}", file.path);
                for change in &file.changes {
                    println!("  {} -> {}", change.before, change.after);
                }
            }

            Ok(())
        }
        OutputFormat::Json => print_json(summary),
    }
}

fn print_refactor_report(output: OutputFormat, report: &RefactorReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.dry_run {
                println!("Dry run for {}", report.action);
            } else {
                println!("Applied {}", report.action);
            }

            if report.files.is_empty() {
                println!("No files changed.");
                return Ok(());
            }

            for file in &report.files {
                println!("- {}", file.path);
                for change in &file.changes {
                    println!("  {} -> {}", change.before, change.after);
                }
            }

            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_bulk_mutation_report(
    output: OutputFormat,
    report: &BulkMutationReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.dry_run {
                println!("Dry run for {}", report.action);
            } else {
                println!("Applied {}", report.action);
            }

            if report.files.is_empty() {
                println!("No files changed.");
                return Ok(());
            }

            for file in &report.files {
                if file.changes.is_empty() {
                    println!("- {} (no change)", file.path);
                } else {
                    println!("- {}", file.path);
                }
            }

            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_doctor_report(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &DoctorReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("Doctor summary for {}", paths.vault_root().display());
            println!("- unresolved links: {}", report.summary.unresolved_links);
            println!(
                "- ambiguous link targets: {}",
                report.summary.ambiguous_links
            );
            println!("- broken embeds: {}", report.summary.broken_embeds);
            println!("- parse failures: {}", report.summary.parse_failures);
            println!("- stale index rows: {}", report.summary.stale_index_rows);
            println!(
                "- missing index rows: {}",
                report.summary.missing_index_rows
            );
            println!("- orphan notes: {}", report.summary.orphan_notes);
            println!("- orphan assets: {}", report.summary.orphan_assets);
            println!("- HTML links: {}", report.summary.html_links);

            if report.summary == zero_summary() {
                println!("No issues found.");
                return Ok(());
            }

            print_link_section("Unresolved links", &report.unresolved_links);
            print_link_section("Ambiguous link targets", &report.ambiguous_links);
            print_link_section("Broken embeds", &report.broken_embeds);
            print_diagnostic_section("Parse failures", &report.parse_failures);
            print_path_section("Stale index rows", &report.stale_index_rows);
            print_path_section("Missing index rows", &report.missing_index_rows);
            print_path_section("Orphan notes", &report.orphan_notes);
            print_path_section("Orphan assets", &report.orphan_assets);
            print_diagnostic_section("HTML links", &report.html_links);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_doctor_fix_report(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &DoctorFixReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.dry_run {
                println!("Doctor fix plan for {}", paths.vault_root().display());
            } else {
                println!("Doctor fix run for {}", paths.vault_root().display());
            }
            if report.fixes.is_empty() {
                println!("No deterministic fixes needed.");
            } else {
                for fix in &report.fixes {
                    println!("- {}: {}", fix.kind, fix.description);
                }
            }

            if !report.suggestions.is_empty() {
                println!("Suggestions:");
                for suggestion in &report.suggestions {
                    println!("- {suggestion}");
                }
            }

            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_graph_path_report(output: OutputFormat, report: &GraphPathReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.path.is_empty() {
                println!(
                    "No resolved path from {} to {}.",
                    report.from_path, report.to_path
                );
            } else {
                println!("{}", report.path.join(" -> "));
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_graph_hubs_report(
    output: OutputFormat,
    report: &GraphHubsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_notes = paginated_items(&report.notes, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = graph_hub_rows(visible_notes);
    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Graph hubs"));
            }
            if visible_notes.is_empty() {
                println!("No graph hubs.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for note in visible_notes {
                    println!(
                        "- {} [{} inbound, {} outbound]",
                        note.document_path, note.inbound, note.outbound
                    );
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_graph_moc_report(
    output: OutputFormat,
    report: &GraphMocReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_notes = paginated_items(&report.notes, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = graph_moc_rows(visible_notes);
    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("{}", palette.cyan("MOC candidates"));
            }
            if visible_notes.is_empty() {
                println!("No MOC candidates.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for note in visible_notes {
                    println!(
                        "- {} [score {}, {} inbound, {} outbound]",
                        note.document_path, note.score, note.inbound, note.outbound
                    );
                    if !note.reasons.is_empty() {
                        println!("  {}", note.reasons.join("; "));
                    }
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_graph_dead_ends_report(
    output: OutputFormat,
    report: &GraphDeadEndsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_notes = paginated_items(&report.notes, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = graph_dead_end_rows(visible_notes);
    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Graph dead ends"));
            }
            if visible_notes.is_empty() {
                println!("No dead ends.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for note in visible_notes {
                    println!("- {note}");
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_graph_components_report(
    output: OutputFormat,
    report: &GraphComponentsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_components = paginated_items(&report.components, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = graph_component_rows(visible_components);
    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Graph components"));
            }
            if visible_components.is_empty() {
                println!("No components.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for component in visible_components {
                    println!("- size {}: {}", component.size, component.notes.join(", "));
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_graph_analytics_report(
    output: OutputFormat,
    report: &GraphAnalyticsReport,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows = graph_analytics_rows(report);
    match output {
        OutputFormat::Human => {
            println!("Notes: {}", report.note_count);
            println!("Attachments: {}", report.attachment_count);
            println!("Bases: {}", report.base_count);
            println!("Resolved note links: {}", report.resolved_note_links);
            println!(
                "Average outbound links: {:.3}",
                report.average_outbound_links
            );
            println!("Orphan notes: {}", report.orphan_notes);
            print_named_count_section("Top tags", &report.top_tags);
            print_named_count_section("Top properties", &report.top_properties);
            export_rows(&rows, None, export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, None, export)?;
            print_json(report)
        }
    }
}

fn print_graph_trends_report(
    output: OutputFormat,
    report: &GraphTrendsReport,
    list_controls: &ListOutputControls,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows = graph_trend_rows(report);
    let visible_rows = paginated_items(&rows, list_controls);
    let visible_points = paginated_items(&report.points, list_controls);
    match output {
        OutputFormat::Human => {
            if report.points.is_empty() {
                println!("No graph trend checkpoints.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible_rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for point in visible_points {
                    println!(
                        "- {}: {} notes, {} orphan, {} stale, {} resolved links",
                        point.label,
                        point.note_count,
                        point.orphan_notes,
                        point.stale_notes,
                        point.resolved_links
                    );
                }
            }
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            if list_controls.fields.is_some() || export.is_some() {
                print_json_lines(visible_rows.to_vec(), list_controls.fields.as_deref())
            } else {
                print_json(report)
            }
        }
    }
}

fn print_checkpoint_record(
    output: OutputFormat,
    record: &CheckpointRecord,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!(
                "Checkpoint {} [{}]: {} notes, {} orphan, {} stale, {} links",
                record.name.as_deref().unwrap_or(&record.id),
                record.source,
                record.note_count,
                record.orphan_notes,
                record.stale_notes,
                record.resolved_links
            );
            Ok(())
        }
        OutputFormat::Json => print_json(record),
    }
}

fn print_checkpoint_list(
    output: OutputFormat,
    records: &[CheckpointRecord],
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible = paginated_items(records, list_controls);
    let rows = checkpoint_rows(visible);
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Checkpoints"));
            }
            if visible.is_empty() {
                println!("No checkpoints.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for record in visible {
                    println!(
                        "- {} [{}] notes={}, orphan={}, stale={}, links={}",
                        record.name.as_deref().unwrap_or(&record.id),
                        record.source,
                        record.note_count,
                        record.orphan_notes,
                        record.stale_notes,
                        record.resolved_links
                    );
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_change_report(
    output: OutputFormat,
    report: &ChangeReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows = change_rows(report);
    let visible = paginated_items(&rows, list_controls);
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!(
                    "{} {}",
                    palette.cyan("Changes since"),
                    palette.bold(&report.anchor)
                );
            }
            if visible.is_empty() {
                println!("No recorded changes.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for row in visible {
                    println!(
                        "- {} {} ({})",
                        row["status"].as_str().unwrap_or("updated"),
                        row["path"].as_str().unwrap_or_default(),
                        row["kind"].as_str().unwrap_or_default()
                    );
                }
            }
            export_rows(visible, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(visible, list_controls.fields.as_deref(), export)?;
            print_json_lines(visible.to_vec(), list_controls.fields.as_deref())
        }
    }
}

fn print_static_search_index_report(
    output: OutputFormat,
    report: &vulcan_core::StaticSearchIndexReport,
    path: Option<&PathBuf>,
    pretty: bool,
) -> Result<(), CliError> {
    let rendered = if pretty {
        serde_json::to_string_pretty(report).map_err(CliError::operation)?
    } else {
        serde_json::to_string(report).map_err(CliError::operation)?
    };

    if let Some(path) = path {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        fs::write(path, format!("{rendered}\n")).map_err(CliError::operation)?;
        match output {
            OutputFormat::Human => {
                println!(
                    "Exported static search index: {} documents, {} chunks -> {}",
                    report.documents,
                    report.chunks,
                    path.display()
                );
                Ok(())
            }
            OutputFormat::Json => print_json(&serde_json::json!({
                "path": path.display().to_string(),
                "documents": report.documents,
                "chunks": report.chunks,
            })),
        }
    } else {
        println!("{rendered}");
        Ok(())
    }
}

fn print_automation_run_report(
    output: OutputFormat,
    report: &AutomationRunReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("Automation actions: {}", report.actions.join(", "));
            if let Some(scan) = report.scan.as_ref() {
                println!(
                    "- scan: {} added, {} updated, {} unchanged, {} deleted",
                    scan.added, scan.updated, scan.unchanged, scan.deleted
                );
            }
            if let Some(summary) = report.doctor_issues.as_ref() {
                println!(
                    "- doctor: unresolved={}, ambiguous={}, parse_failures={}, stale={}, missing={}",
                    summary.unresolved_links,
                    summary.ambiguous_links,
                    summary.parse_failures,
                    summary.stale_index_rows,
                    summary.missing_index_rows
                );
            }
            if let Some(fix) = report.doctor_fix.as_ref() {
                let summary = fix.issues_after.as_ref().unwrap_or(&fix.issues_before);
                println!(
                    "- doctor-fix: {} actions, unresolved={}, ambiguous={}, parse_failures={}, stale={}, missing={}",
                    fix.fixes.len(),
                    summary.unresolved_links,
                    summary.ambiguous_links,
                    summary.parse_failures,
                    summary.stale_index_rows,
                    summary.missing_index_rows
                );
            }
            if let Some(cache) = report.cache_verify.as_ref() {
                println!("- cache-verify: healthy={}", cache.healthy);
            }
            if let Some(fts) = report.repair_fts.as_ref() {
                println!(
                    "- repair-fts: {} chunks across {} documents",
                    fts.indexed_chunks, fts.indexed_documents
                );
            }
            if let Some(batch) = report.reports.as_ref() {
                println!(
                    "- saved-reports: {} succeeded, {} failed",
                    batch.succeeded, batch.failed
                );
            }
            if report.issues_detected {
                println!("Issues detected.");
            } else {
                println!("No issues detected.");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_cache_inspect_report(
    output: OutputFormat,
    report: &CacheInspectReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("Cache: {}", report.cache_path);
            println!("Bytes: {}", report.database_bytes);
            println!("Documents: {}", report.documents);
            println!("Notes: {}", report.notes);
            println!("Attachments: {}", report.attachments);
            println!("Bases: {}", report.bases);
            println!("Links: {}", report.links);
            println!("Chunks: {}", report.chunks);
            println!("Diagnostics: {}", report.diagnostics);
            println!("Search rows: {}", report.search_rows);
            println!("Vector rows: {}", report.vector_rows);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_cache_verify_report(
    output: OutputFormat,
    report: &CacheVerifyReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("Cache healthy: {}", report.healthy);
            for check in &report.checks {
                println!(
                    "- {} [{}] {}",
                    check.name,
                    if check.ok { "ok" } else { "fail" },
                    check.detail
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_cache_vacuum_report(
    output: OutputFormat,
    report: &CacheVacuumReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.dry_run {
                println!("Dry run: cache is {} bytes", report.before_bytes);
            } else {
                println!(
                    "Vacuumed cache: {} -> {} bytes (reclaimed {})",
                    report.before_bytes,
                    report.after_bytes.unwrap_or(report.before_bytes),
                    report.reclaimed_bytes.unwrap_or(0)
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<(), CliError> {
    println!(
        "{}",
        serde_json::to_string(value).map_err(CliError::operation)?
    );
    Ok(())
}

fn print_json_lines(rows: Vec<Value>, fields: Option<&[String]>) -> Result<(), CliError> {
    for row in rows {
        let selected = select_fields(row, fields);
        println!(
            "{}",
            serde_json::to_string(&selected).map_err(CliError::operation)?
        );
    }

    Ok(())
}

fn resolve_vault_root(vault: &PathBuf) -> Result<PathBuf, CliError> {
    if vault.is_absolute() {
        return Ok(vault.clone());
    }

    Ok(std::env::current_dir()
        .map_err(|error| CliError::io(&error))?
        .join(vault))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CliDescribeReport {
    name: String,
    about: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after_help: Option<String>,
    version: Option<String>,
    global_options: Vec<CliArgDescribe>,
    commands: Vec<CliCommandDescribe>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CliCommandDescribe {
    name: String,
    about: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after_help: Option<String>,
    options: Vec<CliArgDescribe>,
    subcommands: Vec<CliCommandDescribe>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CliArgDescribe {
    id: String,
    long: Option<String>,
    short: Option<char>,
    help: Option<String>,
    required: bool,
    value_names: Vec<String>,
    possible_values: Vec<String>,
}

fn describe_cli() -> CliDescribeReport {
    let command = Cli::command().bin_name("vulcan");
    let name = command
        .get_bin_name()
        .unwrap_or(command.get_name())
        .to_string();
    CliDescribeReport {
        name,
        about: command.get_about().map(ToString::to_string),
        after_help: command.get_after_help().map(ToString::to_string),
        version: command.get_version().map(ToString::to_string),
        global_options: command
            .get_arguments()
            .filter(|argument| argument.is_global_set())
            .map(describe_argument)
            .collect(),
        commands: command.get_subcommands().map(describe_command).collect(),
    }
}

fn describe_command(command: &clap::Command) -> CliCommandDescribe {
    CliCommandDescribe {
        name: command.get_name().to_string(),
        about: command.get_about().map(ToString::to_string),
        after_help: command.get_after_help().map(ToString::to_string),
        options: command
            .get_arguments()
            .filter(|argument| !argument.is_global_set())
            .map(describe_argument)
            .collect(),
        subcommands: command.get_subcommands().map(describe_command).collect(),
    }
}

fn describe_argument(argument: &clap::Arg) -> CliArgDescribe {
    CliArgDescribe {
        id: argument.get_id().to_string(),
        long: argument.get_long().map(ToString::to_string),
        short: argument.get_short(),
        help: argument.get_help().map(ToString::to_string),
        required: argument.is_required_set(),
        value_names: argument.get_value_names().map_or_else(Vec::new, |values| {
            values.iter().map(ToString::to_string).collect()
        }),
        possible_values: argument
            .get_possible_values()
            .into_iter()
            .map(|value| value.get_name().to_string())
            .collect(),
    }
}

fn print_link_section(title: &str, issues: &[DoctorLinkIssue]) {
    if issues.is_empty() {
        return;
    }

    println!();
    println!("{title}:");
    for issue in issues {
        let path = issue.document_path.as_deref().unwrap_or("<unknown>");
        if issue.matches.is_empty() {
            if let Some(target) = issue.target.as_deref() {
                println!("- {path}: {target} ({})", issue.message);
            } else {
                println!("- {path}: {}", issue.message);
            }
        } else if let Some(target) = issue.target.as_deref() {
            println!(
                "- {path}: {target} ({}) [{}]",
                issue.message,
                issue.matches.join(", ")
            );
        } else {
            println!("- {path}: {} [{}]", issue.message, issue.matches.join(", "));
        }
    }
}

fn print_diagnostic_section(title: &str, issues: &[DoctorDiagnosticIssue]) {
    if issues.is_empty() {
        return;
    }

    println!();
    println!("{title}:");
    for issue in issues {
        let path = issue.document_path.as_deref().unwrap_or("<unknown>");
        if let Some(byte_range) = issue.byte_range.as_ref() {
            println!(
                "- {path}: {} (bytes {}-{})",
                issue.message, byte_range.start, byte_range.end
            );
        } else {
            println!("- {path}: {}", issue.message);
        }
    }
}

fn print_path_section(title: &str, paths: &[String]) {
    if paths.is_empty() {
        return;
    }

    println!();
    println!("{title}:");
    for path in paths {
        println!("- {path}");
    }
}

fn outgoing_link_rows(report: &OutgoingLinksReport, links: &[OutgoingLinkRecord]) -> Vec<Value> {
    links
        .iter()
        .map(|link| {
            serde_json::json!({
                "note_path": report.note_path,
                "matched_by": report.matched_by,
                "raw_text": link.raw_text,
                "link_kind": link.link_kind,
                "display_text": link.display_text,
                "target_path_candidate": link.target_path_candidate,
                "target_heading": link.target_heading,
                "target_block": link.target_block,
                "resolved_target_path": link.resolved_target_path,
                "resolution_status": link.resolution_status,
                "context": link.context,
            })
        })
        .collect()
}

fn backlink_rows(report: &BacklinksReport, backlinks: &[BacklinkRecord]) -> Vec<Value> {
    backlinks
        .iter()
        .map(|backlink| {
            serde_json::json!({
                "note_path": report.note_path,
                "matched_by": report.matched_by,
                "source_path": backlink.source_path,
                "raw_text": backlink.raw_text,
                "link_kind": backlink.link_kind,
                "display_text": backlink.display_text,
                "context": backlink.context,
            })
        })
        .collect()
}

fn search_hit_rows(report: &SearchReport, hits: &[SearchHit]) -> Vec<Value> {
    hits.iter()
        .map(|hit| {
            serde_json::json!({
                "query": report.query,
                "mode": report.mode,
                "tag": report.tag,
                "path_prefix": report.path_prefix,
                "has_property": report.has_property,
                "filters": report.filters,
                "effective_query": report.plan.as_ref().map(|plan| plan.effective_query.clone()),
                "document_path": hit.document_path,
                "chunk_id": hit.chunk_id,
                "heading_path": hit.heading_path,
                "snippet": hit.snippet,
                "rank": hit.rank,
                "explain": hit.explain,
            })
        })
        .collect()
}

fn vector_neighbor_rows(report: &VectorNeighborsReport, hits: &[VectorNeighborHit]) -> Vec<Value> {
    hits.iter()
        .map(|hit| {
            serde_json::json!({
                "provider_name": report.provider_name,
                "model_name": report.model_name,
                "dimensions": report.dimensions,
                "query_text": report.query_text,
                "note_path": report.note_path,
                "document_path": hit.document_path,
                "chunk_id": hit.chunk_id,
                "heading_path": hit.heading_path,
                "snippet": hit.snippet,
                "distance": hit.distance,
            })
        })
        .collect()
}

fn related_note_rows(report: &RelatedNotesReport, hits: &[RelatedNoteHit]) -> Vec<Value> {
    hits.iter()
        .map(|hit| {
            serde_json::json!({
                "provider_name": report.provider_name,
                "model_name": report.model_name,
                "dimensions": report.dimensions,
                "note_path": report.note_path,
                "document_path": hit.document_path,
                "heading_path": hit.heading_path,
                "snippet": hit.snippet,
                "similarity": hit.similarity,
                "matched_chunks": hit.matched_chunks,
            })
        })
        .collect()
}

fn vector_duplicate_rows(
    report: &VectorDuplicatesReport,
    pairs: &[VectorDuplicatePair],
) -> Vec<Value> {
    pairs
        .iter()
        .map(|pair| {
            serde_json::json!({
                "provider_name": report.provider_name,
                "model_name": report.model_name,
                "dimensions": report.dimensions,
                "threshold": report.threshold,
                "left_document_path": pair.left_document_path,
                "left_chunk_id": pair.left_chunk_id,
                "right_document_path": pair.right_document_path,
                "right_chunk_id": pair.right_chunk_id,
                "similarity": pair.similarity,
            })
        })
        .collect()
}

fn cluster_rows(report: &ClusterReport, clusters: &[vulcan_core::ClusterSummary]) -> Vec<Value> {
    clusters
        .iter()
        .map(|cluster| {
            serde_json::json!({
                "provider_name": report.provider_name,
                "model_name": report.model_name,
                "dimensions": report.dimensions,
                "cluster_count": report.cluster_count,
                "cluster_id": cluster.cluster_id,
                "cluster_label": cluster.cluster_label,
                "keywords": cluster.keywords,
                "chunk_count": cluster.chunk_count,
                "document_count": cluster.document_count,
                "document_path": cluster.exemplar_document_path,
                "heading_path": cluster.exemplar_heading_path,
                "exemplar_document_path": cluster.exemplar_document_path,
                "exemplar_heading_path": cluster.exemplar_heading_path,
                "exemplar_snippet": cluster.exemplar_snippet,
                "top_documents": cluster.top_documents,
            })
        })
        .collect()
}

fn note_rows(report: &NotesReport, notes: &[NoteRecord]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "filters": report.filters,
                "sort_by": report.sort_by,
                "sort_descending": report.sort_descending,
                "document_path": note.document_path,
                "file_name": note.file_name,
                "file_ext": note.file_ext,
                "file_mtime": note.file_mtime,
                "properties": note.properties,
            })
        })
        .collect()
}

fn bases_rows(report: &BasesEvalReport) -> Vec<Value> {
    report
        .views
        .iter()
        .flat_map(|view| {
            view.rows.iter().map(|row| {
                serde_json::json!({
                    "file": report.file,
                    "view_name": view.name,
                    "view_type": view.view_type,
                    "filters": view.filters,
                    "sort_by": view.sort_by,
                    "sort_descending": view.sort_descending,
                    "columns": view.columns,
                    "group_by": view.group_by,
                    "group_value": row.group_value,
                    "document_path": row.document_path,
                    "file_name": row.file_name,
                    "file_ext": row.file_ext,
                    "file_mtime": row.file_mtime,
                    "properties": row.properties,
                    "formulas": row.formulas,
                    "cells": row.cells,
                })
            })
        })
        .collect()
}

fn mention_suggestion_rows(suggestions: &[MentionSuggestion]) -> Vec<Value> {
    suggestions
        .iter()
        .map(|suggestion| {
            serde_json::json!({
                "kind": if suggestion.target_path.is_some() { "mention" } else { "ambiguous_mention" },
                "status": if suggestion.target_path.is_some() { "unambiguous" } else { "ambiguous" },
                "source_path": suggestion.source_path,
                "matched_text": suggestion.matched_text,
                "target_path": suggestion.target_path,
                "candidate_paths": suggestion.candidate_paths,
                "candidate_count": suggestion.candidate_paths.len(),
                "line": suggestion.line,
                "column": suggestion.column,
                "context": suggestion.context,
            })
        })
        .collect()
}

fn duplicate_suggestion_rows(report: &DuplicateSuggestionsReport) -> Vec<Value> {
    let mut rows = Vec::new();
    rows.extend(report.duplicate_titles.iter().map(|group| {
        serde_json::json!({
            "kind": "duplicate_title",
            "value": group.value,
            "paths": group.paths,
            "path_count": group.paths.len(),
            "left_path": Value::Null,
            "right_path": Value::Null,
            "score": Value::Null,
            "reasons": Value::Null,
        })
    }));
    rows.extend(report.alias_collisions.iter().map(|group| {
        serde_json::json!({
            "kind": "alias_collision",
            "value": group.value,
            "paths": group.paths,
            "path_count": group.paths.len(),
            "left_path": Value::Null,
            "right_path": Value::Null,
            "score": Value::Null,
            "reasons": Value::Null,
        })
    }));
    rows.extend(report.merge_candidates.iter().map(|candidate| {
        serde_json::json!({
            "kind": "merge_candidate",
            "value": Value::Null,
            "paths": Value::Null,
            "path_count": 2,
            "left_path": candidate.left_path,
            "right_path": candidate.right_path,
            "score": candidate.score,
            "reasons": candidate.reasons,
        })
    }));
    rows
}

fn saved_report_summary_rows(reports: &[SavedReportSummary]) -> Vec<Value> {
    reports
        .iter()
        .map(|report| {
            serde_json::json!({
                "name": report.name,
                "kind": report.kind,
                "description": report.description,
                "fields": report.fields,
                "limit": report.limit,
                "export_format": report.export.as_ref().map(|export| export.format),
                "export_path": report.export.as_ref().map(|export| export.path.clone()),
            })
        })
        .collect()
}

fn select_fields(row: Value, fields: Option<&[String]>) -> Value {
    let Some(fields) = fields else {
        return row;
    };
    let Some(object) = row.as_object() else {
        return row;
    };
    let mut selected = Map::new();
    for field in fields {
        if let Some(value) = object.get(field) {
            selected.insert(field.clone(), value.clone());
        }
    }
    Value::Object(selected)
}

fn print_selected_human_fields(row: &Value, fields: &[String]) {
    let Some(object) = row.as_object() else {
        println!("{row}");
        return;
    };

    let rendered = fields
        .iter()
        .filter_map(|field| {
            object
                .get(field)
                .map(|value| format!("{field}={}", render_human_value(value)))
        })
        .collect::<Vec<_>>();

    println!("{}", rendered.join(" | "));
}

fn render_human_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".to_string(),
        _ => value.to_string(),
    }
}

fn print_outgoing_link(link: &OutgoingLinkRecord) {
    let target = link
        .resolved_target_path
        .as_deref()
        .or(link.target_path_candidate.as_deref())
        .unwrap_or("(self)");

    if let Some(context) = link.context.as_ref() {
        println!(
            "- {} [{} {:?}] line {}: {}",
            target, link.link_kind, link.resolution_status, context.line, link.raw_text
        );
    } else {
        println!(
            "- {} [{} {:?}]: {}",
            target, link.link_kind, link.resolution_status, link.raw_text
        );
    }
}

fn print_backlink(backlink: &BacklinkRecord) {
    if let Some(context) = backlink.context.as_ref() {
        println!(
            "- {} [{}] line {}: {}",
            backlink.source_path, backlink.link_kind, context.line, backlink.raw_text
        );
    } else {
        println!(
            "- {} [{}]: {}",
            backlink.source_path, backlink.link_kind, backlink.raw_text
        );
    }
}

fn print_mention_suggestion(suggestion: &MentionSuggestion, palette: AnsiPalette) {
    let location = format!(
        "{}:{}:{}",
        suggestion.source_path, suggestion.line, suggestion.column
    );
    let summary = match suggestion.target_path.as_deref() {
        Some(target_path) => format!(
            "{} -> {}",
            palette.bold(&suggestion.matched_text),
            target_path
        ),
        None => format!(
            "{} -> {}",
            palette.bold(&suggestion.matched_text),
            suggestion.candidate_paths.join(", ")
        ),
    };
    let label = if suggestion.target_path.is_some() {
        palette.green("link")
    } else {
        palette.yellow("review")
    };
    println!("- {location} [{label}] {summary}");
    println!("  {}", suggestion.context.trim());
}

fn print_duplicate_groups(title: &str, groups: &[vulcan_core::DuplicateGroup]) {
    if groups.is_empty() {
        return;
    }

    println!("{title}:");
    for group in groups {
        println!("- {} -> {}", group.value, group.paths.join(", "));
    }
    println!();
}

fn print_merge_candidates(candidates: &[MergeCandidate], palette: AnsiPalette) {
    if candidates.is_empty() {
        return;
    }

    println!("Merge candidates:");
    for candidate in candidates {
        println!(
            "- {} <-> {} ({:.2})",
            candidate.left_path, candidate.right_path, candidate.score
        );
        println!("  {}", palette.dim(&candidate.reasons.join(", ")));
    }
}

fn print_search_hit(index: usize, hit: &SearchHit, palette: AnsiPalette) {
    let location = if hit.heading_path.is_empty() {
        hit.document_path.clone()
    } else {
        format!("{} > {}", hit.document_path, hit.heading_path.join(" > "))
    };

    println!("{}. {}", index + 1, palette.bold(&location));
    println!("   {}: {:.3}", palette.cyan("Rank"), hit.rank);

    if let Some(explain) = hit.explain.as_ref() {
        println!(
            "   {}: {}",
            palette.cyan("Explain"),
            render_search_hit_explain(explain)
        );
    }

    let lines = hit
        .snippet
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if let Some((first, rest)) = lines.split_first() {
        println!("   {}: {first}", palette.cyan("Snippet"));
        for line in rest {
            println!("            {line}");
        }
    } else {
        println!("   {}: <empty>", palette.cyan("Snippet"));
    }

    println!();
}

fn print_vector_neighbor(index: usize, hit: &VectorNeighborHit, palette: AnsiPalette) {
    print_ranked_snippet_hit(
        index,
        &hit.document_path,
        &hit.heading_path,
        "Distance",
        f64::from(hit.distance),
        &hit.snippet,
        palette,
    );
}

fn print_vector_duplicate(pair: &VectorDuplicatePair) {
    println!(
        "- {} <-> {} [{:.3}]",
        pair.left_document_path, pair.right_document_path, pair.similarity
    );
}

fn print_cluster_summary(
    index: usize,
    cluster: &vulcan_core::ClusterSummary,
    palette: AnsiPalette,
) {
    println!(
        "{}. {}",
        index + 1,
        palette.bold(&format!(
            "[{}] {}",
            cluster.cluster_id + 1,
            cluster.cluster_label
        ))
    );
    println!(
        "   {}: {} chunks across {} notes",
        palette.cyan("Size"),
        cluster.chunk_count,
        cluster.document_count
    );
    println!(
        "   {}: {}",
        palette.cyan("Exemplar"),
        if cluster.exemplar_heading_path.is_empty() {
            cluster.exemplar_document_path.clone()
        } else {
            format!(
                "{} > {}",
                cluster.exemplar_document_path,
                cluster.exemplar_heading_path.join(" > ")
            )
        }
    );
    let snippet_lines = cluster.exemplar_snippet.lines().collect::<Vec<&str>>();
    if let Some((first, rest)) = snippet_lines.split_first() {
        println!("   {}: {}", palette.cyan("Snippet"), first.trim());
        for line in rest
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
        {
            println!("            {line}");
        }
    }
    if !cluster.top_documents.is_empty() {
        println!("   {}:", palette.cyan("Top notes"));
        for document in &cluster.top_documents {
            println!("   - {} ({})", document.document_path, document.chunk_count);
        }
    }
    println!();
}

fn print_ranked_snippet_hit(
    index: usize,
    document_path: &str,
    heading_path: &[String],
    metric_label: &str,
    metric_value: f64,
    snippet: &str,
    palette: AnsiPalette,
) {
    let location = if heading_path.is_empty() {
        document_path.to_string()
    } else {
        format!("{document_path} > {}", heading_path.join(" > "))
    };

    println!("{}. {}", index + 1, palette.bold(&location));
    println!("   {}: {metric_value:.3}", palette.cyan(metric_label));

    let lines = snippet
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if let Some((first, rest)) = lines.split_first() {
        println!("   {}: {first}", palette.cyan("Snippet"));
        for line in rest {
            println!("            {line}");
        }
    } else {
        println!("   {}: <empty>", palette.cyan("Snippet"));
    }

    println!();
}

fn print_search_plan(plan: &vulcan_core::SearchPlan, palette: AnsiPalette) {
    println!("{}: {}", palette.cyan("Plan"), plan.effective_query);
    if !plan.semantic_text.is_empty() && plan.semantic_text != plan.effective_query {
        println!("{}: {}", palette.cyan("Vector text"), plan.semantic_text);
    }
    if let Some(tag) = plan.tag.as_deref() {
        println!("{}: {tag}", palette.cyan("Tag"));
    }
    if let Some(path_prefix) = plan.path_prefix.as_deref() {
        println!("{}: {path_prefix}", palette.cyan("Path prefix"));
    }
    if let Some(has_property) = plan.has_property.as_deref() {
        println!("{}: {has_property}", palette.cyan("Has property"));
    }
    if !plan.property_filters.is_empty() {
        println!(
            "{}: {}",
            palette.cyan("Filters"),
            plan.property_filters.join(" | ")
        );
    }
    if plan.fuzzy_fallback_used {
        for expansion in &plan.fuzzy_expansions {
            println!(
                "{}: {} -> {}",
                palette.cyan("Fuzzy"),
                expansion.term,
                expansion.candidates.join(", ")
            );
        }
    }
    println!();
}

fn render_search_hit_explain(explain: &vulcan_core::SearchHitExplain) -> String {
    match explain.strategy.as_str() {
        "keyword" => format!(
            "bm25={:.3}, keyword_rank={}",
            explain.bm25.unwrap_or(explain.score),
            explain.keyword_rank.unwrap_or(1)
        ),
        "hybrid" => {
            let mut parts = vec![format!("score={:.3}", explain.score)];
            if let Some(rank) = explain.keyword_rank {
                parts.push(format!(
                    "keyword#{} ({:.3})",
                    rank,
                    explain.keyword_contribution.unwrap_or_default()
                ));
            }
            if let Some(rank) = explain.vector_rank {
                parts.push(format!(
                    "vector#{} ({:.3})",
                    rank,
                    explain.vector_contribution.unwrap_or_default()
                ));
            }
            parts.join(", ")
        }
        _ => format!("score={:.3}", explain.score),
    }
}

fn print_note(note: &NoteRecord) {
    println!("- {}", note.document_path);
}

fn print_bases_human(
    report: &BasesEvalReport,
    list_controls: &ListOutputControls,
    palette: AnsiPalette,
) {
    let mut row_index = 0_usize;
    let mut printed_any = false;
    let end = list_controls.limit.map_or(usize::MAX, |limit| {
        list_controls.offset.saturating_add(limit)
    });

    for view in &report.views {
        let mut visible_rows = Vec::new();
        for row in &view.rows {
            if row_index < list_controls.offset {
                row_index += 1;
                continue;
            }
            if row_index >= end {
                break;
            }
            visible_rows.push(row);
            row_index += 1;
        }

        if !visible_rows.is_empty() {
            print_bases_view_header(view, visible_rows.len(), palette);
            print_bases_table(view, &visible_rows, palette);
            printed_any = true;
        }

        if row_index >= end {
            break;
        }
    }

    if !printed_any {
        println!("No bases rows.");
    }
}

fn print_bases_view_header(
    view: &vulcan_core::BasesEvaluatedView,
    visible_rows: usize,
    palette: AnsiPalette,
) {
    let name = view.name.as_deref().unwrap_or("view");
    let row_summary = if visible_rows == view.rows.len() {
        format!("{} rows", view.rows.len())
    } else {
        format!("{visible_rows} of {} rows", view.rows.len())
    };
    println!(
        "{} {}",
        palette.bold(name),
        palette.dim(&format!("({row_summary})"))
    );
    if !view.columns.is_empty() {
        let columns = view
            .columns
            .iter()
            .map(|column| column.display_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!("{}: {columns}", palette.cyan("Columns"));
    }
    if let Some(group_by) = view.group_by.as_ref() {
        println!(
            "{}: {}{}",
            palette.cyan("Grouped by"),
            group_by.display_name,
            if group_by.descending { " (desc)" } else { "" }
        );
    }
    println!();
}

fn print_bases_table(
    view: &vulcan_core::BasesEvaluatedView,
    rows: &[&vulcan_core::BasesRow],
    palette: AnsiPalette,
) {
    let group_key = view
        .group_by
        .as_ref()
        .map(|group_by| group_by.property.as_str());
    let mut columns = view
        .columns
        .iter()
        .filter(|column| Some(column.key.as_str()) != group_key)
        .collect::<Vec<_>>();
    if columns.is_empty() {
        columns = view.columns.iter().collect();
    }
    let widths = columns
        .iter()
        .map(|column| {
            rows.iter()
                .map(|row| bases_cell_text(row, &column.key).chars().count())
                .fold(column.display_name.chars().count(), usize::max)
                .min(BASES_MAX_COLUMN_WIDTH)
        })
        .collect::<Vec<_>>();

    if view.group_by.is_some() {
        let mut start = 0_usize;
        while start < rows.len() {
            let group_name = bases_group_name(rows[start]);
            let mut end = start + 1;
            while end < rows.len() && bases_group_name(rows[end]) == group_name {
                end += 1;
            }

            println!(
                "{} {} {}",
                palette.green("Group:"),
                palette.bold(&group_name),
                palette.dim(&format!("({} rows)", end - start))
            );
            print_bases_table_header(&columns, &widths, palette);
            for row in &rows[start..end] {
                print_bases_table_row(row, &columns, &widths);
            }
            println!();
            start = end;
        }
    } else {
        print_bases_table_header(&columns, &widths, palette);
        for row in rows {
            print_bases_table_row(row, &columns, &widths);
        }
        println!();
    }
}

fn print_bases_table_header(
    columns: &[&vulcan_core::BasesColumn],
    widths: &[usize],
    palette: AnsiPalette,
) {
    let header = columns
        .iter()
        .zip(widths.iter().copied())
        .map(|(column, width)| fit_bases_cell(&column.display_name, width))
        .collect::<Vec<_>>()
        .join("  ");
    let separator = widths
        .iter()
        .map(|width| "-".repeat(*width))
        .collect::<Vec<_>>()
        .join("  ");
    println!("{}", palette.bold(&header));
    println!("{}", palette.dim(&separator));
}

fn print_bases_table_row(
    row: &vulcan_core::BasesRow,
    columns: &[&vulcan_core::BasesColumn],
    widths: &[usize],
) {
    let rendered = columns
        .iter()
        .zip(widths.iter().copied())
        .map(|(column, width)| fit_bases_cell(&bases_cell_text(row, &column.key), width))
        .collect::<Vec<_>>()
        .join("  ");
    println!("{rendered}");
}

fn bases_group_name(row: &vulcan_core::BasesRow) -> String {
    row.group_value
        .as_ref()
        .map(render_human_value)
        .filter(|value| !value.is_empty() && value != "null")
        .unwrap_or_else(|| "Ungrouped".to_string())
}

fn bases_cell_text(row: &vulcan_core::BasesRow, key: &str) -> String {
    bases_value_for_key(row, key)
        .filter(|value| !value.is_null())
        .map(|value| render_human_value(&value))
        .filter(|value| !value.is_empty() && value != "null")
        .unwrap_or_else(|| "-".to_string())
}

fn fit_bases_cell(value: &str, width: usize) -> String {
    let truncated = if value.chars().count() > width {
        if width <= 3 {
            ".".repeat(width)
        } else {
            value.chars().take(width - 3).collect::<String>() + "..."
        }
    } else {
        value.to_string()
    };
    format!("{truncated:<width$}")
}

fn bases_value_for_key(row: &vulcan_core::BasesRow, key: &str) -> Option<Value> {
    if let Some(value) = row.cells.get(key) {
        return Some(value.clone());
    }
    if let Some(value) = row.formulas.get(key) {
        return Some(value.clone());
    }

    match key {
        "file.path" => Some(Value::String(row.document_path.clone())),
        "file.name" => Some(Value::String(row.file_name.clone())),
        "file.ext" => Some(Value::String(row.file_ext.clone())),
        "file.mtime" => Some(Value::Number(row.file_mtime.into())),
        property => row.properties.get(property).cloned(),
    }
}

fn print_named_count_section(title: &str, counts: &[NamedCount]) {
    if counts.is_empty() {
        return;
    }
    println!("{title}:");
    for count in counts {
        println!("- {} ({})", count.name, count.count);
    }
}

fn zero_summary() -> vulcan_core::DoctorSummary {
    vulcan_core::DoctorSummary {
        unresolved_links: 0,
        ambiguous_links: 0,
        broken_embeds: 0,
        parse_failures: 0,
        stale_index_rows: 0,
        missing_index_rows: 0,
        orphan_notes: 0,
        orphan_assets: 0,
        html_links: 0,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ListOutputControls {
    fields: Option<Vec<String>>,
    limit: Option<usize>,
    offset: usize,
}

impl ListOutputControls {
    fn from_cli(cli: &Cli) -> Self {
        Self {
            fields: cli.fields.clone(),
            limit: cli.limit,
            offset: cli.offset,
        }
    }

    fn with_saved_defaults(&self, fields: Option<Vec<String>>, limit: Option<usize>) -> Self {
        Self {
            fields: self.fields.clone().or(fields),
            limit: self.limit.or(limit),
            offset: self.offset,
        }
    }

    fn requested_result_limit(&self) -> Option<usize> {
        self.limit.map(|limit| limit.saturating_add(self.offset))
    }
}

fn paginated_items<'a, T>(items: &'a [T], controls: &ListOutputControls) -> &'a [T] {
    let start = controls.offset.min(items.len());
    let end = controls.limit.map_or(items.len(), |limit| {
        start.saturating_add(limit).min(items.len())
    });

    &items[start..end]
}

fn graph_hub_rows(notes: &[vulcan_core::GraphNodeScore]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "document_path": note.document_path,
                "inbound": note.inbound,
                "outbound": note.outbound,
                "total": note.total,
            })
        })
        .collect()
}

fn graph_moc_rows(notes: &[GraphMocCandidate]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "document_path": note.document_path,
                "inbound": note.inbound,
                "outbound": note.outbound,
                "score": note.score,
                "reasons": note.reasons,
            })
        })
        .collect()
}

fn graph_dead_end_rows(notes: &[String]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| serde_json::json!({ "document_path": note }))
        .collect()
}

fn graph_component_rows(components: &[vulcan_core::GraphComponent]) -> Vec<Value> {
    components
        .iter()
        .map(|component| {
            serde_json::json!({
                "size": component.size,
                "notes": component.notes,
            })
        })
        .collect()
}

fn graph_analytics_rows(report: &GraphAnalyticsReport) -> Vec<Value> {
    vec![serde_json::json!({
        "note_count": report.note_count,
        "attachment_count": report.attachment_count,
        "base_count": report.base_count,
        "resolved_note_links": report.resolved_note_links,
        "average_outbound_links": report.average_outbound_links,
        "orphan_notes": report.orphan_notes,
        "top_tags": report.top_tags,
        "top_properties": report.top_properties,
    })]
}

fn graph_trend_rows(report: &GraphTrendsReport) -> Vec<Value> {
    report
        .points
        .iter()
        .map(|point| {
            serde_json::json!({
                "label": point.label,
                "created_at": point.created_at,
                "note_count": point.note_count,
                "orphan_notes": point.orphan_notes,
                "stale_notes": point.stale_notes,
                "resolved_links": point.resolved_links,
            })
        })
        .collect()
}

fn checkpoint_rows(records: &[CheckpointRecord]) -> Vec<Value> {
    records
        .iter()
        .map(|record| {
            serde_json::json!({
                "id": record.id,
                "name": record.name,
                "source": record.source,
                "created_at": record.created_at,
                "note_count": record.note_count,
                "orphan_notes": record.orphan_notes,
                "stale_notes": record.stale_notes,
                "resolved_links": record.resolved_links,
            })
        })
        .collect()
}

fn change_rows(report: &ChangeReport) -> Vec<Value> {
    let mut rows = Vec::new();
    append_change_rows(&mut rows, &report.anchor, ChangeKind::Note, &report.notes);
    append_change_rows(&mut rows, &report.anchor, ChangeKind::Link, &report.links);
    append_change_rows(
        &mut rows,
        &report.anchor,
        ChangeKind::Property,
        &report.properties,
    );
    append_change_rows(
        &mut rows,
        &report.anchor,
        ChangeKind::Embedding,
        &report.embeddings,
    );
    rows
}

fn append_change_rows(rows: &mut Vec<Value>, anchor: &str, kind: ChangeKind, items: &[ChangeItem]) {
    let kind_name = serde_json::to_value(kind)
        .expect("change kind should serialize")
        .as_str()
        .expect("change kind should serialize to a string")
        .to_string();
    for item in items {
        rows.push(serde_json::json!({
            "anchor": anchor,
            "kind": kind_name,
            "path": item.path,
            "status": item.status,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn formats_eta_compactly_for_progress_reporting() {
        assert_eq!(format_eta(0, 12.0), "0s");
        assert_eq!(format_eta(5, 10.0), "<1s");
        assert_eq!(format_eta(120, 10.0), "12.0s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2m 5s");
    }

    #[test]
    fn parses_defaults_for_doctor_command() {
        let cli = Cli::try_parse_from(["vulcan", "doctor"]).expect("cli should parse");

        assert_eq!(cli.vault, PathBuf::from("."));
        assert_eq!(cli.output, OutputFormat::Human);
        assert_eq!(cli.fields, None);
        assert_eq!(cli.limit, None);
        assert_eq!(cli.offset, 0);
        assert!(!cli.verbose);
        assert_eq!(
            cli.command,
            Command::Doctor {
                fix: false,
                dry_run: false,
                fail_on_issues: false,
            }
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn parses_links_and_backlinks_commands() {
        let rebuild =
            Cli::try_parse_from(["vulcan", "rebuild", "--dry-run"]).expect("cli should parse");
        let repair = Cli::try_parse_from(["vulcan", "repair", "fts", "--dry-run"])
            .expect("cli should parse");
        let watch = Cli::try_parse_from(["vulcan", "watch", "--debounce-ms", "125"])
            .expect("cli should parse");
        let serve = Cli::try_parse_from([
            "vulcan",
            "serve",
            "--bind",
            "127.0.0.1:4000",
            "--no-watch",
            "--debounce-ms",
            "100",
            "--auth-token",
            "secret",
        ])
        .expect("cli should parse");
        let doctor = Cli::try_parse_from(["vulcan", "doctor", "--fix", "--dry-run"])
            .expect("cli should parse");
        let doctor_fail = Cli::try_parse_from(["vulcan", "doctor", "--fail-on-issues"])
            .expect("cli should parse");
        let graph_path = Cli::try_parse_from(["vulcan", "graph", "path", "Home", "Bob"])
            .expect("cli should parse");
        let graph_moc = Cli::try_parse_from(["vulcan", "graph", "moc"]).expect("cli should parse");
        let graph_trends = Cli::try_parse_from(["vulcan", "graph", "trends", "--limit", "7"])
            .expect("cli should parse");
        let cache_verify = Cli::try_parse_from(["vulcan", "cache", "verify", "--fail-on-errors"])
            .expect("cli should parse");
        let cache_vacuum = Cli::try_parse_from(["vulcan", "cache", "vacuum", "--dry-run"])
            .expect("cli should parse");
        let export_search_index =
            Cli::try_parse_from(["vulcan", "export", "search-index", "--pretty"])
                .expect("cli should parse");
        let links = Cli::try_parse_from(["vulcan", "links", "Home"]).expect("cli should parse");
        let links_picker = Cli::try_parse_from(["vulcan", "links"]).expect("cli should parse");
        let backlinks = Cli::try_parse_from(["vulcan", "backlinks", "Projects/Alpha"])
            .expect("cli should parse");
        let related_picker = Cli::try_parse_from(["vulcan", "related"]).expect("cli should parse");
        let search = Cli::try_parse_from([
            "vulcan",
            "search",
            "dashboard",
            "--where",
            "reviewed = true",
            "--tag",
            "index",
            "--path-prefix",
            "People/",
            "--has-property",
            "status",
            "--context-size",
            "24",
            "--fuzzy",
            "--explain",
        ])
        .expect("cli should parse");
        let notes = Cli::try_parse_from([
            "vulcan",
            "notes",
            "--where",
            "status = done",
            "--where",
            "estimate > 2",
            "--sort",
            "due",
            "--desc",
        ])
        .expect("cli should parse");
        let bases = Cli::try_parse_from(["vulcan", "bases", "eval", "release.base"])
            .expect("cli should parse");
        let bases_tui = Cli::try_parse_from(["vulcan", "bases", "tui", "release.base"])
            .expect("cli should parse");
        let suggest_mentions = Cli::try_parse_from(["vulcan", "suggest", "mentions", "Home"])
            .expect("cli should parse");
        let suggest_duplicates =
            Cli::try_parse_from(["vulcan", "suggest", "duplicates"]).expect("cli should parse");
        let link_mentions = Cli::try_parse_from(["vulcan", "link-mentions", "Home", "--dry-run"])
            .expect("cli should parse");
        let rewrite = Cli::try_parse_from([
            "vulcan",
            "rewrite",
            "--where",
            "reviewed = true",
            "--find",
            "release",
            "--replace",
            "launch",
            "--dry-run",
        ])
        .expect("cli should parse");
        let vectors = Cli::try_parse_from(["vulcan", "vectors", "index", "--dry-run"])
            .expect("cli should parse");
        let vector_repair = Cli::try_parse_from(["vulcan", "vectors", "repair", "--dry-run"])
            .expect("cli should parse");
        let vector_rebuild = Cli::try_parse_from(["vulcan", "vectors", "rebuild", "--dry-run"])
            .expect("cli should parse");
        let vector_queue = Cli::try_parse_from(["vulcan", "vectors", "queue", "status"])
            .expect("cli should parse");
        let vector_related = Cli::try_parse_from(["vulcan", "vectors", "related", "Home"])
            .expect("cli should parse");
        let duplicates =
            Cli::try_parse_from(["vulcan", "vectors", "duplicates"]).expect("cli should parse");
        let cluster = Cli::try_parse_from(["vulcan", "cluster", "--clusters", "3", "--dry-run"])
            .expect("cli should parse");
        let related = Cli::try_parse_from(["vulcan", "related", "Home"]).expect("cli should parse");
        let move_command = Cli::try_parse_from([
            "vulcan",
            "move",
            "Projects/Alpha.md",
            "Archive/Alpha.md",
            "--dry-run",
        ])
        .expect("cli should parse");
        let completions =
            Cli::try_parse_from(["vulcan", "completions", "bash"]).expect("cli should parse");
        let saved_search = Cli::try_parse_from([
            "vulcan",
            "--fields",
            "document_path,rank",
            "--limit",
            "5",
            "saved",
            "search",
            "weekly",
            "dashboard",
            "--where",
            "reviewed = true",
            "--raw-query",
            "--fuzzy",
            "--description",
            "weekly dashboard",
            "--export",
            "csv",
            "--export-path",
            "exports/weekly.csv",
        ])
        .expect("cli should parse");
        let saved_run = Cli::try_parse_from([
            "vulcan",
            "saved",
            "run",
            "weekly",
            "--export",
            "jsonl",
            "--export-path",
            "exports/weekly.jsonl",
        ])
        .expect("cli should parse");
        let checkpoint_create = Cli::try_parse_from(["vulcan", "checkpoint", "create", "weekly"])
            .expect("cli should parse");
        let checkpoint_list =
            Cli::try_parse_from(["vulcan", "checkpoint", "list"]).expect("cli should parse");
        let changes = Cli::try_parse_from(["vulcan", "changes", "--checkpoint", "weekly"])
            .expect("cli should parse");
        let batch = Cli::try_parse_from(["vulcan", "batch", "--all"]).expect("cli should parse");
        let automation = Cli::try_parse_from([
            "vulcan",
            "automation",
            "run",
            "--scan",
            "--doctor",
            "--verify-cache",
            "--repair-fts",
            "--all-reports",
            "--fail-on-issues",
        ])
        .expect("cli should parse");

        assert_eq!(rebuild.command, Command::Rebuild { dry_run: true });
        assert_eq!(
            repair.command,
            Command::Repair {
                command: RepairCommand::Fts { dry_run: true }
            }
        );
        assert_eq!(watch.command, Command::Watch { debounce_ms: 125 });
        assert_eq!(
            serve.command,
            Command::Serve {
                bind: "127.0.0.1:4000".to_string(),
                no_watch: true,
                debounce_ms: 100,
                auth_token: Some("secret".to_string()),
            }
        );
        assert_eq!(
            doctor.command,
            Command::Doctor {
                fix: true,
                dry_run: true,
                fail_on_issues: false,
            }
        );
        assert_eq!(
            doctor_fail.command,
            Command::Doctor {
                fix: false,
                dry_run: false,
                fail_on_issues: true,
            }
        );
        assert_eq!(
            graph_path.command,
            Command::Graph {
                command: GraphCommand::Path {
                    from: Some("Home".to_string()),
                    to: Some("Bob".to_string()),
                }
            }
        );
        assert_eq!(
            graph_moc.command,
            Command::Graph {
                command: GraphCommand::Moc {
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            graph_trends.command,
            Command::Graph {
                command: GraphCommand::Trends {
                    limit: 7,
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            cache_verify.command,
            Command::Cache {
                command: CacheCommand::Verify {
                    fail_on_errors: true,
                }
            }
        );
        assert_eq!(
            cache_vacuum.command,
            Command::Cache {
                command: CacheCommand::Vacuum { dry_run: true }
            }
        );
        assert_eq!(
            export_search_index.command,
            Command::Export {
                command: ExportCommand::SearchIndex {
                    path: None,
                    pretty: true,
                },
            }
        );

        assert_eq!(
            links.command,
            Command::Links {
                note: Some("Home".to_string()),
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            links_picker.command,
            Command::Links {
                note: None,
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            backlinks.command,
            Command::Backlinks {
                note: Some("Projects/Alpha".to_string()),
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            search.command,
            Command::Search {
                query: "dashboard".to_string(),
                filters: vec!["reviewed = true".to_string()],
                mode: SearchMode::Keyword,
                tag: Some("index".to_string()),
                path_prefix: Some("People/".to_string()),
                has_property: Some("status".to_string()),
                context_size: 24,
                raw_query: false,
                fuzzy: true,
                explain: true,
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            notes.command,
            Command::Notes {
                filters: vec!["status = done".to_string(), "estimate > 2".to_string()],
                sort: Some("due".to_string()),
                desc: true,
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            bases.command,
            Command::Bases {
                command: BasesCommand::Eval {
                    file: "release.base".to_string(),
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            bases_tui.command,
            Command::Bases {
                command: BasesCommand::Tui {
                    file: "release.base".to_string(),
                },
            }
        );
        assert_eq!(
            suggest_mentions.command,
            Command::Suggest {
                command: SuggestCommand::Mentions {
                    note: Some("Home".to_string()),
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            suggest_duplicates.command,
            Command::Suggest {
                command: SuggestCommand::Duplicates {
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            link_mentions.command,
            Command::LinkMentions {
                note: Some("Home".to_string()),
                dry_run: true,
            }
        );
        assert_eq!(
            rewrite.command,
            Command::Rewrite {
                filters: vec!["reviewed = true".to_string()],
                find: "release".to_string(),
                replace: "launch".to_string(),
                dry_run: true,
            }
        );
        assert_eq!(
            vectors.command,
            Command::Vectors {
                command: VectorsCommand::Index { dry_run: true },
            }
        );
        assert_eq!(
            vector_repair.command,
            Command::Vectors {
                command: VectorsCommand::Repair { dry_run: true },
            }
        );
        assert_eq!(
            vector_rebuild.command,
            Command::Vectors {
                command: VectorsCommand::Rebuild { dry_run: true },
            }
        );
        assert_eq!(
            vector_queue.command,
            Command::Vectors {
                command: VectorsCommand::Queue {
                    command: VectorQueueCommand::Status,
                },
            }
        );
        assert_eq!(
            vector_related.command,
            Command::Vectors {
                command: VectorsCommand::Related {
                    note: Some("Home".to_string()),
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            duplicates.command,
            Command::Vectors {
                command: VectorsCommand::Duplicates {
                    threshold: 0.95,
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            cluster.command,
            Command::Cluster {
                clusters: 3,
                dry_run: true,
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            related.command,
            Command::Related {
                note: Some("Home".to_string()),
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            related_picker.command,
            Command::Related {
                note: None,
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            move_command.command,
            Command::Move {
                source: "Projects/Alpha.md".to_string(),
                dest: "Archive/Alpha.md".to_string(),
                dry_run: true
            }
        );
        assert_eq!(
            Cli::try_parse_from(["vulcan", "rename-property", "status", "phase", "--dry-run"])
                .expect("cli should parse")
                .command,
            Command::RenameProperty {
                old: "status".to_string(),
                new: "phase".to_string(),
                dry_run: true,
            }
        );
        assert_eq!(
            Cli::try_parse_from(["vulcan", "merge-tags", "project", "initiative", "--dry-run"])
                .expect("cli should parse")
                .command,
            Command::MergeTags {
                source: "project".to_string(),
                dest: "initiative".to_string(),
                dry_run: true,
            }
        );
        assert_eq!(
            Cli::try_parse_from([
                "vulcan",
                "rename-alias",
                "Home",
                "Start",
                "Landing",
                "--dry-run"
            ])
            .expect("cli should parse")
            .command,
            Command::RenameAlias {
                note: "Home".to_string(),
                old: "Start".to_string(),
                new: "Landing".to_string(),
                dry_run: true,
            }
        );
        assert_eq!(
            Cli::try_parse_from([
                "vulcan",
                "rename-heading",
                "Projects/Alpha",
                "Status",
                "Progress",
                "--dry-run"
            ])
            .expect("cli should parse")
            .command,
            Command::RenameHeading {
                note: "Projects/Alpha".to_string(),
                old: "Status".to_string(),
                new: "Progress".to_string(),
                dry_run: true,
            }
        );
        assert_eq!(
            Cli::try_parse_from([
                "vulcan",
                "rename-block-ref",
                "Projects/Alpha",
                "alpha-status",
                "alpha-progress",
                "--dry-run"
            ])
            .expect("cli should parse")
            .command,
            Command::RenameBlockRef {
                note: "Projects/Alpha".to_string(),
                old: "alpha-status".to_string(),
                new: "alpha-progress".to_string(),
                dry_run: true,
            }
        );
        assert_eq!(
            completions.command,
            Command::Completions {
                shell: clap_complete::Shell::Bash
            }
        );
        assert_eq!(
            saved_search.command,
            Command::Saved {
                command: SavedCommand::Search {
                    name: "weekly".to_string(),
                    query: "dashboard".to_string(),
                    filters: vec!["reviewed = true".to_string()],
                    mode: SearchMode::Keyword,
                    tag: None,
                    path_prefix: None,
                    has_property: None,
                    context_size: 18,
                    raw_query: true,
                    fuzzy: true,
                    description: Some("weekly dashboard".to_string()),
                    export: ExportArgs {
                        export: Some(ExportFormat::Csv),
                        export_path: Some(PathBuf::from("exports/weekly.csv")),
                    },
                },
            }
        );
        assert_eq!(
            saved_run.command,
            Command::Saved {
                command: SavedCommand::Run {
                    name: "weekly".to_string(),
                    export: ExportArgs {
                        export: Some(ExportFormat::Jsonl),
                        export_path: Some(PathBuf::from("exports/weekly.jsonl")),
                    },
                },
            }
        );
        assert_eq!(
            checkpoint_create.command,
            Command::Checkpoint {
                command: CheckpointCommand::Create {
                    name: "weekly".to_string(),
                },
            }
        );
        assert_eq!(
            checkpoint_list.command,
            Command::Checkpoint {
                command: CheckpointCommand::List {
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            changes.command,
            Command::Changes {
                checkpoint: Some("weekly".to_string()),
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            batch.command,
            Command::Batch {
                names: Vec::new(),
                all: true,
            }
        );
        assert_eq!(
            automation.command,
            Command::Automation {
                command: AutomationCommand::Run {
                    reports: Vec::new(),
                    all_reports: true,
                    scan: true,
                    doctor: true,
                    doctor_fix: false,
                    verify_cache: true,
                    repair_fts: true,
                    fail_on_issues: true,
                }
            }
        );
    }

    #[test]
    fn parses_global_flags_and_scan_options() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "--vault",
            "/tmp/vault",
            "--output",
            "json",
            "--fields",
            "source_path,raw_text",
            "--limit",
            "10",
            "--offset",
            "2",
            "--verbose",
            "scan",
            "--full",
        ])
        .expect("cli should parse");

        assert_eq!(cli.vault, PathBuf::from("/tmp/vault"));
        assert_eq!(cli.output, OutputFormat::Json);
        assert_eq!(
            cli.fields,
            Some(vec!["source_path".to_string(), "raw_text".to_string()])
        );
        assert_eq!(cli.limit, Some(10));
        assert_eq!(cli.offset, 2);
        assert!(cli.verbose);
        assert_eq!(cli.command, Command::Scan { full: true });
    }

    #[test]
    fn resolves_relative_vault_path_against_current_directory() {
        let current_dir = std::env::current_dir().expect("cwd should be available");
        let resolved = resolve_vault_root(&PathBuf::from("tests/fixtures/vaults/basic"))
            .expect("path resolution should succeed");

        assert_eq!(resolved, current_dir.join("tests/fixtures/vaults/basic"));
    }

    #[test]
    fn describe_report_lists_core_commands() {
        let report = describe_cli();

        assert_eq!(report.name, "vulcan");
        let rebuild = report
            .commands
            .iter()
            .find(|command| command.name == "rebuild")
            .expect("rebuild command should be described");
        assert_eq!(
            rebuild.about.as_deref(),
            Some("Rebuild the cache from disk")
        );
        let completions = report
            .commands
            .iter()
            .find(|command| command.name == "completions")
            .expect("completions command should be described");
        assert_eq!(
            completions.about.as_deref(),
            Some("Generate shell completion scripts")
        );
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "repair"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "watch"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "serve"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "rename-property"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "graph"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "cache"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "saved"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "suggest"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "link-mentions"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "rewrite"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "checkpoint"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "changes"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "batch"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "related"));
    }
}
