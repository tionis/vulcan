//! Synchronous device and file-tree synchronization primitives.
//!
//! This crate owns backend-neutral synchronization contracts and the Git
//! repository engine used by the first sync backend. Long-running scheduling,
//! HTTP transports, and watcher ownership belong in `vulcan-daemon`; complete
//! vault transactions belong in `vulcan-app`.
//!
//! The initial Git engine deliberately uses the installed Git CLI. Its public
//! API exposes typed operations rather than arbitrary Git arguments so a later
//! embedded engine can implement the same contract and conformance suite.

pub mod conformance;
mod contracts;
mod git;
mod lock;
mod merge_policy;
mod platform;
pub mod process;
mod racy_index;
mod refs;

mod sync;
mod tree_ignore;

pub use contracts::{
    IgnoreSyncProgress, SyncAction, SyncBackend, SyncCapabilities, SyncCapability, SyncConflict,
    SyncContext, SyncError, SyncErrorCategory, SyncJob, SyncJobState, SyncJobTrigger, SyncObserver,
    SyncOperation, SyncOperationMode, SyncOutcome, SyncPlan, SyncProgress, SyncReport,
    SyncResolutionState, SyncState, SyncStatus, SYNC_CONTRACT_VERSION,
};

pub use git::{
    vault_path_prefix, BranchAdvanceOutcome, BranchPullConfig, BranchReplayConflict,
    BranchReplayOutcome, BranchReplayResolver, FastForwardOutcome, GitBranchUpstream, GitCapture,
    GitCaptureRequest, GitCaseRenamePolicy, GitCliEngine, GitCloneRequest,
    GitDetachedRecoveryReport, GitDetachedRecoveryRequest, GitEngine, GitEngineError,
    GitEngineKind, GitExecutableBitsPolicy, GitFilterRequirement, GitInstallation, GitMerge,
    GitObjectFormat, GitOid, GitPathLengthPolicy, GitPathObject, GitPlatformPolicy,
    GitPlatformProfile, GitPushResult, GitRefCreateResult, GitRefDeleteResult, GitRefName,
    GitRefUpdateResult, GitReference, GitRemote, GitRemoteRefs, GitRepository, GitRepositoryLayout,
    GitRepositoryRequirements, GitReservedNamesPolicy, GitSafetyState, GitSymlinkPolicy,
    GitTimestampPolicy, GitTreeEntry, GitUnattendedRepositoryState, GitVersion, MergeBranchOutcome,
    PreparedBranchReplay, PullFastForward, PullRebase, RebaseHazard, RebaseOutcome,
};
pub use lock::{RepositoryLock, RepositoryLockError};
pub use merge_policy::{
    MergeAutomation, MergeFileKind, MergePathSelector, MergePolicy, MergePolicyDecision,
    MergePolicyError, MergePolicyRule, MergeResolution, MERGE_POLICY_SCHEMA_VERSION,
};
pub use platform::{
    inspect_git_tree_platform, GitPlatformDiagnostic, GitPlatformDiagnosticSeverity,
    GitPlatformPreflight, GIT_PLATFORM_PREFLIGHT_VERSION,
};
pub use refs::{
    conflict_archive_ref, conflict_ref, detached_recovery_ref, device_recovery_live_ref,
    device_recovery_ref, local_recovery_ref_namespaces, local_sync_ref,
    remote_conflict_proposal_resolution_ref, remote_conflict_ref, remote_device_ref,
    remote_epoch_ref, remote_registration_ref, sync_profile_key, DEFAULT_REMOTE_LIVE_REF,
    LOCAL_FORGE_DESCRIPTOR_MIRROR_REF, LOCAL_RECOVERY_REF_NAMESPACES,
    LOCAL_REGISTRATION_MIRROR_ROOT, LOCAL_VULCAN_REF_ROOT, REMOTE_CONFLICT_BRANCH_ROOT,
    REMOTE_DEVICE_BRANCH_ROOT, REMOTE_EPOCH_BRANCH_ROOT, REMOTE_FORGE_DESCRIPTOR_REF,
    REMOTE_REGISTRATION_BRANCH_ROOT, VULCAN_REF_NAMESPACE_VERSION,
};

pub use sync::{
    classify_git_sync_error, git_published_branch_tip, sync_git_once, sync_git_once_with_control,
    BranchPullPolicy, GitBranchPause, GitBranchSync, GitBranchSyncAction, GitConflictClass,
    GitConflictClassification, GitConflictProjection, GitConflictRefs, GitConflictScope,
    GitDeviceBackup, GitDeviceBackupOutcome, GitSyncAction, GitSyncBackend, GitSyncConflict,
    GitSyncDeviceId, GitSyncDeviceIdKind, GitSyncError, GitSyncObserver, GitSyncObserverError,
    GitSyncOptions, GitSyncOutcome, GitSyncPause, GitSyncPauseReason, GitSyncPhase,
    GitSyncProgress, GitSyncRefs, GitSyncReport, IgnoreGitSyncProgress, SyncCancellationToken,
};
pub use tree_ignore::{tree_ignores, worktree_ignores, OBSIDIAN_DEVICE_FILES};
