use crate::{
    local_sync_ref, remote_device_ref, sync_profile_key, BranchAdvanceOutcome, BranchPullConfig,
    FastForwardOutcome, GitBranchUpstream, GitCaptureRequest, GitEngine, GitEngineError,
    GitInstallation, GitOid, GitPlatformPreflight, GitPlatformProfile, GitPushResult, GitRefName,
    GitReference, GitRemote, GitRepository, GitRepositoryRequirements, GitSafetyState,
    MergeBranchOutcome, MergeFileKind, MergeResolution, PullFastForward, PullRebase, RebaseOutcome,
    SyncAction, SyncBackend, SyncCapabilities, SyncCapability, SyncContext, SyncError,
    SyncErrorCategory, SyncOperation, SyncOperationMode, SyncOutcome, SyncPlan, SyncProgress,
    SyncReport, SyncState, SyncStatus, DEFAULT_REMOTE_LIVE_REF, GIT_PLATFORM_PREFLIGHT_VERSION,
    REMOTE_REGISTRATION_BRANCH_ROOT, SYNC_CONTRACT_VERSION, VULCAN_REF_NAMESPACE_VERSION,
};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::{self};
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tempfile::NamedTempFile;

const SYNC_PROTOCOL_VERSION: u32 = 2;
const PLATFORM_PREFLIGHT_CACHE_VERSION: u32 = 1;
const MAX_PLATFORM_PREFLIGHT_CACHE_BYTES: u64 = 256 * 1024;
/// `merge-tree --write-tree` (used for conflict-free divergence merging)
/// requires Git 2.38.
const MINIMUM_GIT_VERSION: (u32, u32, u32) = (2, 38, 0);

#[derive(Debug, Serialize, Deserialize)]
struct PlatformPreflightCache {
    version: u32,
    policy_key: String,
    preflight: GitPlatformPreflight,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct GitSyncDeviceId(String);

/// How a device ID was derived. Every ID is key-derived (`vdev1_`); the
/// ULID-shaped IDs of earlier versions are no longer accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitSyncDeviceIdKind {
    SshKeyV1,
}

impl GitSyncDeviceId {
    pub fn parse(value: impl Into<String>) -> Result<Self, GitSyncError> {
        let value = value.into();
        let key_digest = value.strip_prefix("vdev1_");
        if key_digest.is_some_and(is_canonical_device_key_digest) {
            return Ok(Self(value));
        }
        Err(GitSyncError::Git(GitEngineError::UnsupportedRepository {
            detail: "sync device identity must be a canonical vdev1_ key ID".to_string(),
        }))
    }

    /// The placeholder actor of a run without a device identity, such as a
    /// dry run before `vulcan device init`. Key-shaped, but `0` is outside
    /// the digest alphabet, so it never parses as a real device.
    #[must_use]
    pub fn anonymous() -> Self {
        Self(format!("vdev1_{}", "0".repeat(52)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn kind(&self) -> GitSyncDeviceIdKind {
        GitSyncDeviceIdKind::SshKeyV1
    }
}

impl GitSyncDeviceIdKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SshKeyV1 => "ssh_key_v1",
        }
    }
}

fn is_canonical_device_key_digest(value: &str) -> bool {
    value.len() == 52
        && value
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'2'..=b'7'))
        // 256 bits encode into 52 Base32 characters; the final character has
        // one data bit and four zero padding bits, so its index is 0 or 16.
        && value
            .as_bytes()
            .last()
            .is_some_and(|byte| device_base32_index(*byte).is_some_and(|index| index % 16 == 0))
}

