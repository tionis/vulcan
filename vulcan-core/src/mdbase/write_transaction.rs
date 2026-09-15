use super::{
    mdbase_content_revision, verify_mdbase_write_preview, MdbaseWritePreview,
    MdbaseWritePreviewChange, MdbaseWritePreviewVerification,
};
use crate::paths::{ensure_vulcan_dir, secure_read_to_string, VaultPaths};
use crate::write_lock::{acquire_read_lock, acquire_write_lock, ReadLockGuard};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tempfile::NamedTempFile;
use ulid::Ulid;

pub const MDBASE_WRITE_JOURNAL_VERSION: u32 = 1;
pub const MDBASE_WRITE_MAX_CHANGED_FILES: usize = 1_000;
pub const MDBASE_WRITE_MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const MDBASE_WRITE_MAX_JOURNAL_BYTES: u64 = 512 * 1024 * 1024;
const MDBASE_WRITE_STATE_DIR: &str = "mdbase-write";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdbaseWriteApplyRequest<'a> {
    pub preview: &'a MdbaseWritePreview,
    pub verification: MdbaseWritePreviewVerification<'a>,
    pub idempotency_key: &'a str,
}

#[derive(Debug)]
pub struct MdbaseConsistentReadGuard {
    _lock: ReadLockGuard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MdbaseWriteOutcomeStatus {
    Committed,
    CommittedWithFollowUpFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWriteOutcome {
    pub transaction_id: String,
    pub plan_id: String,
    pub status: MdbaseWriteOutcomeStatus,
    pub changed_paths: Vec<String>,
    pub follow_up_error: Option<String>,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWritePathEvent {
    pub path: String,
    pub before_revision: Option<String>,
    pub after_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWriteOutboxEvent {
    pub version: u32,
    pub transaction_id: String,
    pub plan_id: String,
    pub operation: String,
    pub committed_at: String,
    pub paths: Vec<MdbaseWritePathEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWriteTransactionError {
    pub code: String,
    pub message: String,
    pub transaction_id: Option<String>,
    pub path: Option<String>,
}

impl MdbaseWriteTransactionError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            transaction_id: None,
            path: None,
        }
    }

    fn io(context: &str, error: impl Display) -> Self {
        Self::new("transaction_io_error", format!("{context}: {error}"))
    }

    fn blocked(transaction_id: &str, path: &str) -> Self {
        Self {
            code: "recovery_blocked".to_string(),
            message: "mdbase transaction recovery found externally changed bytes; explicit repair is required".to_string(),
            transaction_id: Some(transaction_id.to_string()),
            path: Some(path.to_string()),
        }
    }

    fn from_preview(error: super::MdbaseWritePreviewError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            transaction_id: None,
            path: error.path,
        }
    }
}

impl Display for MdbaseWriteTransactionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MdbaseWriteTransactionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalPhase {
    Preparing,
    Staged,
    Applying,
    CommitDecided,
    Consistent,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct OperationIdentity {
    caller_id: String,
    instance_id: String,
    idempotency_key: String,
    plan_digest: String,
    input_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RecoveryConflict {
    path: String,
    observed: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CaseOnlyRename {
    from: String,
    to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WriteJournal {
    version: u32,
    transaction_id: String,
    identity: OperationIdentity,
    preview: MdbaseWritePreview,
    phase: JournalPhase,
    planned_directories: Vec<String>,
    created_directories: Vec<String>,
    case_only_renames: Vec<CaseOnlyRename>,
    applied_paths: Vec<String>,
    committed_at: Option<String>,
    follow_up_error: Option<String>,
    conflict: Option<RecoveryConflict>,
    digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct IdempotencyReceipt {
    version: u32,
    identity: OperationIdentity,
    outcome: MdbaseWriteOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryDirection {
    RollBack,
    RollForward,
}

/// Apply one bounded mdbase batch under the shared vault write lock.
///
/// `reconcile` updates rebuildable projections after the durable commit
/// decision. Its failure is returned as a committed follow-up failure and is
/// retained for recovery; it never makes the canonical write retryable.
pub fn apply_mdbase_write_transaction<F>(
    paths: &VaultPaths,
    collection: &super::MdbaseCollection,
    request: &MdbaseWriteApplyRequest<'_>,
    reconcile: F,
) -> Result<MdbaseWriteOutcome, MdbaseWriteTransactionError>
where
    F: FnMut(&MdbaseWriteOutboxEvent) -> Result<(), String>,
{
    apply_with_boundary_hook(paths, collection, request, reconcile, |_| Ok(()))
}

/// Apply one bounded mdbase batch with a blocking application-layer preflight.
///
/// The preflight runs under the vault write lock, after recovery, idempotency
/// replay detection, and preview verification, but before journal creation or
/// canonical mutation. This lets reusable orchestration dispatch blocking
/// lifecycle hooks exactly once without opening a time-of-check/time-of-use
/// window.
pub fn apply_mdbase_write_transaction_with_preflight<P, F>(
    paths: &VaultPaths,
    collection: &super::MdbaseCollection,
    request: &MdbaseWriteApplyRequest<'_>,
    preflight: P,
    reconcile: F,
) -> Result<MdbaseWriteOutcome, MdbaseWriteTransactionError>
where
    P: FnOnce() -> Result<(), String>,
    F: FnMut(&MdbaseWriteOutboxEvent) -> Result<(), String>,
{
    apply_with_preflight_boundary_hook(paths, collection, request, preflight, reconcile, |_| Ok(()))
}

/// Recover the single vault-wide mdbase write journal, if present.
///
/// Cooperating reads, scans, mutations, and sync entrypoints call this while
/// holding (or before acquiring) their normal shared write-serialization path.
pub fn recover_mdbase_write_transaction<F>(
    paths: &VaultPaths,
    collection: &super::MdbaseCollection,
    reconcile: F,
) -> Result<Option<MdbaseWriteOutcome>, MdbaseWriteTransactionError>
where
    F: FnMut(&MdbaseWriteOutboxEvent) -> Result<(), String>,
{
    let _lock = acquire_write_lock(paths).map_err(|error| {
        MdbaseWriteTransactionError::io("failed to acquire vault write lock", error)
    })?;
    recover_locked(paths, collection, reconcile)
}

/// Hold the shared vault lock while a cooperating reader observes mdbase
/// state. An interrupted or blocked write is reported before any record bytes
/// are read; the caller can invoke recovery through its normal reconciliation
/// workflow and retry.
pub fn acquire_mdbase_consistent_read(
    paths: &VaultPaths,
) -> Result<Option<MdbaseConsistentReadGuard>, MdbaseWriteTransactionError> {
    match fs::symlink_metadata(paths.vulcan_dir()) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(MdbaseWriteTransactionError::io(
                "failed to inspect mdbase read lock state",
                error,
            ));
        }
        Ok(_) => {}
    }
    let lock = acquire_read_lock(paths).map_err(|error| {
        MdbaseWriteTransactionError::io("failed to acquire vault read lock", error)
    })?;
    let journal = journal_path(paths);
    match fs::symlink_metadata(&journal) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            Ok(Some(MdbaseConsistentReadGuard { _lock: lock }))
        }
        Err(error) => Err(MdbaseWriteTransactionError::io(
            "failed to inspect mdbase recovery journal",
            error,
        )),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(MdbaseWriteTransactionError::new(
                "journal_corrupt",
                "mdbase recovery journal is not a plain file",
            ))
        }
        Ok(_) => {
            let journal = load_journal(paths)?;
            let code = if journal
                .as_ref()
                .is_some_and(|journal| journal.phase == JournalPhase::Blocked)
            {
                "recovery_blocked"
            } else {
                "recovery_required"
            };
            Err(MdbaseWriteTransactionError::new(
                code,
                "mdbase transaction recovery is required before records can be read",
            ))
        }
    }
}

/// Load committed post-consistency events in stable transaction order.
pub fn list_mdbase_write_outbox(
    paths: &VaultPaths,
) -> Result<Vec<MdbaseWriteOutboxEvent>, MdbaseWriteTransactionError> {
    let _guard = acquire_mdbase_consistent_read(paths)?;
    let directory = state_root(paths).join("outbox");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(MdbaseWriteTransactionError::io(
                "failed to list mdbase outbox",
                error,
            ));
        }
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| MdbaseWriteTransactionError::io("failed to list mdbase outbox", error))?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    paths.sort();
    if paths.len() > 10_000 {
        return Err(MdbaseWriteTransactionError::new(
            "state_limit_exceeded",
            "mdbase outbox exceeds the 10,000-event inspection limit",
        ));
    }
    paths
        .into_iter()
        .map(|path| {
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                MdbaseWriteTransactionError::io("failed to inspect mdbase outbox event", error)
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(MdbaseWriteTransactionError::new(
                    "outbox_corrupt",
                    "mdbase outbox contains a non-regular event",
                ));
            }
            let event: MdbaseWriteOutboxEvent = serde_json::from_slice(&read_bounded(
                &path,
                4 * 1024 * 1024,
            )?)
            .map_err(|error| {
                MdbaseWriteTransactionError::io("failed to parse mdbase outbox event", error)
            })?;
            if event.version != MDBASE_WRITE_JOURNAL_VERSION {
                return Err(MdbaseWriteTransactionError::new(
                    "outbox_corrupt",
                    "unsupported mdbase outbox event version",
                ));
            }
            Ok(event)
        })
        .collect()
}

