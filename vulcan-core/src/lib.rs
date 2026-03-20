pub mod bases;
pub mod cache;
pub mod chunking;
pub mod config;
pub mod doctor;
pub mod graph;
pub mod init;
pub mod maintenance;
pub mod move_rewrite;
pub mod parser;
pub mod paths;
pub mod properties;
pub mod refactor;
pub mod resolver;
pub mod scan;
pub mod search;
pub mod vector;
pub mod watch;
pub mod write_lock;

pub use bases::{
    evaluate_base_file, BasesDiagnostic, BasesError, BasesEvalReport, BasesEvaluatedView, BasesRow,
};
pub use cache::{CacheDatabase, CacheError, Migration, MigrationRegistry, BUSY_TIMEOUT_MS};
pub use config::{
    create_default_config, default_config_template, load_vault_config, ChunkingConfig,
    ChunkingStrategy, ConfigDiagnostic, ConfigLoadResult, EmbeddingProviderConfig,
    LinkResolutionMode, LinkStylePreference, VaultConfig,
};
pub use doctor::{
    doctor_fix, doctor_vault, DoctorByteRange, DoctorDiagnosticIssue, DoctorError, DoctorFixAction,
    DoctorFixReport, DoctorLinkIssue, DoctorReport, DoctorSummary,
};
pub use graph::{
    query_backlinks, query_links, resolve_note_reference, BacklinkRecord, BacklinksReport,
    GraphQueryError, LineContext, NoteMatchKind, NoteReference, OutgoingLinkRecord,
    OutgoingLinksReport, ResolutionStatus,
};
pub use init::{initialize_vault, InitError, InitSummary};
pub use maintenance::{
    rebuild_vault, rebuild_vault_with_progress, repair_fts, MaintenanceError, RebuildQuery,
    RebuildReport, RepairFtsQuery, RepairFtsReport,
};
pub use move_rewrite::{move_note, LinkChange, MoveError, MoveSummary, RewrittenFile};
pub use parser::{
    parse_document, ChunkText, LinkKind, OriginContext, ParseDiagnostic, ParseDiagnosticKind,
    ParsedDocument, RawBlockRef, RawHeading, RawLink, RawTag,
};
pub use paths::{
    VaultPaths, CACHE_DB_NAME, CONFIG_FILE_NAME, DEFAULT_ATTACHMENT_FOLDER, VULCAN_DIR_NAME,
};
pub use properties::{
    extract_indexed_properties, query_notes, IndexedProperties, IndexedPropertyListItem,
    IndexedPropertyValue, NoteQuery, NoteRecord, NotesReport, PropertyError,
    PropertyTypeDiagnostic,
};
pub use refactor::{
    merge_tags, rename_alias, rename_block_ref, rename_heading, rename_property, RefactorChange,
    RefactorError, RefactorFileReport, RefactorReport,
};
pub use resolver::{
    resolve_link, LinkResolutionProblem, LinkResolutionResult, ResolverDocument, ResolverLink,
};
pub use scan::{
    detect_document_kind, scan_vault, scan_vault_with_progress, DocumentKind, ScanError, ScanMode,
    ScanPhase, ScanProgress, ScanSummary,
};
pub use search::{search_vault, SearchError, SearchHit, SearchQuery, SearchReport};
pub use vector::{
    cluster_vectors, index_vectors, index_vectors_with_progress, query_vector_neighbors,
    vector_duplicates, ClusterAssignment, ClusterError, ClusterQuery, ClusterReport,
    VectorDuplicatePair, VectorDuplicatesError, VectorDuplicatesQuery, VectorDuplicatesReport,
    VectorError, VectorIndexError, VectorIndexPhase, VectorIndexProgress, VectorIndexQuery,
    VectorIndexReport, VectorNeighborHit, VectorNeighborsQuery, VectorNeighborsReport,
};
pub use watch::{watch_vault, WatchError, WatchOptions, WatchReport};

pub const PARSER_VERSION: u32 = 2;
pub const EXTRACTION_VERSION: u32 = 1;
pub const SCHEMA_VERSION: u32 = 6;
