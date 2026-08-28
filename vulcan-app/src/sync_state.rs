//! Durable device-local synchronization state.
//!
//! This state is operational and authoritative for crash recovery, so it lives
//! below the platform user-state directory rather than in the rebuildable
//! per-vault cache or the synchronized worktree.

use crate::AppError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use ulid::Ulid;

pub const SYNC_JOURNAL_VERSION: u32 = 1;
const MAX_SYNC_JOURNAL_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncJournalPhase {
    Preparing,
    Capturing,
    Fetching,
    Merging,
    Pushing,
    Applying,
    Verifying,
    Conflicted,
    Paused,
    Error,
}

impl SyncJournalPhase {
    #[must_use]
    pub const fn requires_recovery(self) -> bool {
        matches!(
            self,
            Self::Preparing
                | Self::Capturing
                | Self::Fetching
                | Self::Merging
                | Self::Pushing
                | Self::Applying
                | Self::Verifying
                | Self::Error
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncJournal {
    pub version: u32,
    pub transaction_id: Ulid,
    pub repository_key: String,
    pub work_tree: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_dir: Option<PathBuf>,
    pub phase: SyncJournalPhase,
    pub remote: String,
    pub live_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_worktree_tree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_snapshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl SyncJournal {
    pub fn preparing(
        work_tree: &Path,
        remote: impl Into<String>,
        live_ref: impl Into<String>,
    ) -> Result<Self, AppError> {
        let work_tree = fs::canonicalize(work_tree).map_err(AppError::operation)?;
        Ok(Self {
            version: SYNC_JOURNAL_VERSION,
            transaction_id: Ulid::new(),
            repository_key: repository_state_key(&work_tree),
            work_tree,
            git_dir: None,
            phase: SyncJournalPhase::Preparing,
            remote: remote.into(),
            live_ref: live_ref.into(),
            expected_worktree_tree: None,
            local_snapshot: None,
            accepted: None,
            error: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStateStore {
    root: PathBuf,
}

impl SyncStateStore {
    pub fn user_default() -> Result<Self, AppError> {
        let root = vulcan_core::vulcan_user_state_dir().ok_or_else(|| {
            AppError::operation(
                "cannot determine the Vulcan user state directory; set XDG_STATE_HOME or HOME",
            )
        })?;
        Ok(Self::at(root.join("sync/repositories")))
    }

    #[must_use]
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn journal_path(&self, repository_key: &str) -> Result<PathBuf, AppError> {
        validate_repository_key(repository_key)?;
        Ok(self.root.join(repository_key).join("transaction.json"))
    }

    pub fn load(&self, repository_key: &str) -> Result<Option<SyncJournal>, AppError> {
        let path = self.journal_path(repository_key)?;
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AppError::operation(error)),
        };
        if metadata.len() > MAX_SYNC_JOURNAL_BYTES {
            return Err(AppError::operation(format!(
                "sync journal at {} exceeds the {} byte limit",
                path.display(),
                MAX_SYNC_JOURNAL_BYTES
            )));
        }
        let source = fs::read(&path).map_err(AppError::operation)?;
        let journal: SyncJournal = serde_json::from_slice(&source).map_err(AppError::operation)?;
        if journal.version != SYNC_JOURNAL_VERSION {
            return Err(AppError::operation(format!(
                "unsupported sync journal version {} at {}",
                journal.version,
                path.display()
            )));
        }
        if journal.repository_key != repository_key {
            return Err(AppError::operation(format!(
                "sync journal repository key mismatch at {}",
                path.display()
            )));
        }
        if repository_state_key(&journal.work_tree) != repository_key {
            return Err(AppError::operation(format!(
                "sync journal worktree identity mismatch at {}",
                path.display()
            )));
        }
        Ok(Some(journal))
    }

    pub fn save(&self, journal: &SyncJournal) -> Result<(), AppError> {
        if journal.version != SYNC_JOURNAL_VERSION {
            return Err(AppError::operation(format!(
                "cannot write unsupported sync journal version {}",
                journal.version
            )));
        }
        let path = self.journal_path(&journal.repository_key)?;
        let parent = path
            .parent()
            .ok_or_else(|| AppError::operation("sync journal path has no parent directory"))?;
        fs::create_dir_all(parent).map_err(AppError::operation)?;
        let bytes = serde_json::to_vec_pretty(journal).map_err(AppError::operation)?;
        let mut temporary = NamedTempFile::new_in(parent).map_err(AppError::operation)?;
        temporary.write_all(&bytes).map_err(AppError::operation)?;
        temporary.write_all(b"\n").map_err(AppError::operation)?;
        temporary
            .as_file()
            .sync_all()
            .map_err(AppError::operation)?;
        temporary
            .persist(&path)
            .map_err(|error| AppError::operation(error.error))?;
        Ok(())
    }

    pub fn clear(&self, repository_key: &str) -> Result<(), AppError> {
        let path = self.journal_path(repository_key)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(AppError::operation(error)),
        }
    }
}

#[must_use]
pub fn repository_state_key(work_tree: &Path) -> String {
    let normalized = work_tree.to_string_lossy();
    blake3::hash(normalized.as_bytes()).to_hex()[..32].to_string()
}

fn validate_repository_key(repository_key: &str) -> Result<(), AppError> {
    if repository_key.len() == 32
        && repository_key
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(AppError::operation(format!(
            "invalid sync repository state key `{repository_key}`"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn journal_round_trips_atomically_outside_the_vault() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).expect("vault directory");
        let store = SyncStateStore::at(temporary.path().join("state"));
        let mut journal =
            SyncJournal::preparing(&vault, "origin", "refs/heads/live").expect("journal");
        journal.phase = SyncJournalPhase::Applying;
        journal.accepted = Some("a".repeat(40));

        store.save(&journal).expect("save journal");
        assert_eq!(
            store.load(&journal.repository_key).expect("load journal"),
            Some(journal.clone())
        );
        assert!(!vault.join(".vulcan/cache.db").exists());

        store.clear(&journal.repository_key).expect("clear journal");
        assert_eq!(
            store.load(&journal.repository_key).expect("load cleared"),
            None
        );
    }

    #[test]
    fn journal_rejects_traversal_keys_versions_and_mismatches() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).expect("vault directory");
        let store = SyncStateStore::at(temporary.path().join("state"));
        assert!(store.journal_path("../outside").is_err());

        let mut journal = SyncJournal::preparing(&vault, "origin", "refs/heads/live")
            .expect("journal should be created");
        journal.version += 1;
        assert!(store.save(&journal).is_err());

        journal.version = SYNC_JOURNAL_VERSION;
        store.save(&journal).expect("valid journal");
        let path = store
            .journal_path(&journal.repository_key)
            .expect("journal path");
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("journal source"))
                .expect("journal JSON");
        value["repository_key"] = serde_json::Value::String("b".repeat(32));
        fs::write(&path, serde_json::to_vec(&value).expect("JSON")).expect("tamper journal");
        assert!(store.load(&journal.repository_key).is_err());
    }

    #[test]
    fn journal_phases_identify_interruption_sensitive_states() {
        assert!(SyncJournalPhase::Applying.requires_recovery());
        assert!(SyncJournalPhase::Error.requires_recovery());
        assert!(!SyncJournalPhase::Paused.requires_recovery());
        assert!(!SyncJournalPhase::Conflicted.requires_recovery());
    }
}
