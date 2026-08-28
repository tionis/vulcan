use crate::{
    GitCaptureRequest, GitEngine, GitEngineError, GitInstallation, GitOid, GitPushResult,
    GitRefName, GitRemote, GitRepository, GitSafetyState, SyncAction, SyncBackend,
    SyncCapabilities, SyncCapability, SyncConflict, SyncContext, SyncError, SyncErrorCategory,
    SyncOperation, SyncOperationMode, SyncOutcome, SyncPlan, SyncProgress, SyncReport,
    SyncResolutionState, SyncState, SyncStatus, SYNC_CONTRACT_VERSION,
};
use fs2::FileExt;
use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const SYNC_PROTOCOL_VERSION: u32 = 1;
const DEFAULT_LIVE_REF: &str = "refs/heads/__vulcan-sync/live";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSyncOptions {
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub max_retries: usize,
    pub dry_run: bool,
}

impl Default for GitSyncOptions {
    fn default() -> Self {
        Self {
            remote: GitRemote::parse("origin").expect("the default Git remote is valid"),
            live_ref: GitRefName::parse(DEFAULT_LIVE_REF).expect("the default live ref is valid"),
            max_retries: 4,
            dry_run: false,
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
pub struct GitSyncConflict {
    pub remote: GitOid,
    pub local: GitOid,
    pub merge_tree: Option<GitOid>,
    pub diagnostics: String,
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
    pub remote_before: Option<GitOid>,
    pub local_before: Option<GitOid>,
    pub local_snapshot: Option<GitOid>,
    pub accepted: Option<GitOid>,
    pub actions: Vec<GitSyncAction>,
    pub retries: usize,
    pub conflict: Option<GitSyncConflict>,
}

impl GitSyncReport {
    fn initial(
        options: &GitSyncOptions,
        installation: GitInstallation,
        repository: GitRepository,
        refs: GitSyncRefs,
        safety: GitSafetyState,
        remote_before: Option<GitOid>,
        local_before: Option<GitOid>,
    ) -> Self {
        Self {
            dry_run: options.dry_run,
            outcome: GitSyncOutcome::Planned,
            installation,
            repository,
            remote: options.remote.clone(),
            refs,
            safety,
            remote_before,
            local_before,
            local_snapshot: None,
            accepted: None,
            actions: Vec::new(),
            retries: 0,
            conflict: None,
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
        GitSyncPhase::Preparing | GitSyncPhase::Capturing => SyncState::CapturePending,
        GitSyncPhase::Captured | GitSyncPhase::Pushing => SyncState::CapturedUnpushed,
        GitSyncPhase::Fetching => SyncState::Fetching,
        GitSyncPhase::Merging => SyncState::Merging,
        GitSyncPhase::Applying | GitSyncPhase::Verifying => SyncState::Applying,
        GitSyncPhase::Paused => SyncState::Paused,
        GitSyncPhase::Conflicted => SyncState::Conflicted,
        GitSyncPhase::Completed => SyncState::Clean,
    }
}

fn git_report_to_backend_report(report: GitSyncReport) -> SyncReport {
    let conflict = report.conflict.as_ref().map(|conflict| {
        let id_source = format!(
            "1\0{}\0{}\0{}",
            conflict.local, conflict.remote, conflict.diagnostics
        );
        SyncConflict {
            id: blake3::hash(id_source.as_bytes()).to_hex()[..32].to_string(),
            paths: Vec::new(),
            base_revision: None,
            local_revision: conflict.local.to_string(),
            remote_revision: conflict.remote.to_string(),
            policy_version: 1,
            resolution: SyncResolutionState::Unresolved,
            preserved: false,
            detail: Some(conflict.diagnostics.clone()),
        }
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
        detail: None,
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
    let installation = engine.installation()?;
    let repository = engine.discover_repository(vault_path)?;
    let refs = GitSyncRefs::for_options(options)?;
    let safety = engine.safety_state(&repository)?;
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
        remote_before,
        local_before,
    );
    emit_progress(observer, GitSyncPhase::Preparing, 0, &report, None)?;
    if options.dry_run {
        emit_progress(observer, GitSyncPhase::Completed, 0, &report, None)?;
        return Ok(report);
    }
    if report.safety.staged_changes || report.safety.operation.is_some() {
        report.outcome = GitSyncOutcome::Paused;
        emit_progress(observer, GitSyncPhase::Paused, 0, &report, None)?;
        return Ok(report);
    }

    let _lock = RepositoryLock::acquire(&report.repository)?;
    for attempt in 0..options.max_retries.max(1) {
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
    }

    Err(GitSyncError::RetryLimit {
        attempts: options.max_retries.max(1),
    })
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
            base,
            target_ref: report.refs.local.clone(),
            message: snapshot_message(&report.refs),
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
            message: snapshot_message(&report.refs),
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
        control.check()?;
        control.emit(GitSyncPhase::Applying, report, None)?;
        engine.apply_tree(&report.repository, &verification.commit, &accepted)?;
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
    if capture.commit == remote {
        return Ok(Some((remote, GitSyncOutcome::UpToDate, false)));
    }
    if engine.is_ancestor(&report.repository, &remote, &capture.commit)? {
        control.check()?;
        control.emit(GitSyncPhase::Pushing, report, None)?;
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
    merge_divergence(engine, options, report, capture, remote, control)
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
    let merge = engine.merge_commits(&report.repository, &remote, &capture.commit)?;
    if !merge.clean {
        report.outcome = GitSyncOutcome::Conflicted;
        report.conflict = Some(GitSyncConflict {
            remote,
            local: capture.commit.clone(),
            merge_tree: merge.tree,
            diagnostics: merge.diagnostics,
        });
        control.emit(GitSyncPhase::Conflicted, report, None)?;
        return Ok(None);
    }
    let tree = merge.tree.ok_or_else(|| {
        GitSyncError::Git(GitEngineError::InvalidOutput {
            operation: "merge live sync commits",
            detail: "the clean merge report omitted its tree".to_string(),
        })
    })?;
    let merged = engine.create_commit(
        &report.repository,
        &tree,
        &[remote.clone(), capture.commit.clone()],
        &merge_message(&report.refs),
    )?;
    engine.update_ref(&report.repository, &report.refs.pending, &merged)?;
    control.check()?;
    control.emit(GitSyncPhase::Pushing, report, None)?;
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

fn snapshot_message(refs: &GitSyncRefs) -> String {
    format!(
        "vulcan live snapshot\n\nVulcan-Sync-Version: {SYNC_PROTOCOL_VERSION}\nVulcan-Sync-Profile: {}\n",
        refs.local
            .as_str()
            .split('/')
            .nth(3)
            .unwrap_or("unknown")
    )
}

fn merge_message(refs: &GitSyncRefs) -> String {
    format!(
        "vulcan live merge\n\nVulcan-Sync-Version: {SYNC_PROTOCOL_VERSION}\nVulcan-Sync-Profile: {}\n",
        refs.local
            .as_str()
            .split('/')
            .nth(3)
            .unwrap_or("unknown")
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

    fn run_git(path: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .current_dir(path)
            .args(arguments)
            .status()
            .expect("Git should launch");
        assert!(status.success(), "Git failed: {arguments:?}");
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
        assert!(!plan
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
    fn staged_changes_pause_before_any_snapshot_or_push() {
        let (_temporary, _remote, writer) = setup_remote_and_writer();
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
        assert_eq!(report.local_snapshot, None);
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
        assert!(report.conflict.is_some());
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
    }
}
