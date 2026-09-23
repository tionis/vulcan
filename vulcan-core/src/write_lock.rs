use crate::VaultPaths;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
#[cfg(any(test, target_os = "android"))]
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug)]
pub struct WriteLockGuard {
    file: File,
}

#[derive(Debug)]
pub struct ReadLockGuard {
    file: File,
}

pub fn acquire_write_lock(paths: &VaultPaths) -> Result<WriteLockGuard, std::io::Error> {
    validate_lock_directory(paths)?;
    let path = lock_file_path(paths)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    file.lock_exclusive()?;

    Ok(WriteLockGuard { file })
}

pub fn acquire_read_lock(paths: &VaultPaths) -> Result<ReadLockGuard, std::io::Error> {
    validate_lock_directory(paths)?;
    let path = lock_file_path(paths)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    fs2::FileExt::lock_shared(&file)?;

    Ok(ReadLockGuard { file })
}

#[allow(clippy::unnecessary_wraps)] // Android path resolution and directory creation are fallible.
fn lock_file_path(paths: &VaultPaths) -> Result<PathBuf, std::io::Error> {
    #[cfg(target_os = "android")]
    {
        let state_root = crate::vulcan_user_state_dir().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "cannot determine the private Vulcan state directory for the Android vault lock",
            )
        })?;
        return private_lock_file_path(paths, &state_root);
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(paths.vulcan_dir().join("write.lock"))
    }
}

#[cfg(any(test, target_os = "android"))]
fn private_lock_file_path(
    paths: &VaultPaths,
    state_root: &Path,
) -> Result<PathBuf, std::io::Error> {
    let canonical = fs::canonicalize(paths.vault_root())?;
    let key = blake3::hash(canonical.to_string_lossy().as_bytes());
    let directory = state_root.join("locks").join(&key.to_hex()[..32]);
    fs::create_dir_all(&directory)?;
    Ok(directory.join("write.lock"))
}

fn validate_lock_directory(paths: &VaultPaths) -> Result<(), std::io::Error> {
    let metadata = fs::symlink_metadata(paths.vulcan_dir()).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "missing {}. Run `vulcan init` in {} first",
                    paths.vulcan_dir().display(),
                    paths.vault_root().display()
                ),
            )
        } else {
            error
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "expected {} to be a directory. Run `vulcan init` after fixing it",
                paths.vulcan_dir().display()
            ),
        ));
    }
    Ok(())
}

impl Drop for WriteLockGuard {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

impl Drop for ReadLockGuard {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquiring_a_lock_does_not_scaffold_or_change_vault_inputs() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir(paths.vulcan_dir()).expect("minimal coordination directory");

        let guard = acquire_write_lock(&paths).expect("write lock");

        assert!(paths.vulcan_dir().join("write.lock").is_file());
        assert!(!paths.gitignore_file().exists());
        assert!(!paths.reports_dir().exists());
        drop(guard);
    }

    #[test]
    fn private_lock_path_is_stable_for_equivalent_vault_paths() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).expect("vault");
        let state = temporary.path().join("private-state");
        let direct = private_lock_file_path(&VaultPaths::new(&vault), &state).expect("lock path");
        fs::create_dir(vault.join("subdir")).expect("subdir");
        let equivalent = private_lock_file_path(
            &VaultPaths::new(vault.join(".").join("subdir").join("..")),
            &state,
        )
        .expect("equivalent lock path");
        assert_eq!(direct, equivalent);
        assert!(direct.starts_with(state));
        assert!(!vault.join(".vulcan/write.lock").exists());
    }
}
