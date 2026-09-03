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
mod merge_policy;
mod notifications;
mod platform;
mod refs;
mod structured_merge;
mod sync;

pub use contracts::{
    IgnoreSyncProgress, SyncAction, SyncBackend, SyncCapabilities, SyncCapability, SyncConflict,
    SyncContext, SyncError, SyncErrorCategory, SyncJob, SyncJobState, SyncJobTrigger, SyncObserver,
    SyncOperation, SyncOperationMode, SyncOutcome, SyncPlan, SyncProgress, SyncReport,
    SyncResolutionState, SyncState, SyncStatus, SYNC_CONTRACT_VERSION,
};

pub use git::{
    CommitSigning, GitCapture, GitCaptureRequest, GitCaseRenamePolicy, GitChange, GitChangeKind,
    GitCliEngine, GitCloneRequest, GitCommitMetadata, GitConflictSide,
    GitContentMergeResolutionRequest, GitDetachedRecoveryReport, GitDetachedRecoveryRequest,
    GitEngine, GitEngineError, GitEngineKind, GitExecutableBitsPolicy, GitFilterRequirement,
    GitInstallation, GitMerge, GitMergeResolutionRequest, GitObjectFormat, GitOid,
    GitPathLengthPolicy, GitPathObject, GitPlatformPolicy, GitPlatformProfile, GitPushResult,
    GitRefCreateResult, GitRefDeleteResult, GitRefName, GitRefUpdateResult, GitReference,
    GitRemote, GitRepository, GitRepositoryLayout, GitRepositoryRequirements,
    GitReservedNamesPolicy, GitResolvedPath, GitSafetyState, GitSymlinkPolicy, GitTimestampPolicy,
    GitTreeApplyAction, GitTreeApplyPath, GitTreeApplyPlan, GitTreeEntry, GitVersion,
};
pub use merge_policy::{
    MergeAutomation, MergeFileKind, MergePathSelector, MergePolicy, MergePolicyDecision,
    MergePolicyError, MergePolicyRule, MergeResolution, MERGE_POLICY_SCHEMA_VERSION,
};
pub use notifications::{
    preview_notification_advertisement, publish_notification_advertisement,
    refresh_notification_advertisement, remove_notification_advertisement,
    DiscoveredNotificationAdvertisement, NotificationAdvertisement, NotificationAdvertisementError,
    NotificationEndpoint, NotificationTransport, NOTIFICATION_ADVERTISEMENT_FILE,
    NOTIFICATION_ADVERTISEMENT_REF,
};
pub use platform::{
    inspect_git_tree_platform, GitPlatformDiagnostic, GitPlatformDiagnosticSeverity,
    GitPlatformPreflight, GIT_PLATFORM_PREFLIGHT_VERSION,
};
pub use refs::{
    checkpoint_ref, conflict_proposal_resolution_ref, conflict_recovery_ref, conflict_ref,
    conflict_resolved_ref, detached_recovery_ref, local_epoch_ref, local_recovery_ref_namespaces,
    local_sync_ref, remote_epoch_ref, semantic_proposal_ref, sync_profile_key,
    DEFAULT_REMOTE_LIVE_REF, LOCAL_RECOVERY_REF_NAMESPACES, LOCAL_VULCAN_REF_ROOT,
    REMOTE_EPOCH_BRANCH_ROOT, VULCAN_REF_NAMESPACE_VERSION,
};
pub use sync::{
    find_git_live_epoch, git_live_epoch_id, sync_git_once, sync_git_once_with_control,
    GitAutomaticMergeValidation, GitAutomaticResolution, GitAutomaticResolutionValidation,
    GitAutomaticValidationCheck, GitConflictClass, GitConflictClassification, GitConflictCopy,
    GitConflictMaterialization, GitConflictRefs, GitLiveEpoch, GitRemoteObservation, GitSyncAction,
    GitSyncBackend, GitSyncConflict, GitSyncDeviceId, GitSyncError, GitSyncObserver,
    GitSyncObserverError, GitSyncOptions, GitSyncOutcome, GitSyncPause, GitSyncPauseReason,
    GitSyncPhase, GitSyncProgress, GitSyncRefs, GitSyncReport, IgnoreGitSyncProgress,
    SyncCancellationToken,
};
