//! Durable identity and recovery projections for hosted non-sync operations.
//!
//! File-tree sync remains owned by `SyncSupervisor`; this ledger gives other
//! hosted work the same observable identity without changing sync coalescing,
//! replay, or journal semantics.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tempfile::NamedTempFile;
use ulid::Ulid;
use vulcan_app::execution::{ExecutionCancellationToken, ExecutionContext, ExecutionRetryClass};

pub const HOSTED_JOB_VERSION: u32 = 1;
const MAX_JOB_BYTES: u64 = 256 * 1024;
const MAX_DETAIL_BYTES: usize = 4096;
const MAX_RECOVERY_RECORDS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedJobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Indeterminate,
    Interrupted,
}

impl HostedJobState {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Interrupted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedRetryDisposition {
    NotApplicable,
    StatusCheckThenRetry,
    ResumeDurableRecovery,
    DoNotRetryDirectly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostedJobRecord {
    pub version: u32,
    pub operation_id: String,
    pub request_id: String,
    pub service_instance_id: String,
    pub canonical_vault: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_key: Option<String>,
    pub retry_class: ExecutionRetryClass,
    pub state: HostedJobState,
    pub retry_disposition: HostedRetryDisposition,
    pub cancel_requested: bool,
    pub dispatched: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committed: Option<bool>,
    pub updated_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl HostedJobRecord {
    fn queued(context: &ExecutionContext, now_unix_ms: u64) -> Self {
        Self {
            version: HOSTED_JOB_VERSION,
            operation_id: context.identity.operation_id.clone(),
            request_id: context.identity.request_id.clone(),
            service_instance_id: context.identity.service_instance_id.clone(),
            canonical_vault: context.vault.canonical_root.clone(),
            repository_key: context
                .vault
                .repository
                .as_ref()
                .map(|repository| repository.key.clone()),
            retry_class: context.retry_class,
            state: HostedJobState::Queued,
            retry_disposition: HostedRetryDisposition::NotApplicable,
            cancel_requested: false,
            dispatched: false,
            committed: None,
            updated_unix_ms: now_unix_ms,
            detail: None,
        }
    }
}

#[derive(Debug)]
pub struct HostedJobLedger {
    root: PathBuf,
    active: Mutex<BTreeMap<String, ExecutionCancellationToken>>,
    storage: Mutex<()>,
}

impl HostedJobLedger {
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            active: Mutex::new(BTreeMap::new()),
            storage: Mutex::new(()),
        }
    }

    pub fn register(
        &self,
        context: &ExecutionContext,
        now_unix_ms: u64,
    ) -> Result<HostedJobRecord, HostedJobError> {
        validate_operation_id(&context.identity.operation_id)?;
        let _storage = self.storage.lock().map_err(|_| HostedJobError::Poisoned)?;
        let path = self.path(&context.identity.operation_id);
        if path.exists() {
            return Err(HostedJobError::DuplicateOperation(
                context.identity.operation_id.clone(),
            ));
        }
        let record = HostedJobRecord::queued(context, now_unix_ms);
        self.save_new(&record)?;
        self.active
            .lock()
            .map_err(|_| HostedJobError::Poisoned)?
            .insert(record.operation_id.clone(), context.cancellation.clone());
        Ok(record)
    }

    pub fn load(&self, operation_id: &str) -> Result<HostedJobRecord, HostedJobError> {
        validate_operation_id(operation_id)?;
        let _storage = self.storage.lock().map_err(|_| HostedJobError::Poisoned)?;
        load_record(&self.path(operation_id), operation_id)
    }

    pub fn mark_running(
        &self,
        operation_id: &str,
        now_unix_ms: u64,
    ) -> Result<HostedJobRecord, HostedJobError> {
        self.update(operation_id, now_unix_ms, |record| {
            require_state(record, &[HostedJobState::Queued])?;
            record.state = HostedJobState::Running;
            record.dispatched = true;
            Ok(())
        })
    }

