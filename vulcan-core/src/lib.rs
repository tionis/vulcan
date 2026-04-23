//! Command-agnostic vault semantics and shared backend logic for Vulcan.
//!
//! `vulcan-core` owns parsing, indexing, cache abstractions, config models,
//! query/evaluation logic, and reusable domain types that can be shared across
//! CLI, daemon, assistant, and other entrypoints.

pub mod assistant;
pub mod bases;
pub mod cache;
pub mod chunking;
pub mod config;
pub mod content_transforms;
pub mod dataview_js;
pub mod doctor;
pub mod dql;
pub mod expression;
mod extraction;
mod file_metadata;
pub mod git;
pub mod graph;
pub mod history;
pub mod init;
pub mod kanban;
pub mod maintenance;
pub mod move_rewrite;
pub mod note;
pub mod parser;
pub mod paths;
pub mod periodic;
pub mod permissions;
pub mod properties;
pub mod query;
pub mod refactor;
pub mod render;
pub mod resolver;
pub mod saved_queries;
pub mod scan;
pub mod search;
pub mod suggestions;
pub mod tasknotes;
pub mod tasks;
pub mod vector;
pub mod watch;
pub mod web;
pub mod write_lock;

pub use assistant::{
    assistant_config_summary, assistant_prompts_root, assistant_skills_root, assistant_tools_root,
    default_assistant_tool_reserved_names, list_assistant_prompts, list_assistant_skills,
    list_assistant_tools, load_assistant_prompt, load_assistant_skill, load_assistant_tool,
    read_vault_agents_file, render_assistant_prompt, AssistantConfigSummary, AssistantError,
    AssistantPrompt, AssistantPromptArgument, AssistantPromptSummary, AssistantSkill,
    AssistantSkillSummary, AssistantTool, AssistantToolRuntime, AssistantToolSummary,
    AssistantToolValidationOptions,
};
pub use bases::{
    bases_view_add, bases_view_delete, bases_view_edit, bases_view_rename, evaluate_base_file,
    inspect_base_file, plan_base_note_create, BaseViewGroupBy, BaseViewPatch, BaseViewSpec,
    BasesColumn, BasesCreateContext, BasesDiagnostic, BasesError, BasesEvalReport,
    BasesEvaluatedView, BasesEvaluator, BasesFileInfo, BasesFileViewInfo, BasesGroupBy, BasesRow,
    BasesSource, BasesSourceRequest, BasesViewEditReport, FileSource, TaskNotesSource,
};
pub use cache::{CacheDatabase, CacheError, Migration, MigrationRegistry, BUSY_TIMEOUT_MS};
pub use config::{
    all_importers, annotate_import_conflicts, create_default_config, default_config_template,
    import_core_plugin_config, import_dataview_plugin_config, import_kanban_plugin_config,
    import_periodic_notes_plugin_config, import_tasknotes_plugin_config,
    import_tasks_plugin_config, import_templater_plugin_config, load_permission_profiles,
    load_vault_config, validate_vulcan_overrides_toml, AssistantConfig, AttachmentExtractionConfig,
    AutoScanMode, ChunkingConfig, ChunkingStrategy, ConfigDiagnostic, ConfigImportError,
    ConfigImportMapping, ConfigImportReport, ConfigLoadResult, ConfigPermissionMode,
    ContentTransformConfig, ContentTransformRuleConfig, CoreImporter, DataviewConfig,
    DataviewImporter, EmbeddingProviderConfig, GitConfig, GitScope, GitTrigger, ImportConflict,
    ImportMigratedFile, ImportMigratedFileAction, ImportSkippedSetting, ImportTarget, InboxConfig,
    JsRuntimeConfig, JsRuntimeSandbox, KanbanConfig, KanbanImporter, LinkResolutionMode,
    LinkStylePreference, NetworkPermissionConfig, PathPermissionConfig, PathPermissionKeyword,
    PathPermissionRules, PeriodicCadenceUnit, PeriodicConfig, PeriodicNoteConfig,
    PeriodicNotesImporter, PeriodicStartOfWeek, PermissionLimit, PermissionLimitKeyword,
    PermissionMode, PermissionProfile, PermissionProfilesLoadResult, PermissionsConfig,
    PluginEvent, PluginImporter, PluginRegistration, ScanConfig, SearchBackendKind,
    TaskNotesConfig, TaskNotesDateDefault, TaskNotesFieldMapping, TaskNotesIdentificationMethod,
    TaskNotesImporter, TaskNotesNlpTriggerConfig, TaskNotesPriorityConfig,
    TaskNotesRecurrenceDefault, TaskNotesSavedViewCondition, TaskNotesSavedViewConfig,
    TaskNotesSavedViewFilterValue, TaskNotesSavedViewGroup, TaskNotesSavedViewNode,
    TaskNotesSavedViewOptionValue, TaskNotesSavedViewQuery, TaskNotesStatusConfig,
    TaskNotesTaskCreationDefaults, TaskNotesUserFieldConfig, TaskNotesUserFieldType, TasksImporter,
    TemplaterCommandPairConfig, TemplaterFileTemplateConfig, TemplaterFolderTemplateConfig,
    TemplaterImporter, TemplatesConfig, VaultConfig,
};
pub use content_transforms::apply_content_transforms;
pub use dataview_js::{
    evaluate_dataview_js, evaluate_dataview_js_query, evaluate_dataview_js_with_options,
    DataviewJsError, DataviewJsEvalOptions, DataviewJsOutput, DataviewJsResult, DataviewJsSession,
};
pub use doctor::{
    doctor_fix, doctor_vault, DoctorByteRange, DoctorDiagnosticIssue, DoctorError, DoctorFixAction,
    DoctorFixReport, DoctorLinkIssue, DoctorReport, DoctorSummary,
};
pub use dql::{
    evaluate_dql, evaluate_dql_with_filter, evaluate_parsed_dql, evaluate_parsed_dql_with_filter,
    load_dataview_blocks, parse_dql_with_diagnostics, DataviewBlockRecord, DqlDiagnostic,
    DqlEvalError, DqlParseOutput, DqlQueryResult,
};
pub use git::{
    auto_commit, git_blame, git_commit, git_diff, git_log, git_recent_log, git_status, is_git_repo,
    AutoCommitReport, GitBlameLine, GitCommitReport, GitError, GitLogEntry, GitStatusReport,
};
pub use graph::{
    export_graph, export_graph_with_filter, list_note_identities, list_note_identities_with_filter,
    list_tagged_note_identities, list_tagged_note_identities_with_filter, list_tags,
    list_tags_with_filter, query_backlinks, query_backlinks_with_filter, query_graph_analytics,
    query_graph_analytics_with_filter, query_graph_components, query_graph_components_with_filter,
    query_graph_dead_ends, query_graph_dead_ends_with_filter, query_graph_hubs,
    query_graph_hubs_with_filter, query_graph_moc_candidates,
    query_graph_moc_candidates_with_filter, query_graph_path, query_graph_path_with_filter,
    query_links, query_links_with_filter, resolve_note_reference,
    resolve_note_reference_with_filter, BacklinkRecord, BacklinksReport, GraphAnalyticsReport,
    GraphComponent, GraphComponentsReport, GraphDeadEndsReport, GraphExportEdge, GraphExportNode,
    GraphExportReport, GraphHubsReport, GraphMocCandidate, GraphMocReport, GraphNodeScore,
    GraphPathReport, GraphQueryError, LineContext, NamedCount, NoteIdentity, NoteMatchKind,
    NoteReference, OutgoingLinkRecord, OutgoingLinksReport, ResolutionStatus,
};
pub use history::{
    create_checkpoint, list_checkpoints, query_change_report, query_graph_trends, ChangeAnchor,
    ChangeItem, ChangeKind, ChangeReport, ChangeStatus, CheckpointError, CheckpointRecord,
    GraphTrendPoint, GraphTrendsReport,
};
pub use init::{initialize_vault, InitError, InitSummary};
pub use kanban::{
    add_kanban_card, archive_kanban_card, list_kanban_boards, load_kanban_board, move_kanban_card,
    KanbanAddReport, KanbanArchiveReport, KanbanBoardRecord, KanbanBoardSummary, KanbanCardRecord,
    KanbanColumnRecord, KanbanError, KanbanMoveReport, KanbanTaskStatus,
};
pub use maintenance::{
    cache_vacuum, inspect_cache, rebuild_vault, rebuild_vault_with_progress, repair_fts,
    verify_cache, CacheInspectReport, CacheVacuumQuery, CacheVacuumReport, CacheVerifyCheck,
    CacheVerifyReport, MaintenanceError, RebuildQuery, RebuildReport, RepairFtsQuery,
    RepairFtsReport,
};
pub use move_rewrite::{move_note, LinkChange, MoveError, MoveSummary, RewrittenFile};
pub use note::{
    byte_range_for_line_span, locate_note_range, outline_note, read_note, select_note_outline,
    NoteLineSpan, NoteLocatedRange, NoteOutline, NoteOutlineBlockRef, NoteOutlineOptions,
    NoteOutlineSection, NoteOutlineSelection, NoteReadOptions, NoteReadSelection, NoteSelectedLine,
    NoteSelectionError,
};
pub use parser::{
    parse_document, ChunkText, LinkKind, OriginContext, ParseDiagnostic, ParseDiagnosticKind,
    ParsedDocument, RawBlockRef, RawDataviewBlock, RawHeading, RawInlineExpression, RawInlineField,
    RawLink, RawListItem, RawTag, RawTask, RawTaskField, RawTasksBlock,
};
pub use paths::{
    ensure_vulcan_dir, initialize_vulcan_dir, VaultPaths, CACHE_DB_NAME, CONFIG_FILE_NAME,
    DEFAULT_ATTACHMENT_FOLDER, LOCAL_CONFIG_FILE_NAME, REPORTS_DIR_NAME, VULCAN_DIR_NAME,
};
pub use periodic::{
    expected_periodic_note_path, export_daily_events_to_ics, list_daily_note_events,
    list_events_between, load_events_for_periodic_note, match_periodic_note_path,
    period_range_for_date, resolve_daily_note, resolve_periodic_note, step_period_start,
    DailyNoteEvents, PeriodicError, PeriodicEvent, PeriodicEventOccurrence, PeriodicIcsExport,
    PeriodicNoteMatch,
};
pub use permissions::{
    combine_cte_fragments, resolve_permission_profile, PathPermission, PermissionError,
    PermissionFilter, PermissionGrant, PermissionGuard, PermissionSql, ProfilePermissionGuard,
    ResolvedPermissionProfile, ResourceLimits, ResourceSpecifier,
};
pub use properties::{
    evaluate_note_inline_expressions, extract_indexed_properties, list_properties,
    list_query_fields, query_notes, query_notes_with_filter, EvaluatedInlineExpression,
    IndexedProperties, IndexedPropertyListItem, IndexedPropertyValue, NoteQuery, NoteRecord,
    NotesReport, PropertyCatalogEntry, PropertyError, PropertyTypeDiagnostic,
    QueryFieldCatalogEntry,
};
pub use query::{
    execute_query, execute_query_dsl, execute_query_json, execute_query_report,
    execute_query_report_with_filter, execute_query_with_filter, QueryAst, QueryError,
    QueryOperator, QueryPredicate, QueryProjection, QueryReport, QuerySort, QuerySource,
    QueryValue,
};
pub use refactor::{
    bulk_set_property, bulk_set_property_on_paths, merge_tags, rename_alias, rename_block_ref,
    rename_heading, rename_property, set_note_property, BulkMutationReport, RefactorChange,
    RefactorError, RefactorFileReport, RefactorReport,
};
pub use render::{render_markdown_fragment_html, render_markdown_html};
pub use resolver::{
    resolve_link, LinkResolutionProblem, LinkResolutionResult, ResolverDocument, ResolverIndex,
    ResolverLink,
};
pub use saved_queries::{
    delete_saved_report, list_saved_reports, load_saved_report, normalize_saved_report_name,
    report_definition_path, save_saved_report, SavedExport, SavedExportFormat,
    SavedReportDefinition, SavedReportError, SavedReportKind, SavedReportQuery, SavedReportSummary,
};
pub use scan::{
    detect_document_kind, scan_vault, scan_vault_with_progress, DocumentKind, ScanError, ScanMode,
    ScanPhase, ScanProgress, ScanSummary,
};
pub use search::{
    export_static_search_index, search_vault, search_vault_with_filter, SearchError,
    SearchFuzzyExpansion, SearchHit, SearchHitExplain, SearchPlan, SearchQuery, SearchReport,
    SearchSort, StaticSearchIndexEntry, StaticSearchIndexReport,
};
pub use suggestions::{
    bulk_replace, bulk_replace_on_paths, link_mentions, suggest_duplicates, suggest_mentions,
    DuplicateGroup, DuplicateSuggestionsReport, MentionSuggestion, MentionSuggestionsReport,
    MergeCandidate, SuggestionError,
};
pub use tasknotes::{
    active_tasknote_time_entry, extract_tasknote, is_tasknote_document, parse_iso8601_duration_ms,
    parse_tasknote_natural_language, parse_tasknote_reminders, parse_tasknote_time_entries,
    tasknotes_default_date_value, tasknotes_default_recurrence_rule,
    tasknotes_default_reminder_values, tasknotes_priority_weight, tasknotes_reminder_notify_at,
    tasknotes_status_definition, tasknotes_status_state, tasknotes_total_time_minutes,
    IndexedTaskNote, ParsedTaskNoteInput, TaskNotesReminder, TaskNotesStatusState,
    TaskNotesTimeEntry,
};
pub use tasks::{
    evaluate_parsed_tasks_query, evaluate_tasks_query, load_tasks_blocks, parse_recurrence_text,
    parse_task_recurrence, parse_tasks_query, shape_tasks_query_result, task_recurrence_anchor,
    task_upcoming_occurrences, TaskRecurrence, TasksBlockRecord, TasksDateField, TasksDateRelation,
    TasksError, TasksFilter, TasksQuery, TasksQueryCommand, TasksQueryGroup, TasksQueryResult,
    TasksTextField,
};
pub use vector::{
    cluster_vectors, cluster_vectors_with_filter, drop_vector_model, index_vectors,
    index_vectors_with_progress, inspect_vector_queue, list_vector_models, query_related_notes,
    query_related_notes_with_filter, query_vector_neighbors, query_vector_neighbors_with_filter,
    rebuild_vectors, rebuild_vectors_with_progress, repair_vectors, repair_vectors_with_progress,
    vector_duplicates, vector_duplicates_with_progress, ClusterAssignment, ClusterDocumentCount,
    ClusterError, ClusterQuery, ClusterReport, ClusterSummary, RelatedNoteHit, RelatedNotesQuery,
    RelatedNotesReport, StoredModelInfo, VectorDuplicatePair, VectorDuplicatesError,
    VectorDuplicatesQuery, VectorDuplicatesReport, VectorError, VectorIndexError, VectorIndexPhase,
    VectorIndexProgress, VectorIndexQuery, VectorIndexReport, VectorNeighborHit,
    VectorNeighborsQuery, VectorNeighborsReport, VectorQueueReport, VectorRebuildQuery,
    VectorRepairQuery, VectorRepairReport,
};
pub use watch::{watch_vault, watch_vault_until, WatchError, WatchOptions, WatchReport};
pub use web::{
    fetch_web, fetch_web_content, html_to_markdown as convert_web_html_to_markdown,
    prepare_search_backend, search_web, FetchedWebContent, PreparedWebSearchBackend,
    WebFetchReport, WebSearchReport, WebSearchResult,
};

const FIXED_NOW_ENV: &str = "VULCAN_FIXED_NOW";

#[must_use]
pub fn current_utc_timestamp_ms() -> i64 {
    current_time_override_ms().unwrap_or_else(|| {
        let duration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        i64::try_from(duration.as_millis()).unwrap_or_default()
    })
}

#[must_use]
pub fn current_time_override_ms() -> Option<i64> {
    let value = std::env::var(FIXED_NOW_ENV).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    expression::functions::parse_date_like_string(trimmed).or_else(|| trimmed.parse::<i64>().ok())
}

pub const PARSER_VERSION: u32 = 7;
pub const EXTRACTION_VERSION: u32 = 1;
pub const SCHEMA_VERSION: u32 = 16;
