//! Transport-neutral operations for local companion clients.
//!
//! HTTP, WebSocket, CLI, and mobile bridges should project these typed reports
//! instead of implementing synchronization or Git behavior themselves.

use crate::registry::{
    RegistryError, UpdateWikiRequest, WikiId, WikiRegistration, WikiRegistrationStatus,
    WikiRegistry,
};
use crate::status::{wiki_sync_status, DaemonSyncStatusError, DaemonWikiSyncStatus};
use crate::supervisor::{
    IdempotentEnqueueSyncReport, SupervisedSyncJob, SupervisorError, SyncSupervisor,
};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{Display, Formatter};
use vulcan_app::sync_conflicts::{
    get_sync_conflict_with_state_store, list_sync_conflicts_with_state_store,
    resolve_sync_conflict_with_state_store, ResolveSyncConflictOptions, ResolveSyncConflictReport,
    SyncConflictDetailReport, SyncConflictListReport, SyncConflictResolutionSide,
};
use vulcan_app::sync_semantic::{
    create_semantic_plan_with_state_store, SemanticPlanOptions, SemanticPlanReport,
};
use vulcan_app::sync_state::SyncStateStore;
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_sync::{GitRefName, GitRemote, SyncJobTrigger, SYNC_CONTRACT_VERSION};

