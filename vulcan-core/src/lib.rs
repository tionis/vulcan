pub mod bases;
pub mod cache;
pub mod chunking;
pub mod config;
pub mod doctor;
pub mod dql;
pub mod expression;
mod extraction;
mod file_metadata;
pub mod git;
pub mod graph;
pub mod history;
pub mod init;
pub mod maintenance;
pub mod move_rewrite;
pub mod parser;
pub mod paths;
pub mod properties;
pub mod query;
pub mod refactor;
pub mod resolver;
pub mod saved_queries;
pub mod scan;
pub mod search;
pub mod suggestions;
pub mod tasks;
pub mod vector;
pub mod watch;
pub mod write_lock;

pub use bases::{
    bases_view_add, bases_view_delete, bases_view_edit, bases_view_rename, evaluate_base_file,
    BaseViewGroupBy, BaseViewPatch, BaseViewSpec, BasesColumn, BasesDiagnostic, BasesError,
    BasesEvalReport, BasesEvaluatedView, BasesGroupBy, BasesRow, BasesViewEditReport,
};
pub use cache::{CacheDatabase, CacheError, Migration, MigrationRegistry, BUSY_TIMEOUT_MS};
pub use config::{
    create_default_config, default_config_template, load_vault_config, AttachmentExtractionConfig,
    AutoScanMode, ChunkingConfig, ChunkingStrategy, ConfigDiagnostic, ConfigLoadResult,
    EmbeddingProviderConfig, GitConfig, GitScope, GitTrigger, InboxConfig, LinkResolutionMode,
    LinkStylePreference, ScanConfig, TemplatesConfig, VaultConfig,
};
pub use doctor::{
    doctor_fix, doctor_vault, DoctorByteRange, DoctorDiagnosticIssue, DoctorError, DoctorFixAction,
    DoctorFixReport, DoctorLinkIssue, DoctorReport, DoctorSummary,
};
pub use dql::{
    evaluate_dql, evaluate_parsed_dql, load_dataview_blocks, DataviewBlockRecord, DqlEvalError,
    DqlQueryResult,
};
pub use git::{
    auto_commit, git_log, git_status, is_git_repo, AutoCommitReport, GitError, GitLogEntry,
    GitStatusReport,
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
pub use tasks::{load_tasks_blocks, TasksBlockRecord, TasksError};
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

pub const PARSER_VERSION: u32 = 5;
pub const EXTRACTION_VERSION: u32 = 1;
pub const SCHEMA_VERSION: u32 = 13;