    pub fn mark_succeeded(
        &self,
        operation_id: &str,
        committed: bool,
        now_unix_ms: u64,
    ) -> Result<HostedJobRecord, HostedJobError> {
        self.finish(operation_id, now_unix_ms, |record| {
            require_state(
                record,
                &[HostedJobState::Running, HostedJobState::Indeterminate],
            )?;
            record.state = HostedJobState::Succeeded;
            record.committed = Some(committed);
            record.retry_disposition = HostedRetryDisposition::NotApplicable;
            record.detail = None;
            Ok(())
        })
    }

    pub fn mark_failed(
        &self,
        operation_id: &str,
        committed: Option<bool>,
        detail: impl Into<String>,
        now_unix_ms: u64,
    ) -> Result<HostedJobRecord, HostedJobError> {
        let detail = bounded_detail(detail.into());
        self.finish(operation_id, now_unix_ms, |record| {
            require_state(
                record,
                &[
                    HostedJobState::Queued,
                    HostedJobState::Running,
                    HostedJobState::Indeterminate,
                ],
            )?;
            record.state = HostedJobState::Failed;
            record.committed = committed;
            record.retry_disposition = if committed == Some(false) {
                retry_after_known_failure(record.retry_class)
            } else {
                HostedRetryDisposition::DoNotRetryDirectly
            };
            record.detail = Some(detail);
            Ok(())
        })
    }

    pub fn mark_indeterminate(
        &self,
        operation_id: &str,
        detail: impl Into<String>,
        now_unix_ms: u64,
    ) -> Result<HostedJobRecord, HostedJobError> {
        let detail = bounded_detail(detail.into());
        self.update(operation_id, now_unix_ms, |record| {
            require_state(record, &[HostedJobState::Running])?;
            record.state = HostedJobState::Indeterminate;
            record.committed = None;
            record.retry_disposition = HostedRetryDisposition::DoNotRetryDirectly;
            record.detail = Some(detail);
            Ok(())
        })
    }

    pub fn request_cancel(
        &self,
        operation_id: &str,
        now_unix_ms: u64,
    ) -> Result<HostedJobRecord, HostedJobError> {
        let token = self
            .active
            .lock()
            .map_err(|_| HostedJobError::Poisoned)?
            .get(operation_id)
            .cloned();
        if let Some(token) = token {
            token.cancel();
        }
        self.update(operation_id, now_unix_ms, |record| {
            if record.state.is_terminal() {
                return Err(HostedJobError::InvalidTransition {
                    operation_id: operation_id.to_string(),
                    from: record.state,
                });
            }
            record.cancel_requested = true;
            Ok(())
        })
    }

    /// Marks work left non-terminal by a prior process. It never reruns it.
    pub fn recover_interrupted(
        &self,
        now_unix_ms: u64,
    ) -> Result<Vec<HostedJobRecord>, HostedJobError> {
        let mut recovered = Vec::new();
        if !self.root.exists() {
            return Ok(recovered);
        }
        let _storage = self.storage.lock().map_err(|_| HostedJobError::Poisoned)?;
        for entry in fs::read_dir(&self.root).map_err(HostedJobError::Io)? {
            if recovered.len() >= MAX_RECOVERY_RECORDS {
                return Err(HostedJobError::TooManyRecords);
            }
            let entry = entry.map_err(HostedJobError::Io)?;
            let metadata = entry.metadata().map_err(HostedJobError::Io)?;
            if !metadata.is_file()
                || entry.path().extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let Some(operation_id) = entry
                .path()
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            validate_operation_id(&operation_id)?;
            let mut record = load_record(&entry.path(), &operation_id)?;
            if record.state.is_terminal() {
                continue;
            }
            let previous = record.state;
            record.state = HostedJobState::Interrupted;
            record.retry_disposition = if record.dispatched {
                retry_after_interruption(record.retry_class)
            } else {
                HostedRetryDisposition::StatusCheckThenRetry
            };
            record.committed = None;
            record.updated_unix_ms = now_unix_ms;
            record.detail = Some(format!(
                "host restarted while operation was {previous:?}; execution was not replayed"
            ));
            self.save_replace(&record)?;
            recovered.push(record);
        }
        recovered.sort_by(|left, right| left.operation_id.cmp(&right.operation_id));
        Ok(recovered)
    }

