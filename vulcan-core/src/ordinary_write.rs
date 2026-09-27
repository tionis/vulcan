//! Crash-recoverable batches of ordinary Markdown creates, replacements, and deletes.
//!
//! The journal is device-local authoritative workflow state, never cache data.
//! A prepared batch rolls forward only while every observed path still matches
//! its recorded before or after bytes; external edits block recovery.

use crate::paths::{
    normalize_relative_input_path, secure_create_atomic, secure_open_read, secure_remove,
    secure_replace, RelativePathOptions, VaultPaths,
};
use crate::write_lock::acquire_write_lock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::fs;
#[cfg(unix)]
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use ulid::Ulid;

const JOURNAL_VERSION: u32 = 1;
const STATE_DIR: &str = "ordinary-write";
const JOURNAL_NAME: &str = "journal.json";
const MAX_CHANGES: usize = 32;
const MAX_CONTENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrdinaryWriteChange {
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrdinaryWriteOutcome {
    pub transaction_id: String,
    pub changed_paths: Vec<String>,
    pub recovered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrdinaryWriteReview {
    pub transaction_id: String,
    pub changes: Vec<OrdinaryWriteReviewChange>,
    pub recoverable: bool,
    pub review_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrdinaryWriteReviewChange {
    pub path: String,
    pub state: &'static str,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub current_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrdinaryWriteAcceptCurrentOutcome {
    pub transaction_id: String,
    pub changed_paths: Vec<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrdinaryWriteError {
    pub code: String,
    pub message: String,
    pub path: Option<String>,
}

impl OrdinaryWriteError {
    fn new(code: &str, message: impl Into<String>, path: Option<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            path,
        }
    }

    fn io(context: &str, error: impl Display) -> Self {
        Self::new("ordinary_write_io", format!("{context}: {error}"), None)
    }

    fn drift(path: &str) -> Self {
        Self::new(
            "ordinary_write_recovery_blocked",
            format!("ordinary write recovery found externally changed bytes at {path}; explicit repair is required"),
            Some(path.to_string()),
        )
    }
}

impl Display for OrdinaryWriteError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for OrdinaryWriteError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Journal {
    version: u32,
    transaction_id: String,
    changes: Vec<OrdinaryWriteChange>,
    digest: String,
}

impl Journal {
    fn seal(&mut self) -> Result<(), OrdinaryWriteError> {
        self.digest.clear();
        self.digest =
            blake3::hash(&serde_json::to_vec(self).map_err(|error| {
                OrdinaryWriteError::io("serialize ordinary write journal", error)
            })?)
            .to_hex()
            .to_string();
        Ok(())
    }

    fn verify(&self) -> Result<(), OrdinaryWriteError> {
        let mut expected = self.clone();
        expected.seal()?;
        if self.version != JOURNAL_VERSION
            || self.digest != expected.digest
            || self.transaction_id.parse::<Ulid>().is_err()
        {
            return Err(OrdinaryWriteError::new(
                "ordinary_write_journal_corrupt",
                "ordinary write journal version, identity, or digest is invalid",
                None,
            ));
        }
        validate_changes(&self.changes)
    }

    fn outcome(&self, recovered: bool) -> OrdinaryWriteOutcome {
        OrdinaryWriteOutcome {
            transaction_id: self.transaction_id.clone(),
            changed_paths: self
                .changes
                .iter()
                .map(|change| change.path.clone())
                .collect(),
            recovered,
        }
    }
}

/// Apply one bounded batch under the cross-process vault write lock.
///
/// If the process stops after journal publication, the next recovery call
/// completes the recorded writes or reports an external-edit conflict.
pub fn apply_ordinary_write_batch(
    paths: &VaultPaths,
    changes: &[OrdinaryWriteChange],
) -> Result<OrdinaryWriteOutcome, OrdinaryWriteError> {
    apply_with_hook(paths, changes, |_| Ok(()))
}

fn apply_with_hook<F>(
    paths: &VaultPaths,
    changes: &[OrdinaryWriteChange],
    mut after_publish: F,
) -> Result<OrdinaryWriteOutcome, OrdinaryWriteError>
where
    F: FnMut(usize) -> Result<(), OrdinaryWriteError>,
{
    validate_changes(changes)?;
    let _lock = acquire_write_lock(paths)
        .map_err(|error| OrdinaryWriteError::io("acquire vault write lock", error))?;
    recover_locked(paths)?;
    let directory = ensure_state_directory(paths)?;
    for change in changes {
        if current_content(paths, &change.path)? != change.before {
            return Err(OrdinaryWriteError::new(
                "ordinary_write_stale",
                format!("ordinary write target {} changed before apply", change.path),
                Some(change.path.clone()),
            ));
        }
    }
    let mut journal = Journal {
        version: JOURNAL_VERSION,
        transaction_id: Ulid::new().to_string(),
        changes: changes.to_vec(),
        digest: String::new(),
    };
    journal.seal()?;
    save_journal(&directory, &journal)?;
    for (index, change) in journal.changes.iter().enumerate() {
        apply_one(paths, change)?;
        after_publish(index)?;
    }
    remove_journal(&directory)?;
    Ok(journal.outcome(false))
}

/// Recover an interrupted ordinary batch under the same vault lock.
/// Recovery is idempotent and never overwrites bytes outside the journal plan.
pub fn recover_ordinary_write_batch(
    paths: &VaultPaths,
) -> Result<Option<OrdinaryWriteOutcome>, OrdinaryWriteError> {
    let _lock = acquire_write_lock(paths)
        .map_err(|error| OrdinaryWriteError::io("acquire vault write lock", error))?;
    recover_locked(paths)
}

/// Inspect a pending batch without exposing note contents or changing files.
/// The review token binds an explicit repair decision to the observed bytes.
pub fn inspect_ordinary_write_batch(
    paths: &VaultPaths,
) -> Result<Option<OrdinaryWriteReview>, OrdinaryWriteError> {
    let Some(directory) = existing_state_directory(paths)? else {
        return Ok(None);
    };
    let Some(journal) = load_journal(&directory)? else {
        return Ok(None);
    };
    inspect_journal(paths, &journal).map(Some)
}

/// Retire a conflicted journal only after a human has reconciled its files.
/// This does not alter note bytes. A stale review token or recoverable batch
/// cannot be accepted as current state.
pub fn accept_current_ordinary_write_batch(
    paths: &VaultPaths,
    transaction_id: &str,
    review_token: &str,
    dry_run: bool,
) -> Result<OrdinaryWriteAcceptCurrentOutcome, OrdinaryWriteError> {
    let _lock = acquire_write_lock(paths)
        .map_err(|error| OrdinaryWriteError::io("acquire vault write lock", error))?;
    let directory = ensure_state_directory(paths)?;
    let journal = load_journal(&directory)?.ok_or_else(|| {
        OrdinaryWriteError::new(
            "ordinary_write_not_found",
            "ordinary write journal is no longer pending",
            None,
        )
    })?;
    let review = inspect_journal(paths, &journal)?;
    if review.transaction_id != transaction_id || review.review_token != review_token {
        return Err(OrdinaryWriteError::new(
            "ordinary_write_review_changed",
            "ordinary write journal or files changed since review; inspect them again",
            None,
        ));
    }
    if review.recoverable {
        return Err(OrdinaryWriteError::new(
            "ordinary_write_recoverable",
            "ordinary write batch can still roll forward; use normal recovery instead",
            None,
        ));
    }
    if !dry_run {
        remove_journal(&directory)?;
    }
    Ok(OrdinaryWriteAcceptCurrentOutcome {
        transaction_id: journal.transaction_id,
        changed_paths: journal
            .changes
            .into_iter()
            .map(|change| change.path)
            .collect(),
        dry_run,
    })
}

fn inspect_journal(
    paths: &VaultPaths,
    journal: &Journal,
) -> Result<OrdinaryWriteReview, OrdinaryWriteError> {
    let mut changes = Vec::with_capacity(journal.changes.len());
    let mut recoverable = true;
    for change in &journal.changes {
        let current = current_bytes(paths, &change.path)?;
        let state = if current.as_deref() == change.before.as_deref().map(str::as_bytes) {
            "before"
        } else if current.as_deref() == change.after.as_deref().map(str::as_bytes) {
            "after"
        } else {
            recoverable = false;
            "diverged"
        };
        changes.push(OrdinaryWriteReviewChange {
            path: change.path.clone(),
            state,
            before_digest: change.before.as_deref().map(content_digest),
            after_digest: change.after.as_deref().map(content_digest),
            current_digest: current
                .as_deref()
                .map(|bytes| blake3::hash(bytes).to_hex().to_string()),
        });
    }
    let review_bytes = serde_json::to_vec(&(&journal.digest, &changes))
        .map_err(|error| OrdinaryWriteError::io("serialize ordinary write review", error))?;
    Ok(OrdinaryWriteReview {
        transaction_id: journal.transaction_id.clone(),
        changes,
        recoverable,
        review_token: blake3::hash(&review_bytes).to_hex().to_string(),
    })
}

fn content_digest(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

fn recover_locked(paths: &VaultPaths) -> Result<Option<OrdinaryWriteOutcome>, OrdinaryWriteError> {
    let Some(directory) = existing_state_directory(paths)? else {
        return Ok(None);
    };
    let Some(journal) = load_journal(&directory)? else {
        return Ok(None);
    };
    // Check the entire batch before changing any path during recovery.
    for change in &journal.changes {
        let current = current_content(paths, &change.path)?;
        if current.as_deref() != change.before.as_deref()
            && current.as_deref() != change.after.as_deref()
        {
            return Err(OrdinaryWriteError::drift(&change.path));
        }
    }
    for change in &journal.changes {
        if current_content(paths, &change.path)?.as_deref() == change.after.as_deref() {
            continue;
        }
        apply_one(paths, change)?;
    }
    remove_journal(&directory)?;
    Ok(Some(journal.outcome(true)))
}

/// Recover while the caller already holds the vault write lock.
pub(crate) fn recover_ordinary_write_batch_unlocked(
    paths: &VaultPaths,
) -> Result<Option<OrdinaryWriteOutcome>, OrdinaryWriteError> {
    recover_locked(paths)
}

fn apply_one(paths: &VaultPaths, change: &OrdinaryWriteChange) -> Result<(), OrdinaryWriteError> {
    let current = current_content(paths, &change.path)?;
    if current != change.before {
        return Err(OrdinaryWriteError::drift(&change.path));
    }
    let write = match (&change.before, &change.after) {
        (None, Some(after)) => {
            secure_create_atomic(paths.vault_root(), Path::new(&change.path), after)
        }
        (Some(_), Some(after)) => {
            secure_replace(paths.vault_root(), Path::new(&change.path), after)
        }
        (Some(_), None) => secure_remove(paths.vault_root(), Path::new(&change.path)),
        (None, None) => unreachable!("validated change must have a before or after image"),
    };
    write.map_err(|error| OrdinaryWriteError::io("publish ordinary write", error))
}

fn current_content(paths: &VaultPaths, path: &str) -> Result<Option<String>, OrdinaryWriteError> {
    current_bytes(paths, path)?
        .map(|bytes| {
            String::from_utf8(bytes).map_err(|error| {
                OrdinaryWriteError::new(
                    "ordinary_write_invalid_utf8",
                    format!("ordinary write target is not UTF-8: {error}"),
                    Some(path.to_string()),
                )
            })
        })
        .transpose()
}

fn current_bytes(paths: &VaultPaths, path: &str) -> Result<Option<Vec<u8>>, OrdinaryWriteError> {
    let file = match secure_open_read(paths.vault_root(), Path::new(path)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(OrdinaryWriteError::io("read ordinary write target", error)),
    };
    let mut bytes = Vec::new();
    file.take((MAX_CONTENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| OrdinaryWriteError::io("read ordinary write target", error))?;
    if bytes.len() > MAX_CONTENT_BYTES {
        return Err(OrdinaryWriteError::new(
            "ordinary_write_limit",
            "ordinary write target exceeds the content limit",
            Some(path.to_string()),
        ));
    }
    Ok(Some(bytes))
}

fn validate_changes(changes: &[OrdinaryWriteChange]) -> Result<(), OrdinaryWriteError> {
    if changes.is_empty() || changes.len() > MAX_CHANGES {
        return Err(OrdinaryWriteError::new(
            "ordinary_write_limit",
            "ordinary write batch must contain 1 to 32 changes",
            None,
        ));
    }
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    for change in changes {
        let normalized = normalize_relative_input_path(
            &change.path,
            RelativePathOptions {
                expected_extension: Some("md"),
                append_extension_if_missing: false,
            },
        )
        .map_err(|error| {
            OrdinaryWriteError::new(
                "ordinary_write_invalid_path",
                error.to_string(),
                Some(change.path.clone()),
            )
        })?;
        if normalized != change.path
            || change
                .path
                .split('/')
                .any(|component| component == ".vulcan" || component == ".git")
            || !seen.insert(change.path.clone())
        {
            return Err(OrdinaryWriteError::new(
                "ordinary_write_invalid_path",
                format!(
                    "ordinary write path is unsafe or duplicated: {}",
                    change.path
                ),
                Some(change.path.clone()),
            ));
        }
        if change.before.is_none() && change.after.is_none() {
            return Err(OrdinaryWriteError::new(
                "ordinary_write_invalid_change",
                format!(
                    "ordinary write change has neither before nor after bytes: {}",
                    change.path
                ),
                Some(change.path.clone()),
            ));
        }
        total = total
            .saturating_add(change.before.as_ref().map_or(0, String::len))
            .saturating_add(change.after.as_ref().map_or(0, String::len));
        if total > MAX_CONTENT_BYTES {
            return Err(OrdinaryWriteError::new(
                "ordinary_write_limit",
                "ordinary write batch exceeds the content limit",
                None,
            ));
        }
    }
    Ok(())
}

fn ensure_state_directory(paths: &VaultPaths) -> Result<PathBuf, OrdinaryWriteError> {
    let directory = paths
        .operational_state_dir()
        .map_err(|error| OrdinaryWriteError::io("resolve ordinary write state", error))?
        .join(STATE_DIR);
    fs::create_dir_all(&directory)
        .map_err(|error| OrdinaryWriteError::io("create ordinary write state", error))?;
    let metadata = fs::symlink_metadata(&directory)
        .map_err(|error| OrdinaryWriteError::io("inspect ordinary write state", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(OrdinaryWriteError::new(
            "ordinary_write_state_invalid",
            "ordinary write state is not a plain directory",
            None,
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| OrdinaryWriteError::io("protect ordinary write state", error))?;
    }
    Ok(directory)
}

fn existing_state_directory(paths: &VaultPaths) -> Result<Option<PathBuf>, OrdinaryWriteError> {
    let directory = paths
        .operational_state_dir()
        .map_err(|error| OrdinaryWriteError::io("resolve ordinary write state", error))?
        .join(STATE_DIR);
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrdinaryWriteError::io(
                "inspect ordinary write state",
                error,
            ))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(OrdinaryWriteError::new(
            "ordinary_write_state_invalid",
            "ordinary write state is not a plain directory",
            None,
        ));
    }
    Ok(Some(directory))
}

fn journal_path(directory: &Path) -> PathBuf {
    directory.join(JOURNAL_NAME)
}

fn load_journal(directory: &Path) -> Result<Option<Journal>, OrdinaryWriteError> {
    let path = journal_path(directory);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrdinaryWriteError::io(
                "inspect ordinary write journal",
                error,
            ))
        }
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_JOURNAL_BYTES
    {
        return Err(OrdinaryWriteError::new(
            "ordinary_write_journal_corrupt",
            "ordinary write journal is not a bounded plain file",
            None,
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(OrdinaryWriteError::new(
                "ordinary_write_journal_corrupt",
                "ordinary write journal must be owner-only",
                None,
            ));
        }
    }
    let journal: Journal = serde_json::from_slice(
        &fs::read(&path)
            .map_err(|error| OrdinaryWriteError::io("read ordinary write journal", error))?,
    )
    .map_err(|error| OrdinaryWriteError::io("parse ordinary write journal", error))?;
    journal.verify()?;
    Ok(Some(journal))
}

fn save_journal(directory: &Path, journal: &Journal) -> Result<(), OrdinaryWriteError> {
    let bytes = serde_json::to_vec(journal)
        .map_err(|error| OrdinaryWriteError::io("serialize ordinary write journal", error))?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(OrdinaryWriteError::new(
            "ordinary_write_limit",
            "ordinary write journal exceeds the size limit",
            None,
        ));
    }
    let mut temporary = NamedTempFile::new_in(directory)
        .map_err(|error| OrdinaryWriteError::io("stage ordinary write journal", error))?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| OrdinaryWriteError::io("sync ordinary write journal", error))?;
    temporary
        .persist(journal_path(directory))
        .map_err(|error| OrdinaryWriteError::io("publish ordinary write journal", error.error))?;
    sync_directory(directory)
}

fn remove_journal(directory: &Path) -> Result<(), OrdinaryWriteError> {
    fs::remove_file(journal_path(directory))
        .map_err(|error| OrdinaryWriteError::io("remove ordinary write journal", error))?;
    sync_directory(directory)
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<(), OrdinaryWriteError> {
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|error| OrdinaryWriteError::io("sync ordinary write state", error))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // Keep one cross-platform durability interface.
fn sync_directory(_directory: &Path) -> Result<(), OrdinaryWriteError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::initialize_vulcan_dir;
    use tempfile::tempdir;

    fn changes() -> Vec<OrdinaryWriteChange> {
        vec![
            OrdinaryWriteChange {
                path: "Task.md".to_string(),
                before: None,
                after: Some("new task\n".to_string()),
            },
            OrdinaryWriteChange {
                path: "Inbox.md".to_string(),
                before: Some("old task\n".to_string()),
                after: Some("[[Task]]\n".to_string()),
            },
        ]
    }

    fn archive_changes() -> Vec<OrdinaryWriteChange> {
        vec![
            OrdinaryWriteChange {
                path: "Archive/Task.md".to_string(),
                before: None,
                after: Some("archived task\n".to_string()),
            },
            OrdinaryWriteChange {
                path: "Task.md".to_string(),
                before: Some("active task\n".to_string()),
                after: None,
            },
        ]
    }

    #[test]
    fn interrupted_archive_rolls_forward_and_removes_source() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Task.md"), "active task\n").expect("source");
        fs::create_dir(temporary.path().join("Archive")).expect("archive folder");
        apply_with_hook(&paths, &archive_changes(), |index| {
            if index == 0 {
                Err(OrdinaryWriteError::new("test_interruption", "stop", None))
            } else {
                Ok(())
            }
        })
        .expect_err("simulated interruption");
        assert!(temporary.path().join("Task.md").exists());
        assert!(temporary.path().join("Archive/Task.md").exists());

        let recovered = recover_ordinary_write_batch(&paths)
            .expect("recover archive")
            .expect("journaled batch");
        assert!(recovered.recovered);
        assert!(!temporary.path().join("Task.md").exists());
        assert_eq!(
            fs::read_to_string(temporary.path().join("Archive/Task.md")).expect("archive"),
            "archived task\n"
        );
        assert!(recover_ordinary_write_batch(&paths)
            .expect("idempotent recovery")
            .is_none());
    }

    #[test]
    fn archive_recovery_preserves_externally_edited_source() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Task.md"), "active task\n").expect("source");
        fs::create_dir(temporary.path().join("Archive")).expect("archive folder");
        apply_with_hook(&paths, &archive_changes(), |_| {
            Err(OrdinaryWriteError::new("test_interruption", "stop", None))
        })
        .expect_err("simulated interruption");
        fs::write(temporary.path().join("Task.md"), "external edit\n").expect("external edit");

        let error = recover_ordinary_write_batch(&paths).expect_err("external edit blocks");
        assert_eq!(error.code, "ordinary_write_recovery_blocked");
        assert_eq!(
            fs::read_to_string(temporary.path().join("Task.md")).expect("source"),
            "external edit\n"
        );
        assert!(temporary.path().join("Archive/Task.md").exists());
    }

    #[test]
    fn conflicted_batch_requires_exact_review_before_accepting_current_files() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Inbox.md"), "old task\n").expect("source");
        apply_with_hook(&paths, &changes(), |_| {
            Err(OrdinaryWriteError::new("test_interruption", "stop", None))
        })
        .expect_err("simulated interruption");
        fs::write(temporary.path().join("Inbox.md"), "manual reconciliation\n")
            .expect("external edit");

        let review = inspect_ordinary_write_batch(&paths)
            .expect("inspect")
            .expect("pending batch");
        assert!(!review.recoverable);
        assert_eq!(review.changes[0].state, "after");
        assert_eq!(review.changes[1].state, "diverged");
        assert!(!review.review_token.is_empty());
        assert!(
            accept_current_ordinary_write_batch(
                &paths,
                &review.transaction_id,
                &review.review_token,
                true,
            )
            .expect("dry-run acceptance")
            .dry_run
        );
        assert!(inspect_ordinary_write_batch(&paths)
            .expect("still pending")
            .is_some());
        fs::write(temporary.path().join("Inbox.md"), "later edit\n").expect("later edit");
        assert_eq!(
            accept_current_ordinary_write_batch(
                &paths,
                &review.transaction_id,
                &review.review_token,
                false,
            )
            .expect_err("stale review")
            .code,
            "ordinary_write_review_changed"
        );
        let updated = inspect_ordinary_write_batch(&paths)
            .expect("reinspect")
            .expect("pending batch");
        accept_current_ordinary_write_batch(
            &paths,
            &updated.transaction_id,
            &updated.review_token,
            false,
        )
        .expect("accept reconciled files");
        assert!(inspect_ordinary_write_batch(&paths)
            .expect("retired")
            .is_none());
        assert_eq!(
            fs::read_to_string(temporary.path().join("Inbox.md")).expect("source"),
            "later edit\n"
        );
    }

    #[test]
    fn recoverable_batch_cannot_be_discarded_as_current() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Inbox.md"), "old task\n").expect("source");
        apply_with_hook(&paths, &changes(), |_| {
            Err(OrdinaryWriteError::new("test_interruption", "stop", None))
        })
        .expect_err("simulated interruption");
        let review = inspect_ordinary_write_batch(&paths)
            .expect("inspect")
            .expect("pending batch");
        assert!(review.recoverable);
        assert_eq!(
            accept_current_ordinary_write_batch(
                &paths,
                &review.transaction_id,
                &review.review_token,
                false,
            )
            .expect_err("roll-forward required")
            .code,
            "ordinary_write_recoverable"
        );
        assert!(inspect_ordinary_write_batch(&paths)
            .expect("still pending")
            .is_some());
    }

    #[test]
    fn binary_external_edit_can_be_reviewed_without_reading_or_replacing_it() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Inbox.md"), "old task\n").expect("source");
        apply_with_hook(&paths, &changes(), |_| {
            Err(OrdinaryWriteError::new("test_interruption", "stop", None))
        })
        .expect_err("simulated interruption");
        fs::write(temporary.path().join("Inbox.md"), [0xff, 0x00]).expect("binary external edit");
        let review = inspect_ordinary_write_batch(&paths)
            .expect("inspect")
            .expect("pending batch");
        assert_eq!(review.changes[1].state, "diverged");
        accept_current_ordinary_write_batch(
            &paths,
            &review.transaction_id,
            &review.review_token,
            false,
        )
        .expect("accept current binary bytes without altering them");
        assert_eq!(
            fs::read(temporary.path().join("Inbox.md")).expect("source"),
            [0xff, 0x00]
        );
    }

    #[test]
    fn interrupted_batch_rolls_forward_without_reapplying_complete_paths() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Inbox.md"), "old task\n").expect("source");
        apply_with_hook(&paths, &changes(), |index| {
            if index == 0 {
                Err(OrdinaryWriteError::new(
                    "test_interruption",
                    "stop after create",
                    None,
                ))
            } else {
                Ok(())
            }
        })
        .expect_err("simulated interruption");
        assert_eq!(
            fs::read_to_string(temporary.path().join("Task.md")).expect("target"),
            "new task\n"
        );
        assert_eq!(
            fs::read_to_string(temporary.path().join("Inbox.md")).expect("source"),
            "old task\n"
        );

        let recovered = recover_ordinary_write_batch(&paths)
            .expect("recover batch")
            .expect("journaled batch");
        assert!(recovered.recovered);
        assert_eq!(recovered.changed_paths, ["Task.md", "Inbox.md"]);
        assert_eq!(
            fs::read_to_string(temporary.path().join("Inbox.md")).expect("source"),
            "[[Task]]\n"
        );
        assert!(recover_ordinary_write_batch(&paths)
            .expect("idempotent recovery")
            .is_none());
    }

    #[test]
    fn scanning_rolls_forward_before_indexing_an_interrupted_batch() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Inbox.md"), "old task\n").expect("source");
        crate::scan_vault(&paths, crate::ScanMode::Full).expect("initial scan");
        apply_with_hook(&paths, &changes(), |_| {
            Err(OrdinaryWriteError::new("test_interruption", "stop", None))
        })
        .expect_err("simulated interruption");

        let summary = crate::scan_vault(&paths, crate::ScanMode::Incremental)
            .expect("scan recovers before indexing");
        assert_eq!(summary.added, 1);
        assert_eq!(summary.updated, 1);
        assert_eq!(
            fs::read_to_string(temporary.path().join("Inbox.md")).expect("source"),
            "[[Task]]\n"
        );
        assert!(inspect_ordinary_write_batch(&paths)
            .expect("journal cleared")
            .is_none());
    }

    #[test]
    fn scanning_refuses_a_conflicted_batch_without_indexing_partial_state() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Inbox.md"), "old task\n").expect("source");
        crate::scan_vault(&paths, crate::ScanMode::Full).expect("initial scan");
        apply_with_hook(&paths, &changes(), |_| {
            Err(OrdinaryWriteError::new("test_interruption", "stop", None))
        })
        .expect_err("simulated interruption");
        fs::write(temporary.path().join("Inbox.md"), "external edit\n").expect("external edit");

        let error = crate::scan_vault(&paths, crate::ScanMode::Incremental)
            .expect_err("conflicted recovery blocks scan");
        assert!(matches!(error, crate::ScanError::OrdinaryWrite(_)));
        assert_eq!(
            fs::read_to_string(temporary.path().join("Inbox.md")).expect("source"),
            "external edit\n"
        );
        assert!(inspect_ordinary_write_batch(&paths)
            .expect("journal preserved")
            .is_some());
    }

    #[test]
    fn watched_path_scan_rescans_every_path_after_recovery() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Inbox.md"), "old task\n").expect("source");
        crate::scan_vault(&paths, crate::ScanMode::Full).expect("initial scan");
        apply_with_hook(&paths, &changes(), |_| {
            Err(OrdinaryWriteError::new("test_interruption", "stop", None))
        })
        .expect_err("simulated interruption");

        let summary =
            crate::scan::scan_watched_paths(&paths, &["Task.md".to_string()].into_iter().collect())
                .expect("watch scan recovers entire batch");
        assert_eq!(summary.added, 1);
        assert_eq!(summary.updated, 1);
    }

    #[test]
    fn recovery_rejects_external_edits_before_changing_other_paths() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Inbox.md"), "old task\n").expect("source");
        apply_with_hook(&paths, &changes(), |_| {
            Err(OrdinaryWriteError::new("test_interruption", "stop", None))
        })
        .expect_err("simulated interruption");
        fs::write(temporary.path().join("Inbox.md"), "external edit\n").expect("external edit");

        let error =
            recover_ordinary_write_batch(&paths).expect_err("external edit blocks recovery");
        assert_eq!(error.code, "ordinary_write_recovery_blocked");
        assert_eq!(error.path.as_deref(), Some("Inbox.md"));
        assert_eq!(
            fs::read_to_string(temporary.path().join("Task.md")).expect("target"),
            "new task\n"
        );
        assert_eq!(
            fs::read_to_string(temporary.path().join("Inbox.md")).expect("source"),
            "external edit\n"
        );
        assert!(journal_path(&ensure_state_directory(&paths).expect("state")).exists());
    }

    #[test]
    fn stale_batch_rejects_before_publishing_journal_or_files() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Inbox.md"), "changed\n").expect("source");
        let error = apply_ordinary_write_batch(&paths, &changes()).expect_err("stale source");
        assert_eq!(error.code, "ordinary_write_stale");
        assert!(!temporary.path().join("Task.md").exists());
        assert!(!journal_path(&ensure_state_directory(&paths).expect("state")).exists());
    }
}