pub const COMPANION_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompanionOperation {
    Capabilities,
    WikiList,
    SyncEnqueue,
    SyncStatus,
    SyncPause,
    SyncResume,
    ConflictList,
    ConflictDetail,
    ConflictResolve,
    SemanticPlanCreate,
    JobStatus,
    JobCancel,
    EventSubscribe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompanionCapabilities {
    pub protocol_version: u32,
    pub sync_contract_version: u32,
    pub operations: Vec<CompanionOperation>,
    pub transports: Vec<String>,
    pub sync_backends: Vec<String>,
    pub conflict_resolution_sides: Vec<SyncConflictResolutionSide>,
    pub agent_conflict_proposals: bool,
    pub agent_semantic_plans: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConflictResolveRequest {
    pub side: SyncConflictResolutionSide,
    #[serde(default = "default_remote")]
    pub remote: String,
    #[serde(default = "default_live_ref")]
    pub live_ref: String,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SemanticPlanRequest {
    pub from: String,
    pub to: String,
    pub semantic_ref: String,
    #[serde(default = "default_remote")]
    pub remote: String,
    #[serde(default = "default_live_ref")]
    pub live_ref: String,
    #[serde(default)]
    pub agent: bool,
    #[serde(default)]
    pub dry_run: bool,
}

fn default_remote() -> String {
    "origin".to_string()
}

fn default_live_ref() -> String {
    "refs/heads/__vulcan-sync/live".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompanionErrorKind {
    InvalidRequest,
    NotFound,
    PermissionDenied,
    Conflict,
    Internal,
}

#[derive(Debug, Serialize)]
pub struct CompanionError {
    pub version: u32,
    pub kind: CompanionErrorKind,
    pub detail: String,
}

impl CompanionError {
    #[must_use]
    pub fn new(kind: CompanionErrorKind, detail: impl Into<String>) -> Self {
        Self {
            version: COMPANION_PROTOCOL_VERSION,
            kind,
            detail: detail.into(),
        }
    }
}

impl Display for CompanionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl Error for CompanionError {}

pub struct CompanionService<'a> {
    registry: &'a WikiRegistry,
    supervisor: &'a SyncSupervisor,
    state_store: &'a SyncStateStore,
}

impl<'a> CompanionService<'a> {
    #[must_use]
    pub const fn new(
        registry: &'a WikiRegistry,
        supervisor: &'a SyncSupervisor,
        state_store: &'a SyncStateStore,
    ) -> Self {
        Self {
            registry,
            supervisor,
            state_store,
        }
    }

    #[must_use]
    pub fn capabilities(&self) -> CompanionCapabilities {
        CompanionCapabilities {
            protocol_version: COMPANION_PROTOCOL_VERSION,
            sync_contract_version: SYNC_CONTRACT_VERSION,
            operations: vec![
                CompanionOperation::Capabilities,
                CompanionOperation::WikiList,
                CompanionOperation::SyncEnqueue,
                CompanionOperation::SyncStatus,
                CompanionOperation::SyncPause,
                CompanionOperation::SyncResume,
                CompanionOperation::ConflictList,
                CompanionOperation::ConflictDetail,
                CompanionOperation::ConflictResolve,
                CompanionOperation::SemanticPlanCreate,
                CompanionOperation::JobStatus,
                CompanionOperation::JobCancel,
            ],
            transports: Vec::new(),
            sync_backends: vec!["git".to_string()],
            conflict_resolution_sides: vec![
                SyncConflictResolutionSide::Base,
                SyncConflictResolutionSide::Local,
                SyncConflictResolutionSide::Remote,
            ],
            agent_conflict_proposals: false,
            agent_semantic_plans: false,
        }
    }

    pub fn list_wikis(
        &self,
        group: Option<&str>,
    ) -> Result<Vec<WikiRegistrationStatus>, CompanionError> {
        self.registry.list(group).map_err(map_registry_error)
    }

    pub fn sync_status(&self, wiki_id: &WikiId) -> Result<DaemonWikiSyncStatus, CompanionError> {
        wiki_sync_status(self.registry, self.supervisor, self.state_store, wiki_id)
            .map_err(map_status_error)
    }

    pub fn enqueue_sync(
        &self,
        wiki_id: &WikiId,
        credential_scope: &str,
        idempotency_key: &str,
    ) -> Result<IdempotentEnqueueSyncReport, CompanionError> {
        let registration = self.registration(wiki_id)?;
        self.supervisor
            .enqueue_idempotent(
                credential_scope,
                idempotency_key,
                wiki_id.as_str(),
                registration.path,
                SyncJobTrigger::Manual,
            )
            .map_err(map_supervisor_error)
    }

    pub fn pause_sync(&self, wiki_id: &WikiId) -> Result<WikiRegistration, CompanionError> {
        self.set_paused(wiki_id, true)
    }

    pub fn resume_sync(
        &self,
        wiki_id: &WikiId,
        credential_scope: &str,
        idempotency_key: &str,
    ) -> Result<IdempotentEnqueueSyncReport, CompanionError> {
        let registration = self.set_paused(wiki_id, false)?;
        self.supervisor
            .enqueue_idempotent(
                credential_scope,
                idempotency_key,
                wiki_id.as_str(),
                registration.path,
                SyncJobTrigger::Resume,
            )
            .map_err(map_supervisor_error)
    }

    pub fn list_conflicts(
        &self,
        wiki_id: &WikiId,
    ) -> Result<SyncConflictListReport, CompanionError> {
        let registration = self.checked_git_registration(wiki_id)?;
        list_sync_conflicts_with_state_store(&VaultPaths::new(registration.path), self.state_store)
            .map_err(map_app_error)
    }

    pub fn conflict_detail(
        &self,
        wiki_id: &WikiId,
        conflict_id: &str,
    ) -> Result<SyncConflictDetailReport, CompanionError> {
        let registration = self.checked_git_registration(wiki_id)?;
        get_sync_conflict_with_state_store(
            &VaultPaths::new(registration.path),
            conflict_id,
            self.state_store,
        )
        .map_err(map_app_error)
    }

    pub fn resolve_conflict(
        &self,
        wiki_id: &WikiId,
        conflict_id: &str,
        request: &ConflictResolveRequest,
    ) -> Result<ResolveSyncConflictReport, CompanionError> {
        let registration = self.checked_git_registration(wiki_id)?;
        resolve_sync_conflict_with_state_store(
            &VaultPaths::new(registration.path),
            conflict_id,
            &ResolveSyncConflictOptions {
                side: request.side,
                remote: GitRemote::parse(&request.remote)
                    .map_err(|error| invalid_request(error.to_string()))?,
                live_ref: GitRefName::parse(&request.live_ref)
                    .map_err(|error| invalid_request(error.to_string()))?,
                dry_run: request.dry_run,
            },
            self.state_store,
        )
        .map_err(map_app_error)
    }

    pub fn create_semantic_plan(
        &self,
        wiki_id: &WikiId,
        request: &SemanticPlanRequest,
    ) -> Result<SemanticPlanReport, CompanionError> {
        let registration = self.checked_git_registration(wiki_id)?;
        create_semantic_plan_with_state_store(
            &VaultPaths::new(registration.path),
            &SemanticPlanOptions {
                from: request.from.clone(),
                to: request.to.clone(),
                semantic_ref: GitRefName::parse(&request.semantic_ref)
                    .map_err(|error| invalid_request(error.to_string()))?,
                remote: GitRemote::parse(&request.remote)
                    .map_err(|error| invalid_request(error.to_string()))?,
                live_ref: GitRefName::parse(&request.live_ref)
                    .map_err(|error| invalid_request(error.to_string()))?,
                agent: request.agent,
                dry_run: request.dry_run,
            },
            self.state_store,
        )
        .map_err(map_app_error)
    }

    pub fn job(&self, job_id: &str) -> Result<SupervisedSyncJob, CompanionError> {
        self.supervisor
            .get(job_id)
            .map_err(map_supervisor_error)?
            .ok_or_else(|| {
                CompanionError::new(
                    CompanionErrorKind::NotFound,
                    format!("unknown synchronization job `{job_id}`"),
                )
            })
    }

    pub fn cancel_job(&self, job_id: &str) -> Result<SupervisedSyncJob, CompanionError> {
        self.supervisor.cancel(job_id).map_err(map_supervisor_error)
    }

    fn set_paused(
        &self,
        wiki_id: &WikiId,
        paused: bool,
    ) -> Result<WikiRegistration, CompanionError> {
        self.registration(wiki_id)?;
        self.registry
            .update(
                wiki_id,
                &UpdateWikiRequest {
                    groups_to_add: Vec::new(),
                    groups_to_remove: Vec::new(),
                    permissions_profile: None,
                    sync_paused: Some(paused),
                },
                false,
            )
            .map_err(map_registry_error)
    }

    fn registration(&self, wiki_id: &WikiId) -> Result<WikiRegistration, CompanionError> {
        self.registry
            .show(wiki_id)
            .map(|status| status.registration)
            .map_err(map_registry_error)
    }

    fn checked_git_registration(
        &self,
        wiki_id: &WikiId,
    ) -> Result<WikiRegistration, CompanionError> {
        let registration = self.registration(wiki_id)?;
        if registration
            .sync_backend
            .as_deref()
            .is_some_and(|backend| backend != "git")
        {
            return Err(invalid_request(format!(
                "wiki `{wiki_id}` uses unsupported sync backend `{}`",
                registration.sync_backend.as_deref().unwrap_or_default()
            )));
        }
        let paths = VaultPaths::new(&registration.path);
        let selection =
            resolve_permission_profile(&paths, registration.permissions_profile.as_deref())
                .map_err(|error| {
                    CompanionError::new(CompanionErrorKind::PermissionDenied, error.to_string())
                })?;
        ProfilePermissionGuard::new(&paths, selection)
            .check_git()
            .map_err(|error| {
                CompanionError::new(CompanionErrorKind::PermissionDenied, error.to_string())
            })?;
        Ok(registration)
    }
}

fn invalid_request(detail: impl Into<String>) -> CompanionError {
    CompanionError::new(CompanionErrorKind::InvalidRequest, detail)
}

fn map_registry_error(error: RegistryError) -> CompanionError {
    let kind = if matches!(error, RegistryError::UnknownWiki(_)) {
        CompanionErrorKind::NotFound
    } else {
        CompanionErrorKind::InvalidRequest
    };
    let detail = error.to_string();
    drop(error);
    CompanionError::new(kind, detail)
}

fn map_supervisor_error(error: SupervisorError) -> CompanionError {
    let kind = match error {
        SupervisorError::UnknownJob(_) => CompanionErrorKind::NotFound,
        SupervisorError::InvalidState(_) => CompanionErrorKind::Conflict,
        SupervisorError::Io(_) | SupervisorError::Json(_) | SupervisorError::Poisoned => {
            CompanionErrorKind::Internal
        }
    };
    let detail = error.to_string();
    drop(error);
    CompanionError::new(kind, detail)
}

fn map_status_error(error: DaemonSyncStatusError) -> CompanionError {
    match error {
        DaemonSyncStatusError::Registry(error) => map_registry_error(error),
        DaemonSyncStatusError::Supervisor(error) => map_supervisor_error(error),
        DaemonSyncStatusError::State(detail) => {
            CompanionError::new(CompanionErrorKind::Internal, detail)
        }
    }
}

fn map_app_error(error: vulcan_app::AppError) -> CompanionError {
    let detail = error.to_string();
    drop(error);
    CompanionError::new(CompanionErrorKind::Conflict, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::AddWikiRequest;
    use serde_json::json;
    use tempfile::tempdir;

    fn fixture(
        temporary: &tempfile::TempDir,
    ) -> (WikiRegistry, SyncSupervisor, SyncStateStore, WikiId) {
        let vault = temporary.path().join("vault");
        std::fs::create_dir(&vault).expect("vault directory");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let wiki_id = WikiId::parse("notes").expect("wiki id");
        registry
            .add(
                &AddWikiRequest {
                    id: wiki_id.clone(),
                    path: vault,
                    groups: vec!["personal".to_string()],
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("register wiki");
        let supervisor =
            SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor");
        let state_store = SyncStateStore::at(temporary.path().join("sync-state"));
        (registry, supervisor, state_store, wiki_id)
    }

    #[test]
    fn capabilities_are_versioned_and_do_not_overpromise_agent_workflows() {
        let temporary = tempdir().expect("temporary directory");
        let (registry, supervisor, state_store, _) = fixture(&temporary);
        let service = CompanionService::new(&registry, &supervisor, &state_store);
        let value = serde_json::to_value(service.capabilities()).expect("serialize capabilities");

        assert_eq!(value["protocol_version"], json!(1));
        assert_eq!(value["sync_contract_version"], json!(1));
        assert_eq!(value["agent_conflict_proposals"], json!(false));
        assert_eq!(value["agent_semantic_plans"], json!(false));
        assert_eq!(value["transports"], json!([]));
        assert!(value["operations"]
            .as_array()
            .expect("operations")
            .contains(&json!("sync_enqueue")));
        assert!(!value["operations"]
            .as_array()
            .expect("operations")
            .contains(&json!("event_subscribe")));
    }

    #[test]
    fn wiki_state_and_manual_jobs_share_one_service_boundary() {
        let temporary = tempdir().expect("temporary directory");
        let (registry, supervisor, state_store, wiki_id) = fixture(&temporary);
        let service = CompanionService::new(&registry, &supervisor, &state_store);

        assert_eq!(
            service.list_wikis(Some("personal")).expect("wikis").len(),
            1
        );
        assert_eq!(
            service.sync_status(&wiki_id).expect("status").status.state,
            vulcan_sync::SyncState::Clean
        );
        let first = service
            .enqueue_sync(&wiki_id, "credential-a", "request-1")
            .expect("enqueue");
        let replay = service
            .enqueue_sync(&wiki_id, "credential-a", "request-1")
            .expect("replay");
        assert!(!first.replay);
        assert!(replay.replay);
        assert_eq!(first.enqueue.job.job.id, replay.enqueue.job.job.id);
        assert_eq!(
            service
                .job(&first.enqueue.job.job.id)
                .expect("job")
                .job
                .wiki_id
                .as_deref(),
            Some("notes")
        );
        assert_eq!(
            service
                .cancel_job(&first.enqueue.job.job.id)
                .expect("cancel")
                .job
                .state,
            vulcan_sync::SyncJobState::Cancelled
        );
    }

    #[test]
    fn pause_is_idempotent_and_resume_queues_a_scoped_job() {
        let temporary = tempdir().expect("temporary directory");
        let (registry, supervisor, state_store, wiki_id) = fixture(&temporary);
        let service = CompanionService::new(&registry, &supervisor, &state_store);

        assert!(service.pause_sync(&wiki_id).expect("pause").sync_paused);
        assert!(
            service
                .pause_sync(&wiki_id)
                .expect("pause again")
                .sync_paused
        );
        let resumed = service
            .resume_sync(&wiki_id, "credential-a", "resume-1")
            .expect("resume");
        assert!(
            !registry
                .show(&wiki_id)
                .expect("wiki")
                .registration
                .sync_paused
        );
        assert_eq!(resumed.enqueue.job.triggers, vec![SyncJobTrigger::Resume]);
        assert!(
            service
                .resume_sync(&wiki_id, "credential-a", "resume-1")
                .expect("resume replay")
                .replay
        );
    }

    #[test]
    fn unknown_resources_have_stable_not_found_errors() {
        let temporary = tempdir().expect("temporary directory");
        let (registry, supervisor, state_store, _) = fixture(&temporary);
        let service = CompanionService::new(&registry, &supervisor, &state_store);

        assert_eq!(
            serde_json::to_value(service.job("missing").expect_err("missing job"))
                .expect("serialize error"),
            json!({
                "version": 1,
                "kind": "not_found",
                "detail": "unknown synchronization job `missing`"
            })
        );
        assert_eq!(
            service
                .sync_status(&WikiId::parse("missing").expect("wiki id"))
                .expect_err("missing wiki")
                .kind,
            CompanionErrorKind::NotFound
        );
    }
}
