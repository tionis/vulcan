mod bases_tui;
mod browse_tui;
mod cli;
mod commit;
mod editor;
mod note_picker;
mod serve;
mod template_engine;

pub use cli::{
    AutomationCommand, BasesCommand, CacheCommand, CheckpointCommand, Cli, Command, ConfigCommand,
    ConfigImportArgs, ConfigImportCommand, ConfigImportSelection, ConfigImportTargetArg,
    DailyCommand, DataviewCommand, DescribeFormatArg, ExportArgs, ExportCommand, ExportFormat,
    GitCommand, GraphCommand, IndexCommand, InitArgs, KanbanCommand, NoteAppendPeriodicArg,
    NoteCommand, OutputFormat, PeriodicOpenArgs, PeriodicSubcommand, QueryFormatArg,
    RefactorCommand, RefreshMode, RepairCommand, SavedCommand, SearchMode, SearchSortArg,
    SuggestCommand, TasksCommand, TasksListSourceArg, TasksViewCommand, TemplateEngineArg,
    TemplateRenderArgs, TemplateSubcommand, VectorQueueCommand, VectorsCommand, WebCommand,
    WebFetchMode,
};

use crate::commit::AutoCommitPolicy;
use crate::editor::open_in_editor;
use crate::template_engine::{
    parse_template_var_bindings, render_template_request, TemplateEngineKind,
    TemplateRenderRequest, TemplateRunMode,
};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use clap_complete::generate;
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::AUTHORIZATION;
use serde::Serialize;
use serde_json::{Map, Value};
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use serve::{serve_forever, ServeOptions};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{Duration, Instant};
use vulcan_core::config::QuickAddImporter;
use vulcan_core::expression::eval::{evaluate as evaluate_expression, is_truthy, EvalContext};
use vulcan_core::expression::functions::{date_components, parse_date_like_string};
use vulcan_core::expression::parse_expression;
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::properties::{extract_indexed_properties, load_note_index};
use vulcan_core::{
    add_kanban_card, all_importers, annotate_import_conflicts, archive_kanban_card, bases_view_add,
    bases_view_delete, bases_view_edit, bases_view_rename, bulk_replace, bulk_set_property,
    cache_vacuum, cluster_vectors, create_checkpoint, doctor_fix, doctor_vault, drop_vector_model,
    evaluate_base_file, evaluate_dataview_js_query, evaluate_dql, evaluate_note_inline_expressions,
    evaluate_tasks_query, execute_query_report, expected_periodic_note_path,
    export_daily_events_to_ics, export_static_search_index, extract_tasknote, git_blame,
    git_commit, git_diff, git_recent_log, git_status, index_vectors_with_progress,
    initialize_vault, inspect_base_file, inspect_cache, inspect_vector_queue, link_mentions,
    list_checkpoints, list_daily_note_events, list_kanban_boards, list_saved_reports,
    list_vector_models, load_dataview_blocks, load_events_for_periodic_note, load_kanban_board,
    load_saved_report, load_tasks_blocks, load_vault_config, merge_tags, move_kanban_card,
    move_note, parse_dql_with_diagnostics, parse_tasknote_natural_language, parse_tasks_query,
    period_range_for_date, plan_base_note_create, query_backlinks, query_change_report,
    query_graph_analytics, query_graph_components, query_graph_dead_ends, query_graph_hubs,
    query_graph_moc_candidates, query_graph_path, query_graph_trends, query_links, query_notes,
    query_related_notes, query_vector_neighbors, rebuild_vault_with_progress,
    rebuild_vectors_with_progress, rename_alias, rename_block_ref, rename_heading, rename_property,
    repair_fts, repair_vectors_with_progress, resolve_link, resolve_note_reference,
    resolve_periodic_note, save_saved_report, scan_vault_with_progress, search_vault,
    shape_tasks_query_result, step_period_start, suggest_duplicates, suggest_mentions,
    task_upcoming_occurrences, tasknotes_default_date_value, tasknotes_default_recurrence_rule,
    tasknotes_status_state, vector_duplicates, verify_cache, watch_vault, AutoScanMode,
    BacklinkRecord, BacklinksReport, BaseViewGroupBy, BaseViewPatch, BaseViewSpec,
    BasesCreateContext, BasesEvalReport, BasesViewEditReport, BulkMutationReport, CacheDatabase,
    CacheInspectReport, CacheVacuumQuery, CacheVacuumReport, CacheVerifyReport, ChangeAnchor,
    ChangeItem, ChangeKind, ChangeReport, CheckpointRecord, ClusterQuery, ClusterReport,
    ConfigImportReport, CoreImporter, DataviewImporter, DataviewJsOutput, DataviewJsResult,
    DoctorByteRange, DoctorDiagnosticIssue, DoctorFixReport, DoctorLinkIssue, DoctorReport,
    DqlQueryResult, DuplicateSuggestionsReport, EvaluatedInlineExpression, GitBlameLine,
    GitCommitReport, GitLogEntry, GraphAnalyticsReport, GraphComponentsReport, GraphDeadEndsReport,
    GraphHubsReport, GraphMocCandidate, GraphMocReport, GraphPathReport, GraphQueryError,
    GraphTrendsReport, ImportTarget, InitSummary, KanbanAddReport, KanbanArchiveReport,
    KanbanBoardRecord, KanbanBoardSummary, KanbanImporter, KanbanMoveReport, KanbanTaskStatus,
    LinkResolutionProblem, MentionSuggestion, MentionSuggestionsReport, MergeCandidate,
    MoveSummary, NamedCount, NoteQuery, NoteRecord, NotesReport, OutgoingLinkRecord,
    OutgoingLinksReport, ParsedTaskNoteInput, PeriodicConfig, PeriodicNotesImporter,
    PluginImporter, QueryAst, QueryReport, RebuildQuery, RebuildReport, RefactorChange,
    RefactorReport, RelatedNoteHit, RelatedNotesQuery, RelatedNotesReport, RepairFtsQuery,
    RepairFtsReport, SavedExport, SavedExportFormat, SavedReportDefinition, SavedReportKind,
    SavedReportQuery, SavedReportSummary, ScanMode, ScanPhase, ScanProgress, ScanSummary,
    SearchHit, SearchQuery, SearchReport, SearchSort, StoredModelInfo, TaskNotesImporter,
    TasksImporter, TasksQueryResult, TemplaterImporter, TemplatesConfig, VaultPaths,
    VectorDuplicatePair, VectorDuplicatesQuery, VectorDuplicatesReport, VectorIndexPhase,
    VectorIndexProgress, VectorIndexQuery, VectorIndexReport, VectorNeighborHit,
    VectorNeighborsQuery, VectorNeighborsReport, VectorQueueReport, VectorRebuildQuery,
    VectorRepairQuery, VectorRepairReport, WatchOptions, WatchReport,
};

#[derive(Debug)]
pub struct CliError {
    exit_code: u8,
    code: &'static str,
    message: String,
}

impl CliError {
    pub(crate) fn io(error: &io::Error) -> Self {
        Self {
            exit_code: 1,
            code: "io_error",
            message: format!("failed to read current working directory: {error}"),
        }
    }

    pub(crate) fn operation(error: impl Display) -> Self {
        Self {
            exit_code: 1,
            code: "operation_failed",
            message: error.to_string(),
        }
    }

    pub(crate) fn issues(message: impl Into<String>) -> Self {
        Self {
            exit_code: 2,
            code: "issues_detected",
            message: message.into(),
        }
    }

    pub(crate) fn clap(error: &clap::Error) -> Self {
        Self {
            exit_code: 2,
            code: "invalid_arguments",
            message: error.to_string(),
        }
    }