fn device_base32_index(byte: u8) -> Option<u8> {
    match byte {
        b'a'..=b'z' => Some(byte - b'a'),
        b'2'..=b'7' => Some(byte - b'2' + 26),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSyncOptions {
    pub remote: GitRemote,
    pub max_retries: usize,
    pub command_timeout: Duration,
    pub dry_run: bool,
    pub device_id: GitSyncDeviceId,
    pub platform: GitPlatformProfile,
    /// Whether cycles publish local commits to the branch upstream.
    pub branch_push: bool,
    /// How cycles reconcile the checked-out branch with its upstream.
    pub branch_pull: BranchPullPolicy,
    /// Whether cycles push the device safety head; without it the snapshot
    /// of uncommitted edits stays in this repository.
    pub publish_device_backup: bool,
}

/// How the branch lane reconciles the checked-out branch with its upstream.
///
/// `Git` follows the device's Git configuration exactly as `git pull`
/// reads it. The other values fix the strategy for the repository, so a
/// vault's shared configuration can require it on every device regardless
/// of local or global Git settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum BranchPullPolicy {
    /// Follow `branch.<name>.rebase`, `pull.rebase`, and `pull.ff`.
    #[default]
    Git,
    /// Fast-forward only; a diverged branch pauses the lane.
    FfOnly,
    /// Fast-forward when possible, merge otherwise.
    Merge,
    /// Rebase local commits onto the upstream.
    Rebase,
    /// Rebase local commits onto the upstream only when that rewrites
    /// commits nothing else builds on: none is reachable from another
    /// branch, tag, remote-tracking ref, or worktree, and none is a merge.
    /// Otherwise, and on a conflict (which is aborted), the lane pauses.
    GuardedRebase,
}

impl BranchPullPolicy {
    #[must_use]
    pub const fn is_git(&self) -> bool {
        matches!(self, Self::Git)
    }

    /// The strategy this policy fixes, or `None` when Git configuration
    /// decides.
    #[must_use]
    pub const fn fixed(self) -> Option<BranchPullConfig> {
        match self {
            Self::Git => None,
            Self::FfOnly => Some(BranchPullConfig {
                fast_forward: PullFastForward::Only,
                rebase: PullRebase::Never,
            }),
            Self::Merge => Some(BranchPullConfig {
                fast_forward: PullFastForward::Always,
                rebase: PullRebase::Never,
            }),
            Self::Rebase | Self::GuardedRebase => Some(BranchPullConfig {
                fast_forward: PullFastForward::Always,
                rebase: PullRebase::Yes,
            }),
        }
    }
}

/// Resolves the branch pull strategy: a fixed vault policy wins, so a
/// malformed device Git setting cannot fail a lane the vault already decided.
fn resolve_branch_pull_config(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    branch: &GitRefName,
    policy: BranchPullPolicy,
) -> Result<BranchPullConfig, GitEngineError> {
    match policy.fixed() {
        Some(config) => Ok(config),
        None => engine.branch_pull_config(repository, branch),
    }
}

impl Default for GitSyncOptions {
    fn default() -> Self {
        Self {
            remote: GitRemote::parse("origin").expect("the default Git remote is valid"),
            max_retries: 4,
            command_timeout: Duration::from_secs(300),
            dry_run: false,
            device_id: GitSyncDeviceId::anonymous(),
            platform: GitPlatformProfile::native(),
            branch_push: true,
            branch_pull: BranchPullPolicy::Git,
            publish_device_backup: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitSyncOutcome {
    Planned,
    Paused,
    UpToDate,
    Pushed,
    Pulled,
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
    BackingUp,
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
    pub namespace_version: u32,
    /// Local snapshot of the worktree, the base of the device backup.
    pub local: GitRefName,
    /// Remote per-device backup head.
    pub device: GitRefName,
    /// Local observation of the last successfully published device head.
    pub device_tracking: GitRefName,
}

/// The ref profile of a sync remote. Profiles were keyed by remote and live
/// ref; every profile now uses the former default live ref so existing local
/// snapshots and device backups keep their names.
#[must_use]
pub fn sync_profile(remote: &GitRemote) -> String {
    sync_profile_key(
        remote,
        &GitRefName::parse(DEFAULT_REMOTE_LIVE_REF).expect("the default live ref is valid"),
    )
}

impl GitSyncRefs {
    pub fn for_options(options: &GitSyncOptions) -> Result<Self, GitSyncError> {
        let profile = sync_profile(&options.remote);
        Ok(Self {
            namespace_version: VULCAN_REF_NAMESPACE_VERSION,
            local: local_sync_ref(&profile, "local")?,
            device: remote_device_ref(&profile, options.device_id.as_str())?,
            device_tracking: local_sync_ref(&profile, "device")?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitDeviceBackupOutcome {
    Current,
    Published,
    Bridged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitDeviceBackup {
    pub device_id: GitSyncDeviceId,
    pub reference: GitRefName,
    pub snapshot_revision: GitOid,
    pub published_revision: GitOid,
    pub outcome: GitDeviceBackupOutcome,
    /// The remote backup had moved past what this checkout last published,
    /// so the bridged history came from elsewhere: another checkout or
    /// installation under the same device ID, or this one re-cloned. A
    /// bridge without it only joins history this checkout rewrote itself,
    /// such as commits replaced by a rebase.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub foreign_history: bool,
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
    pub scope: GitConflictScope,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection: Option<GitConflictProjection>,
    pub merge_tree: Option<GitOid>,
    pub diagnostics: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitConflictScope {
    Paths,
    TreeValidation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitConflictProjection {
    pub tree: GitOid,
    pub published: bool,
    pub applied: bool,
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
    /// Conservative review hint only. This never changes the required
    /// resolution action or authorizes an automatic winner.
    #[serde(default)]
    pub formatting_candidate: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitSyncPauseReason {
    HeadMoved,
    OperationInProgress,
    /// Committed-history-only synchronization stopped because the branch
    /// lane paused; `detail` names the reason.
    BranchPaused,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitBranchSyncAction {
    UpToDate,
    FastForwarded,
    Merged,
    Rebased,
    /// Fast-forwarded through the index only to an upstream tip the
    /// synchronized worktree already contained through live history.
    Adopted,
    /// Nothing to do or nothing known: no upstream, detached HEAD, or a bare
    /// repository without a worktree. The detail names the reason.
    Skipped,
    /// The branch lane stopped while the file lane proceeded: diverged past
    /// `pull.ff=only`, interactive rebase requested, or a merge/rebase
    /// conflict left in place. The detail names the reason.
    Paused,
    /// Deferred to a later cycle, e.g. a dirty worktree the pull must not
    /// overwrite. The file lane still proceeded.
    Deferred,
    /// Branch fetch or strategy resolution failed. The file lane still
    /// proceeded; its own fetch surfaces transport errors.
    Failed,
    /// Dry-run plan without mutation.
    Planned,
}

/// The outcome of pulling the checked-out branch from its upstream inside one
/// finite synchronization cycle. This lane moves ordinary branch refs with
/// the user's own pull configuration; the hidden live refs move separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitBranchSync {
    pub branch: GitRefName,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<GitRemote>,
    /// The branch ref as known on the remote (e.g. `refs/heads/main`).
    /// Absent when no upstream could be resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<GitRefName>,
    /// The local remote-tracking ref leased by the push.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracking: Option<GitRefName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<GitOid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<GitOid>,
    pub action: GitBranchSyncAction,
    /// Why a `paused` lane stopped, so alerts can give the right fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<GitBranchPause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Whether the branch tip was published to the upstream in this cycle.
    pub pushed: bool,
    /// Push detail: rejection or transport/policy failure summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub push_detail: Option<String>,
}

/// Why the branch lane paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitBranchPause {
    /// Diverged under `pull.ff=only` or `sync.branch_pull = "ff-only"`.
    FastForwardOnly,
    /// `pull.rebase=interactive` asks for a person.
    InteractiveRebase,
    /// A guarded rebase would strand another ref built on the commits.
    SharedCommits,
    /// A guarded rebase would flatten local merge commits.
    MergeCommits,
    /// The rebase conflicted and was aborted.
    RebaseConflict,
    /// The merge conflicted and was aborted.
    MergeConflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitSyncReport {
    pub dry_run: bool,
    pub outcome: GitSyncOutcome,
    pub installation: GitInstallation,
    pub repository: GitRepository,
    pub remote: GitRemote,
    /// This installation's device ID, which names its backup ref.
    pub device_id: GitSyncDeviceId,
    pub refs: GitSyncRefs,
    pub safety: GitSafetyState,
    pub requirements: GitRepositoryRequirements,
    pub platform_policy: crate::GitPlatformPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_before: Option<GitOid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_ref_before: Option<GitRefName>,
    pub local_before: Option<GitOid>,
    pub local_snapshot: Option<GitOid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_backup: Option<GitDeviceBackup>,
    /// Remote device-registration refs seen in the same trip as the device
    /// backup observation. `None` means they were not observed; never serialized.
    #[serde(skip)]
    pub registration_tips: Option<Vec<GitReference>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_platform_preflight: Option<GitPlatformPreflight>,
    pub actions: Vec<GitSyncAction>,
    pub retries: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pause: Option<GitSyncPause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<GitBranchSync>,
}

impl GitSyncReport {
    fn initial(
        options: &GitSyncOptions,
        installation: GitInstallation,
        repository: GitRepository,
        refs: GitSyncRefs,
        repository_preflight: (GitSafetyState, GitRepositoryRequirements),
        head_before: (Option<GitOid>, Option<GitRefName>),
        local_before: Option<GitOid>,
    ) -> Self {
        Self {
            dry_run: options.dry_run,
            outcome: GitSyncOutcome::Planned,
            installation,
            repository,
            remote: options.remote.clone(),
            device_id: options.device_id.clone(),
            refs,
            safety: repository_preflight.0,
            requirements: repository_preflight.1,
            platform_policy: options.platform.policy(),
            head_before: head_before.0,
            head_ref_before: head_before.1,
            local_before,
            local_snapshot: None,
            device_backup: None,
            registration_tips: None,
            local_platform_preflight: None,
            actions: Vec::new(),
            retries: 0,
            pause: None,
            branch: None,
        }
    }
}

#[derive(Debug)]
pub enum GitSyncError {
    Git(GitEngineError),
    Locked,
    Cancelled,
    Observer(GitSyncObserverError),
    PlatformIncompatible(GitPlatformPreflight),
    RetryLimit {
        attempts: usize,
    },
    /// Remote live was written with a different path scope by a device whose
    /// branch is at least as new; this device must catch up first.
    ScopeMismatch {
        detail: String,
    },
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
            Self::PlatformIncompatible(preflight) => {
                let codes = preflight
                    .diagnostics
                    .iter()
                    .filter(|item| item.severity == crate::GitPlatformDiagnosticSeverity::Error)
                    .map(|item| item.code.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    formatter,
                    "Git tree {} is incompatible with target platform `{}`: {codes}",
                    preflight.revision,
                    preflight.policy.profile.as_str()
                )
            }
            Self::RetryLimit { attempts } => write!(
                formatter,
                "synchronization did not converge after {attempts} attempts; local snapshots remain preserved"
            ),
            Self::ScopeMismatch { detail } => formatter.write_str(detail),
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
            Self::Locked
            | Self::Cancelled
            | Self::PlatformIncompatible(_)
            | Self::RetryLimit { .. }
            | Self::ScopeMismatch { .. } => None,
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
        .map_err(|error| classify_git_sync_error(&error))
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
                accepted_revision: None,
            })
            .map_err(|error| GitSyncObserverError::new(error.to_string()))
    }
}

fn sync_state_from_phase(phase: GitSyncPhase) -> SyncState {
    match phase {
        GitSyncPhase::Preparing => SyncState::CapturePending,
        GitSyncPhase::Capturing => SyncState::Capturing,
        GitSyncPhase::Captured => SyncState::CapturedUnpushed,
        GitSyncPhase::BackingUp | GitSyncPhase::Pushing => SyncState::Pushing,
        GitSyncPhase::Fetching => SyncState::Fetching,
        GitSyncPhase::Fetched => SyncState::Fetched,
        GitSyncPhase::Merging => SyncState::Merging,
        GitSyncPhase::Applying | GitSyncPhase::Verifying => SyncState::Applying,
        GitSyncPhase::Paused => SyncState::Paused,
        GitSyncPhase::Conflicted => SyncState::Conflicted,
        GitSyncPhase::Completed => SyncState::Clean,
    }
}

fn git_report_to_backend_report(report: GitSyncReport) -> SyncReport {
    let state = match report.outcome {
        GitSyncOutcome::Paused => SyncState::Paused,
        GitSyncOutcome::Planned if report.safety.operation.is_some() => SyncState::Dirty,
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
        remote_revision: None,
        accepted_revision: None,
        unresolved_conflicts: 0,
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
            GitSyncPauseReason::BranchPaused => format!(
                "branch lane paused: {}",
                pause.detail.as_deref().unwrap_or("unknown reason")
            ),
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
            GitSyncOutcome::Pushed => SyncOutcome::Pushed,
            GitSyncOutcome::Pulled => SyncOutcome::Pulled,
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
        conflicts: Vec::new(),
    }
}

/// Converts a Git-engine failure into the stable backend-neutral error
/// contract used by supervisors and transports.
#[must_use]
pub fn classify_git_sync_error(error: &GitSyncError) -> SyncError {
    let (category, retryable) = match error {
        GitSyncError::Locked
        | GitSyncError::ScopeMismatch { .. }
        | GitSyncError::Git(
            GitEngineError::WorktreeChanged | GitEngineError::CommandTimedOut { .. },
        ) => (SyncErrorCategory::Busy, true),
        GitSyncError::Cancelled => (SyncErrorCategory::Cancelled, false),
        GitSyncError::Observer(_) => (SyncErrorCategory::Observer, false),
        GitSyncError::PlatformIncompatible(_) => (SyncErrorCategory::Unsupported, false),
        GitSyncError::RetryLimit { .. } => (SyncErrorCategory::Network, true),
        GitSyncError::Io(_) | GitSyncError::Git(GitEngineError::Io(_)) => {
            (SyncErrorCategory::Io, true)
        }
        GitSyncError::Git(
            GitEngineError::ExecutableUnavailable { .. } | GitEngineError::InvalidRemote(_),
        ) => (SyncErrorCategory::Configuration, false),
        GitSyncError::Git(GitEngineError::RemoteRefMissing { .. }) => {
            (SyncErrorCategory::Network, true)
        }
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
        GitSyncError::Git(
            GitEngineError::CommandFailed { .. } | GitEngineError::ApplyWouldOverwrite { .. },
        ) => (SyncErrorCategory::Repository, false),
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
    require_git_version(&installation)?;
    let repository = engine.discover_repository(vault_path)?;
    let refs = GitSyncRefs::for_options(options)?;
    let safety = engine.safety_state(&repository)?;
    let requirements = engine.repository_requirements(&repository, vault_path)?;
    let head_before = engine.head_commit(&repository)?;
    let head_ref_before = engine.head_reference(&repository)?;
    let local_before = engine.read_ref(&repository, &refs.local)?;
    let mut report = GitSyncReport::initial(
        options,
        installation,
        repository,
        refs,
        (safety, requirements),
        (head_before, head_ref_before),
        local_before,
    );
    emit_progress(observer, GitSyncPhase::Preparing, 0, &report, None)?;
    require_filter_drivers(&report.requirements)?;
    if options.dry_run {
        let upstream = resolve_branch_upstream(engine, &report)
            .transpose()?
            .flatten();
        let upstream_tip = preview_remote_refs(engine, options, &mut report, upstream.as_ref())?;
        if let Some(revision) = report.local_before.as_ref().or(report.head_before.as_ref()) {
            report.local_platform_preflight = Some(platform_preflight(
                engine,
                &report.repository,
                revision,
                options.platform,
                false,
            )?);
        }
        preview_branch_pull(
            engine,
            &mut report,
            upstream,
            upstream_tip,
            options.branch_pull,
        )?;
        emit_progress(observer, GitSyncPhase::Completed, 0, &report, None)?;
        return Ok(report);
    }
    let _lock =
        crate::RepositoryLock::acquire(&report.repository.git_dir).map_err(
            |error| match error {
                crate::RepositoryLockError::Locked => GitSyncError::Locked,
                crate::RepositoryLockError::Io(error) => GitSyncError::Io(error),
            },
        )?;
    sweep_stale_sync_locks(&report.repository);
    engine.persist_repository_requirements_cache(&report.repository)?;
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

fn require_git_version(installation: &GitInstallation) -> Result<(), GitSyncError> {
    let version = installation.version.clone();
    if (version.major, version.minor, version.patch) < MINIMUM_GIT_VERSION {
        return Err(GitSyncError::Git(GitEngineError::UnsupportedRepository {
            detail: format!(
                "Vulcan synchronization requires Git {}.{}.{} or newer (`merge-tree --write-tree`); found {}",
                MINIMUM_GIT_VERSION.0,
                MINIMUM_GIT_VERSION.1,
                MINIMUM_GIT_VERSION.2,
                version.raw
            ),
        }));
    }
    Ok(())
}

fn require_filter_drivers(requirements: &GitRepositoryRequirements) -> Result<(), GitSyncError> {
    let unavailable = requirements
        .required_filters
        .iter()
        .filter(|filter| !filter.ready())
        .map(|filter| filter.name.as_str())
        .collect::<Vec<_>>();
    if unavailable.is_empty() {
        Ok(())
    } else {
        Err(GitSyncError::Git(GitEngineError::UnsupportedRepository {
            detail: format!(
                "tracked files require unavailable Git clean/smudge filter drivers: {}; configure each driver for both capture and projection before synchronizing",
                unavailable.join(", ")
            ),
        }))
    }
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

/// A remote ref's tip as observed by a query; `None` means the ref is absent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedTip(Option<GitOid>);

/// Remote state observed by one `ls-remote` at the start of an attempt, so an
/// unchanged cycle needs a single remote round trip. Every later remote write
/// still uses an exact lease, so a slightly stale observation can only cause a
/// rejected push and a retry, never a lost update.
#[derive(Default)]
struct AttemptRemote {
    /// Whether the attempt queried the remote yet. Capture runs first so an
    /// unreachable remote never prevents the local safety snapshot.
    observed: bool,
    /// Exact device backup head, or `None` once it is no longer known.
    device: Option<ObservedTip>,
    /// Remote device-registration refs observed in the same query.
    registrations: Option<Vec<GitReference>>,
    /// Branch upstream resolution, consumed by the branch lane.
    upstream: Option<Result<Option<GitBranchUpstream>, GitEngineError>>,
    /// Exact upstream branch tip when the upstream lives on the sync remote.
    upstream_tip: Option<(GitRemote, GitRefName, ObservedTip)>,
}

fn resolve_branch_upstream(
    engine: &dyn GitEngine,
    report: &GitSyncReport,
) -> Option<Result<Option<GitBranchUpstream>, GitEngineError>> {
    let branch = report.head_ref_before.as_ref()?;
    report.repository.work_tree.as_ref()?;
    Some(engine.branch_upstream(&report.repository, branch))
}

fn sync_remote_upstream<'a>(
    options: &GitSyncOptions,
    upstream: Option<&'a Result<Option<GitBranchUpstream>, GitEngineError>>,
) -> Option<&'a GitBranchUpstream> {
    upstream
        .and_then(|resolved| resolved.as_ref().ok())
        .and_then(Option::as_ref)
        .filter(|upstream| upstream.remote == options.remote)
}

impl AttemptRemote {
    fn ensure_observed(
        &mut self,
        engine: &dyn GitEngine,
        options: &GitSyncOptions,
        report: &GitSyncReport,
    ) -> Result<(), GitSyncError> {
        if !self.observed {
            let upstream = self.upstream.take();
            *self = observe_attempt_remote(engine, options, report, upstream)?;
        }
        Ok(())
    }
}

fn observe_attempt_remote(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &GitSyncReport,
    upstream: Option<Result<Option<GitBranchUpstream>, GitEngineError>>,
) -> Result<AttemptRemote, GitSyncError> {
    let upstream = upstream.or_else(|| resolve_branch_upstream(engine, report));
    let upstream_ref = sync_remote_upstream(options, upstream.as_ref())
        .map(|upstream| (upstream.remote.clone(), upstream.merge_ref.clone()));
    let registrations = GitRefName::parse(REMOTE_REGISTRATION_BRANCH_ROOT)?;
    let mut references = vec![&report.refs.device];
    if let Some((_, upstream_ref)) = &upstream_ref {
        references.push(upstream_ref);
    }
    let (tips, registration_tips) = engine.remote_refs(
        &report.repository,
        &options.remote,
        &references,
        Some(&registrations),
    )?;
    let mut tips = tips.into_iter();
    let device = tips.next().flatten();
    let upstream_tip = upstream_ref
        .map(|(remote, reference)| (remote, reference, ObservedTip(tips.next().flatten())));
    Ok(AttemptRemote {
        observed: true,
        device: Some(ObservedTip(device)),
        registrations: registration_tips,
        upstream,
        upstream_tip,
    })
}

fn capture_local_worktree(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &GitSyncReport,
    target_before: Option<GitOid>,
    tracking: Option<&GitRefName>,
) -> Result<crate::GitCapture, GitSyncError> {
    let base = match target_before.as_ref() {
        Some(target) => Some(target.clone()),
        None => engine.head_commit(&report.repository)?,
    };
    let link = missing_branch_link(engine, report, tracking, base.as_ref())?;
    Ok(engine.capture_worktree(
        &report.repository,
        &GitCaptureRequest {
            base: base.clone(),
            target_ref: report.refs.local.clone(),
            target_before,
            message: snapshot_message(&report.refs, options, base.as_ref(), link.as_ref()),
            link,
        },
    )?)
}

/// Returns the published tip of the checked-out branch: the merge base of
/// `HEAD` and its upstream tracking ref, so unpushed commits are never
/// included. `None` for bare, detached, or upstream-less checkouts.
pub fn git_published_branch_tip(
    engine: &dyn GitEngine,
    repository: &GitRepository,
) -> Result<Option<GitOid>, GitSyncError> {
    if repository.work_tree.is_none() {
        return Ok(None);
    }
    let Some(branch) = engine.head_reference(repository)? else {
        return Ok(None);
    };
    // Unresolvable upstream configuration is the branch lane's failure to
    // report; it only means there is nothing published to link.
    let Ok(Some(upstream)) = engine.branch_upstream(repository, &branch) else {
        return Ok(None);
    };
    let head = engine.head_commit(repository)?;
    published_branch_tip(engine, repository, head.as_ref(), &upstream.tracking_ref)
}

fn published_branch_tip(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    head: Option<&GitOid>,
    tracking: &GitRefName,
) -> Result<Option<GitOid>, GitSyncError> {
    let (Some(head), Some(tracking)) = (head, engine.read_ref(repository, tracking)?) else {
        return Ok(None);
    };
    if *head == tracking {
        return Ok(Some(tracking));
    }
    Ok(engine.merge_base(repository, head, &tracking)?)
}

/// The published branch tip when `base` does not already contain it, so a
/// live snapshot must link it as an additional parent. `tracking` is the
/// checked-out branch's upstream tracking ref, if it has one.
fn missing_branch_link(
    engine: &dyn GitEngine,
    report: &GitSyncReport,
    tracking: Option<&GitRefName>,
    base: Option<&GitOid>,
) -> Result<Option<GitOid>, GitSyncError> {
    let (Some(base), Some(tracking)) = (base, tracking) else {
        return Ok(None);
    };
    if report.repository.work_tree.is_none() || report.head_ref_before.is_none() {
        return Ok(None);
    }
    let Some(tip) = published_branch_tip(
        engine,
        &report.repository,
        report.head_before.as_ref(),
        tracking,
    )?
    else {
        return Ok(None);
    };
    if &tip == base || engine.is_ancestor(&report.repository, &tip, base)? {
        return Ok(None);
    }
    Ok(Some(tip))
}

/// Pulls the checked-out branch from its upstream inside one attempt, before
/// the hidden live refs move. Branch failures never fail the file lane: the
/// outcome is recorded on the report and reconciliation proceeds. A pull that
/// moves HEAD refreshes the expected head tracked by the pause checks; the
/// repository lock guarantees nothing else moved it.
/// Builds an inert branch report: no fetch, no mutation, only facts.
fn inert_branch_lane(
    branch: GitRefName,
    before: Option<GitOid>,
    action: GitBranchSyncAction,
    detail: &str,
) -> GitBranchSync {
    GitBranchSync {
        branch,
        remote: None,
        upstream: None,
        tracking: None,
        before,
        after: None,
        action,
        pause: None,
        detail: Some(detail.to_string()),
        pushed: false,
        push_detail: None,
    }
}

/// Resolves the branch pull target, recording Skipped/Failed on the report.
/// Returns None when the lane is complete without fetching. The head ref was
/// read at transaction start and refreshed after our own pulls; the lock
/// guarantees nothing else moved it, so no re-read is needed.
fn resolve_pull_target(
    engine: &dyn GitEngine,
    report: &mut GitSyncReport,
    resolved: Option<Result<Option<GitBranchUpstream>, GitEngineError>>,
) -> Option<(GitRefName, GitBranchUpstream)> {
    let repository = &report.repository;
    let branch = report.head_ref_before.clone()?;
    if repository.work_tree.is_none() {
        report.branch = Some(inert_branch_lane(
            branch,
            None,
            GitBranchSyncAction::Skipped,
            "bare repository has no worktree",
        ));
        return None;
    }
    match resolved.unwrap_or_else(|| engine.branch_upstream(repository, &branch)) {
        Ok(Some(upstream)) => Some((branch, upstream)),
        Ok(None) => {
            report.branch = Some(inert_branch_lane(
                branch,
                None,
                GitBranchSyncAction::Skipped,
                "no upstream is configured for the branch",
            ));
            None
        }
        // Unresolvable branch configuration (unknown remote, malformed ref)
        // fails only the branch lane; the file lane proceeds.
        Err(error) => {
            let before = engine.head_commit(repository).ok().flatten();
            report.branch = Some(inert_branch_lane(
                branch,
                before,
                GitBranchSyncAction::Failed,
                &error.to_string(),
            ));
            None
        }
    }
}

/// Returns the upstream tip, fetching only when the attempt's observation does
/// not show the tracking ref already holding it. The inner result is the
/// branch lane's own outcome; only local ref reads fail the cycle.
fn fetch_upstream_tip(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    upstream: &GitBranchUpstream,
    remote: &mut AttemptRemote,
) -> Result<Result<GitOid, GitEngineError>, GitSyncError> {
    let observed_tip = remote
        .upstream_tip
        .take()
        .filter(|(remote, reference, _)| {
            remote == &upstream.remote && reference == &upstream.merge_ref
        })
        .map(|(_, _, tip)| tip);
    Ok(match observed_tip {
        // The tracking ref already holds the observed tip and its objects.
        Some(ObservedTip(Some(tip)))
            if engine
                .read_ref(repository, &upstream.tracking_ref)?
                .as_ref()
                == Some(&tip) =>
        {
            Ok(tip)
        }
        Some(ObservedTip(None)) => Err(GitEngineError::RemoteRefMissing {
            remote: upstream.remote.clone(),
            reference: upstream.merge_ref.clone(),
        }),
        _ => engine.fetch_ref(
            repository,
            &upstream.remote,
            &upstream.merge_ref,
            &upstream.tracking_ref,
        ),
    })
}

fn pull_branch_lane(
    engine: &dyn GitEngine,
    report: &mut GitSyncReport,
    remote: &mut AttemptRemote,
    capture: &GitOid,
    policy: BranchPullPolicy,
) -> Result<(), GitSyncError> {
    // Branch operations must not start while another Git operation owns the
    // worktree; the live pause check then reports it with guidance. Pure
    // filesystem check against the shared marker list, no subprocess.
    if crate::git::GIT_OPERATION_MARKERS.iter().any(|(marker, _)| {
        report.repository.git_dir.join(marker).exists()
            || report.repository.common_dir.join(marker).exists()
    }) {
        return Ok(());
    }
    let repository = &report.repository;
    if let Some(existing) = report.branch.as_ref() {
        if matches!(
            existing.action,
            GitBranchSyncAction::FastForwarded
                | GitBranchSyncAction::Merged
                | GitBranchSyncAction::Rebased
                | GitBranchSyncAction::Adopted
        ) && existing.after == engine.head_commit(repository)?
        {
            return Ok(());
        }
    }
    let Some((branch, upstream)) = resolve_pull_target(engine, report, remote.upstream.take())
    else {
        return Ok(());
    };
    let repository = &report.repository;
    let mut lane = GitBranchSync {
        branch: branch.clone(),
        remote: Some(upstream.remote.clone()),
        upstream: Some(upstream.merge_ref.clone()),
        tracking: Some(upstream.tracking_ref.clone()),
        before: engine.head_commit(repository)?,
        after: None,
        action: GitBranchSyncAction::UpToDate,
        pause: None,
        detail: None,
        pushed: false,
        push_detail: None,
    };
    let fetched = fetch_upstream_tip(engine, repository, &upstream, remote)?;
    let fetched = match fetched {
        Ok(tip) => tip,
        Err(error) => {
            // A deleted upstream branch is a skip, not a failure: there is
            // nothing to pull. Other fetch failures stay Failed.
            if matches!(error, GitEngineError::RemoteRefMissing { .. }) {
                lane.action = GitBranchSyncAction::Skipped;
                lane.detail = Some("upstream ref is absent on the remote".to_string());
            } else {
                lane.action = GitBranchSyncAction::Failed;
                lane.detail = Some(error.to_string());
            }
            report.branch = Some(lane);
            return Ok(());
        }
    };
    if lane.before.as_ref() == Some(&fetched) {
        report.branch = Some(lane);
        return Ok(());
    }
    if let Some(head) = lane.before.clone() {
        if engine.is_ancestor(repository, &fetched, &head)? {
            report.branch = Some(lane);
            return Ok(());
        }
        if try_adopt_upstream(engine, report, &branch, &head, &fetched, capture, &mut lane)? {
            report.branch = Some(lane);
            return Ok(());
        }
    }
    let repository = &report.repository;
    let config = match resolve_branch_pull_config(engine, repository, &branch, policy) {
        Ok(config) => config,
        // An unreadable pull strategy (e.g. a typo'd pull.ff value) fails
        // only the branch lane; the file lane proceeds.
        Err(error) => {
            report.branch = Some(GitBranchSync {
                branch: branch.clone(),
                remote: Some(upstream.remote.clone()),
                upstream: Some(upstream.merge_ref.clone()),
                tracking: Some(upstream.tracking_ref.clone()),
                before: engine.head_commit(repository).ok().flatten(),
                after: None,
                action: GitBranchSyncAction::Failed,
                pause: None,
                detail: Some(error.to_string()),
                pushed: false,
                push_detail: None,
            });
            return Ok(());
        }
    };
    if try_branch_fast_forward(engine, report, &branch, &fetched, config, &mut lane)? {
        report.branch = Some(lane);
        return Ok(());
    }
    pull_branch_with_strategy(engine, report, &fetched, config, policy, &mut lane)?;
    report.branch = Some(lane);
    Ok(())
}

/// Records a branch move on the lane and refreshes the expected head tracked
/// by the pause checks; the repository lock guarantees nothing else moved it.
fn record_branch_move(
    engine: &dyn GitEngine,
    report: &mut GitSyncReport,
    lane: &mut GitBranchSync,
    action: GitBranchSyncAction,
) -> Result<(), GitSyncError> {
    lane.action = action;
    lane.after = engine.head_commit(&report.repository)?;
    report.head_before.clone_from(&lane.after);
    report.head_ref_before = engine.head_reference(&report.repository)?;
    Ok(())
}

/// Adopts an upstream tip that fast-forwards `head` and that `contains` (the
/// captured snapshot or the applied accepted live tip) already contains. The
/// worktree already descends from the upstream content through live, so only
/// the branch and normal index move; an ordinary pull would instead rewrite
/// bytes that later live history changed. Returns true when the lane is
/// complete and the caller should record it.
fn try_adopt_upstream(
    engine: &dyn GitEngine,
    report: &mut GitSyncReport,
    branch: &GitRefName,
    head: &GitOid,
    upstream: &GitOid,
    contains: &GitOid,
    lane: &mut GitBranchSync,
) -> Result<bool, GitSyncError> {
    let repository = &report.repository;
    if !engine.is_ancestor(repository, head, upstream)?
        || !engine.is_ancestor(repository, upstream, contains)?
    {
        return Ok(false);
    }
    match engine.advance_checked_out_branch(repository, branch, head, upstream)? {
        BranchAdvanceOutcome::Advanced => {
            record_branch_move(engine, report, lane, GitBranchSyncAction::Adopted)?;
            lane.detail = None;
        }
        BranchAdvanceOutcome::BlockedStaged => {
            lane.action = GitBranchSyncAction::Deferred;
            lane.detail = Some(
                "staged changes conflict with the upstream branch; retrying next cycle".to_string(),
            );
        }
        BranchAdvanceOutcome::Stale => {
            lane.action = GitBranchSyncAction::Failed;
            lane.detail = Some("the branch moved while adopting its upstream".to_string());
        }
    }
    Ok(true)
}

/// Attempts the fast-forward step of a branch pull. Returns true when the
/// lane is complete and the caller should record it.
fn try_branch_fast_forward(
    engine: &dyn GitEngine,
    report: &mut GitSyncReport,
    branch: &GitRefName,
    fetched: &GitOid,
    config: BranchPullConfig,
    lane: &mut GitBranchSync,
) -> Result<bool, GitSyncError> {
    if config.fast_forward == PullFastForward::Never {
        return Ok(false);
    }
    let ancestor = match &lane.before {
        Some(head) => engine.is_ancestor(&report.repository, head, fetched)?,
        None => false,
    };
    if !ancestor && lane.before.is_some() {
        return Ok(false);
    }
    match engine.fast_forward_branch(&report.repository, branch, fetched)? {
        FastForwardOutcome::UpToDate => Ok(true),
        FastForwardOutcome::Advanced => {
            record_branch_move(engine, report, lane, GitBranchSyncAction::FastForwarded)?;
            Ok(true)
        }
        FastForwardOutcome::NotFastForwardable => Ok(false),
        FastForwardOutcome::BlockedDirty => {
            lane.action = GitBranchSyncAction::Deferred;
            lane.detail =
                Some("dirty worktree would be overwritten; retrying next cycle".to_string());
            Ok(true)
        }
    }
}

/// Backs out a merge or rebase the branch lane started and that conflicted.
///
/// An unattended checkout must not be left mid-operation: anything that
/// commits there next (an agent, an editor hook) would commit conflict
/// markers or finish the rebase by accident. Aborting restores the exact
/// pre-pull branch and worktree, which the device backup already holds.
/// Only if the abort itself fails does the conflict stay for the live pause
/// check to report.
fn abort_conflicted_branch_operation(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    operation: &str,
) -> String {
    match engine.abort_branch_operation(repository) {
        Ok(_) => format!(
            "branch diverged and an automatic {operation} conflicted; the {operation} was \
             aborted and the branch left as it was; integrate the upstream with ordinary Git"
        ),
        Err(error) => format!(
            "branch {operation} conflicted and could not be aborted ({error}); resolve with \
             ordinary Git"
        ),
    }
}

/// Applies the configured rebase-or-merge strategy after fast-forward was
/// impossible or disabled. Conflicts pause the lane; the live pause check
/// then pauses the attempt on the resulting operation state.
fn pull_branch_with_strategy(
    engine: &dyn GitEngine,
    report: &mut GitSyncReport,
    fetched: &GitOid,
    config: BranchPullConfig,
    policy: BranchPullPolicy,
    lane: &mut GitBranchSync,
) -> Result<(), GitSyncError> {
    match config.rebase {
        PullRebase::Interactive => {
            lane.action = GitBranchSyncAction::Paused;
            lane.pause = Some(GitBranchPause::InteractiveRebase);
            lane.detail = Some(
                "pull.rebase=interactive requires a manual rebase; branch lane paused".to_string(),
            );
        }
        PullRebase::Yes | PullRebase::Merges => {
            if policy == BranchPullPolicy::GuardedRebase {
                if let Some(hazard) =
                    engine.rebase_hazard(&report.repository, &lane.branch, fetched)?
                {
                    lane.action = GitBranchSyncAction::Paused;
                    lane.pause = Some(match hazard {
                        crate::RebaseHazard::SharedWith(_) => GitBranchPause::SharedCommits,
                        crate::RebaseHazard::MergeCommits => GitBranchPause::MergeCommits,
                    });
                    lane.detail = Some(match hazard {
                        crate::RebaseHazard::SharedWith(name) => format!(
                            "branch diverged and the guarded rebase did not run: local commits are also on {name}, which a rebase would leave behind; rebase and push yourself"
                        ),
                        crate::RebaseHazard::MergeCommits => "branch diverged and the guarded rebase did not run: the local commits include merges a rebase would flatten; merge or rebase and push yourself".to_string(),
                    });
                    return Ok(());
                }
            }
            let merges = config.rebase == PullRebase::Merges;
            match engine.rebase_branch(&report.repository, fetched, merges)? {
                RebaseOutcome::UpToDate => {}
                RebaseOutcome::Rebased => {
                    record_branch_move(engine, report, lane, GitBranchSyncAction::Rebased)?;
                }
                RebaseOutcome::Conflicted => {
                    lane.action = GitBranchSyncAction::Paused;
                    lane.pause = Some(GitBranchPause::RebaseConflict);
                    lane.detail = Some(abort_conflicted_branch_operation(
                        engine,
                        &report.repository,
                        "rebase",
                    ));
                }
                RebaseOutcome::BlockedDirty if policy == BranchPullPolicy::GuardedRebase => {
                    replay_around_dirty_files(engine, report, fetched, lane)?;
                }
                RebaseOutcome::BlockedDirty => {
                    lane.action = GitBranchSyncAction::Deferred;
                    lane.detail = Some(
                        "dirty worktree would be overwritten; retrying next cycle".to_string(),
                    );
                }
            }
        }
        PullRebase::Never => {
            if config.fast_forward == PullFastForward::Only {
                lane.action = GitBranchSyncAction::Paused;
                lane.pause = Some(GitBranchPause::FastForwardOnly);
                let setting = if policy == BranchPullPolicy::FfOnly {
                    "sync.branch_pull = \"ff-only\""
                } else {
                    "pull.ff=only"
                };
                lane.detail = Some(format!(
                    "branch diverged and {setting} refuses non-fast-forward; rebase the local \
                     commits onto the upstream and push to continue"
                ));
            } else {
                let no_ff = config.fast_forward == PullFastForward::Never;
                match engine.merge_branch(&report.repository, fetched, no_ff)? {
                    MergeBranchOutcome::UpToDate => {}
                    MergeBranchOutcome::Merged => {
                        record_branch_move(engine, report, lane, GitBranchSyncAction::Merged)?;
                    }
                    MergeBranchOutcome::Conflicted => {
                        lane.action = GitBranchSyncAction::Paused;
                        lane.pause = Some(GitBranchPause::MergeConflict);
                        lane.detail = Some(abort_conflicted_branch_operation(
                            engine,
                            &report.repository,
                            "merge",
                        ));
                    }
                    MergeBranchOutcome::BlockedDirty => {
                        lane.action = GitBranchSyncAction::Deferred;
                        lane.detail = Some(
                            "dirty worktree would be overwritten; retrying next cycle".to_string(),
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

/// `git rebase` refuses any uncommitted change. Replaying in a scratch
/// worktree only refuses changes the rebase would actually overwrite, so an
/// agent's unrelated edits no longer hold a guarded rebase back.
fn replay_around_dirty_files(
    engine: &dyn GitEngine,
    report: &mut GitSyncReport,
    fetched: &GitOid,
    lane: &mut GitBranchSync,
) -> Result<(), GitSyncError> {
    match engine.replay_branch(
        &report.repository,
        &lane.branch,
        fetched,
        &mut ConflictsDecline,
    )? {
        crate::BranchReplayOutcome::Replayed { .. } => {
            record_branch_move(engine, report, lane, GitBranchSyncAction::Rebased)?;
        }
        crate::BranchReplayOutcome::Declined { reason } if reason.contains(CONFLICT_DECLINE) => {
            lane.action = GitBranchSyncAction::Paused;
            lane.pause = Some(GitBranchPause::RebaseConflict);
            lane.detail = Some(
                "branch diverged and rebasing the local commits conflicted; nothing was changed"
                    .to_string(),
            );
        }
        crate::BranchReplayOutcome::Declined { reason } => {
            lane.action = GitBranchSyncAction::Deferred;
            lane.detail = Some(format!(
                "uncommitted edits would be overwritten by the rebase ({reason}); retrying next cycle"
            ));
        }
    }
    Ok(())
}

const CONFLICT_DECLINE: &str = "conflicts need a person";

/// A replay resolver for unattended sync: any conflict declines, leaving the
/// branch as it was.
struct ConflictsDecline;

impl crate::BranchReplayResolver for ConflictsDecline {
    fn resolve(
        &mut self,
        _subject: &str,
        _conflicts: &[crate::BranchReplayConflict],
    ) -> Result<Vec<(String, String)>, String> {
        Err(CONFLICT_DECLINE.to_string())
    }
}

/// Previews the branch lane without mutation for dry runs: resolves the
/// upstream, compares tips by object id, and reports the configured strategy.
/// Queries the live ref and, when it lives on the same remote, the branch
/// upstream in one trip. Returns the upstream tip when it was observed.
fn preview_remote_refs(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    upstream: Option<&GitBranchUpstream>,
) -> Result<Option<ObservedTip>, GitSyncError> {
    let upstream_ref = upstream
        .filter(|upstream| upstream.remote == options.remote)
        .map(|upstream| &upstream.merge_ref);
    let Some(upstream_ref) = upstream_ref else {
        return Ok(None);
    };
    let (tips, _) =
        engine.remote_refs(&report.repository, &options.remote, &[upstream_ref], None)?;
    Ok(Some(ObservedTip(tips.into_iter().next().flatten())))
}

fn preview_branch_pull(
    engine: &dyn GitEngine,
    report: &mut GitSyncReport,
    upstream: Option<GitBranchUpstream>,
    observed_tip: Option<ObservedTip>,
    policy: BranchPullPolicy,
) -> Result<(), GitSyncError> {
    let repository = &report.repository;
    let Some(branch) = report.head_ref_before.clone() else {
        return Ok(());
    };
    let Some(upstream) = upstream else {
        return Ok(());
    };
    let before = engine.head_commit(repository)?;
    let tip = match observed_tip {
        Some(ObservedTip(tip)) => tip,
        None => engine.remote_ref(repository, &upstream.remote, &upstream.merge_ref)?,
    };
    let (action, detail) = match (&before, &tip) {
        (Some(head), Some(tip)) if head == tip => (GitBranchSyncAction::UpToDate, None),
        (_, None) => (
            GitBranchSyncAction::Planned,
            Some(format!(
                "would publish {} to {}",
                branch.as_str(),
                upstream.remote.as_str(),
            )),
        ),
        _ => {
            let config = match resolve_branch_pull_config(engine, repository, &branch, policy) {
                Ok(config) => config,
                Err(error) => {
                    report.branch = Some(GitBranchSync {
                        branch: branch.clone(),
                        remote: Some(upstream.remote.clone()),
                        upstream: Some(upstream.merge_ref.clone()),
                        tracking: Some(upstream.tracking_ref.clone()),
                        before: before.clone(),
                        after: None,
                        action: GitBranchSyncAction::Failed,
                        pause: None,
                        detail: Some(error.to_string()),
                        pushed: false,
                        push_detail: None,
                    });
                    return Ok(());
                }
            };
            let strategy = match (config.fast_forward, config.rebase) {
                (_, PullRebase::Interactive) => "manual rebase (pull.rebase=interactive)",
                (_, PullRebase::Yes) if policy == BranchPullPolicy::GuardedRebase => {
                    "guarded rebase"
                }
                (_, PullRebase::Yes) => "rebase",
                (_, PullRebase::Merges) => "rebase with merges",
                (PullFastForward::Only, PullRebase::Never) => "fast-forward only",
                (PullFastForward::Never, PullRebase::Never) => "merge commit",
                (PullFastForward::Always, PullRebase::Never) => "fast-forward or merge",
            };
            (
                GitBranchSyncAction::Planned,
                Some(format!(
                    "would reconcile {} with {} via {strategy}; publishes local commits when ahead",
                    branch.as_str(),
                    upstream.merge_ref.as_str(),
                )),
            )
        }
    };
    report.branch = Some(GitBranchSync {
        branch,
        remote: Some(upstream.remote.clone()),
        upstream: Some(upstream.merge_ref.clone()),
        tracking: Some(upstream.tracking_ref.clone()),
        before,
        after: None,
        action,
        pause: None,
        detail,
        pushed: false,
        push_detail: None,
    });
    Ok(())
}

/// Pushes the checked-out branch tip to its upstream immediately after its
/// pull lane, leasing the tracking ref observed during this attempt. This is
/// deliberately independent of later file-lane reconciliation: a hidden-live
/// conflict must not strand ordinary commits. Never force-pushes; a remote
/// that moved first reports Rejected for the next cycle's pull to incorporate.
fn push_branch_lane(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
) -> Result<(), GitSyncError> {
    let healthy = report.branch.as_ref().is_some_and(|lane| {
        matches!(
            lane.action,
            GitBranchSyncAction::UpToDate
                | GitBranchSyncAction::FastForwarded
                | GitBranchSyncAction::Merged
                | GitBranchSyncAction::Rebased
                | GitBranchSyncAction::Adopted
        )
    });
    if !healthy || !options.branch_push {
        return Ok(());
    }
    let lane = report.branch.as_mut().expect("healthy branch lane");
    let (Some(remote), Some(merge_ref), Some(tracking)) = (
        lane.remote.clone(),
        lane.upstream.clone(),
        lane.tracking.clone(),
    ) else {
        return Ok(());
    };
    // The lane refreshed the expected head after its own moves and the lock
    // rules out external moves; no re-read needed.
    let head = lane.after.clone().or_else(|| report.head_before.clone());
    let Some(head) = head else {
        return Ok(());
    };
    let current = engine.read_ref(&report.repository, &tracking)?;
    if current.as_ref() == Some(&head) {
        return Ok(());
    }
    match engine.push_ref(
        &report.repository,
        &remote,
        &head,
        &merge_ref,
        current.as_ref(),
    ) {
        Ok(GitPushResult::Updated) => {
            lane.pushed = true;
        }
        Ok(GitPushResult::Rejected) => {
            lane.push_detail =
                Some("remote advanced first; will pull then push next cycle".to_string());
        }
        Err(error) => {
            lane.push_detail = Some(format!("failed to push {}: {error}", lane.branch.as_str()));
        }
    }
    Ok(())
}

/// One committed-history-only attempt: capture the worktree for this device's
/// safety head, then pull and push the checked-out branch. Live is never
/// queried, fetched, pushed, or applied.
fn run_attempt(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    control: &mut AttemptControl<'_>,
) -> Result<AttemptResult, GitSyncError> {
    control.check()?;
    let local = engine.read_ref(&report.repository, &report.refs.local)?;
    let mut remote = AttemptRemote::default();
    let Some(capture) = capture_and_publish_device_backup(
        engine,
        options,
        report,
        control,
        &mut remote,
        local.clone(),
    )?
    else {
        return Ok(AttemptResult::Retry);
    };
    if pull_branch_and_refresh_device_backup(
        engine,
        options,
        report,
        control,
        &mut remote,
        capture,
        local,
    )?
    .is_none()
    {
        return Ok(AttemptResult::Retry);
    }
    report.registration_tips = remote.registrations.take();
    if let Some(pause) = sync_pause(engine, report)? {
        report.pause = Some(pause);
        report.outcome = GitSyncOutcome::Paused;
        control.emit(GitSyncPhase::Paused, report, None)?;
        return Ok(AttemptResult::Finished);
    }
    let lane = report.branch.as_ref();
    match lane.map(|lane| lane.action) {
        None => {
            return Err(branch_only_unsupported(
                "HEAD is detached or the repository has no worktree",
            ));
        }
        Some(GitBranchSyncAction::Skipped) => {
            let detail = lane
                .and_then(|lane| lane.detail.as_deref())
                .unwrap_or("the branch lane was skipped");
            return Err(branch_only_unsupported(detail));
        }
        // Without a file lane the branch lane is the whole cycle.
        Some(GitBranchSyncAction::Failed) => {
            return Err(GitSyncError::Git(GitEngineError::CommandFailed {
                operation: "fetch or pull the checked-out branch",
                exit_code: None,
                stderr: lane
                    .and_then(|lane| lane.detail.clone())
                    .unwrap_or_default(),
            }));
        }
        Some(GitBranchSyncAction::Paused) => {
            report.pause = Some(GitSyncPause {
                reason: GitSyncPauseReason::BranchPaused,
                expected_head: report.head_before.clone(),
                actual_head: report.head_before.clone(),
                expected_head_ref: report.head_ref_before.clone(),
                actual_head_ref: report.head_ref_before.clone(),
                operation: None,
                detail: lane.and_then(|lane| lane.detail.clone()),
            });
            report.outcome = GitSyncOutcome::Paused;
            control.emit(GitSyncPhase::Paused, report, None)?;
            return Ok(AttemptResult::Finished);
        }
        Some(_) => {}
    }
    let lane = lane.expect("branch lane");
    report.outcome = if matches!(
        lane.action,
        GitBranchSyncAction::FastForwarded
            | GitBranchSyncAction::Merged
            | GitBranchSyncAction::Rebased
            | GitBranchSyncAction::Adopted
    ) {
        GitSyncOutcome::Pulled
    } else if lane.pushed {
        GitSyncOutcome::Pushed
    } else {
        GitSyncOutcome::UpToDate
    };
    control.emit(GitSyncPhase::Completed, report, None)?;
    Ok(AttemptResult::Finished)
}

fn branch_only_unsupported(detail: &str) -> GitSyncError {
    GitSyncError::Git(GitEngineError::UnsupportedRepository {
        detail: format!(
            "Git sync needs a checked-out branch whose upstream exists on the remote: {detail}"
        ),
    })
}

fn capture_and_publish_device_backup(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    control: &mut AttemptControl<'_>,
    remote: &mut AttemptRemote,
    base: Option<GitOid>,
) -> Result<Option<crate::GitCapture>, GitSyncError> {
    control.emit(GitSyncPhase::Capturing, report, None)?;
    if remote.upstream.is_none() {
        remote.upstream = resolve_branch_upstream(engine, report);
    }
    let tracking = remote
        .upstream
        .as_ref()
        .and_then(|resolved| resolved.as_ref().ok())
        .and_then(Option::as_ref)
        .map(|upstream| upstream.tracking_ref.clone());
    let capture = capture_local_worktree(engine, options, report, base, tracking.as_ref())?;
    report.local_snapshot = Some(capture.commit.clone());
    if capture.created {
        report.actions.push(GitSyncAction::SnapshotCreated);
    }
    control.emit(GitSyncPhase::Captured, report, Some(capture.tree.clone()))?;
    control.check()?;
    if !options.publish_device_backup {
        // Uncommitted edits stay on this device; the local snapshot still
        // keeps them recoverable while the branch lane moves the checkout.
        return Ok(Some(capture));
    }
    control.emit(GitSyncPhase::BackingUp, report, Some(capture.tree.clone()))?;
    remote.ensure_observed(engine, options, report)?;
    if publish_device_backup(engine, options, report, &capture, &mut remote.device)? {
        Ok(Some(capture))
    } else {
        Ok(None)
    }
}

fn pull_branch_and_refresh_device_backup(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    control: &mut AttemptControl<'_>,
    remote: &mut AttemptRemote,
    capture: crate::GitCapture,
    base: Option<GitOid>,
) -> Result<Option<crate::GitCapture>, GitSyncError> {
    control.check()?;
    control.emit(GitSyncPhase::Fetching, report, Some(capture.tree.clone()))?;
    pull_branch_lane(engine, report, remote, &capture.commit, options.branch_pull)?;
    control.check()?;
    control.emit(GitSyncPhase::Pushing, report, Some(capture.tree.clone()))?;
    push_branch_lane(engine, options, report)?;
    let may_have_rewritten_worktree = report.branch.as_ref().is_some_and(|branch| {
        matches!(
            branch.action,
            GitBranchSyncAction::FastForwarded
                | GitBranchSyncAction::Merged
                | GitBranchSyncAction::Rebased
        ) || (branch.action == GitBranchSyncAction::Paused
            && branch
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("conflict")))
    });
    // A pushed or moved branch may now publish a tip the capture does not
    // link yet; recapture so this attempt publishes the link.
    let may_have_published_branch = report.branch.as_ref().is_some_and(|branch| branch.pushed);
    if !may_have_rewritten_worktree && !may_have_published_branch {
        return Ok(Some(capture));
    }
    let tracking = report
        .branch
        .as_ref()
        .and_then(|branch| branch.tracking.clone());
    if missing_branch_link(engine, report, tracking.as_ref(), Some(&capture.commit))?.is_none()
        && engine.worktree_matches_tree(&report.repository, &capture.commit)?
    {
        Ok(Some(capture))
    } else {
        capture_and_publish_device_backup(engine, options, report, control, remote, base)
    }
}

fn publish_device_backup(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    capture: &crate::GitCapture,
    observed: &mut Option<ObservedTip>,
) -> Result<bool, GitSyncError> {
    let published = match observed.take() {
        Some(ObservedTip(current)) => {
            publish_observed_device_backup(engine, options, report, capture, current.as_ref())?
        }
        None => false,
    } || publish_queried_device_backup(engine, options, report, capture)?;
    if published {
        *observed = report
            .device_backup
            .as_ref()
            .map(|backup| ObservedTip(Some(backup.published_revision.clone())));
    }
    Ok(published)
}

/// Publishes against the device head observed at the start of the attempt.
/// Returns `false` when the leased push was rejected, so the caller re-queries.
fn publish_observed_device_backup(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    capture: &crate::GitCapture,
    current: Option<&GitOid>,
) -> Result<bool, GitSyncError> {
    let tracked = engine.read_ref(&report.repository, &report.refs.device_tracking)?;
    if current.is_some() && current != tracked.as_ref() {
        return publish_device_backup_after_remote_moved(engine, options, report, capture, current);
    }
    publish_device_backup_against(engine, options, report, capture, current)
}

fn publish_queried_device_backup(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    capture: &crate::GitCapture,
) -> Result<bool, GitSyncError> {
    let tracked = engine.read_ref(&report.repository, &report.refs.device_tracking)?;
    if let Some(tracked) = tracked.as_ref() {
        let (target, outcome) =
            device_backup_target(engine, options, report, capture, Some(tracked))?;
        if &target == tracked {
            // Nothing new to publish. Confirm the remote head with a ref query
            // instead of a no-op leased push, which still opens a receive-pack
            // connection and runs pre-push hooks such as Git LFS.
            let current =
                engine.remote_ref(&report.repository, &options.remote, &report.refs.device)?;
            if current.as_ref() == Some(tracked) {
                record_device_backup(report, options, capture, target, outcome);
                return Ok(true);
            }
            return publish_device_backup_after_remote_moved(
                engine,
                options,
                report,
                capture,
                current.as_ref(),
            );
        }
        if push_device_backup(
            engine,
            options,
            report,
            capture,
            Some(tracked),
            target,
            outcome,
        )? {
            return Ok(true);
        }
    } else if publish_device_backup_against(engine, options, report, capture, None)? {
        return Ok(true);
    }
    let current = engine.remote_ref(&report.repository, &options.remote, &report.refs.device)?;
    publish_device_backup_after_remote_moved(engine, options, report, capture, current.as_ref())
}

fn publish_device_backup_after_remote_moved(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    capture: &crate::GitCapture,
    current: Option<&GitOid>,
) -> Result<bool, GitSyncError> {
    if let Some(current) = current {
        engine.fetch_ref(
            &report.repository,
            &options.remote,
            &report.refs.device,
            &report.refs.device_tracking,
        )?;
        debug_assert_eq!(
            engine.read_ref(&report.repository, &report.refs.device_tracking)?,
            Some(current.clone())
        );
    }
    let published = publish_device_backup_against(engine, options, report, capture, current)?;
    if let Some(backup) = report
        .device_backup
        .as_mut()
        .filter(|backup| backup.outcome == GitDeviceBackupOutcome::Bridged)
    {
        backup.foreign_history = true;
    }
    Ok(published)
}

/// Publishes against a remote head that was just observed, skipping the push
/// when that head already covers the capture.
fn publish_device_backup_against(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    capture: &crate::GitCapture,
    remote_before: Option<&GitOid>,
) -> Result<bool, GitSyncError> {
    let (target, outcome) = device_backup_target(engine, options, report, capture, remote_before)?;
    if remote_before == Some(&target) {
        record_device_backup(report, options, capture, target, outcome);
        return Ok(true);
    }
    push_device_backup(
        engine,
        options,
        report,
        capture,
        remote_before,
        target,
        outcome,
    )
}

fn push_device_backup(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &mut GitSyncReport,
    capture: &crate::GitCapture,
    remote_before: Option<&GitOid>,
    target: GitOid,
    outcome: GitDeviceBackupOutcome,
) -> Result<bool, GitSyncError> {
    if engine.push_ref(
        &report.repository,
        &options.remote,
        &target,
        &report.refs.device,
        remote_before,
    )? == GitPushResult::Rejected
    {
        return Ok(false);
    }
    if remote_before != Some(&target) {
        engine.update_ref(&report.repository, &report.refs.device_tracking, &target)?;
    }
    record_device_backup(report, options, capture, target, outcome);
    Ok(true)
}

fn record_device_backup(
    report: &mut GitSyncReport,
    options: &GitSyncOptions,
    capture: &crate::GitCapture,
    target: GitOid,
    outcome: GitDeviceBackupOutcome,
) {
    report.device_backup = Some(GitDeviceBackup {
        device_id: options.device_id.clone(),
        reference: report.refs.device.clone(),
        snapshot_revision: capture.commit.clone(),
        published_revision: target,
        outcome,
        foreign_history: false,
    });
}

fn device_backup_target(
    engine: &dyn GitEngine,
    options: &GitSyncOptions,
    report: &GitSyncReport,
    capture: &crate::GitCapture,
    remote_before: Option<&GitOid>,
) -> Result<(GitOid, GitDeviceBackupOutcome), GitSyncError> {
    let Some(remote_before) = remote_before else {
        return Ok((capture.commit.clone(), GitDeviceBackupOutcome::Published));
    };
    if remote_before == &capture.commit
        || engine.is_ancestor(&report.repository, &capture.commit, remote_before)?
    {
        return Ok((remote_before.clone(), GitDeviceBackupOutcome::Current));
    }
    if engine.is_ancestor(&report.repository, remote_before, &capture.commit)? {
        return Ok((capture.commit.clone(), GitDeviceBackupOutcome::Published));
    }
    let bridge = engine.create_reproducible_commit(
        &report.repository,
        &capture.tree,
        &[capture.commit.clone(), remote_before.clone()],
        &device_backup_message(&report.refs, options, &capture.commit, remote_before),
    )?;
    Ok((bridge, GitDeviceBackupOutcome::Bridged))
}

fn device_backup_message(
    refs: &GitSyncRefs,
    options: &GitSyncOptions,
    snapshot: &GitOid,
    previous: &GitOid,
) -> String {
    format!(
        "vulcan device backup bridge\n\n{}Vulcan-Device-Backup-Snapshot: {snapshot}\nVulcan-Device-Backup-Previous: {previous}\n",
        sync_trailers(refs, options, snapshot.as_str())
    )
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
            detail: None,
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
            detail: None,
        }));
    }
    Ok(None)
}

fn platform_preflight(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    revision: &GitOid,
    platform: GitPlatformProfile,
    persist_cache: bool,
) -> Result<GitPlatformPreflight, GitSyncError> {
    let policy = platform.policy();
    let policy_key = platform_policy_cache_key(&policy);
    if let Some(mut preflight) = load_platform_preflight_cache(repository, revision, &policy_key) {
        preflight.policy = policy;
        return Ok(preflight);
    }
    let entries = engine.tree_entries(repository, revision)?;
    let preflight = crate::inspect_git_tree_platform(revision.clone(), &entries, policy);
    if persist_cache {
        save_platform_preflight_cache(repository, &policy_key, &preflight)?;
    }
    Ok(preflight)
}

fn platform_policy_cache_key(policy: &crate::GitPlatformPolicy) -> String {
    let encoded = serde_json::to_vec(policy)
        .expect("the fixed Git platform policy representation is always serializable");
    blake3::hash(&encoded).to_hex().to_string()
}

fn platform_preflight_cache_path(repository: &GitRepository) -> std::path::PathBuf {
    repository
        .git_dir
        .join("vulcan-sync/platform-preflight-cache-v1.json")
}

fn load_platform_preflight_cache(
    repository: &GitRepository,
    revision: &GitOid,
    policy_key: &str,
) -> Option<GitPlatformPreflight> {
    let path = platform_preflight_cache_path(repository);
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_PLATFORM_PREFLIGHT_CACHE_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let cache = serde_json::from_slice::<PlatformPreflightCache>(&bytes).ok()?;
    let diagnostics_compatible = cache
        .preflight
        .diagnostics
        .iter()
        .all(|item| item.severity != crate::GitPlatformDiagnosticSeverity::Error);
    if cache.version != PLATFORM_PREFLIGHT_CACHE_VERSION
        || cache.policy_key != policy_key
        || cache.preflight.version != GIT_PLATFORM_PREFLIGHT_VERSION
        || cache.preflight.revision != *revision
        || cache.preflight.compatible != diagnostics_compatible
    {
        return None;
    }
    Some(cache.preflight)
}

fn save_platform_preflight_cache(
    repository: &GitRepository,
    policy_key: &str,
    preflight: &GitPlatformPreflight,
) -> Result<(), GitSyncError> {
    let path = platform_preflight_cache_path(repository);
    let parent = path
        .parent()
        .expect("the platform preflight cache path always has a parent");
    fs::create_dir_all(parent)?;
    if fs::symlink_metadata(&path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
        fs::remove_file(&path)?;
    }
    let bytes = serde_json::to_vec(&PlatformPreflightCache {
        version: PLATFORM_PREFLIGHT_CACHE_VERSION,
        policy_key: policy_key.to_string(),
        preflight: preflight.clone(),
    })
    .map_err(|error| {
        GitSyncError::Git(GitEngineError::InvalidOutput {
            operation: "cache Git platform preflight",
            detail: error.to_string(),
        })
    })?;
    if bytes.len() as u64 > MAX_PLATFORM_PREFLIGHT_CACHE_BYTES {
        return Ok(());
    }
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| GitSyncError::Io(error.error))?;
    Ok(())
}

fn snapshot_message(
    refs: &GitSyncRefs,
    options: &GitSyncOptions,
    source: Option<&GitOid>,
    link: Option<&GitOid>,
) -> String {
    let link = link.map_or_else(String::new, |link| {
        format!("Vulcan-Sync-Branch-Link: {link}\n")
    });
    format!(
        "vulcan live snapshot\n\n{}{link}",
        sync_trailers(refs, options, source.map_or("unborn", GitOid::as_str)),
    )
}

fn sync_trailers(refs: &GitSyncRefs, options: &GitSyncOptions, source: &str) -> String {
    format!(
        "Vulcan-Sync-Version: {SYNC_PROTOCOL_VERSION}\nVulcan-Ref-Namespace: {}\nVulcan-Sync-Device: {}\nVulcan-Sync-Profile: {}\nVulcan-Sync-Source: {source}\nVulcan-Sync-Semantic: false\n",
        refs.namespace_version,
        options.device_id.as_str(),
        refs.local
            .as_str()
            .split('/')
            .nth(3)
            .unwrap_or("unknown"),
    )
}

/// Removes stale lock files left in Vulcan-owned Git namespaces by a
/// force-terminated cycle. Only reachable while the repository lock is
/// held, so no other Vulcan process can legitimately hold these locks.
/// Locks outside the private namespaces (including the user's real index
/// lock) are left untouched.
fn sweep_stale_sync_locks(repository: &GitRepository) {
    remove_lock_files(&repository.git_dir.join("vulcan-sync"), 1);
    remove_lock_files(&repository.git_dir.join("refs").join("vulcan"), 8);
}

fn remove_lock_files(directory: &Path, depth: usize) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            remove_lock_files(&entry.path(), depth - 1);
        } else if file_type.is_file()
            && entry.file_name().to_string_lossy().ends_with(".lock")
            // The repository lock itself is not a stale Git lock.
            && entry.file_name() != "sync.lock"
        {
            let _ = fs::remove_file(entry.path());
        }
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

    #[test]
    fn device_id_parser_requires_canonical_key_ids() {
        // ULID-shaped IDs of earlier versions are refused in either case.
        for legacy in ["01ARZ3NDEKTSV4RRFFQ69G5FAV", "01arz3ndektsv4rrffq69g5fav"] {
            assert!(GitSyncDeviceId::parse(legacy).is_err(), "{legacy}");
        }
        // The anonymous placeholder never parses as a device.
        assert!(GitSyncDeviceId::parse(GitSyncDeviceId::anonymous().as_str()).is_err());

        let key_value = format!("vdev1_{}", "a".repeat(52));
        let key = GitSyncDeviceId::parse(&key_value).expect("canonical key ID");
        assert_eq!(key.as_str(), key_value);
        assert_eq!(key.kind(), GitSyncDeviceIdKind::SshKeyV1);
        let final_bit_set = format!("vdev1_{}q", "a".repeat(51));
        assert!(GitSyncDeviceId::parse(final_bit_set).is_ok());
        assert_eq!(
            serde_json::to_string(&key.kind()).expect("identity kind JSON"),
            "\"ssh_key_v1\""
        );

        for malformed in [
            format!("VDEV1_{}", "a".repeat(52)),
            format!("vdev1_{}", "A".repeat(52)),
            format!("vdev1_{}", "a".repeat(51)),
            format!("vdev1_{}b", "a".repeat(51)),
            format!("vdev1_{}!", "a".repeat(51)),
            format!("vdev2_{}", "a".repeat(52)),
        ] {
            assert!(
                GitSyncDeviceId::parse(malformed.clone()).is_err(),
                "accepted malformed key ID {malformed}"
            );
        }
    }

    #[test]
    fn git_versions_below_the_merge_tree_floor_are_rejected() {
        let installation = |major: u32, minor: u32, patch: u32| GitInstallation {
            engine: crate::GitEngineKind::Cli,
            executable: PathBuf::from("/usr/bin/git"),
            version: crate::GitVersion {
                raw: format!("git version {major}.{minor}.{patch}"),
                major,
                minor,
                patch,
                vendor_suffix: None,
            },
        };
        assert!(require_git_version(&installation(2, 37, 1)).is_err());
        assert!(require_git_version(&installation(2, 38, 0)).is_ok());
        assert!(require_git_version(&installation(2, 47, 0)).is_ok());
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
        run_git(path, &["config", "core.autocrlf", "false"]);
        run_git(path, &["config", "pull.rebase", "false"]);
        run_git(path, &["config", "pull.ff", "true"]);
    }

    fn commit_all(path: &Path, message: &str) {
        run_git(path, &["add", "--all", "--", "."]);
        run_git(path, &["commit", "--quiet", "-m", message]);
    }

    #[test]
    fn stale_private_lock_files_are_swept() {
        let temporary = TempDir::new().expect("temporary directory");
        init_repo(temporary.path());
        fs::write(temporary.path().join("Home.md"), "initial\n").expect("note");
        commit_all(temporary.path(), "initial");
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(temporary.path())
            .expect("repository");
        let git_dir = temporary.path().join(".git");
        let index_lock = git_dir.join("vulcan-sync").join("index.lock");
        fs::create_dir_all(index_lock.parent().expect("sync dir")).expect("sync dir");
        fs::write(&index_lock, b"stale").expect("stale index lock");
        let ref_lock = git_dir
            .join("refs")
            .join("vulcan")
            .join("sync")
            .join("profile")
            .join("live.lock");
        fs::create_dir_all(ref_lock.parent().expect("ref dir")).expect("ref dir");
        fs::write(&ref_lock, b"stale").expect("stale ref lock");
        let user_index_lock = git_dir.join("index.lock");
        fs::write(&user_index_lock, b"user").expect("user index lock");
        let unrelated = git_dir.join("vulcan-sync").join("sync.lock");
        fs::write(&unrelated, b"").expect("repository lock file");

        sweep_stale_sync_locks(&repository);

        assert!(!index_lock.exists());
        assert!(!ref_lock.exists());
        assert!(user_index_lock.exists());
        assert!(unrelated.exists());
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
                "-c",
                "core.autocrlf=false",
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
        run_git(&reader, &["config", "core.autocrlf", "false"]);
        reader
    }

    #[test]
    fn steady_sync_republishes_a_deleted_device_backup() {
        let (_temporary, _remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        let options = GitSyncOptions::default();
        let bootstrap = sync_git_once(&engine, &writer, &options).expect("bootstrap");
        let published = bootstrap
            .device_backup
            .expect("bootstrap device backup")
            .published_revision;
        run_git(
            &writer,
            &[
                "push",
                "--quiet",
                "origin",
                &format!(":{}", bootstrap.refs.device),
            ],
        );

        let report = sync_git_once(&engine, &writer, &options).expect("steady sync");

        assert_eq!(report.outcome, GitSyncOutcome::UpToDate);
        let repository = engine.discover_repository(&writer).expect("repository");
        assert_eq!(
            engine
                .remote_ref(&repository, &options.remote, &report.refs.device)
                .expect("remote device backup"),
            Some(published)
        );
    }

    #[test]
    fn rewriting_local_commits_bridges_the_device_backup_without_foreign_history() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        let first = sync_git_once(&engine, &writer, &commits_only()).expect("first sync");
        assert!(first.pause.is_none(), "{:?}", first.pause);
        // An agent replaces its pushed-or-not local commit, as a rebase does.
        advance_remote_branch(&temporary, &remote, "advanced\n");
        run_git(&writer, &["pull", "--rebase", "--quiet", "origin", "main"]);

        let second = sync_git_once(&engine, &writer, &commits_only()).expect("second sync");

        let backup = second.device_backup.expect("device backup");
        assert!(!backup.foreign_history, "{backup:?}");
    }

    #[test]
    fn steady_sync_bridges_a_device_backup_moved_elsewhere() {
        let (_temporary, _remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        let options = GitSyncOptions::default();
        let bootstrap = sync_git_once(&engine, &writer, &options).expect("bootstrap");
        let published = bootstrap
            .device_backup
            .expect("bootstrap device backup")
            .published_revision;
        let tree = git_stdout(&writer, &["rev-parse", &format!("{published}^{{tree}}")]);
        let foreign = git_stdout(
            &writer,
            &["commit-tree", &tree, "-m", "foreign device head"],
        );
        run_git(
            &writer,
            &[
                "push",
                "--quiet",
                "--force",
                "origin",
                &format!("{foreign}:{}", bootstrap.refs.device),
            ],
        );

        let report = sync_git_once(&engine, &writer, &options).expect("steady sync");

        assert_eq!(report.device_id, options.device_id);
        let backup = report.device_backup.expect("device backup");
        assert_eq!(backup.outcome, GitDeviceBackupOutcome::Bridged);
        assert!(
            backup.foreign_history,
            "the remote head was written elsewhere"
        );
        let repository = engine.discover_repository(&writer).expect("repository");
        let remote = engine
            .remote_ref(&repository, &options.remote, &report.refs.device)
            .expect("remote device backup")
            .expect("published device backup");
        assert_eq!(remote, backup.published_revision);
        assert!(engine
            .is_ancestor(
                &repository,
                &GitOid::parse(&foreign).expect("foreign oid"),
                &remote
            )
            .expect("ancestry"));
        assert!(engine
            .is_ancestor(&repository, &published, &remote)
            .expect("ancestry"));
    }

    #[test]
    fn pulling_a_remote_commit_rewrites_only_the_changed_paths() {
        let (temporary, remote, writer) = setup_tracked_branch();
        fs::create_dir(writer.join("Notes")).expect("notes directory");
        fs::write(writer.join("Other.md"), "untouched\n").expect("other note");
        fs::write(writer.join("Notes/Deep.md"), "untouched too\n").expect("deep note");
        commit_all(&writer, "add untouched notes");
        let engine = GitCliEngine::default();
        let options = GitSyncOptions::default();
        sync_git_once(&engine, &writer, &options).expect("writer bootstrap");
        let reader = clone_reader(&temporary, &remote, &writer);
        sync_git_once(&engine, &reader, &options).expect("reader adoption");
        backdate_worktree(&reader);
        let mtime = |path: &str| {
            fs::metadata(reader.join(path))
                .and_then(|metadata| metadata.modified())
                .expect("reader mtime")
        };
        let untouched = ["Other.md", "Notes/Deep.md"].map(|path| (path, mtime(path)));
        let home_before = mtime("Home.md");

        fs::write(writer.join("Home.md"), "changed remotely\n").expect("remote edit");
        commit_all(&writer, "remote edit");
        sync_git_once(&engine, &writer, &options).expect("writer publish");
        let report = sync_git_once(&engine, &reader, &options).expect("reader pull");

        assert_eq!(report.outcome, GitSyncOutcome::Pulled);
        assert_eq!(
            fs::read_to_string(reader.join("Home.md")).expect("applied note"),
            "changed remotely\n"
        );
        assert_ne!(mtime("Home.md"), home_before);
        for (path, before) in untouched {
            assert_eq!(
                mtime(path),
                before,
                "applying an unrelated change must not rewrite `{path}`"
            );
        }
    }

    #[test]
    fn live_snapshot_commits_record_versioned_machine_readable_provenance() {
        let (_temporary, _remote, writer) = setup_tracked_branch();
        fs::write(writer.join("Home.md"), "changed before sync\n").expect("local edit");
        let device_id =
            GitSyncDeviceId::parse(format!("vdev1_{}", "a".repeat(52))).expect("device ID");
        let options = GitSyncOptions {
            device_id: device_id.clone(),
            ..GitSyncOptions::default()
        };

        let report = sync_git_once(&GitCliEngine::default(), &writer, &options)
            .expect("snapshot sync should succeed");
        let snapshot = report.local_snapshot.expect("snapshot commit");
        let message = git_stdout(&writer, &["show", "-s", "--format=%B", snapshot.as_str()]);

        assert!(message.starts_with("vulcan live snapshot\n\n"));
        assert!(message.contains("Vulcan-Sync-Version: 2"));
        assert!(message.contains("Vulcan-Ref-Namespace: 3"));
        assert!(message.contains(&format!("Vulcan-Sync-Device: {}", device_id.as_str())));
        assert!(message.contains("Vulcan-Sync-Profile:"));
        assert!(message.contains("Vulcan-Sync-Source:"));
        assert!(message.contains("Vulcan-Sync-Semantic: false"));
    }

    #[test]
    fn git_backend_exposes_capabilities_and_runs_the_shared_contract() {
        let (_temporary, _remote, writer) = setup_tracked_branch();
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
        assert_eq!(report.outcome, SyncOutcome::UpToDate);
        assert_eq!(report.status.state, SyncState::Clean);
        assert_eq!(report.attempts, 1);
    }

    #[test]
    fn backend_errors_expose_stable_categories_and_retry_guidance() {
        let cancelled = classify_git_sync_error(&GitSyncError::Cancelled);
        assert_eq!(cancelled.category, SyncErrorCategory::Cancelled);
        assert!(!cancelled.retryable);

        let unavailable =
            classify_git_sync_error(&GitSyncError::Git(GitEngineError::ExecutableUnavailable {
                executable: PathBuf::from("git"),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
            }));
        assert_eq!(unavailable.category, SyncErrorCategory::Configuration);
        assert!(!unavailable.retryable);

        let remote = classify_git_sync_error(&GitSyncError::Git(GitEngineError::CommandFailed {
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

    fn setup_tracked_branch() -> (TempDir, PathBuf, PathBuf) {
        let (temporary, remote, writer) = setup_remote_and_writer();
        run_git(&writer, &["push", "--quiet", "-u", "origin", "main"]);
        (temporary, remote, writer)
    }

    fn advance_remote_branch(temporary: &TempDir, remote: &Path, contents: &str) {
        let other = temporary.path().join("upstream-work");
        let _ = std::fs::remove_dir_all(&other);
        run_git(
            temporary.path(),
            &[
                "clone",
                "--quiet",
                "-b",
                "main",
                remote.to_str().expect("remote path"),
                other.to_str().expect("clone path"),
            ],
        );
        run_git(&other, &["config", "user.name", "Vulcan Test"]);
        run_git(&other, &["config", "user.email", "vulcan@example.invalid"]);
        fs::write(other.join("Home.md"), contents).expect("upstream note");
        commit_all(&other, "upstream advance");
        run_git(&other, &["push", "--quiet", "origin", "main"]);
    }

    fn branch_action(report: &GitSyncReport) -> (GitBranchSyncAction, Option<String>) {
        let lane = report.branch.as_ref().expect("branch lane report");
        (lane.action, lane.detail.clone())
    }

    /// Moves every worktree file's mtime an hour back, so a sync index written
    /// now cannot hold racily clean entries that a steady check would refresh.
    fn backdate_worktree(path: &Path) {
        let past = std::time::SystemTime::now() - Duration::from_secs(3600);
        for entry in fs::read_dir(path).expect("read worktree directory") {
            let entry = entry.expect("worktree entry");
            if entry.file_name() == ".git" {
                continue;
            }
            let file_type = entry.file_type().expect("worktree entry type");
            if file_type.is_dir() {
                backdate_worktree(&entry.path());
            } else if file_type.is_file() {
                fs::File::options()
                    .write(true)
                    .open(entry.path())
                    .expect("open worktree file")
                    .set_modified(past)
                    .expect("backdate worktree file");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn racily_clean_sync_index_is_refreshed_once() {
        let (temporary, _remote, writer) = setup_tracked_branch();
        let (engine, trace) = traced_engine(&temporary);
        let options = GitSyncOptions::default();
        // An older note lets the refresh write an index that is no longer racy.
        backdate_worktree(&writer);
        sync_git_once(&engine, &writer, &options).expect("bootstrap");
        let repository = engine.discover_repository(&writer).expect("repository");
        let index = repository.git_dir.join("vulcan-sync/index");
        let note_mtime = fs::metadata(writer.join("Home.md"))
            .and_then(|metadata| metadata.modified())
            .expect("note mtime");
        fs::File::options()
            .write(true)
            .open(&index)
            .expect("open sync index")
            .set_modified(note_mtime)
            .expect("make the sync index racy");
        let refreshes = |commands: &str| {
            commands
                .lines()
                .filter(|line| line.contains(" update-index -q --refresh"))
                .count()
        };

        fs::write(&trace, "").expect("reset invocation trace");
        let report = sync_git_once(&engine, &writer, &options).expect("racy steady sync");
        assert_eq!(report.outcome, GitSyncOutcome::UpToDate);
        let commands = fs::read_to_string(&trace).expect("invocation trace");
        assert_eq!(
            refreshes(&commands),
            1,
            "a racily clean sync index should be refreshed once: {commands}"
        );

        fs::write(&trace, "").expect("reset invocation trace");
        let report = sync_git_once(&engine, &writer, &options).expect("healed steady sync");
        assert_eq!(report.outcome, GitSyncOutcome::UpToDate);
        let commands = fs::read_to_string(&trace).expect("invocation trace");
        assert_eq!(
            refreshes(&commands),
            0,
            "a refreshed sync index must not be refreshed again: {commands}"
        );
    }

    /// Returns an engine whose Git invocations are appended to the trace file.
    #[cfg(unix)]
    fn traced_engine(temporary: &TempDir) -> (GitCliEngine, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let trace = temporary.path().join("git-invocations.log");
        let wrapper = temporary.path().join("git-wrapper");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec git \"$@\"\n",
                trace.display()
            ),
        )
        .expect("Git wrapper");
        let mut permissions = fs::metadata(&wrapper)
            .expect("wrapper metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&wrapper, permissions).expect("executable wrapper");
        (GitCliEngine::new(&wrapper), trace)
    }

    #[cfg(unix)]
    #[test]
    fn missing_device_tracking_ref_is_restored_without_a_push() {
        let (temporary, _remote, writer) = setup_tracked_branch();
        let (engine, trace) = traced_engine(&temporary);
        let options = GitSyncOptions::default();
        let bootstrap = sync_git_once(&engine, &writer, &options).expect("bootstrap");
        let published = bootstrap
            .device_backup
            .expect("bootstrap device backup")
            .published_revision;
        run_git(
            &writer,
            &["update-ref", "-d", bootstrap.refs.device_tracking.as_str()],
        );

        fs::write(&trace, "").expect("reset invocation trace");
        let report = sync_git_once(&engine, &writer, &options).expect("steady sync");

        let backup = report.device_backup.expect("device backup");
        assert_eq!(backup.outcome, GitDeviceBackupOutcome::Current);
        assert_eq!(backup.published_revision, published);
        let repository = engine.discover_repository(&writer).expect("repository");
        assert_eq!(
            engine
                .read_ref(&repository, &report.refs.device_tracking)
                .expect("tracking ref"),
            Some(published)
        );
        let commands = fs::read_to_string(&trace).expect("invocation trace");
        assert!(
            commands.lines().all(|line| !line.contains(" push ")),
            "a remote head that already covers the capture must not be pushed: {commands}"
        );
    }

    #[test]
    fn branch_pull_fast_forwards_the_checked_out_branch() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        advance_remote_branch(&temporary, &remote, "advanced\n");

        let report = sync_git_once(&engine, &writer, &GitSyncOptions::default())
            .expect("sync with remote branch movement");

        let (action, _) = branch_action(&report);
        assert_eq!(action, GitBranchSyncAction::FastForwarded);
        assert_eq!(
            report.retries, 0,
            "the post-pull snapshot must converge on the first attempt"
        );
        let lane = report.branch.as_ref().expect("branch lane report");
        assert!(!lane.pushed, "nothing remains to publish after a pull");
        assert_eq!(lane.branch.as_str(), "refs/heads/main");
        assert_eq!(
            lane.upstream.as_ref().map(GitRefName::as_str),
            Some("refs/heads/main")
        );
        let after = lane.after.clone().expect("advanced head");
        let repository = engine.discover_repository(&writer).expect("repository");
        assert_eq!(engine.head_commit(&repository).expect("head"), Some(after));
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("pulled note"),
            "advanced\n"
        );
        assert!(report.pause.is_none());
    }

    #[test]
    fn branch_pull_merges_divergence_with_default_configuration() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        advance_remote_branch(&temporary, &remote, "advanced\n");

        let report = sync_git_once(&engine, &writer, &GitSyncOptions::default())
            .expect("sync with diverged branch");

        let (action, _) = branch_action(&report);
        assert_eq!(action, GitBranchSyncAction::Merged);
        let parents = git_stdout(&writer, &["rev-list", "--parents", "-n", "1", "HEAD"]);
        assert_eq!(
            parents.split_whitespace().count(),
            3,
            "diverged pull should merge with default configuration"
        );
        assert!(report.pause.is_none());
    }

    #[test]
    fn branch_pull_rebases_with_explicit_configuration() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        run_git(&writer, &["config", "pull.rebase", "true"]);
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        advance_remote_branch(&temporary, &remote, "advanced\n");

        let report = sync_git_once(&engine, &writer, &GitSyncOptions::default())
            .expect("sync with rebase configuration");

        let (action, _) = branch_action(&report);
        assert_eq!(action, GitBranchSyncAction::Rebased);
        let parents = git_stdout(&writer, &["rev-list", "--parents", "-n", "1", "HEAD"]);
        assert_eq!(
            parents.split_whitespace().count(),
            2,
            "rebased pull should keep history linear"
        );
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("rebased note"),
            "advanced\n"
        );
        assert!(report.pause.is_none());
    }

    #[test]
    fn branch_pull_pauses_divergence_under_ff_only() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("bootstrap sync");
        run_git(&writer, &["config", "pull.ff", "only"]);
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        advance_remote_branch(&temporary, &remote, "advanced\n");

        let report = sync_git_once(&engine, &writer, &GitSyncOptions::default())
            .expect("sync with ff-only divergence");

        let (action, detail) = branch_action(&report);
        assert_eq!(action, GitBranchSyncAction::Paused);
        assert!(
            detail.is_some_and(|detail| detail.contains("pull.ff=only")),
            "pause detail should name the blocking configuration"
        );
        assert_eq!(report.outcome, GitSyncOutcome::Paused);
    }

    #[test]
    fn vault_branch_pull_policy_overrides_device_git_configuration() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("bootstrap sync");
        // The device would rebase, and a malformed pull.ff must not matter
        // once the vault fixes the strategy.
        run_git(&writer, &["config", "pull.rebase", "true"]);
        run_git(&writer, &["config", "pull.ff", "bogus"]);
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        let local = git_stdout(&writer, &["rev-parse", "HEAD"]);
        advance_remote_branch(&temporary, &remote, "advanced\n");
        let ff_only = GitSyncOptions {
            branch_pull: BranchPullPolicy::FfOnly,
            ..GitSyncOptions::default()
        };

        let preview = sync_git_once(
            &engine,
            &writer,
            &GitSyncOptions {
                dry_run: true,
                ..ff_only.clone()
            },
        )
        .expect("preview with vault policy");
        let (_, detail) = branch_action(&preview);
        assert!(
            detail.is_some_and(|detail| detail.contains("fast-forward only")),
            "preview should report the vault strategy"
        );

        let report = sync_git_once(&engine, &writer, &ff_only).expect("sync with vault ff-only");
        let (action, detail) = branch_action(&report);
        assert_eq!(action, GitBranchSyncAction::Paused);
        assert!(
            detail.is_some_and(|detail| detail.contains("sync.branch_pull")),
            "pause detail should name the vault setting"
        );
        assert_eq!(git_stdout(&writer, &["rev-parse", "HEAD"]), local);

        let report = sync_git_once(
            &engine,
            &writer,
            &GitSyncOptions {
                branch_pull: BranchPullPolicy::Merge,
                ..GitSyncOptions::default()
            },
        )
        .expect("sync with vault merge");
        let (action, _) = branch_action(&report);
        assert_eq!(action, GitBranchSyncAction::Merged);
    }

    #[test]
    fn branch_pull_conflict_is_aborted_and_leaves_the_branch_as_it_was() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        sync_git_once(&engine, &writer, &GitSyncOptions::default()).expect("bootstrap sync");
        fs::write(writer.join("Home.md"), "local conflict\n").expect("conflict note");
        commit_all(&writer, "local conflict");
        let local_before_pull =
            GitOid::parse(git_stdout(&writer, &["rev-parse", "HEAD"])).expect("local revision");
        advance_remote_branch(&temporary, &remote, "remote conflict\n");

        let report = sync_git_once(&engine, &writer, &GitSyncOptions::default())
            .expect("sync with branch conflict reports normally");

        let (action, detail) = branch_action(&report);
        assert_eq!(action, GitBranchSyncAction::Paused);
        assert!(
            detail.is_some_and(|detail| detail.contains("aborted")),
            "pause detail should say the conflicted merge was backed out"
        );
        assert!(
            !report.branch.as_ref().expect("branch lane report").pushed,
            "a conflicted branch must not publish"
        );
        let repository = engine.discover_repository(&writer).expect("repository");
        assert!(
            !repository.git_dir.join("MERGE_HEAD").exists(),
            "an unattended checkout must not be left mid-merge"
        );
        assert_eq!(
            GitOid::parse(git_stdout(&writer, &["rev-parse", "HEAD"])).expect("head"),
            local_before_pull,
            "aborting restores the pre-pull branch"
        );
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("local note"),
            "local conflict\n"
        );
        let pre_pull_tree = engine
            .tree_oid(&repository, &local_before_pull)
            .expect("pre-pull tree");
        let backup = report
            .device_backup
            .as_ref()
            .expect("conflicted pull still publishes a device backup");
        let reachable_trees = git_stdout(
            &writer,
            &["log", "--format=%T", backup.published_revision.as_str()],
        );
        assert!(
            reachable_trees
                .lines()
                .any(|tree| tree == pre_pull_tree.as_str()),
            "the exact pre-pull tree bytes must remain reachable after a conflicted pull"
        );
        assert_eq!(
            engine
                .remote_ref(&repository, &report.remote, &backup.reference)
                .expect("remote device backup"),
            Some(backup.published_revision.clone())
        );
    }

    #[test]
    fn branch_lane_yields_to_in_progress_operations() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        advance_remote_branch(&temporary, &remote, "advanced\n");
        let repository = engine.discover_repository(&writer).expect("repository");
        let head = engine
            .head_commit(&repository)
            .expect("head query")
            .expect("head");
        fs::write(repository.git_dir.join("MERGE_HEAD"), format!("{head}\n")).expect("merge state");

        let report = sync_git_once(&engine, &writer, &GitSyncOptions::default())
            .expect("sync with merge state reports normally");

        assert_eq!(report.outcome, GitSyncOutcome::Paused);
        assert_eq!(
            report.pause.as_ref().map(|pause| pause.reason),
            Some(GitSyncPauseReason::OperationInProgress)
        );
        assert!(
            report.branch.is_none(),
            "the branch lane must not start, let alone error, mid-operation"
        );
    }

    #[test]
    fn branch_lane_previews_without_mutation() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        advance_remote_branch(&temporary, &remote, "advanced\n");
        let head_before = git_stdout(&writer, &["rev-parse", "HEAD"]);
        let repository = engine.discover_repository(&writer).expect("repository");
        let tracking_before = engine
            .read_ref(
                &repository,
                &GitRefName::parse("refs/remotes/origin/main").expect("tracking ref"),
            )
            .expect("tracking ref");

        let report = sync_git_once(
            &engine,
            &writer,
            &GitSyncOptions {
                dry_run: true,
                ..GitSyncOptions::default()
            },
        )
        .expect("dry-run sync");

        let (action, _) = branch_action(&report);
        assert_eq!(action, GitBranchSyncAction::Planned);
        assert_eq!(git_stdout(&writer, &["rev-parse", "HEAD"]), head_before);
        assert_eq!(
            engine
                .read_ref(
                    &repository,
                    &GitRefName::parse("refs/remotes/origin/main").expect("tracking ref"),
                )
                .expect("tracking ref"),
            tracking_before,
            "dry run must not fetch the branch"
        );
    }

    #[test]
    fn branch_push_publishes_local_commits() {
        let (_temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");

        let report = sync_git_once(&engine, &writer, &GitSyncOptions::default())
            .expect("sync with unpublished commits");

        let lane = report.branch.as_ref().expect("branch lane report");
        assert_eq!(lane.action, GitBranchSyncAction::UpToDate);
        assert!(lane.pushed);
        let head = git_stdout(&writer, &["rev-parse", "HEAD"]);
        let remote_tip = git_stdout(
            &writer,
            &[
                "ls-remote",
                remote.to_str().expect("remote path"),
                "refs/heads/main",
            ],
        );
        assert!(
            remote_tip.starts_with(&head),
            "remote main should equal the published head"
        );
    }

    #[test]
    fn published_branch_tip_excludes_unpushed_commits() {
        let (_temporary, _remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        let repository = engine.discover_repository(&writer).expect("repository");
        let published = git_stdout(&writer, &["rev-parse", "HEAD"]);
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "unpushed");

        assert_eq!(
            git_published_branch_tip(&engine, &repository)
                .expect("published tip")
                .map(|tip| tip.to_string()),
            Some(published)
        );
        run_git(&writer, &["branch", "--unset-upstream"]);
        assert_eq!(
            git_published_branch_tip(&engine, &repository).expect("published tip"),
            None
        );
    }

    #[test]
    fn branch_pull_then_push_propagates_both_directions() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        advance_remote_branch(&temporary, &remote, "advanced\n");

        let report = sync_git_once(&engine, &writer, &GitSyncOptions::default())
            .expect("sync with diverged branch");

        let lane = report.branch.as_ref().expect("branch lane report");
        assert_eq!(lane.action, GitBranchSyncAction::Merged);
        assert!(lane.pushed);
        let head = git_stdout(&writer, &["rev-parse", "HEAD"]);
        let remote_tip = git_stdout(
            &writer,
            &[
                "ls-remote",
                remote.to_str().expect("remote path"),
                "refs/heads/main",
            ],
        );
        assert!(
            remote_tip.starts_with(&head),
            "remote main should equal the merged head"
        );
    }

    #[test]
    #[cfg(unix)]
    fn branch_push_decline_records_detail_without_failing_sync() {
        use std::os::unix::fs::PermissionsExt;

        let (_temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        let hook = remote.join("hooks").join("pre-receive");
        fs::write(
            &hook,
            "#!/bin/sh\nwhile read old new ref; do\n  case \"$ref\" in refs/heads/main) echo \"branch main is protected\" >&2; exit 1;; esac\ndone\nexit 0\n",
        )
        .expect("declining hook");
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("hook executable");
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");

        let report = sync_git_once(&engine, &writer, &GitSyncOptions::default())
            .expect("sync with declined push reports normally");

        assert_ne!(report.outcome, GitSyncOutcome::Paused);
        let lane = report.branch.as_ref().expect("branch lane report");
        assert!(!lane.pushed);
        assert!(
            lane.push_detail.as_deref().is_some_and(
                |detail| detail.contains("refs/heads/main") && detail.contains("protected")
            ),
            "push detail should name the branch and the decline: {:?}",
            lane.push_detail
        );
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
        let (_temporary, _remote, writer) = setup_tracked_branch();
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
                detail: None,
            })
        );
    }

    fn commits_only() -> GitSyncOptions {
        GitSyncOptions::default()
    }

    fn remote_ref_tip(remote: &Path, reference: &str) -> String {
        git_stdout(
            remote,
            &["for-each-ref", "--format=%(objectname)", reference],
        )
    }

    #[test]
    fn committed_history_only_keeps_the_safety_snapshot_local_unless_asked() {
        let (_temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        fs::write(writer.join("Local.md"), "local\n").expect("note");
        commit_all(&writer, "local");
        fs::write(writer.join("Home.md"), "half-finished edit\n").expect("uncommitted edit");
        let options = GitSyncOptions {
            publish_device_backup: false,
            ..commits_only()
        };

        let report = sync_git_once(&engine, &writer, &options).expect("sync");

        assert_eq!(report.outcome, GitSyncOutcome::Pushed);
        assert!(
            report.local_snapshot.is_some(),
            "the snapshot still exists locally"
        );
        assert!(report.device_backup.is_none());
        assert_eq!(
            git_stdout(
                &remote,
                &[
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/heads/__vulcan-sync"
                ]
            ),
            "",
            "no Vulcan ref reaches the remote"
        );
        assert_eq!(
            remote_ref_tip(&remote, "refs/heads/main"),
            git_stdout(&writer, &["rev-parse", "HEAD"])
        );
    }

    #[test]
    fn committed_history_only_propagates_commits_but_not_uncommitted_edits() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let reader = clone_reader(&temporary, &remote, &writer);
        let engine = GitCliEngine::default();
        fs::write(writer.join("script.sh"), "echo committed\n").expect("committed file");
        commit_all(&writer, "add script");
        fs::write(writer.join("Home.md"), "half-finished edit\n").expect("uncommitted edit");
        fs::write(writer.join("Draft.md"), "untracked draft\n").expect("untracked file");

        let pushed = sync_git_once(&engine, &writer, &commits_only()).expect("writer sync");

        assert_eq!(pushed.outcome, GitSyncOutcome::Pushed);
        assert!(pushed.branch.as_ref().is_some_and(|lane| lane.pushed));
        assert!(remote_ref_tip(&remote, DEFAULT_REMOTE_LIVE_REF).is_empty());
        assert_eq!(
            remote_ref_tip(&remote, "refs/heads/main"),
            git_stdout(&writer, &["rev-parse", "HEAD"])
        );
        assert!(
            remote_ref_tip(&remote, DEFAULT_REMOTE_LIVE_REF).is_empty(),
            "committed-history sync must never create live"
        );
        let backup = pushed.device_backup.as_ref().expect("device safety head");
        assert_eq!(
            git_stdout(
                &remote,
                &["show", &format!("{}:Draft.md", backup.published_revision)]
            ),
            "untracked draft",
            "the safety head still backs up uncommitted work"
        );

        let pulled = sync_git_once(&engine, &reader, &commits_only()).expect("reader sync");

        assert_eq!(pulled.outcome, GitSyncOutcome::Pulled);
        assert_eq!(
            fs::read_to_string(reader.join("script.sh")).expect("pulled commit"),
            "echo committed\n"
        );
        assert_eq!(
            fs::read_to_string(reader.join("Home.md")).expect("reader note"),
            "initial\n"
        );
        assert!(!reader.join("Draft.md").exists());

        let steady = sync_git_once(&engine, &reader, &commits_only()).expect("steady sync");
        assert_eq!(steady.outcome, GitSyncOutcome::UpToDate);
        assert!(pushed.pause.is_none() && steady.pause.is_none());
    }

    #[test]
    fn committed_history_only_fast_forwards_past_unrelated_dirty_files() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        fs::write(writer.join("Other.md"), "tracked\n").expect("other note");
        commit_all(&writer, "other");
        run_git(&writer, &["push", "--quiet", "origin", "main"]);
        fs::write(writer.join("Other.md"), "dirty local edit\n").expect("dirty edit");
        advance_remote_branch(&temporary, &remote, "advanced\n");

        let report = sync_git_once(&engine, &writer, &commits_only()).expect("sync");

        assert_eq!(report.outcome, GitSyncOutcome::Pulled);
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("pulled note"),
            "advanced\n"
        );
        assert_eq!(
            fs::read_to_string(writer.join("Other.md")).expect("dirty note"),
            "dirty local edit\n"
        );
    }

    #[test]
    fn committed_history_only_pauses_when_the_branch_lane_pauses() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        run_git(&writer, &["config", "pull.ff", "only"]);
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        advance_remote_branch(&temporary, &remote, "advanced\n");

        let report = sync_git_once(&engine, &writer, &commits_only()).expect("sync");

        assert_eq!(report.outcome, GitSyncOutcome::Paused);
        let pause = report.pause.expect("pause");
        assert_eq!(pause.reason, GitSyncPauseReason::BranchPaused);
        assert!(pause
            .detail
            .is_some_and(|detail| detail.contains("pull.ff=only")));
    }

    fn guarded() -> GitSyncOptions {
        GitSyncOptions {
            branch_pull: BranchPullPolicy::GuardedRebase,
            ..commits_only()
        }
    }

    /// A local commit and a newer upstream commit touching different files.
    fn diverged_branch() -> (TempDir, PathBuf, PathBuf, String) {
        let (temporary, remote, writer) = setup_tracked_branch();
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        let local = git_stdout(&writer, &["rev-parse", "HEAD"]);
        advance_remote_branch(&temporary, &remote, "advanced\n");
        (temporary, remote, writer, local)
    }

    #[test]
    fn guarded_rebase_replays_commits_only_this_branch_has_and_publishes_them() {
        let (_temporary, remote, writer, local) = diverged_branch();
        let engine = GitCliEngine::default();

        let report = sync_git_once(&engine, &writer, &guarded()).expect("guarded sync");

        assert!(report.pause.is_none(), "{:?}", report.pause);
        let (action, _) = branch_action(&report);
        assert_eq!(action, GitBranchSyncAction::Rebased);
        assert_ne!(git_stdout(&writer, &["rev-parse", "HEAD"]), local);
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("upstream note"),
            "advanced\n"
        );
        assert!(writer.join("Local.md").exists());
        assert_eq!(
            remote_ref_tip(&remote, "refs/heads/main"),
            git_stdout(&writer, &["rev-parse", "HEAD"]),
            "the rebased commit is published"
        );
    }

    #[test]
    fn guarded_rebase_leaves_commits_another_branch_or_worktree_builds_on() {
        let engine = GitCliEngine::default();
        for shared in ["branch", "worktree"] {
            let (temporary, _remote, writer, local) = diverged_branch();
            if shared == "branch" {
                run_git(&writer, &["branch", "feature"]);
            } else {
                let worktree = temporary.path().join("agent-worktree");
                run_git(
                    &writer,
                    &[
                        "worktree",
                        "add",
                        "--quiet",
                        "--detach",
                        worktree.to_str().expect("worktree path"),
                    ],
                );
            }

            let report = sync_git_once(&engine, &writer, &guarded()).expect("guarded sync");

            let pause = report.pause.expect("paused");
            assert_eq!(pause.reason, GitSyncPauseReason::BranchPaused);
            let detail = pause.detail.expect("detail");
            let expected = if shared == "branch" {
                "refs/heads/feature"
            } else {
                "worktree:"
            };
            assert!(detail.contains(expected), "{shared}: {detail}");
            assert_eq!(
                git_stdout(&writer, &["rev-parse", "HEAD"]),
                local,
                "{shared}"
            );
        }
    }

    #[test]
    fn guarded_rebase_leaves_merges_and_aborts_conflicts() {
        let engine = GitCliEngine::default();
        let (temporary, _remote, writer, _) = diverged_branch();
        run_git(&writer, &["checkout", "--quiet", "-b", "topic", "HEAD~1"]);
        fs::write(writer.join("Topic.md"), "topic\n").expect("topic note");
        commit_all(&writer, "topic");
        run_git(&writer, &["checkout", "--quiet", "main"]);
        run_git(
            &writer,
            &["merge", "--quiet", "--no-ff", "--no-edit", "topic"],
        );
        run_git(&writer, &["branch", "--quiet", "-D", "topic"]);
        let merged = git_stdout(&writer, &["rev-parse", "HEAD"]);
        let report = sync_git_once(&engine, &writer, &guarded()).expect("merge sync");
        let detail = report.pause.and_then(|pause| pause.detail).expect("detail");
        assert!(detail.contains("merges"), "{detail}");
        assert_eq!(git_stdout(&writer, &["rev-parse", "HEAD"]), merged);
        drop(temporary);

        let (temporary, remote, writer) = setup_tracked_branch();
        fs::write(writer.join("Home.md"), "local conflict\n").expect("conflict note");
        commit_all(&writer, "local conflict");
        let local = git_stdout(&writer, &["rev-parse", "HEAD"]);
        advance_remote_branch(&temporary, &remote, "remote conflict\n");
        let report = sync_git_once(&engine, &writer, &guarded()).expect("conflict sync");
        let detail = report.pause.and_then(|pause| pause.detail).expect("detail");
        assert!(detail.contains("aborted"), "{detail}");
        assert_eq!(git_stdout(&writer, &["rev-parse", "HEAD"]), local);
    }

    #[test]
    fn guarded_rebase_replays_past_uncommitted_edits_it_does_not_touch() {
        let (_temporary, remote, writer, local) = diverged_branch();
        let engine = GitCliEngine::default();
        fs::write(writer.join("Local.md"), "local, still being edited\n").expect("dirty edit");
        fs::write(writer.join("Scratch.md"), "untracked\n").expect("untracked note");

        let report = sync_git_once(&engine, &writer, &guarded()).expect("guarded sync");

        let (action, _) = branch_action(&report);
        assert_eq!(action, GitBranchSyncAction::Rebased, "{:?}", report.branch);
        assert_ne!(git_stdout(&writer, &["rev-parse", "HEAD"]), local);
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("upstream note"),
            "advanced\n"
        );
        assert_eq!(
            fs::read_to_string(writer.join("Local.md")).expect("dirty note"),
            "local, still being edited\n"
        );
        assert!(writer.join("Scratch.md").exists());
        assert_eq!(
            remote_ref_tip(&remote, "refs/heads/main"),
            git_stdout(&writer, &["rev-parse", "HEAD"])
        );
    }

    #[test]
    fn guarded_rebase_defers_when_uncommitted_edits_overlap_the_upstream() {
        let (_temporary, _remote, writer, local) = diverged_branch();
        let engine = GitCliEngine::default();
        fs::write(writer.join("Home.md"), "edited here\n").expect("overlapping edit");

        let report = sync_git_once(&engine, &writer, &guarded()).expect("guarded sync");

        let (action, detail) = branch_action(&report);
        assert_eq!(action, GitBranchSyncAction::Deferred, "{detail:?}");
        assert_eq!(git_stdout(&writer, &["rev-parse", "HEAD"]), local);
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("kept edit"),
            "edited here\n"
        );
    }

    #[test]
    fn a_paused_branch_resumes_by_itself_once_the_author_rebases_and_pushes() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        let ff_only = GitSyncOptions {
            branch_pull: BranchPullPolicy::FfOnly,
            ..commits_only()
        };
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        advance_remote_branch(&temporary, &remote, "advanced\n");
        let paused = sync_git_once(&engine, &writer, &ff_only).expect("paused sync");
        assert_eq!(paused.outcome, GitSyncOutcome::Paused);

        // What the pause tells the author to do, and nothing else.
        run_git(&writer, &["pull", "--rebase", "--quiet", "origin", "main"]);
        run_git(&writer, &["push", "--quiet", "origin", "main"]);

        let resumed = sync_git_once(&engine, &writer, &ff_only).expect("resumed sync");
        assert_ne!(resumed.outcome, GitSyncOutcome::Paused);
        assert!(resumed.pause.is_none(), "{:?}", resumed.pause);
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("upstream note"),
            "advanced\n"
        );
    }

    struct FakeResolver {
        answer: Result<String, String>,
        seen: Vec<(String, Vec<crate::BranchReplayConflict>)>,
    }

    impl crate::BranchReplayResolver for FakeResolver {
        fn resolve(
            &mut self,
            subject: &str,
            conflicts: &[crate::BranchReplayConflict],
        ) -> Result<Vec<(String, String)>, String> {
            self.seen.push((subject.to_string(), conflicts.to_vec()));
            let answer = self.answer.clone()?;
            Ok(conflicts
                .iter()
                .map(|conflict| (conflict.path.clone(), answer.clone()))
                .collect())
        }
    }

    /// A local commit and an upstream commit that both rewrite `Home.md`,
    /// plus a non-conflicting local commit on top.
    fn conflicting_branch() -> (TempDir, PathBuf, PathBuf, String, GitOid) {
        let (temporary, remote, writer) = setup_tracked_branch();
        fs::write(writer.join("Home.md"), "local\n").expect("local note");
        commit_all(&writer, "local edit");
        fs::write(writer.join("Other.md"), "other\n").expect("other note");
        commit_all(&writer, "other note");
        let local = git_stdout(&writer, &["rev-parse", "HEAD"]);
        advance_remote_branch(&temporary, &remote, "remote\n");
        run_git(&writer, &["fetch", "--quiet", "origin"]);
        let upstream =
            GitOid::parse(git_stdout(&writer, &["rev-parse", "origin/main"])).expect("upstream");
        (temporary, remote, writer, local, upstream)
    }

    fn replay(
        engine: &GitCliEngine,
        writer: &Path,
        upstream: &GitOid,
        resolver: &mut FakeResolver,
    ) -> crate::BranchReplayOutcome {
        let repository = engine.discover_repository(writer).expect("repository");
        let branch = GitRefName::parse("refs/heads/main").expect("branch");
        engine
            .replay_branch(&repository, &branch, upstream, resolver)
            .expect("replay")
    }

    #[test]
    fn a_resolver_settles_a_conflicted_replay_and_the_branch_moves_cleanly() {
        let (_temporary, _remote, writer, local, upstream) = conflicting_branch();
        let engine = GitCliEngine::default();
        let mut resolver = FakeResolver {
            answer: Ok("local and remote\n".to_string()),
            seen: Vec::new(),
        };

        let outcome = replay(&engine, &writer, &upstream, &mut resolver);

        let crate::BranchReplayOutcome::Replayed {
            from, to, resolved, ..
        } = outcome
        else {
            panic!("declined: {outcome:?}");
        };
        assert_eq!(from.as_str(), local);
        assert_eq!(resolved, vec!["Home.md".to_string()]);
        assert_eq!(git_stdout(&writer, &["rev-parse", "HEAD"]), to.as_str());
        assert_eq!(
            git_stdout(&writer, &["rev-list", "--count", "origin/main..HEAD"]),
            "2"
        );
        assert_eq!(
            git_stdout(&writer, &["merge-base", "HEAD", "origin/main"]),
            upstream.as_str()
        );
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("resolved"),
            "local and remote\n"
        );
        assert!(writer.join("Other.md").exists());
        assert_eq!(git_stdout(&writer, &["status", "--porcelain"]), "");
        let (subject, conflicts) = &resolver.seen[0];
        assert_eq!(subject, "local edit");
        assert_eq!(conflicts[0].local.as_deref(), Some("local\n"));
        assert_eq!(conflicts[0].upstream.as_deref(), Some("remote\n"));
        assert_eq!(conflicts[0].base.as_deref(), Some("initial\n"));
        let worktrees = git_stdout(&writer, &["worktree", "list", "--porcelain"]);
        assert_eq!(worktrees.matches("worktree ").count(), 1, "{worktrees}");
    }

    #[test]
    fn a_replay_drops_commits_the_upstream_already_contains() {
        let (temporary, remote, writer) = setup_tracked_branch();
        fs::write(writer.join("Home.md"), "same\n").expect("local note");
        commit_all(&writer, "same change");
        fs::write(writer.join("Other.md"), "other\n").expect("other note");
        commit_all(&writer, "other note");
        advance_remote_branch(&temporary, &remote, "same\n");
        run_git(&writer, &["fetch", "--quiet", "origin"]);
        let upstream =
            GitOid::parse(git_stdout(&writer, &["rev-parse", "origin/main"])).expect("upstream");
        let engine = GitCliEngine::default();
        let mut resolver = FakeResolver {
            answer: Err("no conflict expected".to_string()),
            seen: Vec::new(),
        };

        let outcome = replay(&engine, &writer, &upstream, &mut resolver);

        assert!(
            matches!(outcome, crate::BranchReplayOutcome::Replayed { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            git_stdout(&writer, &["log", "--format=%s", "origin/main..HEAD"]),
            "other note"
        );
        assert!(resolver.seen.is_empty());
    }

    #[test]
    fn a_replay_runs_no_hooks_and_says_when_it_drops_signing() {
        let (temporary, _remote, writer, _local, upstream) = conflicting_branch();
        let marker = temporary.path().join("hook-ran");
        let hooks = writer.join(".git/hooks");
        fs::create_dir_all(&hooks).expect("hooks");
        for hook in ["pre-commit", "commit-msg", "post-commit"] {
            let path = hooks.join(hook);
            fs::write(
                &path,
                format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
            )
            .expect("hook");
            let mut permissions = fs::metadata(&path).expect("hook").permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
            fs::set_permissions(&path, permissions).expect("executable hook");
        }
        // No signing key exists, so signing would fail the replay.
        run_git(&writer, &["config", "commit.gpgSign", "true"]);
        run_git(&writer, &["config", "gpg.program", "false"]);
        let engine = GitCliEngine::default();
        let mut resolver = FakeResolver {
            answer: Ok("both\n".to_string()),
            seen: Vec::new(),
        };

        let outcome = replay(&engine, &writer, &upstream, &mut resolver);

        let crate::BranchReplayOutcome::Replayed {
            signatures_dropped, ..
        } = outcome
        else {
            panic!("declined: {outcome:?}");
        };
        assert!(signatures_dropped);
        assert!(!marker.exists(), "a hook ran during the replay");
    }

    #[test]
    fn a_replay_declines_paths_with_a_git_filter() {
        let (_temporary, _remote, writer, local, upstream) = conflicting_branch();
        fs::write(
            writer.join(".git/info/attributes"),
            "Home.md filter=crypt\n",
        )
        .expect("attributes");
        let engine = GitCliEngine::default();
        let mut resolver = FakeResolver {
            answer: Ok("both\n".to_string()),
            seen: Vec::new(),
        };

        let outcome = replay(&engine, &writer, &upstream, &mut resolver);

        let crate::BranchReplayOutcome::Declined { reason } = outcome else {
            panic!("replayed: {outcome:?}");
        };
        assert!(reason.contains("crypt"), "{reason}");
        assert!(resolver.seen.is_empty());
        assert_eq!(git_stdout(&writer, &["rev-parse", "HEAD"]), local);
    }

    #[test]
    fn a_prepared_replay_is_not_applied_once_the_branch_moved() {
        let (_temporary, _remote, writer, _local, upstream) = conflicting_branch();
        let engine = GitCliEngine::default();
        let repository = engine.discover_repository(&writer).expect("repository");
        let branch = GitRefName::parse("refs/heads/main").expect("branch");
        let mut resolver = FakeResolver {
            answer: Ok("both\n".to_string()),
            seen: Vec::new(),
        };
        let prepared = engine
            .prepare_branch_replay(&repository, &branch, &upstream, &mut resolver)
            .expect("prepare")
            .expect("prepared");
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("note"),
            "local\n",
            "preparing must not touch the checkout"
        );
        // Someone commits while the agent was thinking.
        fs::write(writer.join("Late.md"), "late\n").expect("late note");
        commit_all(&writer, "late");
        let late = git_stdout(&writer, &["rev-parse", "HEAD"]);

        let outcome = engine
            .apply_branch_replay(&repository, &branch, prepared)
            .expect("apply");

        assert!(
            matches!(outcome, crate::BranchReplayOutcome::Declined { .. }),
            "{outcome:?}"
        );
        assert_eq!(git_stdout(&writer, &["rev-parse", "HEAD"]), late);
        assert_eq!(git_stdout(&writer, &["status", "--porcelain"]), "");
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("note"),
            "local\n"
        );
    }

    #[test]
    fn a_failed_or_unsafe_resolution_leaves_the_branch_alone() {
        let engine = GitCliEngine::default();
        for case in ["error", "markers", "dirty", "shared"] {
            let (_temporary, _remote, writer, local, upstream) = conflicting_branch();
            let answer = match case {
                "error" => Err("model unavailable".to_string()),
                "markers" => Ok("<<<<<<< ours\nlocal\n=======\nremote\n>>>>>>> theirs\n".into()),
                _ => Ok("merged\n".to_string()),
            };
            if case == "dirty" {
                fs::write(writer.join("Home.md"), "uncommitted\n").expect("dirty edit");
            }
            if case == "shared" {
                run_git(&writer, &["branch", "feature"]);
            }
            let mut resolver = FakeResolver {
                answer,
                seen: Vec::new(),
            };

            let outcome = replay(&engine, &writer, &upstream, &mut resolver);

            assert!(
                matches!(outcome, crate::BranchReplayOutcome::Declined { .. }),
                "{case}: {outcome:?}"
            );
            assert_eq!(git_stdout(&writer, &["rev-parse", "HEAD"]), local, "{case}");
            let expected = if case == "dirty" {
                "uncommitted\n"
            } else {
                "local\n"
            };
            assert_eq!(
                fs::read_to_string(writer.join("Home.md")).expect("note"),
                expected,
                "{case}"
            );
            assert_eq!(resolver.seen.is_empty(), case == "shared", "{case}");
            let worktrees = git_stdout(&writer, &["worktree", "list", "--porcelain"]);
            assert_eq!(worktrees.matches("worktree ").count(), 1, "{case}");
        }
    }

    #[test]
    fn committed_history_only_aborts_a_conflicted_rebase_and_pauses() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        run_git(&writer, &["config", "pull.rebase", "true"]);
        fs::write(writer.join("Home.md"), "local conflict\n").expect("conflict note");
        commit_all(&writer, "local conflict");
        let local_before_pull =
            GitOid::parse(git_stdout(&writer, &["rev-parse", "HEAD"])).expect("local revision");
        advance_remote_branch(&temporary, &remote, "remote conflict\n");

        let report = sync_git_once(&engine, &writer, &commits_only()).expect("sync");

        assert_eq!(report.outcome, GitSyncOutcome::Paused);
        let pause = report.pause.expect("pause");
        assert_eq!(pause.reason, GitSyncPauseReason::BranchPaused);
        assert!(pause
            .detail
            .is_some_and(|detail| detail.contains("rebase") && detail.contains("aborted")));
        let repository = engine.discover_repository(&writer).expect("repository");
        assert!(
            !repository.git_dir.join("rebase-merge").exists()
                && !repository.git_dir.join("rebase-apply").exists(),
            "an unattended checkout must not be left mid-rebase"
        );
        assert_eq!(
            GitOid::parse(git_stdout(&writer, &["rev-parse", "HEAD"])).expect("head"),
            local_before_pull
        );
        assert_eq!(
            fs::read_to_string(writer.join("Home.md")).expect("local note"),
            "local conflict\n"
        );
        assert!(git_stdout(&writer, &["status", "--porcelain"])
            .trim()
            .is_empty());
    }

    #[test]
    fn committed_history_only_fails_when_the_branch_lane_fails() {
        let (temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");
        advance_remote_branch(&temporary, &remote, "advanced\n");
        run_git(&writer, &["config", "pull.ff", "bogus"]);

        let error = sync_git_once(&engine, &writer, &commits_only())
            .expect_err("a failed branch lane fails the cycle");

        assert!(error.to_string().contains("pull.ff"), "{error}");
        assert!(classify_git_sync_error(&error).retryable);
    }

    #[test]
    fn sync_requires_an_upstream() {
        let (_temporary, remote, writer) = setup_remote_and_writer();
        let engine = GitCliEngine::default();

        let error = sync_git_once(&engine, &writer, &commits_only())
            .expect_err("nothing to synchronize without an upstream");

        assert!(
            matches!(&error, GitSyncError::Git(GitEngineError::UnsupportedRepository { detail })
                if detail.contains("upstream") && detail.contains("no upstream")),
            "{error}"
        );
        assert!(remote_ref_tip(&remote, DEFAULT_REMOTE_LIVE_REF).is_empty());
    }

    #[test]
    fn committed_history_only_preview_skips_live() {
        let (_temporary, _remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");

        let report = sync_git_once(
            &engine,
            &writer,
            &GitSyncOptions {
                dry_run: true,
                ..commits_only()
            },
        )
        .expect("preview");

        assert_eq!(report.outcome, GitSyncOutcome::Planned);
        assert_eq!(branch_action(&report).0, GitBranchSyncAction::Planned);
    }

    #[test]
    fn disabled_branch_push_keeps_local_commits_local() {
        let (_temporary, remote, writer) = setup_tracked_branch();
        let engine = GitCliEngine::default();
        let published = remote_ref_tip(&remote, "refs/heads/main");
        fs::write(writer.join("Local.md"), "local\n").expect("local note");
        commit_all(&writer, "local");

        let report = sync_git_once(
            &engine,
            &writer,
            &GitSyncOptions {
                branch_push: false,
                ..GitSyncOptions::default()
            },
        )
        .expect("sync");

        assert!(report.branch.as_ref().is_some_and(|lane| !lane.pushed));
        assert_eq!(remote_ref_tip(&remote, "refs/heads/main"), published);
    }

    #[test]
    fn switching_branches_at_the_same_commit_pauses_after_capture() {
        let (_temporary, _remote, writer) = setup_tracked_branch();
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
}
