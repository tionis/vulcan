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
    AggregateSyncJob, IdempotentEnqueueAggregateSyncReport, IdempotentEnqueueSyncReport,
    SupervisedSyncJob, SupervisorError, SyncSupervisor,
};
use crate::sync::{enqueue_registered_wikis, RegisteredSyncError, RegisteredSyncSelection};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{Display, Formatter};
use vulcan_app::sync_conflicts::{
    get_sync_conflict_with_state_store, list_sync_conflicts_with_state_store,
    resolve_sync_conflict_with_state_store, ResolveSyncConflictOptions, ResolveSyncConflictReport,
    SyncConflictDetailReport, SyncConflictListReport, SyncConflictResolutionSide,
};
use vulcan_app::sync_proposals::{
    approve_resolution_proposal_with_state_store, create_resolution_proposal_with_provider,
    reject_resolution_proposal_with_state_store, ApproveResolutionProposalOptions,
    ApproveResolutionProposalReport, RejectResolutionProposalReport, ResolutionAgentProvider,
    ResolutionProposal, ResolutionProposalOptions,
};
use vulcan_app::sync_semantic::{
    create_semantic_plan_with_state_store, SemanticGrouping, SemanticPlanOptions,
    SemanticPlanReport,
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
    SyncSelectionEnqueue,
    SyncStatus,
    SyncPause,
    SyncResume,
    ConflictList,
    ConflictDetail,
    ConflictProposalCreate,
    ConflictProposalApprove,
    ConflictProposalReject,
    ConflictResolve,
    SemanticPlanCreate,
    JobStatus,
    JobCancel,
    AggregateJobStatus,
    AggregateJobCancel,
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
pub struct ConflictProposalRequest {
    #[serde(default)]
    pub context: Vec<String>,
    #[serde(default)]
    pub allow_broad_context: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConflictProposalApprovalRequest {
    pub proposal_id: String,
    #[serde(default = "default_remote")]
    pub remote: String,
    #[serde(default = "default_live_ref")]
    pub live_ref: String,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConflictProposalRejectionRequest {
    pub proposal_id: String,
    #[serde(default)]
    pub dry_run: bool,
}

pub struct CompanionResolutionAgent {
    provider: Box<dyn ResolutionAgentProvider>,
}

impl CompanionResolutionAgent {
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
    pub grouping: SemanticGrouping,
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
    resolution_agent: Option<&'a CompanionResolutionAgent>,
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
            resolution_agent: None,
        }
    }

    #[must_use]
    pub const fn with_resolution_agent(mut self, agent: &'a CompanionResolutionAgent) -> Self {
        self.resolution_agent = Some(agent);
        self
    }

    #[must_use]
    pub fn capabilities(&self) -> CompanionCapabilities {
        CompanionCapabilities {
            protocol_version: COMPANION_PROTOCOL_VERSION,
            sync_contract_version: SYNC_CONTRACT_VERSION,
            operations: {
                let mut operations = vec![
                    CompanionOperation::Capabilities,
                    CompanionOperation::WikiList,
                    CompanionOperation::SyncEnqueue,
                    CompanionOperation::SyncSelectionEnqueue,
                    CompanionOperation::SyncStatus,
                    CompanionOperation::SyncPause,
                    CompanionOperation::SyncResume,
                    CompanionOperation::ConflictList,
                    CompanionOperation::ConflictDetail,
                    CompanionOperation::ConflictResolve,
                    CompanionOperation::ConflictProposalApprove,
                    CompanionOperation::ConflictProposalReject,
                    CompanionOperation::SemanticPlanCreate,
                    CompanionOperation::JobStatus,
                    CompanionOperation::JobCancel,
                    CompanionOperation::AggregateJobStatus,
                    CompanionOperation::AggregateJobCancel,
                ];
                if self.resolution_agent.is_some() {
                    operations.extend([CompanionOperation::ConflictProposalCreate]);
                }
                operations
            },
            transports: Vec::new(),
            sync_backends: vec!["git".to_string()],
            conflict_resolution_sides: vec![
                SyncConflictResolutionSide::Base,
                SyncConflictResolutionSide::Local,
                SyncConflictResolutionSide::Remote,
            ],
            agent_conflict_proposals: self.resolution_agent.is_some(),
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

    pub fn create_conflict_proposal(
        &self,
        wiki_id: &WikiId,
        conflict_id: &str,
        request: &ConflictProposalRequest,
    ) -> Result<ResolutionProposal, CompanionError> {
        let agent = self.resolution_agent.ok_or_else(|| {
            CompanionError::new(
                CompanionErrorKind::NotFound,
                "no resolution agent is configured for this companion service",
            )
        })?;
        let registration = self.checked_git_registration(wiki_id)?;
        let paths = VaultPaths::new(registration.path);
        let profile = registration
            .permissions_profile
            .as_deref()
            .unwrap_or("unrestricted");
        let selection = resolve_permission_profile(&paths, Some(profile)).map_err(|error| {
            CompanionError::new(CompanionErrorKind::PermissionDenied, error.to_string())
        })?;
        if let Some(endpoint) = agent.provider.network_endpoint() {
            ProfilePermissionGuard::new(&paths, selection)
                .check_network(endpoint)
                .map_err(|error| {
                    CompanionError::new(CompanionErrorKind::PermissionDenied, error.to_string())
                })?;
        }
        create_resolution_proposal_with_provider(
            &paths,
            conflict_id,
            &ResolutionProposalOptions {
                permission_profile: profile.to_string(),
                focused_context: request.context.clone(),
                allow_broad_context: request.allow_broad_context,
            },
            agent.provider.as_ref(),
            &vulcan_app::sync::SyncCancellationToken::default(),
            self.state_store,
        )
        .map_err(map_app_error)
    }

    pub fn approve_conflict_proposal(
        &self,
        wiki_id: &WikiId,
        conflict_id: &str,
        request: &ConflictProposalApprovalRequest,
    ) -> Result<ApproveResolutionProposalReport, CompanionError> {
        let registration = self.checked_git_registration(wiki_id)?;
        approve_resolution_proposal_with_state_store(
            &VaultPaths::new(registration.path),
            conflict_id,
            &request.proposal_id,
            &ApproveResolutionProposalOptions {
                remote: GitRemote::parse(&request.remote)
                    .map_err(|error| invalid_request(error.to_string()))?,
                live_ref: GitRefName::parse(&request.live_ref)
                    .map_err(|error| invalid_request(error.to_string()))?,
                dry_run: request.dry_run,
                automatic: false,
            },
            &vulcan_app::sync::SyncCancellationToken::default(),
            self.state_store,
        )
        .map_err(map_app_error)
    }

    pub fn reject_conflict_proposal(
        &self,
        wiki_id: &WikiId,
        conflict_id: &str,
        request: &ConflictProposalRejectionRequest,
    ) -> Result<RejectResolutionProposalReport, CompanionError> {
        let registration = self.checked_git_registration(wiki_id)?;
        reject_resolution_proposal_with_state_store(
            &VaultPaths::new(registration.path),
            conflict_id,
            &request.proposal_id,
            request.dry_run,
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
                grouping: request.grouping,
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
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;
    use vulcan_app::sync::{sync_git_vault_with_state_store, SyncCancellationToken};
    use vulcan_app::sync_proposals::{
        ApproveResolutionProposalOutcome, RejectResolutionProposalOutcome, ResolutionAgentIdentity,
        ResolutionAgentOutput, ResolutionAgentPathOutput, ResolutionAgentRequest,
        ResolutionAgentTools,
    };
    use vulcan_sync::GitSyncOptions;

    struct ConfiguredTestProvider;

    struct ResolvingTestProvider;

    impl ResolutionAgentProvider for ConfiguredTestProvider {
        fn identity(&self) -> ResolutionAgentIdentity {
            ResolutionAgentIdentity {
                provider: "companion-test".to_string(),
                model: "fixture-v1".to_string(),
                prompt_contract_version: 3,
            }
        }

        fn network_endpoint(&self) -> Option<&str> {
            Some("https://agent.example.test/v1/chat/completions")
        }

        fn propose(
            &self,
            _request: &ResolutionAgentRequest,
            _tools: &mut dyn ResolutionAgentTools,
            _cancellation: &SyncCancellationToken,
        ) -> Result<ResolutionAgentOutput, vulcan_app::AppError> {
            Err(vulcan_app::AppError::operation(
                "the capability test must not invoke the provider",
            ))
        }
    }

    impl ResolutionAgentProvider for ResolvingTestProvider {
        fn identity(&self) -> ResolutionAgentIdentity {
            ResolutionAgentIdentity {
                provider: "companion-test".to_string(),
                model: "resolver-v1".to_string(),
                prompt_contract_version: 3,
            }
        }

        fn propose(
            &self,
            request: &ResolutionAgentRequest,
            _tools: &mut dyn ResolutionAgentTools,
            _cancellation: &SyncCancellationToken,
        ) -> Result<ResolutionAgentOutput, vulcan_app::AppError> {
            Ok(ResolutionAgentOutput {
                explanation: "Resolve the test conflict.".to_string(),
                referenced_context: Vec::new(),
                paths: request
                    .files
                    .iter()
                    .map(|file| ResolutionAgentPathOutput {
                        path: file.path.clone(),
                        content: b"companion resolution\n".to_vec(),
                    })
                    .collect(),
            })
        }
    }

    fn git(directory: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .current_dir(directory)
            .args(arguments)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn commit_all(directory: &Path, message: &str) {
        git(directory, &["add", "--all"]);
        git(
            directory,
            &[
                "-c",
                "user.name=Vulcan Test",
                "-c",
                "user.email=vulcan@example.invalid",
                "commit",
                "--quiet",
                "-m",
                message,
            ],
        );
    }

    fn conflict_service_fixture() -> (
        tempfile::TempDir,
        WikiRegistry,
        SyncSupervisor,
        SyncStateStore,
        WikiId,
        String,
    ) {
        let temporary = tempdir().expect("temporary directory");
        let remote = temporary.path().join("remote.git");
        git(
            temporary.path(),
            &[
                "init",
                "--quiet",
                "--bare",
                remote.to_str().expect("remote"),
            ],
        );
        let writer = temporary.path().join("writer");
        fs::create_dir(&writer).expect("writer directory");
        git(
            &writer,
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        git(
            &writer,
            &["remote", "add", "origin", remote.to_str().expect("remote")],
        );
        fs::write(writer.join("Home.md"), "base\n").expect("base note");
        commit_all(&writer, "base");
        let state_store = SyncStateStore::at(temporary.path().join("sync-state"));
        sync_git_vault_with_state_store(
            &VaultPaths::new(&writer),
            &GitSyncOptions::default(),
            &state_store,
        )
        .expect("bootstrap writer");
        let reader = temporary.path().join("reader");
        git(
            temporary.path(),
            &[
                "clone",
                "--quiet",
                writer.to_str().expect("writer"),
                reader.to_str().expect("reader"),
            ],
        );
        git(
            &reader,
            &[
                "remote",
                "set-url",
                "origin",
                remote.to_str().expect("remote"),
            ],
        );
        sync_git_vault_with_state_store(
            &VaultPaths::new(&reader),
            &GitSyncOptions::default(),
            &state_store,
        )
        .expect("bootstrap reader");
        fs::write(writer.join("Home.md"), "writer\n").expect("writer edit");
        fs::write(reader.join("Home.md"), "reader\n").expect("reader edit");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&writer),
            &GitSyncOptions::default(),
            &state_store,
        )
        .expect("publish writer");
        let conflict = sync_git_vault_with_state_store(
            &VaultPaths::new(&reader),
            &GitSyncOptions::default(),
            &state_store,
        )
        .expect("reader conflict")
        .conflict_record
        .expect("conflict record");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let wiki_id = WikiId::parse("notes").expect("wiki id");
        registry
            .add(
                &AddWikiRequest {
                    id: wiki_id.clone(),
                    path: reader,
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("register reader");
        let supervisor =
            SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor");
        (
            temporary,
            registry,
            supervisor,
            state_store,
            wiki_id,
            conflict.id,
        )
    }

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
        assert!(value["operations"]
            .as_array()
            .expect("operations")
            .contains(&json!("sync_selection_enqueue")));
        assert!(!value["operations"]
            .as_array()
            .expect("operations")
            .contains(&json!("event_subscribe")));
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
    fn semantic_plan_requests_default_and_parse_deterministic_grouping() {
        let default: SemanticPlanRequest = serde_json::from_value(json!({
            "from": "main",
            "to": "refs/vulcan/sync/local/live",
            "semantic_ref": "refs/heads/main"
        }))
        .expect("default semantic plan request");
        assert_eq!(default.grouping, SemanticGrouping::TopLevel);

        let by_file: SemanticPlanRequest = serde_json::from_value(json!({
            "from": "main",
            "to": "refs/vulcan/sync/local/live",
            "semantic_ref": "refs/heads/main",
            "grouping": "file"
        }))
        .expect("file-grouped semantic plan request");
        assert_eq!(by_file.grouping, SemanticGrouping::File);
    }

    #[test]
    fn configured_resolution_agent_is_advertised_without_exposing_its_endpoint() {
        let temporary = tempdir().expect("temporary directory");
        let (registry, supervisor, state_store, _) = fixture(&temporary);
        let agent = CompanionResolutionAgent::new(ConfiguredTestProvider);
        let service = CompanionService::new(&registry, &supervisor, &state_store)
            .with_resolution_agent(&agent);
        let value = serde_json::to_value(service.capabilities()).expect("serialize capabilities");

        assert_eq!(value["agent_conflict_proposals"], json!(true));
        assert!(value["operations"]
            .as_array()
            .expect("operations")
            .contains(&json!("conflict_proposal_create")));
        assert!(!value.to_string().contains("agent.example.test"));
    }

    #[test]
    fn companion_proposal_creation_preview_and_rejection_reuse_app_transactions() {
        let (_temporary, registry, supervisor, state_store, wiki_id, conflict_id) =
            conflict_service_fixture();
        let agent = CompanionResolutionAgent::new(ResolvingTestProvider);
        let service = CompanionService::new(&registry, &supervisor, &state_store)
            .with_resolution_agent(&agent);

        let proposal = service
            .create_conflict_proposal(
                &wiki_id,
                &conflict_id,
                &ConflictProposalRequest {
                    context: Vec::new(),
                    allow_broad_context: false,
                },
            )
            .expect("create proposal");
        assert_eq!(proposal.provider, "companion-test");
        let request = ConflictProposalApprovalRequest {
            proposal_id: proposal.proposal_id.clone(),
            remote: default_remote(),
            live_ref: default_live_ref(),
            dry_run: true,
        };
        assert_eq!(
            service
                .approve_conflict_proposal(&wiki_id, &conflict_id, &request)
                .expect("preview approval")
                .outcome,
            ApproveResolutionProposalOutcome::Planned
        );
        assert_eq!(
            service
                .reject_conflict_proposal(
                    &wiki_id,
                    &conflict_id,
                    &ConflictProposalRejectionRequest {
                        proposal_id: request.proposal_id.clone(),
                        dry_run: false,
                    },
                )
                .expect("reject proposal")
                .outcome,
            RejectResolutionProposalOutcome::Rejected
        );
        assert!(service
            .approve_conflict_proposal(&wiki_id, &conflict_id, &request)
            .expect_err("rejected proposal cannot be approved")
            .detail
            .contains("rejected"));
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