    fn finish<F>(
        &self,
        operation_id: &str,
        now_unix_ms: u64,
        update: F,
    ) -> Result<HostedJobRecord, HostedJobError>
    where
        F: FnOnce(&mut HostedJobRecord) -> Result<(), HostedJobError>,
    {
        let record = self.update(operation_id, now_unix_ms, update)?;
        self.active
            .lock()
            .map_err(|_| HostedJobError::Poisoned)?
            .remove(operation_id);
        Ok(record)
    }

    fn update<F>(
        &self,
        operation_id: &str,
        now_unix_ms: u64,
        update: F,
    ) -> Result<HostedJobRecord, HostedJobError>
    where
        F: FnOnce(&mut HostedJobRecord) -> Result<(), HostedJobError>,
    {
        validate_operation_id(operation_id)?;
        let _storage = self.storage.lock().map_err(|_| HostedJobError::Poisoned)?;
        let mut record = load_record(&self.path(operation_id), operation_id)?;
        update(&mut record)?;
        record.updated_unix_ms = now_unix_ms;
        self.save_replace(&record)?;
        Ok(record)
    }

    fn temporary_record(&self, record: &HostedJobRecord) -> Result<NamedTempFile, HostedJobError> {
        fs::create_dir_all(&self.root).map_err(HostedJobError::Io)?;
        set_owner_only_directory(&self.root)?;
        let bytes = serde_json::to_vec_pretty(record).map_err(HostedJobError::Json)?;
        if bytes.len() as u64 > MAX_JOB_BYTES {
            return Err(HostedJobError::TooLarge);
        }
        let mut temporary = NamedTempFile::new_in(&self.root).map_err(HostedJobError::Io)?;
        temporary.write_all(&bytes).map_err(HostedJobError::Io)?;
        temporary.write_all(b"\n").map_err(HostedJobError::Io)?;
        set_owner_only_file(temporary.as_file())?;
        Ok(temporary)
    }

    fn save_new(&self, record: &HostedJobRecord) -> Result<(), HostedJobError> {
        let temporary = self.temporary_record(record)?;
        temporary
            .persist_noclobber(self.path(&record.operation_id))
            .map_err(|error| {
                if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                    HostedJobError::DuplicateOperation(record.operation_id.clone())
                } else {
                    HostedJobError::Io(error.error)
                }
            })?;
        Ok(())
    }

    fn save_replace(&self, record: &HostedJobRecord) -> Result<(), HostedJobError> {
        let temporary = self.temporary_record(record)?;
        temporary
            .persist(self.path(&record.operation_id))
            .map_err(|error| HostedJobError::Io(error.error))?;
        Ok(())
    }

    fn path(&self, operation_id: &str) -> PathBuf {
        self.root.join(format!("{operation_id}.json"))
    }
}

fn require_state(
    record: &HostedJobRecord,
    expected: &[HostedJobState],
) -> Result<(), HostedJobError> {
    if expected.contains(&record.state) {
        Ok(())
    } else {
        Err(HostedJobError::InvalidTransition {
            operation_id: record.operation_id.clone(),
            from: record.state,
        })
    }
}

fn retry_after_known_failure(class: ExecutionRetryClass) -> HostedRetryDisposition {
    match class {
        ExecutionRetryClass::ReadOnly | ExecutionRetryClass::Idempotent => {
            HostedRetryDisposition::StatusCheckThenRetry
        }
        ExecutionRetryClass::DurableRecovery => HostedRetryDisposition::ResumeDurableRecovery,
        ExecutionRetryClass::IndeterminateAfterDispatch => {
            HostedRetryDisposition::DoNotRetryDirectly
        }
    }
}

fn retry_after_interruption(class: ExecutionRetryClass) -> HostedRetryDisposition {
    retry_after_known_failure(class)
}

fn validate_operation_id(value: &str) -> Result<(), HostedJobError> {
    Ulid::from_string(&value.to_ascii_uppercase())
        .map(|_| ())
        .map_err(|_| HostedJobError::InvalidOperationId(value.to_string()))
}

