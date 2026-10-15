//! One index owner per vault.
//!
//! Several long-lived processes keep a vault's index fresh: the daemon's
//! index consumer, `vulcan serve`, MCP over HTTP, and editor language
//! servers. Each watching the whole vault and scanning every change would
//! repeat the same work once per process. The holder of an advisory OS lock
//! on `.vulcan/index-owner.lock` (beside the write lock) is the only one that
//! watches and scans; the others follow, and take over when it exits. The
//! lock, not the file's contents, decides ownership: the contents only name
//! the holder for diagnostics. An OS lock is released when its process exits,
//! so a crashed holder never leaves it stale.
//!
//! Ownership is about work, not correctness: readers still prove freshness
//! from canonical files, and a missing owner only delays indexing.

use std::fs::{File, OpenOptions};
use std::io::{Seek, Write};
use std::path::PathBuf;

use fs2::FileExt;

use crate::VaultPaths;

/// File name of the index-owner lease, beside the write lock.
pub const INDEX_OWNER_LOCK_FILE: &str = "index-owner.lock";

/// The kind of process holding the lease, recorded for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOwnerKind {
    Daemon,
    Serve,
    McpHttp,
    Editor,
    Preview,
    Watch,
}

impl IndexOwnerKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::Serve => "serve",
            Self::McpHttp => "mcp-http",
            Self::Editor => "editor",
            Self::Preview => "preview",
            Self::Watch => "watch",
        }
    }
}

/// The right to watch and index one vault. Released on drop or exit.
#[derive(Debug)]
pub struct IndexOwnerLease {
    _file: File,
}

impl IndexOwnerLease {
    /// Take the lease if no other process holds it. Returns `None` while
    /// another process holds it.
    pub fn try_acquire(
        paths: &VaultPaths,
        kind: IndexOwnerKind,
    ) -> Result<Option<Self>, std::io::Error> {
        crate::write_lock::validate_lock_directory(paths)?;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lease_path(paths)?)?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == fs2::lock_contended_error().kind() => return Ok(None),
            Err(error) => return Err(error),
        }
        // Diagnostics only; a failed write does not undo ownership.
        let _ = file
            .set_len(0)
            .and_then(|()| file.rewind())
            .and_then(|()| writeln!(file, "{} {}", kind.as_str(), std::process::id()));
        Ok(Some(Self { _file: file }))
    }
}

/// Who holds the lease, as `"<kind> <pid>"`, or `None` when nobody does.
/// Advisory: the holder may exit right after this returns.
pub fn current_index_owner(paths: &VaultPaths) -> Result<Option<String>, std::io::Error> {
    let path = lease_path(paths)?;
    let Ok(file) = File::open(&path) else {
        return Ok(None);
    };
    match FileExt::try_lock_shared(&file) {
        Ok(()) => {
            // Nobody holds it exclusively.
            let _ = FileExt::unlock(&file);
            Ok(None)
        }
        Err(error) if error.kind() == fs2::lock_contended_error().kind() => {
            let contents = std::fs::read_to_string(&path).unwrap_or_default();
            Ok(Some(contents.trim().to_string()).filter(|owner| !owner.is_empty()))
        }
        Err(error) => Err(error),
    }
}

fn lease_path(paths: &VaultPaths) -> Result<PathBuf, std::io::Error> {
    Ok(crate::write_lock::lock_file_path(paths)?.with_file_name(INDEX_OWNER_LOCK_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> (tempfile::TempDir, VaultPaths) {
        let temp = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temp.path());
        std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
        (temp, paths)
    }

    #[test]
    fn one_holder_at_a_time_and_release_on_drop() {
        let (_temp, paths) = vault();
        assert_eq!(current_index_owner(&paths).unwrap(), None);
        let lease = IndexOwnerLease::try_acquire(&paths, IndexOwnerKind::Serve)
            .unwrap()
            .expect("free lease");
        assert!(IndexOwnerLease::try_acquire(&paths, IndexOwnerKind::Daemon)
            .unwrap()
            .is_none());
        assert_eq!(
            current_index_owner(&paths).unwrap(),
            Some(format!("serve {}", std::process::id()))
        );
        drop(lease);
        assert_eq!(current_index_owner(&paths).unwrap(), None);
        let daemon = IndexOwnerLease::try_acquire(&paths, IndexOwnerKind::Daemon).unwrap();
        assert!(daemon.is_some());
        // A shorter record replaces the previous one entirely.
        assert_eq!(
            current_index_owner(&paths).unwrap(),
            Some(format!("daemon {}", std::process::id()))
        );
    }

    #[test]
    fn an_uninitialized_vault_has_no_lease() {
        let temp = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temp.path());
        assert!(IndexOwnerLease::try_acquire(&paths, IndexOwnerKind::Editor).is_err());
        assert_eq!(current_index_owner(&paths).unwrap(), None);
    }
}
