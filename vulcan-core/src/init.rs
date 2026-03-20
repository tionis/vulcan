use crate::cache::CacheError;
use crate::config::create_default_config;
use crate::{CacheDatabase, VaultPaths};
use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InitSummary {
    pub vault_root: PathBuf,
    pub config_path: PathBuf,
    pub cache_path: PathBuf,
    pub created_config: bool,
    pub created_cache: bool,
}

#[derive(Debug)]
pub enum InitError {
    Cache(CacheError),
    Io(std::io::Error),
}

impl Display for InitError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cache(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for InitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cache(error) => Some(error),
            Self::Io(error) => Some(error),
        }
    }
}

impl From<CacheError> for InitError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<std::io::Error> for InitError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn initialize_vault(paths: &VaultPaths) -> Result<InitSummary, InitError> {
    let created_cache = !paths.cache_db().exists();
    let created_config = create_default_config(paths)?;
    let _database = CacheDatabase::open(paths)?;

    Ok(InitSummary {
        vault_root: paths.vault_root().to_path_buf(),
        config_path: paths.config_file().to_path_buf(),
        cache_path: paths.cache_db().to_path_buf(),
        created_config,
        created_cache,
    })
}
