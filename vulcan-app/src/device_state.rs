//! Vault-scoped operational state with Android-private storage and legacy reads.

use crate::{durable_file, AppError};
use std::fs;
use std::path::{Path, PathBuf};
use vulcan_core::VaultPaths;

pub(crate) fn path(paths: &VaultPaths, relative: &Path) -> Result<PathBuf, AppError> {
    Ok(paths
        .operational_state_dir()
        .map_err(AppError::operation)?
        .join(relative))
}

pub(crate) fn legacy_path(paths: &VaultPaths, relative: &Path) -> PathBuf {
    paths.vulcan_dir().join(relative)
}

pub(crate) fn readable_path(paths: &VaultPaths, relative: &Path) -> Result<PathBuf, AppError> {
    let target = path(paths, relative)?;
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(target),
        Ok(_) => Err(AppError::operation(format!(
            "operational state at {} is not a regular file",
            target.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let legacy = legacy_path(paths, relative);
            if legacy == target {
                Ok(target)
            } else {
                match fs::symlink_metadata(&legacy) {
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                        Ok(legacy)
                    }
                    Ok(_) => Err(AppError::operation(format!(
                        "legacy operational state at {} is not a regular file",
                        legacy.display()
                    ))),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(target),
                    Err(error) => Err(AppError::operation(error)),
                }
            }
        }
        Err(error) => Err(AppError::operation(error)),
    }
}

/// Called with the private workflow lock held before any external side effect.
pub(crate) fn migrate_file(paths: &VaultPaths, relative: &Path) -> Result<(), AppError> {
    let target = path(paths, relative)?;
    let legacy = legacy_path(paths, relative);
    migrate_file_paths(&legacy, &target)
}

fn migrate_file_paths(legacy: &Path, target: &Path) -> Result<(), AppError> {
    if target == legacy {
        return Ok(());
    }
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => return Ok(()),
        Ok(_) => {
            return Err(AppError::operation(format!(
                "private operational state at {} is not a regular file",
                target.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::operation(error)),
    }
    let metadata = match fs::symlink_metadata(legacy) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(AppError::operation(error)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AppError::operation(format!(
            "legacy operational state at {} is not a regular file",
            legacy.display()
        )));
    }
    let parent = target.parent().expect("operational state has a parent");
    fs::create_dir_all(parent).map_err(AppError::operation)?;
    durable_file::replace(target, &fs::read(legacy).map_err(AppError::operation)?)
}

/// Copy flat content snapshots before migrating their referencing state file.
pub(crate) fn migrate_flat_directory(paths: &VaultPaths, relative: &Path) -> Result<(), AppError> {
    let target = path(paths, relative)?;
    let legacy = legacy_path(paths, relative);
    migrate_flat_directory_paths(&legacy, &target)
}

fn migrate_flat_directory_paths(legacy: &Path, target: &Path) -> Result<(), AppError> {
    if target == legacy || !legacy.exists() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(legacy).map_err(AppError::operation)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AppError::operation(format!(
            "legacy operational state at {} is not a plain directory",
            legacy.display()
        )));
    }
    fs::create_dir_all(target).map_err(AppError::operation)?;
    let target_metadata = fs::symlink_metadata(target).map_err(AppError::operation)?;
    if !target_metadata.is_dir() || target_metadata.file_type().is_symlink() {
        return Err(AppError::operation(format!(
            "private operational state at {} is not a plain directory",
            target.display()
        )));
    }
    for entry in fs::read_dir(legacy).map_err(AppError::operation)? {
        let entry = entry.map_err(AppError::operation)?;
        let metadata = entry.metadata().map_err(AppError::operation)?;
        if !metadata.is_file() || entry.file_type().map_err(AppError::operation)?.is_symlink() {
            return Err(AppError::operation(format!(
                "legacy operational state at {} contains a non-regular entry",
                legacy.display()
            )));
        }
        let destination = target.join(entry.file_name());
        if destination.exists() {
            let existing = fs::symlink_metadata(&destination).map_err(AppError::operation)?;
            if !existing.is_file()
                || existing.file_type().is_symlink()
                || fs::read(&destination).map_err(AppError::operation)?
                    != fs::read(entry.path()).map_err(AppError::operation)?
            {
                return Err(AppError::operation(format!(
                    "private operational snapshot at {} differs from legacy state",
                    destination.display()
                )));
            }
        } else {
            durable_file::replace(
                &destination,
                &fs::read(entry.path()).map_err(AppError::operation)?,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "android"))]
    #[test]
    fn native_state_paths_keep_the_existing_vault_location() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = VaultPaths::new(temporary.path());
        let relative = Path::new("integrations/routes/example.json");
        assert_eq!(
            path(&paths, relative).expect("state path"),
            legacy_path(&paths, relative)
        );
    }

    #[test]
    fn legacy_state_migration_preserves_old_files_and_copies_snapshots_first() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let legacy = temporary.path().join("shared/pull");
        let private = temporary.path().join("private/pull");
        fs::create_dir_all(legacy.join("sources")).expect("legacy source directory");
        fs::write(legacy.join("sources/base.md"), b"base").expect("legacy snapshot");
        fs::write(legacy.join("wiki.json"), b"state").expect("legacy state");

        migrate_flat_directory_paths(&legacy.join("sources"), &private.join("sources"))
            .expect("copy snapshots");
        migrate_file_paths(&legacy.join("wiki.json"), &private.join("wiki.json"))
            .expect("copy state");
        assert_eq!(
            fs::read(private.join("wiki.json")).expect("state"),
            b"state"
        );
        assert_eq!(
            fs::read(private.join("sources/base.md")).expect("snapshot"),
            b"base"
        );
        assert!(legacy.join("wiki.json").exists());
        fs::write(private.join("sources/base.md"), b"tampered").expect("tamper private snapshot");
        assert!(
            migrate_flat_directory_paths(&legacy.join("sources"), &private.join("sources"))
                .is_err()
        );
    }
}
