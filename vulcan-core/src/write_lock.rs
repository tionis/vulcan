use crate::paths::ensure_vulcan_dir;
use crate::VaultPaths;
use fs2::FileExt;
use std::fs::{File, OpenOptions};

#[derive(Debug)]
pub struct WriteLockGuard {
    file: File,
}

pub fn acquire_write_lock(paths: &VaultPaths) -> Result<WriteLockGuard, std::io::Error> {
    ensure_vulcan_dir(paths)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(paths.vulcan_dir().join("write.lock"))?;
    file.lock_exclusive()?;

    Ok(WriteLockGuard { file })
}

impl Drop for WriteLockGuard {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}