/// Acknowledge a delivered outbox event without touching its durable
/// idempotency receipt.
pub fn acknowledge_mdbase_write_outbox(
    paths: &VaultPaths,
    transaction_id: &str,
) -> Result<bool, MdbaseWriteTransactionError> {
    if transaction_id.len() != 26
        || !transaction_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err(MdbaseWriteTransactionError::new(
            "invalid_request",
            "invalid mdbase outbox transaction identifier",
        ));
    }
    let _lock = acquire_write_lock(paths).map_err(|error| {
        MdbaseWriteTransactionError::io("failed to acquire vault write lock", error)
    })?;
    let path = state_root(paths)
        .join("outbox")
        .join(format!("{transaction_id}.json"));
    let existed = path.exists();
    durable_remove(&path)?;
    Ok(existed)
}

fn apply_with_boundary_hook<F, H>(
    paths: &VaultPaths,
    collection: &super::MdbaseCollection,
    request: &MdbaseWriteApplyRequest<'_>,
    reconcile: F,
    boundary: H,
) -> Result<MdbaseWriteOutcome, MdbaseWriteTransactionError>
where
    F: FnMut(&MdbaseWriteOutboxEvent) -> Result<(), String>,
    H: FnMut(&str) -> Result<(), MdbaseWriteTransactionError>,
{
    apply_with_preflight_boundary_hook(paths, collection, request, || Ok(()), reconcile, boundary)
}

fn apply_with_preflight_boundary_hook<P, F, H>(
    paths: &VaultPaths,
    collection: &super::MdbaseCollection,
    request: &MdbaseWriteApplyRequest<'_>,
    preflight: P,
    mut reconcile: F,
    mut boundary: H,
) -> Result<MdbaseWriteOutcome, MdbaseWriteTransactionError>
where
    P: FnOnce() -> Result<(), String>,
    F: FnMut(&MdbaseWriteOutboxEvent) -> Result<(), String>,
    H: FnMut(&str) -> Result<(), MdbaseWriteTransactionError>,
{
    let _lock = acquire_write_lock(paths).map_err(|error| {
        MdbaseWriteTransactionError::io("failed to acquire vault write lock", error)
    })?;
    ensure_state_layout(paths)?;
    recover_locked(paths, collection, &mut reconcile)?;

    let identity = operation_identity(request)?;
    if let Some(mut receipt) = load_receipt(paths, &identity)? {
        receipt.outcome.replayed = true;
        return Ok(receipt.outcome);
    }
    validate_apply(paths, collection, request)?;
    preflight().map_err(|error| {
        MdbaseWriteTransactionError::new(
            "preflight_failed",
            format!("mdbase write preflight failed: {error}"),
        )
    })?;

    let mut journal = WriteJournal {
        version: MDBASE_WRITE_JOURNAL_VERSION,
        transaction_id: Ulid::new().to_string().to_lowercase(),
        identity,
        preview: request.preview.clone(),
        phase: JournalPhase::Preparing,
        planned_directories: planned_directories(collection, request.preview)?,
        created_directories: Vec::new(),
        case_only_renames: case_only_renames(collection, request.preview)?,
        applied_paths: Vec::new(),
        committed_at: None,
        follow_up_error: None,
        conflict: None,
        digest: String::new(),
    };
    save_journal(paths, &mut journal)?;
    boundary("journal_prepared")?;
    stage_replacements(paths, &journal)?;
    journal.phase = JournalPhase::Staged;
    save_journal(paths, &mut journal)?;
    boundary("replacements_staged")?;

    if let Err(error) =
        verify_mdbase_write_preview(collection, request.preview, &request.verification)
    {
        clear_transaction(paths, &journal.transaction_id)?;
        return Err(MdbaseWriteTransactionError::from_preview(error));
    }
    recheck_planned_directories(collection, &journal.planned_directories)?;
    create_planned_directories(paths, collection, &mut journal)?;
    journal.phase = JournalPhase::Applying;
    save_journal(paths, &mut journal)?;

    for index in ordered_change_indices(&journal)? {
        boundary("before_replace")?;
        let change = journal.preview.changes[index].clone();
        apply_change(
            paths,
            collection,
            &mut journal,
            &change,
            RecoveryDirection::RollForward,
        )?;
        journal.applied_paths.push(change.path);
        save_journal(paths, &mut journal)?;
        boundary("after_replace")?;
    }
    journal.phase = JournalPhase::CommitDecided;
    journal.committed_at = Some(timestamp_now());
    save_journal(paths, &mut journal)?;
    boundary("commit_decided")?;

    finish_committed(paths, collection, journal, &mut reconcile, &mut boundary)
}

fn recover_locked<F>(
    paths: &VaultPaths,
    collection: &super::MdbaseCollection,
    reconcile: F,
) -> Result<Option<MdbaseWriteOutcome>, MdbaseWriteTransactionError>
where
    F: FnMut(&MdbaseWriteOutboxEvent) -> Result<(), String>,
{
    ensure_state_layout(paths)?;
    let Some(mut journal) = load_journal(paths)? else {
        return Ok(None);
    };
    ensure_collection_identity(collection, &journal.preview.collection_root)?;
    if journal.phase == JournalPhase::Blocked {
        let path = journal
            .conflict
            .as_ref()
            .map_or("unknown", |conflict| conflict.path.as_str());
        return Err(MdbaseWriteTransactionError::blocked(
            &journal.transaction_id,
            path,
        ));
    }

    match journal.phase {
        JournalPhase::Preparing | JournalPhase::Staged | JournalPhase::Applying => {
            for index in ordered_change_indices(&journal)?.into_iter().rev() {
                let change = journal.preview.changes[index].clone();
                apply_change(
                    paths,
                    collection,
                    &mut journal,
                    &change,
                    RecoveryDirection::RollBack,
                )?;
            }
            remove_planned_directories(collection, &journal.created_directories)?;
            clear_transaction(paths, &journal.transaction_id)?;
            Ok(None)
        }
        JournalPhase::CommitDecided => {
            for index in ordered_change_indices(&journal)? {
                let change = journal.preview.changes[index].clone();
                apply_change(
                    paths,
                    collection,
                    &mut journal,
                    &change,
                    RecoveryDirection::RollForward,
                )?;
            }
            finish_committed(paths, collection, journal, reconcile, |_| Ok(())).map(Some)
        }
        JournalPhase::Consistent => {
            let event = outbox_event(&journal)?;
            persist_outbox(paths, &event)?;
            let outcome = journal_outcome(&journal, MdbaseWriteOutcomeStatus::Committed);
            save_receipt(paths, &journal.identity, &outcome)?;
            clear_transaction(paths, &journal.transaction_id)?;
            Ok(Some(outcome))
        }
        JournalPhase::Blocked => unreachable!("blocked journals return above"),
    }
}

fn finish_committed<F, H>(
    paths: &VaultPaths,
    _collection: &super::MdbaseCollection,
    mut journal: WriteJournal,
    mut reconcile: F,
    mut boundary: H,
) -> Result<MdbaseWriteOutcome, MdbaseWriteTransactionError>
where
    F: FnMut(&MdbaseWriteOutboxEvent) -> Result<(), String>,
    H: FnMut(&str) -> Result<(), MdbaseWriteTransactionError>,
{
    let event = outbox_event(&journal)?;
    if let Err(error) = reconcile(&event) {
        journal.follow_up_error = Some(bounded_message(&error));
        save_journal(paths, &mut journal)?;
        let outcome = journal_outcome(
            &journal,
            MdbaseWriteOutcomeStatus::CommittedWithFollowUpFailure,
        );
        save_receipt(paths, &journal.identity, &outcome)?;
        return Ok(outcome);
    }
    journal.phase = JournalPhase::Consistent;
    journal.follow_up_error = None;
    save_journal(paths, &mut journal)?;
    boundary("reconciled")?;
    persist_outbox(paths, &event)?;
    boundary("outbox_written")?;
    let outcome = journal_outcome(&journal, MdbaseWriteOutcomeStatus::Committed);
    save_receipt(paths, &journal.identity, &outcome)?;
    clear_transaction(paths, &journal.transaction_id)?;
    Ok(outcome)
}