    #[must_use]
    pub fn exit_code(&self) -> u8 {
        self.exit_code
    }

    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
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

struct BundledTextFile {
    kind: &'static str,
    relative_path: &'static str,
    contents: &'static str,
}

const BUNDLED_AGENT_TEMPLATE: BundledTextFile = BundledTextFile {
    kind: "agents_template",
    relative_path: "AGENTS.md",
    contents: include_str!("../../docs/assistant/AGENTS.template.md"),
};

const BUNDLED_SKILL_FILES: &[BundledTextFile] = &[
    BundledTextFile {
        kind: "skill",
        relative_path: "AI/Skills/note-operations.md",
        contents: include_str!("../../docs/assistant/skills/note-operations.md"),
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "AI/Skills/vault-query.md",
        contents: include_str!("../../docs/assistant/skills/vault-query.md"),
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "AI/Skills/js-api-guide.md",
        contents: include_str!("../../docs/assistant/skills/js-api-guide.md"),
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "AI/Skills/graph-exploration.md",
        contents: include_str!("../../docs/assistant/skills/graph-exploration.md"),
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "AI/Skills/daily-notes.md",
        contents: include_str!("../../docs/assistant/skills/daily-notes.md"),
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "AI/Skills/properties-and-tags.md",
        contents: include_str!("../../docs/assistant/skills/properties-and-tags.md"),
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "AI/Skills/refactoring.md",
        contents: include_str!("../../docs/assistant/skills/refactoring.md"),
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "AI/Skills/web-research.md",
        contents: include_str!("../../docs/assistant/skills/web-research.md"),
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "AI/Skills/git-workflow.md",
        contents: include_str!("../../docs/assistant/skills/git-workflow.md"),
    },
    BundledTextFile {
        kind: "skill",
        relative_path: "AI/Skills/task-management.md",
        contents: include_str!("../../docs/assistant/skills/task-management.md"),
    },
];

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct GitLogReport {
    limit: usize,
    entries: Vec<GitLogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct GitDiffReport {
    path: Option<String>,
    changed_paths: Vec<String>,
    diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct GitBlameReport {
    path: String,
    lines: Vec<GitBlameLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct WebSearchResult {
    title: String,
    url: String,
    snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct WebSearchReport {
    backend: String,
    query: String,
    results: Vec<WebSearchResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct WebFetchReport {
    url: String,
    status: u16,
    content_type: String,
    mode: String,
    content: String,
    saved: Option<String>,
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
#[serde(tag = "engine", content = "data", rename_all = "snake_case")]
enum DataviewBlockResult {
    Dql(DqlQueryResult),
    Js(DataviewJsResult),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct DataviewBlockReport {
    block_index: usize,
    line_number: i64,
    language: String,
    source: String,
    result: Option<DataviewBlockResult>,
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

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TaskShowReport {
    path: String,
    title: String,
    status: String,
    status_type: String,
    completed: bool,
    archived: bool,
    priority: String,
    due: Option<String>,
    scheduled: Option<String>,
    completed_date: Option<String>,
    date_created: Option<String>,
    date_modified: Option<String>,
    contexts: Vec<String>,
    projects: Vec<String>,
    tags: Vec<String>,
    recurrence: Option<String>,
    recurrence_anchor: Option<String>,
    complete_instances: Vec<String>,
    skipped_instances: Vec<String>,
    blocked_by: Vec<Value>,
    reminders: Vec<Value>,
    time_entries: Vec<Value>,
    custom_fields: Value,
    frontmatter: Value,
    body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TaskMutationReport {
    action: String,
    dry_run: bool,
    path: String,
    moved_from: Option<String>,
    moved_to: Option<String>,
    changes: Vec<RefactorChange>,
    #[serde(skip)]
    changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TaskAddReport {
    action: String,
    dry_run: bool,
    created: bool,
    used_nlp: bool,
    path: String,
    title: String,
    status: String,
    priority: String,
    due: Option<String>,
    scheduled: Option<String>,
    contexts: Vec<String>,
    projects: Vec<String>,
    tags: Vec<String>,
    time_estimate: Option<usize>,
    recurrence: Option<String>,
    template: Option<String>,
    frontmatter: Value,
    body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parsed_input: Option<ParsedTaskNoteInput>,
    #[serde(skip)]
    changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TaskCreateReport {
    action: String,
    dry_run: bool,
    path: String,
    task: String,
    created_note: bool,
    line_number: i64,
    used_nlp: bool,
    line: String,
    due: Option<String>,
    scheduled: Option<String>,
    priority: Option<String>,
    recurrence: Option<String>,
    contexts: Vec<String>,
    projects: Vec<String>,
    tags: Vec<String>,
    changes: Vec<RefactorChange>,
    #[serde(skip)]
    changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TaskConvertReport {
    action: String,
    dry_run: bool,
    mode: String,
    source_path: String,
    target_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_number: Option<i64>,
    title: String,
    created: bool,
    source_changes: Vec<RefactorChange>,
    task_changes: Vec<RefactorChange>,
    frontmatter: Value,
    body: String,
    #[serde(skip)]
    changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
struct LoadedTaskNote {
    path: String,
    body: String,
    frontmatter: YamlMapping,
    frontmatter_json: Value,
    indexed: vulcan_core::IndexedTaskNote,
    config: vulcan_core::VaultConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedInlineTask {
    path: String,
    line_number: i64,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedTaskConvertLine {
    start_line: i64,
    end_line: i64,
    title_input: String,
    details: String,
    replacement_prefix: String,
    completed: bool,
}

#[derive(Debug, Clone)]
struct PlannedConvertedTaskNote {
    relative_path: String,
    title: String,
    frontmatter: YamlMapping,
    body: String,
    task_changes: Vec<RefactorChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedInlineTaskCreate {
    used_nlp: bool,
    line: String,
    due: Option<String>,
    scheduled: Option<String>,
    priority: Option<String>,
    recurrence: Option<String>,
    contexts: Vec<String>,
    projects: Vec<String>,
    tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NoteEntryInsertion {
    updated: String,
    line_number: i64,
    change: RefactorChange,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct KanbanCardsReport {
    board_path: String,
    board_title: String,
    column_filter: Option<String>,
    status_filter: Option<String>,
    result_count: usize,
    cards: Vec<KanbanCardListItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct KanbanCardListItem {
    board_path: String,
    board_title: String,
    column: String,
    id: String,
    text: String,
    line_number: i64,
    block_id: Option<String>,
    symbol: String,
    tags: Vec<String>,
    outlinks: Vec<String>,
    date: Option<String>,
    time: Option<String>,
    inline_fields: Value,
    metadata: Value,
    task: Option<KanbanTaskStatus>,
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
    engine: String,
    opened_editor: bool,
    warnings: Vec<String>,
    diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TemplateInsertReport {
    template: String,
    template_source: String,
    note: String,
    mode: String,
    engine: String,
    warnings: Vec<String>,
    diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TemplatePreviewReport {
    template: String,
    template_source: String,
    path: String,
    engine: String,
    content: String,
    warnings: Vec<String>,
    diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeriodicEventReport {
    start_time: String,
    end_time: Option<String>,
    title: String,
    metadata: Value,
    tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeriodicOpenReport {
    period_type: String,
    reference_date: String,
    start_date: String,
    end_date: String,
    path: String,
    created: bool,
    opened_editor: bool,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeriodicShowReport {
    period_type: String,
    reference_date: String,
    start_date: String,
    end_date: String,
    path: String,
    content: String,
    events: Vec<PeriodicEventReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DailyListItem {
    period_type: String,
    date: String,
    path: String,
    event_count: usize,
    events: Vec<PeriodicEventReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeriodicListItem {
    period_type: String,
    date: String,
    path: String,
    event_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PeriodicGapItem {
    period_type: String,
    date: String,
    expected_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DailyAppendReport {
    period_type: String,
    reference_date: String,
    start_date: String,
    end_date: String,
    path: String,
    created: bool,
    heading: Option<String>,
    appended: bool,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DailyIcsExportReport {
    from: String,
    to: String,
    calendar_name: String,
    note_count: usize,
    event_count: usize,
    path: Option<String>,
    content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct BasesCreateReport {
    pub(crate) file: String,
    pub(crate) view_name: Option<String>,
    pub(crate) view_index: usize,
    pub(crate) dry_run: bool,
    pub(crate) path: String,
    pub(crate) folder: Option<String>,
    pub(crate) template: Option<String>,
    pub(crate) properties: BTreeMap<String, Value>,
    pub(crate) filters: Vec<String>,
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

#[derive(Debug, Clone, PartialEq, Serialize)]
struct NoteGetReport {
    path: String,
    content: String,
    frontmatter: Option<Value>,
    metadata: NoteGetMetadata,
    #[serde(skip)]
    display_lines: Vec<NoteDisplayLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NoteGetMetadata {
    heading: Option<String>,
    block_ref: Option<String>,
    lines: Option<String>,
    match_pattern: Option<String>,
    context: usize,
    no_frontmatter: bool,
    raw: bool,
    match_count: usize,
    line_spans: Vec<NoteGetLineSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NoteGetLineSpan {
    start_line: usize,
    end_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NoteDisplayLine {
    line_number: usize,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NoteSetReport {
    path: String,
    checked: bool,
    preserved_frontmatter: bool,
    diagnostics: Vec<DoctorDiagnosticIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NoteCreateReport {
    path: String,
    created: bool,
    checked: bool,
    template: Option<String>,
    engine: Option<String>,
    warnings: Vec<String>,
    diagnostics: Vec<DoctorDiagnosticIssue>,
    #[serde(skip)]
    changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NoteAppendReport {
    path: String,
    appended: bool,
    mode: String,
    checked: bool,
    created: bool,
    heading: Option<String>,
    period_type: Option<String>,
    reference_date: Option<String>,
    warnings: Vec<String>,
    diagnostics: Vec<DoctorDiagnosticIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteAppendMode {
    Append,
    Prepend,
    AfterHeading,
}

impl NoteAppendMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Append => "append",
            Self::Prepend => "prepend",
            Self::AfterHeading => "after_heading",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NotePatchReport {
    path: String,
    dry_run: bool,
    checked: bool,
    pattern: String,
    regex: bool,
    replace: String,
    match_count: usize,
    changes: Vec<RefactorChange>,
    diagnostics: Vec<DoctorDiagnosticIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NoteDoctorReport {
    path: String,
    diagnostics: Vec<DoctorDiagnosticIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ConfigImportDiscoveryItem {
    plugin: String,
    display_name: String,
    detected: bool,
    source_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ConfigImportListReport {
    importers: Vec<ConfigImportDiscoveryItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ConfigImportBatchReport {
    dry_run: bool,
    target: ImportTarget,
    detected_count: usize,
    imported_count: usize,
    updated_count: usize,
    reports: Vec<ConfigImportReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct InitSupportFile {
    path: String,
    kind: String,
    created: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct InitReport {
    #[serde(flatten)]
    summary: InitSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    importable_sources: Vec<ConfigImportDiscoveryItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    support_files: Vec<InitSupportFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    imported: Option<ConfigImportBatchReport>,
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
        | Command::Ls { .. }
        | Command::Query { .. }
        | Command::Dataview { .. }
        | Command::Tasks { .. }
        | Command::Kanban { .. }
        | Command::Update { .. }
        | Command::Unset { .. }
        | Command::Notes { .. }
        | Command::Search { .. }
        | Command::Changes { .. }
        | Command::Diff { .. }
        | Command::LinkMentions { .. }
        | Command::Rewrite { .. }
        | Command::Related { .. }
        | Command::Suggest { .. }
        | Command::Refactor { .. }
        | Command::Checkpoint { .. } => true,
        Command::Daily { command } => matches!(
            command,
            DailyCommand::Show { .. } | DailyCommand::List { .. } | DailyCommand::ExportIcs { .. }
        ),
        Command::Periodic { command, .. } => {
            matches!(command, Some(PeriodicSubcommand::List { .. }))
        }
        Command::Edit { new, .. } => !new,
        Command::Bases { command } => matches!(
            command,
            BasesCommand::Eval { .. } | BasesCommand::Tui { .. }
        ),
        Command::Saved { command } => matches!(command, SavedCommand::Run { .. }),
        Command::Export { command } => matches!(command, ExportCommand::SearchIndex { .. }),
        Command::Vectors { command } => matches!(
            command,
            VectorsCommand::Related { .. }
                | VectorsCommand::Neighbors { .. }
                | VectorsCommand::Duplicates { .. }
        ),
        Command::Template { command, .. } => {
            matches!(command, Some(TemplateSubcommand::Insert { .. }))
        }
        Command::Note { command } => matches!(
            command,
            NoteCommand::Links { .. }
                | NoteCommand::Backlinks { .. }
                | NoteCommand::Doctor { .. }
                | NoteCommand::Diff { .. }
        ),
        _ => false,
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
            &ChangeAnchor::Checkpoint(checkpoint.to_string()),
            format!("checkpoint:{checkpoint}"),
        );
    }

    if vulcan_core::is_git_repo(paths.vault_root()) {
        return diff_report_from_git(paths, &resolved.path);
    }

    diff_report_from_change_anchor(
        paths,
        &resolved.path,
        &ChangeAnchor::LastScan,
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
        changed_kinds: if changed {
            vec!["note".to_string()]
        } else {
            Vec::new()
        },
        diff: changed.then_some(diff),
    })
}

fn diff_report_from_change_anchor(
    paths: &VaultPaths,
    path: &str,
    anchor: &ChangeAnchor,
    anchor_label: String,
) -> Result<DiffReport, CliError> {
    let report = query_change_report(paths, anchor).map_err(CliError::operation)?;
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
        Some(ChangeKindStatus::Deleted) => "deleted",
        Some(ChangeKindStatus::Updated) => "changed",
        None => {
            if changed_kinds.is_empty() {
                "unchanged"
            } else {
                "changed"
            }
        }
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

fn normalize_git_scope_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn filter_vault_git_paths(paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|path| path != ".vulcan" && !path.starts_with(".vulcan/"))
        .collect()
}

fn run_git_log_command(paths: &VaultPaths, limit: usize) -> Result<GitLogReport, CliError> {
    let entries = git_recent_log(paths.vault_root(), limit).map_err(CliError::operation)?;
    Ok(GitLogReport { limit, entries })
}

fn run_git_diff_group_command(
    paths: &VaultPaths,
    path: Option<&str>,
) -> Result<GitDiffReport, CliError> {
    let normalized_path = path.map(normalize_git_scope_path);
    let changed_paths = if let Some(path) = normalized_path.as_deref() {
        let changed = filter_vault_git_paths(
            git_status(paths.vault_root())
                .map_err(CliError::operation)?
                .changed_paths(),
        );
        changed
            .into_iter()
            .filter(|candidate| candidate == path)
            .collect()
    } else {
        filter_vault_git_paths(
            git_status(paths.vault_root())
                .map_err(CliError::operation)?
                .changed_paths(),
        )
    };
    let diff =
        git_diff(paths.vault_root(), normalized_path.as_deref()).map_err(CliError::operation)?;

    Ok(GitDiffReport {
        path: normalized_path,
        changed_paths,
        diff,
    })
}

fn run_git_blame_command(paths: &VaultPaths, path: &str) -> Result<GitBlameReport, CliError> {
    let normalized = normalize_git_scope_path(path);
    let lines = git_blame(paths.vault_root(), &normalized).map_err(CliError::operation)?;
    Ok(GitBlameReport {
        path: normalized,
        lines,
    })
}

trait SearchBackend {
    fn name(&self) -> &'static str;
    fn search(&self, query: &str, limit: usize) -> Result<Vec<WebSearchResult>, CliError>;
}

struct KagiSearchBackend {
    client: Client,
    base_url: String,
    api_key: String,
}

impl SearchBackend for KagiSearchBackend {
    fn name(&self) -> &'static str {
        "kagi"
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<WebSearchResult>, CliError> {
        let limit_value = limit.max(1).to_string();
        let response = self
            .client
            .get(&self.base_url)
            .header(AUTHORIZATION, format!("Bot {}", self.api_key))
            .query(&[("q", query), ("limit", limit_value.as_str())])
            .send()
            .map_err(CliError::operation)?;
        if !response.status().is_success() {
            return Err(CliError::operation(format!(
                "web search failed with status {}",
                response.status()
            )));
        }
        let payload = response.json::<Value>().map_err(CliError::operation)?;
        parse_search_results(&payload).ok_or_else(|| {
            CliError::operation("web search backend returned an unexpected payload shape")
        })
    }
}

fn run_web_search_command(
    paths: &VaultPaths,
    query: &str,
    backend_override: Option<&str>,
    limit: usize,
) -> Result<WebSearchReport, CliError> {
    let config = load_vault_config(paths).config.web;
    let backend_name = backend_override.unwrap_or(config.search.backend.as_str());
    let client = build_web_client(&config.user_agent)?;
    let backend: Box<dyn SearchBackend> = match backend_name {
        "kagi" => {
            let api_key = std::env::var(&config.search.api_key_env).map_err(|_| {
                CliError::operation(format!(
                    "missing web search API key env var {}",
                    config.search.api_key_env
                ))
            })?;
            Box::new(KagiSearchBackend {
                client,
                base_url: config.search.base_url,
                api_key,
            })
        }
        other => {
            return Err(CliError::operation(format!(
                "unsupported web search backend: {other}"
            )));
        }
    };

    let results = backend.search(query, limit)?;
    Ok(WebSearchReport {
        backend: backend.name().to_string(),
        query: query.to_string(),
        results,
    })
}

fn run_web_fetch_command(
    paths: &VaultPaths,
    url: &str,
    mode: WebFetchMode,
    save: Option<&PathBuf>,
    extract_article: bool,
) -> Result<WebFetchReport, CliError> {
    let config = load_vault_config(paths).config.web;
    let client = build_web_client(&config.user_agent)?;
    if !robots_allow_fetch(&client, url, &config.user_agent) {
        return Err(CliError::operation(
            "fetch blocked by robots.txt (best-effort check)",
        ));
    }

    let response = client.get(url).send().map_err(CliError::operation)?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = response.bytes().map_err(CliError::operation)?;
    let content = render_fetched_content(&bytes, &content_type, mode, extract_article);
    let saved = save.map(|path| path.to_string_lossy().to_string());

    if let Some(path) = save {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        match mode {
            WebFetchMode::Raw => fs::write(path, &bytes).map_err(CliError::operation)?,
            WebFetchMode::Html | WebFetchMode::Markdown => {
                fs::write(path, content.as_bytes()).map_err(CliError::operation)?;
            }
        }
    }

    Ok(WebFetchReport {
        url: url.to_string(),
        status,
        content_type,
        mode: format!("{mode:?}").to_ascii_lowercase(),
        content,
        saved,
    })
}

fn build_web_client(user_agent: &str) -> Result<Client, CliError> {
    Client::builder()
        .user_agent(user_agent)
        .build()
        .map_err(CliError::operation)
}

fn parse_search_results(payload: &Value) -> Option<Vec<WebSearchResult>> {
    let results = payload
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| payload.get("results").and_then(Value::as_array))?;

    Some(
        results
            .iter()
            .filter_map(|item| {
                let title = item
                    .get("title")
                    .or_else(|| item.get("t"))
                    .and_then(Value::as_str)?;
                let url = item
                    .get("url")
                    .or_else(|| item.get("u"))
                    .and_then(Value::as_str)?;
                let snippet = item
                    .get("snippet")
                    .or_else(|| item.get("desc"))
                    .or_else(|| item.get("body"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                Some(WebSearchResult {
                    title: title.to_string(),
                    url: url.to_string(),
                    snippet: snippet.to_string(),
                })
            })
            .collect(),
    )
}

fn render_fetched_content(
    bytes: &[u8],
    content_type: &str,
    mode: WebFetchMode,
    extract_article: bool,
) -> String {
    let rendered = String::from_utf8_lossy(bytes).to_string();
    match mode {
        WebFetchMode::Raw | WebFetchMode::Html => rendered,
        WebFetchMode::Markdown => {
            if content_type.contains("html") {
                html_to_markdown(&rendered, extract_article)
            } else {
                rendered
            }
        }
    }
}

fn robots_allow_fetch(client: &Client, url: &str, user_agent: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return true;
    };
    let Some(host) = parsed.host_str() else {
        return true;
    };
    let authority = parsed
        .port()
        .map_or_else(|| host.to_string(), |port| format!("{host}:{port}"));
    let robots_url = format!("{}://{authority}/robots.txt", parsed.scheme());
    let Ok(response) = client.get(robots_url).send() else {
        return true;
    };
    if !response.status().is_success() {
        return true;
    }
    let Ok(robots) = response.text() else {
        return true;
    };

    robots_allows_path(&robots, parsed.path(), user_agent)
}

fn robots_allows_path(robots: &str, path: &str, user_agent: &str) -> bool {
    let mut applies = false;
    let normalized_agent = user_agent.to_ascii_lowercase();

    for raw_line in robots.lines() {
        let line = raw_line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();

        if key == "user-agent" {
            let value = value.to_ascii_lowercase();
            applies = value == "*" || normalized_agent.starts_with(&value);
        } else if applies && key == "disallow" && !value.is_empty() && path.starts_with(value) {
            return false;
        }
    }

    true
}

fn html_to_markdown(html: &str, extract_article: bool) -> String {
    let relevant = if extract_article {
        extract_article_html(html).unwrap_or(html)
    } else {
        html
    };
    let mut rendered = Regex::new(r"(?is)<script[^>]*>.*?</script>")
        .expect("regex should compile")
        .replace_all(relevant, "")
        .into_owned();
    rendered = Regex::new(r"(?is)<style[^>]*>.*?</style>")
        .expect("regex should compile")
        .replace_all(&rendered, "")
        .into_owned();
    for (pattern, replacement) in [
        (r"(?i)<br\s*/?>", "\n"),
        (
            r"(?i)</(p|div|section|article|main|body|h1|h2|h3|h4|h5|h6|tr)>",
            "\n",
        ),
        (r"(?i)<li[^>]*>", "- "),
        (r"(?i)</li>", "\n"),
    ] {
        rendered = Regex::new(pattern)
            .expect("regex should compile")
            .replace_all(&rendered, replacement)
            .into_owned();
    }
    rendered = Regex::new(r"(?is)<[^>]+>")
        .expect("regex should compile")
        .replace_all(&rendered, "")
        .into_owned();
    rendered = decode_html_entities(&rendered);
    Regex::new(r"\n{3,}")
        .expect("regex should compile")
        .replace_all(rendered.trim(), "\n\n")
        .into_owned()
}

fn extract_article_html(html: &str) -> Option<&str> {
    for pattern in [
        r"(?is)<article[^>]*>(.*?)</article>",
        r"(?is)<main[^>]*>(.*?)</main>",
        r"(?is)<body[^>]*>(.*?)</body>",
    ] {
        let regex = Regex::new(pattern).expect("regex should compile");
        if let Some(captures) = regex.captures(html) {
            if let Some(content) = captures.get(1) {
                return Some(content.as_str());
            }
        }
    }
    None
}

fn decode_html_entities(input: &str) -> String {
    [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
    ]
    .into_iter()
    .fold(input.to_string(), |acc, (from, to)| acc.replace(from, to))
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

fn run_dataview_query_js_command(
    paths: &VaultPaths,
    js: &str,
    file: Option<&str>,
) -> Result<DataviewJsResult, CliError> {
    evaluate_dataview_js_query(paths, js, file).map_err(CliError::operation)
}

fn strip_shebang_line(source: &str) -> &str {
    if let Some(stripped) = source.strip_prefix("#!") {
        stripped
            .split_once('\n')
            .map_or("", |(_, remainder)| remainder)
    } else {
        source
    }
}

fn resolve_named_run_script_path(paths: &VaultPaths, script: &str) -> Option<PathBuf> {
    let scripts_root = paths.vulcan_dir().join("scripts");
    [PathBuf::from(script), PathBuf::from(format!("{script}.js"))]
        .into_iter()
        .map(|candidate| scripts_root.join(candidate))
        .find(|candidate| candidate.is_file())
}

fn load_run_script_source(
    paths: &VaultPaths,
    script: Option<&str>,
    script_mode: bool,
) -> Result<String, CliError> {
    if let Some(script) = script {
        let direct = PathBuf::from(script);
        let path = if script_mode || direct.is_file() {
            direct
        } else if let Some(named) = resolve_named_run_script_path(paths, script) {
            named
        } else {
            return Err(CliError::operation(format!(
                "script not found: {script}; expected a file path or .vulcan/scripts entry"
            )));
        };
        return fs::read_to_string(path).map_err(CliError::operation);
    }

    if io::stdin().is_terminal() {
        return Err(CliError::operation(
            "`vulcan run` requires a script path, stdin, or an interactive terminal session",
        ));
    }

    let mut buffer = String::new();
    io::stdin()
        .read_to_string(&mut buffer)
        .map_err(CliError::operation)?;
    Ok(buffer)
}

fn run_js_command(
    paths: &VaultPaths,
    script: Option<&str>,
    script_mode: bool,
) -> Result<DataviewJsResult, CliError> {
    let source = load_run_script_source(paths, script, script_mode)?;
    evaluate_dataview_js_query(paths, strip_shebang_line(&source), None)
        .map_err(CliError::operation)
}

fn run_js_repl(paths: &VaultPaths, output: OutputFormat) -> Result<(), CliError> {
    let mut line = String::new();
    let stdin = io::stdin();
    let mut handle = stdin.lock();

    loop {
        print!("vulcan> ");
        io::stdout().flush().map_err(CliError::operation)?;
        line.clear();
        let read = io::BufRead::read_line(&mut handle, &mut line).map_err(CliError::operation)?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if matches!(trimmed, ".exit" | ".quit") {
            break;
        }

        match evaluate_dataview_js_query(paths, trimmed, None).map_err(CliError::operation) {
            Ok(result) => {
                print_dataview_js_result(output, &result, false)?;
            }
            Err(error) => {
                eprintln!("error: {error}");
            }
        }
    }

    Ok(())
}

fn run_dataview_eval_command(
    paths: &VaultPaths,
    file: &str,
    block: Option<usize>,
) -> Result<DataviewEvalReport, CliError> {
    let blocks = load_dataview_blocks(paths, file, block).map_err(CliError::operation)?;
    let file = blocks
        .first()
        .map_or_else(|| file.to_string(), |block| block.file.clone());
    let mut reports = Vec::with_capacity(blocks.len());

    for block in blocks {
        let (result, error) = if block.language == "dataview" {
            match evaluate_dql(paths, &block.source, Some(&block.file)) {
                Ok(result) => (Some(DataviewBlockResult::Dql(result)), None),
                Err(error) => (None, Some(error.to_string())),
            }
        } else if block.language == "dataviewjs" {
            match run_dataview_query_js_command(paths, &block.source, Some(&block.file)) {
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

fn run_tasks_query_command(paths: &VaultPaths, source: &str) -> Result<TasksQueryResult, CliError> {
    let config = load_vault_config(paths).config.tasks;
    let effective_source = tasks_query_source(&config, source, false);
    let mut result = evaluate_tasks_query(paths, &effective_source).map_err(CliError::operation)?;
    strip_global_filter_from_output(&mut result, &config);
    Ok(result)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TaskNotesViewListItem {
    file: String,
    file_stem: String,
    view_name: Option<String>,
    view_type: String,
    supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TaskNotesViewListReport {
    views: Vec<TaskNotesViewListItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskNotesViewTarget {
    file: String,
    view_name: Option<String>,
}

fn run_tasks_view_list_command(paths: &VaultPaths) -> Result<TaskNotesViewListReport, CliError> {
    let mut files = Vec::new();
    let root = paths.vault_root().join("TaskNotes/Views");
    collect_tasknotes_base_files(&root, "TaskNotes/Views", &mut files)?;

    let mut views = Vec::new();
    for file in files {
        let info = inspect_base_file(paths, &file).map_err(CliError::operation)?;
        let file_stem = Path::new(&file)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        for view in info.views {
            views.push(TaskNotesViewListItem {
                file: file.clone(),
                file_stem: file_stem.clone(),
                supported: matches!(
                    view.view_type.to_ascii_lowercase().as_str(),
                    "table" | "tasknotestasklist" | "tasknoteskanban"
                ),
                view_name: view.name,
                view_type: view.view_type,
            });
        }
    }

    views.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then_with(|| left.view_name.cmp(&right.view_name))
            .then_with(|| left.view_type.cmp(&right.view_type))
    });

    Ok(TaskNotesViewListReport { views })
}

fn run_tasks_view_command(paths: &VaultPaths, name: &str) -> Result<BasesEvalReport, CliError> {
    let target = resolve_tasknotes_view_target(paths, name)?;
    let mut report = evaluate_base_file(paths, &target.file).map_err(CliError::operation)?;
    if let Some(view_name) = target.view_name.as_deref() {
        report
            .views
            .retain(|view| view.name.as_deref() == Some(view_name));
        if report.views.is_empty() {
            return Err(CliError::operation(format!(
                "view `{view_name}` was not found in {}",
                target.file
            )));
        }
    }
    Ok(report)
}

fn resolve_tasknotes_view_target(
    paths: &VaultPaths,
    name: &str,
) -> Result<TaskNotesViewTarget, CliError> {
    if is_explicit_tasknotes_view_path(name) {
        let normalized = normalize_relative_input_path(
            name,
            RelativePathOptions {
                expected_extension: Some("base"),
                append_extension_if_missing: true,
            },
        )
        .map_err(CliError::operation)?;
        let _ = inspect_base_file(paths, &normalized).map_err(CliError::operation)?;
        return Ok(TaskNotesViewTarget {
            file: normalized,
            view_name: None,
        });
    }

    let catalog = run_tasks_view_list_command(paths)?;
    if let Some(target) = unique_tasknotes_view_name_match(&catalog.views, name)? {
        return Ok(target);
    }
    if let Some(target) = unique_tasknotes_view_file_match(&catalog.views, name)? {
        return Ok(target);
    }

    Err(CliError::operation(format!(
        "no TaskNotes view matched `{name}`"
    )))
}

fn collect_tasknotes_base_files(
    directory: &Path,
    relative: &str,
    files: &mut Vec<String>,
) -> Result<(), CliError> {
    if !directory.exists() {
        return Ok(());
    }

    let entries = fs::read_dir(directory).map_err(CliError::operation)?;
    for entry in entries {
        let entry = entry.map_err(CliError::operation)?;
        let path = entry.path();
        let relative_path = format!("{relative}/{}", entry.file_name().to_string_lossy());
        let file_type = entry.file_type().map_err(CliError::operation)?;
        if file_type.is_dir() {
            collect_tasknotes_base_files(&path, &relative_path, files)?;
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("base"))
        {
            files.push(relative_path);
        }
    }
    Ok(())
}

fn is_explicit_tasknotes_view_path(name: &str) -> bool {
    name.contains('/')
        || name.contains('\\')
        || Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("base"))
}

fn unique_tasknotes_view_name_match(
    views: &[TaskNotesViewListItem],
    name: &str,
) -> Result<Option<TaskNotesViewTarget>, CliError> {
    let matches = views
        .iter()
        .filter(|view| {
            view.view_name
                .as_deref()
                .is_some_and(|view_name| view_name.eq_ignore_ascii_case(name))
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Ok(None);
    }
    if matches.len() > 1 {
        let options = matches
            .iter()
            .map(|view| {
                format!(
                    "{} ({})",
                    view.view_name.as_deref().unwrap_or("<unnamed>"),
                    view.file
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CliError::operation(format!(
            "multiple TaskNotes views matched `{name}`: {options}"
        )));
    }

    Ok(Some(TaskNotesViewTarget {
        file: matches[0].file.clone(),
        view_name: matches[0].view_name.clone(),
    }))
}

fn unique_tasknotes_view_file_match(
    views: &[TaskNotesViewListItem],
    name: &str,
) -> Result<Option<TaskNotesViewTarget>, CliError> {
    let matches = views
        .iter()
        .filter(|view| {
            tasknotes_view_file_aliases(&view.file_stem)
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Ok(None);
    }

    let file = &matches[0].file;
    if matches.iter().any(|view| view.file != *file) {
        let options = matches
            .iter()
            .map(|view| view.file.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CliError::operation(format!(
            "multiple TaskNotes view files matched `{name}`: {options}"
        )));
    }

    Ok(Some(TaskNotesViewTarget {
        file: file.clone(),
        view_name: None,
    }))
}

fn tasknotes_view_file_aliases(file_stem: &str) -> Vec<String> {
    let mut aliases = vec![file_stem.to_string()];
    if let Some(alias) = file_stem.strip_suffix("-default") {
        aliases.push(alias.to_string());
    }
    aliases
}

#[derive(Debug)]
struct TaskMutationPlan {
    changes: Vec<RefactorChange>,
    moved_to: Option<String>,
}

fn current_utc_timestamp_string() -> String {
    TemplateTimestamp::current().default_strings().datetime
}

fn load_tasknote_note(paths: &VaultPaths, task: &str) -> Result<LoadedTaskNote, CliError> {
    let path = resolve_existing_note_path(paths, task)?;
    let source = fs::read_to_string(paths.vault_root().join(&path)).map_err(CliError::operation)?;
    let config = load_vault_config(paths).config;
    let parsed = vulcan_core::parse_document(&source, &config);
    let indexed_properties = extract_indexed_properties(&parsed, &config)
        .map_err(CliError::operation)?
        .map(|properties| serde_json::from_str::<Value>(&properties.canonical_json))
        .transpose()
        .map_err(CliError::operation)?;
    let (frontmatter, body) =
        parse_frontmatter_document(&source, false).map_err(CliError::operation)?;
    let frontmatter = frontmatter.unwrap_or_default();
    let frontmatter_json = load_note_index(paths)
        .ok()
        .and_then(|index| {
            index
                .into_values()
                .find(|note| note.document_path == path)
                .map(|note| note.properties)
        })
        .or(indexed_properties)
        .unwrap_or_else(|| Value::Object(Map::new()));
    let title = Path::new(&path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    let indexed =
        extract_tasknote(&path, title, &frontmatter_json, &config.tasknotes).or_else(|| {
            let mut permissive = config.tasknotes.clone();
            permissive.excluded_folders.clear();
            extract_tasknote(&path, title, &frontmatter_json, &permissive)
        });
    let indexed = indexed
        .ok_or_else(|| CliError::operation(format!("note is not a TaskNotes task: {task}")))?;

    Ok(LoadedTaskNote {
        path,
        body: normalize_tasknote_body(&body),
        frontmatter,
        frontmatter_json,
        indexed,
        config,
    })
}

fn resolve_inline_task(paths: &VaultPaths, task: &str) -> Result<ResolvedInlineTask, CliError> {
    let note_index = load_note_index(paths).map_err(CliError::operation)?;

    if let Some((note_ref, line_number)) = parse_task_line_reference(task) {
        let path = resolve_existing_note_path(paths, note_ref)?;
        if let Some(task) = find_inline_task_in_path(&note_index, &path, line_number) {
            return Ok(task);
        }
        return Err(CliError::operation(format!(
            "no inline task at {path}:{line_number}"
        )));
    }

    if let Ok(path) = resolve_existing_note_path(paths, task) {
        let mut tasks = inline_tasks_for_path(&note_index, &path);
        return match tasks.len() {
            0 => Err(CliError::operation(format!(
                "note has no inline tasks: {path}"
            ))),
            1 => Ok(tasks.remove(0)),
            _ => Err(CliError::operation(format!(
                "multiple inline tasks found in {path}; use <note>:<line> or exact task text"
            ))),
        };
    }

    let mut matches = note_index
        .values()
        .flat_map(inline_tasks_for_note)
        .filter(|candidate| {
            candidate.text == task || candidate.text.eq_ignore_ascii_case(task.trim())
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line_number.cmp(&right.line_number))
    });
    matches
        .dedup_by(|left, right| left.path == right.path && left.line_number == right.line_number);

    match matches.len() {
        0 => Err(CliError::operation(format!(
            "inline task not found: {task}"
        ))),
        1 => Ok(matches.remove(0)),
        _ => Err(CliError::operation(format!(
            "multiple inline tasks match '{task}'; use <note>:<line> to disambiguate"
        ))),
    }
}

fn parse_task_line_reference(task: &str) -> Option<(&str, i64)> {
    let (note, line_number) = task.rsplit_once(':')?;
    let line_number = line_number.trim().parse::<i64>().ok()?;
    (line_number > 0).then_some((note.trim(), line_number))
}

fn inline_tasks_for_path(
    note_index: &HashMap<String, NoteRecord>,
    path: &str,
) -> Vec<ResolvedInlineTask> {
    note_index
        .values()
        .find(|note| note.document_path == path)
        .map_or_else(Vec::new, inline_tasks_for_note)
}

fn find_inline_task_in_path(
    note_index: &HashMap<String, NoteRecord>,
    path: &str,
    line_number: i64,
) -> Option<ResolvedInlineTask> {
    inline_tasks_for_path(note_index, path)
        .into_iter()
        .find(|candidate| candidate.line_number == line_number)
}

fn inline_tasks_for_note(note: &NoteRecord) -> Vec<ResolvedInlineTask> {
    note.tasks
        .iter()
        .filter(|task| task.properties.get("taskSource").and_then(Value::as_str) != Some("file"))
        .map(|task| ResolvedInlineTask {
            path: note.document_path.clone(),
            line_number: task.line_number,
            text: task.text.clone(),
        })
        .collect()
}

fn normalize_tasknote_body(body: &str) -> String {
    let body = body.trim_start_matches('\n').trim_end_matches('\n');
    if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n")
    }
}

fn tasknote_frontmatter_key(config: &vulcan_core::VaultConfig, property: &str) -> String {
    let property = property.trim();
    let mapping = &config.tasknotes.field_mapping;
    match property {
        "title" => mapping.title.clone(),
        "status" => mapping.status.clone(),
        "priority" => mapping.priority.clone(),
        "due" => mapping.due.clone(),
        "scheduled" => mapping.scheduled.clone(),
        "contexts" => mapping.contexts.clone(),
        "projects" => mapping.projects.clone(),
        "timeEstimate" | "time_estimate" => mapping.time_estimate.clone(),
        "completedDate" | "completed_date" => mapping.completed_date.clone(),
        "dateCreated" | "date_created" => mapping.date_created.clone(),
        "dateModified" | "date_modified" => mapping.date_modified.clone(),
        "recurrence" => mapping.recurrence.clone(),
        "recurrenceAnchor" | "recurrence_anchor" => mapping.recurrence_anchor.clone(),
        "timeEntries" | "time_entries" => mapping.time_entries.clone(),
        "completeInstances" | "complete_instances" => mapping.complete_instances.clone(),
        "skippedInstances" | "skipped_instances" => mapping.skipped_instances.clone(),
        "blockedBy" | "blocked_by" | "blocked-by" => mapping.blocked_by.clone(),
        "reminders" => mapping.reminders.clone(),
        other => other.to_string(),
    }
}

fn parse_tasknote_cli_value(value: &str) -> YamlValue {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return YamlValue::String(String::new());
    }
    match serde_yaml::from_str::<YamlValue>(trimmed) {
        Ok(parsed) => parsed,
        Err(_) => YamlValue::String(value.to_string()),
    }
}

fn tasknote_change_summary(value: Option<&YamlValue>) -> String {
    match value {
        None => "<missing>".to_string(),
        Some(YamlValue::String(text)) => text.clone(),
        Some(value) => serde_json::to_string(&serde_json::to_value(value).unwrap_or(Value::Null))
            .unwrap_or_else(|_| "<unserializable>".to_string()),
    }
}

fn set_tasknote_frontmatter_value(
    frontmatter: &mut YamlMapping,
    key: &str,
    value: Option<YamlValue>,
) -> Option<RefactorChange> {
    let yaml_key = YamlValue::String(key.to_string());
    let before = frontmatter.get(&yaml_key).cloned();

    if let Some(value) = value {
        if before.as_ref() == Some(&value) {
            return None;
        }
        frontmatter.insert(yaml_key, value.clone());
        Some(RefactorChange {
            before: format!("{key}: {}", tasknote_change_summary(before.as_ref())),
            after: format!("{key}: {}", tasknote_change_summary(Some(&value))),
        })
    } else {
        before.as_ref()?;
        frontmatter.remove(&yaml_key);
        Some(RefactorChange {
            before: format!("{key}: {}", tasknote_change_summary(before.as_ref())),
            after: format!("{key}: <removed>"),
        })
    }
}

fn yaml_string_list(value: Option<&YamlValue>) -> Vec<String> {
    match value {
        Some(YamlValue::String(text)) => vec![text.clone()],
        Some(YamlValue::Sequence(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(ToOwned::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

fn yaml_string(value: &YamlValue) -> Option<String> {
    match value {
        YamlValue::Bool(flag) => Some(flag.to_string()),
        YamlValue::Number(number) => Some(number.to_string()),
        YamlValue::String(text) => Some(text.clone()),
        _ => None,
    }
}

fn first_completed_tasknote_status(config: &vulcan_core::VaultConfig) -> String {
    config
        .tasknotes
        .statuses
        .iter()
        .find(|status| status.is_completed)
        .map_or_else(|| "done".to_string(), |status| status.value.clone())
}

fn tasknote_reference_ms() -> i64 {
    parse_date_like_string(&TemplateTimestamp::current().default_date_string()).unwrap_or_default()
}

fn resolve_tasknote_date_input(
    config: &vulcan_core::VaultConfig,
    value: &str,
    scheduled: bool,
) -> Result<String, CliError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(CliError::operation("date value cannot be empty"));
    }
    if parse_date_like_string(trimmed).is_some() {
        return Ok(trimmed.to_string());
    }

    let prefix = if scheduled { "scheduled" } else { "due" };
    let parsed = parse_tasknote_natural_language(
        &format!("placeholder {prefix} {trimmed}"),
        &config.tasknotes,
        tasknote_reference_ms(),
    );
    let resolved = if scheduled {
        parsed.scheduled
    } else {
        parsed.due
    };
    resolved.ok_or_else(|| CliError::operation(format!("failed to parse date value: {value}")))
}

fn normalize_tasknote_context(context: &str) -> Option<String> {
    let trimmed = context.trim().trim_matches('"').trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.starts_with('@') {
        Some(trimmed.to_string())
    } else {
        Some(format!("@{trimmed}"))
    }
}

fn normalize_tasknote_tag(tag: &str) -> Option<String> {
    let trimmed = tag.trim().trim_matches('"').trim().trim_start_matches('#');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn normalize_tasknote_project(project: &str) -> Option<String> {
    let trimmed = project.trim().trim_matches('"').trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
        Some(trimmed.to_string())
    } else {
        Some(format!("[[{trimmed}]]"))
    }
}

fn dedup_tasknote_values<I, F>(values: I, normalize: F) -> Vec<String>
where
    I: IntoIterator<Item = String>,
    F: Fn(&str) -> Option<String>,
{
    let mut deduped = Vec::new();
    for value in values {
        let Some(normalized) = normalize(&value) else {
            continue;
        };
        if !deduped
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&normalized))
        {
            deduped.push(normalized);
        }
    }
    deduped
}

fn resolve_tasks_create_target(
    paths: &VaultPaths,
    note: Option<&str>,
) -> Result<(String, Option<String>), CliError> {
    if let Some(note) = note {
        return match resolve_note_reference(paths, note) {
            Ok(resolved) => Ok((resolved.path, None)),
            Err(GraphQueryError::AmbiguousIdentifier { .. }) => Err(CliError::operation(format!(
                "note identifier '{note}' is ambiguous"
            ))),
            Err(GraphQueryError::CacheMissing | GraphQueryError::NoteNotFound { .. }) => {
                Ok((normalize_note_path(note)?, None))
            }
            Err(error) => Err(CliError::operation(error)),
        };
    }

    let config = load_vault_config(paths).config;
    Ok((
        normalize_note_path(&config.inbox.path)?,
        config.inbox.heading,
    ))
}

fn task_text_contains_tag(text: &str, tag: &str) -> bool {
    let normalized = normalize_tag_name(tag);
    text.split_whitespace()
        .any(|token| normalize_tag_name(token) == normalized)
}

fn inline_task_priority_marker(
    config: &vulcan_core::VaultConfig,
    priority: &str,
) -> Option<&'static str> {
    let normalized = priority.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "none" => None,
        "highest" => Some("⏫"),
        "high" | "urgent" => Some("🔺"),
        "medium" | "normal" => Some("🔼"),
        "low" => Some("🔽"),
        "lowest" => Some("⏬"),
        _ => config
            .tasknotes
            .priorities
            .iter()
            .find(|candidate| candidate.value.eq_ignore_ascii_case(priority))
            .and_then(|candidate| match candidate.weight {
                i32::MIN..=0 => None,
                1 => Some("🔽"),
                2 => Some("🔼"),
                3 => Some("🔺"),
                _ => Some("⏫"),
            }),
    }
}

#[allow(clippy::too_many_lines)]
fn build_inline_task_create_plan(
    config: &vulcan_core::VaultConfig,
    text: &str,
    due: Option<&str>,
    priority: Option<&str>,
) -> Result<PlannedInlineTaskCreate, CliError> {
    let reference_ms = tasknote_reference_ms();
    let raw_text = text.trim();
    if raw_text.is_empty() {
        return Err(CliError::operation("task text cannot be empty"));
    }

    let used_nlp = config.tasknotes.enable_natural_language_input;
    let parsed_input = used_nlp
        .then(|| parse_tasknote_natural_language(raw_text, &config.tasknotes, reference_ms));
    let title = parsed_input
        .as_ref()
        .map(|parsed| parsed.title.as_str())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(raw_text)
        .trim()
        .to_string();
    if title.is_empty() {
        return Err(CliError::operation("task title cannot be empty"));
    }

    let due = match due {
        Some(value) => Some(resolve_tasknote_date_input(config, value, false)?),
        None => parsed_input.as_ref().and_then(|parsed| parsed.due.clone()),
    };
    let scheduled = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.scheduled.clone());
    let priority = priority
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            parsed_input
                .as_ref()
                .and_then(|parsed| parsed.priority.clone())
        });
    if let Some(priority) = priority.as_deref() {
        if inline_task_priority_marker(config, priority).is_none() {
            return Err(CliError::operation(format!(
                "unsupported inline task priority: {priority}"
            )));
        }
    }

    let contexts = parsed_input
        .as_ref()
        .map_or_else(Vec::new, |parsed| parsed.contexts.clone());
    let projects = parsed_input
        .as_ref()
        .map_or_else(Vec::new, |parsed| parsed.projects.clone());
    let mut tags = parsed_input
        .as_ref()
        .map_or_else(Vec::new, |parsed| parsed.tags.clone());
    if let Some(global_filter) = config
        .tasks
        .global_filter
        .as_deref()
        .and_then(normalize_tasknote_tag)
    {
        if !tags
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&global_filter))
            && !task_text_contains_tag(&title, &global_filter)
        {
            tags.push(global_filter);
        }
    }
    tags = dedup_tasknote_values(tags, normalize_tasknote_tag);
    let recurrence = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.recurrence.clone());

    let mut tokens = vec![title.clone()];
    tokens.extend(contexts.iter().cloned());
    tokens.extend(tags.iter().map(|tag| format!("#{tag}")));
    tokens.extend(projects.iter().cloned());
    if let Some(due) = due.as_ref() {
        tokens.push(format!("🗓️ {due}"));
    }
    if let Some(scheduled) = scheduled.as_ref() {
        tokens.push(format!("⏳ {scheduled}"));
    }
    if config.tasks.set_created_date {
        tokens.push(format!("➕ {}", current_utc_date_string()));
    }
    if let Some(priority) = priority
        .as_deref()
        .and_then(|value| inline_task_priority_marker(config, value))
    {
        tokens.push(priority.to_string());
    }
    if let Some(recurrence) = recurrence.as_ref() {
        tokens.push(format!("🔁 {recurrence}"));
    }

    Ok(PlannedInlineTaskCreate {
        used_nlp,
        line: format!("- [ ] {}", tokens.join(" ")),
        due,
        scheduled,
        priority,
        recurrence,
        contexts,
        projects,
        tags,
    })
}

fn yaml_string_sequence(values: &[String]) -> YamlValue {
    YamlValue::Sequence(
        values
            .iter()
            .cloned()
            .map(YamlValue::String)
            .collect::<Vec<_>>(),
    )
}

fn tasknote_frontmatter_json(frontmatter: &YamlMapping) -> Value {
    serde_json::to_value(YamlValue::Mapping(frontmatter.clone())).unwrap_or(Value::Null)
}

fn sanitize_tasknote_filename(title: &str) -> String {
    let mut sanitized = title
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => ' ',
            _ => character,
        })
        .collect::<String>();
    sanitized = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    sanitized = sanitized.trim_matches(['.', ' ']).to_string();
    if sanitized.is_empty() {
        "Untitled Task".to_string()
    } else {
        sanitized
    }
}

fn load_tasknote_template(
    paths: &VaultPaths,
    config: &vulcan_core::VaultConfig,
    template_name: &str,
    target_path: &str,
) -> Result<(Option<YamlMapping>, String), CliError> {
    let templates = discover_templates(
        paths,
        config.templates.obsidian_folder.as_deref(),
        config.templates.templater_folder.as_deref(),
    )?;
    let template_file = resolve_template_file(paths, &templates.templates, template_name)?;
    let template_source =
        fs::read_to_string(&template_file.absolute_path).map_err(CliError::operation)?;
    let vars = HashMap::new();
    let rendered = render_template_request(TemplateRenderRequest {
        paths,
        vault_config: config,
        templates: &templates.templates,
        template_path: Some(&template_file.absolute_path),
        template_text: &template_source,
        target_path,
        target_contents: None,
        engine: TemplateEngineKind::Auto,
        vars: &vars,
        allow_mutations: true,
        run_mode: TemplateRunMode::Create,
    })?;
    let (frontmatter, body) =
        parse_frontmatter_document(&rendered.content, true).map_err(CliError::operation)?;
    Ok((frontmatter, normalize_tasknote_body(&body)))
}

fn tasknote_title_from_path(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or("Untitled Task")
        .to_string()
}

fn prepare_existing_note_tasknote_frontmatter(
    frontmatter: &mut YamlMapping,
    title_hint: &str,
    config: &vulcan_core::VaultConfig,
) -> Vec<RefactorChange> {
    let mapping = &config.tasknotes.field_mapping;
    let mut changes = Vec::new();

    let title_key = YamlValue::String(mapping.title.clone());
    let title = frontmatter
        .get(&title_key)
        .and_then(yaml_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| title_hint.to_string());
    if let Some(change) =
        set_tasknote_frontmatter_value(frontmatter, &mapping.title, Some(YamlValue::String(title)))
    {
        changes.push(change);
    }

    let status_key = YamlValue::String(mapping.status.clone());
    let status = frontmatter
        .get(&status_key)
        .and_then(yaml_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| config.tasknotes.default_status.clone());
    if let Some(change) = set_tasknote_frontmatter_value(
        frontmatter,
        &mapping.status,
        Some(YamlValue::String(status)),
    ) {
        changes.push(change);
    }

    let priority_key = YamlValue::String(mapping.priority.clone());
    let priority = frontmatter
        .get(&priority_key)
        .and_then(yaml_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| config.tasknotes.default_priority.clone());
    if let Some(change) = set_tasknote_frontmatter_value(
        frontmatter,
        &mapping.priority,
        Some(YamlValue::String(priority)),
    ) {
        changes.push(change);
    }

    let created_key = YamlValue::String(mapping.date_created.clone());
    let date_created = frontmatter
        .get(&created_key)
        .and_then(yaml_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(current_utc_timestamp_string);
    if let Some(change) = set_tasknote_frontmatter_value(
        frontmatter,
        &mapping.date_created,
        Some(YamlValue::String(date_created)),
    ) {
        changes.push(change);
    }

    if let Some(change) = set_tasknote_frontmatter_value(
        frontmatter,
        &mapping.date_modified,
        Some(YamlValue::String(current_utc_timestamp_string())),
    ) {
        changes.push(change);
    }

    if config.tasknotes.identification_method == vulcan_core::TaskNotesIdentificationMethod::Tag {
        let tags_key = YamlValue::String("tags".to_string());
        let mut tags = yaml_string_list(frontmatter.get(&tags_key));
        if let Some(task_tag) = normalize_tasknote_tag(&config.tasknotes.task_tag) {
            if !tags
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&task_tag))
            {
                tags.insert(0, task_tag);
                if let Some(change) = set_tasknote_frontmatter_value(
                    frontmatter,
                    "tags",
                    Some(yaml_string_sequence(&tags)),
                ) {
                    changes.push(change);
                }
            }
        }
    } else if let Some(property_name) = config.tasknotes.task_property_name.as_ref() {
        let value = config
            .tasknotes
            .task_property_value
            .as_ref()
            .map_or(YamlValue::Bool(true), |value| {
                YamlValue::String(value.clone())
            });
        if let Some(change) =
            set_tasknote_frontmatter_value(frontmatter, property_name, Some(value))
        {
            changes.push(change);
        }
    }

    changes
}

fn tasknote_link_target(path: &str) -> String {
    path.strip_suffix(".md").unwrap_or(path).to_string()
}

fn extract_line_content_as_task_title(line: &str) -> String {
    let mut cleaned = line.trim().to_string();
    cleaned = Regex::new(r"^\s*(?:[-*+]|\d+[.)])\s*\[[^\]]\]\s*")
        .expect("regex should compile")
        .replace(&cleaned, "")
        .into_owned();
    cleaned = Regex::new(r"^\s*[-*+]\s+")
        .expect("regex should compile")
        .replace(&cleaned, "")
        .into_owned();
    cleaned = Regex::new(r"^\s*\d+[.)]\s+")
        .expect("regex should compile")
        .replace(&cleaned, "")
        .into_owned();
    let blockquote_prefix = Regex::new(r"^\s*>\s*").expect("regex should compile");
    while cleaned.trim_start().starts_with('>') {
        cleaned = blockquote_prefix.replace(&cleaned, "").into_owned();
    }
    cleaned = Regex::new(r"^\s*#{1,6}\s+")
        .expect("regex should compile")
        .replace(&cleaned, "")
        .into_owned();
    if Regex::new(r"^\s*(?:-{3,}|={3,})\s*$")
        .expect("regex should compile")
        .is_match(&cleaned)
    {
        return String::new();
    }
    cleaned.trim().to_string()
}

fn line_replacement_prefix(line: &str) -> String {
    if let Some(captures) = Regex::new(r"^(\s*)((?:[-*+]|\d+[.)])\s+)\[[^\]]\]")
        .expect("regex should compile")
        .captures(line)
    {
        let indent = captures.get(1).map_or("", |capture| capture.as_str());
        let prefix = captures.get(2).map_or("- ", |capture| capture.as_str());
        return format!("{indent}{prefix}");
    }
    if let Some(captures) = Regex::new(r"^(\s*(?:[-*+]|\d+[.)])\s+)")
        .expect("regex should compile")
        .captures(line)
    {
        return captures
            .get(1)
            .map_or("- ".to_string(), |capture| capture.as_str().to_string());
    }
    if let Some(captures) = Regex::new(r"^(\s*(?:>\s*)+)")
        .expect("regex should compile")
        .captures(line)
    {
        return captures
            .get(1)
            .map_or("> ".to_string(), |capture| capture.as_str().to_string());
    }
    "- ".to_string()
}

fn resolve_task_convert_line(
    source: &str,
    line_number: i64,
) -> Result<ResolvedTaskConvertLine, CliError> {
    let lines = source.split('\n').collect::<Vec<_>>();
    let index = usize::try_from(line_number.saturating_sub(1))
        .map_err(|_| CliError::operation(format!("invalid line number: {line_number}")))?;
    let line = lines
        .get(index)
        .copied()
        .ok_or_else(|| CliError::operation(format!("line {line_number} not found")))?;
    let heading = Regex::new(r"^\s*(#{1,6})\s+(.+?)\s*$").expect("regex should compile");
    if let Some(captures) = heading.captures(line) {
        let level = captures.get(1).map_or(0, |capture| capture.as_str().len());
        let title_input = captures
            .get(2)
            .map_or(String::new(), |capture| capture.as_str().trim().to_string());
        if title_input.is_empty() {
            return Err(CliError::operation(format!(
                "line {line_number} does not contain convertible heading text"
            )));
        }

        let mut end_index = index;
        for (candidate_index, candidate) in lines.iter().enumerate().skip(index + 1) {
            if let Some(next_heading) = heading.captures(candidate) {
                let next_level = next_heading
                    .get(1)
                    .map_or(0, |capture| capture.as_str().len());
                if next_level <= level {
                    break;
                }
            }
            end_index = candidate_index;
        }
        let details = lines
            .get(index + 1..=end_index)
            .map_or_else(String::new, |selected| selected.join("\n"));
        return Ok(ResolvedTaskConvertLine {
            start_line: line_number,
            end_line: i64::try_from(end_index + 1)
                .map_err(|_| CliError::operation("heading range exceeds supported size"))?,
            title_input,
            details,
            replacement_prefix: "- ".to_string(),
            completed: false,
        });
    }

    let title_input = extract_line_content_as_task_title(line);
    if title_input.is_empty() {
        return Err(CliError::operation(format!(
            "line {line_number} does not contain convertible task text"
        )));
    }

    let completed = Regex::new(r"^\s*(?:[-*+]|\d+[.)])\s*\[[xX]\]")
        .expect("regex should compile")
        .is_match(line);
    Ok(ResolvedTaskConvertLine {
        start_line: line_number,
        end_line: line_number,
        title_input,
        details: String::new(),
        replacement_prefix: line_replacement_prefix(line),
        completed,
    })
}

fn replace_task_convert_line_range(
    source: &str,
    selection: &ResolvedTaskConvertLine,
    replacement_line: &str,
) -> Result<(String, RefactorChange), CliError> {
    let mut lines = source
        .split('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let start_index = usize::try_from(selection.start_line.saturating_sub(1))
        .map_err(|_| CliError::operation("invalid conversion start line"))?;
    let end_index = usize::try_from(selection.end_line.saturating_sub(1))
        .map_err(|_| CliError::operation("invalid conversion end line"))?;
    if start_index >= lines.len() || end_index >= lines.len() || start_index > end_index {
        return Err(CliError::operation(
            "conversion line range is out of bounds",
        ));
    }

    let before = lines[start_index..=end_index].join("\n");
    lines.splice(start_index..=end_index, [replacement_line.to_string()]);
    Ok((
        lines.join("\n"),
        RefactorChange {
            before,
            after: replacement_line.to_string(),
        },
    ))
}

#[allow(clippy::too_many_lines)]
fn build_converted_tasknote(
    paths: &VaultPaths,
    config: &vulcan_core::VaultConfig,
    title_input: &str,
    details: &str,
    completed: bool,
) -> Result<PlannedConvertedTaskNote, CliError> {
    let reference_ms = tasknote_reference_ms();
    let raw_title = title_input.trim();
    if raw_title.is_empty() {
        return Err(CliError::operation("task text cannot be empty"));
    }

    let parsed_input = config
        .tasknotes
        .enable_natural_language_input
        .then(|| parse_tasknote_natural_language(raw_title, &config.tasknotes, reference_ms));
    let title = parsed_input
        .as_ref()
        .map(|parsed| parsed.title.as_str())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(raw_title)
        .trim()
        .to_string();
    if title.is_empty() {
        return Err(CliError::operation("task title cannot be empty"));
    }

    let status = if completed {
        first_completed_tasknote_status(config)
    } else {
        parsed_input
            .as_ref()
            .and_then(|parsed| parsed.status.clone())
            .unwrap_or_else(|| config.tasknotes.default_status.clone())
    };
    let priority = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.priority.clone())
        .unwrap_or_else(|| config.tasknotes.default_priority.clone());
    let due = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.due.clone())
        .or_else(|| {
            tasknotes_default_date_value(
                config.tasknotes.task_creation_defaults.default_due_date,
                reference_ms,
            )
        });
    let scheduled = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.scheduled.clone())
        .or_else(|| {
            tasknotes_default_date_value(
                config
                    .tasknotes
                    .task_creation_defaults
                    .default_scheduled_date,
                reference_ms,
            )
        });
    let contexts = dedup_tasknote_values(
        config
            .tasknotes
            .task_creation_defaults
            .default_contexts
            .iter()
            .cloned()
            .chain(
                parsed_input
                    .as_ref()
                    .into_iter()
                    .flat_map(|parsed| parsed.contexts.iter().cloned()),
            )
            .collect::<Vec<_>>(),
        normalize_tasknote_context,
    );
    let projects = dedup_tasknote_values(
        config
            .tasknotes
            .task_creation_defaults
            .default_projects
            .iter()
            .cloned()
            .chain(
                parsed_input
                    .as_ref()
                    .into_iter()
                    .flat_map(|parsed| parsed.projects.iter().cloned()),
            )
            .collect::<Vec<_>>(),
        normalize_tasknote_project,
    );
    let mut tags = dedup_tasknote_values(
        config
            .tasknotes
            .task_creation_defaults
            .default_tags
            .iter()
            .cloned()
            .chain(
                parsed_input
                    .as_ref()
                    .into_iter()
                    .flat_map(|parsed| parsed.tags.iter().cloned()),
            )
            .collect::<Vec<_>>(),
        normalize_tasknote_tag,
    );
    if config.tasknotes.identification_method == vulcan_core::TaskNotesIdentificationMethod::Tag {
        if let Some(task_tag) = normalize_tasknote_tag(&config.tasknotes.task_tag) {
            if !tags
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&task_tag))
            {
                tags.insert(0, task_tag);
            }
        }
    }
    let time_estimate = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.time_estimate)
        .or(config
            .tasknotes
            .task_creation_defaults
            .default_time_estimate);
    let recurrence = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.recurrence.clone())
        .or_else(|| {
            tasknotes_default_recurrence_rule(
                config.tasknotes.task_creation_defaults.default_recurrence,
            )
        });

    let relative_path = format!(
        "{}/{}.md",
        config.tasknotes.tasks_folder.trim_end_matches('/'),
        sanitize_tasknote_filename(&title)
    );
    if paths.vault_root().join(&relative_path).exists() {
        return Err(CliError::operation(format!(
            "destination task already exists: {relative_path}"
        )));
    }

    let mapping = &config.tasknotes.field_mapping;
    let timestamp = current_utc_timestamp_string();
    let mut frontmatter = YamlMapping::new();
    let mut task_changes = Vec::new();
    for (key, value) in [
        (
            mapping.title.as_str(),
            Some(YamlValue::String(title.clone())),
        ),
        (mapping.status.as_str(), Some(YamlValue::String(status))),
        (mapping.priority.as_str(), Some(YamlValue::String(priority))),
        (
            mapping.date_created.as_str(),
            Some(YamlValue::String(timestamp.clone())),
        ),
        (
            mapping.date_modified.as_str(),
            Some(YamlValue::String(timestamp)),
        ),
    ] {
        if let Some(change) = set_tasknote_frontmatter_value(&mut frontmatter, key, value) {
            task_changes.push(change);
        }
    }
    if let Some(due) = due {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.due,
            Some(YamlValue::String(due)),
        ) {
            task_changes.push(change);
        }
    }
    if let Some(scheduled) = scheduled {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.scheduled,
            Some(YamlValue::String(scheduled)),
        ) {
            task_changes.push(change);
        }
    }
    if !contexts.is_empty() {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.contexts,
            Some(yaml_string_sequence(&contexts)),
        ) {
            task_changes.push(change);
        }
    }
    if !projects.is_empty() {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.projects,
            Some(yaml_string_sequence(&projects)),
        ) {
            task_changes.push(change);
        }
    }
    if !tags.is_empty() {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            "tags",
            Some(yaml_string_sequence(&tags)),
        ) {
            task_changes.push(change);
        }
    }
    if let Some(time_estimate) = time_estimate {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.time_estimate,
            Some(YamlValue::Number(serde_yaml::Number::from(
                time_estimate as u64,
            ))),
        ) {
            task_changes.push(change);
        }
    }
    if let Some(recurrence) = recurrence {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.recurrence,
            Some(YamlValue::String(recurrence)),
        ) {
            task_changes.push(change);
        }
    }
    if completed {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.completed_date,
            Some(YamlValue::String(current_utc_date_string())),
        ) {
            task_changes.push(change);
        }
    }
    if config.tasknotes.identification_method
        == vulcan_core::TaskNotesIdentificationMethod::Property
    {
        if let Some(property_name) = config.tasknotes.task_property_name.as_ref() {
            let value = config
                .tasknotes
                .task_property_value
                .as_ref()
                .map_or(YamlValue::Bool(true), |value| {
                    YamlValue::String(value.clone())
                });
            if let Some(change) =
                set_tasknote_frontmatter_value(&mut frontmatter, property_name, Some(value))
            {
                task_changes.push(change);
            }
        }
    }

    Ok(PlannedConvertedTaskNote {
        relative_path,
        title,
        frontmatter,
        body: normalize_tasknote_body(details),
        task_changes,
    })
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_tasks_add_command(
    paths: &VaultPaths,
    text: &str,
    no_nlp: bool,
    status: Option<&str>,
    priority: Option<&str>,
    due: Option<&str>,
    scheduled: Option<&str>,
    contexts: &[String],
    projects: &[String],
    tags: &[String],
    template: Option<&str>,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<TaskAddReport, CliError> {
    let config = load_vault_config(paths).config;
    let reference_ms = tasknote_reference_ms();
    let raw_title = text.trim();
    if raw_title.is_empty() {
        return Err(CliError::operation("task text cannot be empty"));
    }

    let used_nlp = config.tasknotes.enable_natural_language_input && !no_nlp;
    let parsed_input = used_nlp
        .then(|| parse_tasknote_natural_language(raw_title, &config.tasknotes, reference_ms));
    let title = parsed_input
        .as_ref()
        .map(|parsed| parsed.title.as_str())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(raw_title)
        .trim()
        .to_string();
    if title.is_empty() {
        return Err(CliError::operation("task title cannot be empty"));
    }

    let status = status
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            parsed_input
                .as_ref()
                .and_then(|parsed| parsed.status.clone())
        })
        .unwrap_or_else(|| config.tasknotes.default_status.clone());
    let priority = priority
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            parsed_input
                .as_ref()
                .and_then(|parsed| parsed.priority.clone())
        })
        .unwrap_or_else(|| config.tasknotes.default_priority.clone());
    let due = match due {
        Some(value) => Some(resolve_tasknote_date_input(&config, value, false)?),
        None => parsed_input
            .as_ref()
            .and_then(|parsed| parsed.due.clone())
            .or_else(|| {
                tasknotes_default_date_value(
                    config.tasknotes.task_creation_defaults.default_due_date,
                    reference_ms,
                )
            }),
    };
    let scheduled = match scheduled {
        Some(value) => Some(resolve_tasknote_date_input(&config, value, true)?),
        None => parsed_input
            .as_ref()
            .and_then(|parsed| parsed.scheduled.clone())
            .or_else(|| {
                tasknotes_default_date_value(
                    config
                        .tasknotes
                        .task_creation_defaults
                        .default_scheduled_date,
                    reference_ms,
                )
            }),
    };
    let contexts = dedup_tasknote_values(
        config
            .tasknotes
            .task_creation_defaults
            .default_contexts
            .iter()
            .cloned()
            .chain(
                parsed_input
                    .as_ref()
                    .into_iter()
                    .flat_map(|parsed| parsed.contexts.iter().cloned()),
            )
            .chain(contexts.iter().cloned())
            .collect::<Vec<_>>(),
        normalize_tasknote_context,
    );
    let projects = dedup_tasknote_values(
        config
            .tasknotes
            .task_creation_defaults
            .default_projects
            .iter()
            .cloned()
            .chain(
                parsed_input
                    .as_ref()
                    .into_iter()
                    .flat_map(|parsed| parsed.projects.iter().cloned()),
            )
            .chain(projects.iter().cloned())
            .collect::<Vec<_>>(),
        normalize_tasknote_project,
    );
    let mut tags = dedup_tasknote_values(
        config
            .tasknotes
            .task_creation_defaults
            .default_tags
            .iter()
            .cloned()
            .chain(
                parsed_input
                    .as_ref()
                    .into_iter()
                    .flat_map(|parsed| parsed.tags.iter().cloned()),
            )
            .chain(tags.iter().cloned())
            .collect::<Vec<_>>(),
        normalize_tasknote_tag,
    );
    if config.tasknotes.identification_method == vulcan_core::TaskNotesIdentificationMethod::Tag {
        if let Some(task_tag) = normalize_tasknote_tag(&config.tasknotes.task_tag) {
            if !tags
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&task_tag))
            {
                tags.insert(0, task_tag);
            }
        }
    }
    let time_estimate = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.time_estimate)
        .or(config
            .tasknotes
            .task_creation_defaults
            .default_time_estimate);
    let recurrence = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.recurrence.clone())
        .or_else(|| {
            tasknotes_default_recurrence_rule(
                config.tasknotes.task_creation_defaults.default_recurrence,
            )
        });

    let relative_path = format!(
        "{}/{}.md",
        config.tasknotes.tasks_folder.trim_end_matches('/'),
        sanitize_tasknote_filename(&title)
    );
    let absolute_path = paths.vault_root().join(&relative_path);
    if absolute_path.exists() {
        return Err(CliError::operation(format!(
            "destination task already exists: {relative_path}"
        )));
    }

    let timestamp = current_utc_timestamp_string();
    let mapping = &config.tasknotes.field_mapping;
    let mut frontmatter = YamlMapping::new();
    frontmatter.insert(
        YamlValue::String(mapping.title.clone()),
        YamlValue::String(title.clone()),
    );
    frontmatter.insert(
        YamlValue::String(mapping.status.clone()),
        YamlValue::String(status.clone()),
    );
    frontmatter.insert(
        YamlValue::String(mapping.priority.clone()),
        YamlValue::String(priority.clone()),
    );
    frontmatter.insert(
        YamlValue::String(mapping.date_created.clone()),
        YamlValue::String(timestamp.clone()),
    );
    frontmatter.insert(
        YamlValue::String(mapping.date_modified.clone()),
        YamlValue::String(timestamp),
    );
    if let Some(due) = due.as_ref() {
        frontmatter.insert(
            YamlValue::String(mapping.due.clone()),
            YamlValue::String(due.clone()),
        );
    }
    if let Some(scheduled) = scheduled.as_ref() {
        frontmatter.insert(
            YamlValue::String(mapping.scheduled.clone()),
            YamlValue::String(scheduled.clone()),
        );
    }
    if !contexts.is_empty() {
        frontmatter.insert(
            YamlValue::String(mapping.contexts.clone()),
            yaml_string_sequence(&contexts),
        );
    }
    if !projects.is_empty() {
        frontmatter.insert(
            YamlValue::String(mapping.projects.clone()),
            yaml_string_sequence(&projects),
        );
    }
    if !tags.is_empty() {
        frontmatter.insert(
            YamlValue::String("tags".to_string()),
            yaml_string_sequence(&tags),
        );
    }
    if let Some(time_estimate) = time_estimate {
        frontmatter.insert(
            YamlValue::String(mapping.time_estimate.clone()),
            YamlValue::Number(serde_yaml::Number::from(time_estimate as u64)),
        );
    }
    if let Some(recurrence) = recurrence.as_ref() {
        frontmatter.insert(
            YamlValue::String(mapping.recurrence.clone()),
            YamlValue::String(recurrence.clone()),
        );
    }
    if config.tasknotes.identification_method
        == vulcan_core::TaskNotesIdentificationMethod::Property
    {
        if let Some(property_name) = config.tasknotes.task_property_name.as_ref() {
            let value = config
                .tasknotes
                .task_property_value
                .as_ref()
                .map_or(YamlValue::Bool(true), |value| {
                    YamlValue::String(value.clone())
                });
            frontmatter.insert(YamlValue::String(property_name.clone()), value);
        }
    }

    let (template_frontmatter, template_body) = match template {
        Some(template_name) => {
            load_tasknote_template(paths, &config, template_name, &relative_path)?
        }
        None => (None, String::new()),
    };
    let merged_frontmatter =
        merge_template_frontmatter(Some(frontmatter), template_frontmatter).unwrap_or_default();
    let rendered = render_note_from_parts(Some(&merged_frontmatter), &template_body)
        .map_err(CliError::operation)?;
    let frontmatter_json = tasknote_frontmatter_json(&merged_frontmatter);

    if !dry_run {
        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        fs::write(&absolute_path, rendered).map_err(CliError::operation)?;
        run_incremental_scan(paths, output, use_stderr_color)?;
    }

    Ok(TaskAddReport {
        action: "add".to_string(),
        dry_run,
        created: !dry_run,
        used_nlp,
        path: relative_path.clone(),
        title,
        status,
        priority,
        due,
        scheduled,
        contexts,
        projects,
        tags,
        time_estimate,
        recurrence,
        template: template.map(ToOwned::to_owned),
        frontmatter: frontmatter_json,
        body: template_body,
        parsed_input,
        changed_paths: if dry_run {
            Vec::new()
        } else {
            vec![relative_path]
        },
    })
}

#[derive(Debug, Clone, Copy)]
struct TasksCreateOptions<'a> {
    text: &'a str,
    note: Option<&'a str>,
    due: Option<&'a str>,
    priority: Option<&'a str>,
    dry_run: bool,
}

fn run_tasks_create_command(
    paths: &VaultPaths,
    options: TasksCreateOptions<'_>,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<TaskCreateReport, CliError> {
    let TasksCreateOptions {
        text,
        note,
        due,
        priority,
        dry_run,
    } = options;
    let config = load_vault_config(paths).config;
    let (relative_path, heading) = resolve_tasks_create_target(paths, note)?;
    let absolute_path = paths.vault_root().join(&relative_path);
    if absolute_path.exists() && !absolute_path.is_file() {
        return Err(CliError::operation(format!(
            "target note is not a file: {relative_path}"
        )));
    }

    let existing = fs::read_to_string(&absolute_path).unwrap_or_default();
    let created_note = !absolute_path.exists();
    let planned = build_inline_task_create_plan(&config, text, due, priority)?;
    let insertion = append_entry_to_note(&existing, &planned.line, heading.as_deref());
    let task = format!("{}:{}", relative_path, insertion.line_number);
    let changed_paths = if dry_run {
        Vec::new()
    } else {
        vec![relative_path.clone()]
    };

    if !dry_run {
        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        fs::write(&absolute_path, insertion.updated).map_err(CliError::operation)?;
        run_incremental_scan(paths, output, use_stderr_color)?;
    }

    Ok(TaskCreateReport {
        action: "create".to_string(),
        dry_run,
        path: relative_path,
        task,
        created_note,
        line_number: insertion.line_number,
        used_nlp: planned.used_nlp,
        line: planned.line,
        due: planned.due,
        scheduled: planned.scheduled,
        priority: planned.priority,
        recurrence: planned.recurrence,
        contexts: planned.contexts,
        projects: planned.projects,
        tags: planned.tags,
        changes: vec![insertion.change],
        changed_paths,
    })
}

fn run_tasks_reschedule_command(
    paths: &VaultPaths,
    task: &str,
    due: &str,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<TaskMutationReport, CliError> {
    if let Ok(loaded) = load_tasknote_note(paths, task) {
        let due_value = resolve_tasknote_date_input(&loaded.config, due, false)?;
        return apply_loaded_tasknote_mutation(
            paths,
            &loaded,
            "reschedule",
            dry_run,
            output,
            use_stderr_color,
            |frontmatter, loaded| {
                let mut changes = Vec::new();
                let due_key = &loaded.config.tasknotes.field_mapping.due;
                if let Some(change) = set_tasknote_frontmatter_value(
                    frontmatter,
                    due_key,
                    Some(YamlValue::String(due_value.clone())),
                ) {
                    changes.push(change);
                }

                let modified_key = &loaded.config.tasknotes.field_mapping.date_modified;
                if let Some(change) = set_tasknote_frontmatter_value(
                    frontmatter,
                    modified_key,
                    Some(YamlValue::String(current_utc_timestamp_string())),
                ) {
                    changes.push(change);
                }

                Ok(TaskMutationPlan {
                    changes,
                    moved_to: None,
                })
            },
        );
    }

    run_inline_task_reschedule_command(paths, task, due, dry_run, output, use_stderr_color)
}

fn run_tasks_convert_command(
    paths: &VaultPaths,
    file: &str,
    line: Option<i64>,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<TaskConvertReport, CliError> {
    if let Some(line_number) = line {
        return run_tasks_convert_line_command(
            paths,
            file,
            line_number,
            dry_run,
            output,
            use_stderr_color,
        );
    }

    let config = load_vault_config(paths).config;
    let (relative_path, source) = read_existing_note_source(paths, file)?;
    let (frontmatter, body) =
        parse_frontmatter_document(&source, false).map_err(CliError::operation)?;
    let mut frontmatter = frontmatter.unwrap_or_default();
    let title_hint = tasknote_title_from_path(&relative_path);
    let frontmatter_json = tasknote_frontmatter_json(&frontmatter);
    if extract_tasknote(
        &relative_path,
        &title_hint,
        &frontmatter_json,
        &config.tasknotes,
    )
    .is_some()
    {
        return Err(CliError::operation(format!(
            "note is already a TaskNotes task: {relative_path}"
        )));
    }

    let task_changes =
        prepare_existing_note_tasknote_frontmatter(&mut frontmatter, &title_hint, &config);
    let frontmatter_json = tasknote_frontmatter_json(&frontmatter);
    let indexed = extract_tasknote(
        &relative_path,
        &title_hint,
        &frontmatter_json,
        &config.tasknotes,
    )
    .ok_or_else(|| CliError::operation("failed to convert note into a TaskNotes task"))?;
    let rendered =
        render_note_from_parts(Some(&frontmatter), &body).map_err(CliError::operation)?;
    let changed_paths = if dry_run || task_changes.is_empty() {
        Vec::new()
    } else {
        vec![relative_path.clone()]
    };

    if !dry_run && !task_changes.is_empty() {
        fs::write(paths.vault_root().join(&relative_path), rendered)
            .map_err(CliError::operation)?;
        run_incremental_scan(paths, output, use_stderr_color)?;
    }

    Ok(TaskConvertReport {
        action: "convert".to_string(),
        dry_run,
        mode: "note".to_string(),
        source_path: relative_path.clone(),
        target_path: relative_path,
        line_number: None,
        title: indexed.title,
        created: false,
        source_changes: Vec::new(),
        task_changes,
        frontmatter: frontmatter_json,
        body,
        changed_paths,
    })
}

fn run_tasks_convert_line_command(
    paths: &VaultPaths,
    file: &str,
    line_number: i64,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<TaskConvertReport, CliError> {
    let config = load_vault_config(paths).config;
    let (source_path, source) = read_existing_note_source(paths, file)?;
    let selection = resolve_task_convert_line(&source, line_number)?;
    let planned = build_converted_tasknote(
        paths,
        &config,
        &selection.title_input,
        &selection.details,
        selection.completed,
    )?;
    let replacement_line = format!(
        "{}[[{}]]",
        selection.replacement_prefix,
        tasknote_link_target(&planned.relative_path)
    );
    let (updated_source, source_change) =
        replace_task_convert_line_range(&source, &selection, &replacement_line)?;
    let rendered_task = render_note_from_parts(Some(&planned.frontmatter), &planned.body)
        .map_err(CliError::operation)?;
    let frontmatter_json = tasknote_frontmatter_json(&planned.frontmatter);
    let changed_paths = if dry_run {
        Vec::new()
    } else {
        vec![source_path.clone(), planned.relative_path.clone()]
    };

    if !dry_run {
        let task_path = paths.vault_root().join(&planned.relative_path);
        if let Some(parent) = task_path.parent() {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        fs::write(&task_path, rendered_task).map_err(CliError::operation)?;
        fs::write(paths.vault_root().join(&source_path), updated_source)
            .map_err(CliError::operation)?;
        run_incremental_scan(paths, output, use_stderr_color)?;
    }

    Ok(TaskConvertReport {
        action: "convert".to_string(),
        dry_run,
        mode: "line".to_string(),
        source_path,
        target_path: planned.relative_path,
        line_number: Some(line_number),
        title: planned.title,
        created: true,
        source_changes: vec![source_change],
        task_changes: planned.task_changes,
        frontmatter: frontmatter_json,
        body: planned.body,
        changed_paths,
    })
}

fn apply_tasknote_mutation<F>(
    paths: &VaultPaths,
    task: &str,
    action: &str,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
    mutate: F,
) -> Result<TaskMutationReport, CliError>
where
    F: FnOnce(&mut YamlMapping, &LoadedTaskNote) -> Result<TaskMutationPlan, CliError>,
{
    let loaded = load_tasknote_note(paths, task)?;
    apply_loaded_tasknote_mutation(
        paths,
        &loaded,
        action,
        dry_run,
        output,
        use_stderr_color,
        mutate,
    )
}

fn apply_loaded_tasknote_mutation<F>(
    paths: &VaultPaths,
    loaded: &LoadedTaskNote,
    action: &str,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
    mutate: F,
) -> Result<TaskMutationReport, CliError>
where
    F: FnOnce(&mut YamlMapping, &LoadedTaskNote) -> Result<TaskMutationPlan, CliError>,
{
    let mut frontmatter = loaded.frontmatter.clone();
    let TaskMutationPlan {
        mut changes,
        moved_to,
    } = mutate(&mut frontmatter, loaded)?;
    let moved_to = moved_to.filter(|path| path != &loaded.path);
    let rendered =
        render_note_from_parts(Some(&frontmatter), &loaded.body).map_err(CliError::operation)?;

    let mut changed_paths = Vec::new();
    if !changes.is_empty() || moved_to.is_some() {
        changed_paths.push(loaded.path.clone());
        if let Some(path) = moved_to.as_ref() {
            changed_paths.push(path.clone());
        }
    }
    changed_paths.sort();
    changed_paths.dedup();

    if !dry_run && !changed_paths.is_empty() {
        let source_path = paths.vault_root().join(&loaded.path);
        if let Some(destination) = moved_to.as_ref() {
            let destination_path = paths.vault_root().join(destination);
            if destination_path.exists() {
                return Err(CliError::operation(format!(
                    "destination task already exists: {destination}"
                )));
            }
        }
        fs::write(&source_path, rendered).map_err(CliError::operation)?;

        if let Some(destination) = moved_to.as_ref() {
            let destination_path = paths.vault_root().join(destination);
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent).map_err(CliError::operation)?;
            }
            fs::rename(&source_path, &destination_path).map_err(CliError::operation)?;
        }

        run_incremental_scan(paths, output, use_stderr_color)?;
    }

    if changes.is_empty() && moved_to.is_some() {
        changes.push(RefactorChange {
            before: loaded.path.clone(),
            after: moved_to.clone().unwrap_or_else(|| loaded.path.clone()),
        });
    }

    Ok(TaskMutationReport {
        action: action.to_string(),
        dry_run,
        path: moved_to.clone().unwrap_or_else(|| loaded.path.clone()),
        moved_from: moved_to.as_ref().map(|_| loaded.path.clone()),
        moved_to,
        changes,
        changed_paths,
    })
}

fn run_tasks_show_command(paths: &VaultPaths, task: &str) -> Result<TaskShowReport, CliError> {
    let loaded = load_tasknote_note(paths, task)?;
    let status_state = tasknotes_status_state(&loaded.config.tasknotes, &loaded.indexed.status);

    Ok(TaskShowReport {
        path: loaded.path,
        title: loaded.indexed.title,
        status: loaded.indexed.status,
        status_type: status_state.status_type,
        completed: status_state.completed,
        archived: loaded.indexed.archived,
        priority: loaded.indexed.priority,
        due: loaded.indexed.due,
        scheduled: loaded.indexed.scheduled,
        completed_date: loaded.indexed.completed_date,
        date_created: loaded.indexed.date_created,
        date_modified: loaded.indexed.date_modified,
        contexts: loaded.indexed.contexts,
        projects: loaded.indexed.projects,
        tags: loaded.indexed.tags,
        recurrence: loaded.indexed.recurrence,
        recurrence_anchor: loaded.indexed.recurrence_anchor,
        complete_instances: loaded.indexed.complete_instances,
        skipped_instances: loaded.indexed.skipped_instances,
        blocked_by: loaded.indexed.blocked_by,
        reminders: loaded.indexed.reminders,
        time_entries: loaded.indexed.time_entries,
        custom_fields: Value::Object(loaded.indexed.custom_fields),
        frontmatter: loaded.frontmatter_json,
        body: loaded.body,
    })
}

fn run_tasks_edit_command(
    paths: &VaultPaths,
    task: &str,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<EditReport, CliError> {
    let loaded = load_tasknote_note(paths, task)?;
    let absolute_path = paths.vault_root().join(&loaded.path);
    open_in_editor(&absolute_path).map_err(CliError::operation)?;
    run_incremental_scan(paths, output, use_stderr_color)?;

    Ok(EditReport {
        path: loaded.path,
        created: false,
        rescanned: true,
    })
}

fn run_tasks_set_command(
    paths: &VaultPaths,
    task: &str,
    property: &str,
    value: &str,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<TaskMutationReport, CliError> {
    apply_tasknote_mutation(
        paths,
        task,
        "set",
        dry_run,
        output,
        use_stderr_color,
        |frontmatter, loaded| {
            let key = tasknote_frontmatter_key(&loaded.config, property);
            let parsed = parse_tasknote_cli_value(value);
            let mut changes = Vec::new();
            let value = (!matches!(parsed, YamlValue::Null)).then_some(parsed.clone());
            if let Some(change) = set_tasknote_frontmatter_value(frontmatter, &key, value.clone()) {
                changes.push(change);
            }

            if key == loaded.config.tasknotes.field_mapping.status
                && loaded.indexed.recurrence.is_none()
            {
                let next_status = value.as_ref().and_then(yaml_string).unwrap_or_default();
                let completed_key = &loaded.config.tasknotes.field_mapping.completed_date;
                let completed_value =
                    if tasknotes_status_state(&loaded.config.tasknotes, &next_status).completed {
                        Some(YamlValue::String(current_utc_date_string()))
                    } else {
                        None
                    };
                if let Some(change) =
                    set_tasknote_frontmatter_value(frontmatter, completed_key, completed_value)
                {
                    changes.push(change);
                }
            }

            let modified_key = &loaded.config.tasknotes.field_mapping.date_modified;
            if let Some(change) = set_tasknote_frontmatter_value(
                frontmatter,
                modified_key,
                Some(YamlValue::String(current_utc_timestamp_string())),
            ) {
                changes.push(change);
            }

            Ok(TaskMutationPlan {
                changes,
                moved_to: None,
            })
        },
    )
}

fn run_tasks_complete_command(
    paths: &VaultPaths,
    task: &str,
    date: Option<&str>,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<TaskMutationReport, CliError> {
    if let Ok(loaded) = load_tasknote_note(paths, task) {
        return apply_loaded_tasknote_mutation(
            paths,
            &loaded,
            "complete",
            dry_run,
            output,
            use_stderr_color,
            |frontmatter, loaded| {
                let mut changes = Vec::new();
                if loaded.indexed.recurrence.is_some() {
                    let target_date = match date {
                        Some(value) => normalize_date_argument(Some(value))?,
                        None => loaded
                            .indexed
                            .scheduled
                            .as_deref()
                            .or(loaded.indexed.due.as_deref())
                            .map(|value| normalize_date_argument(Some(value)))
                            .transpose()?
                            .unwrap_or_else(current_utc_date_string),
                    };

                    let complete_key = &loaded.config.tasknotes.field_mapping.complete_instances;
                    let skipped_key = &loaded.config.tasknotes.field_mapping.skipped_instances;
                    let complete_yaml_key = YamlValue::String(complete_key.clone());
                    let mut complete_instances =
                        yaml_string_list(frontmatter.get(&complete_yaml_key));
                    if !complete_instances.iter().any(|entry| entry == &target_date) {
                        complete_instances.push(target_date.clone());
                        complete_instances.sort();
                    }
                    if let Some(change) = set_tasknote_frontmatter_value(
                        frontmatter,
                        complete_key,
                        Some(YamlValue::Sequence(
                            complete_instances
                                .iter()
                                .cloned()
                                .map(YamlValue::String)
                                .collect(),
                        )),
                    ) {
                        changes.push(change);
                    }

                    let skipped_yaml_key = YamlValue::String(skipped_key.clone());
                    let skipped_instances = yaml_string_list(frontmatter.get(&skipped_yaml_key))
                        .into_iter()
                        .filter(|entry| entry != &target_date)
                        .collect::<Vec<_>>();
                    let skipped_value = if skipped_instances.is_empty() {
                        None
                    } else {
                        Some(YamlValue::Sequence(
                            skipped_instances
                                .into_iter()
                                .map(YamlValue::String)
                                .collect(),
                        ))
                    };
                    if let Some(change) =
                        set_tasknote_frontmatter_value(frontmatter, skipped_key, skipped_value)
                    {
                        changes.push(change);
                    }
                } else {
                    let status_key = &loaded.config.tasknotes.field_mapping.status;
                    if let Some(change) = set_tasknote_frontmatter_value(
                        frontmatter,
                        status_key,
                        Some(YamlValue::String(first_completed_tasknote_status(
                            &loaded.config,
                        ))),
                    ) {
                        changes.push(change);
                    }
                    let completed_key = &loaded.config.tasknotes.field_mapping.completed_date;
                    if let Some(change) = set_tasknote_frontmatter_value(
                        frontmatter,
                        completed_key,
                        Some(YamlValue::String(current_utc_date_string())),
                    ) {
                        changes.push(change);
                    }
                }

                let modified_key = &loaded.config.tasknotes.field_mapping.date_modified;
                if let Some(change) = set_tasknote_frontmatter_value(
                    frontmatter,
                    modified_key,
                    Some(YamlValue::String(current_utc_timestamp_string())),
                ) {
                    changes.push(change);
                }

                Ok(TaskMutationPlan {
                    changes,
                    moved_to: None,
                })
            },
        );
    }

    run_inline_task_complete_command(paths, task, date, dry_run, output, use_stderr_color)
}

fn run_inline_task_reschedule_command(
    paths: &VaultPaths,
    task: &str,
    due: &str,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<TaskMutationReport, CliError> {
    let resolved = resolve_inline_task(paths, task)?;
    let config = load_vault_config(paths).config;
    let due_value = resolve_tasknote_date_input(&config, due, false)?;
    let absolute_path = paths.vault_root().join(&resolved.path);
    let source = fs::read_to_string(&absolute_path).map_err(CliError::operation)?;
    let (rendered, change) =
        reschedule_inline_task_source(&source, resolved.line_number, &due_value)?;
    let changes = change.into_iter().collect::<Vec<_>>();
    let changed_paths = if dry_run || changes.is_empty() {
        Vec::new()
    } else {
        vec![resolved.path.clone()]
    };

    if !dry_run && !changes.is_empty() {
        fs::write(&absolute_path, rendered).map_err(CliError::operation)?;
        run_incremental_scan(paths, output, use_stderr_color)?;
    }

    Ok(TaskMutationReport {
        action: "reschedule".to_string(),
        dry_run,
        path: resolved.path,
        moved_from: None,
        moved_to: None,
        changes,
        changed_paths,
    })
}

fn run_inline_task_complete_command(
    paths: &VaultPaths,
    task: &str,
    date: Option<&str>,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<TaskMutationReport, CliError> {
    let resolved = resolve_inline_task(paths, task)?;
    let config = load_vault_config(paths).config;
    let completed_symbol = first_completed_inline_status_symbol(&config);
    let completed_date = normalize_date_argument(date)?;
    let absolute_path = paths.vault_root().join(&resolved.path);
    let source = fs::read_to_string(&absolute_path).map_err(CliError::operation)?;
    let (rendered, change) = complete_inline_task_source(
        &source,
        resolved.line_number,
        &completed_symbol,
        &completed_date,
    )?;
    let changes = change.into_iter().collect::<Vec<_>>();
    let changed_paths = if dry_run || changes.is_empty() {
        Vec::new()
    } else {
        vec![resolved.path.clone()]
    };

    if !dry_run && !changes.is_empty() {
        fs::write(&absolute_path, rendered).map_err(CliError::operation)?;
        run_incremental_scan(paths, output, use_stderr_color)?;
    }

    Ok(TaskMutationReport {
        action: "complete".to_string(),
        dry_run,
        path: resolved.path,
        moved_from: None,
        moved_to: None,
        changes,
        changed_paths,
    })
}

fn first_completed_inline_status_symbol(config: &vulcan_core::VaultConfig) -> String {
    config
        .tasks
        .statuses
        .completed
        .first()
        .cloned()
        .unwrap_or_else(|| "x".to_string())
}

fn complete_inline_task_source(
    source: &str,
    line_number: i64,
    completed_symbol: &str,
    completed_date: &str,
) -> Result<(String, Option<RefactorChange>), CliError> {
    let mut lines = source
        .split('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let index = usize::try_from(line_number.saturating_sub(1))
        .map_err(|_| CliError::operation(format!("invalid task line number: {line_number}")))?;
    let current = lines
        .get(index)
        .cloned()
        .ok_or_else(|| CliError::operation(format!("task line {line_number} not found")))?;
    let updated = update_inline_task_line(&current, completed_symbol, completed_date)?;
    let change = (updated != current).then(|| RefactorChange {
        before: current.clone(),
        after: updated.clone(),
    });
    lines[index] = updated;
    Ok((lines.join("\n"), change))
}

fn reschedule_inline_task_source(
    source: &str,
    line_number: i64,
    due: &str,
) -> Result<(String, Option<RefactorChange>), CliError> {
    let mut lines = source
        .split('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let index = usize::try_from(line_number.saturating_sub(1))
        .map_err(|_| CliError::operation(format!("invalid task line number: {line_number}")))?;
    let current = lines
        .get(index)
        .cloned()
        .ok_or_else(|| CliError::operation(format!("task line {line_number} not found")))?;
    let updated = update_inline_task_due_marker(&current, due)?;
    let change = (updated != current).then(|| RefactorChange {
        before: current.clone(),
        after: updated.clone(),
    });
    lines[index] = updated;
    Ok((lines.join("\n"), change))
}

fn update_inline_task_line(
    line: &str,
    completed_symbol: &str,
    completed_date: &str,
) -> Result<String, CliError> {
    let completed_char = completed_symbol
        .chars()
        .next()
        .ok_or_else(|| CliError::operation("completed task status cannot be empty"))?;
    let checkbox =
        Regex::new(r"^(\s*(?:[-*+]|\d+[.)])\s+\[)(.)(\])").expect("regex should compile");
    let captures = checkbox.captures(line).ok_or_else(|| {
        CliError::operation(format!(
            "line is not an inline task and cannot be completed: {line}"
        ))
    })?;
    let full = captures
        .get(0)
        .ok_or_else(|| CliError::operation("failed to locate task checkbox"))?;
    let prefix = captures.get(1).map_or("", |capture| capture.as_str());
    let suffix = captures.get(3).map_or("", |capture| capture.as_str());
    let replaced = format!(
        "{}{}{}{}{}",
        &line[..full.start()],
        prefix,
        completed_char,
        suffix,
        &line[full.end()..]
    );
    let completion_marker = Regex::new(r"✅\s+\S+").expect("regex should compile");
    let replaced = if completion_marker.is_match(&replaced) {
        completion_marker
            .replace(&replaced, format!("✅ {completed_date}"))
            .into_owned()
    } else {
        format!("{} ✅ {completed_date}", replaced.trim_end())
    };
    Ok(replaced)
}

fn update_inline_task_due_marker(line: &str, due: &str) -> Result<String, CliError> {
    let checkbox = Regex::new(r"^\s*(?:[-*+]|\d+[.)])\s+\[[^\]]\]").expect("regex should compile");
    if !checkbox.is_match(line) {
        return Err(CliError::operation(format!(
            "line is not an inline task and cannot be rescheduled: {line}"
        )));
    }

    let due_marker = Regex::new(r"🗓(?:️)?\s+\S+").expect("regex should compile");
    if due_marker.is_match(line) {
        Ok(due_marker.replace(line, format!("🗓️ {due}")).into_owned())
    } else {
        Ok(format!("{} 🗓️ {due}", line.trim_end()))
    }
}

fn run_tasks_archive_command(
    paths: &VaultPaths,
    task: &str,
    dry_run: bool,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<TaskMutationReport, CliError> {
    apply_tasknote_mutation(
        paths,
        task,
        "archive",
        dry_run,
        output,
        use_stderr_color,
        |frontmatter, loaded| {
            let status_state =
                tasknotes_status_state(&loaded.config.tasknotes, &loaded.indexed.status);
            if !loaded.indexed.archived && !status_state.completed {
                return Err(CliError::operation(format!(
                    "task must be completed before archiving: {}",
                    loaded.path
                )));
            }

            let mut changes = Vec::new();
            let archive_tag = &loaded.config.tasknotes.field_mapping.archive_tag;
            let tags_key = YamlValue::String("tags".to_string());
            let mut tags = yaml_string_list(frontmatter.get(&tags_key));
            if !tags.iter().any(|tag| tag.eq_ignore_ascii_case(archive_tag)) {
                tags.push(archive_tag.clone());
                tags.sort();
                if let Some(change) = set_tasknote_frontmatter_value(
                    frontmatter,
                    "tags",
                    Some(YamlValue::Sequence(
                        tags.iter().cloned().map(YamlValue::String).collect(),
                    )),
                ) {
                    changes.push(change);
                }
            }

            let modified_key = &loaded.config.tasknotes.field_mapping.date_modified;
            if let Some(change) = set_tasknote_frontmatter_value(
                frontmatter,
                modified_key,
                Some(YamlValue::String(current_utc_timestamp_string())),
            ) {
                changes.push(change);
            }

            let moved_to = Path::new(&loaded.path)
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| {
                    let archive_folder = loaded
                        .config
                        .tasknotes
                        .archive_folder
                        .trim()
                        .trim_matches('/');
                    (!archive_folder.is_empty()).then(|| format!("{archive_folder}/{name}"))
                });

            Ok(TaskMutationPlan { changes, moved_to })
        },
    )
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
        .map_or_else(|| file.to_string(), |block| block.file.clone());
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
    options: TasksListOptions<'_>,
) -> Result<TasksQueryResult, CliError> {
    let config = load_vault_config(paths).config.tasks;
    let filter = options
        .filter
        .map(str::trim)
        .filter(|filter| !filter.is_empty());
    let prefilter_source = tasks_list_prefilter_source(&options);
    let layout_source = tasks_list_layout_source(&options);

    match filter {
        None => {
            let source = join_tasks_query_sections([
                Some(prefilter_source.as_str()),
                Some(layout_source.as_str()),
            ]);
            run_tasks_query_command(paths, &source)
        }
        Some(filter) => match parse_tasks_query(filter) {
            Ok(_) => {
                let source = join_tasks_query_sections([
                    Some(prefilter_source.as_str()),
                    Some(filter),
                    Some(layout_source.as_str()),
                ]);
                run_tasks_query_command(paths, &source)
            }
            Err(tasks_error) => run_tasks_list_dql_filter(
                paths,
                filter,
                &tasks_error,
                &config,
                &prefilter_source,
                &layout_source,
            ),
        },
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
    tasks_error: &str,
    config: &vulcan_core::config::TasksConfig,
    prefilter_source: &str,
    layout_source: &str,
) -> Result<TasksQueryResult, CliError> {
    let expression_source = tasks_dql_filter_expression(config, filter);
    let expression = parse_expression(&expression_source).map_err(|expression_error| {
        CliError::operation(format!(
            "failed to parse filter as Tasks DSL ({tasks_error}); failed to parse as Dataview expression ({expression_error})"
        ))
    })?;

    let base_source = tasks_query_source(config, prefilter_source, false);
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

    let mut result = if layout_source.trim().is_empty() {
        TasksQueryResult {
            result_count: tasks.len(),
            tasks,
            groups: Vec::new(),
            hidden_fields: Vec::new(),
            shown_fields: Vec::new(),
            short_mode: false,
            plan: None,
        }
    } else {
        let layout_query = parse_tasks_query(layout_source).map_err(CliError::operation)?;
        shape_tasks_query_result(tasks, &layout_query)
    };
    strip_global_filter_from_output(&mut result, config);
    Ok(result)
}

fn resolve_tasks_reference_date(from: Option<&str>) -> Result<(String, i64), CliError> {
    let reference_date = from
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || TemplateTimestamp::current().default_date_string(),
            ToOwned::to_owned,
        );
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

#[derive(Debug, Clone, Copy)]
struct TasksListOptions<'a> {
    filter: Option<&'a str>,
    source: TasksListSourceArg,
    status: Option<&'a str>,
    priority: Option<&'a str>,
    due_before: Option<&'a str>,
    due_after: Option<&'a str>,
    project: Option<&'a str>,
    context: Option<&'a str>,
    group_by: Option<&'a str>,
    sort_by: Option<&'a str>,
    include_archived: bool,
}

fn tasks_list_prefilter_source(options: &TasksListOptions<'_>) -> String {
    let mut sections = Vec::new();
    if !options.include_archived {
        sections.push("is not archived".to_string());
    }
    match options.source {
        TasksListSourceArg::File => sections.push("source is file".to_string()),
        TasksListSourceArg::Inline => sections.push("source is inline".to_string()),
        TasksListSourceArg::All => {}
    }
    if let Some(status) = tasks_query_value(options.status) {
        sections.push(format!("status is {}", quote_tasks_query_value(status)));
    }
    if let Some(priority) = tasks_query_value(options.priority) {
        sections.push(format!("priority is {}", quote_tasks_query_value(priority)));
    }
    if let Some(due_before) = tasks_query_value(options.due_before) {
        sections.push(format!(
            "due before {}",
            quote_tasks_query_value(due_before)
        ));
    }
    if let Some(due_after) = tasks_query_value(options.due_after) {
        sections.push(format!("due after {}", quote_tasks_query_value(due_after)));
    }
    if let Some(project) = tasks_query_value(options.project) {
        sections.push(format!(
            "project includes {}",
            quote_tasks_query_value(project)
        ));
    }
    if let Some(context) = tasks_query_value(options.context) {
        sections.push(format!(
            "context includes {}",
            quote_tasks_query_value(context)
        ));
    }
    sections.join("\n")
}

fn tasks_list_layout_source(options: &TasksListOptions<'_>) -> String {
    let mut sections = Vec::new();
    if let Some(sort_by) = tasks_query_value(options.sort_by) {
        sections.push(format!("sort by {}", quote_tasks_query_value(sort_by)));
    }
    if let Some(group_by) = tasks_query_value(options.group_by) {
        sections.push(format!("group by {}", quote_tasks_query_value(group_by)));
    }
    sections.join("\n")
}

fn tasks_query_value(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn quote_tasks_query_value(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '#' | '@'))
    {
        return value.to_string();
    }

    if !value.contains('"') {
        return format!("\"{value}\"");
    }
    if !value.contains('\'') {
        return format!("'{value}'");
    }

    value.to_string()
}

fn join_tasks_query_sections<'a>(sections: impl IntoIterator<Item = Option<&'a str>>) -> String {
    sections
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|section| !section.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>()
        .join("\n")
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
                .map_or(true, |tag| normalize_tag_name(tag) != normalized_tag)
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
        .flat_map(|(key, task)| {
            task_blocker_ids(task).into_iter().map(|blocker_id| {
                let blocker = node_by_id.get(blocker_id.as_str());
                TaskDependencyEdge {
                    blocked_key: key.clone(),
                    blocker_id,
                    resolved: blocker.is_some(),
                    blocker_key: blocker.map(|node| node.key.clone()),
                    blocker_path: blocker.map(|node| node.path.clone()),
                    blocker_line: blocker.map(|node| node.line),
                    blocker_text: blocker.map(|node| node.text.clone()),
                    blocker_completed: blocker.map(|node| node.completed),
                }
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

fn run_kanban_cards_command(
    paths: &VaultPaths,
    board: &str,
    column: Option<&str>,
    status: Option<&str>,
) -> Result<KanbanCardsReport, CliError> {
    let board = load_kanban_board(paths, board, false).map_err(CliError::operation)?;
    let column_filter = normalize_optional_filter(column);
    let status_filter = normalize_optional_filter(status);
    let mut cards = Vec::new();

    for column_record in &board.columns {
        if !kanban_column_matches(column_record.name.as_str(), column_filter.as_deref()) {
            continue;
        }

        for card in &column_record.cards {
            if !kanban_status_matches(card.task.as_ref(), status_filter.as_deref()) {
                continue;
            }

            cards.push(KanbanCardListItem {
                board_path: board.path.clone(),
                board_title: board.title.clone(),
                column: column_record.name.clone(),
                id: card.id.clone(),
                text: card.text.clone(),
                line_number: card.line_number,
                block_id: card.block_id.clone(),
                symbol: card.symbol.clone(),
                tags: card.tags.clone(),
                outlinks: card.outlinks.clone(),
                date: card.date.clone(),
                time: card.time.clone(),
                inline_fields: card.inline_fields.clone(),
                metadata: card.metadata.clone(),
                task: card.task.clone(),
            });
        }
    }

    Ok(KanbanCardsReport {
        board_path: board.path,
        board_title: board.title,
        column_filter,
        status_filter,
        result_count: cards.len(),
        cards,
    })
}

fn run_kanban_archive_command(
    paths: &VaultPaths,
    board: &str,
    card: &str,
    dry_run: bool,
) -> Result<KanbanArchiveReport, CliError> {
    archive_kanban_card(paths, board, card, dry_run).map_err(CliError::operation)
}

fn run_kanban_move_command(
    paths: &VaultPaths,
    board: &str,
    card: &str,
    target_column: &str,
    dry_run: bool,
) -> Result<KanbanMoveReport, CliError> {
    move_kanban_card(paths, board, card, target_column, dry_run).map_err(CliError::operation)
}

fn run_kanban_add_command(
    paths: &VaultPaths,
    board: &str,
    column: &str,
    text: &str,
    dry_run: bool,
) -> Result<KanbanAddReport, CliError> {
    add_kanban_card(paths, board, column, text, dry_run).map_err(CliError::operation)
}

fn normalize_optional_filter(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn kanban_column_matches(name: &str, filter: Option<&str>) -> bool {
    let Some(filter) = filter else {
        return true;
    };

    name == filter || name.eq_ignore_ascii_case(filter)
}

fn kanban_status_matches(task: Option<&KanbanTaskStatus>, filter: Option<&str>) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let Some(task) = task else {
        return false;
    };

    task.status_char == filter
        || task.status_char.eq_ignore_ascii_case(filter)
        || task.status_name.eq_ignore_ascii_case(filter)
        || task.status_type.eq_ignore_ascii_case(filter)
}

fn task_dependency_key(task: &Value) -> Option<String> {
    let path = task.get("path").and_then(Value::as_str)?;
    let line = task.get("line").and_then(Value::as_i64).unwrap_or_default();
    Some(
        task.get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map_or_else(|| format!("{path}:{line}"), ToOwned::to_owned),
    )
}

fn task_blocker_ids(task: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    collect_task_blocker_ids(task.get("blocked-by").unwrap_or(&Value::Null), &mut ids);
    ids
}

fn collect_task_blocker_ids(value: &Value, ids: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            let text = text.trim();
            if !text.is_empty() {
                ids.push(text.to_string());
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_task_blocker_ids(value, ids);
            }
        }
        Value::Object(object) => {
            if let Some(uid) = object.get("uid").and_then(Value::as_str).map(str::trim) {
                if !uid.is_empty() {
                    ids.push(uid.to_string());
                }
            }
        }
        _ => {}
    }
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

#[allow(clippy::too_many_arguments)]
fn run_template_command(
    paths: &VaultPaths,
    name: Option<&str>,
    list: bool,
    output_path: Option<&str>,
    engine: TemplateEngineArg,
    vars: &[String],
    no_commit: bool,
    stdout_is_tty: bool,
) -> Result<TemplateCommandResult, CliError> {
    let config = load_vault_config(paths).config;
    let bound_vars = parse_template_var_bindings(vars)?;
    let templates = discover_templates(
        paths,
        config.templates.obsidian_folder.as_deref(),
        config.templates.templater_folder.as_deref(),
    )?;
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
    let template_source =
        fs::read_to_string(&template.absolute_path).map_err(CliError::operation)?;
    let rendered = render_template_request(TemplateRenderRequest {
        paths,
        vault_config: &config,
        templates: &templates.templates,
        template_path: Some(&template.absolute_path),
        template_text: &template_source,
        target_path: &output_path,
        target_contents: None,
        engine: template_engine_kind(engine),
        vars: &bound_vars,
        allow_mutations: true,
        run_mode: TemplateRunMode::Create,
    })?;
    let absolute_output = paths.vault_root().join(&rendered.target_path);
    if absolute_output.exists() {
        return Err(CliError::operation(format!(
            "destination note already exists: {}",
            rendered.target_path
        )));
    }
    if let Some(parent) = absolute_output.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    fs::write(&absolute_output, &rendered.content).map_err(CliError::operation)?;

    let mut opened_editor = false;
    if stdout_is_tty && io::stdin().is_terminal() {
        open_in_editor(&absolute_output).map_err(CliError::operation)?;
        opened_editor = true;
    }

    run_incremental_scan(paths, OutputFormat::Human, false)?;
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit);
    let mut changed_paths = vec![rendered.target_path.clone()];
    changed_paths.extend(rendered.changed_paths.clone());
    changed_paths.sort();
    changed_paths.dedup();
    auto_commit
        .commit(paths, "template", &changed_paths)
        .map_err(CliError::operation)?;

    Ok(TemplateCommandResult::Create(TemplateCreateReport {
        template: template.name,
        template_source: template.source.to_string(),
        path: rendered.target_path,
        engine: rendered.engine.as_str().to_string(),
        opened_editor,
        warnings: template
            .warning
            .into_iter()
            .chain(rendered.warnings)
            .collect(),
        diagnostics: rendered.diagnostics,
    }))
}

#[allow(clippy::too_many_arguments)]
fn run_template_insert_command(
    paths: &VaultPaths,
    template_name: &str,
    note: Option<&str>,
    mode: TemplateInsertMode,
    engine: TemplateEngineArg,
    vars: &[String],
    no_commit: bool,
    interactive_note_selection: bool,
) -> Result<TemplateInsertReport, CliError> {
    let config = load_vault_config(paths).config;
    let bound_vars = parse_template_var_bindings(vars)?;
    let templates = discover_templates(
        paths,
        config.templates.obsidian_folder.as_deref(),
        config.templates.templater_folder.as_deref(),
    )?;
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
    let template_source =
        fs::read_to_string(&template.absolute_path).map_err(CliError::operation)?;
    let rendered_template = render_template_request(TemplateRenderRequest {
        paths,
        vault_config: &config,
        templates: &templates.templates,
        template_path: Some(&template.absolute_path),
        template_text: &template_source,
        target_path: &target_path,
        target_contents: Some(&target_source),
        engine: template_engine_kind(engine),
        vars: &bound_vars,
        allow_mutations: true,
        run_mode: TemplateRunMode::Append,
    })?;
    let final_target_absolute = paths.vault_root().join(&rendered_template.target_path);
    let prepared = prepare_template_insertion(&target_source, &rendered_template.content)
        .map_err(CliError::operation)?;
    let updated = apply_template_insertion_mode(&prepared, mode).map_err(CliError::operation)?;
    fs::write(&final_target_absolute, updated).map_err(CliError::operation)?;

    run_incremental_scan(paths, OutputFormat::Human, false)?;
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit);
    let mut changed_paths = vec![rendered_template.target_path.clone()];
    changed_paths.extend(rendered_template.changed_paths.clone());
    changed_paths.sort();
    changed_paths.dedup();
    auto_commit
        .commit(paths, "template insert", &changed_paths)
        .map_err(CliError::operation)?;

    Ok(TemplateInsertReport {
        template: template.name,
        template_source: template.source.to_string(),
        note: rendered_template.target_path,
        mode: mode.as_str().to_string(),
        engine: rendered_template.engine.as_str().to_string(),
        warnings: template
            .warning
            .into_iter()
            .chain(rendered_template.warnings)
            .collect(),
        diagnostics: rendered_template.diagnostics,
    })
}

fn run_template_preview_command(
    paths: &VaultPaths,
    template_name: &str,
    output_path: Option<&str>,
    engine: TemplateEngineArg,
    vars: &[String],
) -> Result<TemplatePreviewReport, CliError> {
    let config = load_vault_config(paths).config;
    let bound_vars = parse_template_var_bindings(vars)?;
    let templates = discover_templates(
        paths,
        config.templates.obsidian_folder.as_deref(),
        config.templates.templater_folder.as_deref(),
    )?;
    let template = resolve_template_file(paths, &templates.templates, template_name)?;
    let now = TemplateTimestamp::current();
    let output_path = template_output_path(&template.name, output_path, &now)?;
    let template_source =
        fs::read_to_string(&template.absolute_path).map_err(CliError::operation)?;
    let rendered = render_template_request(TemplateRenderRequest {
        paths,
        vault_config: &config,
        templates: &templates.templates,
        template_path: Some(&template.absolute_path),
        template_text: &template_source,
        target_path: &output_path,
        target_contents: None,
        engine: template_engine_kind(engine),
        vars: &bound_vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Dynamic,
    })?;
    Ok(TemplatePreviewReport {
        template: template.name,
        template_source: template.source.to_string(),
        path: rendered.target_path,
        engine: rendered.engine.as_str().to_string(),
        content: rendered.content,
        warnings: template
            .warning
            .into_iter()
            .chain(rendered.warnings)
            .collect(),
        diagnostics: rendered.diagnostics,
    })
}

fn template_engine_kind(engine: TemplateEngineArg) -> TemplateEngineKind {
    match engine {
        TemplateEngineArg::Native => TemplateEngineKind::Native,
        TemplateEngineArg::Templater => TemplateEngineKind::Templater,
        TemplateEngineArg::Auto => TemplateEngineKind::Auto,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PeriodicTarget {
    period_type: String,
    reference_date: String,
    start_date: String,
    end_date: String,
    path: String,
}

fn current_utc_date_string() -> String {
    TemplateTimestamp::current().default_date_string()
}

fn normalize_date_argument(date: Option<&str>) -> Result<String, CliError> {
    match date
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
    {
        None => Ok(current_utc_date_string()),
        Some(value) if value == "today" => Ok(current_utc_date_string()),
        Some(value) => {
            let timestamp = parse_date_like_string(&value)
                .ok_or_else(|| CliError::operation(format!("invalid date: {value}")))?;
            let (year, month, day, _, _, _, _) = date_components(timestamp);
            Ok(format!("{year:04}-{month:02}-{day:02}"))
        }
    }
}

fn resolve_periodic_target(
    config: &PeriodicConfig,
    period_type: &str,
    date: Option<&str>,
    require_enabled: bool,
) -> Result<PeriodicTarget, CliError> {
    let note = config
        .note(period_type)
        .ok_or_else(|| CliError::operation(format!("unknown periodic note type: {period_type}")))?;
    if require_enabled && !note.enabled {
        return Err(CliError::operation(format!(
            "periodic note type `{period_type}` is disabled in config"
        )));
    }

    let reference_date = normalize_date_argument(date)?;
    let (start_date, end_date) = period_range_for_date(config, period_type, &reference_date)
        .ok_or_else(|| {
            CliError::operation(format!(
                "failed to resolve period range for `{period_type}` and {reference_date}"
            ))
        })?;
    let path =
        expected_periodic_note_path(config, period_type, &reference_date).ok_or_else(|| {
            CliError::operation(format!(
                "failed to resolve note path for `{period_type}` and {reference_date}"
            ))
        })?;

    Ok(PeriodicTarget {
        period_type: period_type.to_string(),
        reference_date,
        start_date,
        end_date,
        path,
    })
}

fn render_periodic_note_contents(
    paths: &VaultPaths,
    period_type: &str,
    relative_path: &str,
    warnings: &mut Vec<String>,
) -> Result<String, CliError> {
    let config = load_vault_config(paths).config;
    let template_name = config
        .periodic
        .note(period_type)
        .and_then(|note| note.template.as_deref());
    let Some(template_name) = template_name else {
        return Ok(String::new());
    };

    let templates = discover_templates(
        paths,
        config.templates.obsidian_folder.as_deref(),
        config.templates.templater_folder.as_deref(),
    )?;
    let template = match resolve_template_file(paths, &templates.templates, template_name) {
        Ok(template) => template,
        Err(error) => {
            warnings.push(format!(
                "failed to resolve periodic template `{template_name}` for `{period_type}`: {error}"
            ));
            return Ok(String::new());
        }
    };
    let contents = fs::read_to_string(&template.absolute_path).map_err(|error| {
        CliError::operation(format!(
            "failed to read template `{}` for `{period_type}`: {error}",
            template.display_path
        ))
    })?;
    let rendered = render_template_request(TemplateRenderRequest {
        paths,
        vault_config: &config,
        templates: &templates.templates,
        template_path: Some(&template.absolute_path),
        template_text: &contents,
        target_path: relative_path,
        target_contents: None,
        engine: TemplateEngineKind::Auto,
        vars: &HashMap::new(),
        allow_mutations: true,
        run_mode: TemplateRunMode::Create,
    })?;
    warnings.extend(rendered.warnings);
    warnings.extend(rendered.diagnostics);
    Ok(rendered.content)
}

fn write_periodic_note_if_missing(
    paths: &VaultPaths,
    period_type: &str,
    relative_path: &str,
    warnings: &mut Vec<String>,
) -> Result<bool, CliError> {
    let absolute_path = paths.vault_root().join(relative_path);
    if absolute_path.is_file() {
        return Ok(false);
    }
    if absolute_path.exists() {
        return Err(CliError::operation(format!(
            "path exists but is not a note file: {relative_path}"
        )));
    }

    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    let contents = render_periodic_note_contents(paths, period_type, relative_path, warnings)?;
    fs::write(&absolute_path, contents).map_err(CliError::operation)?;
    Ok(true)
}

fn commit_periodic_changes_if_needed(
    auto_commit: &AutoCommitPolicy,
    paths: &VaultPaths,
    period_type: &str,
    changed_path: &str,
) -> Result<(), CliError> {
    let changed_file = changed_path.to_string();
    auto_commit
        .commit(
            paths,
            &format!("{period_type}-note"),
            std::slice::from_ref(&changed_file),
        )
        .map_err(CliError::operation)?;
    Ok(())
}

fn run_periodic_open_command(
    paths: &VaultPaths,
    period_type: &str,
    date: Option<&str>,
    no_edit: bool,
    no_commit: bool,
    allow_editor: bool,
) -> Result<PeriodicOpenReport, CliError> {
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit);

    let config = load_vault_config(paths).config;
    let target = resolve_periodic_target(&config.periodic, period_type, date, true)?;
    let mut warnings = Vec::new();
    let created = write_periodic_note_if_missing(paths, period_type, &target.path, &mut warnings)?;
    let absolute_path = paths.vault_root().join(&target.path);
    let opened_editor = !no_edit && allow_editor;

    if opened_editor {
        open_in_editor(&absolute_path).map_err(CliError::operation)?;
    }

    if created || opened_editor {
        run_incremental_scan(paths, OutputFormat::Human, false)?;
        commit_periodic_changes_if_needed(&auto_commit, paths, period_type, &target.path)?;
    }

    Ok(PeriodicOpenReport {
        period_type: target.period_type,
        reference_date: target.reference_date,
        start_date: target.start_date,
        end_date: target.end_date,
        path: target.path,
        created,
        opened_editor,
        warnings,
    })
}

fn load_daily_events_for_path(
    paths: &VaultPaths,
    relative_path: &str,
) -> Result<Vec<PeriodicEventReport>, CliError> {
    load_events_for_periodic_note(paths, relative_path)
        .map(|events| {
            events
                .into_iter()
                .map(|event| PeriodicEventReport {
                    start_time: event.start_time,
                    end_time: event.end_time,
                    title: event.title,
                    metadata: event.metadata,
                    tags: event.tags,
                })
                .collect()
        })
        .map_err(CliError::operation)
}

fn run_daily_show_command(
    paths: &VaultPaths,
    date: Option<&str>,
) -> Result<PeriodicShowReport, CliError> {
    let config = load_vault_config(paths).config;
    let target = resolve_periodic_target(&config.periodic, "daily", date, false)?;
    let resolved = resolve_periodic_note(
        paths.vault_root(),
        &config.periodic,
        "daily",
        &target.reference_date,
    )
    .unwrap_or_else(|| target.path.clone());
    let absolute_path = paths.vault_root().join(&resolved);
    if !absolute_path.is_file() {
        return Err(CliError::operation(format!(
            "daily note does not exist on disk: {}",
            target.path
        )));
    }

    Ok(PeriodicShowReport {
        period_type: "daily".to_string(),
        reference_date: target.reference_date,
        start_date: target.start_date,
        end_date: target.end_date,
        path: resolved.clone(),
        content: fs::read_to_string(&absolute_path).map_err(CliError::operation)?,
        events: load_daily_events_for_path(paths, &resolved)?,
    })
}

fn resolve_daily_list_window(
    config: &PeriodicConfig,
    from: Option<&str>,
    to: Option<&str>,
    week: bool,
    month: bool,
) -> Result<(String, String), CliError> {
    let today = current_utc_date_string();
    if week {
        return period_range_for_date(config, "weekly", &today)
            .ok_or_else(|| CliError::operation("failed to resolve weekly date range"));
    }
    if month {
        return period_range_for_date(config, "monthly", &today)
            .ok_or_else(|| CliError::operation("failed to resolve monthly date range"));
    }

    let start = normalize_date_argument(from)?;
    let end = match to {
        Some(value) => normalize_date_argument(Some(value))?,
        None if from.is_some() => start.clone(),
        None => today,
    };
    if start > end {
        return Err(CliError::operation(format!(
            "start date must be before or equal to end date: {start} > {end}"
        )));
    }
    Ok((start, end))
}

fn run_daily_list_command(
    paths: &VaultPaths,
    from: Option<&str>,
    to: Option<&str>,
    week: bool,
    month: bool,
) -> Result<Vec<DailyListItem>, CliError> {
    let config = load_vault_config(paths).config;
    let (start, end) = resolve_daily_list_window(&config.periodic, from, to, week, month)?;
    list_daily_note_events(paths, &start, &end)
        .map(|items| {
            items
                .into_iter()
                .map(|item| {
                    let events = item
                        .events
                        .into_iter()
                        .map(|event| PeriodicEventReport {
                            start_time: event.start_time,
                            end_time: event.end_time,
                            title: event.title,
                            metadata: event.metadata,
                            tags: event.tags,
                        })
                        .collect::<Vec<_>>();
                    DailyListItem {
                        period_type: "daily".to_string(),
                        date: item.date,
                        path: item.path,
                        event_count: events.len(),
                        events,
                    }
                })
                .collect()
        })
        .map_err(CliError::operation)
}

fn run_daily_export_ics_command(
    paths: &VaultPaths,
    from: Option<&str>,
    to: Option<&str>,
    week: bool,
    month: bool,
    path: Option<&Path>,
    calendar_name: Option<&str>,
) -> Result<DailyIcsExportReport, CliError> {
    let config = load_vault_config(paths).config;
    let (start, end) = resolve_daily_list_window(&config.periodic, from, to, week, month)?;
    let export = export_daily_events_to_ics(paths, &start, &end, calendar_name)
        .map_err(CliError::operation)?;

    let written_path = path.map(|path| path.to_string_lossy().into_owned());
    if let Some(path) = path {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        fs::write(path, &export.content).map_err(CliError::operation)?;
    }

    Ok(DailyIcsExportReport {
        from: start,
        to: end,
        calendar_name: export.calendar_name,
        note_count: export.note_count,
        event_count: export.event_count,
        path: written_path,
        content: export.content,
    })
}

fn run_daily_append_command(
    paths: &VaultPaths,
    text: &str,
    heading: Option<&str>,
    date: Option<&str>,
    no_commit: bool,
) -> Result<DailyAppendReport, CliError> {
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit);

    let config = load_vault_config(paths).config;
    let target = resolve_periodic_target(&config.periodic, "daily", date, true)?;
    let mut warnings = Vec::new();
    let created = write_periodic_note_if_missing(paths, "daily", &target.path, &mut warnings)?;
    let absolute_path = paths.vault_root().join(&target.path);
    let existing = fs::read_to_string(&absolute_path).unwrap_or_default();
    let updated = heading.map_or_else(
        || append_at_end(&existing, text),
        |heading| append_under_heading(&existing, heading, text),
    );
    fs::write(&absolute_path, updated).map_err(CliError::operation)?;

    run_incremental_scan(paths, OutputFormat::Human, false)?;
    commit_periodic_changes_if_needed(&auto_commit, paths, "daily", &target.path)?;

    Ok(DailyAppendReport {
        period_type: target.period_type,
        reference_date: target.reference_date,
        start_date: target.start_date,
        end_date: target.end_date,
        path: target.path,
        created,
        heading: heading.map(ToOwned::to_owned),
        appended: true,
        warnings,
    })
}

fn validate_periodic_type(config: &PeriodicConfig, period_type: &str) -> Result<(), CliError> {
    if config.note(period_type).is_none() {
        return Err(CliError::operation(format!(
            "unknown periodic note type: {period_type}"
        )));
    }
    Ok(())
}

fn run_periodic_list_command(
    paths: &VaultPaths,
    period_type: Option<&str>,
) -> Result<Vec<PeriodicListItem>, CliError> {
    let config = load_vault_config(paths).config;
    if let Some(period_type) = period_type {
        validate_periodic_type(&config.periodic, period_type)?;
    }

    let database = CacheDatabase::open(paths).map_err(CliError::operation)?;
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
        .map_err(CliError::operation)?;
    let rows = statement
        .query_map([period_type], |row| {
            Ok(PeriodicListItem {
                period_type: row.get(0)?,
                date: row.get(1)?,
                path: row.get(2)?,
                event_count: row.get::<_, i64>(3)?.try_into().unwrap_or(usize::MAX),
            })
        })
        .map_err(CliError::operation)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(CliError::operation)
}

fn resolve_gap_range_for_type(
    config: &PeriodicConfig,
    period_type: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(String, String), CliError> {
    let today = current_utc_date_string();
    let from_date = match from {
        Some(value) => normalize_date_argument(Some(value))?,
        None if to.is_some() => normalize_date_argument(to)?,
        None => today.clone(),
    };
    let to_date = match to {
        Some(value) => normalize_date_argument(Some(value))?,
        None if from.is_some() => from_date.clone(),
        None => today,
    };
    if from_date > to_date {
        return Err(CliError::operation(format!(
            "start date must be before or equal to end date: {from_date} > {to_date}"
        )));
    }

    let start = period_range_for_date(config, period_type, &from_date)
        .ok_or_else(|| {
            CliError::operation(format!(
                "failed to resolve period range for `{period_type}` and {from_date}"
            ))
        })?
        .0;
    let end = period_range_for_date(config, period_type, &to_date)
        .ok_or_else(|| {
            CliError::operation(format!(
                "failed to resolve period range for `{period_type}` and {to_date}"
            ))
        })?
        .0;

    Ok((start, end))
}

fn run_periodic_gaps_command(
    paths: &VaultPaths,
    period_type: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Vec<PeriodicGapItem>, CliError> {
    let config = load_vault_config(paths).config;
    let types = if let Some(period_type) = period_type {
        validate_periodic_type(&config.periodic, period_type)?;
        vec![period_type.to_string()]
    } else {
        config
            .periodic
            .notes
            .iter()
            .filter_map(|(name, note)| note.enabled.then_some(name.clone()))
            .collect::<Vec<_>>()
    };
    if types.is_empty() {
        return Err(CliError::operation(
            "no enabled periodic note types are configured",
        ));
    }

    let mut gaps = Vec::new();
    for period_type in types {
        let (range_start, range_end) =
            resolve_gap_range_for_type(&config.periodic, &period_type, from, to)?;
        let mut current = range_start;
        while current <= range_end {
            if resolve_periodic_note(paths.vault_root(), &config.periodic, &period_type, &current)
                .is_none()
            {
                let expected_path =
                    expected_periodic_note_path(&config.periodic, &period_type, &current)
                        .ok_or_else(|| {
                            CliError::operation(format!(
                        "failed to resolve expected note path for `{period_type}` and {current}"
                    ))
                        })?;
                gaps.push(PeriodicGapItem {
                    period_type: period_type.clone(),
                    date: current.clone(),
                    expected_path,
                });
            }
            current =
                step_period_start(&config.periodic, &period_type, &current).ok_or_else(|| {
                    CliError::operation(format!(
                        "failed to step periodic range for `{period_type}` at {current}"
                    ))
                })?;
        }
    }

    Ok(gaps)
}

pub(crate) fn create_note_from_bases_view(
    paths: &VaultPaths,
    file: &str,
    view_index: usize,
    title: Option<&str>,
    dry_run: bool,
) -> Result<BasesCreateReport, CliError> {
    let context = plan_base_note_create(paths, file, view_index).map_err(CliError::operation)?;
    let path = allocate_bases_note_path(paths, &context, title)?;
    let contents = render_bases_note_contents(paths, &context, &path)?;

    if !dry_run {
        let absolute = paths.vault_root().join(&path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        fs::write(&absolute, contents).map_err(CliError::operation)?;
    }

    Ok(BasesCreateReport {
        file: context.file,
        view_name: context.view_name,
        view_index: context.view_index,
        dry_run,
        path,
        folder: context.folder,
        template: context.template,
        properties: context.properties,
        filters: context.filters,
    })
}

fn allocate_bases_note_path(
    paths: &VaultPaths,
    context: &BasesCreateContext,
    title: Option<&str>,
) -> Result<String, CliError> {
    let stem = sanitize_new_note_title(title.unwrap_or("Untitled"));
    let folder_prefix = context
        .folder
        .as_deref()
        .filter(|folder| !folder.is_empty())
        .map_or_else(String::new, |folder| format!("{folder}/"));

    for index in 0.. {
        let suffix = if index == 0 {
            String::new()
        } else {
            format!(" {}", index + 1)
        };
        let candidate = format!("{folder_prefix}{stem}{suffix}.md");
        let normalized = normalize_relative_input_path(
            &candidate,
            RelativePathOptions {
                expected_extension: Some("md"),
                append_extension_if_missing: false,
            },
        )
        .map_err(CliError::operation)?;
        if !paths.vault_root().join(&normalized).exists() {
            return Ok(normalized);
        }
    }

    Err(CliError::operation("failed to allocate a note path"))
}

fn sanitize_new_note_title(title: &str) -> String {
    let trimmed = title.trim();
    let trimmed = trimmed.strip_suffix(".md").unwrap_or(trimmed);
    let sanitized = trimmed
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            _ if character.is_control() => '-',
            _ => character,
        })
        .collect::<String>();
    let sanitized = sanitized.trim().trim_matches('.').to_string();
    if sanitized.is_empty() {
        "Untitled".to_string()
    } else {
        sanitized
    }
}

fn render_bases_note_contents(
    paths: &VaultPaths,
    context: &BasesCreateContext,
    relative_path: &str,
) -> Result<String, CliError> {
    let config = load_vault_config(paths).config;
    let rendered_template = if let Some(template_name) = context.template.as_deref() {
        let templates = discover_templates(
            paths,
            config.templates.obsidian_folder.as_deref(),
            config.templates.templater_folder.as_deref(),
        )?;
        let template = resolve_template_file(paths, &templates.templates, template_name)?;
        let source = fs::read_to_string(&template.absolute_path).map_err(CliError::operation)?;
        render_template_request(TemplateRenderRequest {
            paths,
            vault_config: &config,
            templates: &templates.templates,
            template_path: Some(&template.absolute_path),
            template_text: &source,
            target_path: relative_path,
            target_contents: None,
            engine: TemplateEngineKind::Auto,
            vars: &HashMap::new(),
            allow_mutations: true,
            run_mode: TemplateRunMode::Create,
        })?
        .content
    } else {
        String::new()
    };
    let (template_frontmatter, template_body) =
        parse_frontmatter_document(&rendered_template, true).map_err(CliError::operation)?;
    let derived_frontmatter = build_bases_create_frontmatter(&context.properties)?;
    let merged_frontmatter = merge_template_frontmatter(derived_frontmatter, template_frontmatter);

    render_note_from_parts(merged_frontmatter.as_ref(), &template_body).map_err(CliError::operation)
}

fn build_bases_create_frontmatter(
    properties: &BTreeMap<String, Value>,
) -> Result<Option<YamlMapping>, CliError> {
    if properties.is_empty() {
        return Ok(None);
    }

    let mut mapping = YamlMapping::new();
    for (key, value) in properties {
        mapping.insert(
            YamlValue::String(key.clone()),
            serde_yaml::to_value(value).map_err(CliError::operation)?,
        );
    }
    Ok(Some(mapping))
}

enum TemplateCommandResult {
    List(TemplateListReport),
    Create(TemplateCreateReport),
    Insert(TemplateInsertReport),
    Preview(TemplatePreviewReport),
}

fn discover_templates(
    paths: &VaultPaths,
    obsidian_folder: Option<&Path>,
    templater_folder: Option<&Path>,
) -> Result<TemplateDiscovery, CliError> {
    let mut warnings = Vec::new();
    let mut templates = list_templates_in_directory(
        &paths.vulcan_dir().join("templates"),
        ".vulcan/templates",
        "vulcan",
    )?;
    merge_template_source(
        &mut templates,
        &mut warnings,
        paths,
        templater_folder,
        "templater",
    )?;
    merge_template_source(
        &mut templates,
        &mut warnings,
        paths,
        obsidian_folder,
        "obsidian",
    )?;

    templates.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(TemplateDiscovery {
        templates,
        warnings,
    })
}

fn merge_template_source(
    templates: &mut Vec<TemplateCandidate>,
    warnings: &mut Vec<String>,
    paths: &VaultPaths,
    folder: Option<&Path>,
    source: &'static str,
) -> Result<(), CliError> {
    let mut discovered = folder
        .filter(|folder| !folder.as_os_str().is_empty())
        .map(|folder| {
            list_templates_in_directory(
                &paths.vault_root().join(folder),
                &folder.to_string_lossy(),
                source,
            )
        })
        .transpose()?
        .unwrap_or_default();

    for candidate in discovered.drain(..) {
        if let Some(existing) = templates
            .iter_mut()
            .find(|template| template.name == candidate.name)
        {
            let warning = format!(
                "template {} exists in both {} and {}; using {}",
                existing.name, candidate.display_path, existing.display_path, existing.display_path
            );
            existing.warning = Some(warning.clone());
            warnings.push(warning);
        } else {
            templates.push(candidate);
        }
    }

    Ok(())
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
    let templates_config = load_vault_config(paths).config.templates;
    let templater_folder = templates_config.templater_folder;
    let obsidian_folder = templates_config.obsidian_folder;
    if let Some(folder) = templater_folder.filter(|folder| !folder.as_os_str().is_empty()) {
        searched.push(paths.vault_root().join(folder).display().to_string());
    }
    if let Some(folder) = obsidian_folder.filter(|folder| !folder.as_os_str().is_empty()) {
        searched.push(paths.vault_root().join(folder).display().to_string());
    }

    Err(CliError::operation(format!(
        "template not found in {}: {name}",
        searched.join(", ")
    )))
}

fn list_templates_in_directory(
    template_dir: &Path,
    display_root: &str,
    source: &'static str,
) -> Result<Vec<TemplateCandidate>, CliError> {
    if !template_dir.exists() {
        return Ok(Vec::new());
    }

    let mut templates = fs::read_dir(template_dir)
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
        format!("{date}-{template_file}")
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
    let mapping = value.as_mapping().cloned().ok_or({
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
    frontmatter: Option<&YamlMapping>,
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
    prepared: &PreparedTemplateInsertion,
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

    render_note_from_parts(prepared.merged_frontmatter.as_ref(), &body)
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

#[cfg(test)]
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
            .as_secs()
            .try_into()
            .unwrap_or(i64::MAX);
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

    fn from_millis(ms: i64) -> Self {
        let seconds = ms.div_euclid(1_000);
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
            if let Some(token) = Self::next_obsidian_token(remaining) {
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

    fn next_obsidian_token(input: &str) -> Option<&'static str> {
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
            "hh" => format!("{hour_12:02}"),
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
    let year = y + i64::from(month <= 2);
    (year, month, day)
}

fn append_at_end(contents: &str, entry: &str) -> String {
    append_entry_at_end(contents, entry).updated
}

fn append_under_heading(contents: &str, heading: &str, entry: &str) -> String {
    append_entry_under_heading(contents, heading, entry).updated
}

fn append_entry_to_note(contents: &str, entry: &str, heading: Option<&str>) -> NoteEntryInsertion {
    if let Some(heading) = heading {
        append_entry_under_heading(contents, heading, entry)
    } else {
        append_entry_at_end(contents, entry)
    }
}

fn append_entry_at_end(contents: &str, entry: &str) -> NoteEntryInsertion {
    let mut prefix = contents.trim_end_matches('\n').to_string();
    if !prefix.is_empty() {
        prefix.push_str("\n\n");
    }
    let line_number = i64::try_from(prefix.lines().count().saturating_add(1))
        .expect("line count should fit in i64");
    let mut updated = prefix;
    updated.push_str(entry.trim_end());
    updated.push('\n');

    NoteEntryInsertion {
        updated,
        line_number,
        change: RefactorChange {
            before: String::new(),
            after: entry.trim_end().to_string(),
        },
    }
}

fn append_entry_under_heading(contents: &str, heading: &str, entry: &str) -> NoteEntryInsertion {
    let heading = heading.trim();
    if heading.is_empty() {
        return append_entry_at_end(contents, entry);
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
        let mut prefix = String::new();
        prefix.push_str(&contents[..insert_at]);
        if !prefix.ends_with('\n') {
            prefix.push('\n');
        }
        if !prefix.ends_with("\n\n") {
            prefix.push('\n');
        }
        let line_number = i64::try_from(prefix.lines().count().saturating_add(1))
            .expect("line count should fit in i64");
        let mut updated = prefix;
        updated.push_str(entry.trim_end());
        updated.push('\n');
        if insert_at < contents.len() && !contents[insert_at..].starts_with('\n') {
            updated.push('\n');
        }
        updated.push_str(&contents[insert_at..]);
        NoteEntryInsertion {
            updated,
            line_number,
            change: RefactorChange {
                before: String::new(),
                after: entry.trim_end().to_string(),
            },
        }
    } else {
        let mut prefix = contents.trim_end_matches('\n').to_string();
        if !prefix.is_empty() {
            prefix.push_str("\n\n");
        }
        prefix.push_str(heading);
        prefix.push_str("\n\n");
        let line_number = i64::try_from(prefix.lines().count().saturating_add(1))
            .expect("line count should fit in i64");
        let mut updated = prefix;
        updated.push_str(entry.trim_end());
        updated.push('\n');
        NoteEntryInsertion {
            updated,
            line_number,
            change: RefactorChange {
                before: String::new(),
                after: entry.trim_end().to_string(),
            },
        }
    }
}

fn markdown_heading_level(line: &str) -> Option<usize> {
    let hashes = line.chars().take_while(|ch| *ch == '#').count();
    (hashes > 0 && hashes <= 6 && line.chars().nth(hashes).is_some_and(char::is_whitespace))
        .then_some(hashes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceLine {
    number: usize,
    raw: String,
    text: String,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy)]
struct NoteGetOptions<'a> {
    note: &'a str,
    heading: Option<&'a str>,
    block_ref: Option<&'a str>,
    lines: Option<&'a str>,
    match_pattern: Option<&'a str>,
    context: usize,
    no_frontmatter: bool,
    raw: bool,
}

#[derive(Debug, Clone)]
enum NotePatchMatcher {
    Literal(String),
    Regex(Regex),
}

#[derive(Debug, Clone, Copy)]
struct NotePatchOptions<'a> {
    note: &'a str,
    find: &'a str,
    replace: &'a str,
    replace_all: bool,
    check: bool,
    dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotePatchApplication {
    updated_content: String,
    match_count: usize,
    changes: Vec<RefactorChange>,
    regex: bool,
}

fn run_note_get_command(
    paths: &VaultPaths,
    options: NoteGetOptions<'_>,
) -> Result<NoteGetReport, CliError> {
    let NoteGetOptions {
        note,
        heading,
        block_ref,
        lines,
        match_pattern,
        context,
        no_frontmatter,
        raw,
    } = options;
    let (relative_path, source) = read_existing_note_source(paths, note)?;
    let config = load_vault_config(paths).config;
    let parsed = vulcan_core::parse_document(&source, &config);
    let source_lines = build_source_lines(&source);

    let mut selected = (0..source_lines.len()).collect::<Vec<_>>();
    if let Some(heading) = heading {
        let allowed = heading_line_indices(&source, &source_lines, &parsed, heading)?;
        selected = intersect_sorted_line_indices(&selected, &allowed);
    }
    if let Some(block_ref) = block_ref {
        let allowed = block_ref_line_indices(&source_lines, &parsed, block_ref)?;
        selected = intersect_sorted_line_indices(&selected, &allowed);
    }
    if let Some(spec) = lines {
        selected = select_line_range(&selected, spec)?;
    }

    let mut match_count = 0;
    if let Some(pattern) = match_pattern {
        let regex = Regex::new(pattern).map_err(CliError::operation)?;
        let (filtered, hits) = select_matching_lines(&selected, &source_lines, &regex, context);
        selected = filtered;
        match_count = hits;
    }

    if no_frontmatter {
        selected = strip_frontmatter_lines(&selected, &source, &source_lines);
    }

    let line_spans = selected_line_spans(&selected, &source_lines);
    let selected_content = render_selected_raw_content(&selected, &source_lines);
    let frontmatter = parsed
        .frontmatter
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(CliError::operation)?;

    Ok(NoteGetReport {
        path: relative_path,
        content: selected_content,
        frontmatter,
        metadata: NoteGetMetadata {
            heading: heading.map(ToOwned::to_owned),
            block_ref: block_ref.map(ToOwned::to_owned),
            lines: lines.map(ToOwned::to_owned),
            match_pattern: match_pattern.map(ToOwned::to_owned),
            context,
            no_frontmatter,
            raw,
            match_count,
            line_spans,
        },
        display_lines: selected
            .iter()
            .map(|index| NoteDisplayLine {
                line_number: source_lines[*index].number,
                text: source_lines[*index].text.clone(),
            })
            .collect(),
    })
}

fn run_note_set_command(
    paths: &VaultPaths,
    note: &str,
    file: Option<&PathBuf>,
    no_frontmatter: bool,
    check: bool,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<NoteSetReport, CliError> {
    let (relative_path, existing) = read_existing_note_source(paths, note)?;
    let replacement = note_set_input_text(file)?;
    let updated = if no_frontmatter {
        preserve_existing_frontmatter(&existing, &replacement)
    } else {
        replacement
    };
    fs::write(paths.vault_root().join(&relative_path), &updated).map_err(CliError::operation)?;
    let diagnostics = maybe_check_note(paths, &relative_path, &updated, check)?;
    run_incremental_scan(paths, output, use_stderr_color)?;

    Ok(NoteSetReport {
        path: relative_path,
        checked: check,
        preserved_frontmatter: no_frontmatter,
        diagnostics,
    })
}

fn run_note_create_command(
    paths: &VaultPaths,
    path: &str,
    template: Option<&str>,
    frontmatter: &[String],
    check: bool,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<NoteCreateReport, CliError> {
    let requested_path = normalize_note_path(path)?;

    let config = load_vault_config(paths).config;
    let mut warnings = Vec::new();
    let mut rendered_template = None;
    let mut frontmatter_mapping = parse_frontmatter_bindings(frontmatter)?;
    let mut body = read_optional_stdin_text().map_err(CliError::operation)?;
    let mut final_path = requested_path.clone();
    let mut changed_paths = Vec::new();

    if let Some(template_name) = template {
        let templates = discover_templates(
            paths,
            config.templates.obsidian_folder.as_deref(),
            config.templates.templater_folder.as_deref(),
        )?;
        let template_file = resolve_template_file(paths, &templates.templates, template_name)?;
        let template_source =
            fs::read_to_string(&template_file.absolute_path).map_err(CliError::operation)?;
        let vars = HashMap::new();
        let rendered = render_template_request(TemplateRenderRequest {
            paths,
            vault_config: &config,
            templates: &templates.templates,
            template_path: Some(&template_file.absolute_path),
            template_text: &template_source,
            target_path: &requested_path,
            target_contents: None,
            engine: TemplateEngineKind::Auto,
            vars: &vars,
            allow_mutations: true,
            run_mode: TemplateRunMode::Create,
        })?;
        let (template_frontmatter, template_body) =
            parse_frontmatter_document(&rendered.content, true).map_err(CliError::operation)?;
        frontmatter_mapping = merge_explicit_frontmatter(template_frontmatter, frontmatter_mapping);
        body = merge_note_create_bodies(&template_body, &body);
        final_path.clone_from(&rendered.target_path);
        warnings.extend(template_file.warning);
        warnings.extend(rendered.warnings.clone());
        warnings.extend(rendered.diagnostics.clone());
        changed_paths.extend(rendered.changed_paths.clone());
        rendered_template = Some((template_name.to_string(), rendered));
    }

    let absolute_path = paths.vault_root().join(&final_path);
    if absolute_path.exists() {
        return Err(CliError::operation(format!(
            "destination note already exists: {final_path}"
        )));
    }

    let final_content =
        render_note_from_parts(frontmatter_mapping.as_ref(), &body).map_err(CliError::operation)?;
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    fs::write(&absolute_path, &final_content).map_err(CliError::operation)?;
    let diagnostics = maybe_check_note(paths, &final_path, &final_content, check)?;
    run_incremental_scan(paths, output, use_stderr_color)?;
    changed_paths.push(final_path.clone());
    changed_paths.sort();
    changed_paths.dedup();

    Ok(NoteCreateReport {
        path: final_path,
        created: true,
        checked: check,
        template: rendered_template
            .as_ref()
            .map(|(template_name, _)| template_name.clone()),
        engine: rendered_template
            .as_ref()
            .map(|(_, rendered)| rendered.engine.as_str().to_string()),
        warnings,
        diagnostics,
        changed_paths,
    })
}

fn note_append_periodic_type(periodic: NoteAppendPeriodicArg) -> &'static str {
    match periodic {
        NoteAppendPeriodicArg::Daily => "daily",
        NoteAppendPeriodicArg::Weekly => "weekly",
        NoteAppendPeriodicArg::Monthly => "monthly",
    }
}

fn prepend_entry_after_frontmatter(contents: &str, entry: &str) -> NoteEntryInsertion {
    let body_start = find_frontmatter_block(contents).map_or(0, |(_, _, body_start)| body_start);
    let prefix = &contents[..body_start];
    let body = contents[body_start..].trim_start_matches('\n');
    let mut updated = prefix.to_string();
    let line_number = i64::try_from(updated.lines().count().saturating_add(1))
        .expect("line count should fit in i64");
    updated.push_str(entry.trim_end());
    updated.push('\n');
    if !body.is_empty() {
        updated.push('\n');
        updated.push_str(body.trim_end_matches('\n'));
        updated.push('\n');
    }

    NoteEntryInsertion {
        updated,
        line_number,
        change: RefactorChange {
            before: String::new(),
            after: entry.trim_end().to_string(),
        },
    }
}

#[derive(Debug, Clone, Copy)]
struct NoteAppendOptions<'a> {
    note: Option<&'a str>,
    text: &'a str,
    mode: NoteAppendMode,
    heading: Option<&'a str>,
    periodic: Option<NoteAppendPeriodicArg>,
    date: Option<&'a str>,
    vars: &'a [String],
    check: bool,
}

fn run_note_append_command(
    paths: &VaultPaths,
    options: NoteAppendOptions<'_>,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<NoteAppendReport, CliError> {
    let NoteAppendOptions {
        note,
        text,
        mode,
        heading,
        periodic,
        date,
        vars,
        check,
    } = options;
    let config = load_vault_config(paths).config;
    let bound_vars = parse_template_var_bindings(vars)?;
    let (relative_path, existing, created, period_type, reference_date, mut warnings) =
        if let Some(periodic) = periodic {
            let period_type = note_append_periodic_type(periodic);
            let target = resolve_periodic_target(&config.periodic, period_type, date, true)?;
            let mut warnings = Vec::new();
            let created =
                write_periodic_note_if_missing(paths, period_type, &target.path, &mut warnings)?;
            let absolute_path = paths.vault_root().join(&target.path);
            let existing = fs::read_to_string(&absolute_path).unwrap_or_default();
            (
                target.path,
                existing,
                created,
                Some(period_type.to_string()),
                Some(target.reference_date),
                warnings,
            )
        } else {
            let note = note.ok_or_else(|| {
                CliError::operation("`note append` requires a note or --periodic <type>")
            })?;
            let (relative_path, existing) = read_existing_note_source(paths, note)?;
            (relative_path, existing, false, None, None, Vec::new())
        };
    let appended_text = note_append_input_text(text)?;
    let rendered = render_template_request(TemplateRenderRequest {
        paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: &appended_text,
        target_path: &relative_path,
        target_contents: Some(&existing),
        engine: TemplateEngineKind::Native,
        vars: &bound_vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Append,
    })?;
    warnings.extend(rendered.warnings);
    warnings.extend(rendered.diagnostics);
    let insertion = match mode {
        NoteAppendMode::Append => append_entry_at_end(&existing, &rendered.content),
        NoteAppendMode::Prepend => prepend_entry_after_frontmatter(&existing, &rendered.content),
        NoteAppendMode::AfterHeading => {
            append_entry_under_heading(&existing, heading.unwrap_or_default(), &rendered.content)
        }
    };
    fs::write(paths.vault_root().join(&relative_path), &insertion.updated)
        .map_err(CliError::operation)?;
    let diagnostics = maybe_check_note(paths, &relative_path, &insertion.updated, check)?;
    run_incremental_scan(paths, output, use_stderr_color)?;

    Ok(NoteAppendReport {
        path: relative_path,
        appended: true,
        mode: mode.as_str().to_string(),
        checked: check,
        created,
        heading: heading.map(ToOwned::to_owned),
        period_type,
        reference_date,
        warnings,
        diagnostics,
    })
}

fn run_note_patch_command(
    paths: &VaultPaths,
    options: NotePatchOptions<'_>,
    output: OutputFormat,
    use_stderr_color: bool,
) -> Result<NotePatchReport, CliError> {
    let NotePatchOptions {
        note,
        find,
        replace,
        replace_all,
        check,
        dry_run,
    } = options;
    let (relative_path, existing) = read_existing_note_source(paths, note)?;
    let matcher = parse_note_patch_matcher(find)?;
    let application = apply_note_patch(&existing, &matcher, replace, replace_all)?;
    if !dry_run {
        fs::write(
            paths.vault_root().join(&relative_path),
            &application.updated_content,
        )
        .map_err(CliError::operation)?;
        run_incremental_scan(paths, output, use_stderr_color)?;
    }
    let diagnostics = maybe_check_note(paths, &relative_path, &application.updated_content, check)?;

    Ok(NotePatchReport {
        path: relative_path,
        dry_run,
        checked: check,
        pattern: find.to_string(),
        regex: application.regex,
        replace: replace.to_string(),
        match_count: application.match_count,
        changes: application.changes,
        diagnostics,
    })
}

fn run_note_doctor_command(paths: &VaultPaths, note: &str) -> Result<NoteDoctorReport, CliError> {
    let (relative_path, source) = read_existing_note_source(paths, note)?;
    let diagnostics = diagnose_note_contents(paths, &relative_path, &source)?;
    Ok(NoteDoctorReport {
        path: relative_path,
        diagnostics,
    })
}

fn read_existing_note_source(paths: &VaultPaths, note: &str) -> Result<(String, String), CliError> {
    let relative_path = resolve_existing_note_path(paths, note)?;
    let source =
        fs::read_to_string(paths.vault_root().join(&relative_path)).map_err(CliError::operation)?;
    Ok((relative_path, source))
}

fn resolve_existing_note_path(paths: &VaultPaths, note: &str) -> Result<String, CliError> {
    match resolve_note_reference(paths, note) {
        Ok(resolved) => Ok(resolved.path),
        Err(GraphQueryError::AmbiguousIdentifier { .. }) => Err(CliError::operation(format!(
            "note identifier '{note}' is ambiguous"
        ))),
        Err(GraphQueryError::CacheMissing | GraphQueryError::NoteNotFound { .. }) => {
            let normalized = normalize_note_path(note)?;
            if paths.vault_root().join(&normalized).is_file() {
                Ok(normalized)
            } else {
                Err(CliError::operation(format!("note not found: {note}")))
            }
        }
        Err(error) => Err(CliError::operation(error)),
    }
}

fn normalize_note_path(path: &str) -> Result<String, CliError> {
    normalize_relative_input_path(
        path,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(CliError::operation)
}

fn note_set_input_text(file: Option<&PathBuf>) -> Result<String, CliError> {
    if let Some(file) = file {
        return fs::read_to_string(file).map_err(CliError::operation);
    }
    if io::stdin().is_terminal() {
        return Err(CliError::operation(
            "`note set` requires replacement content on stdin or --file <path>",
        ));
    }
    let mut buffer = String::new();
    io::stdin()
        .read_to_string(&mut buffer)
        .map_err(CliError::operation)?;
    Ok(buffer)
}

fn note_append_input_text(text: &str) -> Result<String, CliError> {
    if text != "-" {
        return Ok(text.to_string());
    }

    let mut buffer = String::new();
    io::stdin()
        .read_to_string(&mut buffer)
        .map_err(CliError::operation)?;
    Ok(buffer)
}

fn read_optional_stdin_text() -> io::Result<String> {
    if io::stdin().is_terminal() {
        return Ok(String::new());
    }

    let mut buffer = String::new();
    io::stdin().read_to_string(&mut buffer)?;
    Ok(buffer)
}

fn preserve_existing_frontmatter(existing: &str, body: &str) -> String {
    find_frontmatter_block(existing).map_or_else(
        || body.to_string(),
        |(_, _, body_start)| {
            let mut rendered = existing[..body_start].to_string();
            rendered.push_str(body);
            rendered
        },
    )
}

fn merge_note_create_bodies(template_body: &str, stdin_body: &str) -> String {
    match (
        template_body.trim().is_empty(),
        stdin_body.trim().is_empty(),
    ) {
        (true, true) => String::new(),
        (false, true) => template_body.to_string(),
        (true, false) => stdin_body.to_string(),
        (false, false) => merge_body_sections(template_body, stdin_body, true),
    }
}

fn parse_frontmatter_bindings(bindings: &[String]) -> Result<Option<YamlMapping>, CliError> {
    if bindings.is_empty() {
        return Ok(None);
    }

    let mut mapping = YamlMapping::new();
    for binding in bindings {
        let Some((key, value)) = binding.split_once('=') else {
            return Err(CliError::operation(format!(
                "frontmatter bindings must use key=value syntax: {binding}"
            )));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(CliError::operation(format!(
                "frontmatter bindings need a non-empty key: {binding}"
            )));
        }
        let parsed =
            serde_yaml::from_str::<YamlValue>(value.trim()).map_err(CliError::operation)?;
        mapping.insert(YamlValue::String(key.to_string()), parsed);
    }

    Ok(Some(mapping))
}

fn merge_explicit_frontmatter(
    existing: Option<YamlMapping>,
    explicit: Option<YamlMapping>,
) -> Option<YamlMapping> {
    match (existing, explicit) {
        (None, None) => None,
        (Some(mapping), None) | (None, Some(mapping)) => Some(mapping),
        (Some(mut existing), Some(explicit)) => {
            for (key, value) in explicit {
                existing.insert(key, value);
            }
            Some(existing)
        }
    }
}

fn build_source_lines(source: &str) -> Vec<SourceLine> {
    let mut offset = 0usize;
    source
        .split_inclusive('\n')
        .enumerate()
        .map(|(index, raw_line)| {
            let line = SourceLine {
                number: index + 1,
                raw: raw_line.to_string(),
                text: raw_line.trim_end_matches(['\n', '\r']).to_string(),
                start: offset,
                end: offset + raw_line.len(),
            };
            offset += raw_line.len();
            line
        })
        .collect()
}

fn heading_line_indices(
    source: &str,
    source_lines: &[SourceLine],
    parsed: &vulcan_core::ParsedDocument,
    heading: &str,
) -> Result<Vec<usize>, CliError> {
    let matches = parsed
        .headings
        .iter()
        .filter(|candidate| candidate.text == heading)
        .collect::<Vec<_>>();
    let heading = match matches.as_slice() {
        [] => {
            return Err(CliError::operation(format!(
                "no heading named '{heading}' found"
            )))
        }
        [heading] => *heading,
        _ => {
            return Err(CliError::operation(format!(
                "multiple heading entries named '{heading}'"
            )))
        }
    };
    let end = parsed
        .headings
        .iter()
        .filter(|candidate| candidate.byte_offset > heading.byte_offset)
        .find(|candidate| candidate.level <= heading.level)
        .map_or(source.len(), |candidate| candidate.byte_offset);
    Ok(line_indices_for_byte_range(
        source_lines,
        heading.byte_offset,
        end,
    ))
}

fn block_ref_line_indices(
    source_lines: &[SourceLine],
    parsed: &vulcan_core::ParsedDocument,
    block_ref: &str,
) -> Result<Vec<usize>, CliError> {
    let matches = parsed
        .block_refs
        .iter()
        .filter(|candidate| candidate.block_id_text == block_ref)
        .collect::<Vec<_>>();
    let block_ref = match matches.as_slice() {
        [] => {
            return Err(CliError::operation(format!(
                "no block ref named '{block_ref}' found"
            )))
        }
        [block_ref] => *block_ref,
        _ => {
            return Err(CliError::operation(format!(
                "multiple block refs named '{block_ref}'"
            )))
        }
    };
    Ok(line_indices_for_byte_range(
        source_lines,
        block_ref.target_block_byte_start,
        block_ref.target_block_byte_end,
    ))
}

fn line_indices_for_byte_range(
    source_lines: &[SourceLine],
    start: usize,
    end: usize,
) -> Vec<usize> {
    source_lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.start < end && line.end > start)
        .map(|(index, _)| index)
        .collect()
}

fn intersect_sorted_line_indices(current: &[usize], allowed: &[usize]) -> Vec<usize> {
    let mut left = 0usize;
    let mut right = 0usize;
    let mut intersection = Vec::new();
    while left < current.len() && right < allowed.len() {
        match current[left].cmp(&allowed[right]) {
            std::cmp::Ordering::Less => left += 1,
            std::cmp::Ordering::Greater => right += 1,
            std::cmp::Ordering::Equal => {
                intersection.push(current[left]);
                left += 1;
                right += 1;
            }
        }
    }
    intersection
}

fn select_line_range(current: &[usize], spec: &str) -> Result<Vec<usize>, CliError> {
    if current.is_empty() {
        return Ok(Vec::new());
    }

    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(CliError::operation("line range must not be empty"));
    }

    let length = current.len();
    let (start, end) = if let Some(last_count) = trimmed.strip_prefix('-') {
        let count = parse_positive_usize(last_count, "line range")?;
        let start = length.saturating_sub(count).saturating_add(1);
        (start.max(1), length)
    } else if let Some((start, end)) = trimmed.split_once('-') {
        let start = parse_positive_usize(start, "line range start")?;
        let end = if end.trim().is_empty() {
            length
        } else {
            parse_positive_usize(end, "line range end")?
        };
        (start, end)
    } else {
        let line = parse_positive_usize(trimmed, "line range")?;
        (line, line)
    };

    if start == 0 || end == 0 || start > end {
        return Err(CliError::operation(format!("invalid line range: {spec}")));
    }

    let start_index = start.saturating_sub(1).min(length);
    let end_index = end.min(length);
    Ok(current[start_index..end_index].to_vec())
}

fn parse_positive_usize(value: &str, label: &str) -> Result<usize, CliError> {
    let parsed = value.trim().parse::<usize>().map_err(CliError::operation)?;
    if parsed == 0 {
        return Err(CliError::operation(format!("{label} must be >= 1")));
    }
    Ok(parsed)
}

fn select_matching_lines(
    current: &[usize],
    source_lines: &[SourceLine],
    pattern: &Regex,
    context: usize,
) -> (Vec<usize>, usize) {
    let hit_positions = current
        .iter()
        .enumerate()
        .filter_map(|(position, index)| {
            pattern
                .is_match(&source_lines[*index].text)
                .then_some(position)
        })
        .collect::<Vec<_>>();

    if hit_positions.is_empty() {
        return (Vec::new(), 0);
    }

    let mut selected_positions = Vec::new();
    for hit_position in &hit_positions {
        let start = hit_position.saturating_sub(context);
        let end = (*hit_position + context + 1).min(current.len());
        for position in start..end {
            if selected_positions.last() != Some(&position) {
                selected_positions.push(position);
            }
        }
    }

    (
        selected_positions
            .into_iter()
            .map(|position| current[position])
            .collect(),
        hit_positions.len(),
    )
}

fn strip_frontmatter_lines(
    current: &[usize],
    source: &str,
    source_lines: &[SourceLine],
) -> Vec<usize> {
    let Some((_, _, body_start)) = find_frontmatter_block(source) else {
        return current.to_vec();
    };

    current
        .iter()
        .copied()
        .filter(|index| source_lines[*index].start >= body_start)
        .collect()
}

fn selected_line_spans(selected: &[usize], source_lines: &[SourceLine]) -> Vec<NoteGetLineSpan> {
    if selected.is_empty() {
        return Vec::new();
    }

    let mut spans = Vec::new();
    let mut start_index = selected[0];
    let mut previous_index = selected[0];

    for current_index in selected.iter().copied().skip(1) {
        if current_index != previous_index + 1 {
            spans.push(NoteGetLineSpan {
                start_line: source_lines[start_index].number,
                end_line: source_lines[previous_index].number,
            });
            start_index = current_index;
        }
        previous_index = current_index;
    }

    spans.push(NoteGetLineSpan {
        start_line: source_lines[start_index].number,
        end_line: source_lines[previous_index].number,
    });
    spans
}

fn render_selected_raw_content(selected: &[usize], source_lines: &[SourceLine]) -> String {
    let mut rendered = String::new();
    for index in selected {
        rendered.push_str(&source_lines[*index].raw);
    }
    rendered
}

fn parse_note_patch_matcher(pattern: &str) -> Result<NotePatchMatcher, CliError> {
    if pattern.is_empty() {
        return Err(CliError::operation("`note patch --find` must not be empty"));
    }

    if let Some(regex_body) = pattern.strip_prefix('/') {
        let Some(regex_body) = regex_body.strip_suffix('/') else {
            return Err(CliError::operation(
                "regex patterns must use /.../ syntax, for example `/TODO \\d+/`",
            ));
        };
        if regex_body.is_empty() {
            return Err(CliError::operation("regex patterns must not be empty"));
        }
        return Regex::new(regex_body)
            .map(NotePatchMatcher::Regex)
            .map_err(CliError::operation);
    }

    Ok(NotePatchMatcher::Literal(pattern.to_string()))
}

fn apply_note_patch(
    source: &str,
    matcher: &NotePatchMatcher,
    replace: &str,
    all: bool,
) -> Result<NotePatchApplication, CliError> {
    match matcher {
        NotePatchMatcher::Literal(find) => {
            let patch_matches = source
                .match_indices(find)
                .map(|(start, matched)| {
                    (
                        start,
                        start + matched.len(),
                        matched.to_string(),
                        replace.to_string(),
                    )
                })
                .collect::<Vec<_>>();
            build_note_patch_application(source, patch_matches, all, false)
        }
        NotePatchMatcher::Regex(regex) => {
            let patch_matches = regex
                .find_iter(source)
                .map(|matched| {
                    if matched.start() == matched.end() {
                        Err(CliError::operation(
                            "regex patterns for `note patch` must not match empty strings",
                        ))
                    } else {
                        Ok((
                            matched.start(),
                            matched.end(),
                            matched.as_str().to_string(),
                            regex.replace(matched.as_str(), replace).into_owned(),
                        ))
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            build_note_patch_application(source, patch_matches, all, true)
        }
    }
}

fn build_note_patch_application(
    source: &str,
    matches: Vec<(usize, usize, String, String)>,
    all: bool,
    regex: bool,
) -> Result<NotePatchApplication, CliError> {
    match matches.len() {
        0 => Err(CliError::operation("pattern not found in note")),
        count if count > 1 && !all => Err(CliError::operation(format!(
            "pattern matched {count} times; rerun with --all to replace every match"
        ))),
        _ => {
            let mut updated = source.to_string();
            for (start, end, _, replacement) in matches.iter().rev() {
                updated.replace_range(*start..*end, replacement);
            }
            Ok(NotePatchApplication {
                updated_content: updated,
                match_count: matches.len(),
                changes: matches
                    .into_iter()
                    .map(|(_, _, before, after)| RefactorChange { before, after })
                    .collect(),
                regex,
            })
        }
    }
}

fn maybe_check_note(
    paths: &VaultPaths,
    relative_path: &str,
    content: &str,
    check: bool,
) -> Result<Vec<DoctorDiagnosticIssue>, CliError> {
    if !check {
        return Ok(Vec::new());
    }

    diagnose_note_contents(paths, relative_path, content)
}

fn diagnose_note_contents(
    paths: &VaultPaths,
    relative_path: &str,
    content: &str,
) -> Result<Vec<DoctorDiagnosticIssue>, CliError> {
    let config = load_vault_config(paths).config;
    let parsed = vulcan_core::parse_document(content, &config);
    let mut diagnostics = parsed
        .diagnostics
        .iter()
        .map(|diagnostic| DoctorDiagnosticIssue {
            document_path: Some(relative_path.to_string()),
            message: diagnostic.message.clone(),
            byte_range: diagnostic.byte_range.as_ref().map(|range| DoctorByteRange {
                start: range.start,
                end: range.end,
            }),
        })
        .collect::<Vec<_>>();

    if let Some(indexed) =
        extract_indexed_properties(&parsed, &config).map_err(CliError::operation)?
    {
        diagnostics.extend(indexed.diagnostics.into_iter().map(|diagnostic| {
            DoctorDiagnosticIssue {
                document_path: Some(relative_path.to_string()),
                message: diagnostic.message,
                byte_range: None,
            }
        }));
    }

    diagnostics.extend(dataview_parse_diagnostics(relative_path, &parsed));
    diagnostics.extend(link_resolution_diagnostics(
        paths,
        relative_path,
        &config,
        &parsed,
    )?);
    diagnostics.sort_by(|left, right| {
        left.document_path
            .cmp(&right.document_path)
            .then(left.message.cmp(&right.message))
            .then_with(|| match (&left.byte_range, &right.byte_range) {
                (Some(left), Some(right)) => {
                    left.start.cmp(&right.start).then(left.end.cmp(&right.end))
                }
                (None, Some(_)) => std::cmp::Ordering::Less,
                (Some(_), None) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
    });
    diagnostics.dedup();
    Ok(diagnostics)
}

fn dataview_parse_diagnostics(
    relative_path: &str,
    parsed: &vulcan_core::ParsedDocument,
) -> Vec<DoctorDiagnosticIssue> {
    parsed
        .dataview_blocks
        .iter()
        .filter(|block| block.language == "dataview")
        .filter_map(|block| {
            let output = parse_dql_with_diagnostics(&block.text);
            output
                .diagnostics
                .first()
                .map(|diagnostic| DoctorDiagnosticIssue {
                    document_path: Some(relative_path.to_string()),
                    message: format!(
                        "Dataview block {} at line {} failed to parse: {}",
                        block.block_index, block.line_number, diagnostic.message
                    ),
                    byte_range: Some(DoctorByteRange {
                        start: block.byte_range.start,
                        end: block.byte_range.end,
                    }),
                })
        })
        .collect()
}

fn link_resolution_diagnostics(
    paths: &VaultPaths,
    relative_path: &str,
    config: &vulcan_core::VaultConfig,
    parsed: &vulcan_core::ParsedDocument,
) -> Result<Vec<DoctorDiagnosticIssue>, CliError> {
    let resolver_documents = build_resolver_documents(paths, relative_path, parsed)?;
    let mut target_documents = HashMap::new();
    let mut diagnostics = Vec::new();

    for link in &parsed.links {
        let resolution = resolve_link(
            &resolver_documents,
            &vulcan_core::ResolverLink {
                source_document_id: relative_path.to_string(),
                source_path: relative_path.to_string(),
                target_path_candidate: link.target_path_candidate.clone(),
                link_kind: link.link_kind,
            },
            config.link_resolution,
        );
        match resolution.problem {
            Some(LinkResolutionProblem::Unresolved) => diagnostics.push(DoctorDiagnosticIssue {
                document_path: Some(relative_path.to_string()),
                message: format!("Unresolved link target `{}`", link.raw_text),
                byte_range: Some(DoctorByteRange {
                    start: link.byte_offset,
                    end: link.byte_offset + link.raw_text.len(),
                }),
            }),
            Some(LinkResolutionProblem::Ambiguous(matches)) => {
                diagnostics.push(DoctorDiagnosticIssue {
                    document_path: Some(relative_path.to_string()),
                    message: format!(
                        "Ambiguous link target `{}` matched {}",
                        link.raw_text,
                        matches.join(", ")
                    ),
                    byte_range: Some(DoctorByteRange {
                        start: link.byte_offset,
                        end: link.byte_offset + link.raw_text.len(),
                    }),
                });
            }
            None => {
                let Some(target_path) = resolution.resolved_target_id else {
                    continue;
                };
                if let Some(target_heading) = link.target_heading.as_deref() {
                    let target = load_target_document(
                        paths,
                        relative_path,
                        parsed,
                        &target_path,
                        &mut target_documents,
                    )?;
                    if !target
                        .headings
                        .iter()
                        .any(|heading| heading.text == target_heading)
                    {
                        diagnostics.push(DoctorDiagnosticIssue {
                            document_path: Some(relative_path.to_string()),
                            message: format!(
                                "Broken heading link `{}`: heading `{target_heading}` was not found in {target_path}",
                                link.raw_text
                            ),
                            byte_range: Some(DoctorByteRange {
                                start: link.byte_offset,
                                end: link.byte_offset + link.raw_text.len(),
                            }),
                        });
                    }
                }
                if let Some(target_block) = link.target_block.as_deref() {
                    let target = load_target_document(
                        paths,
                        relative_path,
                        parsed,
                        &target_path,
                        &mut target_documents,
                    )?;
                    if !target
                        .block_refs
                        .iter()
                        .any(|block_ref| block_ref.block_id_text == target_block)
                    {
                        diagnostics.push(DoctorDiagnosticIssue {
                            document_path: Some(relative_path.to_string()),
                            message: format!(
                                "Broken block link `{}`: block `^{target_block}` was not found in {target_path}",
                                link.raw_text
                            ),
                            byte_range: Some(DoctorByteRange {
                                start: link.byte_offset,
                                end: link.byte_offset + link.raw_text.len(),
                            }),
                        });
                    }
                }
            }
        }
    }

    Ok(diagnostics)
}

fn build_resolver_documents(
    paths: &VaultPaths,
    relative_path: &str,
    parsed: &vulcan_core::ParsedDocument,
) -> Result<Vec<vulcan_core::ResolverDocument>, CliError> {
    if let Ok(note_index) = load_note_index(paths) {
        let mut documents = note_index
            .into_values()
            .map(|note| vulcan_core::ResolverDocument {
                id: note.document_path.clone(),
                path: note.document_path,
                filename: note.file_name,
                aliases: note.aliases,
            })
            .collect::<Vec<_>>();
        if let Some(existing) = documents
            .iter_mut()
            .find(|document| document.path == relative_path)
        {
            existing.aliases.clone_from(&parsed.aliases);
        } else {
            documents.push(resolver_document_from_parsed(relative_path, parsed));
        }
        return Ok(documents);
    }

    let mut documents = Vec::new();
    for path in discover_markdown_note_paths(paths.vault_root()).map_err(CliError::operation)? {
        if path == relative_path {
            documents.push(resolver_document_from_parsed(relative_path, parsed));
            continue;
        }
        let source =
            fs::read_to_string(paths.vault_root().join(&path)).map_err(CliError::operation)?;
        let parsed_document =
            vulcan_core::parse_document(&source, &load_vault_config(paths).config);
        documents.push(resolver_document_from_parsed(&path, &parsed_document));
    }

    if !documents
        .iter()
        .any(|document| document.path == relative_path)
    {
        documents.push(resolver_document_from_parsed(relative_path, parsed));
    }
    Ok(documents)
}

fn resolver_document_from_parsed(
    relative_path: &str,
    parsed: &vulcan_core::ParsedDocument,
) -> vulcan_core::ResolverDocument {
    let filename = Path::new(relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(relative_path)
        .to_string();
    vulcan_core::ResolverDocument {
        id: relative_path.to_string(),
        path: relative_path.to_string(),
        filename,
        aliases: parsed.aliases.clone(),
    }
}

fn load_target_document<'a>(
    paths: &VaultPaths,
    current_path: &str,
    current_parsed: &vulcan_core::ParsedDocument,
    target_path: &str,
    cache: &'a mut HashMap<String, vulcan_core::ParsedDocument>,
) -> Result<&'a vulcan_core::ParsedDocument, CliError> {
    if target_path == current_path {
        cache
            .entry(target_path.to_string())
            .or_insert_with(|| current_parsed.clone());
    } else {
        let config = load_vault_config(paths).config;
        if !cache.contains_key(target_path) {
            let source = fs::read_to_string(paths.vault_root().join(target_path))
                .map_err(CliError::operation)?;
            cache.insert(
                target_path.to_string(),
                vulcan_core::parse_document(&source, &config),
            );
        }
    }
    cache
        .get(target_path)
        .ok_or_else(|| CliError::operation(format!("failed to load target note {target_path}")))
}

fn discover_markdown_note_paths(root: &Path) -> io::Result<Vec<String>> {
    fn walk(root: &Path, current: &Path, paths: &mut Vec<String>) -> io::Result<()> {
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            let file_name = entry.file_name();
            if file_name.to_string_lossy() == ".vulcan" {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, paths)?;
            } else if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            {
                let relative = path
                    .strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_string_lossy()
                    .replace('\\', "/");
                paths.push(relative);
            }
        }
        Ok(())
    }

    let mut paths = Vec::new();
    if root.is_dir() {
        walk(root, root, &mut paths)?;
    }
    paths.sort();
    Ok(paths)
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
        || summary.type_mismatches > 0
        || summary.unsupported_syntax > 0
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
    let args = args.into_iter().map(Into::into).collect::<Vec<OsString>>();
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => match error.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                error.print().map_err(CliError::operation)?;
                return Ok(());
            }
            _ => return Err(CliError::clap(&error)),
        },
    };
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
        Command::Index { ref command } => match command {
            IndexCommand::Init(ref args) => {
                let report = run_init_command(&paths, args)?;
                print_init_summary(cli.output, &paths, &report)?;
                Ok(())
            }
            IndexCommand::Scan { full, no_commit } => {
                let auto_commit = AutoCommitPolicy::for_scan(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let mut progress = (cli.output == OutputFormat::Human)
                    .then(|| ScanProgressReporter::new(use_stderr_color));
                let summary = scan_vault_with_progress(
                    &paths,
                    if *full {
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
            IndexCommand::Rebuild { dry_run } => {
                let mut progress = (cli.output == OutputFormat::Human)
                    .then(|| ScanProgressReporter::new(use_stderr_color));
                let report = rebuild_vault_with_progress(
                    &paths,
                    &RebuildQuery { dry_run: *dry_run },
                    |event| {
                        if let Some(progress) = progress.as_mut() {
                            progress.record(&event);
                        }
                    },
                )
                .map_err(CliError::operation)?;
                print_rebuild_report(cli.output, &report, use_stdout_color)
            }
            IndexCommand::Repair { command } => match command {
                RepairCommand::Fts { dry_run } => {
                    let report = repair_fts(&paths, &RepairFtsQuery { dry_run: *dry_run })
                        .map_err(CliError::operation)?;
                    print_repair_fts_report(cli.output, &report)
                }
            },
            IndexCommand::Watch {
                debounce_ms,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_scan(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                if cli.output == OutputFormat::Human && stdout_is_tty {
                    println!(
                        "Watching {} (debounce {}ms)",
                        paths.vault_root().display(),
                        debounce_ms
                    );
                }
                watch_vault(
                    &paths,
                    &WatchOptions {
                        debounce_ms: *debounce_ms,
                    },
                    |report| {
                        print_watch_report(cli.output, &report)?;
                        if !report.startup
                            && report.summary.added
                                + report.summary.updated
                                + report.summary.deleted
                                > 0
                        {
                            auto_commit
                                .commit(&paths, "scan", &report.paths)
                                .map_err(CliError::operation)?;
                        }
                        Ok::<(), CliError>(())
                    },
                )
                .map_err(CliError::operation)
            }
            IndexCommand::Serve {
                bind,
                no_watch,
                debounce_ms,
                auth_token,
            } => serve_forever(
                &paths,
                &ServeOptions {
                    bind: bind.clone(),
                    watch: !no_watch,
                    debounce_ms: *debounce_ms,
                    auth_token: auth_token.clone(),
                },
            ),
        },
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
        Command::Note { ref command } => match command {
            NoteCommand::Get {
                note,
                heading,
                block_ref,
                lines,
                match_pattern,
                context,
                no_frontmatter,
                raw,
            } => {
                let report = run_note_get_command(
                    &paths,
                    NoteGetOptions {
                        note,
                        heading: heading.as_deref(),
                        block_ref: block_ref.as_deref(),
                        lines: lines.as_deref(),
                        match_pattern: match_pattern.as_deref(),
                        context: *context,
                        no_frontmatter: *no_frontmatter,
                        raw: *raw,
                    },
                )?;
                print_note_get_report(cli.output, &report)
            }
            NoteCommand::Set {
                note,
                file,
                no_frontmatter,
                check,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_note_set_command(
                    &paths,
                    note,
                    file.as_ref(),
                    *no_frontmatter,
                    *check,
                    cli.output,
                    use_stderr_color,
                )?;
                auto_commit
                    .commit(&paths, "note-set", std::slice::from_ref(&report.path))
                    .map_err(CliError::operation)?;
                print_note_set_report(cli.output, &report)
            }
            NoteCommand::Create {
                path,
                template,
                frontmatter,
                check,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_note_create_command(
                    &paths,
                    path,
                    template.as_deref(),
                    frontmatter,
                    *check,
                    cli.output,
                    use_stderr_color,
                )?;
                auto_commit
                    .commit(&paths, "note-create", &report.changed_paths)
                    .map_err(CliError::operation)?;
                print_note_create_report(cli.output, &report)
            }
            NoteCommand::Append {
                note_or_text,
                text,
                heading,
                prepend,
                append: _,
                periodic,
                date,
                vars,
                check,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let (note, text) = match (*periodic, text.as_deref()) {
                    (Some(_), None) => (None, note_or_text.as_str()),
                    (None, Some(text)) => (Some(note_or_text.as_str()), text),
                    (Some(_), Some(_)) => {
                        return Err(CliError::operation(format!(
                            "`note append --periodic` accepts only the appended text; got unexpected note argument `{note_or_text}`"
                        )));
                    }
                    (None, None) => {
                        return Err(CliError::operation(format!(
                            "`note append` requires both NOTE and TEXT; got only `{note_or_text}`"
                        )));
                    }
                };
                let report = run_note_append_command(
                    &paths,
                    NoteAppendOptions {
                        note,
                        text,
                        mode: if *prepend {
                            NoteAppendMode::Prepend
                        } else if heading.is_some() {
                            NoteAppendMode::AfterHeading
                        } else {
                            NoteAppendMode::Append
                        },
                        heading: heading.as_deref(),
                        periodic: *periodic,
                        date: date.as_deref(),
                        vars,
                        check: *check,
                    },
                    cli.output,
                    use_stderr_color,
                )?;
                auto_commit
                    .commit(&paths, "note-append", std::slice::from_ref(&report.path))
                    .map_err(CliError::operation)?;
                print_note_append_report(cli.output, &report)
            }
            NoteCommand::Patch {
                note,
                find,
                replace,
                all,
                check,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_note_patch_command(
                    &paths,
                    NotePatchOptions {
                        note,
                        find,
                        replace,
                        replace_all: *all,
                        check: *check,
                        dry_run: *dry_run,
                    },
                    cli.output,
                    use_stderr_color,
                )?;
                if !*dry_run {
                    auto_commit
                        .commit(&paths, "note-patch", std::slice::from_ref(&report.path))
                        .map_err(CliError::operation)?;
                }
                print_note_patch_report(cli.output, &report)
            }
            NoteCommand::Links { note, export } => {
                let note = resolve_note_argument(
                    &paths,
                    note.as_deref(),
                    interactive_note_selection,
                    "note",
                )?;
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
            NoteCommand::Backlinks { note, export } => {
                let note = resolve_note_argument(
                    &paths,
                    note.as_deref(),
                    interactive_note_selection,
                    "note",
                )?;
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
            NoteCommand::Doctor { note } => {
                let report = run_note_doctor_command(&paths, note)?;
                print_note_doctor_report(cli.output, &report)
            }
            NoteCommand::Diff { note, since } => {
                let report = run_diff_command(&paths, Some(note), since.as_deref(), false)?;
                print_diff_report(cli.output, &report)
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
            BasesCommand::Create {
                ref file,
                ref title,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report =
                    create_note_from_bases_view(&paths, file, 0, title.as_deref(), *dry_run)?;
                if !*dry_run {
                    run_incremental_scan(&paths, cli.output, use_stderr_color)?;
                    auto_commit
                        .commit(&paths, "bases-create", std::slice::from_ref(&report.path))
                        .map_err(CliError::operation)?;
                }
                print_bases_create_report(cli.output, &report)
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
                if !*dry_run {
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
        Command::Help {
            ref search,
            ref topic,
        } => print_help_command(cli.output, topic, search.as_deref()),
        Command::Describe { format } => print_describe_report(cli.output, format),
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
        Command::Init(ref args) => {
            let report = run_init_command(&paths, args)?;
            print_init_summary(cli.output, &paths, &report)?;
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
            format,
            ref glob,
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
                &paths,
                cli.output,
                &report,
                &effective_controls,
                QueryReportRenderOptions {
                    format,
                    glob: glob.as_deref(),
                    explain,
                    stdout_is_tty,
                    use_color: use_stdout_color,
                    export: export.as_ref(),
                },
            )?;
            Ok(())
        }
        Command::Ls {
            ref filters,
            ref glob,
            ref tag,
            format,
            ref export,
        } => {
            let mut query_filters = filters.clone();
            if let Some(tag) = tag.as_deref() {
                query_filters.push(format!("file.tags has_tag {tag}"));
            }
            let note_query = NoteQuery {
                filters: query_filters,
                sort_by: Some("file.path".to_string()),
                sort_descending: false,
            };
            let notes_report = query_notes(&paths, &note_query).map_err(CliError::operation)?;
            let ast = QueryAst::from_note_query(&note_query).map_err(CliError::operation)?;
            let export = resolve_cli_export(export)?;
            print_query_report(
                &paths,
                cli.output,
                &QueryReport {
                    query: ast,
                    notes: notes_report.notes,
                },
                &list_controls,
                QueryReportRenderOptions {
                    format,
                    glob: glob.as_deref(),
                    explain: false,
                    stdout_is_tty,
                    use_color: use_stdout_color,
                    export: export.as_ref(),
                },
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
            DataviewCommand::QueryJs { js, file } => {
                let result = run_dataview_query_js_command(&paths, js, file.as_deref())?;
                print_dataview_js_result(
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
            TasksCommand::Add {
                text,
                no_nlp,
                status,
                priority,
                due,
                scheduled,
                contexts,
                projects,
                tags,
                template,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_tasks_add_command(
                    &paths,
                    text,
                    *no_nlp,
                    status.as_deref(),
                    priority.as_deref(),
                    due.as_deref(),
                    scheduled.as_deref(),
                    contexts,
                    projects,
                    tags,
                    template.as_deref(),
                    *dry_run,
                    cli.output,
                    use_stderr_color,
                )?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "tasks add", &report.changed_paths)
                        .map_err(CliError::operation)?;
                }
                print_task_add_report(cli.output, &report)
            }
            TasksCommand::Show { task } => {
                let report = run_tasks_show_command(&paths, task)?;
                print_task_show_report(cli.output, &report)
            }
            TasksCommand::Edit { task, no_commit } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_tasks_edit_command(&paths, task, cli.output, use_stderr_color)?;
                auto_commit
                    .commit(&paths, "tasks edit", std::slice::from_ref(&report.path))
                    .map_err(CliError::operation)?;
                print_edit_report(cli.output, &report);
                Ok(())
            }
            TasksCommand::Set {
                task,
                property,
                value,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_tasks_set_command(
                    &paths,
                    task,
                    property,
                    value,
                    *dry_run,
                    cli.output,
                    use_stderr_color,
                )?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "tasks set", &report.changed_paths)
                        .map_err(CliError::operation)?;
                }
                print_task_mutation_report(cli.output, &report)
            }
            TasksCommand::Complete {
                task,
                date,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_tasks_complete_command(
                    &paths,
                    task,
                    date.as_deref(),
                    *dry_run,
                    cli.output,
                    use_stderr_color,
                )?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "tasks complete", &report.changed_paths)
                        .map_err(CliError::operation)?;
                }
                print_task_mutation_report(cli.output, &report)
            }
            TasksCommand::Archive {
                task,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_tasks_archive_command(
                    &paths,
                    task,
                    *dry_run,
                    cli.output,
                    use_stderr_color,
                )?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "tasks archive", &report.changed_paths)
                        .map_err(CliError::operation)?;
                }
                print_task_mutation_report(cli.output, &report)
            }
            TasksCommand::Convert {
                file,
                line,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_tasks_convert_command(
                    &paths,
                    file,
                    *line,
                    *dry_run,
                    cli.output,
                    use_stderr_color,
                )?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "tasks convert", &report.changed_paths)
                        .map_err(CliError::operation)?;
                }
                print_task_convert_report(cli.output, &report)
            }
            TasksCommand::Create {
                text,
                note,
                due,
                priority,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_tasks_create_command(
                    &paths,
                    TasksCreateOptions {
                        text,
                        note: note.as_deref(),
                        due: due.as_deref(),
                        priority: priority.as_deref(),
                        dry_run: *dry_run,
                    },
                    cli.output,
                    use_stderr_color,
                )?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "tasks create", &report.changed_paths)
                        .map_err(CliError::operation)?;
                }
                print_task_create_report(cli.output, &report)
            }
            TasksCommand::Reschedule {
                task,
                due,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_tasks_reschedule_command(
                    &paths,
                    task,
                    due,
                    *dry_run,
                    cli.output,
                    use_stderr_color,
                )?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "tasks reschedule", &report.changed_paths)
                        .map_err(CliError::operation)?;
                }
                print_task_mutation_report(cli.output, &report)
            }
            TasksCommand::Query { query } => {
                let result = run_tasks_query_command(&paths, query)?;
                print_tasks_query_result(cli.output, &result)
            }
            TasksCommand::Eval { file, block } => {
                let report = run_tasks_eval_command(&paths, file, *block)?;
                print_tasks_eval_report(cli.output, &report)
            }
            TasksCommand::List {
                filter,
                source,
                status,
                priority,
                due_before,
                due_after,
                project,
                context,
                group_by,
                sort_by,
                include_archived,
            } => {
                let result = run_tasks_list_command(
                    &paths,
                    TasksListOptions {
                        filter: filter.as_deref(),
                        source: *source,
                        status: status.as_deref(),
                        priority: priority.as_deref(),
                        due_before: due_before.as_deref(),
                        due_after: due_after.as_deref(),
                        project: project.as_deref(),
                        context: context.as_deref(),
                        group_by: group_by.as_deref(),
                        sort_by: sort_by.as_deref(),
                        include_archived: *include_archived,
                    },
                )?;
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
            TasksCommand::View { command } => match command {
                TasksViewCommand::Show { name, export } => {
                    let report = run_tasks_view_command(&paths, name)?;
                    let export = resolve_cli_export(export)?;
                    print_bases_report(
                        cli.output,
                        &report,
                        &list_controls,
                        stdout_is_tty,
                        use_stdout_color,
                        export.as_ref(),
                    )
                }
                TasksViewCommand::List => {
                    let report = run_tasks_view_list_command(&paths)?;
                    print_tasknotes_view_list_report(cli.output, &report)
                }
            },
        },
        Command::Kanban { ref command } => match command {
            KanbanCommand::List => {
                let boards = list_kanban_boards(&paths).map_err(CliError::operation)?;
                print_kanban_board_list(
                    cli.output,
                    &boards,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                )
            }
            KanbanCommand::Show {
                board,
                verbose,
                include_archive,
            } => {
                let report = load_kanban_board(&paths, board, *include_archive)
                    .map_err(CliError::operation)?;
                print_kanban_board_report(cli.output, &report, *verbose)
            }
            KanbanCommand::Cards {
                board,
                column,
                status,
            } => {
                let report =
                    run_kanban_cards_command(&paths, board, column.as_deref(), status.as_deref())?;
                print_kanban_cards_report(
                    cli.output,
                    &report,
                    &list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                )
            }
            KanbanCommand::Archive {
                board,
                card,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_kanban_archive_command(&paths, board, card, *dry_run)?;
                if !dry_run {
                    auto_commit
                        .commit(
                            &paths,
                            "kanban-archive",
                            &kanban_archive_changed_files(&report),
                        )
                        .map_err(CliError::operation)?;
                }
                print_kanban_archive_report(cli.output, &report)
            }
            KanbanCommand::Move {
                board,
                card,
                target_column,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_kanban_move_command(&paths, board, card, target_column, *dry_run)?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "kanban-move", &kanban_move_changed_files(&report))
                        .map_err(CliError::operation)?;
                }
                print_kanban_move_report(cli.output, &report)
            }
            KanbanCommand::Add {
                board,
                column,
                text,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = run_kanban_add_command(&paths, board, column, text, *dry_run)?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "kanban-add", &kanban_add_changed_files(&report))
                        .map_err(CliError::operation)?;
                }
                print_kanban_add_report(cli.output, &report)
            }
        },
        Command::Search {
            ref query,
            ref regex,
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
            let effective_query = match (query.as_deref(), regex.as_deref()) {
                (Some(_), Some(_)) => {
                    return Err(CliError::operation(
                        "provide either a query string or --regex, not both",
                    ))
                }
                (Some(query), None) => query.to_string(),
                (None, Some(regex)) => format!("/{regex}/"),
                (None, None) => {
                    return Err(CliError::operation(
                        "provide a search query or --regex pattern",
                    ))
                }
            };
            let report = search_vault(
                &paths,
                &SearchQuery {
                    text: effective_query,
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
        Command::Refactor { ref command } => match command {
            RefactorCommand::RenameAlias {
                note,
                old,
                new,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report =
                    rename_alias(&paths, note, old, new, *dry_run).map_err(CliError::operation)?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "rename-alias", &refactor_changed_files(&report))
                        .map_err(CliError::operation)?;
                }
                print_refactor_report(cli.output, &report)
            }
            RefactorCommand::RenameHeading {
                note,
                old,
                new,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = rename_heading(&paths, note, old, new, *dry_run)
                    .map_err(CliError::operation)?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "rename-heading", &refactor_changed_files(&report))
                        .map_err(CliError::operation)?;
                }
                print_refactor_report(cli.output, &report)
            }
            RefactorCommand::RenameBlockRef {
                note,
                old,
                new,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = rename_block_ref(&paths, note, old, new, *dry_run)
                    .map_err(CliError::operation)?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "rename-block-ref", &refactor_changed_files(&report))
                        .map_err(CliError::operation)?;
                }
                print_refactor_report(cli.output, &report)
            }
            RefactorCommand::RenameProperty {
                old,
                new,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report =
                    rename_property(&paths, old, new, *dry_run).map_err(CliError::operation)?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "rename-property", &refactor_changed_files(&report))
                        .map_err(CliError::operation)?;
                }
                print_refactor_report(cli.output, &report)
            }
            RefactorCommand::MergeTags {
                source,
                dest,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report =
                    merge_tags(&paths, source, dest, *dry_run).map_err(CliError::operation)?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "merge-tags", &refactor_changed_files(&report))
                        .map_err(CliError::operation)?;
                }
                print_refactor_report(cli.output, &report)
            }
            RefactorCommand::Rewrite {
                filters,
                find,
                replace,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = bulk_replace(&paths, filters, find, replace, *dry_run)
                    .map_err(CliError::operation)?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "rewrite", &refactor_changed_files(&report))
                        .map_err(CliError::operation)?;
                }
                print_refactor_report(cli.output, &report)
            }
            RefactorCommand::Move {
                source,
                dest,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let summary =
                    move_note(&paths, source, dest, *dry_run).map_err(CliError::operation)?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "move", &move_changed_files(&summary))
                        .map_err(CliError::operation)?;
                }
                print_move_summary(cli.output, &summary)
            }
            RefactorCommand::LinkMentions {
                note,
                dry_run,
                no_commit,
            } => {
                let auto_commit = AutoCommitPolicy::for_mutation(&paths, *no_commit);
                warn_auto_commit_if_needed(&auto_commit);
                let report = link_mentions(&paths, note.as_deref(), *dry_run)
                    .map_err(CliError::operation)?;
                if !dry_run {
                    auto_commit
                        .commit(&paths, "link-mentions", &refactor_changed_files(&report))
                        .map_err(CliError::operation)?;
                }
                print_refactor_report(cli.output, &report)
            }
            RefactorCommand::Suggest { command } => match command {
                SuggestCommand::Mentions { note, export } => {
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
        },
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
        Command::Config { ref command } => match command {
            ConfigCommand::Import(selection) => {
                if selection.command.is_some() && (selection.all || selection.list) {
                    return Err(CliError::operation(
                        "config import accepts either a subcommand, --all, or --list",
                    ));
                }
                if selection.list {
                    let report = ConfigImportListReport {
                        importers: discover_config_importers(&paths)
                            .into_iter()
                            .map(|(_, discovery)| discovery)
                            .collect(),
                    };
                    return print_config_import_list_report(cli.output, &paths, &report);
                }
                if selection.all {
                    return run_config_import_batch(&paths, cli.output, &selection.args);
                }
                let Some(command) = selection.command.as_ref() else {
                    return Err(CliError::operation(
                        "config import requires a subcommand, --all, or --list",
                    ));
                };
                let importer = importer_for_command(command);
                run_config_import(&paths, cli.output, importer.as_ref(), &selection.args)
            }
        },
        Command::Daily { ref command } => match command {
            DailyCommand::Today { no_edit, no_commit } => {
                let report = run_periodic_open_command(
                    &paths,
                    "daily",
                    None,
                    *no_edit,
                    *no_commit,
                    interactive_note_selection,
                )?;
                print_periodic_open_report(cli.output, &report)
            }
            DailyCommand::Show { date } => {
                let report = run_daily_show_command(&paths, date.as_deref())?;
                print_daily_show_report(cli.output, &report)
            }
            DailyCommand::List {
                from,
                to,
                week,
                month,
            } => {
                let report =
                    run_daily_list_command(&paths, from.as_deref(), to.as_deref(), *week, *month)?;
                print_daily_list_report(cli.output, &report, &list_controls)
            }
            DailyCommand::ExportIcs {
                from,
                to,
                week,
                month,
                path,
                calendar_name,
            } => {
                let report = run_daily_export_ics_command(
                    &paths,
                    from.as_deref(),
                    to.as_deref(),
                    *week,
                    *month,
                    path.as_deref(),
                    calendar_name.as_deref(),
                )?;
                print_daily_export_ics_report(cli.output, &report)
            }
            DailyCommand::Append {
                text,
                heading,
                date,
                no_commit,
            } => {
                let report = run_daily_append_command(
                    &paths,
                    text,
                    heading.as_deref(),
                    date.as_deref(),
                    *no_commit,
                )?;
                print_daily_append_report(cli.output, &report)
            }
        },
        Command::Git { ref command } => match command {
            GitCommand::Status => {
                let mut report = git_status(paths.vault_root()).map_err(CliError::operation)?;
                report.staged = filter_vault_git_paths(report.staged);
                report.unstaged = filter_vault_git_paths(report.unstaged);
                report.untracked = filter_vault_git_paths(report.untracked);
                report.clean = report.staged.is_empty()
                    && report.unstaged.is_empty()
                    && report.untracked.is_empty();
                print_git_status_report(cli.output, &report)
            }
            GitCommand::Log { limit } => {
                let report = run_git_log_command(&paths, *limit)?;
                print_git_log_report(cli.output, &report)
            }
            GitCommand::Diff { path } => {
                let report = run_git_diff_group_command(&paths, path.as_deref())?;
                print_git_diff_group_report(cli.output, &report)
            }
            GitCommand::Commit { message } => {
                let report =
                    git_commit(paths.vault_root(), message).map_err(CliError::operation)?;
                print_git_commit_report(cli.output, &report)
            }
            GitCommand::Blame { path } => {
                let report = run_git_blame_command(&paths, path)?;
                print_git_blame_report(cli.output, &report)
            }
        },
        Command::Run {
            ref script,
            script_mode,
        } => {
            if script.is_none() && io::stdin().is_terminal() {
                run_js_repl(&paths, cli.output)
            } else {
                let result = run_js_command(&paths, script.as_deref(), script_mode)?;
                print_dataview_js_result(cli.output, &result, false)
            }
        }
        Command::Web { ref command } => match command {
            WebCommand::Search {
                query,
                backend,
                limit,
            } => {
                let report = run_web_search_command(&paths, query, backend.as_deref(), *limit)?;
                print_web_search_report(cli.output, &report)
            }
            WebCommand::Fetch {
                url,
                mode,
                save,
                extract_article,
            } => {
                let report =
                    run_web_fetch_command(&paths, url, *mode, save.as_ref(), *extract_article)?;
                print_web_fetch_report(cli.output, &report)
            }
        },
        Command::Weekly { ref args } => {
            let report = run_periodic_open_command(
                &paths,
                "weekly",
                args.date.as_deref(),
                args.no_edit,
                args.no_commit,
                interactive_note_selection,
            )?;
            print_periodic_open_report(cli.output, &report)
        }
        Command::Monthly { ref args } => {
            let report = run_periodic_open_command(
                &paths,
                "monthly",
                args.date.as_deref(),
                args.no_edit,
                args.no_commit,
                interactive_note_selection,
            )?;
            print_periodic_open_report(cli.output, &report)
        }
        Command::Periodic {
            ref command,
            ref period_type,
            ref date,
            no_edit,
            no_commit,
        } => match command {
            Some(PeriodicSubcommand::List { period_type }) => {
                let report = run_periodic_list_command(&paths, period_type.as_deref())?;
                print_periodic_list_report(cli.output, &report, &list_controls)
            }
            Some(PeriodicSubcommand::Gaps {
                period_type,
                from,
                to,
            }) => {
                let report = run_periodic_gaps_command(
                    &paths,
                    period_type.as_deref(),
                    from.as_deref(),
                    to.as_deref(),
                )?;
                print_periodic_gap_report(cli.output, &report, &list_controls)
            }
            None => {
                let period_type = period_type.as_deref().ok_or_else(|| {
                    CliError::operation(
                        "`periodic` requires a period type unless `list` or `gaps` is used",
                    )
                })?;
                let report = run_periodic_open_command(
                    &paths,
                    period_type,
                    date.as_deref(),
                    no_edit,
                    no_commit,
                    interactive_note_selection,
                )?;
                print_periodic_open_report(cli.output, &report)
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
            ref render,
            no_commit,
        } => {
            let result = match command {
                Some(TemplateSubcommand::Insert {
                    template,
                    note,
                    prepend,
                    append: _,
                    render,
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
                    render.engine,
                    &render.vars,
                    *no_commit,
                    interactive_note_selection,
                )?),
                Some(TemplateSubcommand::Preview {
                    template,
                    path,
                    render,
                }) => TemplateCommandResult::Preview(run_template_preview_command(
                    &paths,
                    template,
                    path.as_deref(),
                    render.engine,
                    &render.vars,
                )?),
                None => run_template_command(
                    &paths,
                    name.as_deref(),
                    list,
                    path.as_deref(),
                    render.engine,
                    &render.vars,
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
                TemplateCommandResult::Preview(report) => {
                    print_template_preview_report(cli.output, &report)
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
                    code: "batch_failed",
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

fn print_describe_report(output: OutputFormat, format: DescribeFormatArg) -> Result<(), CliError> {
    match format {
        DescribeFormatArg::JsonSchema => {
            let report = describe_cli();
            match output {
                OutputFormat::Human => {
                    print_describe_human(&report);
                    Ok(())
                }
                OutputFormat::Json => print_json(&report),
            }
        }
        DescribeFormatArg::OpenaiTools => {
            let tools = build_openai_tool_definitions();
            match output {
                OutputFormat::Human => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&tools).map_err(CliError::operation)?
                    );
                    Ok(())
                }
                OutputFormat::Json => print_json(&tools),
            }
        }
        DescribeFormatArg::Mcp => {
            let tools = build_mcp_tool_definitions();
            match output {
                OutputFormat::Human => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&tools).map_err(CliError::operation)?
                    );
                    Ok(())
                }
                OutputFormat::Json => print_json(&tools),
            }
        }
    }
}

fn print_help_command(
    output: OutputFormat,
    topic: &[String],
    search: Option<&str>,
) -> Result<(), CliError> {
    if let Some(keyword) = search {
        let report = search_help_topics(keyword);
        return match output {
            OutputFormat::Human => {
                if report.matches.is_empty() {
                    println!("No help topics matched `{keyword}`.");
                } else {
                    println!("Help topics matching `{keyword}`:");
                    for item in &report.matches {
                        println!("- {} [{}]: {}", item.name, item.kind, item.summary);
                    }
                }
                Ok(())
            }
            OutputFormat::Json => print_json(&report),
        };
    }

    let report = if topic.is_empty() {
        help_overview()
    } else {
        resolve_help_topic(topic)?
    };

    match output {
        OutputFormat::Human => {
            println!("# {}", report.name);
            println!();
            println!("{}", report.summary);
            if !report.body.is_empty() {
                println!();
                println!("{}", report.body);
            }
            if !report.subcommands.is_empty() {
                println!();
                println!("Subcommands:");
                for subcommand in &report.subcommands {
                    println!("- {subcommand}");
                }
            }
            if !report.options.is_empty() {
                println!();
                println!("Options:");
                for option in &report.options {
                    let flag = option
                        .long
                        .as_deref()
                        .map_or_else(|| option.id.clone(), |long| format!("--{long}"));
                    let summary = option.help.as_deref().unwrap_or("undocumented");
                    println!("- {flag}: {summary}");
                }
            }
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
        OutputFormat::Json => print_json(&report),
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

fn query_report_rows(report: &QueryReport, notes: &[&NoteRecord]) -> Vec<Value> {
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

fn query_path_rows(notes: &[&NoteRecord]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| Value::String(note.document_path.clone()))
        .collect()
}

fn query_detail_rows(
    paths: &VaultPaths,
    report: &QueryReport,
    notes: &[&NoteRecord],
) -> Vec<Value> {
    let query_value = serde_json::to_value(&report.query).unwrap_or(Value::Null);
    notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "document_path": note.document_path,
                "properties": note.properties,
                "preview_lines": load_note_preview_lines(paths, note.document_path.as_str(), 5),
                "query": query_value,
            })
        })
        .collect()
}

fn load_note_preview_lines(paths: &VaultPaths, document_path: &str, limit: usize) -> Vec<String> {
    fs::read_to_string(paths.vault_root().join(document_path))
        .ok()
        .map(|content| {
            content
                .lines()
                .map(str::trim_end)
                .filter(|line| !line.trim().is_empty())
                .take(limit)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn print_query_detail_human(paths: &VaultPaths, notes: &[&NoteRecord]) {
    for note in notes {
        println!("- {}", note.document_path);
        if let Some(properties) = note.properties.as_object() {
            if !properties.is_empty() {
                let summary = properties
                    .iter()
                    .take(6)
                    .map(|(key, value)| format!("{key}={}", render_human_value(value)))
                    .collect::<Vec<_>>();
                if !summary.is_empty() {
                    println!("  properties: {}", summary.join(" | "));
                }
            }
        }
        for line in load_note_preview_lines(paths, note.document_path.as_str(), 5) {
            println!("  {line}");
        }
        println!();
    }
}

fn glob_pattern_regex(pattern: &str) -> Result<Regex, CliError> {
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' => {
                if chars.peek().is_some_and(|next| *next == '*') {
                    chars.next();
                    regex.push_str(".*");
                } else {
                    regex.push_str("[^/]*");
                }
            }
            '?' => regex.push_str("[^/]"),
            other => regex.push_str(&regex::escape(&other.to_string())),
        }
    }
    regex.push('$');
    Regex::new(&regex)
        .map_err(|error| CliError::operation(format!("invalid glob pattern: {error}")))
}

fn filter_notes_by_glob<'a>(
    notes: &'a [NoteRecord],
    glob: Option<&str>,
) -> Result<Vec<&'a NoteRecord>, CliError> {
    let Some(glob) = glob else {
        return Ok(notes.iter().collect());
    };
    let matcher = glob_pattern_regex(glob)?;
    Ok(notes
        .iter()
        .filter(|note| matcher.is_match(&note.document_path))
        .collect())
}

#[derive(Clone, Copy)]
struct QueryReportRenderOptions<'a> {
    format: QueryFormatArg,
    glob: Option<&'a str>,
    explain: bool,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&'a ResolvedExport>,
}

fn print_query_report(
    paths: &VaultPaths,
    output: OutputFormat,
    report: &QueryReport,
    list_controls: &ListOutputControls,
    options: QueryReportRenderOptions<'_>,
) -> Result<(), CliError> {
    let filtered_notes = filter_notes_by_glob(&report.notes, options.glob)?;
    let start = list_controls.offset.min(filtered_notes.len());
    let end = list_controls.limit.map_or(filtered_notes.len(), |limit| {
        start.saturating_add(limit).min(filtered_notes.len())
    });
    let visible_notes = &filtered_notes[start..end];
    let palette = AnsiPalette::new(options.use_color);

    match output {
        OutputFormat::Human => {
            if options.explain
                || (options.stdout_is_tty && matches!(options.format, QueryFormatArg::Table))
            {
                let ast_json = serde_json::to_string_pretty(&report.query)
                    .unwrap_or_else(|_| "{}".to_string());
                println!("{}", palette.cyan("Query AST:"));
                println!("{ast_json}");
                println!();
            }
            match options.format {
                QueryFormatArg::Count => {
                    println!("{}", visible_notes.len());
                    return Ok(());
                }
                QueryFormatArg::Paths => {
                    let rows = query_path_rows(visible_notes);
                    for note in visible_notes {
                        println!("{}", note.document_path);
                    }
                    export_rows(&rows, list_controls.fields.as_deref(), options.export)?;
                    return Ok(());
                }
                QueryFormatArg::Detail => {
                    if visible_notes.is_empty() {
                        println!("No notes matched.");
                        return Ok(());
                    }
                    let rows = query_detail_rows(paths, report, visible_notes);
                    print_query_detail_human(paths, visible_notes);
                    export_rows(&rows, list_controls.fields.as_deref(), options.export)?;
                    return Ok(());
                }
                QueryFormatArg::Table => {}
            }
            let rows = query_report_rows(report, visible_notes);
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
            export_rows(&rows, list_controls.fields.as_deref(), options.export)?;
            Ok(())
        }
        OutputFormat::Json => {
            if matches!(options.format, QueryFormatArg::Count) {
                let payload = serde_json::json!({ "count": visible_notes.len() });
                export_rows(
                    std::slice::from_ref(&payload),
                    list_controls.fields.as_deref(),
                    options.export,
                )?;
                return print_json(&payload);
            }
            let rows = match options.format {
                QueryFormatArg::Table => query_report_rows(report, visible_notes),
                QueryFormatArg::Paths => query_path_rows(visible_notes),
                QueryFormatArg::Detail => query_detail_rows(paths, report, visible_notes),
                QueryFormatArg::Count => unreachable!("count handled above"),
            };
            if options.explain {
                let payload = serde_json::json!({
                    "query": report.query,
                    "notes": rows,
                });
                export_rows(
                    std::slice::from_ref(&payload),
                    list_controls.fields.as_deref(),
                    options.export,
                )?;
                print_json(&payload)
            } else {
                export_rows(&rows, list_controls.fields.as_deref(), options.export)?;
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
        OutputFormat::Json => print_json(&report),
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
        OutputFormat::Json => print_json(&report),
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

fn print_bases_create_report(
    output: OutputFormat,
    report: &BasesCreateReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.dry_run {
                println!("Would create {} from {}.", report.path, report.file);
            } else {
                println!("Created {} from {}.", report.path, report.file);
            }

            let view = report
                .view_name
                .as_deref()
                .map_or_else(|| format!("#{}", report.view_index + 1), ToOwned::to_owned);
            println!("View: {view}");
            println!(
                "Folder: {}",
                report.folder.as_deref().unwrap_or("<vault root>")
            );
            println!(
                "Template: {}",
                report.template.as_deref().unwrap_or("<none>")
            );

            if report.properties.is_empty() {
                println!("Properties: <none>");
            } else {
                println!("Properties:");
                for (key, value) in &report.properties {
                    println!("  {key}: {}", render_human_value(value));
                }
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

fn run_init_command(paths: &VaultPaths, args: &InitArgs) -> Result<InitReport, CliError> {
    let summary = initialize_vault(paths).map_err(CliError::operation)?;
    let support_files = if args.agent_files {
        write_bundled_support_files(paths)?
    } else {
        Vec::new()
    };
    let importable_sources = if args.no_import {
        Vec::new()
    } else {
        discover_config_importers(paths)
            .into_iter()
            .filter_map(|(_, discovery)| discovery.detected.then_some(discovery))
            .collect()
    };
    let imported = if args.import {
        let target = ImportTarget::Shared;
        let mut reports = Vec::new();
        for importer in all_importers()
            .into_iter()
            .filter(|importer| importer.detect(paths))
        {
            reports.push(
                importer
                    .import(paths, target)
                    .map_err(CliError::operation)?,
            );
        }
        annotate_import_conflicts(&mut reports);
        Some(ConfigImportBatchReport {
            dry_run: false,
            target,
            detected_count: reports.len(),
            imported_count: reports.len(),
            updated_count: reports.iter().filter(|report| report.updated).count(),
            reports,
        })
    } else {
        None
    };

    Ok(InitReport {
        summary,
        importable_sources,
        support_files,
        imported,
    })
}

fn print_init_summary(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &InitReport,
) -> Result<(), CliError> {
    let normalized_importable = report
        .importable_sources
        .iter()
        .map(|item| normalize_import_discovery_item(paths, item))
        .collect::<Vec<_>>();
    let normalized_imported = report
        .imported
        .as_ref()
        .map(|batch| ConfigImportBatchReport {
            dry_run: batch.dry_run,
            target: batch.target,
            detected_count: batch.detected_count,
            imported_count: batch.imported_count,
            updated_count: batch.updated_count,
            reports: batch
                .reports
                .iter()
                .map(|item| normalize_config_import_report(paths, item))
                .collect(),
        });
    let normalized = InitReport {
        summary: report.summary.clone(),
        importable_sources: normalized_importable,
        support_files: report.support_files.clone(),
        imported: normalized_imported,
    };

    match output {
        OutputFormat::Human => {
            println!(
                "Initialized {} (config {}, cache {})",
                normalized.summary.vault_root.display(),
                if normalized.summary.created_config {
                    "created"
                } else {
                    "existing"
                },
                if normalized.summary.created_cache {
                    "created"
                } else {
                    "existing"
                },
            );
            if let Some(imported) = &normalized.imported {
                println!(
                    "Imported {} detected importer{} ({} updated).",
                    imported.imported_count,
                    if imported.imported_count == 1 {
                        ""
                    } else {
                        "s"
                    },
                    imported.updated_count
                );
            } else if !normalized.importable_sources.is_empty() {
                println!("Importable settings detected:");
                for importer in &normalized.importable_sources {
                    println!("- {} ({})", importer.plugin, importer.display_name);
                }
                println!("Run `vulcan config import --all` to import them.");
            }
            if !normalized.support_files.is_empty() {
                println!("Bundled agent support files:");
                for file in &normalized.support_files {
                    let status = if file.created { "created" } else { "kept" };
                    println!("- {} [{}; {}]", file.path, file.kind, status);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(&normalized),
    }
}

fn write_bundled_support_files(paths: &VaultPaths) -> Result<Vec<InitSupportFile>, CliError> {
    let mut reports = Vec::new();
    reports.push(write_bundled_text_file(paths, &BUNDLED_AGENT_TEMPLATE)?);
    for file in BUNDLED_SKILL_FILES {
        reports.push(write_bundled_text_file(paths, file)?);
    }
    Ok(reports)
}

fn write_bundled_text_file(
    paths: &VaultPaths,
    file: &BundledTextFile,
) -> Result<InitSupportFile, CliError> {
    let destination = paths.vault_root().join(file.relative_path);
    let created = write_text_file_if_missing(&destination, file.contents)?;
    Ok(InitSupportFile {
        path: file.relative_path.to_string(),
        kind: file.kind.to_string(),
        created,
    })
}

fn write_text_file_if_missing(path: &Path, contents: &str) -> Result<bool, CliError> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    let rendered = if contents.ends_with('\n') {
        contents.to_string()
    } else {
        format!("{contents}\n")
    };
    fs::write(path, rendered).map_err(CliError::operation)?;
    Ok(true)
}

fn print_note_get_report(output: OutputFormat, report: &NoteGetReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.metadata.raw
                || (report.metadata.lines.is_none() && report.metadata.match_pattern.is_none())
            {
                print!("{}", report.content);
            } else {
                let mut previous_line = None;
                for line in &report.display_lines {
                    if previous_line.is_some_and(|line_number| line.line_number != line_number + 1)
                    {
                        println!("--");
                    }
                    println!("{}: {}", line.line_number, line.text);
                    previous_line = Some(line.line_number);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_note_set_report(output: OutputFormat, report: &NoteSetReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!(
                "Updated {}{}.",
                report.path,
                if report.preserved_frontmatter {
                    " (preserved frontmatter)"
                } else {
                    ""
                }
            );
            print_note_check_warnings(&report.path, &report.diagnostics);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_note_create_report(
    output: OutputFormat,
    report: &NoteCreateReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("Created {}.", report.path);
            if let Some(template) = report.template.as_deref() {
                let engine = report.engine.as_deref().unwrap_or("auto");
                println!("Template: {template} ({engine})");
            }
            for warning in &report.warnings {
                eprintln!("warning: {warning}");
            }
            print_note_check_warnings(&report.path, &report.diagnostics);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_note_append_report(
    output: OutputFormat,
    report: &NoteAppendReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            let target = report.period_type.as_deref().map_or_else(
                || report.path.clone(),
                |period_type| {
                    if let Some(reference_date) = report.reference_date.as_deref() {
                        format!("{} ({period_type} {reference_date})", report.path)
                    } else {
                        format!("{} ({period_type})", report.path)
                    }
                },
            );
            match report.mode.as_str() {
                "after_heading" => println!(
                    "Appended to {} under {}.",
                    target,
                    report.heading.as_deref().unwrap_or_default()
                ),
                "prepend" => println!("Prepended to {target}."),
                _ => println!("Appended to {target}."),
            }
            if report.created {
                println!("Created missing note first.");
            }
            for warning in &report.warnings {
                eprintln!("warning: {warning}");
            }
            print_note_check_warnings(&report.path, &report.diagnostics);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_note_patch_report(output: OutputFormat, report: &NotePatchReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.dry_run {
                println!(
                    "Dry run: would patch {} ({} match{}).",
                    report.path,
                    report.match_count,
                    if report.match_count == 1 { "" } else { "es" }
                );
            } else {
                println!(
                    "Patched {} ({} match{}).",
                    report.path,
                    report.match_count,
                    if report.match_count == 1 { "" } else { "es" }
                );
            }
            for change in &report.changes {
                println!("- {} -> {}", change.before, change.after);
            }
            print_note_check_warnings(&report.path, &report.diagnostics);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_note_check_warnings(path: &str, diagnostics: &[DoctorDiagnosticIssue]) {
    for diagnostic in diagnostics {
        eprintln!(
            "warning: {}: {}",
            diagnostic.document_path.as_deref().unwrap_or(path),
            diagnostic.message
        );
    }
}

fn config_import_target(target: ConfigImportTargetArg) -> ImportTarget {
    match target {
        ConfigImportTargetArg::Shared => ImportTarget::Shared,
        ConfigImportTargetArg::Local => ImportTarget::Local,
    }
}

fn importer_for_command(command: &ConfigImportCommand) -> Box<dyn PluginImporter> {
    match command {
        ConfigImportCommand::Core => Box::new(CoreImporter),
        ConfigImportCommand::Dataview => Box::new(DataviewImporter),
        ConfigImportCommand::Templater => Box::new(TemplaterImporter),
        ConfigImportCommand::Quickadd => Box::new(QuickAddImporter),
        ConfigImportCommand::Kanban => Box::new(KanbanImporter),
        ConfigImportCommand::PeriodicNotes => Box::new(PeriodicNotesImporter),
        ConfigImportCommand::TaskNotes => Box::new(TaskNotesImporter),
        ConfigImportCommand::Tasks => Box::new(TasksImporter),
    }
}

fn discover_config_importers(
    paths: &VaultPaths,
) -> Vec<(Box<dyn PluginImporter>, ConfigImportDiscoveryItem)> {
    all_importers()
        .into_iter()
        .map(|importer| {
            let source_paths = importer.source_paths(paths);
            let detected = importer.detect(paths);
            let discovery = ConfigImportDiscoveryItem {
                plugin: importer.name().to_string(),
                display_name: importer.display_name().to_string(),
                detected,
                source_paths,
            };
            (importer, discovery)
        })
        .collect()
}

fn normalize_import_discovery_item(
    paths: &VaultPaths,
    item: &ConfigImportDiscoveryItem,
) -> ConfigImportDiscoveryItem {
    ConfigImportDiscoveryItem {
        plugin: item.plugin.clone(),
        display_name: item.display_name.clone(),
        detected: item.detected,
        source_paths: item
            .source_paths
            .iter()
            .map(|path| relativize_config_import_path(paths, path))
            .collect(),
    }
}

fn run_config_import(
    paths: &VaultPaths,
    output: OutputFormat,
    importer: &dyn PluginImporter,
    args: &ConfigImportArgs,
) -> Result<(), CliError> {
    let target = config_import_target(args.target);
    let report = if args.dry_run {
        importer
            .dry_run_to(paths, target)
            .map_err(CliError::operation)?
    } else {
        let auto_commit = AutoCommitPolicy::for_mutation(paths, args.no_commit);
        warn_auto_commit_if_needed(&auto_commit);
        let had_gitignore = paths.gitignore_file().exists();
        let report = importer
            .import(paths, target)
            .map_err(CliError::operation)?;
        if report.updated {
            auto_commit
                .commit(
                    paths,
                    &format!("config-import-{}", importer.name()),
                    &config_import_changed_files(paths, had_gitignore, &report),
                )
                .map_err(CliError::operation)?;
        }
        report
    };

    print_config_import_report(output, paths, &report)
}

fn run_config_import_batch(
    paths: &VaultPaths,
    output: OutputFormat,
    args: &ConfigImportArgs,
) -> Result<(), CliError> {
    let target = config_import_target(args.target);
    let discovered = discover_config_importers(paths);
    let importers = discovered
        .into_iter()
        .filter_map(|(importer, discovery)| discovery.detected.then_some(importer))
        .collect::<Vec<_>>();
    let detected_count = importers.len();
    let mut reports = Vec::new();

    if args.dry_run {
        for importer in importers {
            reports.push(
                importer
                    .dry_run_to(paths, target)
                    .map_err(CliError::operation)?,
            );
        }
    } else {
        let auto_commit = AutoCommitPolicy::for_mutation(paths, args.no_commit);
        warn_auto_commit_if_needed(&auto_commit);
        let had_gitignore = paths.gitignore_file().exists();
        for importer in importers {
            reports.push(
                importer
                    .import(paths, target)
                    .map_err(CliError::operation)?,
            );
        }

        if reports.iter().any(|report| report.updated) {
            let changed_files = config_import_batch_changed_files(paths, had_gitignore, &reports);
            auto_commit
                .commit(paths, "config-import-all", &changed_files)
                .map_err(CliError::operation)?;
        }
    }

    annotate_import_conflicts(&mut reports);
    let updated_count = reports.iter().filter(|report| report.updated).count();
    let report = ConfigImportBatchReport {
        dry_run: args.dry_run,
        target,
        detected_count,
        imported_count: reports.len(),
        updated_count,
        reports,
    };
    print_config_import_batch_report(output, paths, &report)
}

fn print_config_import_list_report(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &ConfigImportListReport,
) -> Result<(), CliError> {
    let normalized = ConfigImportListReport {
        importers: report
            .importers
            .iter()
            .map(|item| normalize_import_discovery_item(paths, item))
            .collect(),
    };
    match output {
        OutputFormat::Human => {
            if normalized.importers.is_empty() {
                println!("No importers are registered.");
                return Ok(());
            }

            for item in &normalized.importers {
                let status = if item.detected { "detected" } else { "missing" };
                println!("- {} [{}]", item.plugin, status);
                println!("  {}", item.display_name);
                for source_path in &item.source_paths {
                    println!("  source: {}", source_path.display());
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(&normalized),
    }
}

fn print_config_import_batch_report(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &ConfigImportBatchReport,
) -> Result<(), CliError> {
    let normalized = ConfigImportBatchReport {
        dry_run: report.dry_run,
        target: report.target,
        detected_count: report.detected_count,
        imported_count: report.imported_count,
        updated_count: report.updated_count,
        reports: report
            .reports
            .iter()
            .map(|item| normalize_config_import_report(paths, item))
            .collect(),
    };
    match output {
        OutputFormat::Human => {
            println!(
                "{} {} detected importer{} into {} ({} updated)",
                if normalized.dry_run {
                    "Dry run:"
                } else {
                    "Imported"
                },
                normalized.imported_count,
                if normalized.imported_count == 1 {
                    ""
                } else {
                    "s"
                },
                match normalized.target {
                    ImportTarget::Shared => ".vulcan/config.toml",
                    ImportTarget::Local => ".vulcan/config.local.toml",
                },
                normalized.updated_count
            );
            if normalized.imported_count == 0 {
                println!("  no compatible sources were detected");
            }
            for item in &normalized.reports {
                println!(
                    "  - {}: {}",
                    item.plugin,
                    if item.updated {
                        if item.dry_run {
                            "would update"
                        } else {
                            "updated"
                        }
                    } else {
                        "unchanged"
                    }
                );
                for conflict in &item.conflicts {
                    println!(
                        "    warning: conflict on {} from {}",
                        conflict.key,
                        conflict.sources.join(", ")
                    );
                }
                for file in &item.migrated_files {
                    println!(
                        "    view: {} -> {} ({})",
                        file.source.display(),
                        file.target.display(),
                        render_config_import_migrated_file_action(report.dry_run, file.action)
                    );
                }
                for skipped in &item.skipped {
                    println!("    skipped: {} ({})", skipped.source, skipped.reason);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(&normalized),
    }
}

fn config_import_batch_changed_files(
    paths: &VaultPaths,
    had_gitignore: bool,
    reports: &[ConfigImportReport],
) -> Vec<String> {
    let mut changed = reports
        .iter()
        .filter(|report| report.updated)
        .flat_map(|report| config_import_changed_files(paths, had_gitignore, report))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    changed.sort();
    changed
}

fn print_config_import_report(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &ConfigImportReport,
) -> Result<(), CliError> {
    let report = normalize_config_import_report(paths, report);
    match output {
        OutputFormat::Human => {
            println!(
                "{} {} settings from {} into {} ({}, {})",
                if report.dry_run {
                    "Dry run:"
                } else {
                    "Imported"
                },
                report.plugin,
                if report.source_paths.is_empty() {
                    report.source_path.display().to_string()
                } else if report.source_paths.len() == 1 {
                    report.source_paths[0].display().to_string()
                } else {
                    format!("{} source files", report.source_paths.len())
                },
                report.target_file.display(),
                if report.created_config {
                    if report.dry_run {
                        "would create config"
                    } else {
                        "created config"
                    }
                } else {
                    "existing config"
                },
                if report.updated {
                    if report.dry_run {
                        "would update"
                    } else {
                        "updated"
                    }
                } else {
                    "unchanged"
                }
            );
            if report.source_paths.len() > 1 {
                println!("  sources:");
                for source_path in &report.source_paths {
                    println!("    {}", source_path.display());
                }
            }
            for mapping in &report.mappings {
                println!(
                    "  {} -> {} = {}",
                    mapping.source,
                    mapping.target,
                    render_config_import_value(&mapping.value)?
                );
            }
            for conflict in &report.conflicts {
                println!(
                    "  warning: conflict on {} from {} (kept {})",
                    conflict.key,
                    conflict.sources.join(", "),
                    render_config_import_value(&conflict.kept_value)?
                );
            }
            for file in &report.migrated_files {
                println!(
                    "  view: {} -> {} ({})",
                    file.source.display(),
                    file.target.display(),
                    render_config_import_migrated_file_action(report.dry_run, file.action)
                );
            }
            for skipped in &report.skipped {
                println!("  skipped: {} ({})", skipped.source, skipped.reason);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(&report),
    }
}

fn normalize_config_import_report(
    paths: &VaultPaths,
    report: &ConfigImportReport,
) -> ConfigImportReport {
    let mut report = report.clone();
    report.source_path = relativize_config_import_path(paths, &report.source_path);
    report.source_paths = report
        .source_paths
        .iter()
        .map(|path| relativize_config_import_path(paths, path))
        .collect();
    report.config_path = relativize_config_import_path(paths, &report.config_path);
    report.target_file = relativize_config_import_path(paths, &report.target_file);
    report.migrated_files = report
        .migrated_files
        .iter()
        .map(|file| vulcan_core::ImportMigratedFile {
            source: relativize_config_import_path(paths, &file.source),
            target: relativize_config_import_path(paths, &file.target),
            action: file.action,
        })
        .collect();
    report
}

fn relativize_config_import_path(paths: &VaultPaths, path: &Path) -> PathBuf {
    path.strip_prefix(paths.vault_root())
        .map_or_else(|_| path.to_path_buf(), Path::to_path_buf)
}

fn render_config_import_value(value: &Value) -> Result<String, CliError> {
    match value {
        Value::Null => Ok("<unset>".to_string()),
        Value::String(text) => Ok(format!("{text:?}")),
        Value::Bool(value_bool) => Ok(value_bool.to_string()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(value).map_err(CliError::operation)
        }
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

fn config_import_changed_files(
    paths: &VaultPaths,
    had_gitignore: bool,
    report: &ConfigImportReport,
) -> Vec<String> {
    let mut changed = Vec::new();
    if report.config_updated {
        changed.push(
            report
                .target_file
                .strip_prefix(paths.vault_root())
                .map_or_else(
                    |_| report.target_file.display().to_string(),
                    |path| path.display().to_string(),
                ),
        );
    }
    changed.extend(
        report
            .migrated_files
            .iter()
            .filter(|file| matches!(file.action, vulcan_core::ImportMigratedFileAction::Copy))
            .map(|file| {
                file.target.strip_prefix(paths.vault_root()).map_or_else(
                    |_| file.target.display().to_string(),
                    |path| path.display().to_string(),
                )
            }),
    );
    if report.config_updated && !had_gitignore && paths.gitignore_file().exists() {
        changed.push(".vulcan/.gitignore".to_string());
    }
    changed.sort();
    changed.dedup();
    changed
}

fn render_config_import_migrated_file_action(
    dry_run: bool,
    action: vulcan_core::ImportMigratedFileAction,
) -> &'static str {
    match (dry_run, action) {
        (true, vulcan_core::ImportMigratedFileAction::Copy) => "would copy and validate",
        (false, vulcan_core::ImportMigratedFileAction::Copy) => "copied and validated",
        (true, vulcan_core::ImportMigratedFileAction::ValidateOnly) => "would validate",
        (false, vulcan_core::ImportMigratedFileAction::ValidateOnly) => "validated",
    }
}

fn kanban_archive_changed_files(report: &KanbanArchiveReport) -> Vec<String> {
    vec![report.path.clone()]
}

fn kanban_move_changed_files(report: &KanbanMoveReport) -> Vec<String> {
    vec![report.path.clone()]
}

fn kanban_add_changed_files(report: &KanbanAddReport) -> Vec<String> {
    vec![report.path.clone()]
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
            println!("- type mismatches: {}", report.summary.type_mismatches);
            println!(
                "- unsupported syntax: {}",
                report.summary.unsupported_syntax
            );
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
            print_diagnostic_section("Type mismatches", &report.type_mismatches);
            print_diagnostic_section("Unsupported syntax", &report.unsupported_syntax);
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

fn print_note_doctor_report(
    output: OutputFormat,
    report: &NoteDoctorReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("Doctor summary for {}", report.path);
            if report.diagnostics.is_empty() {
                println!("No issues found.");
            } else {
                print_diagnostic_section("Diagnostics", &report.diagnostics);
            }
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

fn print_git_status_report(
    output: OutputFormat,
    report: &vulcan_core::GitStatusReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.clean {
                println!("Working tree clean.");
                return Ok(());
            }
            if !report.staged.is_empty() {
                println!("Staged:");
                for path in &report.staged {
                    println!("- {path}");
                }
            }
            if !report.unstaged.is_empty() {
                println!("Unstaged:");
                for path in &report.unstaged {
                    println!("- {path}");
                }
            }
            if !report.untracked.is_empty() {
                println!("Untracked:");
                for path in &report.untracked {
                    println!("- {path}");
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_git_log_report(output: OutputFormat, report: &GitLogReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.entries.is_empty() {
                println!("No commits.");
                return Ok(());
            }
            for entry in &report.entries {
                println!(
                    "- {} {} ({}, {})",
                    entry.commit.chars().take(8).collect::<String>(),
                    entry.summary,
                    entry.author_name,
                    entry.committed_at
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_git_diff_group_report(
    output: OutputFormat,
    report: &GitDiffReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.diff.trim().is_empty() {
                if let Some(path) = &report.path {
                    println!("No changes in {path}.");
                } else {
                    println!("Working tree clean.");
                }
            } else {
                print!("{}", report.diff);
                if !report.diff.ends_with('\n') {
                    println!();
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_git_commit_report(output: OutputFormat, report: &GitCommitReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.committed {
                let sha = report.sha.as_deref().unwrap_or_default();
                println!(
                    "Committed {} file(s) as {}: {}",
                    report.files.len(),
                    sha.chars().take(8).collect::<String>(),
                    report.message
                );
            } else {
                println!("{}", report.message);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_git_blame_report(output: OutputFormat, report: &GitBlameReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            for line in &report.lines {
                println!(
                    "{:>4} {} {:<16} | {}",
                    line.line_number,
                    line.commit.chars().take(8).collect::<String>(),
                    line.author_name,
                    line.line
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_web_search_report(output: OutputFormat, report: &WebSearchReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.results.is_empty() {
                println!("No web results.");
                return Ok(());
            }
            for result in &report.results {
                println!("- {} [{}]", result.title, result.url);
                if !result.snippet.is_empty() {
                    println!("  {}", result.snippet);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_web_fetch_report(output: OutputFormat, report: &WebFetchReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if let Some(saved) = &report.saved {
                println!(
                    "Fetched {} [{} {}] -> {}",
                    report.url, report.status, report.content_type, saved
                );
            } else {
                print!("{}", report.content);
                if !report.content.ends_with('\n') {
                    println!();
                }
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
                    print_dataview_block_result_human(result, show_result_count);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_dataview_js_result(
    output: OutputFormat,
    result: &DataviewJsResult,
    show_result_count: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            print_dataview_js_result_human(result, show_result_count);
            Ok(())
        }
        OutputFormat::Json => print_json(result),
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

fn print_task_show_report(output: OutputFormat, report: &TaskShowReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("{}", report.path);
            println!("Title: {}", report.title);
            println!(
                "Status: {} ({}){}",
                report.status,
                report.status_type,
                if report.archived { ", archived" } else { "" }
            );
            println!("Priority: {}", report.priority);
            if let Some(due) = &report.due {
                println!("Due: {due}");
            }
            if let Some(scheduled) = &report.scheduled {
                println!("Scheduled: {scheduled}");
            }
            if let Some(completed_date) = &report.completed_date {
                println!("Completed: {completed_date}");
            }
            if !report.contexts.is_empty() {
                println!("Contexts: {}", report.contexts.join(", "));
            }
            if !report.projects.is_empty() {
                println!("Projects: {}", report.projects.join(", "));
            }
            if !report.tags.is_empty() {
                println!("Tags: {}", report.tags.join(", "));
            }
            if !report.body.trim().is_empty() {
                println!();
                println!("{}", report.body.trim_end());
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_add_report(output: OutputFormat, report: &TaskAddReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            let suffix = if report.dry_run { " (dry-run)" } else { "" };
            println!("{}{}", report.path, suffix);
            println!("Title: {}", report.title);
            println!("Status: {}", report.status);
            println!("Priority: {}", report.priority);
            if let Some(due) = &report.due {
                println!("Due: {due}");
            }
            if let Some(scheduled) = &report.scheduled {
                println!("Scheduled: {scheduled}");
            }
            if !report.contexts.is_empty() {
                println!("Contexts: {}", report.contexts.join(", "));
            }
            if !report.projects.is_empty() {
                println!("Projects: {}", report.projects.join(", "));
            }
            if !report.tags.is_empty() {
                println!("Tags: {}", report.tags.join(", "));
            }
            if let Some(time_estimate) = report.time_estimate {
                println!("Estimate: {time_estimate}m");
            }
            if let Some(recurrence) = &report.recurrence {
                println!("Recurrence: {recurrence}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_create_report(
    output: OutputFormat,
    report: &TaskCreateReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            let suffix = if report.dry_run { " (dry-run)" } else { "" };
            println!("{}{}", report.task, suffix);
            if report.created_note {
                println!("Created note: {}", report.path);
            }
            println!("{}", report.line);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_convert_report(
    output: OutputFormat,
    report: &TaskConvertReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            let suffix = if report.dry_run { " (dry-run)" } else { "" };
            if report.source_path == report.target_path {
                println!("{}{}", report.target_path, suffix);
            } else {
                println!("{} -> {}{}", report.source_path, report.target_path, suffix);
            }
            println!("Mode: {}", report.mode);
            println!("Title: {}", report.title);
            if let Some(line_number) = report.line_number {
                println!("Line: {line_number}");
            }
            if report.source_changes.is_empty() && report.task_changes.is_empty() {
                println!("No changes.");
            } else {
                for change in &report.source_changes {
                    println!("- {} -> {}", change.before, change.after);
                }
                for change in &report.task_changes {
                    println!("- {} -> {}", change.before, change.after);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_task_mutation_report(
    output: OutputFormat,
    report: &TaskMutationReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            let suffix = if report.dry_run { " (dry-run)" } else { "" };
            println!("{}{}", report.path, suffix);
            if let (Some(from), Some(to)) = (&report.moved_from, &report.moved_to) {
                println!("Moved: {from} -> {to}");
            }
            if report.changes.is_empty() {
                println!("No changes.");
            } else {
                for change in &report.changes {
                    println!("- {} -> {}", change.before, change.after);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_tasknotes_view_list_report(
    output: OutputFormat,
    report: &TaskNotesViewListReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.views.is_empty() {
                println!("No TaskNotes views.");
                return Ok(());
            }

            let mut current_file: Option<&str> = None;
            for view in &report.views {
                if current_file != Some(view.file.as_str()) {
                    if current_file.is_some() {
                        println!();
                    }
                    current_file = Some(view.file.as_str());
                    println!("{}", view.file);
                }
                let name = view.view_name.as_deref().unwrap_or("<unnamed>");
                let support = if view.supported { "" } else { " [deferred]" };
                println!("- {name} ({}){support}", view.view_type);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
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

fn print_kanban_board_list(
    output: OutputFormat,
    boards: &[KanbanBoardSummary],
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    let visible_boards = paginated_items(boards, list_controls);
    let rows = kanban_board_rows(visible_boards);
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Kanban boards"));
            }
            if visible_boards.is_empty() {
                println!("No indexed Kanban boards.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for board in visible_boards {
                    println!(
                        "- {} ({}) [{}] {} column(s), {} card(s)",
                        board.title, board.path, board.format, board.column_count, board.card_count
                    );
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
    }
}

fn print_kanban_board_report(
    output: OutputFormat,
    report: &KanbanBoardRecord,
    verbose: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            let card_count = report
                .columns
                .iter()
                .map(|column| column.card_count)
                .sum::<usize>();
            println!("{} ({})", report.title, report.path);
            println!("Format: {}", report.format);
            println!("Columns: {}", report.columns.len());
            println!("Cards: {card_count}");
            println!("Date trigger: {}", report.date_trigger);
            println!("Time trigger: {}", report.time_trigger);
            if report.columns.is_empty() {
                println!("No columns.");
                return Ok(());
            }

            for column in &report.columns {
                println!();
                println!("{} ({})", column.name, column.card_count);
                if !verbose {
                    continue;
                }
                for card in &column.cards {
                    print_kanban_card_summary(card);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_kanban_cards_report(
    output: OutputFormat,
    report: &KanbanCardsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    let visible_cards = paginated_items(&report.cards, list_controls);
    let rows = kanban_card_rows(report, visible_cards);
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!(
                    "{} {}",
                    palette.cyan("Kanban cards for"),
                    palette.bold(&report.board_title)
                );
            }
            if visible_cards.is_empty() {
                println!("No matching Kanban cards.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
                return Ok(());
            }

            let mut current_column: Option<&str> = None;
            for card in visible_cards {
                if current_column != Some(card.column.as_str()) {
                    current_column = Some(card.column.as_str());
                    println!("{}", card.column);
                }
                print_kanban_card_list_item(card);
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
    }
}

fn print_kanban_archive_report(
    output: OutputFormat,
    report: &KanbanArchiveReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.dry_run {
                println!(
                    "Dry run: archive {} from {} to {} in {}",
                    report.card_id, report.source_column, report.archive_column, report.path
                );
            } else {
                println!(
                    "Archived {} from {} to {} in {}",
                    report.card_id, report.source_column, report.archive_column, report.path
                );
            }
            println!("Card: {}", report.card_text);
            if report.created_archive_column {
                println!("Created archive column: {}", report.archive_column);
            }
            if report.archive_with_date_applied {
                println!("Archived text: {}", report.archived_text);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_kanban_move_report(
    output: OutputFormat,
    report: &KanbanMoveReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.dry_run {
                println!(
                    "Dry run: move {} from {} to {} in {}",
                    report.card_id, report.source_column, report.target_column, report.path
                );
            } else {
                println!(
                    "Moved {} from {} to {} in {}",
                    report.card_id, report.source_column, report.target_column, report.path
                );
            }
            println!("Card: {}", report.card_text);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_kanban_add_report(output: OutputFormat, report: &KanbanAddReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.dry_run {
                println!("Dry run: add card to {} in {}", report.column, report.path);
            } else {
                println!("Added card to {} in {}", report.column, report.path);
            }
            println!("Card: {}", report.card_text);
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
    print_dql_diagnostics_human(&result.diagnostics);
}

fn print_dataview_block_result_human(result: &DataviewBlockResult, show_result_count: bool) {
    match result {
        DataviewBlockResult::Dql(result) => print_dql_query_result_human(result, show_result_count),
        DataviewBlockResult::Js(result) => {
            print_dataview_js_result_human(result, show_result_count);
        }
    }
}

fn print_dataview_js_result_human(result: &DataviewJsResult, show_result_count: bool) {
    for (index, output) in result.outputs.iter().enumerate() {
        if index > 0 {
            println!();
        }
        match output {
            DataviewJsOutput::Query { result } => {
                print_dql_query_result_human(result, show_result_count);
            }
            DataviewJsOutput::Table { headers, rows } => {
                if !headers.is_empty() {
                    println!("{}", headers.join(" | "));
                }
                for row in rows {
                    let rendered = row
                        .iter()
                        .map(render_dataview_inline_value)
                        .collect::<Vec<_>>()
                        .join(" | ");
                    println!("{rendered}");
                }
            }
            DataviewJsOutput::List { items } => {
                for item in items {
                    println!("- {}", render_dataview_inline_value(item));
                }
            }
            DataviewJsOutput::TaskList {
                tasks,
                group_by_file,
            } => {
                let mut current_file: Option<&str> = None;
                for task in tasks {
                    let file = task
                        .get("path")
                        .and_then(Value::as_str)
                        .or_else(|| {
                            task.get("file")
                                .and_then(|file| file.get("path"))
                                .and_then(Value::as_str)
                        })
                        .unwrap_or("<unknown>");
                    if *group_by_file && current_file != Some(file) {
                        current_file = Some(file);
                        println!("{file}");
                    }
                    let status = task.get("status").and_then(Value::as_str).unwrap_or(" ");
                    let text = task
                        .get("text")
                        .map(render_dataview_inline_value)
                        .unwrap_or_default();
                    println!("- [{status}] {text}");
                }
            }
            DataviewJsOutput::Paragraph { text } | DataviewJsOutput::Span { text } => {
                println!("{text}");
            }
            DataviewJsOutput::Header { level, text } => {
                let prefix = "#".repeat((*level).max(1));
                println!("{prefix} {text}");
            }
            DataviewJsOutput::Element {
                element,
                text,
                attrs: _,
            } => {
                println!("<{element}> {text}");
            }
        }
    }

    if result.outputs.is_empty() {
        if let Some(value) = &result.value {
            println!("{}", render_dataview_inline_value(value));
        }
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

    let file_column = result.columns.first().map_or("File", String::as_str);
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

    let file_column = result.columns.get(1).map_or("File", String::as_str);
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

fn print_dql_diagnostics_human(diagnostics: &[vulcan_core::DqlDiagnostic]) {
    if diagnostics.is_empty() {
        return;
    }

    println!("Diagnostics:");
    for diagnostic in diagnostics {
        println!("- {}", diagnostic.message);
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
                "Created {} from {} ({}, {})",
                report.path, report.template, report.template_source, report.engine
            );
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            for diagnostic in &report.diagnostics {
                eprintln!("Diagnostic: {diagnostic}");
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
                "Inserted {} into {} ({}, {}, {})",
                report.template, report.note, report.mode, report.template_source, report.engine
            );
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            for diagnostic in &report.diagnostics {
                eprintln!("Diagnostic: {diagnostic}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_template_preview_report(
    output: OutputFormat,
    report: &TemplatePreviewReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("{}", report.content);
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            for diagnostic in &report.diagnostics {
                eprintln!("Diagnostic: {diagnostic}");
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

fn print_periodic_open_report(
    output: OutputFormat,
    report: &PeriodicOpenReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.created {
                println!("Created {}", report.path);
            } else {
                println!("Using {}", report.path);
            }
            println!(
                "{} period: {} to {}",
                report.period_type, report.start_date, report.end_date
            );
            if report.opened_editor {
                println!("Opened in editor.");
            }
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_daily_show_report(
    output: OutputFormat,
    report: &PeriodicShowReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            print!("{}", report.content);
            if !report.content.ends_with('\n') {
                println!();
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_daily_list_report(
    output: OutputFormat,
    items: &[DailyListItem],
    list_controls: &ListOutputControls,
) -> Result<(), CliError> {
    let visible = paginated_items(items, list_controls);
    let rows = visible
        .iter()
        .map(|item| serde_json::to_value(item).expect("daily list row should serialize"))
        .collect::<Vec<_>>();
    match output {
        OutputFormat::Human => {
            if visible.is_empty() {
                println!("No daily notes in range.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
                return Ok(());
            }
            for item in visible {
                println!("{} ({})", item.date, item.path);
                if item.events.is_empty() {
                    println!("- no events");
                    continue;
                }
                for event in &item.events {
                    match &event.end_time {
                        Some(end_time) => {
                            println!("- {}-{} {}", event.start_time, end_time, event.title);
                        }
                        None => println!("- {} {}", event.start_time, event.title),
                    }
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
    }
}

fn print_daily_export_ics_report(
    output: OutputFormat,
    report: &DailyIcsExportReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if let Some(path) = report.path.as_deref() {
                println!(
                    "Wrote {} event(s) from {} daily note(s) to {}",
                    report.event_count, report.note_count, path
                );
                println!("Range: {} to {}", report.from, report.to);
                println!("Calendar: {}", report.calendar_name);
                Ok(())
            } else {
                print!("{}", report.content);
                Ok(())
            }
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_daily_append_report(
    output: OutputFormat,
    report: &DailyAppendReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if report.created {
                println!("Created {}", report.path);
            }
            println!("Appended to {}", report.path);
            if let Some(heading) = report.heading.as_deref() {
                println!("Heading: {heading}");
            }
            for warning in &report.warnings {
                eprintln!("Warning: {warning}");
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_periodic_list_report(
    output: OutputFormat,
    items: &[PeriodicListItem],
    list_controls: &ListOutputControls,
) -> Result<(), CliError> {
    let visible = paginated_items(items, list_controls);
    let rows = visible
        .iter()
        .map(|item| serde_json::to_value(item).expect("periodic list row should serialize"))
        .collect::<Vec<_>>();
    match output {
        OutputFormat::Human => {
            if visible.is_empty() {
                println!("No indexed periodic notes.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
                return Ok(());
            }
            let mut current_type: Option<&str> = None;
            for item in visible {
                if current_type != Some(item.period_type.as_str()) {
                    current_type = Some(item.period_type.as_str());
                    println!("{}", item.period_type);
                }
                println!(
                    "- {} {} ({} event(s))",
                    item.date, item.path, item.event_count
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
    }
}

fn print_periodic_gap_report(
    output: OutputFormat,
    items: &[PeriodicGapItem],
    list_controls: &ListOutputControls,
) -> Result<(), CliError> {
    let visible = paginated_items(items, list_controls);
    let rows = visible
        .iter()
        .map(|item| serde_json::to_value(item).expect("periodic gap row should serialize"))
        .collect::<Vec<_>>();
    match output {
        OutputFormat::Human => {
            if visible.is_empty() {
                println!("No periodic gaps in range.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
                return Ok(());
            }
            let mut current_type: Option<&str> = None;
            for item in visible {
                if current_type != Some(item.period_type.as_str()) {
                    current_type = Some(item.period_type.as_str());
                    println!("{}", item.period_type);
                }
                println!("- {} -> {}", item.date, item.expected_path);
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
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
                    "- doctor: unresolved={}, ambiguous={}, parse_failures={}, type_mismatches={}, unsupported_syntax={}, stale={}, missing={}",
                    summary.unresolved_links,
                    summary.ambiguous_links,
                    summary.parse_failures,
                    summary.type_mismatches,
                    summary.unsupported_syntax,
                    summary.stale_index_rows,
                    summary.missing_index_rows
                );
            }
            if let Some(fix) = report.doctor_fix.as_ref() {
                let summary = fix.issues_after.as_ref().unwrap_or(&fix.issues_before);
                println!(
                    "- doctor-fix: {} actions, unresolved={}, ambiguous={}, parse_failures={}, type_mismatches={}, unsupported_syntax={}, stale={}, missing={}",
                    fix.fixes.len(),
                    summary.unresolved_links,
                    summary.ambiguous_links,
                    summary.parse_failures,
                    summary.type_mismatches,
                    summary.unsupported_syntax,
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
        commands: command
            .get_subcommands()
            .filter(|subcommand| !subcommand.is_hide_set())
            .map(describe_command)
            .collect(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum HelpTopicKind {
    Overview,
    Command,
    Concept,
    Guide,
}

impl Display for HelpTopicKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overview => formatter.write_str("overview"),
            Self::Command => formatter.write_str("command"),
            Self::Concept => formatter.write_str("concept"),
            Self::Guide => formatter.write_str("guide"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct HelpTopicReport {
    name: String,
    kind: HelpTopicKind,
    summary: String,
    body: String,
    options: Vec<CliArgDescribe>,
    subcommands: Vec<String>,
    related: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct HelpSearchReport {
    keyword: String,
    matches: Vec<HelpSearchMatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct HelpSearchMatch {
    name: String,
    kind: HelpTopicKind,
    summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct OpenAiToolsReport {
    tools: Vec<OpenAiToolDefinition>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct OpenAiToolDefinition {
    #[serde(rename = "type")]
    kind: String,
    function: OpenAiFunctionDefinition,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct OpenAiFunctionDefinition {
    name: String,
    description: String,
    parameters: Value,
    examples: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct McpToolsReport {
    tools: Vec<McpToolDefinition>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct McpToolDefinition {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
    examples: Vec<String>,
}

fn cli_command_tree() -> clap::Command {
    Cli::command().bin_name("vulcan")
}

fn print_describe_human(report: &CliDescribeReport) {
    if let Some(about) = report.about.as_deref() {
        println!("{about}");
        println!();
    }
    println!("Commands:");
    print_describe_command_list(&report.commands, "");
}

fn print_describe_command_list(commands: &[CliCommandDescribe], prefix: &str) {
    for command in commands {
        let name = if prefix.is_empty() {
            command.name.clone()
        } else {
            format!("{prefix} {}", command.name)
        };
        let about = command.about.as_deref().unwrap_or("undocumented");
        println!("- {name}: {about}");
        if !command.subcommands.is_empty() {
            print_describe_command_list(&command.subcommands, &name);
        }
    }
}

fn resolve_help_topic(topic: &[String]) -> Result<HelpTopicReport, CliError> {
    let key = topic.join(" ");
    if let Some(report) = builtin_help_topic(&key) {
        return Ok(report);
    }

    let root = cli_command_tree();
    let topic_refs = topic.iter().map(String::as_str).collect::<Vec<_>>();
    let Some(command) = find_command(&root, &topic_refs) else {
        return Err(CliError::operation(format!("unknown help topic `{key}`")));
    };
    Ok(help_topic_from_command(command, topic))
}

fn help_overview() -> HelpTopicReport {
    let root = cli_command_tree();
    let command_topics = collect_help_command_topics(&root);
    let command_names = command_topics
        .iter()
        .map(|topic| topic.name.clone())
        .collect::<Vec<_>>();
    let concept_names = builtin_help_topics()
        .into_iter()
        .map(|topic| topic.name)
        .collect::<Vec<_>>();
    HelpTopicReport {
        name: "help".to_string(),
        kind: HelpTopicKind::Overview,
        summary: "Integrated documentation for commands and core concepts.".to_string(),
        body: format!(
            "Use `vulcan help <topic>` for one topic or `vulcan help --search <keyword>` to search.\n\nCommand topics include paths like `query`, `note get`, `refactor`, and `daily append`.\nConcept topics include: {}.",
            concept_names.join(", ")
        ),
        options: Vec::new(),
        subcommands: command_names,
        related: concept_names,
    }
}

fn static_help_topic(
    name: &str,
    kind: HelpTopicKind,
    summary: &str,
    body: &str,
    related: &[&str],
) -> HelpTopicReport {
    HelpTopicReport {
        name: name.to_string(),
        kind,
        summary: summary.to_string(),
        body: body.trim().to_string(),
        options: Vec::new(),
        subcommands: Vec::new(),
        related: related.iter().map(|item| (*item).to_string()).collect(),
    }
}

fn builtin_help_topics() -> Vec<HelpTopicReport> {
    vec![
        static_help_topic(
            "getting-started",
            HelpTopicKind::Guide,
            "Quick orientation for the CLI and its main workflows.",
            include_str!("../../docs/guide/getting-started.md"),
            &["query", "search", "note get", "note create"],
        ),
        static_help_topic(
            "examples",
            HelpTopicKind::Guide,
            "Representative command patterns for common vault workflows.",
            include_str!("../../docs/examples/recipes.md"),
            &["filters", "query-dsl", "note get", "refactor"],
        ),
        static_help_topic(
            "filters",
            HelpTopicKind::Concept,
            "Typed `--where` filter grammar shared across notes, search, and mutations.",
            include_str!("../../docs/guide/filters.md"),
            &["notes", "search", "query"],
        ),
        static_help_topic(
            "query-dsl",
            HelpTopicKind::Concept,
            "The shared query DSL used by `vulcan query` and related tooling.",
            include_str!("../../docs/guide/query-dsl.md"),
            &["query", "ls", "search"],
        ),
        static_help_topic(
            "scripting",
            HelpTopicKind::Concept,
            "Current scripting-oriented surfaces and the path to the standalone JS runtime.",
            include_str!("../../docs/guide/scripting.md"),
            &["sandbox", "js", "describe"],
        ),
        static_help_topic(
            "sandbox",
            HelpTopicKind::Concept,
            "Sandbox guarantees and execution limits for JavaScript-backed features.",
            include_str!("../../docs/guide/sandbox.md"),
            &["scripting", "js.vault", "web"],
        ),
        static_help_topic(
            "js",
            HelpTopicKind::Concept,
            "Overview of the JS runtime surface, including current and planned namespaces.",
            include_str!("../../docs/reference/js-api/index.md"),
            &["js.vault", "js.vault.graph", "js.vault.note"],
        ),
        static_help_topic(
            "js.vault",
            HelpTopicKind::Concept,
            "Primary JS namespace for vault-oriented reads, queries, and periodic helpers.",
            include_str!("../../docs/reference/js-api/vault.md"),
            &["js", "js.vault.graph", "js.vault.note"],
        ),
        static_help_topic(
            "js.vault.graph",
            HelpTopicKind::Concept,
            "Planned graph traversal and relationship inspection surface for the JS runtime.",
            include_str!("../../docs/reference/js-api/graph.md"),
            &["js.vault", "graph", "graph path"],
        ),
        static_help_topic(
            "js.vault.note",
            HelpTopicKind::Concept,
            "Shape and usage guidance for the planned JS Note object.",
            include_str!("../../docs/reference/js-api/note-object.md"),
            &["js.vault", "note get", "query"],
        ),
    ]
}

fn builtin_help_topic(name: &str) -> Option<HelpTopicReport> {
    builtin_help_topics()
        .into_iter()
        .find(|topic| topic.name.eq_ignore_ascii_case(name))
}

fn search_help_topics(keyword: &str) -> HelpSearchReport {
    let lowered = keyword.to_ascii_lowercase();
    let mut matches = builtin_help_topics()
        .into_iter()
        .filter(|topic| {
            topic.name.to_ascii_lowercase().contains(&lowered)
                || topic.summary.to_ascii_lowercase().contains(&lowered)
                || topic.body.to_ascii_lowercase().contains(&lowered)
        })
        .map(|topic| HelpSearchMatch {
            name: topic.name,
            kind: topic.kind,
            summary: topic.summary,
        })
        .collect::<Vec<_>>();

    matches.extend(
        collect_help_command_topics(&cli_command_tree())
            .into_iter()
            .filter(|topic| {
                topic.name.to_ascii_lowercase().contains(&lowered)
                    || topic.summary.to_ascii_lowercase().contains(&lowered)
                    || topic.body.to_ascii_lowercase().contains(&lowered)
            })
            .map(|topic| HelpSearchMatch {
                name: topic.name,
                kind: topic.kind,
                summary: topic.summary,
            }),
    );

    matches.sort_by(|left, right| left.name.cmp(&right.name));
    HelpSearchReport {
        keyword: keyword.to_string(),
        matches,
    }
}

fn collect_help_command_topics(command: &clap::Command) -> Vec<HelpTopicReport> {
    let mut topics = Vec::new();
    for subcommand in command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
    {
        collect_help_command_topics_inner(subcommand, Vec::new(), &mut topics);
    }
    topics
}

fn collect_help_command_topics_inner(
    command: &clap::Command,
    mut prefix: Vec<String>,
    topics: &mut Vec<HelpTopicReport>,
) {
    prefix.push(command.get_name().to_string());
    topics.push(help_topic_from_command(command, &prefix));
    for subcommand in command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
    {
        collect_help_command_topics_inner(subcommand, prefix.clone(), topics);
    }
}

fn help_topic_from_command(command: &clap::Command, path: &[String]) -> HelpTopicReport {
    let summary = command.get_about().map_or_else(
        || format!("Help for `{}`", path.join(" ")),
        ToString::to_string,
    );
    let mut sections = Vec::new();
    if let Some(after_help) = command.get_after_help() {
        let trimmed = after_help.to_string();
        if !trimmed.is_empty() {
            sections.push(trimmed);
        }
    }
    let subcommands = command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .map(|subcommand| {
            format!(
                "{} {}",
                path.join(" "),
                subcommand.get_name().replace('-', "_").replace('_', "-")
            )
        })
        .collect::<Vec<_>>();

    HelpTopicReport {
        name: path.join(" "),
        kind: HelpTopicKind::Command,
        summary,
        body: sections.join("\n\n"),
        options: command
            .get_arguments()
            .filter(|argument| !argument.is_global_set())
            .map(describe_argument)
            .collect(),
        subcommands,
        related: Vec::new(),
    }
}

fn find_command<'a>(command: &'a clap::Command, path: &[&str]) -> Option<&'a clap::Command> {
    let mut current = command;
    for segment in path {
        current = current
            .get_subcommands()
            .find(|candidate| candidate.get_name().eq_ignore_ascii_case(segment))?;
    }
    Some(current)
}

fn build_openai_tool_definitions() -> OpenAiToolsReport {
    OpenAiToolsReport {
        tools: collect_leaf_commands(&cli_command_tree())
            .into_iter()
            .map(|tool| OpenAiToolDefinition {
                kind: "function".to_string(),
                function: OpenAiFunctionDefinition {
                    name: tool.name,
                    description: tool.description,
                    parameters: tool.input_schema,
                    examples: tool.examples,
                },
            })
            .collect(),
    }
}

fn build_mcp_tool_definitions() -> McpToolsReport {
    McpToolsReport {
        tools: collect_leaf_commands(&cli_command_tree())
            .into_iter()
            .map(|tool| McpToolDefinition {
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
                examples: tool.examples,
            })
            .collect(),
    }
}

#[derive(Debug, Clone)]
struct ToolCommandDescribe {
    name: String,
    description: String,
    input_schema: Value,
    examples: Vec<String>,
}

fn collect_leaf_commands(command: &clap::Command) -> Vec<ToolCommandDescribe> {
    let mut tools = Vec::new();
    for subcommand in command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
    {
        collect_leaf_commands_inner(subcommand, Vec::new(), &mut tools);
    }
    tools
}

fn collect_leaf_commands_inner(
    command: &clap::Command,
    mut prefix: Vec<String>,
    tools: &mut Vec<ToolCommandDescribe>,
) {
    prefix.push(command.get_name().to_string());
    let subcommands = command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .collect::<Vec<_>>();
    if subcommands.is_empty() {
        tools.push(ToolCommandDescribe {
            name: tool_name_from_path(&prefix),
            description: command
                .get_about()
                .map_or_else(|| prefix.join(" "), ToString::to_string),
            input_schema: command_input_schema(command),
            examples: extract_examples(command),
        });
        return;
    }
    for subcommand in subcommands {
        collect_leaf_commands_inner(subcommand, prefix.clone(), tools);
    }
}

fn tool_name_from_path(path: &[String]) -> String {
    path.iter()
        .map(|segment| segment.replace('-', "_"))
        .collect::<Vec<_>>()
        .join("_")
}

fn command_input_schema(command: &clap::Command) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for argument in command
        .get_arguments()
        .filter(|argument| !argument.is_global_set())
    {
        properties.insert(
            argument.get_id().to_string(),
            argument_json_schema(argument),
        );
        if argument.is_required_set() {
            required.push(Value::String(argument.get_id().to_string()));
        }
    }
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert("properties".to_string(), Value::Object(properties));
    schema.insert("additionalProperties".to_string(), Value::Bool(false));
    if !required.is_empty() {
        schema.insert("required".to_string(), Value::Array(required));
    }
    Value::Object(schema)
}

fn argument_json_schema(argument: &clap::Arg) -> Value {
    let schema = match argument.get_action() {
        clap::ArgAction::SetTrue | clap::ArgAction::SetFalse => serde_json::json!({
            "type": "boolean",
        }),
        clap::ArgAction::Append => serde_json::json!({
            "type": "array",
            "items": scalar_argument_schema(argument),
        }),
        clap::ArgAction::Count => serde_json::json!({
            "type": "integer",
        }),
        _ => scalar_argument_schema(argument),
    };

    let mut schema = schema;
    if let Some(description) = argument.get_help().map(ToString::to_string) {
        if let Some(object) = schema.as_object_mut() {
            object.insert("description".to_string(), Value::String(description));
        }
    }
    if let Some(default) = argument.get_default_values().first() {
        if let Some(object) = schema.as_object_mut() {
            object.insert(
                "default".to_string(),
                Value::String(default.to_string_lossy().to_string()),
            );
        }
    }
    schema
}

fn scalar_argument_schema(argument: &clap::Arg) -> Value {
    let values = argument
        .get_possible_values()
        .into_iter()
        .map(|value| Value::String(value.get_name().to_string()))
        .collect::<Vec<_>>();
    if values
        == [
            Value::String("true".to_string()),
            Value::String("false".to_string()),
        ]
    {
        serde_json::json!({ "type": "boolean" })
    } else if values.is_empty() {
        serde_json::json!({ "type": "string" })
    } else {
        serde_json::json!({
            "type": "string",
            "enum": values,
        })
    }
}

fn extract_examples(command: &clap::Command) -> Vec<String> {
    let Some(after_help) = command.get_after_help() else {
        return Vec::new();
    };
    let mut capture = false;
    let mut examples = Vec::new();
    for line in after_help.to_string().lines() {
        let trimmed = line.trim();
        if trimmed == "Examples:" {
            capture = true;
            continue;
        }
        if !capture {
            continue;
        }
        if trimmed.is_empty() {
            break;
        }
        examples.push(trimmed.to_string());
    }
    examples
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
        subcommands: command
            .get_subcommands()
            .filter(|subcommand| !subcommand.is_hide_set())
            .map(describe_command)
            .collect(),
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

fn kanban_board_rows(boards: &[KanbanBoardSummary]) -> Vec<Value> {
    boards
        .iter()
        .map(|board| {
            serde_json::json!({
                "path": board.path,
                "title": board.title,
                "format": board.format,
                "column_count": board.column_count,
                "card_count": board.card_count,
            })
        })
        .collect()
}

fn kanban_card_rows(report: &KanbanCardsReport, cards: &[KanbanCardListItem]) -> Vec<Value> {
    cards
        .iter()
        .map(|card| {
            serde_json::json!({
                "board_path": report.board_path,
                "board_title": report.board_title,
                "column_filter": report.column_filter,
                "status_filter": report.status_filter,
                "column": card.column,
                "card_id": card.id,
                "text": card.text,
                "line_number": card.line_number,
                "block_id": card.block_id,
                "symbol": card.symbol,
                "tags": card.tags,
                "outlinks": card.outlinks,
                "date": card.date,
                "time": card.time,
                "inline_fields": card.inline_fields,
                "metadata": card.metadata,
                "task": card.task,
                "task_status_char": card.task.as_ref().map(|task| task.status_char.clone()),
                "task_status_name": card.task.as_ref().map(|task| task.status_name.clone()),
                "task_status_type": card.task.as_ref().map(|task| task.status_type.clone()),
                "task_checked": card.task.as_ref().map(|task| task.checked),
                "task_completed": card.task.as_ref().map(|task| task.completed),
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

fn print_kanban_card_summary(card: &vulcan_core::KanbanCardRecord) {
    let mut details = vec![format!("line {}", card.line_number)];
    if let Some(date) = card.date.as_deref() {
        details.push(format!("date {date}"));
    }
    if let Some(time) = card.time.as_deref() {
        details.push(format!("time {time}"));
    }
    if !card.tags.is_empty() {
        details.push(format!("tags {}", card.tags.join(", ")));
    }
    if !card.outlinks.is_empty() {
        details.push(format!("links {}", card.outlinks.join(", ")));
    }
    if let Some(task) = card.task.as_ref() {
        println!(
            "- [{}] {} ({})",
            task.status_char,
            card.text,
            details.join(", ")
        );
    } else {
        println!("- {} ({})", card.text, details.join(", "));
    }
}

fn print_kanban_card_list_item(card: &KanbanCardListItem) {
    let mut details = vec![format!("line {}", card.line_number)];
    if let Some(date) = card.date.as_deref() {
        details.push(format!("date {date}"));
    }
    if let Some(time) = card.time.as_deref() {
        details.push(format!("time {time}"));
    }
    if let Some(task) = card.task.as_ref() {
        println!(
            "- [{}] {} ({})",
            task.status_char,
            card.text,
            details.join(", ")
        );
    } else {
        println!("- {} ({})", card.text, details.join(", "));
    }
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
        type_mismatches: 0,
        unsupported_syntax: 0,
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
        assert!(status.success(), "git command failed: {args:?}");
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
    fn parses_dataview_query_js_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "dataview",
            "query-js",
            "dv.current()",
            "--file",
            "Dashboard",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Dataview {
                command: DataviewCommand::QueryJs {
                    js: "dv.current()".to_string(),
                    file: Some("Dashboard".to_string()),
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
    fn parses_tasks_add_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "add",
            "Buy groceries tomorrow @home",
            "--status",
            "open",
            "--priority",
            "high",
            "--due",
            "2026-04-10",
            "--scheduled",
            "2026-04-09",
            "--context",
            "@errands",
            "--project",
            "Website",
            "--tag",
            "shopping",
            "--template",
            "task",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Add {
                    text: "Buy groceries tomorrow @home".to_string(),
                    no_nlp: false,
                    status: Some("open".to_string()),
                    priority: Some("high".to_string()),
                    due: Some("2026-04-10".to_string()),
                    scheduled: Some("2026-04-09".to_string()),
                    contexts: vec!["@errands".to_string()],
                    projects: vec!["Website".to_string()],
                    tags: vec!["shopping".to_string()],
                    template: Some("task".to_string()),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_show_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "show", "Write Docs"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Show {
                    task: "Write Docs".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_tasks_edit_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "edit", "Write Docs", "--no-commit"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Edit {
                    task: "Write Docs".to_string(),
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_set_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "set",
            "Write Docs",
            "due",
            "2026-04-12",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Set {
                    task: "Write Docs".to_string(),
                    property: "due".to_string(),
                    value: "2026-04-12".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_complete_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "complete",
            "Write Docs",
            "--date",
            "2026-04-10",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Complete {
                    task: "Write Docs".to_string(),
                    date: Some("2026-04-10".to_string()),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_archive_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "archive",
            "Prep Outline",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Archive {
                    task: "Prep Outline".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_convert_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "convert",
            "Notes/Idea.md",
            "--line",
            "12",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Convert {
                    file: "Notes/Idea.md".to_string(),
                    line: Some(12),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_create_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "create",
            "Call Alice tomorrow @desk",
            "--in",
            "Inbox",
            "--due",
            "2026-04-12",
            "--priority",
            "high",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Create {
                    text: "Call Alice tomorrow @desk".to_string(),
                    note: Some("Inbox".to_string()),
                    due: Some("2026-04-12".to_string()),
                    priority: Some("high".to_string()),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_tasks_reschedule_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "reschedule",
            "Inbox:3",
            "--due",
            "2026-04-12",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::Reschedule {
                    task: "Inbox:3".to_string(),
                    due: "2026-04-12".to_string(),
                    dry_run: true,
                    no_commit: true,
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
        let cli = Cli::try_parse_from([
            "vulcan",
            "tasks",
            "list",
            "--filter",
            "completed",
            "--source",
            "file",
            "--status",
            "in-progress",
            "--priority",
            "high",
            "--due-before",
            "2026-04-11",
            "--due-after",
            "2026-04-01",
            "--project",
            "[[Projects/Website]]",
            "--context",
            "@desk",
            "--group-by",
            "source",
            "--sort-by",
            "due",
            "--include-archived",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::List {
                    filter: Some("completed".to_string()),
                    source: TasksListSourceArg::File,
                    status: Some("in-progress".to_string()),
                    priority: Some("high".to_string()),
                    due_before: Some("2026-04-11".to_string()),
                    due_after: Some("2026-04-01".to_string()),
                    project: Some("[[Projects/Website]]".to_string()),
                    context: Some("@desk".to_string()),
                    group_by: Some("source".to_string()),
                    sort_by: Some("due".to_string()),
                    include_archived: true,
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
    fn parses_tasks_view_show_command() {
        let cli = Cli::try_parse_from(["vulcan", "tasks", "view", "show", "Tasks"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::View {
                    command: TasksViewCommand::Show {
                        name: "Tasks".to_string(),
                        export: ExportArgs::default(),
                    },
                },
            }
        );
    }

    #[test]
    fn parses_tasks_view_list_command() {
        let cli =
            Cli::try_parse_from(["vulcan", "tasks", "view", "list"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Tasks {
                command: TasksCommand::View {
                    command: TasksViewCommand::List,
                },
            }
        );
    }

    #[test]
    fn parses_kanban_list_command() {
        let cli = Cli::try_parse_from(["vulcan", "kanban", "list"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Kanban {
                command: KanbanCommand::List,
            }
        );
    }

    #[test]
    fn parses_kanban_show_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "kanban",
            "show",
            "Board",
            "--verbose",
            "--include-archive",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Kanban {
                command: KanbanCommand::Show {
                    board: "Board".to_string(),
                    verbose: true,
                    include_archive: true,
                },
            }
        );
    }

    #[test]
    fn parses_kanban_cards_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "kanban",
            "cards",
            "Board",
            "--column",
            "Todo",
            "--status",
            "IN_PROGRESS",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Kanban {
                command: KanbanCommand::Cards {
                    board: "Board".to_string(),
                    column: Some("Todo".to_string()),
                    status: Some("IN_PROGRESS".to_string()),
                },
            }
        );
    }

    #[test]
    fn parses_kanban_archive_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "kanban",
            "archive",
            "Board",
            "build-release",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Kanban {
                command: KanbanCommand::Archive {
                    board: "Board".to_string(),
                    card: "build-release".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_kanban_move_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "kanban",
            "move",
            "Board",
            "build-release",
            "Done",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Kanban {
                command: KanbanCommand::Move {
                    board: "Board".to_string(),
                    card: "build-release".to_string(),
                    target_column: "Done".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_kanban_add_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "kanban",
            "add",
            "Board",
            "Todo",
            "Build release",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Kanban {
                command: KanbanCommand::Add {
                    board: "Board".to_string(),
                    column: "Todo".to_string(),
                    text: "Build release".to_string(),
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_daily_append_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "daily",
            "append",
            "Called Alice",
            "--heading",
            "## Log",
            "--date",
            "2026-04-03",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Daily {
                command: DailyCommand::Append {
                    text: "Called Alice".to_string(),
                    heading: Some("## Log".to_string()),
                    date: Some("2026-04-03".to_string()),
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_note_get_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "note",
            "get",
            "Dashboard",
            "--heading",
            "Tasks",
            "--match",
            "TODO",
            "--context",
            "1",
            "--raw",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Note {
                command: NoteCommand::Get {
                    note: "Dashboard".to_string(),
                    heading: Some("Tasks".to_string()),
                    block_ref: None,
                    lines: None,
                    match_pattern: Some("TODO".to_string()),
                    context: 1,
                    no_frontmatter: false,
                    raw: true,
                },
            }
        );
    }

    #[test]
    fn parses_note_append_periodic_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "note",
            "append",
            "- {{VALUE:title|case:slug}}",
            "--periodic",
            "daily",
            "--date",
            "2026-04-03",
            "--prepend",
            "--var",
            "title=Release Planning",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Note {
                command: NoteCommand::Append {
                    note_or_text: "- {{VALUE:title|case:slug}}".to_string(),
                    text: None,
                    heading: None,
                    prepend: true,
                    append: false,
                    periodic: Some(NoteAppendPeriodicArg::Daily),
                    date: Some("2026-04-03".to_string()),
                    vars: vec!["title=Release Planning".to_string()],
                    check: false,
                    no_commit: false,
                },
            }
        );
    }

    #[test]
    fn parses_note_patch_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "note",
            "patch",
            "Dashboard",
            "--find",
            "/TODO \\d+/",
            "--replace",
            "DONE",
            "--all",
            "--check",
            "--dry-run",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Note {
                command: NoteCommand::Patch {
                    note: "Dashboard".to_string(),
                    find: "/TODO \\d+/".to_string(),
                    replace: "DONE".to_string(),
                    all: true,
                    check: true,
                    dry_run: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_daily_export_ics_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "daily",
            "export-ics",
            "--month",
            "--path",
            "journal.ics",
            "--calendar-name",
            "Journal",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Daily {
                command: DailyCommand::ExportIcs {
                    from: None,
                    to: None,
                    week: false,
                    month: true,
                    path: Some(PathBuf::from("journal.ics")),
                    calendar_name: Some("Journal".to_string()),
                },
            }
        );
    }

    #[test]
    fn parses_git_status_command() {
        let cli = Cli::try_parse_from(["vulcan", "git", "status"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Git {
                command: GitCommand::Status,
            }
        );
    }

    #[test]
    fn parses_git_log_command() {
        let cli = Cli::try_parse_from(["vulcan", "git", "log", "--limit", "5"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Git {
                command: GitCommand::Log { limit: 5 },
            }
        );
    }

    #[test]
    fn parses_git_diff_command() {
        let cli =
            Cli::try_parse_from(["vulcan", "git", "diff", "Home.md"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Git {
                command: GitCommand::Diff {
                    path: Some("Home.md".to_string()),
                },
            }
        );
    }

    #[test]
    fn parses_git_commit_command() {
        let cli = Cli::try_parse_from(["vulcan", "git", "commit", "-m", "Update notes"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Git {
                command: GitCommand::Commit {
                    message: "Update notes".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_git_blame_command() {
        let cli =
            Cli::try_parse_from(["vulcan", "git", "blame", "Home.md"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Git {
                command: GitCommand::Blame {
                    path: "Home.md".to_string(),
                },
            }
        );
    }

    #[test]
    fn parses_web_search_command() {
        let cli = Cli::try_parse_from(["vulcan", "web", "search", "release notes", "--limit", "5"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Web {
                command: WebCommand::Search {
                    query: "release notes".to_string(),
                    backend: None,
                    limit: 5,
                },
            }
        );
    }

    #[test]
    fn parses_web_fetch_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "web",
            "fetch",
            "https://example.com",
            "--mode",
            "raw",
            "--save",
            "page.bin",
            "--extract-article",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Web {
                command: WebCommand::Fetch {
                    url: "https://example.com".to_string(),
                    mode: WebFetchMode::Raw,
                    save: Some(PathBuf::from("page.bin")),
                    extract_article: true,
                },
            }
        );
    }

    #[test]
    fn parses_weekly_command() {
        let cli =
            Cli::try_parse_from(["vulcan", "weekly", "2026-04-03", "--no-edit", "--no-commit"])
                .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Weekly {
                args: PeriodicOpenArgs {
                    date: Some("2026-04-03".to_string()),
                    no_edit: true,
                    no_commit: true,
                },
            }
        );
    }

    #[test]
    fn parses_periodic_gaps_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "periodic",
            "gaps",
            "--type",
            "daily",
            "--from",
            "2026-04-01",
            "--to",
            "2026-04-07",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Periodic {
                command: Some(PeriodicSubcommand::Gaps {
                    period_type: Some("daily".to_string()),
                    from: Some("2026-04-01".to_string()),
                    to: Some("2026-04-07".to_string()),
                }),
                period_type: None,
                date: None,
                no_edit: false,
                no_commit: false,
            }
        );
    }

    #[test]
    fn parses_config_import_tasks_command() {
        let cli =
            Cli::try_parse_from(["vulcan", "config", "import", "tasks"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::Tasks),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        target: ConfigImportTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_periodic_notes_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "import", "periodic-notes"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::PeriodicNotes),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        target: ConfigImportTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_tasknotes_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "import", "tasknotes"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::TaskNotes),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        target: ConfigImportTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_templater_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "import", "templater"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::Templater),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        target: ConfigImportTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_quickadd_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "import", "quickadd"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::Quickadd),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        target: ConfigImportTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_templater_template_preview_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "template",
            "preview",
            "daily",
            "--path",
            "Journal/Today",
            "--engine",
            "templater",
            "--var",
            "project=Vulcan",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Template {
                command: Some(TemplateSubcommand::Preview {
                    template: "daily".to_string(),
                    path: Some("Journal/Today".to_string()),
                    render: TemplateRenderArgs {
                        engine: TemplateEngineArg::Templater,
                        vars: vec!["project=Vulcan".to_string()],
                    },
                }),
                name: None,
                list: false,
                path: None,
                render: TemplateRenderArgs {
                    engine: TemplateEngineArg::Auto,
                    vars: Vec::new(),
                },
                no_commit: false,
            }
        );
    }

    #[test]
    fn parses_config_import_kanban_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "import", "kanban"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::Kanban),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        target: ConfigImportTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_core_command_with_shared_flags() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "config",
            "import",
            "core",
            "--dry-run",
            "--target",
            "local",
            "--no-commit",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::Core),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: true,
                        target: ConfigImportTargetArg::Local,
                        no_commit: true,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_dataview_command() {
        let cli = Cli::try_parse_from(["vulcan", "config", "import", "dataview"])
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: Some(ConfigImportCommand::Dataview),
                    all: false,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: false,
                        target: ConfigImportTargetArg::Shared,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_config_import_all_command() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "config",
            "import",
            "--all",
            "--dry-run",
            "--target",
            "local",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Config {
                command: ConfigCommand::Import(ConfigImportSelection {
                    command: None,
                    all: true,
                    list: false,
                    args: ConfigImportArgs {
                        dry_run: true,
                        target: ConfigImportTargetArg::Local,
                        no_commit: false,
                    },
                }),
            }
        );
    }

    #[test]
    fn parses_init_import_flags() {
        let cli = Cli::try_parse_from(["vulcan", "init", "--import"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Init(InitArgs {
                import: true,
                no_import: false,
                agent_files: false,
            })
        );
    }

    #[test]
    fn parses_init_agent_files_flag() {
        let cli =
            Cli::try_parse_from(["vulcan", "init", "--agent-files"]).expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Init(InitArgs {
                import: false,
                no_import: false,
                agent_files: true,
            })
        );
    }

    #[test]
    fn config_import_dry_run_does_not_write_target_file() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let plugin_dir = temp_dir
            .path()
            .join(".obsidian/plugins/obsidian-tasks-plugin");
        fs::create_dir_all(&plugin_dir).expect("tasks plugin dir should be created");
        fs::write(
            plugin_dir.join("data.json"),
            r##"{
              "globalFilter": "#task",
              "globalQuery": "not done",
              "removeGlobalFilter": true,
              "setCreatedDate": false
            }"##,
        )
        .expect("tasks config should be written");

        run_from([
            "vulcan",
            "--vault",
            temp_dir.path().to_str().expect("vault path should be utf8"),
            "config",
            "import",
            "tasks",
            "--dry-run",
        ])
        .expect("config import dry run should succeed");

        assert!(!temp_dir.path().join(".vulcan/config.toml").exists());
    }

    #[test]
    fn config_import_target_local_writes_local_config_file() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let obsidian_dir = temp_dir.path().join(".obsidian");
        fs::create_dir_all(&obsidian_dir).expect("obsidian dir should be created");
        fs::write(
            obsidian_dir.join("app.json"),
            r#"{
              "useMarkdownLinks": true,
              "strictLineBreaks": true
            }"#,
        )
        .expect("core app config should be written");

        run_from([
            "vulcan",
            "--vault",
            temp_dir.path().to_str().expect("vault path should be utf8"),
            "config",
            "import",
            "core",
            "--target",
            "local",
        ])
        .expect("core import should succeed");

        let local_config = fs::read_to_string(temp_dir.path().join(".vulcan/config.local.toml"))
            .expect("local config should exist");
        assert!(local_config.contains("[links]"));
        assert!(local_config.contains("style = \"markdown\""));
        assert!(local_config.contains("strict_line_breaks = true"));
        assert!(!temp_dir.path().join(".vulcan/config.toml").exists());
    }

    #[test]
    fn config_import_dry_run_target_local_does_not_write_local_file() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let obsidian_dir = temp_dir.path().join(".obsidian");
        fs::create_dir_all(&obsidian_dir).expect("obsidian dir should be created");
        fs::write(
            obsidian_dir.join("templates.json"),
            r#"{
              "dateFormat": "DD/MM/YYYY",
              "timeFormat": "HH:mm"
            }"#,
        )
        .expect("templates config should be written");

        run_from([
            "vulcan",
            "--vault",
            temp_dir.path().to_str().expect("vault path should be utf8"),
            "config",
            "import",
            "core",
            "--dry-run",
            "--target",
            "local",
        ])
        .expect("core dry run should succeed");

        assert!(!temp_dir.path().join(".vulcan/config.local.toml").exists());
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

    fn write_bases_create_fixture(vault_root: &Path, with_template: bool) {
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        if with_template {
            fs::create_dir_all(vault_root.join(".vulcan/templates"))
                .expect("template dir should exist");
            fs::write(
                vault_root.join(".vulcan/templates/Project.md"),
                concat!(
                    "---\n",
                    "owner: Template Owner\n",
                    "tags:\n",
                    "  - base\n",
                    "---\n",
                    "# {{title}}\n\n",
                    "Template body.\n",
                ),
            )
            .expect("template should be written");
        }
        fs::write(
            vault_root.join("release.base"),
            if with_template {
                concat!(
                    "create_template: Project\n",
                    "filters:\n",
                    "  - 'file.folder = \"Projects\"'\n",
                    "views:\n",
                    "  - name: Inbox\n",
                    "    type: table\n",
                    "    filters:\n",
                    "      - 'status = todo'\n",
                    "      - 'priority = 2'\n",
                )
            } else {
                concat!(
                    "filters:\n",
                    "  - 'file.folder = \"Projects\"'\n",
                    "views:\n",
                    "  - name: Inbox\n",
                    "    type: table\n",
                    "    filters:\n",
                    "      - 'status = todo'\n",
                )
            },
        )
        .expect("base file should be written");
    }

    #[test]
    fn bases_create_dry_run_does_not_write_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        write_bases_create_fixture(temp_dir.path(), false);
        let paths = VaultPaths::new(temp_dir.path());

        let report = create_note_from_bases_view(&paths, "release.base", 0, None, true)
            .expect("bases create should succeed");

        assert_eq!(report.path, "Projects/Untitled.md");
        assert!(!temp_dir.path().join("Projects/Untitled.md").exists());
    }

    #[test]
    fn bases_create_writes_template_with_derived_frontmatter() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        write_bases_create_fixture(temp_dir.path(), true);
        let paths = VaultPaths::new(temp_dir.path());

        let report =
            create_note_from_bases_view(&paths, "release.base", 0, Some("Launch Plan"), false)
                .expect("bases create should succeed");

        assert_eq!(report.path, "Projects/Launch Plan.md");
        let source = fs::read_to_string(temp_dir.path().join(&report.path))
            .expect("created note should be readable");
        let (frontmatter, body) =
            parse_frontmatter_document(&source, false).expect("created note should parse");
        let frontmatter = YamlValue::Mapping(frontmatter.expect("frontmatter should exist"));

        assert_eq!(frontmatter["status"], YamlValue::String("todo".to_string()));
        assert_eq!(frontmatter["priority"], YamlValue::Number(2_i64.into()));
        assert_eq!(
            frontmatter["owner"],
            YamlValue::String("Template Owner".to_string())
        );
        assert_eq!(
            frontmatter["tags"],
            serde_yaml::from_str::<YamlValue>("- base\n").expect("tag yaml should parse")
        );
        assert!(body.contains("# Launch Plan"));
        assert!(body.contains("Template body."));
    }

    #[test]
    fn bases_create_auto_commit_creates_git_commit() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        init_git_repo(temp_dir.path());
        write_bases_create_fixture(temp_dir.path(), false);
        fs::write(
            temp_dir.path().join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\n",
        )
        .expect("config should be written");

        run_from([
            "vulcan",
            "--vault",
            temp_dir.path().to_str().expect("temp dir should be utf8"),
            "bases",
            "create",
            "release.base",
            "--title",
            "Launch Plan",
        ])
        .expect("bases create should succeed");

        assert!(temp_dir.path().join("Projects/Launch Plan.md").exists());
        assert_eq!(
            git_head_summary(temp_dir.path()),
            "vulcan bases-create: Projects/Launch Plan.md"
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
            ..TemplatesConfig::default()
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

        let result = run_template_command(
            &paths,
            None,
            true,
            None,
            TemplateEngineArg::Auto,
            &[],
            false,
            false,
        )
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
    fn template_command_lists_templater_templates_with_sources() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(paths.vulcan_dir().join("templates")).expect("template dir");
        fs::create_dir_all(temp_dir.path().join(".obsidian/plugins/templater-obsidian"))
            .expect("templater dir");
        fs::create_dir_all(temp_dir.path().join("Templater")).expect("templater templates dir");
        fs::write(
            temp_dir
                .path()
                .join(".obsidian/plugins/templater-obsidian/data.json"),
            r#"{"templates_folder":"Templater"}"#,
        )
        .expect("templater config should be written");
        fs::write(
            temp_dir.path().join("Templater").join("daily.md"),
            "# Templater\n",
        )
        .expect("templater template should be written");

        let result = run_template_command(
            &paths,
            None,
            true,
            None,
            TemplateEngineArg::Auto,
            &[],
            false,
            false,
        )
        .expect("template list should succeed");
        let TemplateCommandResult::List(report) = result else {
            panic!("template command should list templates");
        };

        assert_eq!(
            report.templates,
            vec![TemplateSummary {
                name: "daily.md".to_string(),
                source: "templater".to_string(),
                path: "Templater/daily.md".to_string(),
            }]
        );
        assert!(report.warnings.is_empty());
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
            TemplateEngineArg::Auto,
            &[],
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

        let rendered =
            render_note_from_parts(prepared.merged_frontmatter.as_ref(), &prepared.target_body)
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
            TemplateEngineArg::Auto,
            &[],
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
            TemplateEngineArg::Auto,
            &[],
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
            TemplateEngineArg::Auto,
            &[],
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
        let adjusted_year = year - i64::from(month <= 2);
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
        let bases_create = Cli::try_parse_from([
            "vulcan",
            "bases",
            "create",
            "release.base",
            "--title",
            "Launch Plan",
            "--dry-run",
        ])
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
                query: Some("dashboard".to_string()),
                regex: None,
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
            bases_create.command,
            Command::Bases {
                command: BasesCommand::Create {
                    file: "release.base".to_string(),
                    title: Some("Launch Plan".to_string()),
                    dry_run: true,
                    no_commit: false,
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
                render: TemplateRenderArgs {
                    engine: TemplateEngineArg::Auto,
                    vars: Vec::new(),
                },
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
                    render: TemplateRenderArgs {
                        engine: TemplateEngineArg::Auto,
                        vars: Vec::new(),
                    },
                    no_commit: false,
                }),
                name: None,
                list: false,
                path: None,
                render: TemplateRenderArgs {
                    engine: TemplateEngineArg::Auto,
                    vars: Vec::new(),
                },
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
    fn parses_query_format_and_glob_flags() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "query",
            "--format",
            "paths",
            "--glob",
            "Projects/**",
            "from notes where file.name matches \"^2026-\"",
        ])
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Command::Query {
                dsl: Some("from notes where file.name matches \"^2026-\"".to_string()),
                json: None,
                format: QueryFormatArg::Paths,
                glob: Some("Projects/**".to_string()),
                explain: false,
                export: ExportArgs::default(),
            }
        );
    }

    #[test]
    fn parses_ls_and_refactor_group_commands() {
        let ls = Cli::try_parse_from([
            "vulcan",
            "ls",
            "--where",
            "status = done",
            "--tag",
            "project",
            "--format",
            "detail",
        ])
        .expect("ls should parse");
        let refactor = Cli::try_parse_from([
            "vulcan",
            "refactor",
            "rename-property",
            "status",
            "phase",
            "--dry-run",
        ])
        .expect("refactor should parse");

        assert_eq!(
            ls.command,
            Command::Ls {
                filters: vec!["status = done".to_string()],
                glob: None,
                tag: Some("project".to_string()),
                format: QueryFormatArg::Detail,
                export: ExportArgs::default(),
            }
        );
        assert_eq!(
            refactor.command,
            Command::Refactor {
                command: RefactorCommand::RenameProperty {
                    old: "status".to_string(),
                    new: "phase".to_string(),
                    dry_run: true,
                    no_commit: false,
                },
            }
        );
    }

    #[test]
    fn parses_help_and_describe_format_commands() {
        let help = Cli::try_parse_from(["vulcan", "help", "note", "get", "--output", "json"])
            .expect("help should parse");
        let describe = Cli::try_parse_from(["vulcan", "describe", "--format", "openai-tools"])
            .expect("describe should parse");

        assert_eq!(
            help.command,
            Command::Help {
                search: None,
                topic: vec!["note".to_string(), "get".to_string()],
            }
        );
        assert_eq!(
            describe.command,
            Command::Describe {
                format: DescribeFormatArg::OpenaiTools,
            }
        );
    }

    #[test]
    fn parses_index_note_and_run_commands() {
        let index = Cli::try_parse_from(["vulcan", "index", "scan", "--full"])
            .expect("index scan should parse");
        let note_links = Cli::try_parse_from(["vulcan", "note", "links", "Dashboard"])
            .expect("note links should parse");
        let run =
            Cli::try_parse_from(["vulcan", "run", "demo", "--script"]).expect("run should parse");

        assert_eq!(
            index.command,
            Command::Index {
                command: IndexCommand::Scan {
                    full: true,
                    no_commit: false,
                },
            }
        );
        assert_eq!(
            note_links.command,
            Command::Note {
                command: NoteCommand::Links {
                    note: Some("Dashboard".to_string()),
                    export: ExportArgs::default(),
                },
            }
        );
        assert_eq!(
            run.command,
            Command::Run {
                script: Some("demo".to_string()),
                script_mode: true,
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
        let index = report
            .commands
            .iter()
            .find(|command| command.name == "index")
            .expect("index command should be described");
        assert_eq!(
            index.about.as_deref(),
            Some("Initialize, scan, rebuild, repair, watch, and serve index state")
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
        assert!(report.commands.iter().any(|command| command.name == "help"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "index"));
        assert!(report.commands.iter().any(|command| command.name == "note"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "kanban"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "refactor"));
        assert!(report
            .commands
            .iter()
            .any(|command| command.name == "config"));
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
        assert!(report.commands.iter().any(|command| command.name == "run"));
        assert!(report
            .commands
            .iter()
            .all(|command| command.name != "suggest"));
        assert!(report
            .commands
            .iter()
            .all(|command| command.name != "rewrite"));
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
