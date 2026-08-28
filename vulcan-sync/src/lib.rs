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

mod git;
mod sync;

pub use git::{
    GitCapture, GitCaptureRequest, GitCaseRenamePolicy, GitCliEngine, GitCloneRequest, GitEngine,
    GitEngineError, GitEngineKind, GitExecutableBitsPolicy, GitInstallation, GitMerge,
    GitObjectFormat, GitOid, GitPathLengthPolicy, GitPlatformPolicy, GitPlatformProfile,
    GitPushResult, GitRefName, GitRemote, GitRepository, GitRepositoryLayout,
    GitReservedNamesPolicy, GitSafetyState, GitSymlinkPolicy, GitTimestampPolicy, GitVersion,
};
pub use sync::{
    sync_git_once, GitSyncAction, GitSyncConflict, GitSyncError, GitSyncOptions, GitSyncOutcome,
    GitSyncRefs, GitSyncReport,
};