fn validate_apply(
    paths: &VaultPaths,
    collection: &super::MdbaseCollection,
    request: &MdbaseWriteApplyRequest<'_>,
) -> Result<(), MdbaseWriteTransactionError> {
    if request.idempotency_key.is_empty()
        || request.idempotency_key.len() > 256
        || request.idempotency_key.chars().any(char::is_control)
    {
        return Err(MdbaseWriteTransactionError::new(
            "invalid_request",
            "mdbase idempotency key must be a bounded non-empty value",
        ));
    }
    if request.preview.changes.len() > MDBASE_WRITE_MAX_CHANGED_FILES {
        return Err(MdbaseWriteTransactionError::new(
            "limit_exceeded",
            "mdbase write exceeds the 1,000 changed-file limit",
        ));
    }
    let bytes = request
        .preview
        .changes
        .iter()
        .try_fold(0_u64, |total, change| {
            let before = change.before.as_ref().map_or(0, String::len);
            let after = change.after.as_ref().map_or(0, String::len);
            total.checked_add(u64::try_from(before + after).unwrap_or(u64::MAX))
        });
    if bytes.is_none_or(|bytes| bytes > MDBASE_WRITE_MAX_ARTIFACT_BYTES) {
        return Err(MdbaseWriteTransactionError::new(
            "limit_exceeded",
            "mdbase write exceeds the 64 MiB artifact limit",
        ));
    }
    ensure_collection_belongs_to_vault(paths, collection)?;
    verify_mdbase_write_preview(collection, request.preview, &request.verification)
        .map_err(MdbaseWriteTransactionError::from_preview)
}

fn operation_identity(
    request: &MdbaseWriteApplyRequest<'_>,
) -> Result<OperationIdentity, MdbaseWriteTransactionError> {
    let exact_input = serde_json::to_vec(&serde_json::json!({
        "plan_id": request.preview.plan_id,
        "plan_digest": request.preview.digest,
        "accepted_revisions": request.preview.accepted_revisions,
        "caller_id": request.verification.caller_id,
        "instance_id": request.verification.instance_id,
        "operation": request.verification.operation,
    }))
    .map_err(|error| MdbaseWriteTransactionError::io("failed to bind apply input", error))?;
    Ok(OperationIdentity {
        caller_id: request.verification.caller_id.to_string(),
        instance_id: request.verification.instance_id.to_string(),
        idempotency_key: request.idempotency_key.to_string(),
        plan_digest: request.preview.digest.clone(),
        input_digest: sha256(&exact_input),
    })
}

fn apply_change(
    paths: &VaultPaths,
    collection: &super::MdbaseCollection,
    journal: &mut WriteJournal,
    change: &MdbaseWritePreviewChange,
    direction: RecoveryDirection,
) -> Result<(), MdbaseWriteTransactionError> {
    // A case-insensitive filesystem exposes the source bytes when the
    // destination spelling is read during preview. Transactionally the
    // destination is still absent: the source must be removed first, and a
    // rollback must remove the destination before restoring the source name.
    let logical_before = if journal
        .case_only_renames
        .iter()
        .any(|rename| rename.to == change.path)
    {
        None
    } else {
        change.before.as_deref()
    };
    let (expected, desired) = match direction {
        RecoveryDirection::RollForward => (logical_before, change.after.as_deref()),
        RecoveryDirection::RollBack => (change.after.as_deref(), logical_before),
    };
    let observed = read_optional(collection, &change.path).map_err(|error| {
        MdbaseWriteTransactionError::io("failed to inspect a transaction path", error)
    })?;
    let revision_conflict = direction == RecoveryDirection::RollForward
        && change.if_revision.is_some()
        && observed.as_deref() != expected;
    if revision_conflict && journal.applied_paths.is_empty() {
        clear_transaction(paths, &journal.transaction_id)?;
        return Err(MdbaseWriteTransactionError::from_preview(
            super::MdbaseWritePreviewError::concurrent(&change.path),
        ));
    }
    if observed.as_deref() == desired && !revision_conflict {
        return Ok(());
    }
    if observed.as_deref() != expected {
        block_journal(paths, journal, &change.path, observed)?;
        return Err(MdbaseWriteTransactionError::blocked(
            &journal.transaction_id,
            &change.path,
        ));
    }
    write_desired(
        paths,
        collection,
        journal,
        change,
        desired,
        expected.is_none(),
    )?;
    let observed = read_optional(collection, &change.path).map_err(|error| {
        MdbaseWriteTransactionError::io("failed to verify a transaction path", error)
    })?;
    if observed.as_deref() != desired {
        block_journal(paths, journal, &change.path, observed)?;
        return Err(MdbaseWriteTransactionError::blocked(
            &journal.transaction_id,
            &change.path,
        ));
    }
    Ok(())
}

fn write_desired(
    paths: &VaultPaths,
    collection: &super::MdbaseCollection,
    journal: &WriteJournal,
    change: &MdbaseWritePreviewChange,
    desired: Option<&str>,
    create_without_clobber: bool,
) -> Result<(), MdbaseWriteTransactionError> {
    let target = collection.root.join(&change.path);
    let parent = target.parent().ok_or_else(|| {
        MdbaseWriteTransactionError::new("invalid_request", "changed path has no parent")
    })?;
    if let Some(contents) = desired {
        let replacement = if change.after.as_deref() == Some(contents) {
            let staged = staged_path(paths, &journal.transaction_id, change);
            let staged_contents = read_bounded(&staged, MDBASE_WRITE_MAX_ARTIFACT_BYTES)?;
            if staged_contents != contents.as_bytes() {
                return Err(MdbaseWriteTransactionError::new(
                    "journal_corrupt",
                    "staged mdbase replacement does not match its journal",
                ));
            }
            staged_contents
        } else {
            contents.as_bytes().to_vec()
        };
        let mut temporary = NamedTempFile::new_in(parent).map_err(|error| {
            MdbaseWriteTransactionError::io("failed to create replacement file", error)
        })?;
        temporary.write_all(&replacement).map_err(|error| {
            MdbaseWriteTransactionError::io("failed to write replacement file", error)
        })?;
        temporary.as_file().sync_all().map_err(|error| {
            MdbaseWriteTransactionError::io("failed to sync replacement file", error)
        })?;
        if create_without_clobber {
            temporary.persist_noclobber(&target).map_err(|error| {
                MdbaseWriteTransactionError::io(
                    "refused to overwrite a newly appeared file",
                    error.error,
                )
            })?;
        } else {
            temporary.persist(&target).map_err(|error| {
                MdbaseWriteTransactionError::io("failed to replace transaction file", error.error)
            })?;
        }
        sync_directory(parent)?;
    } else {
        fs::remove_file(&target).map_err(|error| {
            MdbaseWriteTransactionError::io("failed to remove transaction file", error)
        })?;
        sync_directory(parent)?;
    }
    Ok(())
}

fn stage_replacements(
    paths: &VaultPaths,
    journal: &WriteJournal,
) -> Result<(), MdbaseWriteTransactionError> {
    let directory = transaction_stage_dir(paths, &journal.transaction_id);
    ensure_plain_directory(&directory)?;
    for change in &journal.preview.changes {
        if let Some(after) = &change.after {
            let path = staged_path(paths, &journal.transaction_id, change);
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|error| {
                    MdbaseWriteTransactionError::io("failed to stage replacement", error)
                })?;
            file.write_all(after.as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(|error| {
                    MdbaseWriteTransactionError::io("failed to sync staged replacement", error)
                })?;
        }
    }
    sync_directory(&directory)
}

