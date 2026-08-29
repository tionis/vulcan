use crate::{
    GitCaptureRequest, GitContentMergeResolutionRequest, GitEngine, GitEngineError,
    GitInstallation, GitOid, GitPathObject, GitPushResult, GitRefName, GitRemote, GitRepository,
    GitResolvedPath, GitSafetyState, GitTreeApplyPlan, MergeAutomation, MergeFileKind, MergePolicy,
    MergeResolution, SyncAction, SyncBackend, SyncCapabilities, SyncCapability, SyncConflict,
    SyncContext, SyncError, SyncErrorCategory, SyncOperation, SyncOperationMode, SyncOutcome,
    SyncPlan, SyncProgress, SyncReport, SyncResolutionState, SyncState, SyncStatus,
    SYNC_CONTRACT_VERSION,
};
use fs2::FileExt;
use serde::Serialize;
use std::collections::HashSet;
use std::error::Error;
use std::fmt::{Display, Formatter, Write as _};
use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const SYNC_PROTOCOL_VERSION: u32 = 1;
const DEFAULT_LIVE_REF: &str = "refs/heads/__vulcan-sync/live";

#[must_use]
pub fn git_live_epoch_id(profile: &str, previous: &GitOid) -> String {
    blake3::hash(
        format!("vulcan-sync-epoch-v{SYNC_PROTOCOL_VERSION}\0{profile}\0{previous}").as_bytes(),
    )
    .to_hex()[..32]
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct GitSyncDeviceId(String);

impl GitSyncDeviceId {
    pub fn parse(value: impl Into<String>) -> Result<Self, GitSyncError> {
        let value = value.into().to_ascii_lowercase();
        let valid = value.len() == 26
            && value.bytes().all(|byte| {
                byte.is_ascii_digit()
                    || matches!(byte, b'a'..=b'h' | b'j'..=b'k' | b'm'..=b'n' | b'p'..=b't' | b'v'..=b'z')
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(GitSyncError::Git(GitEngineError::UnsupportedRepository {
                detail: "sync device identity must be a 26-character Crockford Base32 ULID"
                    .to_string(),
            }))
        }
    }

    #[must_use]
    pub fn anonymous() -> Self {
        Self("00000000000000000000000000".to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSyncOptions {
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub max_retries: usize,
    pub dry_run: bool,
    pub device_id: GitSyncDeviceId,
    pub merge_policy: MergePolicy,
    pub merge_automation: MergeAutomation,
}

impl Default for GitSyncOptions {
    fn default() -> Self {
        Self {
            remote: GitRemote::parse("origin").expect("the default Git remote is valid"),
            live_ref: GitRefName::parse(DEFAULT_LIVE_REF).expect("the default live ref is valid"),
            max_retries: 4,
            dry_run: false,
            device_id: GitSyncDeviceId::anonymous(),
            merge_policy: MergePolicy::default(),
            merge_automation: MergeAutomation::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitSyncOutcome {
    Planned,
    Paused,
    UpToDate,
    Bootstrapped,
    Pushed,
    Pulled,
    Merged,
    Conflicted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitSyncAction {
    SnapshotCreated,
    Pushed,
    WorktreeApplied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitSyncPhase {
    Preparing,
    Capturing,
    Captured,
    Fetching,
    Fetched,
    Merging,
    Pushing,
    Applying,
    Verifying,
    Paused,
    Conflicted,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitSyncProgress {
    pub phase: GitSyncPhase,
    pub attempt: usize,
    pub repository: GitRepository,
    pub local_snapshot: Option<GitOid>,
    pub local_tree: Option<GitOid>,
    pub accepted: Option<GitOid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSyncObserverError {
    detail: String,
}

impl GitSyncObserverError {
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl Display for GitSyncObserverError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl Error for GitSyncObserverError {}

pub trait GitSyncObserver {
    fn progress(&mut self, progress: &GitSyncProgress) -> Result<(), GitSyncObserverError>;

    fn validate_automatic_merge(
        &mut self,
        _engine: &dyn GitEngine,
        _request: &GitAutomaticMergeValidation<'_>,
    ) -> Result<Vec<GitAutomaticValidationCheck>, GitSyncObserverError> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Default)]
pub struct IgnoreGitSyncProgress;

impl GitSyncObserver for IgnoreGitSyncProgress {
    fn progress(&mut self, _progress: &GitSyncProgress) -> Result<(), GitSyncObserverError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct SyncCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl SyncCancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn check(&self) -> Result<(), GitSyncError> {
        if self.is_cancelled() {
            Err(GitSyncError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitSyncRefs {
    pub live: GitRefName,
    pub local: GitRefName,
    pub fetched: GitRefName,
    pub pending: GitRefName,
}

impl GitSyncRefs {
    pub fn for_options(options: &GitSyncOptions) -> Result<Self, GitSyncError> {
        let profile =
            blake3::hash(format!("{}\0{}", options.remote, options.live_ref).as_bytes()).to_hex();
        let profile = &profile[..16];
        Ok(Self {
            live: options.live_ref.clone(),
            local: GitRefName::parse(format!("refs/vulcan/sync/{profile}/local/live"))?,
            fetched: GitRefName::parse(format!("refs/vulcan/sync/{profile}/remotes/live"))?,
            pending: GitRefName::parse(format!("refs/vulcan/sync/{profile}/pending/live"))?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitConflictRefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<GitRefName>,
    pub local: GitRefName,
    pub remote: GitRefName,
    pub record: GitRefName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitSyncConflict {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<GitOid>,
    pub remote: GitOid,
    pub local: GitOid,
    pub paths: Vec<String>,
    pub classifications: Vec<GitConflictClassification>,
    pub policy_version: u32,
    pub policy_hash: String,
    pub preserved_refs: GitConflictRefs,
    pub provenance_revision: GitOid,
    pub merge_tree: Option<GitOid>,
    pub diagnostics: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitConflictClass {
    OverlappingText,
    OverlappingBinary,
    DeleteModify,
    RenameRename,
    DirectoryFile,
    CaseCollision,
    DeviceLocalState,
    UnsupportedObject,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct GitConflictClassification {
    pub path: String,
    pub class: GitConflictClass,
    pub file_kind: MergeFileKind,
    pub rule_id: String,
    pub configured_resolution: MergeResolution,
    pub effective_resolution: MergeResolution,
    pub diagnostic_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitAutomaticResolution {
    pub path: String,
    pub kind: MergeFileKind,
    pub rule_id: String,
    pub validation: GitAutomaticResolutionValidation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitAutomaticResolutionValidation {
    pub checks: Vec<GitAutomaticValidationCheck>,
}

pub struct GitAutomaticMergeValidation<'a> {
    pub repository: &'a GitRepository,
    pub base: &'a GitOid,
    pub local_candidate: &'a GitOid,
    pub accepted_remote: &'a GitOid,
    pub merged_tree: &'a GitOid,
    pub resolved_paths: &'a [String],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitAutomaticValidationCheck {
    PathSafe,
    SyntaxValid,
    SchemaValid,
    MarkdownLinksPreserved,
    ExactTreeObject,
    NoFileDeletion,
    WholeTreeLinksValid,
    MassDeletionPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitSyncPauseReason {
    HeadMoved,
    OperationInProgress,
    StagedChanges,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitSyncPause {
    pub reason: GitSyncPauseReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_head: Option<GitOid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_head: Option<GitOid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_head_ref: Option<GitRefName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_head_ref: Option<GitRefName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitSyncReport {
    pub dry_run: bool,
    pub outcome: GitSyncOutcome,
    pub installation: GitInstallation,
    pub repository: GitRepository,
    pub remote: GitRemote,
    pub refs: GitSyncRefs,
    pub safety: GitSafetyState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_before: Option<GitOid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_ref_before: Option<GitRefName>,
    pub remote_before: Option<GitOid>,
    pub local_before: Option<GitOid>,
    pub local_snapshot: Option<GitOid>,
    pub accepted: Option<GitOid>,
    pub actions: Vec<GitSyncAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automatic_resolutions: Vec<GitAutomaticResolution>,
    pub retries: usize,
    pub conflict: Option<GitSyncConflict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pause: Option<GitSyncPause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application: Option<GitTreeApplyPlan>,
}

impl GitSyncReport {
    fn initial(
        options: &GitSyncOptions,
        installation: GitInstallation,
        repository: GitRepository,
        refs: GitSyncRefs,
        safety: GitSafetyState,
        head_before: (Option<GitOid>, Option<GitRefName>),
        observed: (Option<GitOid>, Option<GitOid>),
    ) -> Self {
        Self {
            dry_run: options.dry_run,
            outcome: GitSyncOutcome::Planned,
            installation,
            repository,
            remote: options.remote.clone(),
            refs,
            safety,
            head_before: head_before.0,
            head_ref_before: head_before.1,
            remote_before: observed.0,
            local_before: observed.1,
            local_snapshot: None,
            accepted: None,
            actions: Vec::new(),
            automatic_resolutions: Vec::new(),
            retries: 0,
            conflict: None,
            pause: None,
            application: None,
        }
    }
}

#[derive(Debug)]
pub enum GitSyncError {
    Git(GitEngineError),
    Locked,
    Cancelled,
    Observer(GitSyncObserverError),
    RetryLimit { attempts: usize },
    Io(std::io::Error),
}

impl Display for GitSyncError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Git(error) => Display::fmt(error, formatter),
            Self::Locked => formatter.write_str(
                "another Vulcan synchronization cycle already holds the repository lock",
            ),
            Self::Cancelled => formatter.write_str(
                "synchronization was cancelled; captured refs and recovery state remain preserved",
            ),
            Self::Observer(error) => write!(formatter, "sync progress observer failed: {error}"),
            Self::RetryLimit { attempts } => write!(
                formatter,
                "synchronization did not converge after {attempts} attempts; local snapshots remain preserved"
            ),
            Self::Io(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for GitSyncError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Git(error) => Some(error),
            Self::Observer(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Locked | Self::Cancelled | Self::RetryLimit { .. } => None,
        }
    }
}

impl From<GitEngineError> for GitSyncError {
    fn from(error: GitEngineError) -> Self {
        Self::Git(error)
    }
}

impl From<std::io::Error> for GitSyncError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<GitSyncObserverError> for GitSyncError {
    fn from(error: GitSyncObserverError) -> Self {
        Self::Observer(error)
    }
}

pub struct GitSyncBackend<'a> {
    engine: &'a dyn GitEngine,
    options: GitSyncOptions,
}

impl<'a> GitSyncBackend<'a> {
    #[must_use]
    pub fn new(engine: &'a dyn GitEngine, options: GitSyncOptions) -> Self {
        Self { engine, options }
    }
}

impl SyncBackend for GitSyncBackend<'_> {
    fn name(&self) -> &'static str {
        "git"
    }

    fn capabilities(&self) -> SyncCapabilities {
        SyncCapabilities {
            operation_modes: vec![SyncOperationMode::Finite],
            features: vec![
                SyncCapability::Fetch,
                SyncCapability::Push,
                SyncCapability::SafePause,
                SyncCapability::SafeCancel,
                SyncCapability::Progress,
                SyncCapability::RemoteRevision,
                SyncCapability::OfflineRecovery,
                SyncCapability::ConflictPreservation,
                SyncCapability::DetachedGitDirectory,
            ],
        }
    }

    fn plan(&self, context: &SyncContext<'_>) -> Result<SyncPlan, SyncError> {
        Ok(SyncPlan {
            version: SYNC_CONTRACT_VERSION,
            backend: self.name().to_string(),
            vault: context.vault_path.to_path_buf(),
            dry_run: context.dry_run,
            capabilities: self.capabilities(),
            operations: vec![
                SyncOperation::Capture,
                SyncOperation::Fetch,
                SyncOperation::Merge,
                SyncOperation::Push,
                SyncOperation::Apply,
                SyncOperation::Verify,
            ],
        })
    }

    fn sync_once(
        &self,
        context: &SyncContext<'_>,
        cancellation: &SyncCancellationToken,
    ) -> Result<SyncReport, SyncError> {
        let mut options = self.options.clone();
        options.dry_run = context.dry_run;
        let mut observer = BackendObserver {
            vault: context.vault_path,
            observer: context.observer,
        };
        sync_git_once_with_control(
            self.engine,
            context.vault_path,
            &options,
            cancellation,
            &mut observer,
        )
        .map(git_report_to_backend_report)
        .map_err(|error| sync_error_from_git(&error))
    }
}

struct BackendObserver<'a> {
    vault: &'a Path,
    observer: &'a dyn crate::SyncObserver,
}

impl GitSyncObserver for BackendObserver<'_> {
    fn progress(&mut self, progress: &GitSyncProgress) -> Result<(), GitSyncObserverError> {
        self.observer
            .progress(&SyncProgress {
                backend: "git".to_string(),
                vault: self.vault.to_path_buf(),
                state: sync_state_from_phase(progress.phase),
                attempt: progress.attempt,
                local_revision: progress.local_snapshot.as_ref().map(ToString::to_string),
                accepted_revision: progress.accepted.as_ref().map(ToString::to_string),
            })
            .map_err(|error| GitSyncObserverError::new(error.to_string()))
    }
}

fn sync_state_from_phase(phase: GitSyncPhase) -> SyncState {
    match phase {
        GitSyncPhase::Preparing => SyncState::CapturePending,
        GitSyncPhase::Capturing => SyncState::Capturing,
        GitSyncPhase::Captured => SyncState::CapturedUnpushed,
        GitSyncPhase::Fetching => SyncState::Fetching,
        GitSyncPhase::Fetched => SyncState::Fetched,
        GitSyncPhase::Merging => SyncState::Merging,
        GitSyncPhase::Pushing => SyncState::Pushing,
        GitSyncPhase::Applying | GitSyncPhase::Verifying => SyncState::Applying,
        GitSyncPhase::Paused => SyncState::Paused,
        GitSyncPhase::Conflicted => SyncState::Conflicted,
        GitSyncPhase::Completed => SyncState::Clean,
    }
}

fn git_report_to_backend_report(report: GitSyncReport) -> SyncReport {
    let conflict = report.conflict.as_ref().map(|conflict| SyncConflict {
        id: conflict.id.clone(),
        paths: conflict
            .paths
            .iter()
            .map(std::path::PathBuf::from)
            .collect(),
        base_revision: conflict.base.as_ref().map(ToString::to_string),
        local_revision: conflict.local.to_string(),
        remote_revision: conflict.remote.to_string(),
        policy_version: conflict.policy_version,
        resolution: SyncResolutionState::Unresolved,
        preserved: true,
        detail: Some(conflict.diagnostics.clone()),
    });
    let state = match report.outcome {
        GitSyncOutcome::Paused => SyncState::Paused,
        GitSyncOutcome::Conflicted => SyncState::Conflicted,
        GitSyncOutcome::Planned
            if report.safety.staged_changes || report.safety.operation.is_some() =>
        {
            SyncState::Dirty
        }
        _ => SyncState::Clean,
    };
    let status = SyncStatus {
        state,
        backend: "git".to_string(),
        vault: report
            .repository
            .work_tree
            .clone()
            .unwrap_or_else(|| report.repository.git_dir.clone()),
        local_revision: report.local_snapshot.as_ref().map(ToString::to_string),
        remote_revision: report.remote_before.as_ref().map(ToString::to_string),
        accepted_revision: report.accepted.as_ref().map(ToString::to_string),
        unresolved_conflicts: usize::from(conflict.is_some()),
        detail: report.pause.as_ref().map(|pause| match pause.reason {
            GitSyncPauseReason::HeadMoved => format!(
                "HEAD moved from {} to {} during synchronization",
                pause
                    .expected_head
                    .as_ref()
                    .map_or("unborn", GitOid::as_str),
                pause.actual_head.as_ref().map_or("unborn", GitOid::as_str)
            ),
            GitSyncPauseReason::OperationInProgress => format!(
                "Git {} operation is in progress",
                pause.operation.as_deref().unwrap_or("unknown")
            ),
            GitSyncPauseReason::StagedChanges => {
                "the normal Git index contains staged changes".to_string()
            }
        }),
    };
    SyncReport {
        version: SYNC_CONTRACT_VERSION,
        backend: "git".to_string(),
        dry_run: report.dry_run,
        outcome: match report.outcome {
            GitSyncOutcome::Planned => SyncOutcome::Planned,
            GitSyncOutcome::Paused => SyncOutcome::Paused,
            GitSyncOutcome::UpToDate => SyncOutcome::UpToDate,
            GitSyncOutcome::Bootstrapped => SyncOutcome::Bootstrapped,
            GitSyncOutcome::Pushed => SyncOutcome::Pushed,
            GitSyncOutcome::Pulled => SyncOutcome::Pulled,
            GitSyncOutcome::Merged => SyncOutcome::Merged,
            GitSyncOutcome::Conflicted => SyncOutcome::Conflicted,
        },
        status,
        actions: report
            .actions
            .into_iter()
            .map(|action| match action {
                GitSyncAction::SnapshotCreated => SyncAction::SnapshotCreated,
                GitSyncAction::Pushed => SyncAction::Pushed,
                GitSyncAction::WorktreeApplied => SyncAction::WorktreeApplied,
            })
            .collect(),
        attempts: if report.dry_run {
            0
        } else {
            report.retries + 1
        },
        conflicts: conflict.into_iter().collect(),
    }
}

fn sync_error_from_git(error: &GitSyncError) -> SyncError {
    let (category, retryable) = match error {
        GitSyncError::Locked | GitSyncError::Git(GitEngineError::WorktreeChanged) => {
            (SyncErrorCategory::Busy, true)
        }
        GitSyncError::Cancelled => (SyncErrorCategory::Cancelled, false),
        GitSyncError::Observer(_) => (SyncErrorCategory::Observer, false),
        GitSyncError::RetryLimit { .. } => (SyncErrorCategory::Network, true),
        GitSyncError::Io(_) | GitSyncError::Git(GitEngineError::Io(_)) => {
            (SyncErrorCategory::Io, true)
        }
        GitSyncError::Git(
            GitEngineError::ExecutableUnavailable { .. } | GitEngineError::InvalidRemote(_),
        ) => (SyncErrorCategory::Configuration, false),
        GitSyncError::Git(GitEngineError::CommandFailed { operation, .. })
            if operation.contains("remote")
                || operation.contains("fetch")
                || operation.contains("push")
                || operation.contains("clone") =>
        {
            (SyncErrorCategory::Network, true)
        }
        GitSyncError::Git(GitEngineError::UnsupportedRepository { .. }) => {
            (SyncErrorCategory::Unsupported, false)
        }
        GitSyncError::Git(
            GitEngineError::InvalidOutput { .. }
            | GitEngineError::InvalidObjectId(_)
            | GitEngineError::InvalidRefName(_),
        ) => (SyncErrorCategory::Invariant, false),
        GitSyncError::Git(GitEngineError::CommandFailed { .. }) => {
            (SyncErrorCategory::Repository, false)
        }
    };
    SyncError::new(category, error.to_string(), retryable)
}

pub fn sync_git_once(
    engine: &dyn GitEngine,
    vault_path: &Path,
    options: &GitSyncOptions,
) -> Result<GitSyncReport, GitSyncError> {
    sync_git_once_with_control(
        engine,
        vault_path,
        options,
        &SyncCancellationToken::default(),
        &mut IgnoreGitSyncProgress,
    )
}

pub fn sync_git_once_with_control(
    engine: &dyn GitEngine,
    vault_path: &Path,
    options: &GitSyncOptions,
    cancellation: &SyncCancellationToken,
    observer: &mut dyn GitSyncObserver,
) -> Result<GitSyncReport, GitSyncError> {
    cancellation.check()?;
    options.merge_policy.validate().map_err(|error| {
        GitSyncError::Git(GitEngineError::UnsupportedRepository {
            detail: error.to_string(),
        })
    })?;
    let installation = engine.installation()?;
    let repository = engine.discover_repository(vault_path)?;
    let refs = GitSyncRefs::for_options(options)?;
    let safety = engine.safety_state(&repository)?;
    let head_before = engine.head_commit(&repository)?;
    let head_ref_before = engine.head_reference(&repository)?;
    let local_before = engine.read_ref(&repository, &refs.local)?;
    let remote_before = if options.dry_run {
        engine.remote_ref(&repository, &options.remote, &refs.live)?
    } else {
        None
    };
    let mut report = GitSyncReport::initial(
        options,
        installation,
        repository,
        refs,
        safety,
        (head_before, head_ref_before),
        (remote_before, local_before),
    );
    emit_progress(observer, GitSyncPhase::Preparing, 0, &report, None)?;
    if options.dry_run {
        emit_progress(observer, GitSyncPhase::Completed, 0, &report, None)?;
        return Ok(report);
    }
    let _lock = RepositoryLock::acquire(&report.repository)?;
    let attempts = options.max_retries.max(1);
    for attempt in 0..attempts {
        cancellation.check()?;
        report.retries = attempt;
        let mut control = AttemptControl {
            attempt,
            cancellation,
            observer,
        };
        if run_attempt(engine, options, &mut report, &mut control)? == AttemptResult::Finished {
            return Ok(report);
        }
        if attempt + 1 < attempts {
            control.check()?;
            std::thread::sleep(retry_backoff(attempt));
            control.check()?;
        }
    }

    Err(GitSyncError::RetryLimit { attempts })
}

fn retry_backoff(attempt: usize) -> Duration {
    const BASE_MILLIS: u64 = 25;
    const MAX_MILLIS: u64 = 400;
    let shift = u32::try_from(attempt.min(16)).expect("bounded retry shift fits u32");
    let multiplier = 1_u64.checked_shl(shift).unwrap_or(u64::MAX);
    Duration::from_millis(BASE_MILLIS.saturating_mul(multiplier).min(MAX_MILLIS))
}

fn emit_progress(
    observer: &mut dyn GitSyncObserver,
    phase: GitSyncPhase,
    attempt: usize,
    report: &GitSyncReport,
    local_tree: Option<GitOid>,
) -> Result<(), GitSyncError> {
    observer
        .progress(&GitSyncProgress {
            phase,
            attempt,
            repository: report.repository.clone(),
            local_snapshot: report.local_snapshot.clone(),
            local_tree,
            accepted: report.accepted.clone(),
        })
        .map_err(GitSyncError::from)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptResult {
    Retry,
    Finished,
}

struct AttemptControl<'a> {
    attempt: usize,
    cancellation: &'a SyncCancellationToken,
    observer: &'a mut dyn GitSyncObserver,
}

impl AttemptControl<'_> {
    fn check(&self) -> Result<(), GitSyncError> {
        self.cancellation.check()
    }

    fn emit(
        &mut self,
        phase: GitSyncPhase,
        report: &GitSyncReport,
        local_tree: Option<GitOid>,
    ) -> Result<(), GitSyncError> {
        emit_progress(self.observer, phase, self.attempt, report, local_tree)
    }

    fn validate_automatic_merge(
        &mut self,
        engine: &dyn GitEngine,
        request: &GitAutomaticMergeValidation<'_>,
    ) -> Result<Vec<GitAutomaticValidationCheck>, String> {
        self.check().map_err(|error| error.to_string())?;
        let checks = self
            .observer
            .validate_automatic_merge(engine, request)
            .map_err(|error| error.to_string())?;
        self.check().map_err(|error| error.to_string())?;
        Ok(checks)
    }
}

fn run_attempt(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    control: &mut AttemptControl<'_>,
) -> Result<AttemptResult, GitSyncError> {
    control.check()?;
    control.emit(GitSyncPhase::Capturing, report, None)?;
    let base = engine
        .read_ref(&report.repository, &report.refs.local)?
        .or(engine.head_commit(&report.repository)?);
    let capture = engine.capture_worktree(
        &report.repository,
        &GitCaptureRequest {
            base: base.clone(),
            target_ref: report.refs.local.clone(),
            message: snapshot_message(&report.refs, options, base.as_ref()),
        },
    )?;
    report.local_snapshot = Some(capture.commit.clone());
    if capture.created {
        report.actions.push(GitSyncAction::SnapshotCreated);
    }
    control.emit(GitSyncPhase::Captured, report, Some(capture.tree.clone()))?;

    control.check()?;
    control.emit(GitSyncPhase::Fetching, report, None)?;
    let remote_tip = engine.remote_ref(&report.repository, &options.remote, &report.refs.live)?;
    if control.attempt == 0 {
        report.remote_before.clone_from(&remote_tip);
    }
    let has_remote = remote_tip.is_some();
    if let Some(pause) = sync_pause(engine, report)? {
        if has_remote {
            engine.fetch_ref(
                &report.repository,
                &options.remote,
                &report.refs.live,
                &report.refs.fetched,
            )?;
            control.emit(GitSyncPhase::Fetched, report, None)?;
        }
        report.pause = Some(pause);
        report.outcome = GitSyncOutcome::Paused;
        control.emit(GitSyncPhase::Paused, report, None)?;
        return Ok(AttemptResult::Finished);
    }
    let Some((accepted, outcome, pushed)) =
        reconcile(engine, options, report, &capture, has_remote, control)?
    else {
        return Ok(if report.outcome == GitSyncOutcome::Conflicted {
            AttemptResult::Finished
        } else {
            AttemptResult::Retry
        });
    };

    control.check()?;
    control.emit(GitSyncPhase::Verifying, report, None)?;
    let verification = engine.capture_worktree(
        &report.repository,
        &GitCaptureRequest {
            base: Some(capture.commit.clone()),
            target_ref: report.refs.local.clone(),
            message: snapshot_message(&report.refs, options, Some(&capture.commit)),
        },
    )?;
    if verification.commit != capture.commit {
        return Ok(AttemptResult::Retry);
    }
    if pushed {
        report.actions.push(GitSyncAction::Pushed);
    }
    report.accepted = Some(accepted.clone());
    if verification.tree != engine.tree_oid(&report.repository, &accepted)? {
        if let Some(pause) = sync_pause(engine, report)? {
            engine.update_ref(&report.repository, &report.refs.pending, &accepted)?;
            report.pause = Some(pause);
            report.outcome = GitSyncOutcome::Paused;
            control.emit(GitSyncPhase::Paused, report, None)?;
            return Ok(AttemptResult::Finished);
        }
        control.check()?;
        control.emit(GitSyncPhase::Applying, report, None)?;
        report.application =
            Some(engine.apply_tree(&report.repository, &verification.commit, &accepted)?);
        report.actions.push(GitSyncAction::WorktreeApplied);
    }
    engine.update_ref(&report.repository, &report.refs.local, &accepted)?;
    engine.update_ref(&report.repository, &report.refs.fetched, &accepted)?;
    engine.update_ref(&report.repository, &report.refs.pending, &accepted)?;
    report.outcome = outcome;
    report.accepted = Some(accepted);
    control.emit(GitSyncPhase::Completed, report, None)?;
    Ok(AttemptResult::Finished)
}

fn sync_pause(
    engine: &dyn GitEngine,
    report: &GitSyncReport,
) -> Result<Option<GitSyncPause>, GitSyncError> {
    let actual_head = engine.head_commit(&report.repository)?;
    let actual_head_ref = engine.head_reference(&report.repository)?;
    if actual_head != report.head_before || actual_head_ref != report.head_ref_before {
        return Ok(Some(GitSyncPause {
            reason: GitSyncPauseReason::HeadMoved,
            expected_head: report.head_before.clone(),
            actual_head,
            expected_head_ref: report.head_ref_before.clone(),
            actual_head_ref,
            operation: None,
        }));
    }
    let safety = engine.safety_state(&report.repository)?;
    if let Some(operation) = safety.operation {
        return Ok(Some(GitSyncPause {
            reason: GitSyncPauseReason::OperationInProgress,
            expected_head: report.head_before.clone(),
            actual_head,
            expected_head_ref: report.head_ref_before.clone(),
            actual_head_ref,
            operation: Some(operation),
        }));
    }
    if safety.staged_changes {
        return Ok(Some(GitSyncPause {
            reason: GitSyncPauseReason::StagedChanges,
            expected_head: report.head_before.clone(),
            actual_head,
            expected_head_ref: report.head_ref_before.clone(),
            actual_head_ref,
            operation: None,
        }));
    }
    Ok(None)
}

fn reconcile(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    capture: &crate::GitCapture,
    has_remote: bool,
    control: &mut AttemptControl<'_>,
) -> Result<Option<(GitOid, GitSyncOutcome, bool)>, GitSyncError> {
    if !has_remote {
        control.check()?;
        control.emit(GitSyncPhase::Pushing, report, None)?;
        if !captured_worktree_is_current(engine, &report.repository, capture)? {
            return Ok(None);
        }
        return Ok(
            match engine.push_ref(
                &report.repository,
                &options.remote,
                &capture.commit,
                &report.refs.live,
                None,
            )? {
                GitPushResult::Updated => {
                    Some((capture.commit.clone(), GitSyncOutcome::Bootstrapped, true))
                }
                GitPushResult::Rejected => None,
            },
        );
    }
    let remote = engine.fetch_ref(
        &report.repository,
        &options.remote,
        &report.refs.live,
        &report.refs.fetched,
    )?;
    control.emit(GitSyncPhase::Fetched, report, None)?;
    if capture.commit == remote {
        return Ok(Some((remote, GitSyncOutcome::UpToDate, false)));
    }
    if engine.is_ancestor(&report.repository, &remote, &capture.commit)? {
        control.check()?;
        control.emit(GitSyncPhase::Pushing, report, None)?;
        if !captured_worktree_is_current(engine, &report.repository, capture)? {
            return Ok(None);
        }
        return Ok(
            match engine.push_ref(
                &report.repository,
                &options.remote,
                &capture.commit,
                &report.refs.live,
                Some(&remote),
            )? {
                GitPushResult::Updated => {
                    Some((capture.commit.clone(), GitSyncOutcome::Pushed, true))
                }
                GitPushResult::Rejected => None,
            },
        );
    }
    if engine.is_ancestor(&report.repository, &capture.commit, &remote)? {
        return Ok(Some((remote, GitSyncOutcome::Pulled, false)));
    }
    if let Some(epoch) = find_git_live_epoch(engine, &report.repository, &report.refs, &remote)? {
        if engine.is_ancestor(&report.repository, &epoch.root, &capture.commit)? {
            return merge_divergence(engine, options, report, capture, remote, control);
        }
        return reconcile_epoch_root(engine, options, report, capture, remote, &epoch, control);
    }
    merge_divergence(engine, options, report, capture, remote, control)
}

const MAX_EPOCH_DISCOVERY_COMMITS: usize = 100_001;
const MAX_MISSED_EPOCHS: usize = 1_024;

#[derive(Debug, Clone)]
pub struct GitLiveEpoch {
    pub root: GitOid,
    pub id: String,
    pub previous: GitOid,
    pub remote_archive: GitRefName,
    pub local_archive: GitRefName,
}

pub fn find_git_live_epoch(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    refs: &GitSyncRefs,
    live: &GitOid,
) -> Result<Option<GitLiveEpoch>, GitSyncError> {
    for commit in engine.first_parent_history(repository, live, MAX_EPOCH_DISCOVERY_COMMITS)? {
        let metadata = engine.commit_metadata(repository, &commit)?;
        let Some(id) = trailer(&metadata.message, "Vulcan-Sync-Epoch") else {
            continue;
        };
        if metadata.parents.is_empty() {
            let previous = trailer(&metadata.message, "Vulcan-Sync-Previous-Epoch")
                .ok_or_else(|| invalid_epoch("epoch root has no previous-epoch trailer"))
                .and_then(|value| GitOid::parse(value).map_err(GitSyncError::from))?;
            let remote_archive = trailer(&metadata.message, "Vulcan-Sync-Epoch-Archive")
                .ok_or_else(|| invalid_epoch("epoch root has no archive-ref trailer"))
                .and_then(|value| GitRefName::parse(value).map_err(GitSyncError::from))?;
            let profile = refs
                .local
                .as_str()
                .split('/')
                .nth(3)
                .ok_or_else(|| invalid_epoch("local sync ref has no profile component"))?;
            if trailer(&metadata.message, "Vulcan-Sync-Profile") != Some(profile) {
                return Err(invalid_epoch(
                    "epoch root profile does not match this sync target",
                ));
            }
            let expected_archive_prefix = format!("refs/heads/__vulcan-sync/epochs/{profile}/");
            if !remote_archive
                .as_str()
                .starts_with(&expected_archive_prefix)
            {
                return Err(invalid_epoch(
                    "epoch archive ref is outside the target profile",
                ));
            }
            if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(invalid_epoch(
                    "epoch ID is not a 32-character hexadecimal value",
                ));
            }
            if id != git_live_epoch_id(profile, &previous) {
                return Err(invalid_epoch(
                    "epoch ID does not match its profile and previous tip",
                ));
            }
            if remote_archive.as_str() != format!("refs/heads/__vulcan-sync/epochs/{profile}/{id}")
            {
                return Err(invalid_epoch(
                    "epoch archive ref does not match the epoch ID",
                ));
            }
            let local_archive =
                GitRefName::parse(format!("refs/vulcan/epochs/live/{profile}/{id}"))?;
            return Ok(Some(GitLiveEpoch {
                root: commit,
                id: id.to_string(),
                previous,
                remote_archive,
                local_archive,
            }));
        }
        return Err(invalid_epoch("epoch trailer appears on a non-root commit"));
    }
    Ok(None)
}

fn trailer<'a>(message: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}: ");
    let mut values = message
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix));
    let value = values.next()?;
    if value.is_empty() || values.next().is_some() {
        None
    } else {
        Some(value)
    }
}

fn invalid_epoch(detail: &str) -> GitSyncError {
    GitSyncError::Git(GitEngineError::UnsupportedRepository {
        detail: format!("invalid Vulcan live epoch root: {detail}"),
    })
}

fn reconcile_epoch_root(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    capture: &crate::GitCapture,
    remote_live: GitOid,
    epoch: &GitLiveEpoch,
    control: &mut AttemptControl<'_>,
) -> Result<Option<(GitOid, GitSyncOutcome, bool)>, GitSyncError> {
    let bridge_parent = epoch_bridge_parent(
        engine,
        options,
        &report.repository,
        &report.refs,
        &capture.commit,
        epoch.clone(),
    )?;
    let remote_tree = engine.tree_oid(&report.repository, &remote_live)?;
    if capture.tree == remote_tree {
        return Ok(Some((remote_live, GitSyncOutcome::Pulled, false)));
    }

    control.check()?;
    control.emit(GitSyncPhase::Merging, report, None)?;
    report.automatic_resolutions.clear();
    let bridge = engine.create_reproducible_commit(
        &report.repository,
        &remote_tree,
        std::slice::from_ref(&bridge_parent),
        &format!(
            "vulcan epoch reconciliation bridge\n\nVulcan-Sync-Version: 1\nVulcan-Sync-Epoch: {}\nVulcan-Sync-Previous-Epoch: {}\nVulcan-Sync-Semantic: false\n",
            epoch.id, bridge_parent
        ),
    )?;
    let mut merge = engine.merge_commits(&report.repository, &bridge, &capture.commit)?;
    let tree = resolve_merge_candidate_tree(
        engine,
        options,
        report,
        control,
        &capture.commit,
        &bridge,
        &mut merge,
    );
    let Some(tree) = tree else {
        report.outcome = GitSyncOutcome::Conflicted;
        report.conflict = Some(build_sync_conflict(
            engine,
            options,
            &report.repository,
            capture,
            remote_live,
            merge,
        )?);
        control.emit(GitSyncPhase::Conflicted, report, None)?;
        return Ok(None);
    };
    if tree == remote_tree {
        return Ok(Some((remote_live, GitSyncOutcome::Pulled, false)));
    }
    let rebased = engine.create_commit(
        &report.repository,
        &tree,
        std::slice::from_ref(&remote_live),
        &format!(
            "vulcan live epoch reconciliation\n\n{}Vulcan-Sync-Rebased-Epoch: {}\nVulcan-Sync-Rebased-From: {}\n",
            sync_trailers(&report.refs, options, capture.commit.as_str()),
            epoch.id,
            bridge_parent
        ),
    )?;
    engine.update_ref(&report.repository, &report.refs.pending, &rebased)?;
    control.check()?;
    control.emit(GitSyncPhase::Pushing, report, None)?;
    if !captured_worktree_is_current(engine, &report.repository, capture)? {
        return Ok(None);
    }
    Ok(
        match engine.push_ref(
            &report.repository,
            &options.remote,
            &rebased,
            &report.refs.live,
            Some(&remote_live),
        )? {
            GitPushResult::Updated => Some((rebased, GitSyncOutcome::Merged, true)),
            GitPushResult::Rejected => None,
        },
    )
}

fn epoch_bridge_parent(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    repository: &GitRepository,
    refs: &GitSyncRefs,
    capture: &GitOid,
    mut epoch: GitLiveEpoch,
) -> Result<GitOid, GitSyncError> {
    let mut visited = HashSet::new();
    for _ in 0..MAX_MISSED_EPOCHS {
        if !visited.insert(epoch.root.to_string()) {
            return Err(invalid_epoch("epoch archive chain contains a cycle"));
        }
        let archived = fetch_epoch_archive(engine, options, repository, &epoch)?;
        if engine.merge_base(repository, &archived, capture)?.is_some() {
            return Ok(archived);
        }
        epoch = find_git_live_epoch(engine, repository, refs, &archived)?.ok_or_else(|| {
            invalid_epoch("offline candidate has no common ancestry with the epoch archive chain")
        })?;
    }
    Err(invalid_epoch(
        "offline candidate exceeds the supported missed-epoch bound",
    ))
}

fn fetch_epoch_archive(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    repository: &GitRepository,
    epoch: &GitLiveEpoch,
) -> Result<GitOid, GitSyncError> {
    match engine.remote_ref(repository, &options.remote, &epoch.remote_archive)? {
        Some(remote) if remote != epoch.previous => {
            return Err(invalid_epoch(
                "remote archive does not match previous-epoch trailer",
            ));
        }
        Some(_) => {}
        None => {
            return Err(invalid_epoch(
                "required archive has expired; this offline candidate predates the retained epoch horizon",
            ));
        }
    }
    if let Some(local_archive) = engine.read_ref(repository, &epoch.local_archive)? {
        if local_archive != epoch.previous {
            return Err(invalid_epoch(
                "existing local archive does not match previous-epoch trailer",
            ));
        }
    }
    let archived = engine.fetch_ref(
        repository,
        &options.remote,
        &epoch.remote_archive,
        &epoch.local_archive,
    )?;
    if archived != epoch.previous {
        return Err(invalid_epoch(
            "fetched archive does not match previous-epoch trailer",
        ));
    }
    let root_tree = engine.tree_oid(repository, &epoch.root)?;
    if engine.tree_oid(repository, &archived)? != root_tree {
        return Err(invalid_epoch(
            "epoch root tree differs from its archived predecessor",
        ));
    }
    Ok(archived)
}

fn merge_divergence(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    capture: &crate::GitCapture,
    remote: GitOid,
    control: &mut AttemptControl<'_>,
) -> Result<Option<(GitOid, GitSyncOutcome, bool)>, GitSyncError> {
    control.check()?;
    control.emit(GitSyncPhase::Merging, report, None)?;
    report.automatic_resolutions.clear();
    let mut merge = engine.merge_commits(&report.repository, &remote, &capture.commit)?;
    let tree = resolve_merge_candidate_tree(
        engine,
        options,
        report,
        control,
        &capture.commit,
        &remote,
        &mut merge,
    );
    if tree.is_none() {
        report.outcome = GitSyncOutcome::Conflicted;
        report.conflict = Some(build_sync_conflict(
            engine,
            options,
            &report.repository,
            capture,
            remote,
            merge,
        )?);
        control.emit(GitSyncPhase::Conflicted, report, None)?;
        return Ok(None);
    }
    let tree = tree.ok_or_else(|| {
        GitSyncError::Git(GitEngineError::InvalidOutput {
            operation: "merge live sync commits",
            detail: "the clean merge report omitted its tree".to_string(),
        })
    })?;
    let merged = engine.create_commit(
        &report.repository,
        &tree,
        &[remote.clone(), capture.commit.clone()],
        &merge_message(&report.refs, options, &remote, &capture.commit),
    )?;
    engine.update_ref(&report.repository, &report.refs.pending, &merged)?;
    control.check()?;
    control.emit(GitSyncPhase::Pushing, report, None)?;
    if !captured_worktree_is_current(engine, &report.repository, capture)? {
        return Ok(None);
    }
    Ok(
        match engine.push_ref(
            &report.repository,
            &options.remote,
            &merged,
            &report.refs.live,
            Some(&remote),
        )? {
            GitPushResult::Updated => Some((merged, GitSyncOutcome::Merged, true)),
            GitPushResult::Rejected => None,
        },
    )
}

fn resolve_merge_candidate_tree(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    control: &mut AttemptControl<'_>,
    local: &GitOid,
    remote: &GitOid,
    merge: &mut crate::GitMerge,
) -> Option<GitOid> {
    if merge.clean {
        return merge.tree.clone();
    }
    match try_structured_merge(
        engine,
        options,
        &report.repository,
        merge.base.as_ref(),
        local,
        remote,
        &merge.conflict_paths,
    ) {
        Ok(Some((tree, mut resolutions))) => {
            let validation = merge
                .base
                .as_ref()
                .ok_or_else(|| "structured merge validation requires a merge base".to_string())
                .and_then(|base| {
                    validate_automatic_tree(
                        control,
                        engine,
                        &AutomaticMergeCandidate {
                            repository: &report.repository,
                            base,
                            local,
                            remote,
                            tree: &tree,
                        },
                        &mut resolutions,
                    )
                });
            match validation {
                Ok(()) => {
                    report.automatic_resolutions = resolutions;
                    Some(tree)
                }
                Err(detail) => {
                    append_structured_merge_failure(merge, &detail);
                    None
                }
            }
        }
        Ok(None) => None,
        Err(detail) => {
            append_structured_merge_failure(merge, &detail);
            None
        }
    }
}

struct AutomaticMergeCandidate<'a> {
    repository: &'a GitRepository,
    base: &'a GitOid,
    local: &'a GitOid,
    remote: &'a GitOid,
    tree: &'a GitOid,
}

fn validate_automatic_tree(
    control: &mut AttemptControl<'_>,
    engine: &dyn GitEngine,
    candidate: &AutomaticMergeCandidate<'_>,
    resolutions: &mut [GitAutomaticResolution],
) -> Result<(), String> {
    let resolved_paths = resolutions
        .iter()
        .map(|resolution| resolution.path.clone())
        .collect::<Vec<_>>();
    let checks = control.validate_automatic_merge(
        engine,
        &GitAutomaticMergeValidation {
            repository: candidate.repository,
            base: candidate.base,
            local_candidate: candidate.local,
            accepted_remote: candidate.remote,
            merged_tree: candidate.tree,
            resolved_paths: &resolved_paths,
        },
    )?;
    for resolution in resolutions {
        resolution.validation.checks.extend(checks.iter().copied());
    }
    Ok(())
}

fn append_structured_merge_failure(merge: &mut crate::GitMerge, detail: &str) {
    if !merge.diagnostics.is_empty() {
        merge.diagnostics.push('\n');
    }
    write!(merge.diagnostics, "Vulcan structured merge: {detail}")
        .expect("writing to a String cannot fail");
}

fn captured_worktree_is_current(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    capture: &crate::GitCapture,
) -> Result<bool, GitSyncError> {
    Ok(engine.snapshot_worktree_tree(repository, Some(&capture.commit))? == capture.tree)
}

fn build_sync_conflict(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    repository: &GitRepository,
    capture: &crate::GitCapture,
    remote: GitOid,
    merge: crate::GitMerge,
) -> Result<GitSyncConflict, GitSyncError> {
    let classifications = classify_conflicts(
        engine,
        options,
        repository,
        merge.base.as_ref(),
        &capture.commit,
        &remote,
        &merge.conflict_paths,
        &merge.diagnostics,
    )?;
    let (id, policy_hash) = conflict_identity(
        &options.merge_policy,
        merge.base.as_ref(),
        &capture.commit,
        &remote,
        &merge.conflict_paths,
    )?;
    let (preserved_refs, provenance_revision) = preserve_conflict_refs(
        engine,
        repository,
        &ConflictPreservationRequest {
            options,
            id: &id,
            policy_hash: &policy_hash,
            base: merge.base.as_ref(),
            local: &capture.commit,
            remote: &remote,
            merge_tree: merge.tree.as_ref(),
        },
    )?;
    Ok(GitSyncConflict {
        id,
        base: merge.base,
        remote,
        local: capture.commit.clone(),
        paths: merge.conflict_paths,
        classifications,
        policy_version: options.merge_policy.version,
        policy_hash,
        preserved_refs,
        provenance_revision,
        merge_tree: merge.tree,
        diagnostics: merge.diagnostics,
    })
}

#[allow(clippy::too_many_arguments)]
fn classify_conflicts(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    repository: &GitRepository,
    base: Option<&GitOid>,
    local: &GitOid,
    remote: &GitOid,
    paths: &[String],
    diagnostics: &str,
) -> Result<Vec<GitConflictClassification>, GitSyncError> {
    paths
        .iter()
        .map(|path| {
            let base_object = base
                .map(|base| engine.path_object(repository, base, path))
                .transpose()?
                .flatten();
            let local_object = engine.path_object(repository, local, path)?;
            let remote_object = engine.path_object(repository, remote, path)?;
            let file_kind = MergeFileKind::classify(
                path,
                &[
                    object_data(base_object.as_ref()),
                    object_data(local_object.as_ref()),
                    object_data(remote_object.as_ref()),
                ],
            );
            let decision = options
                .merge_policy
                .decision_for(path, file_kind, MergeAutomation::AllowPolicy)
                .map_err(|error| {
                    GitSyncError::Git(GitEngineError::InvalidOutput {
                        operation: "classify a sync conflict",
                        detail: error.to_string(),
                    })
                })?;
            let class = conflict_class(
                path,
                paths,
                diagnostics,
                base_object.as_ref(),
                local_object.as_ref(),
                remote_object.as_ref(),
                file_kind,
            );
            Ok(GitConflictClassification {
                path: path.clone(),
                class,
                file_kind,
                rule_id: decision.rule_id,
                configured_resolution: decision.resolution,
                effective_resolution: MergeResolution::RequireReview,
                diagnostic_code: conflict_diagnostic_code(class).to_string(),
            })
        })
        .collect()
}

fn conflict_class(
    path: &str,
    paths: &[String],
    diagnostics: &str,
    base: Option<&crate::GitPathObject>,
    local: Option<&crate::GitPathObject>,
    remote: Option<&crate::GitPathObject>,
    file_kind: MergeFileKind,
) -> GitConflictClass {
    let path_key = path.to_lowercase();
    if paths
        .iter()
        .any(|candidate| candidate != path && candidate.to_lowercase() == path_key)
    {
        return GitConflictClass::CaseCollision;
    }
    let diagnostic = diagnostics.to_ascii_lowercase();
    if paths.len() == 1 || diagnostic.contains(&path.to_ascii_lowercase()) {
        if diagnostic.contains("rename/rename") {
            return GitConflictClass::RenameRename;
        }
        if diagnostic.contains("directory/file") {
            return GitConflictClass::DirectoryFile;
        }
    }
    if [base, local, remote]
        .into_iter()
        .flatten()
        .any(|object| object.kind != "blob")
    {
        return GitConflictClass::UnsupportedObject;
    }
    if base.is_some() && (local.is_none() != remote.is_none()) {
        return GitConflictClass::DeleteModify;
    }
    match file_kind {
        MergeFileKind::ObsidianState => GitConflictClass::DeviceLocalState,
        MergeFileKind::Binary => GitConflictClass::OverlappingBinary,
        MergeFileKind::Missing => GitConflictClass::Ambiguous,
        _ if base.is_some() && local.is_some() && remote.is_some() => {
            GitConflictClass::OverlappingText
        }
        _ => GitConflictClass::Ambiguous,
    }
}

const fn conflict_diagnostic_code(class: GitConflictClass) -> &'static str {
    match class {
        GitConflictClass::OverlappingText => "sync.conflict.overlapping-text",
        GitConflictClass::OverlappingBinary => "sync.conflict.overlapping-binary",
        GitConflictClass::DeleteModify => "sync.conflict.delete-modify",
        GitConflictClass::RenameRename => "sync.conflict.rename-rename",
        GitConflictClass::DirectoryFile => "sync.conflict.directory-file",
        GitConflictClass::CaseCollision => "sync.conflict.case-collision",
        GitConflictClass::DeviceLocalState => "sync.conflict.device-local-state",
        GitConflictClass::UnsupportedObject => "sync.conflict.unsupported-object",
        GitConflictClass::Ambiguous => "sync.conflict.ambiguous",
    }
}

fn try_structured_merge(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    repository: &GitRepository,
    base: Option<&GitOid>,
    local: &GitOid,
    remote: &GitOid,
    paths: &[String],
) -> Result<Option<(GitOid, Vec<GitAutomaticResolution>)>, String> {
    let Some(base) = base else {
        return Ok(None);
    };
    if paths.is_empty() {
        return Ok(None);
    }
    let mut resolved_paths = Vec::with_capacity(paths.len());
    let mut resolutions = Vec::with_capacity(paths.len());
    for path in paths {
        let base_object = engine
            .path_object(repository, base, path)
            .map_err(|error| error.to_string())?;
        let local_object = engine
            .path_object(repository, local, path)
            .map_err(|error| error.to_string())?;
        let remote_object = engine
            .path_object(repository, remote, path)
            .map_err(|error| error.to_string())?;
        if [&base_object, &local_object, &remote_object]
            .into_iter()
            .flatten()
            .any(|object| object.kind != "blob")
        {
            return Ok(None);
        }
        let kind = MergeFileKind::classify(
            path,
            &[
                object_data(base_object.as_ref()),
                object_data(local_object.as_ref()),
                object_data(remote_object.as_ref()),
            ],
        );
        let decision = options
            .merge_policy
            .decision_for(path, kind, options.merge_automation)
            .map_err(|error| error.to_string())?;
        if decision.resolution != MergeResolution::Structured {
            return Ok(None);
        }
        let crate::structured_merge::StructuredMergeOutcome::Resolved(data) =
            crate::structured_merge::merge_structured_path(
                kind,
                object_data(base_object.as_ref()),
                object_data(local_object.as_ref()),
                object_data(remote_object.as_ref()),
                local.as_str(),
                remote.as_str(),
            )?
        else {
            return Ok(None);
        };
        let MergedObjectMode::Resolved(mode) = merge_object_mode(
            base_object.as_ref(),
            local_object.as_ref(),
            remote_object.as_ref(),
        ) else {
            return Ok(None);
        };
        if data.is_none() {
            return Ok(None);
        }
        resolved_paths.push(GitResolvedPath {
            path: path.clone(),
            mode,
            data,
        });
        resolutions.push(GitAutomaticResolution {
            path: path.clone(),
            kind,
            rule_id: decision.rule_id,
            validation: automatic_validation(kind),
        });
    }
    let request = GitContentMergeResolutionRequest {
        base: base.clone(),
        accepted_remote: remote.clone(),
        local_candidate: local.clone(),
        paths: resolved_paths,
    };
    let tree = engine
        .resolve_merge_tree_with_paths(repository, &request)
        .map_err(|error| error.to_string())?;
    validate_resolved_tree(engine, repository, &tree, &request.paths)?;
    for resolution in &mut resolutions {
        resolution
            .validation
            .checks
            .push(GitAutomaticValidationCheck::ExactTreeObject);
    }
    Ok(Some((tree, resolutions)))
}

fn automatic_validation(kind: MergeFileKind) -> GitAutomaticResolutionValidation {
    GitAutomaticResolutionValidation {
        checks: [
            GitAutomaticValidationCheck::PathSafe,
            GitAutomaticValidationCheck::SyntaxValid,
            GitAutomaticValidationCheck::SchemaValid,
            GitAutomaticValidationCheck::NoFileDeletion,
        ]
        .into_iter()
        .chain(
            (kind == MergeFileKind::Markdown)
                .then_some(GitAutomaticValidationCheck::MarkdownLinksPreserved),
        )
        .collect(),
    }
}

fn validate_resolved_tree(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    tree: &GitOid,
    resolved_paths: &[GitResolvedPath],
) -> Result<(), String> {
    for resolved in resolved_paths {
        let Some(expected_data) = resolved.data.as_ref() else {
            return Err(format!(
                "automatic resolution may not delete conflicted path `{}`",
                resolved.path
            ));
        };
        let actual = engine
            .path_object(repository, tree, &resolved.path)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("resolved tree omitted `{}`", resolved.path))?;
        if actual.kind != "blob"
            || actual.mode.as_str() != resolved.mode.as_deref().unwrap_or_default()
            || actual.data.as_deref() != Some(expected_data.as_slice())
        {
            return Err(format!(
                "resolved tree object for `{}` differs from the validated result",
                resolved.path
            ));
        }
    }
    Ok(())
}

fn object_data(object: Option<&GitPathObject>) -> Option<&[u8]> {
    object.and_then(|object| object.data.as_deref())
}

enum MergedObjectMode {
    Resolved(Option<String>),
    Unresolved,
}

fn merge_object_mode(
    base: Option<&GitPathObject>,
    local: Option<&GitPathObject>,
    remote: Option<&GitPathObject>,
) -> MergedObjectMode {
    let base = base.map(|object| object.mode.as_str());
    let local = local.map(|object| object.mode.as_str());
    let remote = remote.map(|object| object.mode.as_str());
    if local == remote {
        MergedObjectMode::Resolved(local.map(str::to_string))
    } else if local == base {
        MergedObjectMode::Resolved(remote.map(str::to_string))
    } else if remote == base {
        MergedObjectMode::Resolved(local.map(str::to_string))
    } else {
        MergedObjectMode::Unresolved
    }
}

fn conflict_identity(
    policy: &MergePolicy,
    base: Option<&GitOid>,
    local: &GitOid,
    remote: &GitOid,
    paths: &[String],
) -> Result<(String, String), GitSyncError> {
    let policy_hash = policy.policy_hash().map_err(|error| {
        GitSyncError::Git(GitEngineError::UnsupportedRepository {
            detail: error.to_string(),
        })
    })?;
    let mut candidates = [local.as_str(), remote.as_str()];
    candidates.sort_unstable();
    let mut canonical_paths = paths.to_vec();
    canonical_paths.sort();
    canonical_paths.dedup();
    let identity = format!(
        "{}\0{policy_hash}\0{}\0{}\0{}\0{}",
        policy.version,
        base.map_or("-", GitOid::as_str),
        candidates[0],
        candidates[1],
        canonical_paths.join("\0")
    );
    Ok((
        blake3::hash(identity.as_bytes()).to_hex()[..32].to_string(),
        policy_hash,
    ))
}

struct ConflictPreservationRequest<'a> {
    options: &'a GitSyncOptions,
    id: &'a str,
    policy_hash: &'a str,
    base: Option<&'a GitOid>,
    local: &'a GitOid,
    remote: &'a GitOid,
    merge_tree: Option<&'a GitOid>,
}

fn preserve_conflict_refs(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    request: &ConflictPreservationRequest<'_>,
) -> Result<(GitConflictRefs, GitOid), GitSyncError> {
    let options = request.options;
    let id = request.id;
    let policy_hash = request.policy_hash;
    let base = request.base;
    let local = request.local;
    let remote = request.remote;
    let merge_tree = request.merge_tree;
    let base_ref = base
        .map(|_| GitRefName::parse(format!("refs/vulcan/conflicts/{id}/base")))
        .transpose()?;
    let local_ref = GitRefName::parse(format!("refs/vulcan/conflicts/{id}/local"))?;
    let remote_ref = GitRefName::parse(format!("refs/vulcan/conflicts/{id}/remote"))?;
    let record_ref = GitRefName::parse(format!("refs/vulcan/conflicts/{id}/record"))?;
    if let (Some(base), Some(reference)) = (base, base_ref.as_ref()) {
        preserve_exact_ref(engine, repository, reference, base)?;
    }
    preserve_exact_ref(engine, repository, &local_ref, local)?;
    preserve_exact_ref(engine, repository, &remote_ref, remote)?;
    let tree = merge_tree
        .cloned()
        .map_or_else(|| engine.tree_oid(repository, local), Ok)?;
    let refs = GitSyncRefs::for_options(options)?;
    let base_trailer = base.map_or_else(
        || "Vulcan-Conflict-Base: none\n".to_string(),
        |base| format!("Vulcan-Conflict-Base: {base}\n"),
    );
    let provenance_revision = engine.create_reproducible_commit(
        repository,
        &tree,
        &[remote.clone(), local.clone()],
        &format!(
            "vulcan preserved conflict\n\nVulcan-Conflict: {id}\n{base_trailer}{}",
            sync_trailers(
                &refs,
                options,
                &format!("{remote}+{local};policy={policy_hash}"),
            )
        ),
    )?;
    preserve_exact_ref(engine, repository, &record_ref, &provenance_revision)?;
    Ok((
        GitConflictRefs {
            base: base_ref,
            local: local_ref,
            remote: remote_ref,
            record: record_ref,
        },
        provenance_revision,
    ))
}

fn preserve_exact_ref(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    reference: &GitRefName,
    target: &GitOid,
) -> Result<(), GitSyncError> {
    if engine.read_ref(repository, reference)?.as_ref() == Some(target) {
        return Ok(());
    }
    if engine.create_ref(repository, reference, target)? == crate::GitRefCreateResult::Created
        || engine.read_ref(repository, reference)?.as_ref() == Some(target)
    {
        return Ok(());
    }
    Err(GitEngineError::UnsupportedRepository {
        detail: format!(
            "preserved conflict ref `{reference}` already identifies a different commit"
        ),
    }
    .into())
}

fn snapshot_message(
    refs: &GitSyncRefs,
    options: &GitSyncOptions,
    source: Option<&GitOid>,
) -> String {
    format!(
        "vulcan live snapshot\n\n{}",
        sync_trailers(refs, options, source.map_or("unborn", GitOid::as_str))
    )
}

fn merge_message(
    refs: &GitSyncRefs,
    options: &GitSyncOptions,
    remote: &GitOid,
    local: &GitOid,
) -> String {
    format!(
        "vulcan live merge\n\n{}",
        sync_trailers(refs, options, &format!("{remote}+{local}"))
    )
}

fn sync_trailers(refs: &GitSyncRefs, options: &GitSyncOptions, source: &str) -> String {
    let policy_hash = options
        .merge_policy
        .policy_hash()
        .expect("sync policy is validated before creating commits");
    format!(
        "Vulcan-Sync-Version: {SYNC_PROTOCOL_VERSION}\nVulcan-Sync-Device: {}\nVulcan-Sync-Profile: {}\nVulcan-Sync-Policy: {}:{policy_hash}\nVulcan-Sync-Source: {source}\nVulcan-Sync-Semantic: false\n",
        options.device_id.as_str(),
        refs.local
            .as_str()
            .split('/')
            .nth(3)
            .unwrap_or("unknown"),
        options.merge_policy.version,
    )
}

struct RepositoryLock {
    _file: File,
}

impl RepositoryLock {
    fn acquire(repository: &GitRepository) -> Result<Self, GitSyncError> {
        let lock_path = repository.git_dir.join("vulcan-sync/sync.lock");
        fs::create_dir_all(
            lock_path
                .parent()
                .expect("the sync lock path always has a parent"),
        )?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)?;
        file.try_lock_exclusive().map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                GitSyncError::Locked
            } else {
                GitSyncError::Io(error)
            }
        })?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GitCliEngine;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    fn conflict_blob(data: &[u8]) -> GitPathObject {
        GitPathObject {
            mode: "100644".to_string(),
            kind: "blob".to_string(),
            oid: GitOid::parse("1111111111111111111111111111111111111111").expect("object ID"),
            data: Some(data.to_vec()),
        }
    }

    #[test]
    fn conflict_classes_cover_structural_content_and_portability_failures() {
        let text = conflict_blob(b"text\n");
        let binary = conflict_blob(b"\0binary");
        let paths = vec!["Note.md".to_string()];
        assert_eq!(
            conflict_class(
                "Note.md",
                &paths,
                "CONFLICT (content)",
                Some(&text),
                Some(&text),
                Some(&text),
                MergeFileKind::Markdown,
            ),
            GitConflictClass::OverlappingText
        );
        assert_eq!(
            conflict_class(
                "asset.bin",
                &["asset.bin".to_string()],
                "CONFLICT (content)",
                Some(&binary),
                Some(&binary),
                Some(&binary),
                MergeFileKind::Binary,
            ),
            GitConflictClass::OverlappingBinary
        );
        assert_eq!(
            conflict_class(
                "Note.md",
                &paths,
                "CONFLICT (modify/delete)",
                Some(&text),
                Some(&text),
                None,
                MergeFileKind::Markdown,
            ),
            GitConflictClass::DeleteModify
        );
        assert_eq!(
            conflict_class(
                "renamed.md",
                &["renamed.md".to_string()],
                "CONFLICT (rename/rename)",
                Some(&text),
                Some(&text),
                Some(&text),
                MergeFileKind::Markdown,
            ),
            GitConflictClass::RenameRename
        );
        assert_eq!(
            conflict_class(
                "Notes",
                &["Notes".to_string()],
                "CONFLICT (directory/file)",
                Some(&text),
                Some(&text),
                Some(&text),
                MergeFileKind::Text,
            ),
            GitConflictClass::DirectoryFile
        );
        assert_eq!(
            conflict_class(
                "Note.md",
                &["Note.md".to_string(), "note.md".to_string()],
                "",
                Some(&text),
                Some(&text),
                Some(&text),
                MergeFileKind::Markdown,
            ),
            GitConflictClass::CaseCollision
        );
        assert_eq!(
            conflict_class(
                ".obsidian/workspace.json",
                &[".obsidian/workspace.json".to_string()],
                "CONFLICT (content)",
                Some(&text),
                Some(&text),
                Some(&text),
                MergeFileKind::ObsidianState,
            ),
            GitConflictClass::DeviceLocalState
        );
    }

    #[derive(Default)]
    struct RecordingObserver {
        phases: Vec<GitSyncPhase>,
        cancel_on: Option<GitSyncPhase>,
        cancellation: SyncCancellationToken,
    }

    impl GitSyncObserver for RecordingObserver {
        fn progress(&mut self, progress: &GitSyncProgress) -> Result<(), GitSyncObserverError> {
            self.phases.push(progress.phase);
            if self.cancel_on == Some(progress.phase) {
                self.cancellation.cancel();
            }
            Ok(())
        }
    }

    struct RejectFirstPushObserver {
        repository: PathBuf,
        fired: bool,
    }

    struct EditBeforePushObserver {
        repository: PathBuf,
        remote: PathBuf,
        fired: bool,
        remote_was_absent_on_retry: bool,
    }

    impl GitSyncObserver for EditBeforePushObserver {
        fn progress(&mut self, progress: &GitSyncProgress) -> Result<(), GitSyncObserverError> {
            if progress.phase == GitSyncPhase::Pushing && !self.fired {
                self.fired = true;
                fs::write(
                    self.repository.join("Home.md"),
                    "edited during pre-push validation\n",
                )
                .map_err(|error| GitSyncObserverError::new(error.to_string()))?;
            } else if progress.phase == GitSyncPhase::Capturing && progress.attempt == 1 {
                let status = Command::new("git")
                    .args(["--git-dir"])
                    .arg(&self.remote)
                    .args([
                        "rev-parse",
                        "--quiet",
                        "--verify",
                        "refs/heads/__vulcan-sync/live",
                    ])
                    .status()
                    .map_err(|error| GitSyncObserverError::new(error.to_string()))?;
                self.remote_was_absent_on_retry = !status.success();
            }
            Ok(())
        }
    }

    struct MoveHeadObserver {
        repository: PathBuf,
        target: String,
        fired: bool,
    }

    impl GitSyncObserver for MoveHeadObserver {
        fn progress(&mut self, progress: &GitSyncProgress) -> Result<(), GitSyncObserverError> {
            if progress.phase == GitSyncPhase::Fetching && !self.fired {
                self.fired = true;
                run_git(
                    &self.repository,
                    &["update-ref", "refs/heads/main", &self.target],
                );
            }
            Ok(())
        }
    }

    struct SwitchHeadObserver {
        repository: PathBuf,
        fired: bool,
    }

    impl GitSyncObserver for SwitchHeadObserver {
        fn progress(&mut self, progress: &GitSyncProgress) -> Result<(), GitSyncObserverError> {
            if progress.phase == GitSyncPhase::Fetching && !self.fired {
                self.fired = true;
                run_git(
                    &self.repository,
                    &["symbolic-ref", "HEAD", "refs/heads/other"],
                );
            }
            Ok(())
        }
    }

    impl GitSyncObserver for RejectFirstPushObserver {
        fn progress(&mut self, progress: &GitSyncProgress) -> Result<(), GitSyncObserverError> {
            if progress.phase == GitSyncPhase::Pushing && !self.fired {
                self.fired = true;
                run_git(
                    &self.repository,
                    &[
                        "push",
                        "--quiet",
                        "origin",
                        "HEAD:refs/heads/__vulcan-sync/live",
                    ],
                );
            }
            Ok(())
        }
    }

    fn run_git(path: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .current_dir(path)
            .args(arguments)
            .status()
            .expect("Git should launch");
        assert!(status.success(), "Git failed: {arguments:?}");
    }

    fn git_stdout(path: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(path)
            .args(arguments)
            .output()
            .expect("Git should launch");
        assert!(output.status.success(), "Git failed: {arguments:?}");
        String::from_utf8(output.stdout)
            .expect("Git output should be UTF-8")
            .trim()
            .to_string()
    }

    fn init_repo(path: &Path) {
        run_git(path, &["-c", "init.defaultBranch=main", "init", "--quiet"]);
        run_git(path, &["config", "user.name", "Vulcan Test"]);
        run_git(path, &["config", "user.email", "vulcan@example.invalid"]);
    }

    fn commit_all(path: &Path, message: &str) {
        run_git(path, &["add", "--all", "--", "."]);
        run_git(path, &["commit", "--quiet", "-m", message]);
    }

    fn setup_remote_and_writer() -> (TempDir, PathBuf, PathBuf) {
        let temporary = TempDir::new().expect("temporary directory");
        let remote = temporary.path().join("remote.git");
        run_git(
            temporary.path(),
            &[
                "init",
                "--quiet",
                "--bare",
                remote.to_str().expect("remote path"),
            ],
        );
        let writer = temporary.path().join("writer");
        fs::create_dir(&writer).expect("writer directory");
        init_repo(&writer);
        run_git(
            &writer,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        fs::write(writer.join("Home.md"), "initial\n").expect("initial note");
        commit_all(&writer, "initial");
        (temporary, remote, writer)
    }

    fn clone_reader(temporary: &TempDir, remote: &Path, writer: &Path) -> PathBuf {
        let reader = temporary.path().join("reader");
        run_git(
            temporary.path(),
            &[
                "clone",
                "--quiet",
                writer.to_str().expect("writer path"),
                reader.to_str().expect("reader path"),
            ],
        );
        run_git(
            &reader,
            &[
                "remote",
                "set-url",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        run_git(&reader, &["config", "user.name", "Vulcan Test"]);
        run_git(&reader, &["config", "user.email", "vulcan@example.invalid"]);
        reader
    }

    #[test]
    fn first_sync_bootstraps_the_remote_live_ref() {
        let (_temporary, _remote, writer) = setup_remote_and_writer();

        let report = sync_git_once(
            &GitCliEngine::default(),
            &writer,
            &GitSyncOptions::default(),
        )
        .expect("sync should succeed");

        assert_eq!(report.outcome, GitSyncOutcome::Bootstrapped);
        assert!(report.actions.contains(&GitSyncAction::Pushed));
        assert!(!report.actions.contains(&GitSyncAction::WorktreeApplied));
        assert_eq!(report.accepted, report.local_snapshot);
    }

    #[test]
    fn live_snapshot_commits_record_versioned_machine_readable_provenance() {
        let (_temporary, _remote, writer) = setup_remote_and_writer();
        fs::write(writer.join("Home.md"), "changed before sync\n").expect("local edit");
        let device_id = GitSyncDeviceId::parse("01arz3ndektsv4rrffq69g5fav").expect("device ID");
        let options = GitSyncOptions {
            device_id: device_id.clone(),
            ..GitSyncOptions::default()
        };

        let report = sync_git_once(&GitCliEngine::default(), &writer, &options)
            .expect("snapshot sync should succeed");
        let snapshot = report.local_snapshot.expect("snapshot commit");
        let message = git_stdout(&writer, &["show", "-s", "--format=%B", snapshot.as_str()]);

        assert!(message.starts_with("vulcan live snapshot\n\n"));
        assert!(message.contains("Vulcan-Sync-Version: 1"));
        assert!(message.contains(&format!("Vulcan-Sync-Device: {}", device_id.as_str())));
        assert!(message.contains("Vulcan-Sync-Profile:"));
        assert!(message.contains("Vulcan-Sync-Policy: 1:"));
        assert!(message.contains("Vulcan-Sync-Source:"));
        assert!(message.contains("Vulcan-Sync-Semantic: false"));
    }

    #[test]
    fn conflict_identity_is_independent_of_candidate_and_path_arrival_order() {
        let policy = MergePolicy::default();
        let base = GitOid::parse("0".repeat(40)).expect("base oid");
        let first = GitOid::parse("1".repeat(40)).expect("first oid");
        let second = GitOid::parse("2".repeat(40)).expect("second oid");
        let (forward, forward_policy) = conflict_identity(
            &policy,
            Some(&base),
            &first,
            &second,
            &["B.md".to_string(), "A.md".to_string(), "A.md".to_string()],
        )
        .expect("forward identity");
        let (swapped, swapped_policy) = conflict_identity(
            &policy,
            Some(&base),
            &second,
            &first,
            &["A.md".to_string(), "B.md".to_string()],
        )
        .expect("swapped identity");
        assert_eq!(forward, swapped);
        assert_eq!(forward_policy, swapped_policy);
    }

    #[test]
    fn progress_reports_ordered_finite_cycle_phases() {
        let (_temporary, _remote, writer) = setup_remote_and_writer();
        let cancellation = SyncCancellationToken::default();
        let mut observer = RecordingObserver {
            cancellation: cancellation.clone(),
            ..RecordingObserver::default()
        };

        let report = sync_git_once_with_control(
            &GitCliEngine::default(),
            &writer,
            &GitSyncOptions::default(),
            &cancellation,
            &mut observer,
        )
        .expect("sync with progress");

        assert_eq!(report.outcome, GitSyncOutcome::Bootstrapped);
        assert_eq!(
            observer.phases,
            [
                GitSyncPhase::Preparing,
                GitSyncPhase::Capturing,
                GitSyncPhase::Captured,
                GitSyncPhase::Fetching,
                GitSyncPhase::Pushing,
                GitSyncPhase::Verifying,
                GitSyncPhase::Completed,
            ]
        );
    }

    #[test]
    fn git_backend_exposes_capabilities_and_runs_the_shared_contract() {
        let (_temporary, _remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();
        let backend = GitSyncBackend::new(&engine, GitSyncOptions::default());
        let observer = crate::IgnoreSyncProgress;
        let context = SyncContext::new(&writer, false, &observer);

        let plan = backend.plan(&context).expect("backend plan");
        assert_eq!(plan.backend, "git");
        assert_eq!(plan.operations[0], SyncOperation::Capture);
        assert_eq!(
            plan.capabilities.operation_modes,
            [SyncOperationMode::Finite]
        );
        assert!(plan.capabilities.supports(SyncCapability::SafeCancel));
        assert!(plan
            .capabilities
            .supports(SyncCapability::ConflictPreservation));

        let report = backend
            .sync_once(&context, &SyncCancellationToken::default())
            .expect("backend cycle");
        assert_eq!(report.version, SYNC_CONTRACT_VERSION);
        assert_eq!(report.outcome, SyncOutcome::Bootstrapped);
        assert_eq!(report.status.state, SyncState::Clean);
        assert_eq!(report.attempts, 1);
        assert!(report.status.accepted_revision.is_some());
    }

    #[test]
    fn backend_errors_expose_stable_categories_and_retry_guidance() {
        let cancelled = sync_error_from_git(&GitSyncError::Cancelled);
        assert_eq!(cancelled.category, SyncErrorCategory::Cancelled);
        assert!(!cancelled.retryable);

        let unavailable =
            sync_error_from_git(&GitSyncError::Git(GitEngineError::ExecutableUnavailable {
                executable: PathBuf::from("git"),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
            }));
        assert_eq!(unavailable.category, SyncErrorCategory::Configuration);
        assert!(!unavailable.retryable);

        let remote = sync_error_from_git(&GitSyncError::Git(GitEngineError::CommandFailed {
            operation: "fetch the live sync ref",
            exit_code: Some(128),
            stderr: "offline".to_string(),
        }));
        assert_eq!(remote.category, SyncErrorCategory::Network);
        assert!(remote.retryable);
    }

    #[test]
    fn rejected_push_retry_backoff_is_exponential_and_bounded() {
        assert_eq!(retry_backoff(0), Duration::from_millis(25));
        assert_eq!(retry_backoff(1), Duration::from_millis(50));
        assert_eq!(retry_backoff(4), Duration::from_millis(400));
        assert_eq!(retry_backoff(usize::MAX), Duration::from_millis(400));
    }

    #[test]
    fn rejected_compare_and_swap_is_refetched_and_retried() {
        let (temporary, remote, writer) = setup_remote_and_writer();
        let peer = clone_reader(&temporary, &remote, &writer);
        fs::write(peer.join("Race.md"), "peer won the first push\n").expect("peer edit");
        commit_all(&peer, "peer race");
        let cancellation = SyncCancellationToken::default();
        let mut observer = RejectFirstPushObserver {
            repository: peer,
            fired: false,
        };

        let report = sync_git_once_with_control(
            &GitCliEngine::default(),
            &writer,
            &GitSyncOptions::default(),
            &cancellation,
            &mut observer,
        )
        .expect("rejected push should converge on retry");

        assert!(observer.fired);
        assert_eq!(report.retries, 1);
        assert_eq!(report.outcome, GitSyncOutcome::Pulled);
        assert_eq!(
            fs::read_to_string(writer.join("Race.md")).expect("peer file pulled"),
            "peer won the first push\n"
        );
    }

    #[test]
    fn worktree_change_before_bootstrap_push_is_recaptured_before_publication() {
        let (_temporary, remote, writer) = setup_remote_and_writer();
        let cancellation = SyncCancellationToken::default();
        let mut observer = EditBeforePushObserver {
            repository: writer.clone(),
            remote,
            fired: false,
            remote_was_absent_on_retry: false,
        };

        let report = sync_git_once_with_control(
            &GitCliEngine::default(),
            &writer,
            &GitSyncOptions::default(),
            &cancellation,
            &mut observer,
        )
        .expect("changed worktree should be recaptured");

        assert!(observer.fired);
        assert!(observer.remote_was_absent_on_retry);
        assert_eq!(report.retries, 1);
        assert_eq!(report.outcome, GitSyncOutcome::Bootstrapped);
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("current note"),
            "edited during pre-push validation\n"
        );
    }

    #[test]
    fn cancellation_after_capture_preserves_the_local_snapshot_ref() {
        let (_temporary, _remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();
        let options = GitSyncOptions::default();
        let cancellation = SyncCancellationToken::default();
        let mut observer = RecordingObserver {
            cancel_on: Some(GitSyncPhase::Captured),
            cancellation: cancellation.clone(),
            ..RecordingObserver::default()
        };

        assert!(matches!(
            sync_git_once_with_control(&engine, &writer, &options, &cancellation, &mut observer,),
            Err(GitSyncError::Cancelled)
        ));

        let repository = engine
            .discover_repository(&writer)
            .expect("repository after cancellation");
        let refs = GitSyncRefs::for_options(&options).expect("sync refs");
        assert!(engine
            .read_ref(&repository, &refs.local)
            .expect("local ref")
            .is_some());
        assert_eq!(
            engine
                .remote_ref(&repository, &options.remote, &options.live_ref)
                .expect("remote ref"),
            None
        );
    }

    #[test]
    fn unavailable_remote_does_not_prevent_local_capture() {
        let temporary = TempDir::new().expect("temporary directory");
        let writer = temporary.path().join("writer");
        fs::create_dir(&writer).expect("writer directory");
        init_repo(&writer);
        run_git(
            &writer,
            &[
                "remote",
                "add",
                "origin",
                temporary
                    .path()
                    .join("missing.git")
                    .to_str()
                    .expect("remote path"),
            ],
        );
        fs::write(writer.join("Home.md"), "offline work\n").expect("offline note");
        let engine = GitCliEngine::default();
        let options = GitSyncOptions::default();

        assert!(sync_git_once(&engine, &writer, &options).is_err());

        let repository = engine
            .discover_repository(&writer)
            .expect("repository after failed remote access");
        let refs = GitSyncRefs::for_options(&options).expect("sync refs");
        assert!(engine
            .read_ref(&repository, &refs.local)
            .expect("local candidate")
            .is_some());
    }

    #[test]
    fn dry_run_does_not_create_local_refs_or_remote_state() {
        let (_temporary, _remote, writer) = setup_remote_and_writer();
        let options = GitSyncOptions {
            dry_run: true,
            ..GitSyncOptions::default()
        };
        let engine = GitCliEngine::default();

        let report = sync_git_once(&engine, &writer, &options).expect("plan should succeed");

        assert_eq!(report.outcome, GitSyncOutcome::Planned);
        assert_eq!(report.remote_before, None);
        assert_eq!(
            engine
                .read_ref(&report.repository, &report.refs.local)
                .expect("local ref"),
            None
        );
        assert_eq!(
            engine
                .remote_ref(&report.repository, &options.remote, &options.live_ref)
                .expect("remote ref"),
            None
        );
    }

    #[test]
    fn staged_changes_are_captured_before_reconciliation_pauses() {
        let (_temporary, remote, writer) = setup_remote_and_writer();
        fs::write(writer.join("Home.md"), "staged\n").expect("staged note");
        run_git(&writer, &["add", "Home.md"]);

        let report = sync_git_once(
            &GitCliEngine::default(),
            &writer,
            &GitSyncOptions::default(),
        )
        .expect("paused sync should report normally");

        assert_eq!(report.outcome, GitSyncOutcome::Paused);
        assert!(report.safety.staged_changes);
        assert!(report.local_snapshot.is_some());
        assert_eq!(
            report.pause.as_ref().map(|pause| pause.reason),
            Some(GitSyncPauseReason::StagedChanges)
        );
        assert_eq!(
            GitCliEngine::default()
                .remote_ref(
                    &report.repository,
                    &GitRemote::parse(remote.to_string_lossy()).expect("remote"),
                    &report.refs.live,
                )
                .expect("remote query"),
            None
        );
    }

    #[test]
    fn staged_changes_still_fetch_the_remote_before_pausing() {
        let (temporary, remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("bootstrap sync");
        let reader = clone_reader(&temporary, &remote, &writer);
        fs::write(writer.join("Remote.md"), "remote\n").expect("remote note");
        let pushed =
            sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("remote update");
        let remote_tip = pushed.accepted.expect("accepted remote update");
        fs::write(reader.join("Home.md"), "staged locally\n").expect("local staged edit");
        run_git(&reader, &["add", "Home.md"]);

        let report = sync_git_once(&engine, &reader, &GitSyncOptions::default())
            .expect("paused sync should report normally");

        assert_eq!(report.outcome, GitSyncOutcome::Paused);
        assert!(report.local_snapshot.is_some());
        assert_eq!(
            report.pause.as_ref().map(|pause| pause.reason),
            Some(GitSyncPauseReason::StagedChanges)
        );
        assert_eq!(
            engine
                .read_ref(&report.repository, &report.refs.fetched)
                .expect("fetched ref"),
            Some(remote_tip)
        );
        assert!(!reader.join("Remote.md").exists());
    }

    #[test]
    fn in_progress_git_operation_is_captured_before_pausing() {
        let (_temporary, _remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();
        let repository = engine.discover_repository(&writer).expect("repository");
        let head = engine
            .head_commit(&repository)
            .expect("head query")
            .expect("head");
        fs::write(repository.git_dir.join("MERGE_HEAD"), format!("{head}\n")).expect("merge state");

        let report = sync_git_once(&engine, &writer, &GitSyncOptions::default())
            .expect("paused sync should report normally");

        assert_eq!(report.outcome, GitSyncOutcome::Paused);
        assert!(report.local_snapshot.is_some());
        assert_eq!(
            report.pause.as_ref().map(|pause| pause.reason),
            Some(GitSyncPauseReason::OperationInProgress)
        );
        assert_eq!(
            report.pause.and_then(|pause| pause.operation),
            Some("merge".to_string())
        );
    }

    #[test]
    fn unexplained_head_movement_pauses_after_capture() {
        let (_temporary, _remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();
        let repository = engine.discover_repository(&writer).expect("repository");
        let expected_head = engine
            .head_commit(&repository)
            .expect("head query")
            .expect("initial head");
        fs::write(writer.join("Moved.md"), "moved head\n").expect("moved note");
        commit_all(&writer, "alternate head");
        let moved_head = engine
            .head_commit(&repository)
            .expect("head query")
            .expect("moved head");
        run_git(&writer, &["reset", "--hard", expected_head.as_str()]);
        let cancellation = SyncCancellationToken::default();
        let mut observer = MoveHeadObserver {
            repository: writer.clone(),
            target: moved_head.to_string(),
            fired: false,
        };

        let report = sync_git_once_with_control(
            &engine,
            &writer,
            &GitSyncOptions::default(),
            &cancellation,
            &mut observer,
        )
        .expect("head movement should produce a paused report");

        assert!(observer.fired);
        assert_eq!(report.outcome, GitSyncOutcome::Paused);
        assert!(report.local_snapshot.is_some());
        assert_eq!(
            report.pause,
            Some(GitSyncPause {
                reason: GitSyncPauseReason::HeadMoved,
                expected_head: Some(expected_head),
                actual_head: Some(moved_head),
                expected_head_ref: Some(GitRefName::parse("refs/heads/main").expect("main ref"),),
                actual_head_ref: Some(GitRefName::parse("refs/heads/main").expect("main ref")),
                operation: None,
            })
        );
    }

    #[test]
    fn switching_branches_at_the_same_commit_pauses_after_capture() {
        let (_temporary, _remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();
        let repository = engine.discover_repository(&writer).expect("repository");
        let head = engine
            .head_commit(&repository)
            .expect("head query")
            .expect("initial head");
        engine
            .create_ref(
                &repository,
                &GitRefName::parse("refs/heads/other").expect("other ref"),
                &head,
            )
            .expect("other branch");
        let cancellation = SyncCancellationToken::default();
        let mut observer = SwitchHeadObserver {
            repository: writer.clone(),
            fired: false,
        };

        let report = sync_git_once_with_control(
            &engine,
            &writer,
            &GitSyncOptions::default(),
            &cancellation,
            &mut observer,
        )
        .expect("branch movement should produce a paused report");

        let pause = report.pause.expect("pause detail");
        assert!(observer.fired);
        assert_eq!(report.outcome, GitSyncOutcome::Paused);
        assert_eq!(pause.reason, GitSyncPauseReason::HeadMoved);
        assert_eq!(pause.expected_head, Some(head.clone()));
        assert_eq!(pause.actual_head, Some(head));
        assert_eq!(
            pause.expected_head_ref.as_ref().map(GitRefName::as_str),
            Some("refs/heads/main")
        );
        assert_eq!(
            pause.actual_head_ref.as_ref().map(GitRefName::as_str),
            Some("refs/heads/other")
        );
    }

    #[test]
    fn two_worktrees_pull_and_merge_non_overlapping_changes() {
        let (temporary, remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("bootstrap sync");
        let reader = clone_reader(&temporary, &remote, &writer);

        fs::write(writer.join("Writer.md"), "from writer\n").expect("writer edit");
        let writer_report =
            sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("writer push");
        assert_eq!(writer_report.outcome, GitSyncOutcome::Pushed);

        let pull_report =
            sync_git_once(&engine, &reader, &GitSyncOptions::default()).expect("reader pull");
        assert_eq!(pull_report.outcome, GitSyncOutcome::Pulled);
        assert_eq!(
            fs::read_to_string(reader.join("Writer.md")).expect("pulled note"),
            "from writer\n"
        );

        fs::write(writer.join("Writer.md"), "writer revision\n").expect("writer revision");
        fs::write(reader.join("Reader.md"), "from reader\n").expect("reader edit");
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("writer push");
        let merge_report =
            sync_git_once(&engine, &reader, &GitSyncOptions::default()).expect("reader merge");
        assert_eq!(merge_report.outcome, GitSyncOutcome::Merged);
        assert!(merge_report.actions.contains(&GitSyncAction::Pushed));
        assert!(merge_report
            .actions
            .contains(&GitSyncAction::WorktreeApplied));
        assert_eq!(
            fs::read_to_string(reader.join("Writer.md")).expect("merged writer note"),
            "writer revision\n"
        );
        assert_eq!(
            fs::read_to_string(reader.join("Reader.md")).expect("local reader note"),
            "from reader\n"
        );

        let convergence = sync_git_once(&engine, &writer, &GitSyncOptions::default())
            .expect("writer convergence pull");
        assert_eq!(convergence.outcome, GitSyncOutcome::Pulled);
        assert_eq!(
            fs::read_to_string(writer.join("Reader.md")).expect("converged reader note"),
            "from reader\n"
        );
    }

    #[test]
    fn conflicting_edits_are_reported_without_overwriting_local_bytes() {
        let (temporary, remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("bootstrap sync");
        let reader = clone_reader(&temporary, &remote, &writer);
        sync_git_once(&engine, &reader, &GitSyncOptions::default()).expect("reader baseline");

        fs::write(writer.join("Home.md"), "writer version\n").expect("writer edit");
        fs::write(reader.join("Home.md"), "reader version\n").expect("reader edit");
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("writer push");
        let report =
            sync_git_once(&engine, &reader, &GitSyncOptions::default()).expect("conflict report");

        assert_eq!(report.outcome, GitSyncOutcome::Conflicted);
        let conflict = report.conflict.as_ref().expect("conflict details");
        assert_eq!(conflict.paths, ["Home.md"]);
        assert_eq!(conflict.classifications.len(), 1);
        assert_eq!(
            conflict.classifications[0].class,
            GitConflictClass::OverlappingText
        );
        assert_eq!(
            conflict.classifications[0].configured_resolution,
            MergeResolution::Structured
        );
        assert_eq!(
            conflict.classifications[0].effective_resolution,
            MergeResolution::RequireReview
        );
        assert!(conflict.base.is_some());
        assert_eq!(conflict.id.len(), 32);
        assert_eq!(conflict.policy_version, MergePolicy::default().version);
        assert_eq!(
            engine
                .read_ref(&report.repository, &conflict.preserved_refs.local)
                .expect("conflict local ref"),
            Some(conflict.local.clone())
        );
        assert_eq!(
            engine
                .read_ref(&report.repository, &conflict.preserved_refs.remote)
                .expect("conflict remote ref"),
            Some(conflict.remote.clone())
        );
        assert_eq!(
            conflict.preserved_refs.base.as_ref().map(|reference| engine
                .read_ref(&report.repository, reference)
                .expect("base ref")),
            Some(conflict.base.clone())
        );
        assert_eq!(
            engine
                .read_ref(&report.repository, &conflict.preserved_refs.record)
                .expect("provenance ref"),
            Some(conflict.provenance_revision.clone())
        );
        let provenance_message = git_stdout(
            reader.as_path(),
            &[
                "show",
                "-s",
                "--format=%B",
                conflict.provenance_revision.as_str(),
            ],
        );
        assert!(provenance_message.contains(&format!("Vulcan-Conflict: {}", conflict.id)));
        assert!(provenance_message.contains("Vulcan-Sync-Semantic: false"));
        assert_eq!(
            fs::read_to_string(reader.join("Home.md")).expect("preserved reader note"),
            "reader version\n"
        );
        assert_eq!(
            engine
                .read_ref(&report.repository, &report.refs.local)
                .expect("preserved local ref"),
            report.local_snapshot
        );

        let repeated = sync_git_once(&engine, &reader, &GitSyncOptions::default())
            .expect("repeat conflict report");
        assert_eq!(
            repeated.conflict.as_ref().map(|item| &item.id),
            Some(&conflict.id)
        );
        engine
            .update_ref(
                &report.repository,
                &conflict.preserved_refs.record,
                &conflict.local,
            )
            .expect("simulate provenance ref tampering");
        let error = sync_git_once(&engine, &reader, &GitSyncOptions::default())
            .expect_err("a changed preservation ref must fail closed");
        assert!(error
            .to_string()
            .contains("already identifies a different commit"));
    }

    #[test]
    fn structured_json_conflicts_are_resolved_and_reported_deterministically() {
        let (temporary, remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();
        fs::write(writer.join("data.json"), "{\"base\":true}\n").expect("base JSON");
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("bootstrap sync");
        let reader = clone_reader(&temporary, &remote, &writer);
        sync_git_once(&engine, &reader, &GitSyncOptions::default()).expect("reader baseline");

        fs::write(writer.join("data.json"), "{\"base\":true,\"writer\":1}\n").expect("writer JSON");
        fs::write(reader.join("data.json"), "{\"base\":true,\"reader\":2}\n").expect("reader JSON");
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("writer push");
        let report =
            sync_git_once(&engine, &reader, &GitSyncOptions::default()).expect("structured merge");

        assert_eq!(report.outcome, GitSyncOutcome::Merged);
        assert!(report.conflict.is_none());
        assert_eq!(report.automatic_resolutions.len(), 1);
        let resolution = &report.automatic_resolutions[0];
        assert_eq!(resolution.path, "data.json");
        assert_eq!(resolution.kind, MergeFileKind::Json);
        assert_eq!(resolution.rule_id, "json-structured");
        assert_eq!(
            resolution.validation,
            GitAutomaticResolutionValidation {
                checks: vec![
                    GitAutomaticValidationCheck::PathSafe,
                    GitAutomaticValidationCheck::SyntaxValid,
                    GitAutomaticValidationCheck::SchemaValid,
                    GitAutomaticValidationCheck::NoFileDeletion,
                    GitAutomaticValidationCheck::ExactTreeObject,
                ],
            }
        );
        let merged: serde_json::Value =
            serde_json::from_slice(&fs::read(reader.join("data.json")).expect("merged JSON bytes"))
                .expect("merged JSON");
        assert_eq!(
            merged,
            serde_json::json!({"base": true, "reader": 2, "writer": 1})
        );
    }

    #[test]
    fn shared_policy_can_narrowly_merge_selected_obsidian_plugin_json() {
        let (temporary, remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();
        let relative = ".obsidian/plugins/example/data.json";
        fs::create_dir_all(writer.join(".obsidian/plugins/example")).expect("plugin directory");
        fs::write(writer.join(relative), "{\"base\":true}\n").expect("base plugin state");
        let policy = MergePolicy {
            version: crate::MERGE_POLICY_SCHEMA_VERSION,
            rules: vec![
                crate::MergePolicyRule {
                    id: "selected-plugin-state".to_string(),
                    selector: crate::MergePathSelector {
                        glob: relative.to_string(),
                        kinds: vec![MergeFileKind::ObsidianState],
                    },
                    resolution: MergeResolution::Structured,
                },
                crate::MergePolicyRule {
                    id: "fallback-review".to_string(),
                    selector: crate::MergePathSelector {
                        glob: "**".to_string(),
                        kinds: Vec::new(),
                    },
                    resolution: MergeResolution::RequireReview,
                },
            ],
        };
        let options = GitSyncOptions {
            merge_policy: policy,
            ..GitSyncOptions::default()
        };
        sync_git_once(&engine, &writer, &options).expect("bootstrap sync");
        let reader = clone_reader(&temporary, &remote, &writer);
        sync_git_once(&engine, &reader, &options).expect("reader baseline");

        fs::write(writer.join(relative), "{\"base\":true,\"writer\":1}\n")
            .expect("writer plugin state");
        fs::write(reader.join(relative), "{\"base\":true,\"reader\":2}\n")
            .expect("reader plugin state");
        sync_git_once(&engine, &writer, &options).expect("writer push");
        let report = sync_git_once(&engine, &reader, &options).expect("selected state merge");

        assert_eq!(report.outcome, GitSyncOutcome::Merged);
        assert_eq!(report.automatic_resolutions[0].path, relative);
        assert_eq!(
            report.automatic_resolutions[0].kind,
            MergeFileKind::ObsidianState
        );
        assert_eq!(
            report.automatic_resolutions[0].rule_id,
            "selected-plugin-state"
        );
    }

    #[test]
    fn device_review_ceiling_preserves_otherwise_resolvable_conflicts() {
        let (temporary, remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();
        let policy = MergePolicy::default();
        fs::write(writer.join("data.json"), "{\"base\":true}\n").expect("base JSON");
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("bootstrap sync");
        let reader = clone_reader(&temporary, &remote, &writer);
        sync_git_once(&engine, &reader, &GitSyncOptions::default()).expect("reader baseline");
        fs::write(writer.join("data.json"), "{\"base\":true,\"writer\":1}\n").expect("writer JSON");
        fs::write(reader.join("data.json"), "{\"base\":true,\"reader\":2}\n").expect("reader JSON");
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("writer push");

        let report = sync_git_once(
            &engine,
            &reader,
            &GitSyncOptions {
                merge_automation: MergeAutomation::RequireReview,
                ..GitSyncOptions::default()
            },
        )
        .expect("review conflict");

        assert_eq!(report.outcome, GitSyncOutcome::Conflicted);
        assert!(report.automatic_resolutions.is_empty());
        assert_eq!(
            report.conflict.expect("conflict").policy_hash,
            policy.policy_hash().expect("policy hash")
        );
    }
}
