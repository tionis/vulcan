mod bases_tui;
mod browse_tui;
mod cli;
mod commit;
mod editor;
mod note_picker;
mod serve;

pub use cli::{
    AutomationCommand, BasesCommand, CacheCommand, CheckpointCommand, Cli, Command,
    DataviewCommand, ExportArgs, ExportCommand, ExportFormat, GraphCommand, OutputFormat,
    RefreshMode, RepairCommand, SavedCommand, SearchMode, SearchSortArg, SuggestCommand,
    TasksCommand, TemplateSubcommand, VectorQueueCommand, VectorsCommand,
};

use crate::commit::AutoCommitPolicy;
use crate::editor::open_in_editor;
use clap::{CommandFactory, Parser};
use clap_complete::generate;
use serde::Serialize;
use serde_json::{Map, Value};
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use serve::{serve_forever, ServeOptions};
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{Duration, Instant};
use vulcan_core::expression::eval::{evaluate as evaluate_expression, is_truthy, EvalContext};
use vulcan_core::expression::functions::{date_components, parse_date_like_string};
use vulcan_core::expression::parse_expression;
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::properties::load_note_index;
use vulcan_core::{
    bases_view_add, bases_view_delete, bases_view_edit, bases_view_rename, bulk_replace,
    bulk_set_property, cache_vacuum, cluster_vectors, create_checkpoint, doctor_fix, doctor_vault,
    drop_vector_model, evaluate_base_file, evaluate_dql, evaluate_note_inline_expressions,
    evaluate_tasks_query, execute_query_report, export_static_search_index, git_status,
    index_vectors_with_progress, initialize_vault, inspect_cache, inspect_vector_queue,
    link_mentions, list_checkpoints, list_saved_reports, list_vector_models, load_dataview_blocks,
    load_saved_report, load_tasks_blocks, load_vault_config, merge_tags, move_note,
    parse_tasks_query, query_backlinks, query_change_report, query_graph_analytics,
    query_graph_components, query_graph_dead_ends, query_graph_hubs, query_graph_moc_candidates,
    query_graph_path, query_graph_trends, query_links, query_notes, query_related_notes,
    query_vector_neighbors, rebuild_vault_with_progress, rebuild_vectors_with_progress,
    rename_alias, rename_block_ref, rename_heading, rename_property, repair_fts,
    repair_vectors_with_progress, resolve_note_reference, save_saved_report,
    scan_vault_with_progress, search_vault, suggest_duplicates, suggest_mentions,
    task_upcoming_occurrences, vector_duplicates, verify_cache, watch_vault, AutoScanMode,
    BacklinkRecord, BacklinksReport, BaseViewGroupBy, BaseViewPatch, BaseViewSpec, BasesEvalReport,
    BasesViewEditReport, BulkMutationReport, CacheInspectReport, CacheVacuumQuery,
    CacheVacuumReport, CacheVerifyReport, ChangeAnchor, ChangeItem, ChangeKind, ChangeReport,
    CheckpointRecord, ClusterQuery, ClusterReport, DoctorDiagnosticIssue, DoctorFixReport,
    DoctorLinkIssue, DoctorReport, DqlQueryResult, DuplicateSuggestionsReport,
    EvaluatedInlineExpression, GraphAnalyticsReport, GraphComponentsReport, GraphDeadEndsReport,
    GraphHubsReport, GraphMocCandidate, GraphMocReport, GraphPathReport, GraphQueryError,
    GraphTrendsReport, InitSummary, MentionSuggestion, MentionSuggestionsReport, MergeCandidate,
    MoveSummary, NamedCount, NoteQuery, NoteRecord, NotesReport, OutgoingLinkRecord,
    OutgoingLinksReport, QueryAst, QueryReport, RebuildQuery, RebuildReport, RefactorReport,
    RelatedNoteHit, RelatedNotesQuery, RelatedNotesReport, RepairFtsQuery, RepairFtsReport,
    SavedExport, SavedExportFormat, SavedReportDefinition, SavedReportKind, SavedReportQuery,
    SavedReportSummary, ScanMode, ScanPhase, ScanProgress, ScanSummary, SearchHit, SearchQuery,
    SearchReport, SearchSort, StoredModelInfo, TasksQueryResult, TemplatesConfig, VaultPaths,
    VectorDuplicatePair, VectorDuplicatesQuery, VectorDuplicatesReport, VectorIndexPhase,
    VectorIndexProgress, VectorIndexQuery, VectorIndexReport, VectorNeighborHit,
    VectorNeighborsQuery, VectorNeighborsReport, VectorQueueReport, VectorRebuildQuery,
    VectorRepairQuery, VectorRepairReport, WatchOptions, WatchReport,
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
enum RefreshTarget {
    Command,
    Browse,
}

impl Display for TemplateInsertionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoteFrontmatterNotMapping => {
                formatter.write_str("target note frontmatter must be a YAML mapping")
            }
            Self::NoteFrontmatterParse(error) => {
                write!(
                    formatter,
                    "failed to parse target note frontmatter: {error}"
                )
            }
            Self::TemplateFrontmatterNotMapping => {
                formatter.write_str("template frontmatter must be a YAML mapping")
            }
            Self::TemplateFrontmatterParse(error) => {
                write!(formatter, "failed to parse template frontmatter: {error}")
            }
            Self::YamlSerialize(error) => {
                write!(formatter, "failed to serialize merged frontmatter: {error}")
            }
        }
    }
}