fn planned_directories(
    collection: &super::MdbaseCollection,
    preview: &MdbaseWritePreview,
) -> Result<Vec<String>, MdbaseWriteTransactionError> {
    let mut directories = BTreeSet::new();
    for change in preview
        .changes
        .iter()
        .filter(|change| change.after.is_some())
    {
        let path = Path::new(&change.path);
        let mut ancestors = path
            .ancestors()
            .skip(1)
            .filter(|path| !path.as_os_str().is_empty())
            .collect::<Vec<_>>();
        ancestors.reverse();
        for relative in ancestors {
            let absolute = collection.root.join(relative);
            match fs::symlink_metadata(&absolute) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(MdbaseWriteTransactionError::new(
                        "stale_state",
                        "mdbase write parent is not a plain directory",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    directories.insert(relative.to_string_lossy().replace('\\', "/"));
                }
                Err(error) => {
                    return Err(MdbaseWriteTransactionError::io(
                        "failed to inspect planned directory",
                        error,
                    ));
                }
            }
        }
    }
    Ok(directories.into_iter().collect())
}

fn case_only_renames(
    collection: &super::MdbaseCollection,
    preview: &MdbaseWritePreview,
) -> Result<Vec<CaseOnlyRename>, MdbaseWriteTransactionError> {
    let deletions = preview
        .changes
        .iter()
        .filter(|change| change.before.is_some() && change.after.is_none());
    let destination_changes = preview
        .changes
        .iter()
        .filter(|change| change.after.is_some())
        .collect::<Vec<_>>();
    let mut renames = Vec::new();
    let mut matched_destinations = BTreeSet::new();
    for deletion in deletions {
        let mut matches = Vec::new();
        for destination in &destination_changes {
            if deletion.path == destination.path
                || !deletion.path.eq_ignore_ascii_case(&destination.path)
            {
                continue;
            }
            let is_logically_absent = destination.before.is_none()
                || (destination.before == deletion.before
                    && paths_alias_existing_file(collection, &deletion.path, &destination.path)?);
            if is_logically_absent {
                matches.push(*destination);
            }
        }
        if matches.len() > 1 {
            return Err(MdbaseWriteTransactionError::new(
                "invalid_request",
                "case-only mdbase rename has ambiguous destinations",
            ));
        }
        if let Some(creation) = matches.first() {
            if !matched_destinations.insert(creation.path.clone()) {
                return Err(MdbaseWriteTransactionError::new(
                    "invalid_request",
                    "case-only mdbase rename destination is duplicated",
                ));
            }
            renames.push(CaseOnlyRename {
                from: deletion.path.clone(),
                to: creation.path.clone(),
            });
        }
    }
    renames.sort_by(|left, right| left.from.cmp(&right.from));
    Ok(renames)
}

fn paths_alias_existing_file(
    collection: &super::MdbaseCollection,
    left: &str,
    right: &str,
) -> Result<bool, MdbaseWriteTransactionError> {
    let canonicalize = |path: &str| match fs::canonicalize(collection.root.join(path)) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(MdbaseWriteTransactionError::io(
            "failed to resolve a possible case-only rename path",
            error,
        )),
    };
    Ok(canonicalize(left)?
        .zip(canonicalize(right)?)
        .is_some_and(|(left, right)| left == right))
}

fn ordered_change_indices(
    journal: &WriteJournal,
) -> Result<Vec<usize>, MdbaseWriteTransactionError> {
    let by_path = journal
        .preview
        .changes
        .iter()
        .enumerate()
        .map(|(index, change)| (change.path.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut ordered = Vec::with_capacity(journal.preview.changes.len());
    let mut included = BTreeSet::new();
    for rename in &journal.case_only_renames {
        for path in [&rename.from, &rename.to] {
            let index = *by_path.get(path.as_str()).ok_or_else(|| {
                MdbaseWriteTransactionError::new(
                    "journal_corrupt",
                    "case-only rename references an absent journal change",
                )
            })?;
            if included.insert(index) {
                ordered.push(index);
            }
        }
    }
    for index in 0..journal.preview.changes.len() {
        if included.insert(index) {
            ordered.push(index);
        }
    }
    Ok(ordered)
}

fn recheck_planned_directories(
    collection: &super::MdbaseCollection,
    directories: &[String],
) -> Result<(), MdbaseWriteTransactionError> {
    for directory in directories {
        match fs::symlink_metadata(collection.root.join(directory)) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(MdbaseWriteTransactionError::new(
                    "stale_state",
                    "a planned mdbase parent directory appeared after preview",
                ));
            }
            Err(error) => {
                return Err(MdbaseWriteTransactionError::io(
                    "failed to recheck planned directory",
                    error,
                ));
            }
        }
    }
    Ok(())
}

fn create_planned_directories(
    paths: &VaultPaths,
    collection: &super::MdbaseCollection,
    journal: &mut WriteJournal,
) -> Result<(), MdbaseWriteTransactionError> {
    for directory in journal.planned_directories.clone() {
        let path = collection.root.join(&directory);
        fs::create_dir(&path).map_err(|error| {
            MdbaseWriteTransactionError::io("failed to create planned directory", error)
        })?;
        sync_directory(
            path.parent()
                .expect("planned directory has collection parent"),
        )?;
        journal.created_directories.push(directory);
        save_journal(paths, journal)?;
    }
    Ok(())
}

fn remove_planned_directories(
    collection: &super::MdbaseCollection,
    directories: &[String],
) -> Result<(), MdbaseWriteTransactionError> {
    for directory in directories.iter().rev() {
        let path = collection.root.join(directory);
        match fs::remove_dir(&path) {
            Ok(()) => sync_directory(path.parent().expect("planned directory has a parent"))?,
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => {
                return Err(MdbaseWriteTransactionError::io(
                    "failed to remove planned directory during rollback",
                    error,
                ));
            }
        }
    }
    Ok(())
}

fn block_journal(
    paths: &VaultPaths,
    journal: &mut WriteJournal,
    path: &str,
    observed: Option<String>,
) -> Result<(), MdbaseWriteTransactionError> {
    journal.phase = JournalPhase::Blocked;
    journal.conflict = Some(RecoveryConflict {
        path: path.to_string(),
        observed,
    });
    save_journal(paths, journal)
}

fn outbox_event(
    journal: &WriteJournal,
) -> Result<MdbaseWriteOutboxEvent, MdbaseWriteTransactionError> {
    let committed_at = journal.committed_at.clone().ok_or_else(|| {
        MdbaseWriteTransactionError::new(
            "journal_corrupt",
            "committed journal has no decision timestamp",
        )
    })?;
    Ok(MdbaseWriteOutboxEvent {
        version: MDBASE_WRITE_JOURNAL_VERSION,
        transaction_id: journal.transaction_id.clone(),
        plan_id: journal.preview.plan_id.clone(),
        operation: journal.preview.operation.clone(),
        committed_at,
        paths: journal
            .preview
            .changes
            .iter()
            .map(|change| MdbaseWritePathEvent {
                path: change.path.clone(),
                before_revision: change.before_revision.clone(),
                after_revision: change.after.as_deref().map(mdbase_content_revision),
            })
            .collect(),
    })
}

fn journal_outcome(journal: &WriteJournal, status: MdbaseWriteOutcomeStatus) -> MdbaseWriteOutcome {
    MdbaseWriteOutcome {
        transaction_id: journal.transaction_id.clone(),
        plan_id: journal.preview.plan_id.clone(),
        status,
        changed_paths: journal
            .preview
            .changes
            .iter()
            .map(|change| change.path.clone())
            .collect(),
        follow_up_error: journal.follow_up_error.clone(),
        replayed: false,
    }
}

fn ensure_state_layout(paths: &VaultPaths) -> Result<(), MdbaseWriteTransactionError> {
    ensure_vulcan_dir(paths)
        .map_err(|error| MdbaseWriteTransactionError::io("failed to validate .vulcan", error))?;
    let root = state_root(paths);
    ensure_plain_directory(&root)?;
    ensure_plain_directory(&root.join("receipts"))?;
    ensure_plain_directory(&root.join("outbox"))?;
    ensure_plain_directory(&root.join("staging"))?;
    Ok(())
}

fn ensure_plain_directory(path: &Path) -> Result<(), MdbaseWriteTransactionError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(MdbaseWriteTransactionError::new(
            "state_unsafe",
            "mdbase transaction state path is not a plain directory",
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| {
                MdbaseWriteTransactionError::io(
                    "failed to create transaction state directory",
                    error,
                )
            })?;
            sync_directory(path.parent().expect("state directory has a parent"))
        }
        Err(error) => Err(MdbaseWriteTransactionError::io(
            "failed to inspect transaction state directory",
            error,
        )),
    }
}

