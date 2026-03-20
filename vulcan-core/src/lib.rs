pub mod cache;
pub mod paths;

pub use cache::{CacheDatabase, CacheError, Migration, MigrationRegistry, BUSY_TIMEOUT_MS};
pub use paths::{
    VaultPaths, CACHE_DB_NAME, CONFIG_FILE_NAME, DEFAULT_ATTACHMENT_FOLDER, VULCAN_DIR_NAME,
};

pub const PARSER_VERSION: u32 = 1;
pub const EXTRACTION_VERSION: u32 = 1;
pub const SCHEMA_VERSION: u32 = 1;