fn bounded_detail(mut detail: String) -> String {
    if detail.len() <= MAX_DETAIL_BYTES {
        return detail;
    }
    let mut end = MAX_DETAIL_BYTES;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    detail.truncate(end);
    detail
}

fn load_record(
    path: &Path,
    expected_operation_id: &str,
) -> Result<HostedJobRecord, HostedJobError> {
    let metadata = fs::symlink_metadata(path).map_err(HostedJobError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(HostedJobError::UnsafePath(path.to_path_buf()));
    }
    if metadata.len() > MAX_JOB_BYTES {
        return Err(HostedJobError::TooLarge);
    }
    let file = File::open(path).map_err(HostedJobError::Io)?;
    let mut bytes = Vec::new();
    file.take(MAX_JOB_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(HostedJobError::Io)?;
    if bytes.len() as u64 > MAX_JOB_BYTES {
        return Err(HostedJobError::TooLarge);
    }
    let record: HostedJobRecord = serde_json::from_slice(&bytes).map_err(HostedJobError::Json)?;
    if record.version != HOSTED_JOB_VERSION {
        return Err(HostedJobError::UnsupportedVersion(record.version));
    }
    validate_operation_id(&record.operation_id)?;
    if record.operation_id != expected_operation_id {
        return Err(HostedJobError::IdentityMismatch {
            expected: expected_operation_id.to_string(),
            actual: record.operation_id,
        });
    }
    Ok(record)
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> Result<(), HostedJobError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(HostedJobError::Io)
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn set_owner_only_directory(_path: &Path) -> Result<(), HostedJobError> {
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_file(file: &File) -> Result<(), HostedJobError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(HostedJobError::Io)
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn set_owner_only_file(_file: &File) -> Result<(), HostedJobError> {
    Ok(())
}

#[derive(Debug)]
pub enum HostedJobError {
    InvalidOperationId(String),
    DuplicateOperation(String),
    InvalidTransition {
        operation_id: String,
        from: HostedJobState,
    },
    UnsupportedVersion(u32),
    IdentityMismatch {
        expected: String,
        actual: String,
    },
    UnsafePath(PathBuf),
    TooLarge,
    TooManyRecords,
    Io(std::io::Error),
    Json(serde_json::Error),
    Poisoned,
}

impl Display for HostedJobError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOperationId(id) => write!(formatter, "invalid hosted operation id `{id}`"),
            Self::DuplicateOperation(id) => {
                write!(formatter, "hosted operation `{id}` already exists")
            }
            Self::InvalidTransition { operation_id, from } => write!(
                formatter,
                "hosted operation `{operation_id}` cannot transition from {from:?}"
            ),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported hosted job version {version}")
            }
            Self::IdentityMismatch { expected, actual } => write!(
                formatter,
                "hosted job file identity `{expected}` does not match record identity `{actual}`"
            ),
            Self::UnsafePath(path) => {
                write!(formatter, "unsafe hosted job path `{}`", path.display())
            }
            Self::TooLarge => formatter.write_str("hosted job record exceeds its size limit"),
            Self::TooManyRecords => {
                formatter.write_str("hosted job recovery record limit exceeded")
            }
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Json(error) => Display::fmt(error, formatter),
            Self::Poisoned => formatter.write_str("hosted job state lock is poisoned"),
        }
    }
}

impl std::error::Error for HostedJobError {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vulcan_app::execution::{ExecutionAuthority, ExecutionIdentity, ExecutionVaultIdentity};
    use vulcan_core::{PathPermission, PermissionGrant, ResourceLimits, ResourceSpecifier};

    fn grant() -> PermissionGrant {
        PermissionGrant {
            read: PathPermission {
                allow: vec![ResourceSpecifier::All],
                deny: Vec::new(),
            },
            write: PathPermission::default(),
            refactor: PathPermission::default(),
            git: false,
            network: false,
            network_domains: Vec::new(),
            index: false,
            config_read: false,
            config_write: false,
            execute: false,
            shell: false,
            limits: ResourceLimits::default(),
        }
    }