fn save_journal(
    paths: &VaultPaths,
    journal: &mut WriteJournal,
) -> Result<(), MdbaseWriteTransactionError> {
    journal.digest.clear();
    journal.digest = journal_digest(journal)?;
    durable_json_replace(&journal_path(paths), journal)
}

fn load_journal(paths: &VaultPaths) -> Result<Option<WriteJournal>, MdbaseWriteTransactionError> {
    let path = journal_path(paths);
    let bytes = match read_bounded(&path, MDBASE_WRITE_MAX_JOURNAL_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.code == "not_found" => return Ok(None),
        Err(error) => return Err(error),
    };
    let journal: WriteJournal = serde_json::from_slice(&bytes).map_err(|error| {
        MdbaseWriteTransactionError::io("failed to parse mdbase journal", error)
    })?;
    if journal.version != MDBASE_WRITE_JOURNAL_VERSION
        || journal.digest != journal_digest(&journal)?
    {
        return Err(MdbaseWriteTransactionError::new(
            "journal_corrupt",
            "mdbase write journal failed its version or integrity check",
        ));
    }
    Ok(Some(journal))
}

fn journal_digest(journal: &WriteJournal) -> Result<String, MdbaseWriteTransactionError> {
    let mut payload = journal.clone();
    payload.digest.clear();
    serde_json::to_vec(&payload)
        .map(|bytes| sha256(&bytes))
        .map_err(|error| MdbaseWriteTransactionError::io("failed to digest mdbase journal", error))
}

fn save_receipt(
    paths: &VaultPaths,
    identity: &OperationIdentity,
    outcome: &MdbaseWriteOutcome,
) -> Result<(), MdbaseWriteTransactionError> {
    if let Some(existing) = load_receipt(paths, identity)? {
        if existing.outcome.transaction_id != outcome.transaction_id {
            return Err(MdbaseWriteTransactionError::new(
                "idempotency_conflict",
                "mdbase idempotency receipt belongs to another transaction",
            ));
        }
        return Ok(());
    }
    let receipt = IdempotencyReceipt {
        version: MDBASE_WRITE_JOURNAL_VERSION,
        identity: identity.clone(),
        outcome: outcome.clone(),
    };
    durable_json_replace(&receipt_path(paths, identity), &receipt)
}

fn load_receipt(
    paths: &VaultPaths,
    identity: &OperationIdentity,
) -> Result<Option<IdempotencyReceipt>, MdbaseWriteTransactionError> {
    let path = receipt_path(paths, identity);
    let bytes = match read_bounded(&path, 1024 * 1024) {
        Ok(bytes) => bytes,
        Err(error) if error.code == "not_found" => return Ok(None),
        Err(error) => return Err(error),
    };
    let receipt: IdempotencyReceipt = serde_json::from_slice(&bytes).map_err(|error| {
        MdbaseWriteTransactionError::io("failed to parse idempotency receipt", error)
    })?;
    if receipt.version != MDBASE_WRITE_JOURNAL_VERSION {
        return Err(MdbaseWriteTransactionError::new(
            "receipt_corrupt",
            "unsupported mdbase idempotency receipt version",
        ));
    }
    if receipt.identity != *identity {
        return Err(MdbaseWriteTransactionError::new(
            "idempotency_conflict",
            "mdbase idempotency key was already used with different exact input",
        ));
    }
    Ok(Some(receipt))
}

fn persist_outbox(
    paths: &VaultPaths,
    event: &MdbaseWriteOutboxEvent,
) -> Result<(), MdbaseWriteTransactionError> {
    let path = state_root(paths)
        .join("outbox")
        .join(format!("{}.json", event.transaction_id));
    if path.exists() {
        let current = read_bounded(&path, 4 * 1024 * 1024)?;
        let expected = serde_json::to_vec_pretty(event).map_err(|error| {
            MdbaseWriteTransactionError::io("failed to encode outbox event", error)
        })?;
        if current == expected {
            return Ok(());
        }
        return Err(MdbaseWriteTransactionError::new(
            "outbox_conflict",
            "existing mdbase outbox event does not match the committed transaction",
        ));
    }
    durable_json_replace(&path, event)
}

fn clear_transaction(
    paths: &VaultPaths,
    transaction_id: &str,
) -> Result<(), MdbaseWriteTransactionError> {
    durable_remove(&journal_path(paths))?;
    let stage = transaction_stage_dir(paths, transaction_id);
    match fs::remove_dir_all(&stage) {
        Ok(()) => sync_directory(stage.parent().expect("staging transaction has a parent")),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MdbaseWriteTransactionError::io(
            "failed to clear staged transaction",
            error,
        )),
    }
}

fn durable_json_replace<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), MdbaseWriteTransactionError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        MdbaseWriteTransactionError::io("failed to encode durable state", error)
    })?;
    let parent = path.parent().ok_or_else(|| {
        MdbaseWriteTransactionError::new("state_unsafe", "durable state has no parent")
    })?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| {
        MdbaseWriteTransactionError::io("failed to create durable state", error)
    })?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| MdbaseWriteTransactionError::io("failed to sync durable state", error))?;
    temporary.persist(path).map_err(|error| {
        MdbaseWriteTransactionError::io("failed to replace durable state", error.error)
    })?;
    sync_directory(parent)
}

fn durable_remove(path: &Path) -> Result<(), MdbaseWriteTransactionError> {
    match fs::remove_file(path) {
        Ok(()) => sync_directory(path.parent().expect("durable file has a parent")),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MdbaseWriteTransactionError::io(
            "failed to remove durable state",
            error,
        )),
    }
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, MdbaseWriteTransactionError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(MdbaseWriteTransactionError::new(
                "not_found",
                "durable state was not found",
            ));
        }
        Err(error) => {
            return Err(MdbaseWriteTransactionError::io(
                "failed to open durable state",
                error,
            ))
        }
    };
    if file
        .metadata()
        .map_err(|error| MdbaseWriteTransactionError::io("failed to inspect durable state", error))?
        .len()
        > limit
    {
        return Err(MdbaseWriteTransactionError::new(
            "state_limit_exceeded",
            "durable mdbase state exceeds its byte limit",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MdbaseWriteTransactionError::io("failed to read durable state", error))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(MdbaseWriteTransactionError::new(
            "state_limit_exceeded",
            "durable mdbase state exceeds its byte limit",
        ));
    }
    Ok(bytes)
}

