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

pub use git::{
    GitCliEngine, GitEngine, GitEngineError, GitEngineKind, GitInstallation, GitObjectFormat,
    GitRepository, GitRepositoryLayout, GitVersion,
};
