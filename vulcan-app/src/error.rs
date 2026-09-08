use std::fmt::{Display, Formatter};
use std::io;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppError {
    message: String,
    sync_error: Option<vulcan_sync::SyncError>,
}

impl AppError {
    #[must_use]
    pub fn operation(error: impl Display) -> Self {
        Self {
            message: error.to_string(),
            sync_error: None,
        }
    }

    #[must_use]
    pub fn sync(error: vulcan_sync::SyncError) -> Self {
        Self {
            message: error.message.clone(),
            sync_error: Some(error),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn sync_error(&self) -> Option<&vulcan_sync::SyncError> {
        self.sync_error.as_ref()
    }
}

impl Display for AppError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AppError {}

impl From<io::Error> for AppError {
    fn from(error: io::Error) -> Self {
        Self::operation(error)
    }
}

impl From<vulcan_sync::RepositoryLockError> for AppError {
    fn from(error: vulcan_sync::RepositoryLockError) -> Self {
        match error {
            vulcan_sync::RepositoryLockError::Locked => {
                Self::operation("another synchronization operation holds the repository lock")
            }
            vulcan_sync::RepositoryLockError::Io(error) => Self::operation(error),
        }
    }
}
