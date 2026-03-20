use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum CacheError {
    Downgrade {
        database_version: u32,
        application_version: u32,
    },
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
}

impl Display for CacheError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Downgrade {
                database_version,
                application_version,
            } => write!(
                formatter,
                "database schema version {database_version} is newer than application schema version {application_version}"
            ),
            Self::Io(error) => write!(formatter, "filesystem error: {error}"),
            Self::Sqlite(error) => write!(formatter, "sqlite error: {error}"),
        }
    }
}

impl Error for CacheError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Downgrade { .. } => None,
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for CacheError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for CacheError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}