impl std::error::Error for TemplateInsertionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NoteFrontmatterParse(error)
            | Self::TemplateFrontmatterParse(error)
            | Self::YamlSerialize(error) => Some(error),
            Self::NoteFrontmatterNotMapping | Self::TemplateFrontmatterNotMapping => None,
        }
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct EditReport {
    path: String,
    created: bool,
    rescanned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DiffReport {
    path: String,
    anchor: String,
    source: String,
    status: String,
    changed: bool,
    changed_kinds: Vec<String>,
    diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct DataviewInlineReport {
    file: String,
    results: Vec<EvaluatedInlineExpression>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct DataviewEvalReport {
    file: String,
    blocks: Vec<DataviewBlockReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct DataviewBlockReport {
    block_index: usize,
    line_number: i64,
    language: String,
    source: String,
    result: Option<DqlQueryResult>,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TasksEvalReport {
    file: String,
    blocks: Vec<TasksBlockEvalReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TasksBlockEvalReport {
    block_index: usize,
    line_number: i64,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_source: Option<String>,
    result: Option<TasksQueryResult>,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TasksNextReport {
    reference_date: String,
    result_count: usize,
    occurrences: Vec<TasksNextOccurrence>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TasksNextOccurrence {
    date: String,
    sequence: usize,
    task: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TasksBlockedReport {
    tasks: Vec<TasksBlockedItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TasksBlockedItem {
    task: Value,
    blockers: Vec<TaskDependencyEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TasksGraphReport {
    nodes: Vec<TaskDependencyNode>,
    edges: Vec<TaskDependencyEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TaskDependencyNode {
    key: String,
    id: Option<String>,
    path: String,
    line: i64,
    text: String,
    completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TaskDependencyEdge {
    blocked_key: String,
    blocker_id: String,
    resolved: bool,
    blocker_key: Option<String>,
    blocker_path: Option<String>,
    blocker_line: Option<i64>,
    blocker_text: Option<String>,
    blocker_completed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct InboxReport {
    path: String,
    appended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TemplateListReport {
    templates: Vec<TemplateSummary>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TemplateCreateReport {
    template: String,
    template_source: String,
    path: String,
    opened_editor: bool,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TemplateInsertReport {
    template: String,
    template_source: String,
    note: String,
    mode: String,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TemplateSummary {
    name: String,
    source: String,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TemplateCandidate {
    name: String,
    source: &'static str,
    display_path: String,
    absolute_path: PathBuf,
    warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TemplateDiscovery {
    templates: Vec<TemplateCandidate>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplateInsertMode {
    Append,
    Prepend,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedTemplateInsertion {
    merged_frontmatter: Option<YamlMapping>,
    target_body: String,
    template_body: String,
}

#[derive(Debug)]
enum TemplateInsertionError {
    NoteFrontmatterNotMapping,
    NoteFrontmatterParse(serde_yaml::Error),
    TemplateFrontmatterNotMapping,
    TemplateFrontmatterParse(serde_yaml::Error),
    YamlSerialize(serde_yaml::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct OpenReport {
    path: String,
    uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TemplateVariables {
    title: String,
    date: String,
    time: String,
    datetime: String,
    uuid: String,
    timestamp: TemplateTimestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TemplateTimestamp {
    days_since_epoch: i64,
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
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

fn run_incremental_scan(
    paths: &VaultPaths,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<ScanSummary, CliError> {
    let mut progress =
        (output == OutputFormat::Human).then(|| ScanProgressReporter::new(use_stderr_color));
    scan_vault_with_progress(paths, ScanMode::Incremental, |event| {
        if let Some(progress) = progress.as_mut() {
            progress.record(&event);
        }
    })
    .map_err(CliError::operation)
}

fn refresh_mode_for_target(paths: &VaultPaths, cli: &Cli, target: RefreshTarget) -> AutoScanMode {
    if let Some(mode) = cli.refresh {
        return match mode {
            RefreshMode::Off => AutoScanMode::Off,
            RefreshMode::Blocking => AutoScanMode::Blocking,
            RefreshMode::Background => AutoScanMode::Background,
        };
    }

    let scan_config = load_vault_config(paths).config.scan;
    match target {
        RefreshTarget::Command => scan_config.default_mode,
        RefreshTarget::Browse => scan_config.browse_mode,
    }
}

fn command_uses_auto_refresh(command: &Command) -> bool {
    match command {
        Command::Backlinks { .. }
        | Command::Graph { .. }
        | Command::Open { .. }
        | Command::Cluster { .. }
        | Command::Doctor { .. }
        | Command::Move { .. }
        | Command::RenameProperty { .. }
        | Command::MergeTags { .. }
        | Command::RenameAlias { .. }
        | Command::RenameHeading { .. }
        | Command::RenameBlockRef { .. }
        | Command::Links { .. }
        | Command::Query { .. }
        | Command::Dataview { .. }
        | Command::Tasks { .. }
        | Command::Update { .. }
        | Command::Unset { .. }
        | Command::Notes { .. }
        | Command::Search { .. }
        | Command::Changes { .. }
        | Command::Diff { .. }
        | Command::LinkMentions { .. }
        | Command::Rewrite { .. }
        | Command::Related { .. } => true,
        Command::Edit { new, .. } => !new,
        Command::Browse { .. } => false,
        Command::Bases { command } => matches!(
            command,
            BasesCommand::Eval { .. } | BasesCommand::Tui { .. }
        ),
        Command::Suggest { .. } => true,
        Command::Saved { command } => matches!(command, SavedCommand::Run { .. }),
        Command::Checkpoint { .. } => true,
        Command::Export { command } => matches!(command, ExportCommand::SearchIndex { .. }),
        Command::Vectors { command } => matches!(
            command,
            VectorsCommand::Related { .. }
                | VectorsCommand::Neighbors { .. }
                | VectorsCommand::Duplicates { .. }
        ),
        Command::Init
        | Command::Rebuild { .. }
        | Command::Repair { .. }
        | Command::Serve { .. }
        | Command::Watch { .. }
        | Command::Completions { .. }
        | Command::Describe
        | Command::Cache { .. }
        | Command::Inbox { .. }
        | Command::Batch { .. }
        | Command::Automation { .. }
        | Command::Scan { .. } => false,
        Command::Template { command, .. } => {
            matches!(command, Some(TemplateSubcommand::Insert { .. }))
        }
    }
}

fn maybe_auto_refresh_command_cache(
    paths: &VaultPaths,
    cli: &Cli,
    use_stderr_color: bool,
) -> Result<(), CliError> {
    if !command_uses_auto_refresh(&cli.command) {
        return Ok(());
    }

    match refresh_mode_for_target(paths, cli, RefreshTarget::Command) {
        AutoScanMode::Off => Ok(()),
        AutoScanMode::Blocking | AutoScanMode::Background => {
            run_incremental_scan(paths, cli.output, use_stderr_color)?;
            Ok(())
        }
    }
}

fn warn_auto_commit_if_needed(policy: &AutoCommitPolicy) {
    if let Some(message) = policy.warning() {
        eprintln!("warning: {message}");
    }
}

fn refactor_changed_files(report: &RefactorReport) -> Vec<String> {
    report.files.iter().map(|file| file.path.clone()).collect()
}

fn bulk_mutation_changed_files(report: &BulkMutationReport) -> Vec<String> {
    report.files.iter().map(|file| file.path.clone()).collect()
}

fn move_changed_files(summary: &MoveSummary) -> Vec<String> {
    std::iter::once(summary.source_path.clone())
        .chain(std::iter::once(summary.destination_path.clone()))
        .chain(summary.rewritten_files.iter().map(|file| file.path.clone()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn resolve_edit_path(
    paths: &VaultPaths,
    cli: &Cli,
    stdout_is_tty: bool,
    use_stderr_color: bool,
    note: Option<&str>,
    new: bool,
) -> Result<(String, bool), CliError> {
    if new {
        let note = note.ok_or_else(|| {
            CliError::operation("`edit --new` requires a relative note path such as Notes/Idea.md")
        })?;
        let path = normalize_relative_input_path(
            note,
            RelativePathOptions {
                expected_extension: Some("md"),
                append_extension_if_missing: true,
            },
        )
        .map_err(CliError::operation)?;
        return Ok((path, true));
    }

    if !paths.cache_db().exists() {
        run_incremental_scan(paths, cli.output, use_stderr_color)?;
    }

    let interactive = interactive_note_selection_allowed(cli, stdout_is_tty);
    let note = resolve_note_argument(paths, note, interactive, "note")?;
    let resolved = resolve_note_reference(paths, &note).map_err(CliError::operation)?;
    Ok((resolved.path, false))
}

fn run_edit_command(
    paths: &VaultPaths,
    cli: &Cli,
    stdout_is_tty: bool,
    use_stderr_color: bool,
    note: Option<&str>,
    new: bool,
) -> Result<EditReport, CliError> {
    let (relative_path, creating_new_note) =
        resolve_edit_path(paths, cli, stdout_is_tty, use_stderr_color, note, new)?;
    let absolute_path = paths.vault_root().join(&relative_path);
    let mut created = false;
    if creating_new_note {
        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        if !absolute_path.exists() {
            fs::write(&absolute_path, "").map_err(CliError::operation)?;
            created = true;
        }
    } else if !absolute_path.is_file() {
        return Err(CliError::operation(format!(
            "note does not exist on disk: {relative_path}"
        )));
    }

    open_in_editor(&absolute_path).map_err(CliError::operation)?;
    run_incremental_scan(paths, cli.output, use_stderr_color)?;

    Ok(EditReport {
        path: relative_path,
        created,
        rescanned: true,
    })
}

fn run_diff_command(
    paths: &VaultPaths,
    note: Option<&str>,
    since: Option<&str>,
    interactive_note_selection: bool,
) -> Result<DiffReport, CliError> {
    let note = resolve_note_argument(paths, note, interactive_note_selection, "note")?;
    let resolved = resolve_note_reference(paths, &note).map_err(CliError::operation)?;

    if let Some(checkpoint) = since {
        return diff_report_from_change_anchor(
            paths,
            &resolved.path,
            ChangeAnchor::Checkpoint(checkpoint.to_string()),
            format!("checkpoint:{checkpoint}"),
        );
    }

    if vulcan_core::is_git_repo(paths.vault_root()) {
        return diff_report_from_git(paths, &resolved.path);
    }

    diff_report_from_change_anchor(
        paths,
        &resolved.path,
        ChangeAnchor::LastScan,
        "last_scan".to_string(),
    )
}

fn diff_report_from_git(paths: &VaultPaths, path: &str) -> Result<DiffReport, CliError> {
    let status = git_status(paths.vault_root()).map_err(CliError::operation)?;
    let untracked = status.untracked.iter().any(|candidate| candidate == path);
    let diff = render_git_diff(paths.vault_root(), path, untracked)?;
    let changed = !diff.trim().is_empty();

    Ok(DiffReport {
        path: path.to_string(),
        anchor: "HEAD".to_string(),
        source: "git_head".to_string(),
        status: if untracked {
            "new".to_string()
        } else if changed {
            "changed".to_string()
        } else {
            "unchanged".to_string()
        },
        changed,
        changed_kinds: changed
            .then(|| vec!["note".to_string()])
            .unwrap_or_default(),
        diff: changed.then_some(diff),
    })
}

fn diff_report_from_change_anchor(
    paths: &VaultPaths,
    path: &str,
    anchor: ChangeAnchor,
    anchor_label: String,
) -> Result<DiffReport, CliError> {
    let report = query_change_report(paths, &anchor).map_err(CliError::operation)?;
    let mut changed_kinds = Vec::new();
    let note_status = report
        .notes
        .iter()
        .find(|item| item.path == path)
        .map(|item| item.status);

    if note_status.is_some() {
        changed_kinds.push("note".to_string());
    }
    if report.links.iter().any(|item| item.path == path) {
        changed_kinds.push("links".to_string());
    }
    if report.properties.iter().any(|item| item.path == path) {
        changed_kinds.push("properties".to_string());
    }
    if report.embeddings.iter().any(|item| item.path == path) {
        changed_kinds.push("embeddings".to_string());
    }

    let status = match note_status {
        Some(ChangeKindStatus::Added) => "new",
        Some(ChangeKindStatus::Updated) => "changed",
        Some(ChangeKindStatus::Deleted) => "deleted",
        None if changed_kinds.is_empty() => "unchanged",
        None => "changed",
    }
    .to_string();

    Ok(DiffReport {
        path: path.to_string(),
        anchor: anchor_label,
        source: "cache".to_string(),
        changed: status != "unchanged",
        status,
        changed_kinds,
        diff: None,
    })
}

fn run_dataview_inline_command(
    paths: &VaultPaths,
    file: &str,
) -> Result<DataviewInlineReport, CliError> {
    let resolved = resolve_note_reference(paths, file).map_err(CliError::operation)?;
    let note_index = load_note_index(paths).map_err(CliError::operation)?;
    let note = note_index
        .values()
        .find(|note| note.document_path == resolved.path)
        .ok_or_else(|| CliError::operation(format!("note is not indexed: {}", resolved.path)))?;
    let results = evaluate_note_inline_expressions(note, &note_index);

    Ok(DataviewInlineReport {
        file: resolved.path,
        results,
    })
}

fn run_dataview_query_command(paths: &VaultPaths, dql: &str) -> Result<DqlQueryResult, CliError> {
    evaluate_dql(paths, dql, None).map_err(CliError::operation)
}

fn run_dataview_eval_command(
    paths: &VaultPaths,
    file: &str,
    block: Option<usize>,
) -> Result<DataviewEvalReport, CliError> {
    let blocks = load_dataview_blocks(paths, file, block).map_err(CliError::operation)?;
    let file = blocks
        .first()
        .map(|block| block.file.clone())
        .unwrap_or_else(|| file.to_string());
    let mut reports = Vec::with_capacity(blocks.len());

    for block in blocks {
        let (result, error) = if block.language == "dataview" {
            match evaluate_dql(paths, &block.source, Some(&block.file)) {
                Ok(result) => (Some(result), None),
                Err(error) => (None, Some(error.to_string())),
            }
        } else {
            (
                None,
                Some("DataviewJS blocks require the `dataviewjs` feature flag".to_string()),
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

fn run_tasks_query_command(paths: &VaultPaths, source: &str) -> Result<TasksQueryResult, CliError> {
    let config = load_vault_config(paths).config.tasks;
    let effective_source = tasks_query_source(&config, source, false);
    let mut result = evaluate_tasks_query(paths, &effective_source).map_err(CliError::operation)?;
    strip_global_filter_from_output(&mut result, &config);
    Ok(result)
}

fn run_tasks_eval_command(
    paths: &VaultPaths,
    file: &str,
    block: Option<usize>,
) -> Result<TasksEvalReport, CliError> {
    let config = load_vault_config(paths).config.tasks;
    let blocks = load_tasks_blocks(paths, file, block).map_err(CliError::operation)?;
    let file = blocks
        .first()
        .map(|block| block.file.clone())
        .unwrap_or_else(|| file.to_string());
    let mut reports = Vec::with_capacity(blocks.len());

    for block in blocks {
        let effective_source = tasks_query_source(&config, &block.source, true);
        let effective_source_override =
            (effective_source != block.source).then(|| effective_source.clone());
        let (mut result, error) = match evaluate_tasks_query(paths, &effective_source) {
            Ok(result) => (Some(result), None),
            Err(error) => (None, Some(error.to_string())),
        };
        if let Some(result) = result.as_mut() {
            strip_global_filter_from_output(result, &config);
        }

        reports.push(TasksBlockEvalReport {
            block_index: block.block_index,
            line_number: block.line_number,
            source: block.source,
            effective_source: effective_source_override,
            result,
            error,
        });
    }

    Ok(TasksEvalReport {
        file,
        blocks: reports,
    })
}

fn run_tasks_list_command(
    paths: &VaultPaths,
    filter: Option<&str>,
) -> Result<TasksQueryResult, CliError> {
    let config = load_vault_config(paths).config.tasks;
    let Some(filter) = filter.map(str::trim).filter(|filter| !filter.is_empty()) else {
        return run_tasks_query_command(paths, "");
    };

    match parse_tasks_query(filter) {
        Ok(_) => run_tasks_query_command(paths, filter),
        Err(tasks_error) => run_tasks_list_dql_filter(paths, filter, tasks_error, &config),
    }
}

fn run_tasks_next_command(
    paths: &VaultPaths,
    count: usize,
    from: Option<&str>,
) -> Result<TasksNextReport, CliError> {
    let (reference_date, reference_ms) = resolve_tasks_reference_date(from)?;
    let result = run_tasks_query_command(paths, "is recurring")?;
    let mut occurrences = Vec::new();

    for task in result.tasks {
        let Value::Object(task_object) = task.clone() else {
            continue;
        };

        for (sequence, date) in task_upcoming_occurrences(&task_object, reference_ms, count)
            .into_iter()
            .enumerate()
        {
            occurrences.push(TasksNextOccurrence {
                date,
                sequence: sequence.saturating_add(1),
                task: task.clone(),
            });
        }
    }

    occurrences.sort_by(|left, right| {
        left.date
            .cmp(&right.date)
            .then_with(|| task_sort_key(&left.task).cmp(&task_sort_key(&right.task)))
            .then_with(|| left.sequence.cmp(&right.sequence))
    });
    occurrences.truncate(count);

    Ok(TasksNextReport {
        reference_date,
        result_count: occurrences.len(),
        occurrences,
    })
}

fn run_tasks_list_dql_filter(
    paths: &VaultPaths,
    filter: &str,
    tasks_error: String,
    config: &vulcan_core::config::TasksConfig,
) -> Result<TasksQueryResult, CliError> {
    let expression_source = tasks_dql_filter_expression(config, filter);
    let expression = parse_expression(&expression_source).map_err(|expression_error| {
        CliError::operation(format!(
            "failed to parse filter as Tasks DSL ({tasks_error}); failed to parse as Dataview expression ({expression_error})"
        ))
    })?;

    let base_source = tasks_query_source(config, "", false);
    let base_result = evaluate_tasks_query(paths, &base_source).map_err(CliError::operation)?;
    let note_index = load_note_index(paths).map_err(CliError::operation)?;
    let note_by_path = note_index
        .values()
        .map(|note| (note.document_path.as_str(), note))
        .collect::<HashMap<_, _>>();
    let formulas = BTreeMap::new();
    let mut tasks = Vec::new();

    for task in base_result.tasks {
        let Some(path) = task.get("path").and_then(Value::as_str) else {
            continue;
        };
        let Some(note) = note_by_path.get(path) else {
            continue;
        };
        let Value::Object(task_fields) = task.clone() else {
            continue;
        };

        let mut scoped_note = (*note).clone();
        scoped_note.properties = Value::Object(task_fields);
        let context = EvalContext::new(&scoped_note, &formulas).with_note_lookup(&note_index);
        let value = evaluate_expression(&expression, &context).map_err(|error| {
            CliError::operation(format!(
                "failed to evaluate Dataview expression for {path}: {error}"
            ))
        })?;
        if is_truthy(&value) {
            tasks.push(task);
        }
    }

    let mut result = TasksQueryResult {
        result_count: tasks.len(),
        tasks,
        groups: Vec::new(),
        hidden_fields: Vec::new(),
        shown_fields: Vec::new(),
        short_mode: false,
        plan: None,
    };
    strip_global_filter_from_output(&mut result, config);
    Ok(result)
}

fn resolve_tasks_reference_date(from: Option<&str>) -> Result<(String, i64), CliError> {
    let reference_date = from
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| TemplateTimestamp::current().default_date_string());
    let reference_ms = parse_date_like_string(&reference_date).ok_or_else(|| {
        CliError::operation(format!(
            "failed to parse recurrence reference date: {reference_date}"
        ))
    })?;

    Ok((day_string_from_ms(reference_ms), reference_ms))
}

fn day_string_from_ms(ms: i64) -> String {
    let (year, month, day, _, _, _, _) = date_components(ms);
    format!("{year:04}-{month:02}-{day:02}")
}

fn tasks_query_source(
    config: &vulcan_core::config::TasksConfig,
    source: &str,
    include_global_query: bool,
) -> String {
    let mut sections = Vec::new();
    if let Some(tag) = config
        .global_filter
        .as_deref()
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
    {
        sections.push(format!("tag includes {tag}"));
    }
    if include_global_query {
        if let Some(query) = config
            .global_query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
        {
            sections.push(query.to_string());
        }
    }
    if !source.trim().is_empty() {
        sections.push(source.trim().to_string());
    }
    sections.join("\n")
}

fn tasks_dql_filter_expression(config: &vulcan_core::config::TasksConfig, filter: &str) -> String {
    let mut clauses = Vec::new();
    if let Some(tag) = config
        .global_filter
        .as_deref()
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
    {
        let quoted = serde_json::to_string(tag).expect("task filter tag should serialize");
        clauses.push(format!("contains(tags, {quoted})"));
    }
    clauses.push(format!("({})", filter.trim()));
    clauses.join(" && ")
}

fn strip_global_filter_from_output(
    result: &mut TasksQueryResult,
    config: &vulcan_core::config::TasksConfig,
) {
    if !config.remove_global_filter {
        return;
    }
    let Some(global_filter) = config
        .global_filter
        .as_deref()
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
    else {
        return;
    };

    let normalized = normalize_tag_name(global_filter);
    for task in &mut result.tasks {
        strip_task_global_filter(task, global_filter, &normalized);
    }
    for group in &mut result.groups {
        for task in &mut group.tasks {
            strip_task_global_filter(task, global_filter, &normalized);
        }
    }
}

fn strip_task_global_filter(task: &mut Value, raw_tag: &str, normalized_tag: &str) {
    let Some(object) = task.as_object_mut() else {
        return;
    };

    if let Some(Value::Array(tags)) = object.get_mut("tags") {
        tags.retain(|tag| {
            tag.as_str()
                .map(|tag| normalize_tag_name(tag) != normalized_tag)
                .unwrap_or(true)
        });
    }

    for field in ["text", "visual"] {
        if let Some(Value::String(text)) = object.get_mut(field) {
            *text = strip_tag_from_text(text, raw_tag, normalized_tag);
        }
    }

    if let Some(Value::Array(children)) = object.get_mut("children") {
        for child in children {
            strip_task_global_filter(child, raw_tag, normalized_tag);
        }
    }
}

fn strip_tag_from_text(text: &str, raw_tag: &str, normalized_tag: &str) -> String {
    text.split_whitespace()
        .filter(|token| {
            !token.eq_ignore_ascii_case(raw_tag) && normalize_tag_name(token) != normalized_tag
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_tag_name(tag: &str) -> String {
    tag.trim().trim_start_matches('#').to_ascii_lowercase()
}

fn run_tasks_blocked_command(paths: &VaultPaths) -> Result<TasksBlockedReport, CliError> {
    let graph = build_tasks_graph_report(paths)?;
    let task_result = run_tasks_query_command(paths, "")?;
    let tasks_by_key = task_result
        .tasks
        .into_iter()
        .filter_map(|task| task_dependency_key(&task).map(|key| (key, task)))
        .collect::<HashMap<_, _>>();

    let mut blockers_by_task = HashMap::<String, Vec<TaskDependencyEdge>>::new();
    for edge in graph.edges {
        if !edge.resolved || edge.blocker_completed != Some(true) {
            blockers_by_task
                .entry(edge.blocked_key.clone())
                .or_default()
                .push(edge);
        }
    }

    let mut tasks = blockers_by_task
        .into_iter()
        .filter_map(|(key, blockers)| {
            tasks_by_key
                .get(&key)
                .cloned()
                .map(|task| TasksBlockedItem { task, blockers })
        })
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| task_sort_key(&left.task).cmp(&task_sort_key(&right.task)));

    Ok(TasksBlockedReport { tasks })
}

fn build_tasks_graph_report(paths: &VaultPaths) -> Result<TasksGraphReport, CliError> {
    let result = run_tasks_query_command(paths, "")?;
    let mut tasks = result
        .tasks
        .into_iter()
        .filter_map(|task| {
            let key = task_dependency_key(&task)?;
            Some((key, task))
        })
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| task_sort_key(&left.1).cmp(&task_sort_key(&right.1)));

    let mut node_by_id = HashMap::<String, TaskDependencyNode>::new();
    let mut nodes = Vec::with_capacity(tasks.len());
    for (key, task) in &tasks {
        let node = TaskDependencyNode {
            key: key.clone(),
            id: task
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .filter(|id| !id.trim().is_empty()),
            path: task
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            line: task.get("line").and_then(Value::as_i64).unwrap_or_default(),
            text: task
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            completed: task
                .get("completed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        if let Some(id) = node.id.clone() {
            node_by_id.entry(id).or_insert_with(|| node.clone());
        }
        nodes.push(node);
    }

    let mut edges = tasks
        .iter()
        .filter_map(|(key, task)| {
            let blocker_id = task
                .get("blocked-by")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())?;
            let blocker = node_by_id.get(blocker_id);
            Some(TaskDependencyEdge {
                blocked_key: key.clone(),
                blocker_id: blocker_id.to_string(),
                resolved: blocker.is_some(),
                blocker_key: blocker.map(|node| node.key.clone()),
                blocker_path: blocker.map(|node| node.path.clone()),
                blocker_line: blocker.map(|node| node.line),
                blocker_text: blocker.map(|node| node.text.clone()),
                blocker_completed: blocker.map(|node| node.completed),
            })
        })
        .collect::<Vec<_>>();
    edges.sort_by(|left, right| {
        left.blocked_key
            .cmp(&right.blocked_key)
            .then_with(|| left.blocker_id.cmp(&right.blocker_id))
    });

    Ok(TasksGraphReport { nodes, edges })
}

fn task_dependency_key(task: &Value) -> Option<String> {
    let path = task.get("path").and_then(Value::as_str)?;
    let line = task.get("line").and_then(Value::as_i64).unwrap_or_default();
    Some(
        task.get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("{path}:{line}")),
    )
}

fn task_sort_key(task: &Value) -> (String, i64) {
    (
        task.get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        task.get("line").and_then(Value::as_i64).unwrap_or_default(),
    )
}

type ChangeKindStatus = vulcan_core::ChangeStatus;

fn render_git_diff(vault_root: &Path, path: &str, untracked: bool) -> Result<String, CliError> {
    let output = if untracked {
        let empty_path = std::env::temp_dir().join(format!(
            "vulcan-empty-diff-{}-{}",
            std::process::id(),
            path.replace('/', "_")
        ));
        fs::write(&empty_path, "").map_err(CliError::operation)?;
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(vault_root)
            .args(["diff", "--no-index", "--no-color"])
            .arg(&empty_path)
            .arg(vault_root.join(path))
            .output()
            .map_err(CliError::operation)?;
        let _ = fs::remove_file(&empty_path);
        output
    } else {
        ProcessCommand::new("git")
            .arg("-C")
            .arg(vault_root)
            .args(["diff", "--no-color", "HEAD", "--", path])
            .output()
            .map_err(CliError::operation)?
    };

    if untracked {
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Err(CliError::operation(String::from_utf8_lossy(&output.stderr)));
        }
    } else if !output.status.success() {
        return Err(CliError::operation(String::from_utf8_lossy(&output.stderr)));
    }

    String::from_utf8(output.stdout).map_err(CliError::operation)
}

fn run_inbox_command(
    paths: &VaultPaths,
    text: Option<&str>,
    file: Option<&PathBuf>,
    no_commit: bool,
) -> Result<InboxReport, CliError> {
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit);
    let inbox_config = vulcan_core::load_vault_config(paths).config.inbox;
    let relative_path = normalize_relative_input_path(
        &inbox_config.path,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(CliError::operation)?;

    let raw_text = inbox_input_text(text, file)?;
    let variables = template_variables_for_path(&relative_path, TemplateTimestamp::current());
    let rendered_entry = render_inbox_entry(&inbox_config.format, &raw_text, &variables);
    let entry = if inbox_config.timestamp {
        format!("{} {}", variables.datetime, rendered_entry)
    } else {
        rendered_entry
    };

    let absolute_path = paths.vault_root().join(&relative_path);
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    let existing = fs::read_to_string(&absolute_path).unwrap_or_default();
    let updated = if let Some(heading) = inbox_config.heading.as_deref() {
        append_under_heading(&existing, heading, &entry)
    } else {
        append_at_end(&existing, &entry)
    };
    fs::write(&absolute_path, updated).map_err(CliError::operation)?;
    run_incremental_scan(paths, OutputFormat::Human, false)?;
    auto_commit
        .commit(paths, "inbox", std::slice::from_ref(&relative_path))
        .map_err(CliError::operation)?;

    Ok(InboxReport {
        path: relative_path,
        appended: true,
    })
}

fn run_template_command(
    paths: &VaultPaths,
    name: Option<&str>,
    list: bool,
    output_path: Option<&str>,
    no_commit: bool,
    stdout_is_tty: bool,
) -> Result<TemplateCommandResult, CliError> {
    let config = load_vault_config(paths).config;
    let templates = discover_templates(paths, config.templates.obsidian_folder.as_deref())?;
    if list {
        return Ok(TemplateCommandResult::List(TemplateListReport {
            templates: templates
                .templates
                .iter()
                .map(|template| TemplateSummary {
                    name: template.name.clone(),
                    source: template.source.to_string(),
                    path: template.display_path.clone(),
                })
                .collect(),
            warnings: templates.warnings,
        }));
    }

    let template_name = name.ok_or_else(|| {
        CliError::operation("`template` requires a template name unless --list is used")
    })?;
    let now = TemplateTimestamp::current();
    let template = resolve_template_file(paths, &templates.templates, template_name)?;
    let output_path = template_output_path(&template.name, output_path, &now)?;
    let absolute_output = paths.vault_root().join(&output_path);
    if absolute_output.exists() {
        return Err(CliError::operation(format!(
            "destination note already exists: {output_path}"
        )));
    }

    let rendered = render_template_contents(
        &fs::read_to_string(&template.absolute_path).map_err(CliError::operation)?,
        &template_variables_for_path(&output_path, now),
        &config.templates,
    );
    if let Some(parent) = absolute_output.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    fs::write(&absolute_output, rendered).map_err(CliError::operation)?;

    let mut opened_editor = false;
    if stdout_is_tty && io::stdin().is_terminal() {
        open_in_editor(&absolute_output).map_err(CliError::operation)?;
        opened_editor = true;
    }

    run_incremental_scan(paths, OutputFormat::Human, false)?;
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit);
    auto_commit
        .commit(paths, "template", std::slice::from_ref(&output_path))
        .map_err(CliError::operation)?;

    Ok(TemplateCommandResult::Create(TemplateCreateReport {
        template: template.name,
        template_source: template.source.to_string(),
        path: output_path,
        opened_editor,
        warnings: template.warning.into_iter().collect(),
    }))
}

fn run_template_insert_command(
    paths: &VaultPaths,
    template_name: &str,
    note: Option<&str>,
    mode: TemplateInsertMode,
    no_commit: bool,
    interactive_note_selection: bool,
) -> Result<TemplateInsertReport, CliError> {
    let config = load_vault_config(paths).config;
    let templates = discover_templates(paths, config.templates.obsidian_folder.as_deref())?;
    let template = resolve_template_file(paths, &templates.templates, template_name)?;
    let target_identifier = resolve_note_argument(
        paths,
        note,
        interactive_note_selection,
        "template insert target note",
    )?;
    let resolved =
        resolve_note_reference(paths, &target_identifier).map_err(CliError::operation)?;
    let target_path = resolved.path;
    let target_absolute = paths.vault_root().join(&target_path);
    let target_source = fs::read_to_string(&target_absolute).map_err(CliError::operation)?;
    let rendered_template = render_template_contents(
        &fs::read_to_string(&template.absolute_path).map_err(CliError::operation)?,
        &template_variables_for_path(&target_path, TemplateTimestamp::current()),
        &config.templates,
    );
    let updated = apply_template_insertion_mode(
        prepare_template_insertion(&target_source, &rendered_template)
            .map_err(CliError::operation)?,
        mode,
    )
    .map_err(CliError::operation)?;
    fs::write(&target_absolute, updated).map_err(CliError::operation)?;

    run_incremental_scan(paths, OutputFormat::Human, false)?;
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit);
    auto_commit
        .commit(paths, "template insert", std::slice::from_ref(&target_path))
        .map_err(CliError::operation)?;

    Ok(TemplateInsertReport {
        template: template.name,
        template_source: template.source.to_string(),
        note: target_path,
        mode: mode.as_str().to_string(),
        warnings: template.warning.into_iter().collect(),
    })
}

enum TemplateCommandResult {
    List(TemplateListReport),
    Create(TemplateCreateReport),
    Insert(TemplateInsertReport),
}

fn discover_templates(
    paths: &VaultPaths,
    obsidian_folder: Option<&Path>,
) -> Result<TemplateDiscovery, CliError> {
    let mut warnings = Vec::new();
    let mut templates = list_templates_in_directory(
        paths.vulcan_dir().join("templates"),
        ".vulcan/templates",
        "vulcan",
    )?;
    let mut obsidian_templates = obsidian_folder
        .filter(|folder| !folder.as_os_str().is_empty())
        .map(|folder| {
            list_templates_in_directory(
                paths.vault_root().join(folder),
                &folder.to_string_lossy(),
                "obsidian",
            )
        })
        .transpose()?
        .unwrap_or_default();

    for obsidian_template in obsidian_templates.drain(..) {
        if let Some(existing) = templates
            .iter_mut()
            .find(|template| template.name == obsidian_template.name)
        {
            let warning = format!(
                "template {} exists in both {} and {}; using {}",
                existing.name,
                obsidian_template.display_path,
                existing.display_path,
                existing.display_path
            );
            existing.warning = Some(warning.clone());
            warnings.push(warning);
        } else {
            templates.push(obsidian_template);
        }
    }

    templates.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(TemplateDiscovery {
        templates,
        warnings,
    })
}

fn resolve_template_file(
    paths: &VaultPaths,
    templates: &[TemplateCandidate],
    name: &str,
) -> Result<TemplateCandidate, CliError> {
    if let Some(template) = templates
        .iter()
        .find(|template| template.name == name || template.name.trim_end_matches(".md") == name)
    {
        return Ok(template.clone());
    }

    let mut searched = vec![paths.vulcan_dir().join("templates").display().to_string()];
    let obsidian_folder = load_vault_config(paths).config.templates.obsidian_folder;
    if let Some(folder) = obsidian_folder.filter(|folder| !folder.as_os_str().is_empty()) {
        searched.push(paths.vault_root().join(folder).display().to_string());
    }

    Err(CliError::operation(format!(
        "template not found in {}: {name}",
        searched.join(", ")
    )))
}

fn list_templates_in_directory(
    template_dir: PathBuf,
    display_root: &str,
    source: &'static str,
) -> Result<Vec<TemplateCandidate>, CliError> {
    if !template_dir.exists() {
        return Ok(Vec::new());
    }

    let mut templates = fs::read_dir(&template_dir)
        .map_err(CliError::operation)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            (path.extension().and_then(|ext| ext.to_str()) == Some("md")).then(|| {
                let name = path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .map(ToOwned::to_owned)?;
                Some(TemplateCandidate {
                    display_path: format!("{display_root}/{name}"),
                    name,
                    source,
                    absolute_path: path,
                    warning: None,
                })
            })?
        })
        .collect::<Vec<_>>();
    templates.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(templates)
}

fn template_output_path(
    template_file: &str,
    output_path: Option<&str>,
    now: &TemplateTimestamp,
) -> Result<String, CliError> {
    let path = if let Some(path) = output_path {
        path.to_string()
    } else {
        let date = now.default_date_string();
        format!("{date}-{}", template_file)
    };

    normalize_relative_input_path(
        &path,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(CliError::operation)
}

fn prepare_template_insertion(
    target_source: &str,
    rendered_template: &str,
) -> Result<PreparedTemplateInsertion, TemplateInsertionError> {
    let (target_frontmatter, target_body) = parse_frontmatter_document(target_source, false)?;
    let (template_frontmatter, template_body) =
        parse_frontmatter_document(rendered_template, true)?;

    Ok(PreparedTemplateInsertion {
        merged_frontmatter: merge_template_frontmatter(target_frontmatter, template_frontmatter),
        target_body,
        template_body,
    })
}

fn parse_frontmatter_document(
    source: &str,
    template_document: bool,
) -> Result<(Option<YamlMapping>, String), TemplateInsertionError> {
    let Some((yaml_start, yaml_end, body_start)) = find_frontmatter_block(source) else {
        return Ok((None, source.to_string()));
    };

    let raw_yaml = &source[yaml_start..yaml_end];
    let value = serde_yaml::from_str::<YamlValue>(raw_yaml).map_err(|error| {
        if template_document {
            TemplateInsertionError::TemplateFrontmatterParse(error)
        } else {
            TemplateInsertionError::NoteFrontmatterParse(error)
        }
    })?;
    let mapping = value.as_mapping().cloned().ok_or_else(|| {
        if template_document {
            TemplateInsertionError::TemplateFrontmatterNotMapping
        } else {
            TemplateInsertionError::NoteFrontmatterNotMapping
        }
    })?;

    Ok((Some(mapping), source[body_start..].to_string()))
}

fn merge_template_frontmatter(
    target_frontmatter: Option<YamlMapping>,
    template_frontmatter: Option<YamlMapping>,
) -> Option<YamlMapping> {
    match (target_frontmatter, template_frontmatter) {
        (None, None) => None,
        (Some(target), None) => Some(target),
        (None, Some(template)) => Some(template),
        (Some(mut target), Some(template)) => {
            for (key, template_value) in template {
                if let Some(existing_value) = target.get_mut(&key) {
                    merge_template_property_value(existing_value, &template_value);
                } else {
                    target.insert(key, template_value);
                }
            }
            Some(target)
        }
    }
}

fn merge_template_property_value(existing: &mut YamlValue, template: &YamlValue) {
    if let (Some(existing_items), Some(template_items)) =
        (existing.as_sequence_mut(), template.as_sequence())
    {
        for template_item in template_items {
            if !existing_items.iter().any(|item| item == template_item) {
                existing_items.push(template_item.clone());
            }
        }
    }
}

fn render_note_from_parts(
    frontmatter: &Option<YamlMapping>,
    body: &str,
) -> Result<String, TemplateInsertionError> {
    let mut rendered = String::new();
    if let Some(frontmatter) = frontmatter {
        rendered.push_str(&format_frontmatter_block(frontmatter)?);
    }
    rendered.push_str(body);
    Ok(rendered)
}

fn apply_template_insertion_mode(
    prepared: PreparedTemplateInsertion,
    mode: TemplateInsertMode,
) -> Result<String, TemplateInsertionError> {
    let body = match mode {
        TemplateInsertMode::Append => {
            append_template_body(&prepared.target_body, &prepared.template_body)
        }
        TemplateInsertMode::Prepend => {
            prepend_template_body(&prepared.target_body, &prepared.template_body)
        }
    };

    render_note_from_parts(&prepared.merged_frontmatter, &body)
}

fn append_template_body(target_body: &str, template_body: &str) -> String {
    merge_body_sections(target_body, template_body, false)
}

fn prepend_template_body(target_body: &str, template_body: &str) -> String {
    merge_body_sections(template_body, target_body, true)
}

fn merge_body_sections(first: &str, second: &str, preserve_second_leading_space: bool) -> String {
    let first = first.trim_end_matches('\n');
    let second = if preserve_second_leading_space {
        second.trim_end_matches('\n')
    } else {
        second.trim_matches('\n')
    };

    match (first.is_empty(), second.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("{first}\n"),
        (true, false) => format!("{second}\n"),
        (false, false) => format!("{first}\n\n{second}\n"),
    }
}

impl TemplateInsertMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Append => "append",
            Self::Prepend => "prepend",
        }
    }
}

fn format_frontmatter_block(frontmatter: &YamlMapping) -> Result<String, TemplateInsertionError> {
    let mut yaml = serde_yaml::to_string(&YamlValue::Mapping(frontmatter.clone()))
        .map_err(TemplateInsertionError::YamlSerialize)?;
    if let Some(stripped) = yaml.strip_prefix("---\n") {
        yaml = stripped.to_string();
    }
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }
    Ok(format!("---\n{yaml}---\n"))
}

fn find_frontmatter_block(source: &str) -> Option<(usize, usize, usize)> {
    let mut lines = source.split_inclusive('\n');
    let first_line = lines.next()?;
    if !matches!(first_line, "---\n" | "---\r\n" | "---") {
        return None;
    }

    let yaml_start = first_line.len();
    let mut offset = yaml_start;
    for line in lines {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            return Some((yaml_start, offset, offset + line.len()));
        }
        offset += line.len();
    }

    None
}

fn inbox_input_text(text: Option<&str>, file: Option<&PathBuf>) -> Result<String, CliError> {
    if let Some(file) = file {
        return fs::read_to_string(file).map_err(CliError::operation);
    }

    match text {
        Some("-") => {
            let mut buffer = String::new();
            io::stdin()
                .read_to_string(&mut buffer)
                .map_err(CliError::operation)?;
            Ok(buffer)
        }
        Some(text) => Ok(text.to_string()),
        None => Err(CliError::operation(
            "`inbox` requires text, `-`, or --file <path>",
        )),
    }
}

fn render_inbox_entry(format: &str, text: &str, variables: &TemplateVariables) -> String {
    format
        .replace("{text}", text.trim_end())
        .replace("{date}", &variables.date)
        .replace("{time}", &variables.time)
        .replace("{datetime}", &variables.datetime)
}

fn render_template_contents(
    template: &str,
    variables: &TemplateVariables,
    config: &TemplatesConfig,
) -> String {
    let mut rendered = String::with_capacity(template.len());
    let mut remaining = template;

    while let Some(start) = remaining.find("{{") {
        rendered.push_str(&remaining[..start]);
        let rest = &remaining[start + 2..];
        let Some(end) = rest.find("}}") else {
            rendered.push_str(&remaining[start..]);
            return rendered;
        };

        let expression = rest[..end].trim();
        let replacement =
            render_template_variable(expression, variables, config).unwrap_or_else(|| {
                let mut original = String::with_capacity(expression.len() + 4);
                original.push_str("{{");
                original.push_str(expression);
                original.push_str("}}");
                original
            });
        rendered.push_str(&replacement);
        remaining = &rest[end + 2..];
    }

    rendered.push_str(remaining);
    rendered
}

fn render_template_variable(
    expression: &str,
    variables: &TemplateVariables,
    config: &TemplatesConfig,
) -> Option<String> {
    if let Some(format) = expression.strip_prefix("date:") {
        return Some(variables.timestamp.format_obsidian(format.trim()));
    }
    if let Some(format) = expression.strip_prefix("time:") {
        return Some(variables.timestamp.format_obsidian(format.trim()));
    }

    match expression {
        "title" => Some(variables.title.clone()),
        "date" => Some(
            variables
                .timestamp
                .format_obsidian(config.date_format.trim()),
        ),
        "time" => Some(
            variables
                .timestamp
                .format_obsidian(config.time_format.trim()),
        ),
        "datetime" => Some(variables.datetime.clone()),
        "uuid" => Some(variables.uuid.clone()),
        _ => None,
    }
}

fn template_variables_for_path(path: &str, timestamp: TemplateTimestamp) -> TemplateVariables {
    let path = Path::new(path);
    let title = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Untitled")
        .to_string();
    let strings = timestamp.default_strings();

    TemplateVariables {
        title,
        date: strings.date,
        time: strings.time,
        datetime: strings.datetime,
        uuid: generated_uuid_string(),
        timestamp,
    }
}

struct TimestampStrings {
    date: String,
    time: String,
    datetime: String,
}

impl TemplateTimestamp {
    fn current() -> Self {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let days_since_epoch = seconds.div_euclid(86_400);
        let seconds_of_day = seconds.rem_euclid(86_400);
        let hour = seconds_of_day / 3_600;
        let minute = (seconds_of_day % 3_600) / 60;
        let second = seconds_of_day % 60;
        let (year, month, day) = civil_from_days(days_since_epoch);

        Self {
            days_since_epoch,
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }

    fn default_strings(self) -> TimestampStrings {
        let date = self.default_date_string();
        let time = format!("{:02}:{:02}:{:02}Z", self.hour, self.minute, self.second);
        let datetime = format!("{date}T{time}");

        TimestampStrings {
            date,
            time,
            datetime,
        }
    }

    fn default_date_string(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    fn weekday_index(self) -> usize {
        usize::try_from((self.days_since_epoch + 4).rem_euclid(7)).unwrap_or(0)
    }

    fn format_obsidian(self, format: &str) -> String {
        let format = if format.is_empty() {
            "YYYY-MM-DD"
        } else {
            format
        };
        let mut rendered = String::with_capacity(format.len());
        let mut remaining = format;

        while !remaining.is_empty() {
            if let Some(token) = self.next_obsidian_token(remaining) {
                rendered.push_str(&self.token_value(token));
                remaining = &remaining[token.len()..];
            } else if let Some(character) = remaining.chars().next() {
                rendered.push(character);
                remaining = &remaining[character.len_utf8()..];
            } else {
                break;
            }
        }

        rendered
    }

    fn next_obsidian_token<'a>(self, input: &'a str) -> Option<&'static str> {
        const TOKENS: [&str; 19] = [
            "YYYY", "dddd", "MMMM", "MMM", "ddd", "Do", "YY", "MM", "DD", "dd", "HH", "hh", "mm",
            "ss", "M", "D", "H", "h", "A",
        ];

        for token in TOKENS {
            if input.starts_with(token) {
                return Some(token);
            }
        }
        if input.starts_with('m') {
            return Some("m");
        }
        if input.starts_with('s') {
            return Some("s");
        }
        if input.starts_with('a') {
            return Some("a");
        }
        None
    }

    fn token_value(self, token: &str) -> String {
        const MONTH_NAMES: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        const MONTH_ABBREVIATIONS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        const WEEKDAY_NAMES: [&str; 7] = [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ];
        const WEEKDAY_ABBREVIATIONS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const WEEKDAY_SHORT: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

        let month_index = usize::try_from(self.month.saturating_sub(1)).unwrap_or(0);
        let weekday_index = self.weekday_index();
        let hour_12 = match self.hour % 12 {
            0 => 12,
            hour => hour,
        };

        match token {
            "YYYY" => format!("{:04}", self.year),
            "YY" => format!("{:02}", self.year.rem_euclid(100)),
            "MMMM" => MONTH_NAMES[month_index].to_string(),
            "MMM" => MONTH_ABBREVIATIONS[month_index].to_string(),
            "MM" => format!("{:02}", self.month),
            "M" => self.month.to_string(),
            "DD" => format!("{:02}", self.day),
            "Do" => ordinal_day(self.day),
            "D" => self.day.to_string(),
            "dddd" => WEEKDAY_NAMES[weekday_index].to_string(),
            "ddd" => WEEKDAY_ABBREVIATIONS[weekday_index].to_string(),
            "dd" => WEEKDAY_SHORT[weekday_index].to_string(),
            "HH" => format!("{:02}", self.hour),
            "H" => self.hour.to_string(),
            "hh" => format!("{:02}", hour_12),
            "h" => hour_12.to_string(),
            "mm" => format!("{:02}", self.minute),
            "m" => self.minute.to_string(),
            "ss" => format!("{:02}", self.second),
            "s" => self.second.to_string(),
            "A" => {
                if self.hour < 12 {
                    "AM".to_string()
                } else {
                    "PM".to_string()
                }
            }
            "a" => {
                if self.hour < 12 {
                    "am".to_string()
                } else {
                    "pm".to_string()
                }
            }
            _ => token.to_string(),
        }
    }
}

fn ordinal_day(day: i64) -> String {
    let suffix = match day.rem_euclid(100) {
        11..=13 => "th",
        _ => match day.rem_euclid(10) {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        },
    };
    format!("{day}{suffix}")
}

fn generated_uuid_string() -> String {
    let value = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        ^ u128::from(std::process::id());
    let hex = format!("{value:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

fn append_at_end(contents: &str, entry: &str) -> String {
    let mut rendered = contents.trim_end_matches('\n').to_string();
    if !rendered.is_empty() {
        rendered.push_str("\n\n");
    }
    rendered.push_str(entry.trim_end());
    rendered.push('\n');
    rendered
}

fn append_under_heading(contents: &str, heading: &str, entry: &str) -> String {
    let heading = heading.trim();
    if heading.is_empty() {
        return append_at_end(contents, entry);
    }

    let heading_level = markdown_heading_level(heading);
    let mut offset = 0usize;
    let mut insert_at = None;
    for line in contents.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if insert_at.is_none() && trimmed == heading {
            insert_at = Some(offset + line.len());
        } else if insert_at.is_some()
            && markdown_heading_level(trimmed).is_some_and(|level| Some(level) <= heading_level)
        {
            insert_at = Some(offset);
            break;
        }
        offset += line.len();
    }

    if let Some(insert_at) = insert_at {
        let mut rendered = String::new();
        rendered.push_str(&contents[..insert_at]);
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        if !rendered.ends_with("\n\n") {
            rendered.push('\n');
        }
        rendered.push_str(entry.trim_end());
        rendered.push('\n');
        if insert_at < contents.len() && !contents[insert_at..].starts_with('\n') {
            rendered.push('\n');
        }
        rendered.push_str(&contents[insert_at..]);
        rendered
    } else {
        let mut rendered = contents.trim_end_matches('\n').to_string();
        if !rendered.is_empty() {
            rendered.push_str("\n\n");
        }
        rendered.push_str(heading);
        rendered.push_str("\n\n");
        rendered.push_str(entry.trim_end());
        rendered.push('\n');
        rendered
    }
}

fn markdown_heading_level(line: &str) -> Option<usize> {
    let hashes = line.chars().take_while(|ch| *ch == '#').count();
    (hashes > 0 && hashes <= 6 && line.chars().nth(hashes).is_some_and(char::is_whitespace))
        .then_some(hashes)
}

fn run_open_command(
    paths: &VaultPaths,
    note: Option<&str>,
    interactive_note_selection: bool,
) -> Result<OpenReport, CliError> {
    let note = resolve_note_argument(paths, note, interactive_note_selection, "note")?;
    let resolved = resolve_note_reference(paths, &note).map_err(CliError::operation)?;
    let uri = build_obsidian_uri(paths, &resolved.path);
    launch_uri(&uri)?;

    Ok(OpenReport {
        path: resolved.path,
        uri,
    })
}

fn build_obsidian_uri(paths: &VaultPaths, path: &str) -> String {
    let vault_name = paths
        .vault_root()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("vault");
    format!(
        "obsidian://open?vault={}&file={}",
        percent_encode(vault_name),
        percent_encode(path)
    )
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn launch_uri(uri: &str) -> Result<(), CliError> {
    let mut command = ProcessCommand::new(open_uri_program());
    for arg in open_uri_args(uri) {
        command.arg(arg);
    }
    let status = command.status().map_err(CliError::operation)?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::operation(format!(
            "launcher exited with status {status} for {uri}"
        )))
    }
}

#[cfg(target_os = "linux")]
fn open_uri_program() -> &'static str {
    "xdg-open"
}

#[cfg(target_os = "macos")]
fn open_uri_program() -> &'static str {
    "open"
}

#[cfg(target_os = "windows")]
fn open_uri_program() -> &'static str {
    "cmd"
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn open_uri_program() -> &'static str {
    "xdg-open"
}

#[cfg(target_os = "windows")]
fn open_uri_args(uri: &str) -> Vec<String> {
    vec![
        "/C".to_string(),
        "start".to_string(),
        String::new(),
        uri.to_string(),
    ]
}

#[cfg(not(target_os = "windows"))]
fn open_uri_args(uri: &str) -> Vec<String> {
    vec![uri.to_string()]
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
            sort,
            match_case,
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
                    sort: *sort,
                    match_case: *match_case,
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

fn cli_search_mode(mode: SearchMode) -> vulcan_core::search::SearchMode {
    match mode {
        SearchMode::Keyword => vulcan_core::search::SearchMode::Keyword,
        SearchMode::Hybrid => vulcan_core::search::SearchMode::Hybrid,
    }
}

fn cli_search_sort(sort: SearchSortArg) -> SearchSort {
    match sort {
        SearchSortArg::Relevance => SearchSort::Relevance,
        SearchSortArg::PathAsc => SearchSort::PathAsc,
        SearchSortArg::PathDesc => SearchSort::PathDesc,
        SearchSortArg::ModifiedNewest => SearchSort::ModifiedNewest,
        SearchSortArg::ModifiedOldest => SearchSort::ModifiedOldest,
        SearchSortArg::CreatedNewest => SearchSort::CreatedNewest,
        SearchSortArg::CreatedOldest => SearchSort::CreatedOldest,
    }
}

fn display_search_sort(sort: SearchSort) -> &'static str {
    match sort {
        SearchSort::Relevance => "relevance",
        SearchSort::PathAsc => "path-asc",
        SearchSort::PathDesc => "path-desc",
        SearchSort::ModifiedNewest => "modified-newest",
        SearchSort::ModifiedOldest => "modified-oldest",
        SearchSort::CreatedNewest => "created-newest",
        SearchSort::CreatedOldest => "created-oldest",
    }
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
    maybe_auto_refresh_command_cache(&paths, cli, use_stderr_color)?;
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
        Command::Edit {
            ref note,
            new,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit);
            let report = run_edit_command(
                &paths,
                cli,
                stdout_is_tty,
                use_stderr_color,
                note.as_deref(),
                new,
            )?;
            auto_commit
                .commit(&paths, "edit", std::slice::from_ref(&report.path))
                .map_err(CliError::operation)?;
            print_edit_report(cli.output, &report);
            Ok(())
        }
        Command::Open { ref note } => {
            let report = run_open_command(&paths, note.as_deref(), interactive_note_selection)?;
            print_open_report(cli.output, &report)
        }
        Command::Browse { no_commit } => {
            if cli.output != OutputFormat::Human || !stdout_is_tty || !io::stdin().is_terminal() {
                return Err(CliError::operation(
                    "browse requires an interactive terminal with `--output human`",
                ));
            }
            let refresh_mode = refresh_mode_for_target(&paths, cli, RefreshTarget::Browse);
            browse_tui::run_browse_tui(&paths, refresh_mode, no_commit).map_err(CliError::operation)
        }
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
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
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
                if !dry_run {
                    auto_commit
                        .commit(&paths, "bases-view-add", std::slice::from_ref(file))
                        .map_err(CliError::operation)?;
                }
                print_bases_view_edit_report(cli.output, &report)
            }
            BasesCommand::ViewDelete {
                ref file,
                ref name,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report =
                    bases_view_delete(&paths, file, name, *dry_run).map_err(CliError::operation)?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "bases-view-delete", std::slice::from_ref(file))
                        .map_err(CliError::operation)?;
                }
                print_bases_view_edit_report(cli.output, &report)
            }
            BasesCommand::ViewRename {
                ref file,
                ref old_name,
                ref new_name,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = bases_view_rename(&paths, file, old_name, new_name, *dry_run)
                    .map_err(CliError::operation)?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "bases-view-rename", std::slice::from_ref(file))
                        .map_err(CliError::operation)?;
                }
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
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
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
                if !dry_run {
                    auto_commit
                        .commit(&paths, "bases-view-edit", std::slice::from_ref(file))
                        .map_err(CliError::operation)?;
                }
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
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit);
            let summary = move_note(&paths, source, dest, dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(&paths, "move", &move_changed_files(&summary))
                    .map_err(CliError::operation)?;
            }
            print_move_summary(cli.output, &summary)?;
            Ok(())
        }
        Command::RenameProperty {
            ref old,
            ref new,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit);
            let report = rename_property(&paths, old, new, dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(&paths, "rename-property", &refactor_changed_files(&report))
                    .map_err(CliError::operation)?;
            }
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::MergeTags {
            ref source,
            ref dest,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit);
            let report = merge_tags(&paths, source, dest, dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(&paths, "merge-tags", &refactor_changed_files(&report))
                    .map_err(CliError::operation)?;
            }
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::RenameAlias {
            ref note,
            ref old,
            ref new,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit);
            let note = resolve_note_argument(
                &paths,
                Some(note.as_str()),
                interactive_note_selection,
                "note to update",
            )?;
            let report =
                rename_alias(&paths, &note, old, new, dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(&paths, "rename-alias", &refactor_changed_files(&report))
                    .map_err(CliError::operation)?;
            }
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::RenameHeading {
            ref note,
            ref old,
            ref new,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit);
            let note = resolve_note_argument(
                &paths,
                Some(note.as_str()),
                interactive_note_selection,
                "note containing heading",
            )?;
            let report =
                rename_heading(&paths, &note, old, new, dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(&paths, "rename-heading", &refactor_changed_files(&report))
                    .map_err(CliError::operation)?;
            }
            print_refactor_report(cli.output, &report)?;
            Ok(())
        }
        Command::RenameBlockRef {
            ref note,
            ref old,
            ref new,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit);
            let note = resolve_note_argument(
                &paths,
                Some(note.as_str()),
                interactive_note_selection,
                "note containing block ref",
            )?;
            let report =
                rename_block_ref(&paths, &note, old, new, dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(&paths, "rename-block-ref", &refactor_changed_files(&report))
                    .map_err(CliError::operation)?;
            }
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
        Command::Watch {
            debounce_ms,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_scan(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit);
            if cli.output == OutputFormat::Human && stdout_is_tty {
                println!(
                    "Watching {} (debounce {}ms)",
                    paths.vault_root().display(),
                    debounce_ms
                );
            }
            watch_vault(&paths, &WatchOptions { debounce_ms }, |report| {
                print_watch_report(cli.output, &report)?;
                if !report.startup
                    && report.summary.added + report.summary.updated + report.summary.deleted > 0
                {
                    auto_commit
                        .commit(&paths, "scan", &report.paths)
                        .map_err(CliError::operation)?;
                }
                Ok::<(), CliError>(())
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
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit);
            let report = bulk_set_property(&paths, filters, key, Some(value.as_str()), dry_run)
                .map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(&paths, "update", &bulk_mutation_changed_files(&report))
                    .map_err(CliError::operation)?;
            }
            print_bulk_mutation_report(cli.output, &report)
        }
        Command::Unset {
            ref filters,
            ref key,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit);
            let report = bulk_set_property(&paths, filters, key, None, dry_run)
                .map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(&paths, "unset", &bulk_mutation_changed_files(&report))
                    .map_err(CliError::operation)?;
            }
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
        Command::Dataview { ref command } => match command {
            DataviewCommand::Inline { file } => {
                let report = run_dataview_inline_command(&paths, file)?;
                print_dataview_inline_report(cli.output, &report)
            }
            DataviewCommand::Query { dql } => {
                let result = run_dataview_query_command(&paths, dql)?;
                print_dql_query_result(
                    cli.output,
                    &result,
                    load_vault_config(&paths)
                        .config
                        .dataview
                        .display_result_count,
                )
            }
            DataviewCommand::Eval { file, block } => {
                let report = run_dataview_eval_command(&paths, file, *block)?;
                print_dataview_eval_report(
                    cli.output,
                    &report,
                    load_vault_config(&paths)
                        .config
                        .dataview
                        .display_result_count,
                )
            }
        },
        Command::Tasks { ref command } => match command {
            TasksCommand::Query { query } => {
                let result = run_tasks_query_command(&paths, query)?;
                print_tasks_query_result(cli.output, &result)
            }
            TasksCommand::Eval { file, block } => {
                let report = run_tasks_eval_command(&paths, file, *block)?;
                print_tasks_eval_report(cli.output, &report)
            }
            TasksCommand::List { filter } => {
                let result = run_tasks_list_command(&paths, filter.as_deref())?;
                print_tasks_query_result(cli.output, &result)
            }
            TasksCommand::Next { count, from } => {
                let report = run_tasks_next_command(&paths, *count, from.as_deref())?;
                print_tasks_next_report(cli.output, &report)
            }
            TasksCommand::Blocked => {
                let report = run_tasks_blocked_command(&paths)?;
                print_tasks_blocked_report(cli.output, &report)
            }
            TasksCommand::Graph => {
                let report = build_tasks_graph_report(&paths)?;
                print_tasks_graph_report(cli.output, &report)
            }
        },
        Command::Search {
            ref query,
            ref filters,
            mode,
            ref tag,
            ref path_prefix,
            ref has_property,
            sort,
            match_case,
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
                    mode: cli_search_mode(mode),
                    sort: sort.map(cli_search_sort),
                    match_case: match_case.then_some(true),
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
                sort,
                match_case,
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
                        mode: cli_search_mode(*mode),
                        tag: tag.clone(),
                        path_prefix: path_prefix.clone(),
                        has_property: has_property.clone(),
                        filters: filters.clone(),
                        context_size: *context_size,
                        sort: sort.map(cli_search_sort),
                        match_case: match_case.then_some(true),
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
        Command::Diff {
            ref note,
            ref since,
        } => {
            let report = run_diff_command(
                &paths,
                note.as_deref(),
                since.as_deref(),
                interactive_note_selection,
            )?;
            print_diff_report(cli.output, &report)
        }
        Command::Inbox {
            ref text,
            ref file,
            no_commit,
        } => {
            let report = run_inbox_command(&paths, text.as_deref(), file.as_ref(), no_commit)?;
            print_inbox_report(cli.output, &report)
        }
        Command::Template {
            ref command,
            ref name,
            list,
            ref path,
            no_commit,
        } => {
            let result = match command {
                Some(TemplateSubcommand::Insert {
                    template,
                    note,
                    prepend,
                    append: _,
                    no_commit,
                }) => TemplateCommandResult::Insert(run_template_insert_command(
                    &paths,
                    template,
                    note.as_deref(),
                    if *prepend {
                        TemplateInsertMode::Prepend
                    } else {
                        TemplateInsertMode::Append
                    },
                    *no_commit,
                    interactive_note_selection,
                )?),
                None => run_template_command(
                    &paths,
                    name.as_deref(),
                    list,
                    path.as_deref(),
                    no_commit,
                    stdout_is_tty,
                )?,
            };

            match result {
                TemplateCommandResult::List(report) => {
                    print_template_list_report(cli.output, &report)
                }
                TemplateCommandResult::Create(report) => {
                    print_template_create_report(cli.output, &report)
                }
                TemplateCommandResult::Insert(report) => {
                    print_template_insert_report(cli.output, &report)
                }
            }
        }
        Command::LinkMentions {
            ref note,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit);
            let report =
                link_mentions(&paths, note.as_deref(), dry_run).map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(&paths, "link-mentions", &refactor_changed_files(&report))
                    .map_err(CliError::operation)?;
            }
            print_refactor_report(cli.output, &report)
        }
        Command::Rewrite {
            ref filters,
            ref find,
            ref replace,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit);
            let report = bulk_replace(&paths, filters, find, replace, dry_run)
                .map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(&paths, "rewrite", &refactor_changed_files(&report))
                    .map_err(CliError::operation)?;
            }
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
                let models = list_vector_models(&paths).map_err(CliError::operation)?;
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
                let dropped = drop_vector_model(&paths, key).map_err(CliError::operation)?;
                if dropped {
                    if cli.output == OutputFormat::Json {
                        println!("{}", serde_json::json!({"dropped": true, "cache_key": key}));
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
        Command::Scan { full, no_commit } => {
            let auto_commit = AutoCommitPolicy::for_scan(&paths, no_commit);
            warn_auto_commit_if_needed(&auto_commit);
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
            if summary.added + summary.updated + summary.deleted > 0 {
                auto_commit
                    .commit(&paths, "scan", &[])
                    .map_err(CliError::operation)?;
            }
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
                    sort,
                    match_case,
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
                    if let Some(sort) = sort {
                        println!("Sort: {}", display_search_sort(*sort));
                    }
                    if *match_case == Some(true) {
                        println!("Match case: true");
                    }
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
                "inline_expressions": note.inline_expressions,
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

fn print_edit_report(output: OutputFormat, report: &EditReport) {
    match output {
        OutputFormat::Human => {
            if report.created {
                println!("Created and edited {}", report.path);
            } else {
                println!("Edited {}", report.path);
            }
        }
        OutputFormat::Json => {
            print_json(report).expect("edit report JSON serialization should succeed");
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

fn print_diff_report(output: OutputFormat, report: &DiffReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if let Some(diff) = report.diff.as_deref() {
                if diff.trim().is_empty() {
                    println!("No changes in {} since {}.", report.path, report.anchor);
                } else {
                    println!("Diff for {} against {}:", report.path, report.anchor);
                    print!("{diff}");
                    if !diff.ends_with('\n') {
                        println!();
                    }
                }
            } else if report.changed {
                println!(
                    "{} changed since {} ({})",
                    report.path,
                    report.anchor,
                    report.changed_kinds.join(", ")
                );
            } else {
                println!("No changes in {} since {}.", report.path, report.anchor);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_dataview_inline_report(
    output: OutputFormat,
    report: &DataviewInlineReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.results.is_empty() {
                println!("No inline expressions in {}", report.file);
                return Ok(());
            }
            println!("Dataview inline expressions for {}", report.file);
            for result in &report.results {
                if let Some(error) = &result.error {
                    println!("- {} => error: {}", result.expression, error);
                } else {
                    println!(
                        "- {} => {}",
                        result.expression,
                        render_dataview_inline_value(&result.value)
                    );
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_dataview_eval_report(
    output: OutputFormat,
    report: &DataviewEvalReport,
    show_result_count: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.blocks.is_empty() {
                println!("No Dataview blocks in {}", report.file);
                return Ok(());
            }

            println!("Dataview blocks for {}", report.file);
            for (index, block) in report.blocks.iter().enumerate() {
                if index > 0 {
                    println!();
                }
                println!(
                    "Block {} ({}, line {})",
                    block.block_index, block.language, block.line_number
                );
                if let Some(error) = &block.error {
                    println!("error: {error}");
                    continue;
                }
                if let Some(result) = &block.result {
                    print_dql_query_result_human(result, show_result_count);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_dql_query_result(
    output: OutputFormat,
    result: &DqlQueryResult,
    show_result_count: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            print_dql_query_result_human(result, show_result_count);
            Ok(())
        }
        OutputFormat::Json => print_json(result),
    }
}

fn print_tasks_query_result(
    output: OutputFormat,
    result: &TasksQueryResult,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            print_tasks_query_result_human(result)?;
            Ok(())
        }
        OutputFormat::Json => print_json(result),
    }
}

fn print_tasks_eval_report(output: OutputFormat, report: &TasksEvalReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.blocks.is_empty() {
                println!("No Tasks blocks in {}", report.file);
                return Ok(());
            }

            println!("Tasks blocks for {}", report.file);
            for (index, block) in report.blocks.iter().enumerate() {
                if index > 0 {
                    println!();
                }
                println!("Block {} (line {})", block.block_index, block.line_number);
                if let Some(error) = &block.error {
                    println!("error: {error}");
                    continue;
                }
                if let Some(result) = &block.result {
                    print_tasks_query_result_human(result)?;
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_tasks_next_report(output: OutputFormat, report: &TasksNextReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.occurrences.is_empty() {
                println!("No recurring task instances.");
                return Ok(());
            }

            let mut current_date: Option<&str> = None;
            let mut current_path: Option<&str> = None;
            for occurrence in &report.occurrences {
                if current_date != Some(occurrence.date.as_str()) {
                    if current_date.is_some() {
                        println!();
                    }
                    current_date = Some(occurrence.date.as_str());
                    current_path = None;
                    println!("{}", occurrence.date);
                }

                let path = occurrence
                    .task
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown>");
                if current_path != Some(path) {
                    current_path = Some(path);
                    println!("{path}");
                }

                let status = occurrence
                    .task
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or(" ");
                let text = occurrence
                    .task
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                println!("- [{status}] {text}");
            }

            println!("{} occurrence(s)", report.result_count);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_tasks_blocked_report(
    output: OutputFormat,
    report: &TasksBlockedReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.tasks.is_empty() {
                println!("No blocked tasks.");
                return Ok(());
            }

            for blocked in &report.tasks {
                let status = blocked
                    .task
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or(" ");
                let path = blocked
                    .task
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown>");
                let text = blocked
                    .task
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                println!("{path}");
                println!("- [{status}] {text}");
                for blocker in &blocked.blockers {
                    if blocker.resolved {
                        println!(
                            "  blocked by {} ({}, line {}) [{}]",
                            blocker.blocker_id,
                            blocker.blocker_path.as_deref().unwrap_or("<unknown>"),
                            blocker.blocker_line.unwrap_or_default(),
                            if blocker.blocker_completed == Some(true) {
                                "done"
                            } else {
                                "open"
                            }
                        );
                    } else {
                        println!("  blocked by {} [unresolved]", blocker.blocker_id);
                    }
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_tasks_graph_report(
    output: OutputFormat,
    report: &TasksGraphReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("Tasks: {}", report.nodes.len());
            println!("Dependencies: {}", report.edges.len());
            if report.edges.is_empty() {
                return Ok(());
            }
            for edge in &report.edges {
                if edge.resolved {
                    println!(
                        "- {} -> {} ({}, line {})",
                        edge.blocked_key,
                        edge.blocker_id,
                        edge.blocker_path.as_deref().unwrap_or("<unknown>"),
                        edge.blocker_line.unwrap_or_default()
                    );
                } else {
                    println!("- {} -> {} [unresolved]", edge.blocked_key, edge.blocker_id);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_tasks_query_result_human(result: &TasksQueryResult) -> Result<(), CliError> {
    if let Some(plan) = &result.plan {
        println!(
            "Plan:\n{}",
            serde_json::to_string_pretty(plan).map_err(CliError::operation)?
        );
        if result.tasks.is_empty() {
            return Ok(());
        }
        println!();
    } else if result.tasks.is_empty() {
        println!("No tasks matched.");
        return Ok(());
    }

    if result.groups.is_empty() {
        print_tasks_by_file_human(&result.tasks);
    } else {
        for (index, group) in result.groups.iter().enumerate() {
            if index > 0 {
                println!();
            }
            println!("{}", render_human_value(&group.key));
            print_tasks_by_file_human(&group.tasks);
        }
    }
    println!("{} task(s)", result.result_count);
    Ok(())
}

fn print_tasks_by_file_human(tasks: &[Value]) {
    let mut current_file: Option<&str> = None;
    for task in tasks {
        let path = task
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        if current_file != Some(path) {
            current_file = Some(path);
            println!("{path}");
        }

        let status = task.get("status").and_then(Value::as_str).unwrap_or(" ");
        let text = task.get("text").and_then(Value::as_str).unwrap_or_default();
        println!("- [{status}] {text}");
    }
}

fn print_dql_query_result_human(result: &DqlQueryResult, show_result_count: bool) {
    match result.query_type {
        vulcan_core::dql::DqlQueryType::Table => print_dql_table_human(result, show_result_count),
        vulcan_core::dql::DqlQueryType::List => print_dql_list_human(result),
        vulcan_core::dql::DqlQueryType::Task => print_dql_task_human(result, show_result_count),
        vulcan_core::dql::DqlQueryType::Calendar => print_dql_calendar_human(result),
    }
}

fn print_dql_table_human(result: &DqlQueryResult, show_result_count: bool) {
    if !result.columns.is_empty() {
        println!("{}", result.columns.join(" | "));
    }
    for row in &result.rows {
        let line = result
            .columns
            .iter()
            .map(|column| render_dataview_inline_value(&row[column]))
            .collect::<Vec<_>>()
            .join(" | ");
        println!("{line}");
    }
    if show_result_count {
        println!("{} result(s)", result.result_count);
    }
}

fn print_dql_list_human(result: &DqlQueryResult) {
    if result.rows.is_empty() {
        return;
    }
    for row in &result.rows {
        let rendered = match result.columns.as_slice() {
            [column] => render_dataview_inline_value(&row[column]),
            [left, right, ..] => format!(
                "{}: {}",
                render_dataview_inline_value(&row[left]),
                render_dataview_inline_value(&row[right])
            ),
            [] => serde_json::to_string(row).unwrap_or_default(),
        };
        println!("- {rendered}");
    }
}

fn print_dql_task_human(result: &DqlQueryResult, show_result_count: bool) {
    if result.rows.is_empty() {
        return;
    }

    let file_column = result.columns.first().map(String::as_str).unwrap_or("File");
    let mut current_file: Option<&str> = None;
    for row in &result.rows {
        let file = row[file_column].as_str().unwrap_or_default();
        if current_file != Some(file) {
            current_file = Some(file);
            println!("{file}");
        }
        let status = row["status"].as_str().unwrap_or(" ");
        let text = render_dataview_inline_value(&row["text"]);
        println!("- [{status}] {text}");
    }
    if show_result_count {
        println!("{} task(s)", result.result_count);
    }
}

fn print_dql_calendar_human(result: &DqlQueryResult) {
    if result.rows.is_empty() {
        println!("No calendar entries.");
        return;
    }

    let file_column = result.columns.get(1).map(String::as_str).unwrap_or("File");
    let mut current_date: Option<&str> = None;
    for row in &result.rows {
        let date = row["date"].as_str().unwrap_or_default();
        if current_date != Some(date) {
            current_date = Some(date);
            println!("{date}");
        }
        println!("- {}", render_dataview_inline_value(&row[file_column]));
    }
}

fn render_dataview_inline_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => serde_json::to_string(value).expect("inline result should serialize"),
    }
}

fn print_inbox_report(output: OutputFormat, report: &InboxReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("Appended to {}", report.path);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_template_list_report(
    output: OutputFormat,
    report: &TemplateListReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.templates.is_empty() {
                println!("No templates found.");
            } else {
                for template in &report.templates {
                    println!("{} [{}: {}]", template.name, template.source, template.path);
                }
            }
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_template_create_report(
    output: OutputFormat,
    report: &TemplateCreateReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!(
                "Created {} from {} ({})",
                report.path, report.template, report.template_source
            );
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_template_insert_report(
    output: OutputFormat,
    report: &TemplateInsertReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!(
                "Inserted {} into {} ({}, {})",
                report.template, report.note, report.mode, report.template_source
            );
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_open_report(output: OutputFormat, report: &OpenReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("Opened {} in Obsidian", report.path);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
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
    if hits.is_empty() && report.plan.is_some() {
        return vec![serde_json::json!({
            "query": report.query,
            "mode": report.mode,
            "tag": report.tag,
            "path_prefix": report.path_prefix,
            "has_property": report.has_property,
            "filters": report.filters,
            "effective_query": report.plan.as_ref().map(|plan| plan.effective_query.clone()),
            "parsed_query_explanation": report
                .plan
                .as_ref()
                .map(|plan| plan.parsed_query_explanation.clone()),
            "document_path": Value::Null,
            "chunk_id": Value::Null,
            "heading_path": Vec::<String>::new(),
            "snippet": Value::Null,
            "matched_line": Value::Null,
            "rank": Value::Null,
            "explain": Value::Null,
            "no_results": true,
        })];
    }

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
                "parsed_query_explanation": report
                    .plan
                    .as_ref()
                    .map(|plan| plan.parsed_query_explanation.clone()),
                "document_path": hit.document_path,
                "chunk_id": hit.chunk_id,
                "heading_path": hit.heading_path,
                "snippet": hit.snippet,
                "matched_line": hit.matched_line,
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
                "inline_expressions": note.inline_expressions,
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

#[allow(clippy::float_cmp, clippy::cast_possible_truncation)]
fn render_human_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".to_string(),
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f == f.trunc() && f.abs() < 1e15 {
                format!("{}", f as i64)
            } else {
                n.to_string()
            }
        }
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
    if let Some(line) = hit.matched_line {
        println!("   {}: {line}", palette.cyan("Line"));
    }

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
    if !plan.parsed_query_explanation.is_empty() {
        println!("{}:", palette.cyan("Query plan"));
        for line in &plan.parsed_query_explanation {
            println!("  {line}");
        }
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
    use std::fs;
    use std::process::Command as ProcessCommand;
    use tempfile::TempDir;

    fn run_git(vault_root: &Path, args: &[&str]) {
        let status = ProcessCommand::new("git")
            .arg("-C")
            .arg(vault_root)
            .args(args)
            .status()
            .expect("git should launch");
        assert!(status.success(), "git command failed: {:?}", args);
    }

    fn init_git_repo(vault_root: &Path) {
        run_git(vault_root, &["init"]);
        run_git(vault_root, &["config", "user.name", "Vulcan Test"]);
        run_git(vault_root, &["config", "user.email", "vulcan@example.com"]);
    }

    fn git_head_summary(vault_root: &Path) -> String {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(vault_root)
            .args(["log", "-1", "--pretty=%s"])
            .output()
            .expect("git log should launch");
        assert!(output.status.success(), "git log should succeed");
        String::from_utf8(output.stdout)
            .expect("git stdout should be utf8")
            .trim()
            .to_string()
    }

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
    fn parses_dataview_inline_command() {
        let cli = Cli::try_parse_from(["vulcan", "dataview", "inline", "Dashboard"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Dataview {
                command: DataviewCommand::Inline {
                    file: "Dashboard".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_dataview_query_command() {
        let cli = Cli::try_parse_from(["vulcan", "dataview", "query", "TABLE status FROM #tag"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Dataview {
                command: DataviewCommand::Query {
                    dql: "TABLE status FROM #tag".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_dataview_eval_command() {
        let cli = Cli::try_parse_from(["vulcan", "dataview", "eval", "Dashboard", "--block", "1"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Dataview {
                command: DataviewCommand::Eval {
                    file: "Dashboard".to_string(),
                    block: Some(1),
                },
            }
        );
    }

    #[test]
    fn parses_tasks_query_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "query", "not done"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Query {
                    query: "not done".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_tasks_eval_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "eval", "Dashboard", "--block", "1"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Eval {
                    file: "Dashboard".to_string(),
                    block: Some(1),
                },
            }
        );
    }

    #[test]
    fn parses_tasks_list_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "list", "--filter", "completed"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::List {
                    filter: Some("completed".to_string()),
                },
            }
        );
    }

    #[test]
    fn parses_tasks_next_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "next", "5", "--from", "2026-03-29"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Next {
                    count: 5,
                    from: Some("2026-03-29".to_string()),
                },
            }
        );
    }

    #[test]
    fn parses_tasks_blocked_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "blocked"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Blocked,
            }
        );
    }

    #[test]
    fn parses_tasks_graph_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "graph"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Graph,
            }
        );
    }

    #[test]
    fn edit_new_auto_commit_creates_git_commit() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        fs::create_dir_all(temp_dir.path().join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            temp_dir.path().join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\n",
        )
        .expect("config should be written");

        let original_editor = std::env::var_os("EDITOR");
        std::env::set_var("EDITOR", "true");

        let result = run_from([
            "vulcan",
            "--vault",
            temp_dir.path().to_str().expect("temp dir should be utf8"),
            "edit",
            "--new",
            "Notes/Idea.md",
        ]);

        match original_editor {
            Some(value) => std::env::set_var("EDITOR", value),
            None => std::env::remove_var("EDITOR"),
        }

        result.expect("edit should succeed");
        assert!(temp_dir.path().join("Notes/Idea.md").exists());
        assert_eq!(
            git_head_summary(temp_dir.path()),
            "vulcan edit: Notes/Idea.md"
        );
    }

    #[test]
    fn diff_command_uses_git_head_for_modified_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        fs::write(temp_dir.path().join("Home.md"), "# Home\n").expect("note should be written");
        let paths = VaultPaths::new(temp_dir.path());
        vulcan_core::scan_vault(&paths, ScanMode::Incremental).expect("scan should succeed");
        init_git_repo(temp_dir.path());
        run_git(temp_dir.path(), &["add", "Home.md"]);
        run_git(temp_dir.path(), &["commit", "-m", "Initial"]);

        fs::write(temp_dir.path().join("Home.md"), "# Home\nUpdated\n")
            .expect("note should be updated");

        let report =
            run_diff_command(&paths, Some("Home"), None, false).expect("diff should succeed");

        assert_eq!(report.path, "Home.md");
        assert_eq!(report.source, "git_head");
        assert_eq!(report.status, "changed");
        assert!(report.changed);
        assert!(report
            .diff
            .as_deref()
            .is_some_and(|diff| diff.contains("+Updated")));
    }

    #[test]
    fn append_under_heading_inserts_before_next_peer_heading() {
        let rendered = append_under_heading(
            "# Notes\n\n## Inbox\n\n- earlier\n\n## Later\n\ncontent\n",
            "## Inbox",
            "- new item",
        );

        assert!(rendered.contains("## Inbox\n\n- earlier\n\n- new item\n\n## Later"));
    }

    #[test]
    fn render_template_contents_supports_obsidian_format_strings() {
        let timestamp = test_template_timestamp(2026, 3, 26, 17, 4, 5);
        let variables = template_variables_for_path("Journal/Today.md", timestamp);
        let config = TemplatesConfig {
            date_format: "dddd, MMMM Do YYYY".to_string(),
            time_format: "hh:mm A".to_string(),
            obsidian_folder: None,
        };

        let rendered = render_template_contents(
            "Date {{date}}\nTime {{time}}\nAlt {{time:YYYY-MM-DD}}\nWeekday {{date:dd}} {{date:ddd}} {{date:dddd}}\nStamp {{datetime}}\n",
            &variables,
            &config,
        );

        assert!(rendered.contains("Date Thursday, March 26th 2026"));
        assert!(rendered.contains("Time 05:04 PM"));
        assert!(rendered.contains("Alt 2026-03-26"));
        assert!(rendered.contains("Weekday Th Thu Thursday"));
        assert!(rendered.contains(&format!("Stamp {}", variables.datetime)));
    }

    #[test]
    fn render_template_contents_preserves_datetime_and_uuid_variables() {
        let timestamp = test_template_timestamp(2026, 3, 26, 17, 4, 5);
        let mut variables = template_variables_for_path("Journal/Today.md", timestamp);
        variables.uuid = "00000000-0000-0000-0000-000000000000".to_string();
        let config = TemplatesConfig::default();

        let rendered = render_template_contents(
            "{{datetime}}\n{{uuid}}\n{{date}}\n{{time}}\n",
            &variables,
            &config,
        );

        assert!(rendered.contains("2026-03-26T17:04:05Z"));
        assert!(rendered.contains("00000000-0000-0000-0000-000000000000"));
        assert!(rendered.contains("2026-03-26"));
        assert!(rendered.contains("17:04"));
    }

    #[test]
    fn template_command_lists_obsidian_templates_with_sources_and_conflicts() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
        fs::create_dir_all(temp_dir.path().join(".obsidian")).expect("obsidian dir");
        fs::create_dir_all(temp_dir.path().join("Shared Templates")).expect("shared templates dir");
        fs::write(
            temp_dir.path().join(".obsidian/templates.json"),
            r#"{"folder":"Shared Templates"}"#,
        )
        .expect("templates config should be written");
        fs::write(
            paths.vulcan_dir().join("templates").join("daily.md"),
            "# Vulcan\n",
        )
        .expect("vulcan template should be written");
        fs::write(
            temp_dir.path().join("Shared Templates").join("daily.md"),
            "# Obsidian\n",
        )
        .expect("obsidian daily template should be written");
        fs::write(
            temp_dir.path().join("Shared Templates").join("meeting.md"),
            "# Meeting\n",
        )
        .expect("obsidian meeting template should be written");

        let result = run_template_command(&paths, None, true, None, false, false)
            .expect("template list should succeed");
        let TemplateCommandResult::List(report) = result else {
            panic!("template command should list templates");
        };

        assert_eq!(
            report.templates,
            vec![
                TemplateSummary {
                    name: "daily.md".to_string(),
                    source: "vulcan".to_string(),
                    path: ".vulcan/templates/daily.md".to_string(),
                },
                TemplateSummary {
                    name: "meeting.md".to_string(),
                    source: "obsidian".to_string(),
                    path: "Shared Templates/meeting.md".to_string(),
                },
            ]
        );
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("daily.md"));
        assert!(report.warnings[0].contains(".vulcan/templates/daily.md"));
    }

    #[test]
    fn template_command_prefers_vulcan_template_over_obsidian_conflict() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
        fs::create_dir_all(temp_dir.path().join(".obsidian")).expect("obsidian dir");
        fs::create_dir_all(temp_dir.path().join("Shared Templates")).expect("shared templates dir");
        fs::write(
            temp_dir.path().join(".obsidian/templates.json"),
            r#"{"folder":"Shared Templates"}"#,
        )
        .expect("templates config should be written");
        fs::write(
            paths.vulcan_dir().join("templates").join("daily.md"),
            "# Vulcan {{title}}\n",
        )
        .expect("vulcan template should be written");
        fs::write(
            temp_dir.path().join("Shared Templates").join("daily.md"),
            "# Obsidian {{title}}\n",
        )
        .expect("obsidian template should be written");

        let result = run_template_command(
            &paths,
            Some("daily"),
            false,
            Some("Journal/Today"),
            false,
            false,
        )
        .expect("template command should succeed");

        let TemplateCommandResult::Create(report) = result else {
            panic!("template command should create a note");
        };
        assert_eq!(report.template, "daily.md");
        assert_eq!(report.template_source, "vulcan");
        assert_eq!(report.warnings.len(), 1);

        let contents = fs::read_to_string(temp_dir.path().join("Journal/Today.md"))
            .expect("created note should be readable");
        assert!(contents.contains("# Vulcan Today"));
        assert!(!contents.contains("# Obsidian Today"));
    }

    #[test]
    fn prepare_template_insertion_merges_frontmatter_without_overwriting_existing_values() {
        let timestamp = test_template_timestamp(2026, 3, 26, 17, 4, 5);
        let variables = template_variables_for_path("Projects/Alpha.md", timestamp);
        let rendered_template = render_template_contents(
            "---\nstatus: backlog\ncreated: \"{{date}}\"\ntags:\n- team\n- release\n---\n\n## Template Section\n",
            &variables,
            &TemplatesConfig::default(),
        );

        let prepared = prepare_template_insertion(
            "---\nstatus: done\ntags:\n- team\n- shipped\nowner: Alice\n---\n# Existing\n",
            &rendered_template,
        )
        .expect("template insertion should be prepared");

        let merged_frontmatter = prepared
            .merged_frontmatter
            .expect("merged frontmatter should be present");
        let merged = YamlValue::Mapping(merged_frontmatter);
        assert_eq!(merged["status"], YamlValue::String("done".to_string()));
        assert_eq!(merged["owner"], YamlValue::String("Alice".to_string()));
        assert_eq!(
            merged["created"],
            YamlValue::String("2026-03-26".to_string())
        );
        assert_eq!(
            merged["tags"],
            serde_yaml::from_str::<YamlValue>("- team\n- shipped\n- release\n")
                .expect("tags should parse")
        );
        assert_eq!(prepared.target_body, "# Existing\n");
        assert_eq!(prepared.template_body, "\n## Template Section\n");
    }

    #[test]
    fn prepare_template_insertion_uses_template_frontmatter_when_target_has_none() {
        let prepared = prepare_template_insertion(
            "# Existing\n",
            "---\nstatus: backlog\n---\nTemplate body\n",
        )
        .expect("template insertion should be prepared");

        let rendered = render_note_from_parts(&prepared.merged_frontmatter, &prepared.target_body)
            .expect("note should render");
        assert!(rendered.starts_with("---\nstatus: backlog\n---\n# Existing\n"));
        assert_eq!(prepared.template_body, "Template body\n");
    }

    #[test]
    fn template_command_creates_note_and_renders_variables() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
        fs::write(
            paths.vulcan_dir().join("templates").join("daily.md"),
            "# {{title}}\n\nCreated {{date}} {{time}}\nID {{uuid}}\n",
        )
        .expect("template should be written");

        let result = run_template_command(
            &paths,
            Some("daily"),
            false,
            Some("Journal/Today"),
            false,
            false,
        )
        .expect("template command should succeed");

        let TemplateCommandResult::Create(report) = result else {
            panic!("template command should create a note");
        };
        assert_eq!(report.path, "Journal/Today.md");
        let contents = fs::read_to_string(temp_dir.path().join("Journal/Today.md"))
            .expect("created note should be readable");
        assert!(contents.contains("# Today"));
        assert!(contents.contains("Created "));
        assert!(contents.contains("ID "));
    }

    #[test]
    fn template_insert_command_prepends_and_merges_frontmatter() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
        fs::write(
            temp_dir.path().join("Home.md"),
            "---\nstatus: done\ntags:\n- team\n- shipped\nowner: Alice\n---\n# Existing\n",
        )
        .expect("target note should be written");
        fs::write(
            paths.vulcan_dir().join("templates").join("daily.md"),
            "---\nstatus: backlog\ncreated: \"{{date}}\"\ntags:\n- team\n- release\n---\n\n## Inserted\n",
        )
        .expect("template should be written");
        vulcan_core::scan_vault(&paths, ScanMode::Incremental).expect("scan should succeed");

        let report = run_template_insert_command(
            &paths,
            "daily",
            Some("Home"),
            TemplateInsertMode::Prepend,
            false,
            false,
        )
        .expect("template insert should succeed");

        assert_eq!(report.note, "Home.md");
        assert_eq!(report.mode, "prepend");
        let updated =
            fs::read_to_string(temp_dir.path().join("Home.md")).expect("updated note should exist");
        let (frontmatter, body) =
            parse_frontmatter_document(&updated, false).expect("updated note should parse");
        let frontmatter = YamlValue::Mapping(frontmatter.expect("frontmatter should exist"));
        assert_eq!(frontmatter["status"], YamlValue::String("done".to_string()));
        assert_eq!(frontmatter["owner"], YamlValue::String("Alice".to_string()));
        assert_eq!(
            frontmatter["created"],
            YamlValue::String(TemplateTimestamp::current().default_date_string())
        );
        assert_eq!(
            frontmatter["tags"],
            serde_yaml::from_str::<YamlValue>("- team\n- shipped\n- release\n")
                .expect("tags should parse"),
        );
        assert_eq!(body, "\n## Inserted\n\n# Existing\n");
    }

    #[test]
    fn template_insert_command_appends_and_auto_commits() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
        fs::write(temp_dir.path().join("Home.md"), "# Existing\n")
            .expect("target note should be written");
        fs::write(
            paths.vulcan_dir().join("templates").join("daily.md"),
            "## Inserted\n",
        )
        .expect("template should be written");
        fs::write(paths.config_file(), "[git]\nauto_commit = true\n")
            .expect("config should be written");
        vulcan_core::scan_vault(&paths, ScanMode::Incremental).expect("scan should succeed");
        init_git_repo(temp_dir.path());
        run_git(temp_dir.path(), &["add", "Home.md", ".vulcan/config.toml"]);
        run_git(temp_dir.path(), &["commit", "-m", "Initial"]);

        let report = run_template_insert_command(
            &paths,
            "daily",
            Some("Home"),
            TemplateInsertMode::Append,
            false,
            false,
        )
        .expect("template insert should succeed");

        assert_eq!(report.note, "Home.md");
        assert_eq!(report.mode, "append");
        assert_eq!(
            fs::read_to_string(temp_dir.path().join("Home.md")).expect("updated note should exist"),
            "# Existing\n\n## Inserted\n",
        );
        assert_eq!(
            git_head_summary(temp_dir.path()),
            "vulcan template insert: Home.md"
        );
    }

    fn test_template_timestamp(
        year: i64,
        month: i64,
        day: i64,
        hour: i64,
        minute: i64,
        second: i64,
    ) -> TemplateTimestamp {
        TemplateTimestamp {
            days_since_epoch: days_from_civil(year, month, day),
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }

    fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
        let adjusted_year = year - if month <= 2 { 1 } else { 0 };
        let era = if adjusted_year >= 0 {
            adjusted_year
        } else {
            adjusted_year - 399
        } / 400;
        let year_of_era = adjusted_year - era * 400;
        let month_index = month + if month > 2 { -3 } else { 9 };
        let day_of_year = (153 * month_index + 2) / 5 + day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

        era * 146_097 + day_of_era - 719_468
    }

    #[test]
    fn build_obsidian_uri_uses_vault_name_and_percent_encoding() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("My Vault");
        fs::create_dir_all(&vault_root).expect("vault root should be created");
        let paths = VaultPaths::new(&vault_root);

        let uri = build_obsidian_uri(&paths, "Notes/Hello World.md");

        assert_eq!(
            uri,
            "obsidian://open?vault=My%20Vault&file=Notes%2FHello%20World.md"
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
        let diff = Cli::try_parse_from(["vulcan", "diff", "Home"]).expect("cli should parse");
        let inbox = Cli::try_parse_from(["vulcan", "inbox", "idea"]).expect("cli should parse");
        let template = Cli::try_parse_from(["vulcan", "template", "daily", "--path", "Notes/Day"])
            .expect("cli should parse");
        let template_insert =
            Cli::try_parse_from(["vulcan", "template", "insert", "daily", "Home", "--prepend"])
                .expect("cli should parse");
        let open = Cli::try_parse_from(["vulcan", "open", "Home"]).expect("cli should parse");
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
        let browse = Cli::try_parse_from(["vulcan", "browse"]).expect("cli should parse");
        let refreshed_browse = Cli::try_parse_from(["vulcan", "--refresh", "background", "browse"])
            .expect("cli should parse");
        let edit = Cli::try_parse_from(["vulcan", "edit", "Home"]).expect("cli should parse");
        let edit_new = Cli::try_parse_from(["vulcan", "edit", "--new", "Notes/Idea"])
            .expect("cli should parse");
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
        assert_eq!(
            watch.command,
            Command::Watch {
                debounce_ms: 125,
                no_commit: false,
            }
        );
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
                sort: None,
                match_case: false,
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
            diff.command,
            Command::Diff {
                note: Some("Home".to_string()),
                since: None,
            }
        );
        assert_eq!(
            inbox.command,
            Command::Inbox {
                text: Some("idea".to_string()),
                file: None,
                no_commit: false,
            }
        );
        assert_eq!(
            template.command,
            Command::Template {
                command: None,
                name: Some("daily".to_string()),
                list: false,
                path: Some("Notes/Day".to_string()),
                no_commit: false,
            }
        );
        assert_eq!(
            template_insert.command,
            Command::Template {
                command: Some(TemplateSubcommand::Insert {
                    template: "daily".to_string(),
                    note: Some("Home".to_string()),
                    prepend: true,
                    append: false,
                    no_commit: false,
                }),
                name: None,
                list: false,
                path: None,
                no_commit: false,
            }
        );
        assert_eq!(
            open.command,
            Command::Open {
                note: Some("Home".to_string())
            }
        );
        assert_eq!(
            link_mentions.command,
            Command::LinkMentions {
                note: Some("Home".to_string()),
                dry_run: true,
                no_commit: false,
            }
        );
        assert_eq!(
            rewrite.command,
            Command::Rewrite {
                filters: vec!["reviewed = true".to_string()],
                find: "release".to_string(),
                replace: "launch".to_string(),
                dry_run: true,
                no_commit: false,
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
        assert_eq!(browse.command, Command::Browse { no_commit: false });
        assert_eq!(browse.refresh, None);
        assert_eq!(refreshed_browse.refresh, Some(RefreshMode::Background));
        assert_eq!(
            refreshed_browse.command,
            Command::Browse { no_commit: false }
        );
        assert_eq!(
            related_picker.command,
            Command::Related {
                note: None,
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            edit.command,
            Command::Edit {
                note: Some("Home".to_string()),
                new: false,
                no_commit: false,
            }
        );
        assert_eq!(
            edit_new.command,
            Command::Edit {
                note: Some("Notes/Idea".to_string()),
                new: true,
                no_commit: false,
            }
        );
        assert_eq!(
            move_command.command,
            Command::Move {
                source: "Projects/Alpha.md".to_string(),
                dest: "Archive/Alpha.md".to_string(),
                dry_run: true,
                no_commit: false,
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
                no_commit: false,
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
                no_commit: false,
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
                no_commit: false,
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
                no_commit: false,
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
                no_commit: false,
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
                    sort: None,
                    match_case: false,
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
        assert_eq!(
            cli.command,
            Command::Scan {
                full: true,
                no_commit: false,
            }
        );
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
            .any(|command| command.name == "browse"));
        assert!(report.commands.iter().any(|command| command.name == "edit"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "graph"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "dataview"));
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
