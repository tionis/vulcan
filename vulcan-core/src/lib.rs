pub mod bases;
pub mod cache;
pub mod chunking;
pub mod config;
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
pub mod parser;
pub mod paths;
pub mod periodic;
pub mod properties;
pub mod query;
pub mod refactor;
pub mod resolver;
pub mod saved_queries;
pub mod scan;
pub mod search;
pub mod suggestions;
pub mod tasknotes;
pub mod tasks;
pub mod vector;
pub mod watch;
pub mod write_lock;

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
    import_periodic_notes_plugin_config, import_tasks_plugin_config,
    import_templater_plugin_config, load_vault_config, AttachmentExtractionConfig, AutoScanMode,
    ChunkingConfig, ChunkingStrategy, ConfigDiagnostic, ConfigImportError, ConfigImportMapping,
    ConfigImportReport, ConfigLoadResult, CoreImporter, DataviewConfig, DataviewImporter,
    EmbeddingProviderConfig, GitConfig, GitScope, GitTrigger, ImportConflict, ImportTarget,
    InboxConfig, KanbanConfig, KanbanImporter, LinkResolutionMode, LinkStylePreference,
    PeriodicCadenceUnit, PeriodicConfig, PeriodicNoteConfig, PeriodicNotesImporter,
    PeriodicStartOfWeek, PluginImporter, ScanConfig, TaskNotesConfig, TaskNotesFieldMapping,
    TaskNotesIdentificationMethod, TaskNotesPriorityConfig, TaskNotesStatusConfig,
    TaskNotesUserFieldConfig, TaskNotesUserFieldType, TasksImporter, TemplaterCommandPairConfig,
    TemplaterFileTemplateConfig, TemplaterFolderTemplateConfig, TemplaterImporter, TemplatesConfig,
    VaultConfig,
};
pub use dataview_js::{
    evaluate_dataview_js, evaluate_dataview_js_query, DataviewJsError, DataviewJsOutput,
    DataviewJsResult,
};
pub use doctor::{
    doctor_fix, doctor_vault, DoctorByteRange, DoctorDiagnosticIssue, DoctorError, DoctorFixAction,
    DoctorFixReport, DoctorLinkIssue, DoctorReport, DoctorSummary,
};
pub use dql::{
    evaluate_dql, evaluate_parsed_dql, load_dataview_blocks, parse_dql_with_diagnostics,
    DataviewBlockRecord, DqlDiagnostic, DqlEvalError, DqlParseOutput, DqlQueryResult,
};
pub use git::{
    auto_commit, git_blame, git_commit, git_diff, git_log, git_recent_log, git_status, is_git_repo,
    AutoCommitReport, GitBlameLine, GitCommitReport, GitError, GitLogEntry, GitStatusReport,
};
pub use graph::{
    list_note_identities, list_tagged_note_identities, list_tags, query_backlinks,
    query_graph_analytics, query_graph_components, query_graph_dead_ends, query_graph_hubs,
    query_graph_moc_candidates, query_graph_path, query_links, resolve_note_reference,
    BacklinkRecord, BacklinksReport, GraphAnalyticsReport, GraphComponent, GraphComponentsReport,
    GraphDeadEndsReport, GraphHubsReport, GraphMocCandidate, GraphMocReport, GraphNodeScore,
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
pub use parser::{
    parse_document, ChunkText, LinkKind, OriginContext, ParseDiagnostic, ParseDiagnosticKind,
    ParsedDocument, RawBlockRef, RawDataviewBlock, RawHeading, RawInlineExpression, RawInlineField,
    RawLink, RawListItem, RawTag, RawTask, RawTaskField, RawTasksBlock,
};
pub use paths::{
    VaultPaths, CACHE_DB_NAME, CONFIG_FILE_NAME, DEFAULT_ATTACHMENT_FOLDER, LOCAL_CONFIG_FILE_NAME,
    REPORTS_DIR_NAME, VULCAN_DIR_NAME,
};
pub use periodic::{
    expected_periodic_note_path, export_daily_events_to_ics, list_daily_note_events,
    list_events_between, load_events_for_periodic_note, match_periodic_note_path,
    period_range_for_date, resolve_daily_note, resolve_periodic_note, step_period_start,
    DailyNoteEvents, PeriodicError, PeriodicEvent, PeriodicEventOccurrence, PeriodicIcsExport,
    PeriodicNoteMatch,
};
pub use properties::{
    evaluate_note_inline_expressions, extract_indexed_properties, query_notes,
    EvaluatedInlineExpression, IndexedProperties, IndexedPropertyListItem, IndexedPropertyValue,
    NoteQuery, NoteRecord, NotesReport, PropertyError, PropertyTypeDiagnostic,
};
pub use query::{
    execute_query, execute_query_dsl, execute_query_json, execute_query_report, QueryAst,
    QueryError, QueryOperator, QueryPredicate, QueryProjection, QueryReport, QuerySort,
    QuerySource, QueryValue,
};
pub use refactor::{
    bulk_set_property, merge_tags, rename_alias, rename_block_ref, rename_heading, rename_property,
    set_note_property, BulkMutationReport, RefactorChange, RefactorError, RefactorFileReport,
    RefactorReport,
};
pub use resolver::{
    resolve_link, LinkResolutionProblem, LinkResolutionResult, ResolverDocument, ResolverIndex,
    ResolverLink,
};
pub use saved_queries::{
    list_saved_reports, load_saved_report, normalize_saved_report_name, report_definition_path,
    save_saved_report, SavedExport, SavedExportFormat, SavedReportDefinition, SavedReportError,
    SavedReportKind, SavedReportQuery, SavedReportSummary,
};
pub use scan::{
    detect_document_kind, scan_vault, scan_vault_with_progress, DocumentKind, ScanError, ScanMode,
    ScanPhase, ScanProgress, ScanSummary,
};
pub use search::{
    export_static_search_index, search_vault, SearchError, SearchFuzzyExpansion, SearchHit,
    SearchHitExplain, SearchPlan, SearchQuery, SearchReport, SearchSort, StaticSearchIndexEntry,
    StaticSearchIndexReport,
};
pub use suggestions::{
    bulk_replace, link_mentions, suggest_duplicates, suggest_mentions, DuplicateGroup,
    DuplicateSuggestionsReport, MentionSuggestion, MentionSuggestionsReport, MergeCandidate,
    SuggestionError,
};
pub use tasknotes::{
    extract_tasknote, is_tasknote_document, tasknotes_priority_weight, tasknotes_status_state,
    IndexedTaskNote, TaskNotesStatusState,
};
pub use tasks::{
    evaluate_parsed_tasks_query, evaluate_tasks_query, load_tasks_blocks, parse_recurrence_text,
    parse_task_recurrence, parse_tasks_query, task_recurrence_anchor, task_upcoming_occurrences,
    TaskRecurrence, TasksBlockRecord, TasksDateField, TasksDateRelation, TasksError, TasksFilter,
    TasksQuery, TasksQueryCommand, TasksQueryGroup, TasksQueryResult, TasksTextField,
};
pub use vector::{
    cluster_vectors, drop_vector_model, index_vectors, index_vectors_with_progress,
    inspect_vector_queue, list_vector_models, query_related_notes, query_vector_neighbors,
    rebuild_vectors, rebuild_vectors_with_progress, repair_vectors, repair_vectors_with_progress,
    vector_duplicates, ClusterAssignment, ClusterDocumentCount, ClusterError, ClusterQuery,
    ClusterReport, ClusterSummary, RelatedNoteHit, RelatedNotesQuery, RelatedNotesReport,
    StoredModelInfo, VectorDuplicatePair, VectorDuplicatesError, VectorDuplicatesQuery,
    VectorDuplicatesReport, VectorError, VectorIndexError, VectorIndexPhase, VectorIndexProgress,
    VectorIndexQuery, VectorIndexReport, VectorNeighborHit, VectorNeighborsQuery,
    VectorNeighborsReport, VectorQueueReport, VectorRebuildQuery, VectorRepairQuery,
    VectorRepairReport,
};
pub use watch::{watch_vault, watch_vault_until, WatchError, WatchOptions, WatchReport};

pub const PARSER_VERSION: u32 = 6;
pub const EXTRACTION_VERSION: u32 = 1;
pub const SCHEMA_VERSION: u32 = 16;
