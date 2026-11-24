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
    ActivityOutcome, AggregateSyncJob, IdempotentEnqueueAggregateSyncReport,
    IdempotentEnqueueSyncReport, SupervisedSyncJob, SupervisorError, SyncSupervisor,
};
use crate::sync::{enqueue_registered_wikis, RegisteredSyncError, RegisteredSyncSelection};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{Display, Formatter};
use vulcan_app::sync_conflicts::{
    get_sync_conflict_page_with_state_store, get_sync_conflict_with_state_store,
    list_sync_conflicts_with_state_store, SyncConflictDetailReport, SyncConflictListReport,
};
use vulcan_app::sync_proposals::ResolutionAgentProvider;
use vulcan_app::sync_semantic::SemanticAgentProvider;
use vulcan_app::sync_state::SyncStateStore;
use vulcan_core::{
    resolve_device_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_sync::{SyncJobTrigger, SYNC_CONTRACT_VERSION};

pub const COMPANION_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompanionOperation {
    Capabilities,
    WikiList,
    SyncEnqueue,
    SyncSelectionEnqueue,
    SyncStatus,
    SyncPause,
    SyncResume,
    /// `POST /{id}/sync/activity`: someone is about to use the wiki.
    SyncActivity,
    ConflictList,
    ConflictDetail,
    JobStatus,
    JobCancel,
    AggregateJobStatus,
    AggregateJobCancel,
    EventSubscribe,
    DaemonShutdown,
    /// `POST /{id}/ops/query`: a routed read (Roadmap 10.5).
    RoutedQuery,
    /// `POST /{id}/ops/search`: a routed keyword search.
    RoutedSearch,
    /// `POST /{id}/ops/ls`: a routed note listing.
    RoutedLs,
    /// `POST /{id}/ops/dql`: a routed DQL query.
    RoutedDql,
    /// `POST /{id}/ops/tasks-query`: a routed Tasks query.
    RoutedTasksQuery,
    /// `POST /{id}/ops/note`: a routed single-note read (info, links,
    /// backlinks).
    RoutedNoteRead,
    /// `POST /{id}/ops/graph`: a routed whole-graph report.
    RoutedGraph,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanionCapabilities {
    pub protocol_version: u32,
    pub sync_contract_version: u32,
    pub operations: Vec<CompanionOperation>,
    pub transports: Vec<String>,
    pub sync_backends: Vec<String>,
    pub conflict_contract: CompanionConflictCapabilities,
    /// The REST routes this listener serves, when it serves them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest: Option<RestCapabilities>,
}

/// The REST surface a listener serves (`docs/specs/daemon-rest-api.md`):
/// its daemon-level routes and the first path segment of each per-vault
/// route group, so a client can tell whether, say, `/{id}/doctor` exists
/// before calling it. `openapi` describes the routes and their parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestCapabilities {
    pub daemon_routes: Vec<String>,
    pub vault_route_groups: Vec<String>,
    pub openapi: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanionConflictCapabilities {
    pub path_pagination: bool,
    pub maximum_path_page_size: usize,
}

/// How recently a sync must have started for an activity signal to skip a
/// new one: long enough to absorb a burst of sessions and editors, short
/// enough that a session starts from a current checkout.
pub const ACTIVITY_FRESHNESS: std::time::Duration = std::time::Duration::from_secs(30);

/// What an activity signal did for one wiki.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WikiActivityOutcome {
    Queued,
    Pending,
    Fresh,
    /// Automatic synchronization is paused for the wiki.
    Paused,
    /// The wiki is not synchronized with Git.
    NotSynchronized,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiActivityReport {
    pub wiki_id: String,
    pub outcome: WikiActivityOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<SupervisedSyncJob>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSelectionRequest {
    #[serde(default)]
    pub wiki: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub all: bool,
}

impl SyncSelectionRequest {
    fn selection(&self) -> Result<RegisteredSyncSelection, CompanionError> {
        let selected = usize::from(self.wiki.is_some())
            + usize::from(self.group.is_some())
            + usize::from(self.all);
        if selected != 1 {
            return Err(invalid_request(
                "select exactly one of `wiki`, `group`, or `all`",
            ));
        }
        if let Some(wiki) = &self.wiki {
            return WikiId::parse(wiki.clone())
                .map(RegisteredSyncSelection::Wiki)
                .map_err(map_registry_error);
        }
        if let Some(group) = &self.group {
            WikiId::parse(group.clone()).map_err(map_registry_error)?;
            return Ok(RegisteredSyncSelection::Group(group.clone()));
        }
        Ok(RegisteredSyncSelection::All)
    }
}

pub struct CompanionResolutionAgent {
    provider: Box<dyn ResolutionAgentProvider>,
}

pub struct CompanionSemanticAgent {
    provider: Box<dyn SemanticAgentProvider>,
}

impl CompanionSemanticAgent {
    #[must_use]
    pub fn new(provider: impl SemanticAgentProvider + 'static) -> Self {
        Self {
            provider: Box::new(provider),
        }
    }

    pub(crate) fn provider(&self) -> &dyn SemanticAgentProvider {
        self.provider.as_ref()
    }

    #[cfg(feature = "web")]
    pub fn openai_compatible(
        endpoint: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Result<Self, CompanionError> {
        let endpoint = endpoint.into();
        let provider = vulcan_app::sync_semantic::OpenAiCompatibleSemanticProvider::new(
            &endpoint, model, api_key,
        )
        .map_err(|error| invalid_request(error.to_string()))?;
        Ok(Self::new(provider))
    }
}

impl CompanionResolutionAgent {
    pub(crate) fn provider(&self) -> &dyn ResolutionAgentProvider {
        self.provider.as_ref()
    }

    #[must_use]
    pub fn new(provider: impl ResolutionAgentProvider + 'static) -> Self {
        Self {
            provider: Box::new(provider),
        }
    }

    #[cfg(feature = "web")]
    pub fn openai_compatible(
        endpoint: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Result<Self, CompanionError> {
        let endpoint = endpoint.into();
        let provider = vulcan_app::sync_proposals::OpenAiCompatibleResolutionProvider::new(
            &endpoint, model, api_key,
        )
        .map_err(|error| invalid_request(error.to_string()))?;
        Ok(Self::new(provider))
    }
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
                CompanionOperation::SyncActivity,
                CompanionOperation::SyncSelectionEnqueue,
                CompanionOperation::SyncStatus,
                CompanionOperation::SyncPause,
                CompanionOperation::SyncResume,
                CompanionOperation::ConflictList,
                CompanionOperation::ConflictDetail,
                CompanionOperation::JobStatus,
                CompanionOperation::JobCancel,
                CompanionOperation::AggregateJobStatus,
                CompanionOperation::AggregateJobCancel,
            ],
            transports: Vec::new(),
            sync_backends: vec!["git".to_string()],
            conflict_contract: CompanionConflictCapabilities {
                path_pagination: true,
                maximum_path_page_size: 256,
            },
            rest: None,
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

    /// Records that someone is about to use the wiki: queues an automatic
    /// sync unless one is queued, running, or started within
    /// [`ACTIVITY_FRESHNESS`]. Either way the daemon's adaptive polling for
    /// the wiki returns to its shortest interval once the job finishes.
    pub fn report_activity(&self, wiki_id: &WikiId) -> Result<WikiActivityReport, CompanionError> {
        let registration = self.registration(wiki_id)?;
        let report = |outcome, job| WikiActivityReport {
            wiki_id: wiki_id.as_str().to_string(),
            outcome,
            job,
        };
        if registration
            .sync_backend
            .as_deref()
            .is_some_and(|backend| backend != "git")
        {
            return Ok(report(WikiActivityOutcome::NotSynchronized, None));
        }
        if registration.sync_paused {
            return Ok(report(WikiActivityOutcome::Paused, None));
        }
        let enqueued = self
            .supervisor
            .enqueue_activity(wiki_id.as_str(), registration.path, ACTIVITY_FRESHNESS)
            .map_err(map_supervisor_error)?;
        let outcome = match enqueued.outcome {
            ActivityOutcome::Queued => WikiActivityOutcome::Queued,
            ActivityOutcome::Pending => WikiActivityOutcome::Pending,
            ActivityOutcome::Fresh => WikiActivityOutcome::Fresh,
        };
        Ok(report(outcome, Some(enqueued.job)))
    }

    pub fn enqueue_sync_selection(
        &self,
        request: &SyncSelectionRequest,
        credential_scope: &str,
        idempotency_key: &str,
    ) -> Result<IdempotentEnqueueAggregateSyncReport, CompanionError> {
        enqueue_registered_wikis(
            self.registry,
            self.supervisor,
            &request.selection()?,
            credential_scope,
            idempotency_key,
        )
        .map_err(map_registered_sync_error)
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

    pub fn conflict_detail_page(
        &self,
        wiki_id: &WikiId,
        conflict_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<SyncConflictDetailReport, CompanionError> {
        let registration = self.checked_git_registration(wiki_id)?;
        get_sync_conflict_page_with_state_store(
            &VaultPaths::new(registration.path),
            conflict_id,
            offset,
            limit,
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

    pub fn aggregate_job(&self, job_id: &str) -> Result<AggregateSyncJob, CompanionError> {
        self.supervisor
            .aggregate(job_id)
            .map_err(map_supervisor_error)?
            .ok_or_else(|| {
                CompanionError::new(
                    CompanionErrorKind::NotFound,
                    format!("unknown aggregate synchronization job `{job_id}`"),
                )
            })
    }

    pub fn cancel_aggregate_job(&self, job_id: &str) -> Result<AggregateSyncJob, CompanionError> {
        self.supervisor
            .cancel_aggregate(job_id)
            .map_err(map_supervisor_error)
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
                    sync_backend: None,
                    profile: None,
                    groups_to_add: Vec::new(),
                    groups_to_remove: Vec::new(),
                    permissions_profile: None,
                    sync_paused: Some(paused),
                    execution_trust: None,
                    mcp_remote: None,
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
            resolve_device_permission_profile(&paths, registration.permissions_profile.as_deref())
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

fn map_registered_sync_error(error: RegisteredSyncError) -> CompanionError {
    match error {
        RegisteredSyncError::Registry(error) => map_registry_error(error),
        RegisteredSyncError::Supervisor(error) => map_supervisor_error(error),
        RegisteredSyncError::EmptyGroup(group) => CompanionError::new(
            CompanionErrorKind::NotFound,
            format!("no registered wikis belong to group `{group}`"),
        ),
    }
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
                    source: None,
                    profile: None,
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
    fn capabilities_are_versioned_and_list_only_served_operations() {
        let temporary = tempdir().expect("temporary directory");
        let (registry, supervisor, state_store, _) = fixture(&temporary);
        let service = CompanionService::new(&registry, &supervisor, &state_store);
        let value = serde_json::to_value(service.capabilities()).expect("serialize capabilities");

        assert_eq!(value["protocol_version"], json!(1));
        assert_eq!(value["sync_contract_version"], json!(1));
        assert_eq!(value["conflict_contract"]["path_pagination"], json!(true));
        assert_eq!(
            value["conflict_contract"]["maximum_path_page_size"],
            json!(256)
        );
        assert_eq!(value["transports"], json!([]));
        assert!(value["operations"]
            .as_array()
            .expect("operations")
            .contains(&json!("sync_enqueue")));
        assert!(value["operations"]
            .as_array()
            .expect("operations")
            .contains(&json!("sync_selection_enqueue")));
        for retired in [
            "event_subscribe",
            "conflict_resolve",
            "conflict_proposal_create",
            "semantic_plan_create",
        ] {
            assert!(!value["operations"]
                .as_array()
                .expect("operations")
                .contains(&json!(retired)));
        }
    }

    #[test]
    fn companion_enqueues_registered_selections_as_aggregate_jobs() {
        let temporary = tempdir().expect("temporary directory");
        let (registry, supervisor, state_store, _) = fixture(&temporary);
        let service = CompanionService::new(&registry, &supervisor, &state_store);
        let request = SyncSelectionRequest {
            wiki: None,
            group: Some("personal".to_string()),
            all: false,
        };
        let first = service
            .enqueue_sync_selection(&request, "credential-a", "selection-1")
            .expect("enqueue selection");
        assert!(!first.replay);
        assert_eq!(first.aggregate.selection, "group:personal");
        assert_eq!(first.aggregate.total, 1);
        let replay = service
            .enqueue_sync_selection(&request, "credential-a", "selection-1")
            .expect("replay selection");
        assert!(replay.replay);
        assert_eq!(replay.aggregate.id, first.aggregate.id);
        assert_eq!(
            service
                .aggregate_job(&first.aggregate.id)
                .expect("aggregate status")
                .state,
            vulcan_sync::SyncJobState::Queued
        );

        let invalid = SyncSelectionRequest {
            wiki: Some("notes".to_string()),
            group: None,
            all: true,
        };
        assert!(service
            .enqueue_sync_selection(&invalid, "credential-a", "selection-2")
            .is_err());
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
