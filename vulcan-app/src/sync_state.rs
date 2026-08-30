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
use vulcan_sync::GitSyncDeviceId;

pub const SYNC_JOURNAL_VERSION: u32 = 1;
const MAX_SYNC_JOURNAL_BYTES: u64 = 1024 * 1024;
const SYNC_DEVICE_IDENTITY_VERSION: u32 = 1;
pub const SYNC_APPLY_MARKER_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SyncDeviceIdentity {
    version: u32,
    device_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncJournalPhase {
    Preparing,
    Capturing,
    Captured,
    Fetching,
    Fetched,
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
                | Self::Captured
                | Self::Fetching
                | Self::Fetched
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncApplyMarker {
    pub version: u32,
    pub transaction_id: Ulid,
    pub repository_key: String,
    pub expected_revision: String,
    pub accepted: String,
}

impl SyncApplyMarker {
    pub fn from_journal(journal: &SyncJournal) -> Result<Self, AppError> {
        let expected_revision = journal
            .local_snapshot
            .clone()
            .ok_or_else(|| AppError::operation("applying journal has no local snapshot"))?;
        let accepted = journal
            .accepted
            .clone()
            .ok_or_else(|| AppError::operation("applying journal has no accepted revision"))?;
        Ok(Self {
            version: SYNC_APPLY_MARKER_VERSION,
            transaction_id: journal.transaction_id,
            repository_key: journal.repository_key.clone(),
            expected_revision,
            accepted,
        })
    }
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

    pub fn load_or_create_device_id(
        &self,
        create: bool,
    ) -> Result<Option<GitSyncDeviceId>, AppError> {
        let path = self.root.join("_device.json");
        match fs::read(&path) {
            Ok(source) => return parse_device_identity(&path, &source).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => {
                return Ok(None);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(AppError::operation(error)),
        }
        fs::create_dir_all(&self.root).map_err(AppError::operation)?;
        let identity = SyncDeviceIdentity {
            version: SYNC_DEVICE_IDENTITY_VERSION,
            device_id: Ulid::new().to_string().to_ascii_lowercase(),
        };
        let bytes = serde_json::to_vec_pretty(&identity).map_err(AppError::operation)?;
        let mut temporary = NamedTempFile::new_in(&self.root).map_err(AppError::operation)?;
        temporary.write_all(&bytes).map_err(AppError::operation)?;
        temporary.write_all(b"\n").map_err(AppError::operation)?;
        temporary
            .as_file()
            .sync_all()
            .map_err(AppError::operation)?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => GitSyncDeviceId::parse(identity.device_id)
                .map(Some)
                .map_err(AppError::operation),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                parse_device_identity(&path, &fs::read(&path).map_err(AppError::operation)?)
                    .map(Some)
            }
            Err(error) => Err(AppError::operation(error.error)),
        }
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

    pub fn load_apply_marker(&self, git_dir: &Path) -> Result<Option<SyncApplyMarker>, AppError> {
        let path = apply_marker_path(git_dir, false)?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AppError::operation(error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(AppError::operation(format!(
                "sync apply marker at {} is not a regular file",
                path.display()
            )));
        }
        if metadata.len() > MAX_SYNC_JOURNAL_BYTES {
            return Err(AppError::operation(format!(
                "sync apply marker at {} exceeds the {} byte limit",
                path.display(),
                MAX_SYNC_JOURNAL_BYTES
            )));
        }
        let marker: SyncApplyMarker =
            serde_json::from_slice(&fs::read(&path).map_err(AppError::operation)?)
                .map_err(AppError::operation)?;
        validate_apply_marker(&path, &marker)?;
        Ok(Some(marker))
    }

    pub fn save_apply_marker(
        &self,
        git_dir: &Path,
        marker: &SyncApplyMarker,
    ) -> Result<(), AppError> {
        validate_apply_marker(Path::new("sync apply marker"), marker)?;
        let path = apply_marker_path(git_dir, true)?;
        let parent = path
            .parent()
            .ok_or_else(|| AppError::operation("sync apply marker path has no parent"))?;
        let bytes = serde_json::to_vec_pretty(marker).map_err(AppError::operation)?;
        let mut temporary = NamedTempFile::new_in(parent).map_err(AppError::operation)?;
        temporary.write_all(&bytes).map_err(AppError::operation)?;
        temporary.write_all(b"\n").map_err(AppError::operation)?;
        temporary
            .as_file()
            .sync_all()
            .map_err(AppError::operation)?;
        temporary
            .persist(path)
            .map_err(|error| AppError::operation(error.error))?;
        Ok(())
    }

    pub fn clear_apply_marker(&self, git_dir: &Path) -> Result<(), AppError> {
        let path = apply_marker_path(git_dir, false)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(AppError::operation(error)),
        }
    }
}

fn apply_marker_path(git_dir: &Path, create: bool) -> Result<PathBuf, AppError> {
    let git_dir = fs::canonicalize(git_dir).map_err(AppError::operation)?;
    let directory = git_dir.join("vulcan-sync");
    if create {
        fs::create_dir_all(&directory).map_err(AppError::operation)?;
    }
    if directory.exists() {
        let canonical = fs::canonicalize(&directory).map_err(AppError::operation)?;
        if !canonical.starts_with(&git_dir) || canonical == git_dir {
            return Err(AppError::operation(
                "sync marker directory escapes the canonical Git directory",
            ));
        }
    }
    Ok(directory.join("apply.json"))
}

fn validate_apply_marker(path: &Path, marker: &SyncApplyMarker) -> Result<(), AppError> {
    if marker.version != SYNC_APPLY_MARKER_VERSION {
        return Err(AppError::operation(format!(
            "unsupported sync apply marker version {} at {}",
            marker.version,
            path.display()
        )));
    }
    validate_repository_key(&marker.repository_key)?;
    vulcan_sync::GitOid::parse(marker.expected_revision.clone()).map_err(AppError::operation)?;
    vulcan_sync::GitOid::parse(marker.accepted.clone()).map_err(AppError::operation)?;
    Ok(())
}

fn parse_device_identity(path: &Path, source: &[u8]) -> Result<GitSyncDeviceId, AppError> {
    if source.len() as u64 > MAX_SYNC_JOURNAL_BYTES {
        return Err(AppError::operation(format!(
            "sync device identity at {} exceeds the {} byte limit",
            path.display(),
            MAX_SYNC_JOURNAL_BYTES
        )));
    }
    let identity: SyncDeviceIdentity =
        serde_json::from_slice(source).map_err(AppError::operation)?;
    if identity.version != SYNC_DEVICE_IDENTITY_VERSION {
        return Err(AppError::operation(format!(
            "unsupported sync device identity version {} at {}",
            identity.version,
            path.display()
        )));
    }
    GitSyncDeviceId::parse(identity.device_id).map_err(AppError::operation)
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
        for phase in [
            SyncJournalPhase::Preparing,
            SyncJournalPhase::Capturing,
            SyncJournalPhase::Captured,
            SyncJournalPhase::Fetching,
            SyncJournalPhase::Fetched,
            SyncJournalPhase::Merging,
            SyncJournalPhase::Pushing,
            SyncJournalPhase::Applying,
            SyncJournalPhase::Verifying,
            SyncJournalPhase::Error,
        ] {
            assert!(phase.requires_recovery(), "{phase:?} must recover");
        }
        for phase in [SyncJournalPhase::Paused, SyncJournalPhase::Conflicted] {
            assert!(
                !phase.requires_recovery(),
                "{phase:?} is retained review state"
            );
        }
    }

    #[test]
    fn apply_marker_round_trips_in_git_state_and_rejects_symlinked_directory() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        let git_dir = vault.join(".git");
        fs::create_dir_all(&git_dir).expect("Git directory");
        let store = SyncStateStore::at(temporary.path().join("state"));
        let mut journal =
            SyncJournal::preparing(&vault, "origin", "refs/heads/live").expect("journal");
        journal.repository_key =
            repository_state_key(&fs::canonicalize(&vault).expect("canonical vault"));
        journal.local_snapshot = Some("a".repeat(40));
        journal.accepted = Some("b".repeat(40));
        let marker = SyncApplyMarker::from_journal(&journal).expect("apply marker");

        store
            .save_apply_marker(&git_dir, &marker)
            .expect("save marker");
        assert_eq!(
            store.load_apply_marker(&git_dir).expect("load marker"),
            Some(marker)
        );
        store.clear_apply_marker(&git_dir).expect("clear marker");
        assert_eq!(
            store.load_apply_marker(&git_dir).expect("load cleared"),
            None
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = temporary.path().join("outside");
            fs::create_dir(&outside).expect("outside directory");
            fs::remove_dir(git_dir.join("vulcan-sync")).expect("remove empty marker directory");
            symlink(&outside, git_dir.join("vulcan-sync")).expect("marker symlink");
            assert!(store
                .save_apply_marker(
                    &git_dir,
                    &SyncApplyMarker::from_journal(&journal).expect("marker")
                )
                .is_err());
        }
    }

    #[test]
    fn device_identity_is_state_free_on_read_and_stable_after_creation() {
        let temporary = tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        let store = SyncStateStore::at(root.clone());

        assert_eq!(
            store
                .load_or_create_device_id(false)
                .expect("read-only identity lookup"),
            None
        );
        assert!(!root.exists());

        let created = store
            .load_or_create_device_id(true)
            .expect("create identity")
            .expect("created identity");
        let loaded = store
            .load_or_create_device_id(false)
            .expect("load identity")
            .expect("stored identity");
        assert_eq!(loaded, created);
        assert_eq!(created.as_str().len(), 26);
        assert!(root.join("_device.json").is_file());
    }
}
