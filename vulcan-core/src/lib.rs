pub mod cache;
pub mod config;
pub mod init;
pub mod paths;
pub mod scan;

pub use cache::{CacheDatabase, CacheError, Migration, MigrationRegistry, BUSY_TIMEOUT_MS};
pub use config::{
    create_default_config, default_config_template, load_vault_config, ChunkingConfig,
    ChunkingStrategy, ConfigDiagnostic, ConfigLoadResult, EmbeddingProviderConfig,
    LinkResolutionMode, LinkStylePreference, VaultConfig,
};
pub use init::{initialize_vault, InitError, InitSummary};
pub use paths::{
    VaultPaths, CACHE_DB_NAME, CONFIG_FILE_NAME, DEFAULT_ATTACHMENT_FOLDER, VULCAN_DIR_NAME,
};
pub use scan::{detect_document_kind, scan_vault, DocumentKind, ScanError, ScanMode, ScanSummary};

pub const PARSER_VERSION: u32 = 1;
pub const EXTRACTION_VERSION: u32 = 1;
pub const SCHEMA_VERSION: u32 = 1;