    fn context(vault: &TempDir, class: ExecutionRetryClass) -> ExecutionContext {
        ExecutionContext::new(
            ExecutionVaultIdentity::resolve(vault.path(), None, None).expect("vault"),
            ExecutionAuthority::Caller {
                principal_id: "test".to_string(),
                credential_id: None,
                permission_ceiling: grant(),
            },
            grant(),
            ExecutionIdentity::new("daemon:test"),
            None,
            class,
            ExecutionCancellationToken::default(),
            None,
        )
        .expect("context")
    }

    #[test]
    fn lifecycle_is_durable_and_cancellation_reaches_the_active_token() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let vault = tempfile::tempdir().expect("vault");
        let ledger = HostedJobLedger::at(temporary.path());
        let context = context(&vault, ExecutionRetryClass::IndeterminateAfterDispatch);
        let operation = context.identity.operation_id.clone();

        ledger.register(&context, 1).expect("register");
        ledger.mark_running(&operation, 2).expect("running");
        let cancelled = ledger.request_cancel(&operation, 3).expect("cancel");
        assert!(cancelled.cancel_requested);
        assert!(context.cancellation.is_cancelled());
        let completed = ledger
            .mark_succeeded(&operation, true, 4)
            .expect("complete");
        assert_eq!(completed.state, HostedJobState::Succeeded);
        assert_eq!(completed.committed, Some(true));
        assert_eq!(ledger.load(&operation).expect("reload"), completed);
    }

    #[test]
    fn restart_classifies_interruption_without_replaying_operations() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let vault = tempfile::tempdir().expect("vault");
        let ledger = HostedJobLedger::at(temporary.path());
        let recoverable = context(&vault, ExecutionRetryClass::DurableRecovery);
        let unknown = context(&vault, ExecutionRetryClass::IndeterminateAfterDispatch);
        ledger.register(&recoverable, 1).expect("recoverable");
        ledger
            .mark_running(&recoverable.identity.operation_id, 2)
            .expect("running");
        ledger.register(&unknown, 3).expect("unknown");
        ledger
            .mark_running(&unknown.identity.operation_id, 4)
            .expect("running");

        let restarted = HostedJobLedger::at(temporary.path());
        let recovered = restarted.recover_interrupted(5).expect("recover");
        assert_eq!(recovered.len(), 2);
        let by_id = recovered
            .into_iter()
            .map(|record| (record.operation_id.clone(), record))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            by_id[&recoverable.identity.operation_id].retry_disposition,
            HostedRetryDisposition::ResumeDurableRecovery
        );
        assert_eq!(
            by_id[&unknown.identity.operation_id].retry_disposition,
            HostedRetryDisposition::DoNotRetryDirectly
        );
        assert!(by_id.values().all(|record| {
            record.state == HostedJobState::Interrupted && record.committed.is_none()
        }));
        assert!(restarted
            .recover_interrupted(6)
            .expect("idempotent")
            .is_empty());
    }

    #[test]
    fn queued_restart_is_known_pre_dispatch_and_duplicate_ids_are_rejected() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let vault = tempfile::tempdir().expect("vault");
        let ledger = HostedJobLedger::at(temporary.path());
        let context = context(&vault, ExecutionRetryClass::IndeterminateAfterDispatch);
        ledger.register(&context, 1).expect("register");
        assert!(matches!(
            ledger.register(&context, 2),
            Err(HostedJobError::DuplicateOperation(_))
        ));
        let record = HostedJobLedger::at(temporary.path())
            .recover_interrupted(3)
            .expect("recover")
            .pop()
            .expect("record");
        assert!(!record.dispatched);
        assert_eq!(
            record.retry_disposition,
            HostedRetryDisposition::StatusCheckThenRetry
        );
    }

    #[cfg(unix)]
    #[test]
    fn persisted_records_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().expect("temporary directory");
        let vault = tempfile::tempdir().expect("vault");
        let ledger = HostedJobLedger::at(temporary.path().join("jobs"));
        let context = context(&vault, ExecutionRetryClass::ReadOnly);
        ledger.register(&context, 1).expect("register");
        assert_eq!(
            fs::metadata(temporary.path().join("jobs"))
                .expect("directory")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(ledger.path(&context.identity.operation_id))
                .expect("record")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
