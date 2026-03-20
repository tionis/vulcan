use crate::VaultPaths;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};

#[derive(Debug)]
pub struct WriteLockGuard {
    file: File,
}

pub fn acquire_write_lock(paths: &VaultPaths) -> Result<WriteLockGuard, std::io::Error> {
    fs::create_dir_all(paths.vulcan_dir())?;
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
