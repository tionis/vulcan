//! Backend-neutral contracts shared by direct, daemon, and companion clients.

use crate::SyncCancellationToken;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

pub const SYNC_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncOperationMode {
    Finite,
    Continuous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncCapabilities {
    pub operation_modes: Vec<SyncOperationMode>,
    pub features: Vec<SyncCapability>,
}

impl SyncCapabilities {
    #[must_use]
    pub fn supports(&self, capability: SyncCapability) -> bool {
        self.features.contains(&capability)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncCapability {
    Fetch,
    Push,
    SafePause,
    SafeCancel,
    Progress,
    RemoteRevision,
    OfflineRecovery,
    ConflictPreservation,
    DetachedGitDirectory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncOperation {
    Capture,
    Fetch,
    Merge,
    Push,
    Apply,
    Verify,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncPlan {
    pub version: u32,
    pub backend: String,
    pub vault: PathBuf,
    pub dry_run: bool,
    pub capabilities: SyncCapabilities,
    pub operations: Vec<SyncOperation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    Clean,
    Dirty,
    CapturePending,
    Capturing,
    CapturedUnpushed,
    Fetching,
    Fetched,
    Merging,
    Pushing,
    Applying,
    Conflicted,
    Paused,
    Offline,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStatus {
    pub state: SyncState,
    pub backend: String,
    pub vault: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_revision: Option<String>,
    pub unresolved_conflicts: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncResolutionState {
    Unresolved,
    Proposed,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflict {
    pub id: String,
    pub paths: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
    pub local_revision: String,
    pub remote_revision: String,
    pub policy_version: u32,
    pub resolution: SyncResolutionState,
    pub preserved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncOutcome {
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
pub enum SyncAction {
    SnapshotCreated,
    Pushed,
    WorktreeApplied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    pub version: u32,
    pub backend: String,
    pub dry_run: bool,
    pub outcome: SyncOutcome,
    pub status: SyncStatus,
    pub actions: Vec<SyncAction>,
    pub attempts: usize,
    pub conflicts: Vec<SyncConflict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncJobTrigger {
    Manual,
    Poll,
    Watch,
    RemoteNotification,
    Resume,
    Recovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncJobState {
    Queued,
    Running,
    Succeeded,
    Conflicted,
    Paused,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncJob {
    pub version: u32,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wiki_id: Option<String>,
    pub backend: String,
    pub vault: PathBuf,
    pub trigger: SyncJobTrigger,
    pub state: SyncJobState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SyncStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<SyncError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncErrorCategory {
    Configuration,
    Authentication,
    Network,
    Repository,
    Conflict,
    Cancelled,
    Busy,
    Invariant,
    Io,
    Observer,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncError {
    pub category: SyncErrorCategory,
    pub message: String,
    pub retryable: bool,
}

impl SyncError {
    #[must_use]
    pub fn new(category: SyncErrorCategory, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            category,
            message: message.into(),
            retryable,
        }
    }
}

impl Display for SyncError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SyncError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncProgress {
    pub backend: String,
    pub vault: PathBuf,
    pub state: SyncState,
    pub attempt: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_revision: Option<String>,
}

pub trait SyncObserver: Send + Sync {
    fn progress(&self, progress: &SyncProgress) -> Result<(), SyncError>;
}

#[derive(Debug, Default)]
pub struct IgnoreSyncProgress;

impl SyncObserver for IgnoreSyncProgress {
    fn progress(&self, _progress: &SyncProgress) -> Result<(), SyncError> {
        Ok(())
    }
}

pub struct SyncContext<'a> {
    pub vault_path: &'a Path,
    pub dry_run: bool,
    pub observer: &'a dyn SyncObserver,
}

impl<'a> SyncContext<'a> {
    #[must_use]
    pub fn new(vault_path: &'a Path, dry_run: bool, observer: &'a dyn SyncObserver) -> Self {
        Self {
            vault_path,
            dry_run,
            observer,
        }
    }
}

pub trait SyncBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> SyncCapabilities;
    fn plan(&self, context: &SyncContext<'_>) -> Result<SyncPlan, SyncError>;
    fn sync_once(
        &self,
        context: &SyncContext<'_>,
        cancellation: &SyncCancellationToken,
    ) -> Result<SyncReport, SyncError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_contracts_have_stable_json_discriminants() {
        let status = SyncStatus {
            state: SyncState::CapturedUnpushed,
            backend: "git".to_string(),
            vault: PathBuf::from("/vault"),
            local_revision: Some("abc".to_string()),
            remote_revision: None,
            accepted_revision: None,
            unresolved_conflicts: 0,
            detail: None,
        };
        let value = serde_json::to_value(status).expect("serialize status");
        assert_eq!(value["state"], "captured_unpushed");
        assert_eq!(value["backend"], "git");
        assert!(value.get("remote_revision").is_none());
        assert_eq!(
            [SyncState::Capturing, SyncState::Fetched, SyncState::Pushing,]
                .into_iter()
                .map(|state| serde_json::to_value(state).expect("serialize state"))
                .collect::<Vec<_>>(),
            ["capturing", "fetched", "pushing"]
        );

        let error = SyncError::new(SyncErrorCategory::Network, "offline", true);
        let value = serde_json::to_value(error).expect("serialize error");
        assert_eq!(value["category"], "network");
        assert_eq!(value["retryable"], true);
    }
}