fn read_optional(
    collection: &super::MdbaseCollection,
    path: &str,
) -> Result<Option<String>, std::io::Error> {
    match secure_read_to_string(&collection.root, Path::new(path)) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn ensure_collection_belongs_to_vault(
    paths: &VaultPaths,
    collection: &super::MdbaseCollection,
) -> Result<(), MdbaseWriteTransactionError> {
    let vault = fs::canonicalize(paths.vault_root())
        .map_err(|error| MdbaseWriteTransactionError::io("failed to resolve vault root", error))?;
    let vulcan_state = fs::canonicalize(paths.vulcan_dir()).map_err(|error| {
        MdbaseWriteTransactionError::io("failed to resolve .vulcan state root", error)
    })?;
    let collection_root = fs::canonicalize(&collection.root).map_err(|error| {
        MdbaseWriteTransactionError::io("failed to resolve collection root", error)
    })?;
    if !collection_root.starts_with(&vault) || collection_root.starts_with(vulcan_state) {
        return Err(MdbaseWriteTransactionError::new(
            "invalid_request",
            "mdbase collection must be inside the active vault",
        ));
    }
    Ok(())
}

fn ensure_collection_identity(
    collection: &super::MdbaseCollection,
    expected: &str,
) -> Result<(), MdbaseWriteTransactionError> {
    let current = fs::canonicalize(&collection.root)
        .map_err(|error| {
            MdbaseWriteTransactionError::io("failed to resolve collection during recovery", error)
        })?
        .to_string_lossy()
        .replace('\\', "/");
    if current != expected {
        return Err(MdbaseWriteTransactionError::new(
            "recovery_blocked",
            "mdbase collection identity changed while a transaction requires recovery",
        ));
    }
    Ok(())
}

fn staged_path(
    paths: &VaultPaths,
    transaction_id: &str,
    change: &MdbaseWritePreviewChange,
) -> PathBuf {
    let index = sha256(change.path.as_bytes())
        .trim_start_matches("sha256:")
        .to_string();
    transaction_stage_dir(paths, transaction_id).join(index)
}

fn state_root(paths: &VaultPaths) -> PathBuf {
    paths.vulcan_dir().join(MDBASE_WRITE_STATE_DIR)
}

fn journal_path(paths: &VaultPaths) -> PathBuf {
    state_root(paths).join("journal.json")
}

fn transaction_stage_dir(paths: &VaultPaths, transaction_id: &str) -> PathBuf {
    state_root(paths).join("staging").join(transaction_id)
}

fn receipt_path(paths: &VaultPaths, identity: &OperationIdentity) -> PathBuf {
    let key = format!(
        "{}\0{}\0{}",
        identity.caller_id, identity.instance_id, identity.idempotency_key
    );
    state_root(paths).join("receipts").join(format!(
        "{}.json",
        sha256(key.as_bytes()).trim_start_matches("sha256:")
    ))
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn timestamp_now() -> String {
    chrono::DateTime::<Utc>::from(SystemTime::now()).to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn bounded_message(message: &str) -> String {
    message.chars().take(2_048).collect()
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<(), MdbaseWriteTransactionError> {
    File::open(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| MdbaseWriteTransactionError::io("failed to sync directory", error))
}

#[cfg(not(unix))]
// Keep the fallible signature shared with Unix so durability call sites cannot
// accidentally discard Unix directory-sync failures behind platform cfgs.
#[allow(clippy::unnecessary_wraps)]
fn sync_directory(_directory: &Path) -> Result<(), MdbaseWriteTransactionError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdbase::{
        build_mdbase_write_preview, load_mdbase_collection, MdbaseWritePreviewChangeRequest,
        MdbaseWritePreviewRequest,
    };
    use crate::paths::initialize_vulcan_dir;
    use chrono::TimeZone;
    use std::cell::Cell;
    use tempfile::tempdir;

    fn write(root: &Path, path: &str, contents: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().expect("fixture parent"))
            .expect("create fixture directory");
        fs::write(path, contents).expect("write fixture");
    }

    fn directory_entry_names(root: &Path, path: &str) -> Vec<String> {
        let mut names = fs::read_dir(root.join(path))
            .expect("read fixture directory")
            .map(|entry| {
                entry
                    .expect("read fixture entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn fixture() -> (
        tempfile::TempDir,
        VaultPaths,
        super::super::MdbaseCollection,
    ) {
        let directory = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(directory.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        write(directory.path(), "mdbase.yaml", "spec_version: 0.3.0\n");
        write(directory.path(), "records/a.md", "before a\n");
        write(directory.path(), "records/b.md", "before b\n");
        let collection = load_mdbase_collection(directory.path())
            .expect("load collection")
            .expect("collection");
        (directory, paths, collection)
    }

    fn preview(
        collection: &super::super::MdbaseCollection,
        changes: Vec<MdbaseWritePreviewChangeRequest>,
    ) -> MdbaseWritePreview {
        build_mdbase_write_preview(
            collection,
            MdbaseWritePreviewRequest {
                plan_id: "plan-1".to_string(),
                caller_id: "caller".to_string(),
                instance_id: "instance".to_string(),
                operation: "batch".to_string(),
                issued_at: Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap(),
                expires_at: Utc.with_ymd_and_hms(2026, 9, 13, 12, 10, 0).unwrap(),
                permission_revision: "grant:v1".to_string(),
                config_revision: "config:v1".to_string(),
                changes,
                matched_types: vec!["task".to_string()],
                relevant_record_namespaces: vec!["records/**".to_string()],
                generated_values: BTreeMap::new(),
            },
        )
        .expect("build preview")
    }

    fn verification() -> MdbaseWritePreviewVerification<'static> {
        MdbaseWritePreviewVerification {
            caller_id: "caller",
            instance_id: "instance",
            operation: "batch",
            permission_revision: "grant:v1",
            config_revision: "config:v1",
            now: Utc.with_ymd_and_hms(2026, 9, 13, 12, 1, 0).unwrap(),
        }
    }

    #[test]
    fn batch_commits_once_then_replays_durable_receipt() {
        let (directory, paths, collection) = fixture();
        let preview = preview(
            &collection,
            vec![
                MdbaseWritePreviewChangeRequest {
                    path: "records/a.md".to_string(),
                    after: Some("after a\n".to_string()),
                    if_revision: None,
                },
                MdbaseWritePreviewChangeRequest {
                    path: "records/new.md".to_string(),
                    after: Some("new\n".to_string()),
                    if_revision: None,
                },
            ],
        );
        let request = MdbaseWriteApplyRequest {
            preview: &preview,
            verification: verification(),
            idempotency_key: "key-1",
        };
        let mut reconciliations = 0;
        let outcome = apply_mdbase_write_transaction(&paths, &collection, &request, |_| {
            reconciliations += 1;
            Ok(())
        })
        .expect("apply");
        assert_eq!(outcome.status, MdbaseWriteOutcomeStatus::Committed);
        assert!(!outcome.replayed);
        assert_eq!(reconciliations, 1);
        assert_eq!(
            fs::read_to_string(directory.path().join("records/a.md")).unwrap(),
            "after a\n"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("records/new.md")).unwrap(),
            "new\n"
        );
        fs::write(paths.cache_db(), "rebuildable cache bytes").expect("cache fixture");
        fs::remove_file(paths.cache_db()).expect("clear rebuildable cache");
        let replay = apply_mdbase_write_transaction(&paths, &collection, &request, |_| {
            panic!("receipt replay must not reconcile")
        })
        .expect("replay");
        assert!(replay.replayed);
        assert_eq!(replay.transaction_id, outcome.transaction_id);
        assert!(state_root(&paths)
            .join("outbox")
            .join(format!("{}.json", outcome.transaction_id))
            .is_file());
        let events = list_mdbase_write_outbox(&paths).expect("list outbox");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].transaction_id, outcome.transaction_id);
        assert!(
            acknowledge_mdbase_write_outbox(&paths, &outcome.transaction_id)
                .expect("acknowledge event")
        );
        assert!(list_mdbase_write_outbox(&paths)
            .expect("empty outbox")
            .is_empty());
        assert!(
            !acknowledge_mdbase_write_outbox(&paths, &outcome.transaction_id)
                .expect("idempotent acknowledgement")
        );
    }

    #[test]
    fn preflight_runs_once_and_is_skipped_for_idempotent_replay() {
        let (_directory, paths, collection) = fixture();
        let preview = preview(
            &collection,
            vec![MdbaseWritePreviewChangeRequest {
                path: "records/a.md".to_string(),
                after: Some("after a\n".to_string()),
                if_revision: None,
            }],
        );
        let request = MdbaseWriteApplyRequest {
            preview: &preview,
            verification: verification(),
            idempotency_key: "preflight-once",
        };
        let calls = Cell::new(0);
        apply_mdbase_write_transaction_with_preflight(
            &paths,
            &collection,
            &request,
            || {
                calls.set(calls.get() + 1);
                Ok(())
            },
            |_| Ok(()),
        )
        .expect("first apply");
        let replay = apply_mdbase_write_transaction_with_preflight(
            &paths,
            &collection,
            &request,
            || {
                calls.set(calls.get() + 1);
                Ok(())
            },
            |_| Ok(()),
        )
        .expect("replay");

        assert!(replay.replayed);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn precommit_crash_rolls_back_and_postcommit_crash_rolls_forward() {
        let (directory, paths, collection) = fixture();
        let preview = preview(
            &collection,
            vec![
                MdbaseWritePreviewChangeRequest {
                    path: "records/a.md".to_string(),
                    after: Some("after a\n".to_string()),
                    if_revision: None,
                },
                MdbaseWritePreviewChangeRequest {
                    path: "records/b.md".to_string(),
                    after: Some("after b\n".to_string()),
                    if_revision: None,
                },
            ],
        );
        let request = MdbaseWriteApplyRequest {
            preview: &preview,
            verification: verification(),
            idempotency_key: "precommit",
        };
        let mut replacements = 0;
        let interrupted = apply_with_boundary_hook(
            &paths,
            &collection,
            &request,
            |_| Ok(()),
            |boundary| {
                if boundary == "after_replace" {
                    replacements += 1;
                }
                if replacements == 1 {
                    return Err(MdbaseWriteTransactionError::new(
                        "interrupted",
                        "fault injection",
                    ));
                }
                Ok(())
            },
        );
        assert_eq!(interrupted.unwrap_err().code, "interrupted");
        assert_eq!(
            acquire_mdbase_consistent_read(&paths).unwrap_err().code,
            "recovery_required"
        );
        recover_mdbase_write_transaction(&paths, &collection, |_| Ok(()))
            .expect("rollback recovery");
        assert_eq!(
            fs::read_to_string(directory.path().join("records/a.md")).unwrap(),
            "before a\n"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("records/b.md")).unwrap(),
            "before b\n"
        );

        let request = MdbaseWriteApplyRequest {
            preview: &preview,
            verification: verification(),
            idempotency_key: "postcommit",
        };
        let interrupted = apply_with_boundary_hook(
            &paths,
            &collection,
            &request,
            |_| Ok(()),
            |boundary| {
                if boundary == "commit_decided" {
                    return Err(MdbaseWriteTransactionError::new(
                        "interrupted",
                        "fault injection",
                    ));
                }
                Ok(())
            },
        );
        assert_eq!(interrupted.unwrap_err().code, "interrupted");
        let recovered = recover_mdbase_write_transaction(&paths, &collection, |_| Ok(()))
            .expect("roll-forward recovery")
            .expect("committed outcome");
        assert_eq!(recovered.status, MdbaseWriteOutcomeStatus::Committed);
        assert_eq!(
            fs::read_to_string(directory.path().join("records/a.md")).unwrap(),
            "after a\n"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("records/b.md")).unwrap(),
            "after b\n"
        );
    }

    #[test]
    fn recovery_preserves_external_edits_and_blocks_the_transaction() {
        let (directory, paths, collection) = fixture();
        let preview = self::preview(
            &collection,
            vec![MdbaseWritePreviewChangeRequest {
                path: "records/a.md".to_string(),
                after: Some("after a\n".to_string()),
                if_revision: None,
            }],
        );
        let request = MdbaseWriteApplyRequest {
            preview: &preview,
            verification: verification(),
            idempotency_key: "blocked",
        };
        let interrupted = apply_with_boundary_hook(
            &paths,
            &collection,
            &request,
            |_| Ok(()),
            |boundary| {
                if boundary == "after_replace" {
                    return Err(MdbaseWriteTransactionError::new(
                        "interrupted",
                        "fault injection",
                    ));
                }
                Ok(())
            },
        );
        assert!(interrupted.is_err());
        write(directory.path(), "records/a.md", "external\n");
        let error = recover_mdbase_write_transaction(&paths, &collection, |_| Ok(())).unwrap_err();
        assert_eq!(error.code, "recovery_blocked");
        assert_eq!(
            acquire_mdbase_consistent_read(&paths).unwrap_err().code,
            "recovery_blocked"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("records/a.md")).unwrap(),
            "external\n"
        );
        let journal = load_journal(&paths)
            .expect("journal read")
            .expect("blocked journal");
        assert_eq!(journal.phase, JournalPhase::Blocked);
        assert_eq!(
            journal.conflict.unwrap().observed.as_deref(),
            Some("external\n")
        );

        let (directory, paths, collection) = fixture();
        let preview = self::preview(
            &collection,
            vec![MdbaseWritePreviewChangeRequest {
                path: "records/a.md".to_string(),
                after: Some("after a\n".to_string()),
                if_revision: None,
            }],
        );
        let request = MdbaseWriteApplyRequest {
            preview: &preview,
            verification: verification(),
            idempotency_key: "blocked-after-commit",
        };
        apply_with_boundary_hook(
            &paths,
            &collection,
            &request,
            |_| Ok(()),
            |boundary| {
                if boundary == "commit_decided" {
                    return Err(MdbaseWriteTransactionError::new(
                        "interrupted",
                        "fault injection",
                    ));
                }
                Ok(())
            },
        )
        .expect_err("commit boundary interruption");
        write(directory.path(), "records/a.md", "external after commit\n");
        let error = recover_mdbase_write_transaction(&paths, &collection, |_| Ok(())).unwrap_err();
        assert_eq!(error.code, "recovery_blocked");
        assert_eq!(
            fs::read_to_string(directory.path().join("records/a.md")).unwrap(),
            "external after commit\n"
        );
    }

    #[test]
    fn follow_up_failure_is_committed_and_idempotently_replayed() {
        let (directory, paths, collection) = fixture();
        let preview = preview(
            &collection,
            vec![MdbaseWritePreviewChangeRequest {
                path: "records/a.md".to_string(),
                after: Some("after a\n".to_string()),
                if_revision: None,
            }],
        );
        let request = MdbaseWriteApplyRequest {
            preview: &preview,
            verification: verification(),
            idempotency_key: "follow-up",
        };
        let outcome = apply_mdbase_write_transaction(&paths, &collection, &request, |_| {
            Err("cache refresh failed".to_string())
        })
        .expect("committed failure outcome");
        assert_eq!(
            outcome.status,
            MdbaseWriteOutcomeStatus::CommittedWithFollowUpFailure
        );
        assert!(!state_root(&paths)
            .join("outbox")
            .join(format!("{}.json", outcome.transaction_id))
            .exists());
        assert_eq!(
            fs::read_to_string(directory.path().join("records/a.md")).unwrap(),
            "after a\n"
        );
        let recovered = recover_mdbase_write_transaction(&paths, &collection, |_| Ok(()))
            .expect("recovery")
            .expect("outcome");
        assert_eq!(recovered.status, MdbaseWriteOutcomeStatus::Committed);
        assert!(state_root(&paths)
            .join("outbox")
            .join(format!("{}.json", outcome.transaction_id))
            .is_file());
        let replay = apply_mdbase_write_transaction(&paths, &collection, &request, |_| Ok(()))
            .expect("replay");
        assert!(replay.replayed);
        assert_eq!(
            replay.status,
            MdbaseWriteOutcomeStatus::CommittedWithFollowUpFailure
        );
    }

    #[test]
    fn idempotency_key_reuse_with_different_input_fails() {
        let (_directory, paths, collection) = fixture();
        let first = preview(
            &collection,
            vec![MdbaseWritePreviewChangeRequest {
                path: "records/a.md".to_string(),
                after: Some("after a\n".to_string()),
                if_revision: None,
            }],
        );
        let request = MdbaseWriteApplyRequest {
            preview: &first,
            verification: verification(),
            idempotency_key: "same-key",
        };
        apply_mdbase_write_transaction(&paths, &collection, &request, |_| Ok(()))
            .expect("first apply");
        let mut second = first.clone();
        second.plan_id = "different-plan".to_string();
        second.digest = super::super::write_preview::preview_digest_for_test(&second);
        let request = MdbaseWriteApplyRequest {
            preview: &second,
            verification: verification(),
            idempotency_key: "same-key",
        };
        let error =
            apply_mdbase_write_transaction(&paths, &collection, &request, |_| Ok(())).unwrap_err();
        assert_eq!(error.code, "idempotency_conflict");
    }

    #[test]
    fn limits_are_rejected_before_a_journal_is_created() {
        let (_directory, paths, collection) = fixture();
        let mut preview = preview(
            &collection,
            vec![MdbaseWritePreviewChangeRequest {
                path: "records/a.md".to_string(),
                after: Some("after a\n".to_string()),
                if_revision: None,
            }],
        );
        preview.changes = (0..=MDBASE_WRITE_MAX_CHANGED_FILES)
            .map(|index| MdbaseWritePreviewChange {
                path: format!("records/{index}.md"),
                before: None,
                after: Some(String::new()),
                before_revision: None,
                if_revision: None,
            })
            .collect();
        preview.digest = super::super::write_preview::preview_digest_for_test(&preview);
        let request = MdbaseWriteApplyRequest {
            preview: &preview,
            verification: verification(),
            idempotency_key: "too-many",
        };
        let error =
            apply_mdbase_write_transaction(&paths, &collection, &request, |_| Ok(())).unwrap_err();
        assert_eq!(error.code, "limit_exceeded");
        assert!(!journal_path(&paths).exists());
    }

    #[test]
    fn missing_parent_directories_are_explicit_and_removed_on_rollback() {
        let (directory, paths, collection) = fixture();
        let preview = preview(
            &collection,
            vec![MdbaseWritePreviewChangeRequest {
                path: "new/deep/record.md".to_string(),
                after: Some("new\n".to_string()),
                if_revision: None,
            }],
        );
        let request = MdbaseWriteApplyRequest {
            preview: &preview,
            verification: verification(),
            idempotency_key: "directories",
        };
        let interrupted = apply_with_boundary_hook(
            &paths,
            &collection,
            &request,
            |_| Ok(()),
            |boundary| {
                if boundary == "after_replace" {
                    return Err(MdbaseWriteTransactionError::new(
                        "interrupted",
                        "fault injection",
                    ));
                }
                Ok(())
            },
        );
        assert!(interrupted.is_err());
        let journal = load_journal(&paths).unwrap().unwrap();
        assert_eq!(journal.planned_directories, ["new", "new/deep"]);
        recover_mdbase_write_transaction(&paths, &collection, |_| Ok(())).expect("rollback");
        assert!(!directory.path().join("new").exists());
    }

    #[test]
    fn every_durable_boundary_recovers_in_the_decided_direction() {
        let boundaries = [
            ("journal_prepared", "before a\n"),
            ("replacements_staged", "before a\n"),
            ("before_replace", "before a\n"),
            ("after_replace", "before a\n"),
            ("commit_decided", "after a\n"),
            ("reconciled", "after a\n"),
            ("outbox_written", "after a\n"),
        ];
        for (fault_boundary, expected) in boundaries {
            let (directory, paths, collection) = fixture();
            let preview = preview(
                &collection,
                vec![MdbaseWritePreviewChangeRequest {
                    path: "records/a.md".to_string(),
                    after: Some("after a\n".to_string()),
                    if_revision: None,
                }],
            );
            let request = MdbaseWriteApplyRequest {
                preview: &preview,
                verification: verification(),
                idempotency_key: fault_boundary,
            };
            let interrupted = apply_with_boundary_hook(
                &paths,
                &collection,
                &request,
                |_| Ok(()),
                |boundary| {
                    if boundary == fault_boundary {
                        return Err(MdbaseWriteTransactionError::new(
                            "interrupted",
                            "fault injection",
                        ));
                    }
                    Ok(())
                },
            );
            assert_eq!(interrupted.unwrap_err().code, "interrupted");
            recover_mdbase_write_transaction(&paths, &collection, |_| Ok(()))
                .expect("boundary recovery");
            assert_eq!(
                fs::read_to_string(directory.path().join("records/a.md")).unwrap(),
                expected,
                "wrong recovery direction after {fault_boundary}"
            );
            assert!(!journal_path(&paths).exists());
        }
    }

    #[test]
    fn first_path_revision_race_preserves_external_bytes_without_a_journal() {
        let (directory, paths, collection) = fixture();
        let preview = preview(
            &collection,
            vec![MdbaseWritePreviewChangeRequest {
                path: "records/a.md".to_string(),
                after: Some("planned\n".to_string()),
                if_revision: Some(mdbase_content_revision("before a\n")),
            }],
        );
        let request = MdbaseWriteApplyRequest {
            preview: &preview,
            verification: verification(),
            idempotency_key: "revision-race",
        };

        let error = apply_with_boundary_hook(
            &paths,
            &collection,
            &request,
            |_| Ok(()),
            |boundary| {
                if boundary == "before_replace" {
                    write(directory.path(), "records/a.md", "external\n");
                }
                Ok(())
            },
        )
        .expect_err("revision race must fail");

        assert_eq!(error.code, "concurrent_modification");
        assert_eq!(error.path.as_deref(), Some("records/a.md"));
        assert_eq!(
            fs::read_to_string(directory.path().join("records/a.md")).expect("external source"),
            "external\n"
        );
        assert!(!journal_path(&paths).exists());
        assert!(list_mdbase_write_outbox(&paths).expect("outbox").is_empty());
    }

    #[test]
    fn concurrent_creator_and_phantom_membership_are_preserved_and_rejected() {
        let (directory, paths, collection) = fixture();
        let create_preview = preview(
            &collection,
            vec![MdbaseWritePreviewChangeRequest {
                path: "records/new.md".to_string(),
                after: Some("planned\n".to_string()),
                if_revision: None,
            }],
        );
        let request = MdbaseWriteApplyRequest {
            preview: &create_preview,
            verification: verification(),
            idempotency_key: "concurrent-create",
        };
        let error = apply_with_boundary_hook(
            &paths,
            &collection,
            &request,
            |_| Ok(()),
            |boundary| {
                if boundary == "before_replace" {
                    write(directory.path(), "records/new.md", "external\n");
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "recovery_blocked");
        assert_eq!(
            fs::read_to_string(directory.path().join("records/new.md")).unwrap(),
            "external\n"
        );

        let (directory, paths, collection) = fixture();
        let update_preview = preview(
            &collection,
            vec![MdbaseWritePreviewChangeRequest {
                path: "records/a.md".to_string(),
                after: Some("planned\n".to_string()),
                if_revision: None,
            }],
        );
        write(
            directory.path(),
            "records/phantom.md",
            "duplicate candidate\n",
        );
        let request = MdbaseWriteApplyRequest {
            preview: &update_preview,
            verification: verification(),
            idempotency_key: "phantom",
        };
        let error =
            apply_mdbase_write_transaction(&paths, &collection, &request, |_| Ok(())).unwrap_err();
        assert_eq!(error.code, "stale_state");
        assert_eq!(
            fs::read_to_string(directory.path().join("records/a.md")).unwrap(),
            "before a\n"
        );
        recover_mdbase_write_transaction(&paths, &collection, |_| Ok(()))
            .expect("clear stale staged transaction");
    }

    #[test]
    fn case_only_rename_is_explicit_and_orders_delete_before_create() {
        let (directory, paths, collection) = fixture();
        write(directory.path(), "records/Name.md", "rename me\n");
        let rename_preview = preview(
            &collection,
            vec![
                MdbaseWritePreviewChangeRequest {
                    path: "records/Name.md".to_string(),
                    after: None,
                    if_revision: None,
                },
                MdbaseWritePreviewChangeRequest {
                    path: "records/name.md".to_string(),
                    after: Some("rename me\n".to_string()),
                    if_revision: None,
                },
            ],
        );
        let request = MdbaseWriteApplyRequest {
            preview: &rename_preview,
            verification: verification(),
            idempotency_key: "case-only",
        };
        let mut saw_explicit_rename = false;
        apply_with_boundary_hook(
            &paths,
            &collection,
            &request,
            |_| Ok(()),
            |boundary| {
                if boundary == "journal_prepared" {
                    let journal = load_journal(&paths)?.expect("journal exists");
                    assert_eq!(
                        journal.case_only_renames,
                        [CaseOnlyRename {
                            from: "records/Name.md".to_string(),
                            to: "records/name.md".to_string(),
                        }]
                    );
                    saw_explicit_rename = true;
                }
                Ok(())
            },
        )
        .expect("case-only rename");
        assert!(saw_explicit_rename);
        assert!(!directory_entry_names(directory.path(), "records")
            .iter()
            .any(|name| name == "Name.md"));
        assert!(directory_entry_names(directory.path(), "records")
            .iter()
            .any(|name| name == "name.md"));
        assert_eq!(
            fs::read_to_string(directory.path().join("records/name.md")).unwrap(),
            "rename me\n"
        );
    }

    #[test]
    fn interrupted_case_only_rename_restores_the_original_spelling() {
        let (directory, paths, collection) = fixture();
        write(directory.path(), "records/Name.md", "rename me\n");
        let rename_preview = preview(
            &collection,
            vec![
                MdbaseWritePreviewChangeRequest {
                    path: "records/Name.md".to_string(),
                    after: None,
                    if_revision: None,
                },
                MdbaseWritePreviewChangeRequest {
                    path: "records/name.md".to_string(),
                    after: Some("rename me\n".to_string()),
                    if_revision: None,
                },
            ],
        );
        let request = MdbaseWriteApplyRequest {
            preview: &rename_preview,
            verification: verification(),
            idempotency_key: "case-only-rollback",
        };
        let mut replacements = 0;
        let interrupted = apply_with_boundary_hook(
            &paths,
            &collection,
            &request,
            |_| Ok(()),
            |boundary| {
                if boundary == "after_replace" {
                    replacements += 1;
                    if replacements == 1 {
                        return Err(MdbaseWriteTransactionError::new(
                            "interrupted",
                            "fault injection",
                        ));
                    }
                }
                Ok(())
            },
        );
        assert_eq!(interrupted.unwrap_err().code, "interrupted");

        recover_mdbase_write_transaction(&paths, &collection, |_| Ok(()))
            .expect("roll back case-only rename");
        let names = directory_entry_names(directory.path(), "records");
        assert!(names.iter().any(|name| name == "Name.md"));
        assert!(!names.iter().any(|name| name == "name.md"));
        assert_eq!(
            fs::read_to_string(directory.path().join("records/Name.md")).unwrap(),
            "rename me\n"
        );
    }
}
