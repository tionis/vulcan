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

mod contracts;
mod git;
mod merge_policy;
mod structured_merge;
mod sync;

pub use contracts::{
    IgnoreSyncProgress, SyncAction, SyncBackend, SyncCapabilities, SyncCapability, SyncConflict,
    SyncContext, SyncError, SyncErrorCategory, SyncJob, SyncJobState, SyncJobTrigger, SyncObserver,
    SyncOperation, SyncOperationMode, SyncOutcome, SyncPlan, SyncProgress, SyncReport,
    SyncResolutionState, SyncState, SyncStatus, SYNC_CONTRACT_VERSION,
};

pub use git::{
    GitCapture, GitCaptureRequest, GitCaseRenamePolicy, GitCliEngine, GitCloneRequest,
    GitConflictSide, GitContentMergeResolutionRequest, GitDetachedRecoveryReport,
    GitDetachedRecoveryRequest, GitEngine, GitEngineError, GitEngineKind, GitExecutableBitsPolicy,
    GitFilterRequirement, GitInstallation, GitMerge, GitMergeResolutionRequest, GitObjectFormat,
    GitOid, GitPathLengthPolicy, GitPathObject, GitPlatformPolicy, GitPlatformProfile,
    GitPushResult, GitRefCreateResult, GitRefDeleteResult, GitRefName, GitRefUpdateResult,
    GitRemote, GitRepository, GitRepositoryLayout, GitRepositoryRequirements,
    GitReservedNamesPolicy, GitResolvedPath, GitSafetyState, GitSymlinkPolicy, GitTimestampPolicy,
    GitTreeApplyAction, GitTreeApplyPath, GitTreeApplyPlan, GitVersion,
};
pub use merge_policy::{
    MergeAutomation, MergeFileKind, MergePathSelector, MergePolicy, MergePolicyDecision,
    MergePolicyError, MergePolicyRule, MergeResolution, MERGE_POLICY_SCHEMA_VERSION,
};
pub use sync::{
    sync_git_once, sync_git_once_with_control, GitAutomaticMergeValidation, GitAutomaticResolution,
    GitAutomaticResolutionValidation, GitAutomaticValidationCheck, GitConflictClass,
    GitConflictClassification, GitConflictRefs, GitSyncAction, GitSyncBackend, GitSyncConflict,
    GitSyncDeviceId, GitSyncError, GitSyncObserver, GitSyncObserverError, GitSyncOptions,
    GitSyncOutcome, GitSyncPause, GitSyncPauseReason, GitSyncPhase, GitSyncProgress, GitSyncRefs,
    GitSyncReport, IgnoreGitSyncProgress, SyncCancellationToken,
};
