//! Exclusive per-repository mutation lock shared by every sync transaction.
//!
//! All finite sync workflows (direct cycles, conflict resolution, proposals,
//! checkpoints, retention, semantic history) serialize on
//! `<git-dir>/vulcan-sync/sync.lock` so only one Vulcan mutation per
//! repository runs at a time. Centralizing acquisition here keeps the lock
//! path, open flags, and contention semantics identical everywhere; a second
//! implementation is a future divergence bug.
//!
//! `acquire` spins briefly on contention instead of failing on the first
//! `try_lock`: in this multi-threaded, child-spawning process a
//! forked-but-not-yet-exec'd child can transiently hold an inherited copy of
//! the lock fd, making a single attempt spuriously fail even when no live
//! holder exists. Genuine contention still surfaces after the budget.
//! `try_acquire` is the single-attempt primitive for latency-sensitive
//! paths; read-only probes that must not create any state (such as the sync
//! doctor) keep their own open flags instead.

use fs2::FileExt;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

const ACQUIRE_RETRIES: usize = 100;
const ACQUIRE_RETRY_DELAY: Duration = Duration::from_millis(20);

#[derive(Debug)]
pub enum RepositoryLockError {
    Locked,
    Io(io::Error),
}

impl Display for RepositoryLockError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Locked => {
                formatter.write_str("another Vulcan mutation holds the repository lock")
            }
            Self::Io(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for RepositoryLockError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Locked => None,
            Self::Io(error) => Some(error),
        }
    }
}

pub struct RepositoryLock {
    _file: File,
}

impl RepositoryLock {
    #[must_use]
    pub fn lock_path(git_dir: &Path) -> PathBuf {
        git_dir.join("vulcan-sync/sync.lock")
    }

    /// Single-attempt acquisition for read-only probes and latency-sensitive
    /// paths. Never waits.
    pub fn try_acquire(git_dir: &Path) -> Result<Self, RepositoryLockError> {
        let file = open_lock_file(git_dir)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { _file: file }),
            Err(error) => Err(classify_lock_error(error)),
        }
    }

    /// Acquisition with bounded spin for mutating transactions.
    pub fn acquire(git_dir: &Path) -> Result<Self, RepositoryLockError> {
        let file = open_lock_file(git_dir)?;
        for attempt in 0..=ACQUIRE_RETRIES {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(error) => match classify_lock_error(error) {
                    RepositoryLockError::Locked if attempt < ACQUIRE_RETRIES => {
                        std::thread::sleep(ACQUIRE_RETRY_DELAY);
                    }
                    failure => return Err(failure),
                },
            }
        }
        unreachable!("the acquisition loop returns on every path");
    }
}

fn classify_lock_error(error: io::Error) -> RepositoryLockError {
    if error.kind() == fs2::lock_contended_error().kind() {
        RepositoryLockError::Locked
    } else {
        RepositoryLockError::Io(error)
    }
}

fn open_lock_file(git_dir: &Path) -> Result<File, RepositoryLockError> {
    let path = RepositoryLock::lock_path(git_dir);
    fs::create_dir_all(
        path.parent()
            .expect("the sync lock path always has a parent"),
    )
    .map_err(RepositoryLockError::Io)?;
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(RepositoryLockError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contention_surfaces_as_locked_not_io() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let git_dir = temporary.path().join("git");
        let _held = RepositoryLock::acquire(&git_dir).expect("first acquisition");
        assert!(matches!(
            RepositoryLock::try_acquire(&git_dir),
            Err(RepositoryLockError::Locked)
        ));
    }

    #[test]
    fn sequential_acquire_release_cycles_succeed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let git_dir = temporary.path().join("git");
        for _ in 0..3 {
            drop(RepositoryLock::acquire(&git_dir).expect("acquisition"));
        }
        assert!(RepositoryLock::lock_path(&git_dir).is_file());
    }
}
