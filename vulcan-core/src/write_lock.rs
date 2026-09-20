use crate::VaultPaths;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};

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
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(paths.vulcan_dir().join("write.lock"))?;
    file.lock_exclusive()?;

    Ok(WriteLockGuard { file })
}

pub fn acquire_read_lock(paths: &VaultPaths) -> Result<ReadLockGuard, std::io::Error> {
    validate_lock_directory(paths)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(paths.vulcan_dir().join("write.lock"))?;
    fs2::FileExt::lock_shared(&file)?;

    Ok(ReadLockGuard { file })
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
}
