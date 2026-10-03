//! Crash-durable file operations for authoritative app state, as `AppError`
//! results over [`vulcan_core::durable`].

use crate::AppError;
use std::path::Path;
use vulcan_core::durable::{self, Durability};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableCreate {
    Created,
    AlreadyExists,
}

pub(crate) fn replace(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    durable::replace(path, bytes, Durability::Full).map_err(AppError::operation)
}

pub(crate) fn create(path: &Path, bytes: &[u8]) -> Result<DurableCreate, AppError> {
    let created =
        durable::create_new(path, bytes, Durability::Full).map_err(AppError::operation)?;
    Ok(if created {
        DurableCreate::Created
    } else {
        DurableCreate::AlreadyExists
    })
}

pub(crate) fn remove(path: &Path) -> Result<bool, AppError> {
    durable::remove(path, Durability::Full).map_err(AppError::operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn durable_create_replace_and_remove_preserve_their_contracts() {
        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("state.json");

        assert_eq!(
            create(&path, b"one\n").expect("create"),
            DurableCreate::Created
        );
        assert_eq!(
            create(&path, b"ignored\n").expect("create collision"),
            DurableCreate::AlreadyExists
        );
        assert_eq!(fs::read(&path).expect("created bytes"), b"one\n");

        replace(&path, b"two\n").expect("replace");
        assert_eq!(fs::read(&path).expect("replaced bytes"), b"two\n");

        assert!(remove(&path).expect("remove"));
        assert!(!remove(&path).expect("idempotent remove"));
    }
}
