//! Small crash-durable file primitives shared by authoritative sync state.

use crate::AppError;
use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurableCreate {
    Created,
    AlreadyExists,
}

pub(crate) fn replace(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let parent = parent(path)?;
    let temporary = synced_temporary(parent, bytes)?;
    temporary
        .persist(path)
        .map_err(|error| AppError::operation(error.error))?;
    sync_directory(parent)
}

pub(crate) fn create(path: &Path, bytes: &[u8]) -> Result<DurableCreate, AppError> {
    let parent = parent(path)?;
    let temporary = synced_temporary(parent, bytes)?;
    match temporary.persist_noclobber(path) {
        Ok(_) => {
            sync_directory(parent)?;
            Ok(DurableCreate::Created)
        }
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(DurableCreate::AlreadyExists)
        }
        Err(error) => Err(AppError::operation(error.error)),
    }
}

pub(crate) fn remove(path: &Path) -> Result<bool, AppError> {
    let parent = parent(path)?;
    match fs::remove_file(path) {
        Ok(()) => {
            sync_directory(parent)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(AppError::operation(error)),
    }
}

fn parent(path: &Path) -> Result<&Path, AppError> {
    path.parent()
        .ok_or_else(|| AppError::operation("durable file path has no parent directory"))
}

fn synced_temporary(parent: &Path, bytes: &[u8]) -> Result<NamedTempFile, AppError> {
    let mut temporary = NamedTempFile::new_in(parent).map_err(AppError::operation)?;
    temporary.write_all(bytes).map_err(AppError::operation)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AppError::operation)?;
    Ok(temporary)
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<(), AppError> {
    File::open(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(AppError::operation)
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> Result<(), AppError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
