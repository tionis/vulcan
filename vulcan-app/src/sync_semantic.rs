//! Reviewable semantic histories derived from immutable accepted sync snapshots.

use crate::sync::SyncCancellationToken;
use crate::sync_state::{repository_state_key, SyncStateStore};
use crate::AppError;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
#[cfg(feature = "web")]
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use ulid::Ulid;
use vulcan_core::VaultPaths;
use vulcan_sync::{
    semantic_proposal_ref as namespace_semantic_proposal_ref, GitChange, GitChangeKind,
    GitCliEngine, GitEngine, GitOid, GitPushResult, GitRefDeleteResult, GitRefName,
    GitRefUpdateResult, GitRemote, GitRepository, GitSyncOptions, GitSyncRefs,
};

pub const SEMANTIC_PLAN_VERSION: u32 = 6;
const MAX_SEMANTIC_PLAN_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SEMANTIC_AGENT_PATCH_BYTES: usize = 8 * 1024 * 1024;
const MAX_SEMANTIC_AGENT_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_SEMANTIC_AGENT_LABEL_BYTES: usize = 256;
#[cfg(feature = "web")]
const MAX_SEMANTIC_AGENT_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticPlanStatus {
    Preview,
    Prepared,
    Ready,
    Applying,
    Applied,
    Rejecting,
    Rejected,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticGrouping {
    #[default]
    TopLevel,
    File,
    Change,
    Hunk,
    All,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticPlanOptions {
    pub from: String,
    pub to: String,
    pub semantic_ref: GitRefName,
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub grouping: SemanticGrouping,
    pub agent: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticAgentIdentity {
    pub provider: String,
    pub model: String,
    pub prompt_contract_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticAgentChange {
    pub path: String,
    pub patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticAgentRequest {
    pub source_revision: String,
    pub target_revision: String,
    pub changes: Vec<SemanticAgentChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticAgentCommit {
    pub group: String,
    pub message: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticAgentOutput {
    pub commits: Vec<SemanticAgentCommit>,
}

pub trait SemanticAgentProvider: Send + Sync {
    fn identity(&self) -> SemanticAgentIdentity;

    fn network_endpoint(&self) -> Option<&str> {
        None
    }

    fn propose(
        &self,
        request: &SemanticAgentRequest,
        cancellation: &SyncCancellationToken,
    ) -> Result<SemanticAgentOutput, AppError>;
}

#[cfg(feature = "web")]
pub struct OpenAiCompatibleSemanticProvider {
    client: reqwest::blocking::Client,
    endpoint: reqwest::Url,
    model: String,
    api_key: Option<String>,
}

#[cfg(feature = "web")]
impl OpenAiCompatibleSemanticProvider {
    pub fn new(
        base_url: &str,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Result<Self, AppError> {
        let mut endpoint = reqwest::Url::parse(base_url).map_err(AppError::operation)?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(AppError::operation(
                "semantic agent base URL must be an absolute HTTP(S) URL without credentials, query, or fragment",
            ));
        }
        let path = endpoint.path().trim_end_matches('/');
        endpoint.set_path(&format!("{path}/chat/completions"));
        let model = model.into();
        validate_agent_identity(&SemanticAgentIdentity {
            provider: "openai-compatible".to_string(),
            model: model.clone(),
            prompt_contract_version: 1,
        })?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(AppError::operation)?;
        Ok(Self {
            client,
            endpoint,
            model,
            api_key,
        })
    }

    fn send(&self, body: &serde_json::Value) -> Result<Vec<u8>, AppError> {
        let mut builder = self.client.post(self.endpoint.clone()).json(body);
        if let Some(api_key) = self.api_key.as_deref() {
            builder = builder.bearer_auth(api_key);
        }
        let mut response = builder.send().map_err(AppError::operation)?;
        let status = response.status();
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take((MAX_SEMANTIC_AGENT_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(AppError::operation)?;
        if bytes.len() > MAX_SEMANTIC_AGENT_RESPONSE_BYTES {
            return Err(AppError::operation(
                "semantic agent response exceeds its byte limit",
            ));
        }
        if !status.is_success() {
            return Err(AppError::operation(format!(
                "semantic agent provider returned HTTP {status}"
            )));
        }
        Ok(bytes)
    }
}

#[cfg(feature = "web")]
impl SemanticAgentProvider for OpenAiCompatibleSemanticProvider {
    fn identity(&self) -> SemanticAgentIdentity {
        SemanticAgentIdentity {
            provider: "openai-compatible".to_string(),
            model: self.model.clone(),
            prompt_contract_version: 1,
        }
    }

    fn network_endpoint(&self) -> Option<&str> {
        Some(self.endpoint.as_str())
    }

    fn propose(
        &self,
        request: &SemanticAgentRequest,
        cancellation: &SyncCancellationToken,
    ) -> Result<SemanticAgentOutput, AppError> {
        semantic_cancellation_check(cancellation)?;
        let input = serde_json::to_string(request).map_err(AppError::operation)?;
        let body = serde_json::json!({
            "model": self.model,
            "temperature": 0,
            "response_format": { "type": "json_object" },
            "messages": [
                {
                    "role": "system",
                    "content": "Organize only the supplied accepted Git changes into an ordered semantic commit plan. Return exactly one JSON object with commits, an ordered array of objects containing group (a unique short label), message (a human commit message without Vulcan-Semantic trailers), and paths (an array). Include every supplied path exactly once, name no other path, preserve dependency order, and do not propose or invent file content. Emit no Markdown fence or commentary outside the JSON object."
                },
                { "role": "user", "content": input }
            ]
        });
        let bytes = self.send(&body)?;
        semantic_cancellation_check(cancellation)?;
        parse_openai_semantic_output(&bytes)
    }
}

#[cfg(feature = "web")]
fn parse_openai_semantic_output(bytes: &[u8]) -> Result<SemanticAgentOutput, AppError> {
    #[derive(Deserialize)]
    struct Response {
        choices: Vec<Choice>,
    }
    #[derive(Deserialize)]
    struct Choice {
        message: Message,
    }
    #[derive(Deserialize)]
    struct Message {
        content: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Output {
        commits: Vec<Commit>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Commit {
        group: String,
        message: String,
        paths: Vec<String>,
    }

    let response: Response = serde_json::from_slice(bytes).map_err(AppError::operation)?;
    let content = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| AppError::operation("semantic agent response contained no choices"))?
        .message
        .content;
    let output: Output = serde_json::from_str(&content).map_err(|error| {
        AppError::operation(format!(
            "semantic agent response content was not exact JSON: {error}"
        ))
    })?;
    Ok(SemanticAgentOutput {
        commits: output
            .commits
            .into_iter()
            .map(|commit| SemanticAgentCommit {
                group: commit.group,
                message: commit.message,
                paths: commit.paths,
            })
            .collect(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticCommitProposal {
    pub position: usize,
    pub group: String,
    pub message: String,
    pub paths: Vec<String>,
    pub from_revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    pub patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct SemanticPlanValidation {
    pub source_ref_matches: bool,
    pub source_is_ancestor: bool,
    pub target_is_accepted_live: bool,
    pub final_tree_matches_target: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticPlanReport {
    pub version: u32,
    pub plan_id: String,
    pub status: SemanticPlanStatus,
    pub dry_run: bool,
    pub agent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<SemanticAgentIdentity>,
    #[serde(default)]
    pub grouping: SemanticGrouping,
    pub vault: PathBuf,
    pub repository_key: String,
    pub semantic_ref: String,
    pub proposal_ref: String,
    pub remote: String,
    pub live_ref: String,
    pub source_revision: String,
    pub target_revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_tip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_remote_previous_revision: Option<String>,
    pub commits: Vec<SemanticCommitProposal>,
    pub validation: SemanticPlanValidation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedSemanticGroup {
    group: String,
    message: String,
    paths: Vec<String>,
    patch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SemanticApplyReport {
    pub version: u32,
    pub plan_id: String,
    pub dry_run: bool,
    pub semantic_ref: String,
    pub previous_revision: String,
    pub applied_revision: String,
    pub target_revision: String,
    pub proposal_ref_released: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SemanticPublishReport {
    pub version: u32,
    pub plan_id: String,
    pub dry_run: bool,
    pub remote: String,
    pub semantic_ref: String,
    pub previous_revision: String,
    pub published_revision: String,
    pub already_published: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRejectOutcome {
    Planned,
    Rejected,
    AlreadyRejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SemanticRejectReport {
    pub version: u32,
    pub plan_id: String,
    pub dry_run: bool,
    pub outcome: SemanticRejectOutcome,
    pub proposal_ref: String,
    pub proposal_tip: String,
    pub record_retained: bool,
}

pub fn create_semantic_plan(
    paths: &VaultPaths,
    options: &SemanticPlanOptions,
) -> Result<SemanticPlanReport, AppError> {
    let store = SyncStateStore::user_default()?;
    create_semantic_plan_with_state_store(paths, options, &store)
}

pub fn create_semantic_plan_with_state_store(
    paths: &VaultPaths,
    options: &SemanticPlanOptions,
    store: &SyncStateStore,
) -> Result<SemanticPlanReport, AppError> {
    if options.agent {
        return Err(AppError::operation(
            "agent-assisted semantic grouping requires a configured semantic planning provider",
        ));
    }
    if options.grouping == SemanticGrouping::Agent {
        return Err(AppError::operation(
            "agent grouping requires agent mode and a configured provider",
        ));
    }
    create_semantic_plan_internal(
        paths,
        options,
        store,
        None,
        &SyncCancellationToken::default(),
    )
}

pub fn create_semantic_plan_with_provider(
    paths: &VaultPaths,
    options: &SemanticPlanOptions,
    provider: &dyn SemanticAgentProvider,
    cancellation: &SyncCancellationToken,
) -> Result<SemanticPlanReport, AppError> {
    let store = SyncStateStore::user_default()?;
    create_semantic_plan_with_provider_and_state_store(
        paths,
        options,
        provider,
        cancellation,
        &store,
    )
}

pub fn create_semantic_plan_with_provider_and_state_store(
    paths: &VaultPaths,
    options: &SemanticPlanOptions,
    provider: &dyn SemanticAgentProvider,
    cancellation: &SyncCancellationToken,
    store: &SyncStateStore,
) -> Result<SemanticPlanReport, AppError> {
    if !options.agent {
        return Err(AppError::operation(
            "a semantic planning provider may only be used with agent mode enabled",
        ));
    }
    create_semantic_plan_internal(paths, options, store, Some(provider), cancellation)
}

fn create_semantic_plan_internal(
    paths: &VaultPaths,
    options: &SemanticPlanOptions,
    store: &SyncStateStore,
    provider: Option<&dyn SemanticAgentProvider>,
    cancellation: &SyncCancellationToken,
) -> Result<SemanticPlanReport, AppError> {
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let source = engine
        .resolve_revision(&repository, &options.from)
        .map_err(AppError::operation)?;
    let target = engine
        .resolve_revision(&repository, &options.to)
        .map_err(AppError::operation)?;
    validate_plan_inputs(&engine, &repository, options, &source, &target)?;
    let plan_id = Ulid::new().to_string().to_ascii_lowercase();
    let proposal_ref = semantic_proposal_ref(&plan_id)?;
    let changed_paths = engine
        .changed_paths(&repository, &source, &target)
        .map_err(AppError::operation)?;
    let (groups, agent_identity) = match provider {
        Some(provider) => plan_agent_groups(
            &engine,
            &repository,
            &source,
            &target,
            changed_paths,
            provider,
            cancellation,
        )?,
        None if options.grouping == SemanticGrouping::Change => {
            let changes = engine
                .changed_entries(&repository, &source, &target)
                .map_err(AppError::operation)?;
            (deterministic_change_groups(changes), None)
        }
        None if options.grouping == SemanticGrouping::Hunk => {
            let changes = engine
                .changed_entries(&repository, &source, &target)
                .map_err(AppError::operation)?;
            (
                deterministic_hunk_groups(&engine, &repository, &source, &target, changes)?,
                None,
            )
        }
        None => (deterministic_groups(changed_paths, options.grouping), None),
    };
    let mut report = initial_plan_report(
        &vault,
        options,
        &source,
        &target,
        &plan_id,
        &proposal_ref,
        agent_identity,
    );
    if options.dry_run {
        report.commits = preview_commits(&engine, &repository, &source, &target, groups)?;
        report.validation.final_tree_matches_target = true;
        return Ok(report);
    }

    let _lock = SemanticLock::acquire(&repository)?;
    validate_plan_inputs(&engine, &repository, options, &source, &target)?;
    report.status = SemanticPlanStatus::Prepared;
    save_plan(store, &report, true)?;
    let (commits, tip) =
        construct_proposal(&engine, &repository, &source, &target, &plan_id, groups)?;
    let final_tree = engine
        .tree_oid(&repository, &tip)
        .map_err(AppError::operation)?;
    let target_tree = engine
        .tree_oid(&repository, &target)
        .map_err(AppError::operation)?;
    if final_tree != target_tree {
        return Err(AppError::operation(format!(
            "semantic proposal final tree {final_tree} differs from accepted target tree {target_tree}"
        )));
    }
    if engine
        .compare_and_swap_ref(&repository, &proposal_ref, &tip, None)
        .map_err(AppError::operation)?
        != GitRefUpdateResult::Updated
    {
        return Err(AppError::operation(format!(
            "semantic proposal ref {proposal_ref} already exists"
        )));
    }
    report.status = SemanticPlanStatus::Ready;
    report.proposal_tip = Some(tip.to_string());
    report.commits = commits;
    report.validation.final_tree_matches_target = true;
    save_plan(store, &report, false)?;
    Ok(report)
}

pub fn load_semantic_plan(plan_id: &str) -> Result<SemanticPlanReport, AppError> {
    load_semantic_plan_with_state_store(plan_id, &SyncStateStore::user_default()?)
}

pub fn load_semantic_plan_with_state_store(
    plan_id: &str,
    store: &SyncStateStore,
) -> Result<SemanticPlanReport, AppError> {
    validate_plan_id(plan_id)?;
    let path = semantic_plan_path(store, plan_id);
    let metadata = fs::symlink_metadata(&path).map_err(AppError::operation)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppError::operation(format!(
            "semantic plan at {} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_SEMANTIC_PLAN_BYTES {
        return Err(AppError::operation(format!(
            "semantic plan at {} exceeds the {} byte limit",
            path.display(),
            MAX_SEMANTIC_PLAN_BYTES
        )));
    }
    let plan: SemanticPlanReport =
        serde_json::from_slice(&fs::read(&path).map_err(AppError::operation)?)
            .map_err(AppError::operation)?;
    validate_loaded_plan(plan_id, &path, &plan)?;
    Ok(plan)
}

pub fn apply_semantic_plan(plan_id: &str, dry_run: bool) -> Result<SemanticApplyReport, AppError> {
    let store = SyncStateStore::user_default()?;
    apply_semantic_plan_with_state_store(plan_id, dry_run, &store)
}

pub fn publish_semantic_plan(
    plan_id: &str,
    dry_run: bool,
) -> Result<SemanticPublishReport, AppError> {
    publish_semantic_plan_with_state_store(plan_id, dry_run, &SyncStateStore::user_default()?)
}

pub fn publish_semantic_plan_with_state_store(
    plan_id: &str,
    dry_run: bool,
    store: &SyncStateStore,
) -> Result<SemanticPublishReport, AppError> {
    let mut plan = load_semantic_plan_with_state_store(plan_id, store)?;
    if plan.status != SemanticPlanStatus::Applied {
        return Err(AppError::operation(format!(
            "semantic plan {plan_id} must be applied before publication"
        )));
    }
    let engine = GitCliEngine::default();
    let repository = engine
        .discover_repository(&plan.vault)
        .map_err(AppError::operation)?;
    if repository_state_key(&plan.vault) != plan.repository_key {
        return Err(AppError::operation(
            "semantic plan vault identity no longer matches its repository key",
        ));
    }
    let _lock = SemanticLock::acquire(&repository)?;
    let source = GitOid::parse(plan.source_revision.clone()).map_err(AppError::operation)?;
    let tip = GitOid::parse(
        plan.proposal_tip
            .clone()
            .ok_or_else(|| AppError::operation("semantic plan has no proposal tip"))?,
    )
    .map_err(AppError::operation)?;
    let target = GitOid::parse(plan.target_revision.clone()).map_err(AppError::operation)?;
    let semantic_ref = GitRefName::parse(plan.semantic_ref.clone()).map_err(AppError::operation)?;
    let remote = GitRemote::parse(plan.remote.clone()).map_err(AppError::operation)?;
    validate_semantic_publication(&engine, &repository, &semantic_ref, &source, &target, &tip)?;
    let remote_revision = engine
        .remote_ref(&repository, &remote, &semantic_ref)
        .map_err(AppError::operation)?;
    let already_published = remote_revision.as_ref() == Some(&tip);
    if !already_published && remote_revision.as_ref() != Some(&source) {
        return Err(AppError::operation(format!(
            "remote semantic ref {semantic_ref} changed from expected source {source}"
        )));
    }
    let report = SemanticPublishReport {
        version: SEMANTIC_PLAN_VERSION,
        plan_id: plan_id.to_string(),
        dry_run,
        remote: remote.to_string(),
        semantic_ref: semantic_ref.to_string(),
        previous_revision: source.to_string(),
        published_revision: tip.to_string(),
        already_published,
    };
    if dry_run {
        return Ok(report);
    }
    if !already_published
        && engine
            .push_ref(&repository, &remote, &tip, &semantic_ref, Some(&source))
            .map_err(AppError::operation)?
            != GitPushResult::Updated
    {
        return Err(AppError::operation(
            "remote semantic branch changed while publishing the applied plan",
        ));
    }
    if engine
        .remote_ref(&repository, &remote, &semantic_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(&tip)
    {
        return Err(AppError::operation(
            "remote semantic branch does not identify the published proposal tip",
        ));
    }
    plan.version = SEMANTIC_PLAN_VERSION;
    plan.published_revision = Some(tip.to_string());
    plan.published_remote_previous_revision = Some(source.to_string());
    save_plan(store, &plan, false)?;
    Ok(report)
}

fn validate_semantic_publication(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    semantic_ref: &GitRefName,
    source: &GitOid,
    target: &GitOid,
    tip: &GitOid,
) -> Result<(), AppError> {
    if engine
        .read_ref(repository, semantic_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(tip)
    {
        return Err(AppError::operation(
            "local semantic branch does not identify the applied proposal tip",
        ));
    }
    if !engine
        .is_ancestor(repository, source, tip)
        .map_err(AppError::operation)?
    {
        return Err(AppError::operation(
            "published semantic proposal is not a fast-forward of its source",
        ));
    }
    if engine
        .tree_oid(repository, tip)
        .map_err(AppError::operation)?
        != engine
            .tree_oid(repository, target)
            .map_err(AppError::operation)?
    {
        return Err(AppError::operation(
            "published semantic proposal no longer matches its accepted target tree",
        ));
    }
    Ok(())
}

pub fn reject_semantic_plan(
    plan_id: &str,
    dry_run: bool,
) -> Result<SemanticRejectReport, AppError> {
    let store = SyncStateStore::user_default()?;
    reject_semantic_plan_with_state_store(plan_id, dry_run, &store)
}

pub fn reject_semantic_plan_with_state_store(
    plan_id: &str,
    dry_run: bool,
    store: &SyncStateStore,
) -> Result<SemanticRejectReport, AppError> {
    let plan = load_semantic_plan_with_state_store(plan_id, store)?;
    let engine = GitCliEngine::default();
    let repository = engine
        .discover_repository(&plan.vault)
        .map_err(AppError::operation)?;
    if repository_state_key(&plan.vault) != plan.repository_key {
        return Err(AppError::operation(
            "semantic plan vault identity no longer matches its repository key",
        ));
    }
    let _lock = SemanticLock::acquire(&repository)?;
    let mut plan = load_semantic_plan_with_state_store(plan_id, store)?;
    if matches!(
        plan.status,
        SemanticPlanStatus::Applying | SemanticPlanStatus::Applied
    ) {
        return Err(AppError::operation(format!(
            "semantic plan {plan_id} is already being applied or applied"
        )));
    }
    if !matches!(
        plan.status,
        SemanticPlanStatus::Ready | SemanticPlanStatus::Rejecting | SemanticPlanStatus::Rejected
    ) {
        return Err(AppError::operation(format!(
            "semantic plan {plan_id} is not ready for rejection"
        )));
    }
    let tip = GitOid::parse(
        plan.proposal_tip
            .clone()
            .ok_or_else(|| AppError::operation("semantic plan has no proposal tip"))?,
    )
    .map_err(AppError::operation)?;
    let proposal_ref = GitRefName::parse(plan.proposal_ref.clone()).map_err(AppError::operation)?;
    let current = engine
        .read_ref(&repository, &proposal_ref)
        .map_err(AppError::operation)?;
    if plan.status == SemanticPlanStatus::Rejected {
        if current.is_some() {
            return Err(AppError::operation(
                "rejected semantic plan unexpectedly retains its proposal ref",
            ));
        }
        return Ok(semantic_reject_report(
            &plan,
            dry_run,
            SemanticRejectOutcome::AlreadyRejected,
            &tip,
        ));
    }
    if current.as_ref().is_some_and(|current| current != &tip) {
        return Err(AppError::operation(
            "semantic proposal ref changed before rejection",
        ));
    }
    if plan.status == SemanticPlanStatus::Ready && current.is_none() {
        return Err(AppError::operation(
            "semantic proposal ref is missing before rejection",
        ));
    }
    let report = semantic_reject_report(&plan, dry_run, SemanticRejectOutcome::Planned, &tip);
    if dry_run {
        return Ok(report);
    }
    if plan.status == SemanticPlanStatus::Ready {
        plan.version = SEMANTIC_PLAN_VERSION;
        plan.status = SemanticPlanStatus::Rejecting;
        save_plan(store, &plan, false)?;
    }
    if current.is_some()
        && engine
            .delete_ref(&repository, &proposal_ref, &tip)
            .map_err(AppError::operation)?
            != GitRefDeleteResult::Deleted
    {
        return Err(AppError::operation(
            "semantic proposal ref changed while rejecting the plan",
        ));
    }
    if engine
        .read_ref(&repository, &proposal_ref)
        .map_err(AppError::operation)?
        .is_some()
    {
        return Err(AppError::operation(
            "semantic proposal ref still exists after rejection",
        ));
    }
    plan.status = SemanticPlanStatus::Rejected;
    plan.version = SEMANTIC_PLAN_VERSION;
    save_plan(store, &plan, false)?;
    Ok(semantic_reject_report(
        &plan,
        false,
        SemanticRejectOutcome::Rejected,
        &tip,
    ))
}

pub fn apply_semantic_plan_with_state_store(
    plan_id: &str,
    dry_run: bool,
    store: &SyncStateStore,
) -> Result<SemanticApplyReport, AppError> {
    let mut plan = load_semantic_plan_with_state_store(plan_id, store)?;
    if !matches!(
        plan.status,
        SemanticPlanStatus::Ready | SemanticPlanStatus::Applying | SemanticPlanStatus::Applied
    ) {
        return Err(AppError::operation(format!(
            "semantic plan {plan_id} is not ready for application"
        )));
    }
    let engine = GitCliEngine::default();
    let repository = engine
        .discover_repository(&plan.vault)
        .map_err(AppError::operation)?;
    if repository_state_key(&plan.vault) != plan.repository_key {
        return Err(AppError::operation(
            "semantic plan vault identity no longer matches its repository key",
        ));
    }
    let _lock = SemanticLock::acquire(&repository)?;
    let source = GitOid::parse(plan.source_revision.clone()).map_err(AppError::operation)?;
    let target = GitOid::parse(plan.target_revision.clone()).map_err(AppError::operation)?;
    let tip = GitOid::parse(
        plan.proposal_tip
            .clone()
            .ok_or_else(|| AppError::operation("semantic plan has no proposal tip"))?,
    )
    .map_err(AppError::operation)?;
    let semantic_ref = GitRefName::parse(plan.semantic_ref.clone()).map_err(AppError::operation)?;
    let proposal_ref = GitRefName::parse(plan.proposal_ref.clone()).map_err(AppError::operation)?;
    let remote = GitRemote::parse(plan.remote.clone()).map_err(AppError::operation)?;
    let live_ref = GitRefName::parse(plan.live_ref.clone()).map_err(AppError::operation)?;
    let runtime = SemanticApplyRuntime {
        engine: &engine,
        repository: &repository,
        store,
        source: &source,
        target: &target,
        tip: &tip,
        semantic_ref: &semantic_ref,
        proposal_ref: &proposal_ref,
        remote: &remote,
        live_ref: &live_ref,
    };
    if matches!(
        plan.status,
        SemanticPlanStatus::Applying | SemanticPlanStatus::Applied
    ) && engine
        .read_ref(&repository, &semantic_ref)
        .map_err(AppError::operation)?
        .as_ref()
        == Some(&tip)
    {
        return finish_existing_semantic_application(plan_id, dry_run, &mut plan, &runtime);
    }
    validate_apply_inputs(
        &engine,
        &repository,
        &semantic_ref,
        &proposal_ref,
        &remote,
        &live_ref,
        &source,
        &target,
        &tip,
    )?;
    let report = semantic_apply_report(
        plan_id,
        dry_run,
        &semantic_ref,
        &source,
        &tip,
        &target,
        false,
    );
    if dry_run {
        return Ok(report);
    }
    plan.status = SemanticPlanStatus::Applying;
    plan.version = SEMANTIC_PLAN_VERSION;
    save_plan(store, &plan, false)?;
    if engine
        .compare_and_swap_ref(&repository, &semantic_ref, &tip, Some(&source))
        .map_err(AppError::operation)?
        != GitRefUpdateResult::Updated
    {
        return Err(AppError::operation(
            "semantic branch changed while applying the proposal; the plan is stale",
        ));
    }
    plan.status = SemanticPlanStatus::Applied;
    save_plan(store, &plan, false)?;
    release_semantic_proposal_ref(&engine, &repository, &proposal_ref, &tip)?;
    Ok(SemanticApplyReport {
        proposal_ref_released: true,
        ..report
    })
}

struct SemanticApplyRuntime<'a> {
    engine: &'a dyn GitEngine,
    repository: &'a GitRepository,
    store: &'a SyncStateStore,
    source: &'a GitOid,
    target: &'a GitOid,
    tip: &'a GitOid,
    semantic_ref: &'a GitRefName,
    proposal_ref: &'a GitRefName,
    remote: &'a GitRemote,
    live_ref: &'a GitRefName,
}

fn finish_existing_semantic_application(
    plan_id: &str,
    dry_run: bool,
    plan: &mut SemanticPlanReport,
    runtime: &SemanticApplyRuntime<'_>,
) -> Result<SemanticApplyReport, AppError> {
    let proposal_ref_present =
        validate_applied_inputs(runtime, plan.status == SemanticPlanStatus::Applied)?;
    if dry_run {
        return Ok(semantic_apply_report(
            plan_id,
            true,
            runtime.semantic_ref,
            runtime.source,
            runtime.tip,
            runtime.target,
            !proposal_ref_present,
        ));
    }
    if plan.status != SemanticPlanStatus::Applied {
        plan.version = SEMANTIC_PLAN_VERSION;
        plan.status = SemanticPlanStatus::Applied;
        save_plan(runtime.store, plan, false)?;
    }
    if proposal_ref_present {
        release_semantic_proposal_ref(
            runtime.engine,
            runtime.repository,
            runtime.proposal_ref,
            runtime.tip,
        )?;
    }
    Ok(semantic_apply_report(
        plan_id,
        false,
        runtime.semantic_ref,
        runtime.source,
        runtime.tip,
        runtime.target,
        true,
    ))
}

fn validate_plan_inputs(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    options: &SemanticPlanOptions,
    source: &GitOid,
    target: &GitOid,
) -> Result<(), AppError> {
    if engine
        .read_ref(repository, &options.semantic_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(source)
    {
        return Err(AppError::operation(format!(
            "semantic ref {} does not identify the selected source revision {source}",
            options.semantic_ref
        )));
    }
    if !engine
        .is_ancestor(repository, source, target)
        .map_err(AppError::operation)?
    {
        return Err(AppError::operation(
            "semantic source must be an ancestor of the accepted target",
        ));
    }
    validate_accepted_target(
        engine,
        repository,
        &options.remote,
        &options.live_ref,
        target,
    )
}

fn validate_accepted_target(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    remote: &GitRemote,
    live_ref: &GitRefName,
    target: &GitOid,
) -> Result<(), AppError> {
    let refs = GitSyncRefs::for_options(&GitSyncOptions {
        remote: remote.clone(),
        live_ref: live_ref.clone(),
        ..GitSyncOptions::default()
    })
    .map_err(AppError::operation)?;
    for (name, reference) in [
        ("local", &refs.local),
        ("fetched", &refs.fetched),
        ("pending", &refs.pending),
    ] {
        if engine
            .read_ref(repository, reference)
            .map_err(AppError::operation)?
            .as_ref()
            != Some(target)
        {
            return Err(AppError::operation(format!(
                "the {name} sync ref does not identify the selected accepted target {target}"
            )));
        }
    }
    if engine
        .remote_ref(repository, remote, live_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(target)
    {
        return Err(AppError::operation(
            "the remote live ref does not identify the selected accepted target",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_apply_inputs(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    semantic_ref: &GitRefName,
    proposal_ref: &GitRefName,
    remote: &GitRemote,
    live_ref: &GitRefName,
    source: &GitOid,
    target: &GitOid,
    tip: &GitOid,
) -> Result<(), AppError> {
    if engine
        .read_ref(repository, semantic_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(source)
    {
        return Err(AppError::operation(
            "semantic source branch moved after the plan was created",
        ));
    }
    let safety = engine
        .safety_state(repository)
        .map_err(AppError::operation)?;
    if safety.staged_changes || safety.operation.is_some() {
        return Err(AppError::operation(
            "cannot apply a semantic plan while the normal Git index is staged or an operation is in progress",
        ));
    }
    if engine
        .read_ref(repository, proposal_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(tip)
    {
        return Err(AppError::operation(
            "semantic proposal ref no longer identifies the recorded proposal tip",
        ));
    }
    if !engine
        .is_ancestor(repository, source, tip)
        .map_err(AppError::operation)?
    {
        return Err(AppError::operation(
            "semantic proposal is not a fast-forward of its source",
        ));
    }
    if engine
        .tree_oid(repository, tip)
        .map_err(AppError::operation)?
        != engine
            .tree_oid(repository, target)
            .map_err(AppError::operation)?
    {
        return Err(AppError::operation(
            "semantic proposal final tree no longer matches the selected live target",
        ));
    }
    validate_accepted_target(engine, repository, remote, live_ref, target)
}

fn validate_applied_inputs(
    runtime: &SemanticApplyRuntime<'_>,
    allow_missing_proposal_ref: bool,
) -> Result<bool, AppError> {
    let proposal_ref_present = match runtime
        .engine
        .read_ref(runtime.repository, runtime.proposal_ref)
        .map_err(AppError::operation)?
    {
        Some(current) if current == *runtime.tip => true,
        None if allow_missing_proposal_ref => false,
        None => {
            return Err(AppError::operation(
                "semantic proposal ref is missing while application is incomplete",
            ));
        }
        Some(_) => {
            return Err(AppError::operation(
                "semantic proposal ref no longer identifies the applied proposal tip",
            ));
        }
    };
    if runtime
        .engine
        .tree_oid(runtime.repository, runtime.tip)
        .map_err(AppError::operation)?
        != runtime
            .engine
            .tree_oid(runtime.repository, runtime.target)
            .map_err(AppError::operation)?
    {
        return Err(AppError::operation(
            "applied semantic proposal no longer matches the selected live target tree",
        ));
    }
    validate_accepted_target(
        runtime.engine,
        runtime.repository,
        runtime.remote,
        runtime.live_ref,
        runtime.target,
    )?;
    Ok(proposal_ref_present)
}

fn semantic_apply_report(
    plan_id: &str,
    dry_run: bool,
    semantic_ref: &GitRefName,
    source: &GitOid,
    tip: &GitOid,
    target: &GitOid,
    proposal_ref_released: bool,
) -> SemanticApplyReport {
    SemanticApplyReport {
        version: SEMANTIC_PLAN_VERSION,
        plan_id: plan_id.to_string(),
        dry_run,
        semantic_ref: semantic_ref.to_string(),
        previous_revision: source.to_string(),
        applied_revision: tip.to_string(),
        target_revision: target.to_string(),
        proposal_ref_released,
    }
}

fn release_semantic_proposal_ref(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    proposal_ref: &GitRefName,
    tip: &GitOid,
) -> Result<(), AppError> {
    if engine
        .delete_ref(repository, proposal_ref, tip)
        .map_err(AppError::operation)?
        != GitRefDeleteResult::Deleted
    {
        return Err(AppError::operation(
            "semantic proposal ref changed while releasing the applied plan",
        ));
    }
    if engine
        .read_ref(repository, proposal_ref)
        .map_err(AppError::operation)?
        .is_some()
    {
        return Err(AppError::operation(
            "semantic proposal ref still exists after release",
        ));
    }
    Ok(())
}

fn semantic_reject_report(
    plan: &SemanticPlanReport,
    dry_run: bool,
    outcome: SemanticRejectOutcome,
    tip: &GitOid,
) -> SemanticRejectReport {
    SemanticRejectReport {
        version: SEMANTIC_PLAN_VERSION,
        plan_id: plan.plan_id.clone(),
        dry_run,
        outcome,
        proposal_ref: plan.proposal_ref.clone(),
        proposal_tip: tip.to_string(),
        record_retained: true,
    }
}

fn construct_proposal(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    source: &GitOid,
    target: &GitOid,
    plan_id: &str,
    groups: Vec<PlannedSemanticGroup>,
) -> Result<(Vec<SemanticCommitProposal>, GitOid), AppError> {
    let mut parent = source.clone();
    let mut commits = Vec::with_capacity(groups.len());
    for (position, planned) in groups.into_iter().enumerate() {
        let PlannedSemanticGroup {
            group,
            message: proposed_message,
            paths,
            patch: planned_patch,
        } = planned;
        let tree = match &planned_patch {
            Some(patch) => engine
                .apply_patch_to_tree(repository, &parent, patch.as_bytes())
                .map_err(AppError::operation)?,
            None => engine
                .tree_with_paths(repository, &parent, target, &paths)
                .map_err(AppError::operation)?,
        };
        let message = semantic_message(&proposed_message, &group, plan_id, source, target);
        let commit = engine
            .create_commit(repository, &tree, std::slice::from_ref(&parent), &message)
            .map_err(AppError::operation)?;
        validate_intermediate_commit(
            engine,
            repository,
            &parent,
            &commit,
            target,
            &paths,
            planned_patch.is_none(),
        )?;
        let patch = engine
            .diff_patch(repository, &parent, &commit, &paths)
            .map_err(AppError::operation)?;
        commits.push(SemanticCommitProposal {
            position: position + 1,
            group,
            message,
            paths,
            from_revision: parent.to_string(),
            revision: Some(commit.to_string()),
            tree: Some(tree.to_string()),
            patch,
        });
        parent = commit;
    }
    Ok((commits, parent))
}

fn validate_intermediate_commit(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    parent: &GitOid,
    commit: &GitOid,
    target: &GitOid,
    paths: &[String],
    require_target_objects: bool,
) -> Result<(), AppError> {
    let mut actual = engine
        .changed_paths(repository, parent, commit)
        .map_err(AppError::operation)?;
    let mut expected = paths.to_vec();
    actual.sort();
    expected.sort();
    if actual != expected {
        return Err(AppError::operation(format!(
            "semantic intermediate commit {commit} changed paths outside its proposed group"
        )));
    }
    if !require_target_objects {
        return Ok(());
    }
    for path in paths {
        let proposed = engine
            .path_object(repository, commit, path)
            .map_err(AppError::operation)?;
        let accepted = engine
            .path_object(repository, target, path)
            .map_err(AppError::operation)?;
        let same = match (&proposed, &accepted) {
            (None, None) => true,
            (Some(proposed), Some(accepted)) => {
                proposed.oid == accepted.oid
                    && proposed.mode == accepted.mode
                    && proposed.kind == accepted.kind
            }
            _ => false,
        };
        if !same {
            return Err(AppError::operation(format!(
                "semantic intermediate commit {commit} does not reproduce accepted path {path}"
            )));
        }
    }
    Ok(())
}

fn preview_commits(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    source: &GitOid,
    target: &GitOid,
    groups: Vec<PlannedSemanticGroup>,
) -> Result<Vec<SemanticCommitProposal>, AppError> {
    groups
        .into_iter()
        .enumerate()
        .map(|(position, planned)| {
            let PlannedSemanticGroup {
                group,
                message: proposed_message,
                paths,
                patch: planned_patch,
            } = planned;
            let patch = match planned_patch {
                Some(patch) => patch,
                None => engine
                    .diff_patch(repository, source, target, &paths)
                    .map_err(AppError::operation)?,
            };
            Ok(SemanticCommitProposal {
                position: position + 1,
                message: semantic_message(&proposed_message, &group, "dry-run", source, target),
                group,
                paths,
                from_revision: source.to_string(),
                revision: None,
                tree: None,
                patch,
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn plan_agent_groups(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    source: &GitOid,
    target: &GitOid,
    mut changed_paths: Vec<String>,
    provider: &dyn SemanticAgentProvider,
    cancellation: &SyncCancellationToken,
) -> Result<(Vec<PlannedSemanticGroup>, Option<SemanticAgentIdentity>), AppError> {
    changed_paths.sort();
    changed_paths.dedup();
    let identity = provider.identity();
    validate_agent_identity(&identity)?;
    if changed_paths.is_empty() {
        return Ok((Vec::new(), Some(identity)));
    }
    semantic_cancellation_check(cancellation)?;
    let mut total_patch_bytes = 0_usize;
    let changes = changed_paths
        .iter()
        .map(|path| {
            let patch = engine
                .diff_patch(repository, source, target, std::slice::from_ref(path))
                .map_err(AppError::operation)?;
            total_patch_bytes = total_patch_bytes.saturating_add(patch.len());
            if total_patch_bytes > MAX_SEMANTIC_AGENT_PATCH_BYTES {
                return Err(AppError::operation(format!(
                    "semantic agent input patches exceed the {MAX_SEMANTIC_AGENT_PATCH_BYTES} byte limit"
                )));
            }
            Ok(SemanticAgentChange {
                path: path.clone(),
                patch,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let output = provider.propose(
        &SemanticAgentRequest {
            source_revision: source.to_string(),
            target_revision: target.to_string(),
            changes,
        },
        cancellation,
    )?;
    semantic_cancellation_check(cancellation)?;
    let groups = validate_agent_output(&changed_paths, output)?;
    Ok((groups, Some(identity)))
}

fn validate_agent_identity(identity: &SemanticAgentIdentity) -> Result<(), AppError> {
    for (field, value) in [
        ("provider", identity.provider.as_str()),
        ("model", identity.model.as_str()),
    ] {
        if value.trim().is_empty()
            || value.len() > MAX_SEMANTIC_AGENT_LABEL_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(AppError::operation(format!(
                "semantic agent {field} identity is invalid"
            )));
        }
    }
    if identity.prompt_contract_version == 0 {
        return Err(AppError::operation(
            "semantic agent prompt contract version must be greater than zero",
        ));
    }
    Ok(())
}

fn validate_agent_output(
    changed_paths: &[String],
    output: SemanticAgentOutput,
) -> Result<Vec<PlannedSemanticGroup>, AppError> {
    if output.commits.is_empty() || output.commits.len() > changed_paths.len() {
        return Err(AppError::operation(
            "semantic agent must propose between one commit and the number of changed paths",
        ));
    }
    let expected = changed_paths.iter().cloned().collect::<BTreeSet<_>>();
    let mut seen_paths = BTreeSet::new();
    let mut seen_groups = BTreeSet::new();
    let mut groups = Vec::with_capacity(output.commits.len());
    for commit in output.commits {
        validate_agent_group_label(&commit.group)?;
        if !seen_groups.insert(commit.group.clone()) {
            return Err(AppError::operation(format!(
                "semantic agent repeated group label `{}`",
                commit.group
            )));
        }
        validate_agent_message(&commit.message)?;
        if commit.paths.is_empty() {
            return Err(AppError::operation(format!(
                "semantic agent group `{}` has no paths",
                commit.group
            )));
        }
        let mut paths = commit.paths;
        let original_path_count = paths.len();
        paths.sort();
        paths.dedup();
        if paths.len() != original_path_count {
            return Err(AppError::operation(format!(
                "semantic agent group `{}` repeats a path",
                commit.group
            )));
        }
        for path in &paths {
            if !expected.contains(path) {
                return Err(AppError::operation(format!(
                    "semantic agent proposed unchanged or unknown path `{path}`"
                )));
            }
            if !seen_paths.insert(path.clone()) {
                return Err(AppError::operation(format!(
                    "semantic agent proposed path `{path}` more than once"
                )));
            }
        }
        groups.push(PlannedSemanticGroup {
            group: commit.group,
            message: commit.message.trim().to_string(),
            paths,
            patch: None,
        });
    }
    if seen_paths != expected {
        let missing = expected
            .difference(&seen_paths)
            .cloned()
            .collect::<Vec<_>>();
        return Err(AppError::operation(format!(
            "semantic agent omitted changed paths: {}",
            missing.join(", ")
        )));
    }
    Ok(groups)
}

fn validate_agent_group_label(group: &str) -> Result<(), AppError> {
    if group.trim().is_empty()
        || group.len() > MAX_SEMANTIC_AGENT_LABEL_BYTES
        || group.chars().any(char::is_control)
    {
        return Err(AppError::operation(
            "semantic agent group labels must be non-empty bounded single-line text",
        ));
    }
    Ok(())
}

fn validate_agent_message(message: &str) -> Result<(), AppError> {
    if message.trim().is_empty()
        || message.len() > MAX_SEMANTIC_AGENT_MESSAGE_BYTES
        || message
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
        || message.lines().any(|line| {
            line.trim_start()
                .to_ascii_lowercase()
                .starts_with("vulcan-semantic-")
        })
    {
        return Err(AppError::operation(
            "semantic agent commit messages must be bounded text without reserved Vulcan trailers",
        ));
    }
    Ok(())
}

fn semantic_cancellation_check(cancellation: &SyncCancellationToken) -> Result<(), AppError> {
    if cancellation.is_cancelled() {
        Err(AppError::operation("semantic agent planning was cancelled"))
    } else {
        Ok(())
    }
}

fn deterministic_groups(
    mut paths: Vec<String>,
    grouping: SemanticGrouping,
) -> Vec<PlannedSemanticGroup> {
    paths.sort();
    paths.dedup();
    let mut groups = BTreeMap::<String, Vec<String>>::new();
    for path in paths {
        let group = match grouping {
            SemanticGrouping::TopLevel => path
                .split_once('/')
                .map_or_else(|| path.clone(), |(top, _)| top.to_string()),
            SemanticGrouping::File | SemanticGrouping::Change | SemanticGrouping::Hunk => {
                path.clone()
            }
            SemanticGrouping::All => "all changes".to_string(),
            SemanticGrouping::Agent => "agent plan".to_string(),
        };
        groups.entry(group).or_default().push(path);
    }
    groups
        .into_iter()
        .map(|(group, paths)| PlannedSemanticGroup {
            message: format!("Update {group}"),
            group,
            paths,
            patch: None,
        })
        .collect()
}

fn deterministic_change_groups(mut changes: Vec<GitChange>) -> Vec<PlannedSemanticGroup> {
    changes.sort_by(|left, right| {
        change_precedence(left.kind)
            .cmp(&change_precedence(right.kind))
            .then_with(|| left.source_path.cmp(&right.source_path))
            .then_with(|| left.path.cmp(&right.path))
    });
    changes.dedup_by(|left, right| {
        left.kind == right.kind && left.path == right.path && left.source_path == right.source_path
    });

    let renames = changes
        .iter()
        .filter(|change| change.kind == GitChangeKind::Renamed)
        .cloned()
        .collect::<Vec<_>>();
    let mut rename_groups = rename_components(&renames);
    let mut groups = Vec::new();
    for change in changes {
        if change.kind == GitChangeKind::Renamed {
            continue;
        }
        let (verb, group) = match change.kind {
            GitChangeKind::Added => ("Add", format!("add {}", change.path)),
            GitChangeKind::Modified => ("Update", format!("update {}", change.path)),
            GitChangeKind::Deleted => ("Remove", format!("remove {}", change.path)),
            GitChangeKind::TypeChanged => ("Change type of", format!("type {}", change.path)),
            GitChangeKind::Renamed => unreachable!(),
        };
        groups.push((
            change_precedence(change.kind),
            change.path.clone(),
            PlannedSemanticGroup {
                group,
                message: format!("{verb} {}", change.path),
                paths: vec![change.path],
                patch: None,
            },
        ));
    }
    for component in rename_groups.drain(..) {
        let mut paths = BTreeSet::new();
        for change in &component {
            paths.insert(
                change
                    .source_path
                    .as_ref()
                    .expect("rename changes have a source")
                    .clone(),
            );
            paths.insert(change.path.clone());
        }
        let first = &component[0];
        let (group, message) = if component.len() == 1 {
            let source = first
                .source_path
                .as_ref()
                .expect("rename changes have a source");
            (
                format!("rename {source} -> {}", first.path),
                format!("Rename {source} to {}", first.path),
            )
        } else {
            let label = paths.iter().next().expect("rename component has paths");
            (
                format!("rename set {label}"),
                format!("Rename {} related paths", component.len()),
            )
        };
        groups.push((
            change_precedence(GitChangeKind::Renamed),
            group.clone(),
            PlannedSemanticGroup {
                group,
                message,
                paths: paths.into_iter().collect(),
                patch: None,
            },
        ));
    }
    groups.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    groups.into_iter().map(|(_, _, group)| group).collect()
}

fn deterministic_hunk_groups(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    source: &GitOid,
    target: &GitOid,
    changes: Vec<GitChange>,
) -> Result<Vec<PlannedSemanticGroup>, AppError> {
    let modified_paths = changes
        .iter()
        .filter(|change| change.kind == GitChangeKind::Modified)
        .map(|change| change.path.clone())
        .collect::<BTreeSet<_>>();
    let mut groups = Vec::new();
    for group in deterministic_change_groups(changes) {
        if group.paths.len() != 1 || !modified_paths.contains(&group.paths[0]) {
            groups.push(group);
            continue;
        }
        let path = &group.paths[0];
        let file_diff = engine
            .diff_patch(repository, source, target, std::slice::from_ref(path))
            .map_err(AppError::operation)?;
        let hunks = split_modified_patch_hunks(&file_diff);
        if hunks.len() < 2 {
            groups.push(group);
            continue;
        }
        let count = hunks.len();
        for (index, patch) in hunks.into_iter().enumerate() {
            let position = index + 1;
            groups.push(PlannedSemanticGroup {
                group: format!("hunk {position}/{count} {path}"),
                message: format!("Update {path} (hunk {position}/{count})"),
                paths: vec![path.clone()],
                patch: Some(patch),
            });
        }
    }
    Ok(groups)
}

fn split_modified_patch_hunks(patch: &str) -> Vec<String> {
    if !patch.ends_with('\n')
        || [
            "new file mode ",
            "deleted file mode ",
            "old mode ",
            "new mode ",
            "rename from ",
            "rename to ",
            "copy from ",
            "copy to ",
            "Binary files ",
            "GIT binary patch",
        ]
        .iter()
        .any(|marker| patch.lines().any(|line| line.starts_with(marker)))
    {
        return vec![patch.to_string()];
    }
    let lines = patch.split_inclusive('\n').collect::<Vec<_>>();
    if lines
        .iter()
        .filter(|line| line.starts_with("diff --git "))
        .count()
        != 1
    {
        return vec![patch.to_string()];
    }
    let hunk_starts = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.starts_with("@@ ").then_some(index))
        .collect::<Vec<_>>();
    if hunk_starts.len() < 2 {
        return vec![patch.to_string()];
    }
    let header = lines[..hunk_starts[0]].concat();
    hunk_starts
        .iter()
        .enumerate()
        .map(|(position, start)| {
            let end = hunk_starts
                .get(position + 1)
                .copied()
                .unwrap_or(lines.len());
            format!("{header}{}", lines[*start..end].concat())
        })
        .collect()
}

const fn change_precedence(kind: GitChangeKind) -> u8 {
    match kind {
        GitChangeKind::Deleted => 0,
        GitChangeKind::Renamed => 1,
        GitChangeKind::TypeChanged => 2,
        GitChangeKind::Added => 3,
        GitChangeKind::Modified => 4,
    }
}

fn rename_components(renames: &[GitChange]) -> Vec<Vec<GitChange>> {
    let mut remaining = renames.to_vec();
    let mut components = Vec::new();
    while let Some(seed) = remaining.pop() {
        let mut component = vec![seed];
        let mut paths = component_paths(&component);
        loop {
            let Some(index) = remaining.iter().position(|change| {
                paths.contains(&change.path)
                    || change
                        .source_path
                        .as_ref()
                        .is_some_and(|source| paths.contains(source))
            }) else {
                break;
            };
            component.push(remaining.remove(index));
            paths = component_paths(&component);
        }
        component.sort_by(|left, right| {
            left.source_path
                .cmp(&right.source_path)
                .then_with(|| left.path.cmp(&right.path))
        });
        components.push(component);
    }
    components.sort_by(|left, right| {
        left[0]
            .source_path
            .cmp(&right[0].source_path)
            .then_with(|| left[0].path.cmp(&right[0].path))
    });
    components
}

fn component_paths(changes: &[GitChange]) -> BTreeSet<String> {
    changes
        .iter()
        .flat_map(|change| {
            change
                .source_path
                .iter()
                .cloned()
                .chain(std::iter::once(change.path.clone()))
        })
        .collect()
}

fn semantic_message(
    proposed: &str,
    group: &str,
    plan_id: &str,
    source: &GitOid,
    target: &GitOid,
) -> String {
    format!(
        "{}\n\nVulcan-Semantic-Version: 1\nVulcan-Semantic-Plan: {plan_id}\nVulcan-Semantic-Source: {source}\nVulcan-Semantic-Target: {target}\nVulcan-Semantic-Group: {group}\n",
        proposed.trim()
    )
}

fn initial_plan_report(
    vault: &Path,
    options: &SemanticPlanOptions,
    source: &GitOid,
    target: &GitOid,
    plan_id: &str,
    proposal_ref: &GitRefName,
    agent_identity: Option<SemanticAgentIdentity>,
) -> SemanticPlanReport {
    SemanticPlanReport {
        version: SEMANTIC_PLAN_VERSION,
        plan_id: plan_id.to_string(),
        status: SemanticPlanStatus::Preview,
        dry_run: options.dry_run,
        agent: options.agent,
        agent_identity,
        grouping: if options.agent {
            SemanticGrouping::Agent
        } else {
            options.grouping
        },
        vault: vault.to_path_buf(),
        repository_key: repository_state_key(vault),
        semantic_ref: options.semantic_ref.to_string(),
        proposal_ref: proposal_ref.to_string(),
        remote: options.remote.to_string(),
        live_ref: options.live_ref.to_string(),
        source_revision: source.to_string(),
        target_revision: target.to_string(),
        proposal_tip: None,
        published_revision: None,
        published_remote_previous_revision: None,
        commits: Vec::new(),
        validation: SemanticPlanValidation {
            source_ref_matches: true,
            source_is_ancestor: true,
            target_is_accepted_live: true,
            final_tree_matches_target: false,
        },
    }
}

fn semantic_proposal_ref(plan_id: &str) -> Result<GitRefName, AppError> {
    namespace_semantic_proposal_ref(plan_id).map_err(AppError::operation)
}

fn semantic_plan_path(store: &SyncStateStore, plan_id: &str) -> PathBuf {
    store
        .root()
        .join("_semantic_plans")
        .join(format!("{plan_id}.json"))
}

fn save_plan(
    store: &SyncStateStore,
    plan: &SemanticPlanReport,
    create: bool,
) -> Result<(), AppError> {
    validate_plan_id(&plan.plan_id)?;
    let path = semantic_plan_path(store, &plan.plan_id);
    let parent = path
        .parent()
        .ok_or_else(|| AppError::operation("semantic plan path has no parent"))?;
    fs::create_dir_all(parent).map_err(AppError::operation)?;
    let bytes = serde_json::to_vec_pretty(plan).map_err(AppError::operation)?;
    if bytes.len() as u64 > MAX_SEMANTIC_PLAN_BYTES {
        return Err(AppError::operation(format!(
            "semantic plan exceeds the {MAX_SEMANTIC_PLAN_BYTES} byte limit"
        )));
    }
    let mut temporary = NamedTempFile::new_in(parent).map_err(AppError::operation)?;
    temporary.write_all(&bytes).map_err(AppError::operation)?;
    temporary.write_all(b"\n").map_err(AppError::operation)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AppError::operation)?;
    if create {
        temporary
            .persist_noclobber(path)
            .map_err(|error| AppError::operation(error.error))?;
    } else {
        temporary
            .persist(path)
            .map_err(|error| AppError::operation(error.error))?;
    }
    Ok(())
}

fn validate_loaded_plan(
    plan_id: &str,
    path: &Path,
    plan: &SemanticPlanReport,
) -> Result<(), AppError> {
    if !(1..=SEMANTIC_PLAN_VERSION).contains(&plan.version) {
        return Err(AppError::operation(format!(
            "unsupported semantic plan version {} at {}",
            plan.version,
            path.display()
        )));
    }
    if plan.plan_id != plan_id {
        return Err(AppError::operation(format!(
            "semantic plan identity mismatch at {}",
            path.display()
        )));
    }
    if semantic_proposal_ref(plan_id)?.as_str() != plan.proposal_ref {
        return Err(AppError::operation(format!(
            "semantic proposal ref mismatch at {}",
            path.display()
        )));
    }
    match (
        plan.published_revision.as_deref(),
        plan.published_remote_previous_revision.as_deref(),
    ) {
        (None, None) => {}
        (Some(published), Some(previous)) => {
            if plan.status != SemanticPlanStatus::Applied {
                return Err(AppError::operation(format!(
                    "semantic publication metadata requires applied status at {}",
                    path.display()
                )));
            }
            if plan.proposal_tip.as_deref() != Some(published) {
                return Err(AppError::operation(format!(
                    "semantic published revision differs from the proposal tip at {}",
                    path.display()
                )));
            }
            if previous != plan.source_revision {
                return Err(AppError::operation(format!(
                    "semantic publication lease differs from the plan source at {}",
                    path.display()
                )));
            }
        }
        _ => {
            return Err(AppError::operation(format!(
                "semantic publication metadata is incomplete at {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn validate_plan_id(plan_id: &str) -> Result<(), AppError> {
    let parsed = Ulid::from_string(&plan_id.to_ascii_uppercase())
        .map_err(|_| AppError::operation("semantic plan ID must be a 26-character ULID"))?;
    if parsed.to_string().to_ascii_lowercase() != plan_id {
        return Err(AppError::operation(
            "semantic plan ID must use canonical lowercase Crockford Base32",
        ));
    }
    Ok(())
}

struct SemanticLock {
    _file: File,
}

impl SemanticLock {
    fn acquire(repository: &GitRepository) -> Result<Self, AppError> {
        let path = repository.git_dir.join("vulcan-sync/sync.lock");
        fs::create_dir_all(
            path.parent()
                .expect("the sync repository lock always has a parent"),
        )
        .map_err(AppError::operation)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(AppError::operation)?;
        file.try_lock_exclusive().map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                AppError::operation("another synchronization operation holds the repository lock")
            } else {
                AppError::operation(error)
            }
        })?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        deterministic_change_groups, deterministic_groups, load_semantic_plan_with_state_store,
        semantic_plan_path, semantic_proposal_ref, split_modified_patch_hunks,
        validate_agent_output, validate_loaded_plan, validate_plan_id, SemanticAgentCommit,
        SemanticAgentOutput, SemanticGrouping, SemanticPlanReport, SemanticPlanStatus,
        SemanticPlanValidation,
    };
    use crate::sync_state::SyncStateStore;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;
    use vulcan_sync::{GitChange, GitChangeKind};

    #[test]
    fn deterministic_groups_are_top_level_and_sorted() {
        let groups = deterministic_groups(
            vec![
                "Z.md".to_string(),
                "Area/Two.md".to_string(),
                "Area/One.md".to_string(),
                "A.md".to_string(),
            ],
            SemanticGrouping::TopLevel,
        );
        assert_eq!(
            groups
                .iter()
                .map(|group| group.group.as_str())
                .collect::<Vec<_>>(),
            ["A.md", "Area", "Z.md"]
        );
        assert_eq!(groups[1].paths, ["Area/One.md", "Area/Two.md"]);
    }

    #[test]
    fn deterministic_grouping_supports_file_and_all_strategies() {
        let paths = vec!["Area/Two.md".to_string(), "Area/One.md".to_string()];
        let by_file = deterministic_groups(paths.clone(), SemanticGrouping::File);
        assert_eq!(
            by_file
                .iter()
                .map(|group| group.group.as_str())
                .collect::<Vec<_>>(),
            ["Area/One.md", "Area/Two.md"]
        );
        let all = deterministic_groups(paths, SemanticGrouping::All);
        assert_eq!(all[0].group, "all changes");
        assert_eq!(all[0].paths, ["Area/One.md", "Area/Two.md"]);
    }

    #[test]
    fn change_grouping_orders_dependencies_and_keeps_renames_atomic() {
        let groups = deterministic_change_groups(vec![
            GitChange {
                kind: GitChangeKind::Added,
                path: "Folder/Note.md".to_string(),
                source_path: None,
                similarity: None,
            },
            GitChange {
                kind: GitChangeKind::Renamed,
                path: "New.md".to_string(),
                source_path: Some("Old.md".to_string()),
                similarity: Some(100),
            },
            GitChange {
                kind: GitChangeKind::Deleted,
                path: "Folder".to_string(),
                source_path: None,
                similarity: None,
            },
            GitChange {
                kind: GitChangeKind::Modified,
                path: "Root.md".to_string(),
                source_path: None,
                similarity: None,
            },
        ]);
        assert_eq!(
            groups
                .iter()
                .map(|group| group.message.as_str())
                .collect::<Vec<_>>(),
            [
                "Remove Folder",
                "Rename Old.md to New.md",
                "Add Folder/Note.md",
                "Update Root.md",
            ]
        );
        assert_eq!(groups[1].paths, ["New.md", "Old.md"]);
    }

    #[test]
    fn modified_text_patches_split_only_at_real_hunk_boundaries() {
        let patch = concat!(
            "diff --git a/Note.md b/Note.md\n",
            "index 1111111..2222222 100644\n",
            "--- a/Note.md\n",
            "+++ b/Note.md\n",
            "@@ -1,4 +1,4 @@\n",
            "-before one\n",
            "+after one\n",
            " context\n",
            "@@ -20,4 +20,4 @@ context\n",
            "-before two\n",
            "+after two\n",
        );
        let hunks = split_modified_patch_hunks(patch);
        assert_eq!(hunks.len(), 2);
        assert!(hunks[0].contains("before one"));
        assert!(!hunks[0].contains("before two"));
        assert!(hunks[1].contains("before two"));
        assert!(hunks
            .iter()
            .all(|hunk| hunk.starts_with("diff --git a/Note.md b/Note.md\n")));

        let deletion = patch.replacen(
            "index 1111111..2222222 100644\n",
            "deleted file mode 100644\n",
            1,
        );
        assert_eq!(split_modified_patch_hunks(&deletion), [deletion]);
    }

    #[test]
    fn semantic_agent_output_requires_exact_once_only_path_coverage() {
        let changed = vec!["A.md".to_string(), "B.md".to_string()];
        let valid = validate_agent_output(
            &changed,
            SemanticAgentOutput {
                commits: vec![
                    SemanticAgentCommit {
                        group: "foundation".to_string(),
                        message: "Add the foundation".to_string(),
                        paths: vec!["B.md".to_string()],
                    },
                    SemanticAgentCommit {
                        group: "entrypoint".to_string(),
                        message: "Connect the entrypoint".to_string(),
                        paths: vec!["A.md".to_string()],
                    },
                ],
            },
        )
        .expect("valid agent grouping");
        assert_eq!(valid[0].group, "foundation");
        assert_eq!(valid[1].group, "entrypoint");

        for output in [
            SemanticAgentOutput {
                commits: vec![SemanticAgentCommit {
                    group: "partial".to_string(),
                    message: "Only one path".to_string(),
                    paths: vec!["A.md".to_string()],
                }],
            },
            SemanticAgentOutput {
                commits: vec![SemanticAgentCommit {
                    group: "unknown".to_string(),
                    message: "Invent a path".to_string(),
                    paths: vec!["A.md".to_string(), "C.md".to_string()],
                }],
            },
        ] {
            assert!(validate_agent_output(&changed, output).is_err());
        }
    }

    #[test]
    fn semantic_agent_messages_cannot_spoof_provenance_trailers() {
        let output = SemanticAgentOutput {
            commits: vec![SemanticAgentCommit {
                group: "notes".to_string(),
                message: "Update notes\n\nVulcan-Semantic-Target: forged".to_string(),
                paths: vec!["A.md".to_string()],
            }],
        };
        assert!(validate_agent_output(&["A.md".to_string()], output).is_err());
    }

    #[cfg(feature = "web")]
    #[test]
    fn openai_semantic_provider_sends_bounded_patches_and_parses_exact_groups() {
        use super::{
            OpenAiCompatibleSemanticProvider, SemanticAgentChange, SemanticAgentProvider,
            SemanticAgentRequest,
        };
        use crate::sync::SyncCancellationToken;
        use std::io::{BufRead, BufReader, Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("request");
            let mut reader = BufReader::new(stream);
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("header");
                if line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
            assert!(headers
                .to_ascii_lowercase()
                .contains("authorization: bearer secret"));
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_string)
                })
                .and_then(|value| value.trim().parse::<usize>().ok())
                .expect("content length");
            let mut body = vec![0; length];
            reader.read_exact(&mut body).expect("request body");
            let body: serde_json::Value = serde_json::from_slice(&body).expect("request JSON");
            assert_eq!(body["model"], "fixture-model");
            assert!(body["messages"][1]["content"]
                .as_str()
                .is_some_and(|content| content.contains("diff --git") && content.contains("A.md")));
            let content = serde_json::json!({
                "commits": [{
                    "group": "notes",
                    "message": "Update the notes",
                    "paths": ["A.md"]
                }]
            })
            .to_string();
            let response = serde_json::json!({
                "id": "response-id",
                "choices": [{"message": {"role": "assistant", "content": content}}]
            })
            .to_string();
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            )
            .expect("response");
        });
        let provider = OpenAiCompatibleSemanticProvider::new(
            &format!("http://{address}/v1"),
            "fixture-model",
            Some("secret".to_string()),
        )
        .expect("provider");
        let output = provider
            .propose(
                &SemanticAgentRequest {
                    source_revision: "0".repeat(40),
                    target_revision: "1".repeat(40),
                    changes: vec![SemanticAgentChange {
                        path: "A.md".to_string(),
                        patch: "diff --git a/A.md b/A.md".to_string(),
                    }],
                },
                &SyncCancellationToken::default(),
            )
            .expect("provider output");
        assert_eq!(output.commits[0].group, "notes");
        assert_eq!(output.commits[0].paths, ["A.md"]);
        server.join().expect("server thread");
    }

    #[test]
    fn semantic_plan_ids_are_canonical_and_ref_safe() {
        let id = "01arz3ndektsv4rrffq69g5fav";
        validate_plan_id(id).expect("canonical plan ID");
        assert_eq!(
            semantic_proposal_ref(id).expect("proposal ref").as_str(),
            "refs/vulcan/proposals/semantic/01arz3ndektsv4rrffq69g5fav"
        );
        assert!(validate_plan_id("01ARZ3NDEKTSV4RRFFQ69G5FAV").is_err());
        assert!(validate_plan_id("../unsafe").is_err());
    }

    #[test]
    fn version_one_semantic_plan_records_remain_readable() {
        let id = "01arz3ndektsv4rrffq69g5fav";
        let plan = SemanticPlanReport {
            version: 1,
            plan_id: id.to_string(),
            status: SemanticPlanStatus::Ready,
            dry_run: false,
            agent: false,
            agent_identity: None,
            grouping: SemanticGrouping::TopLevel,
            vault: "/tmp/vault".into(),
            repository_key: "repository".to_string(),
            semantic_ref: "refs/heads/main".to_string(),
            proposal_ref: semantic_proposal_ref(id).expect("proposal ref").to_string(),
            remote: "origin".to_string(),
            live_ref: "refs/heads/__vulcan-sync/live".to_string(),
            source_revision: "0".repeat(40),
            target_revision: "1".repeat(40),
            proposal_tip: Some("2".repeat(40)),
            published_revision: None,
            published_remote_previous_revision: None,
            commits: Vec::new(),
            validation: SemanticPlanValidation {
                source_ref_matches: true,
                source_is_ancestor: true,
                target_is_accepted_live: true,
                final_tree_matches_target: true,
            },
        };

        validate_loaded_plan(id, Path::new("legacy-plan.json"), &plan).expect("version-one plan");
        let mut legacy = serde_json::to_value(&plan).expect("serialize legacy fixture");
        legacy
            .as_object_mut()
            .expect("plan object")
            .remove("grouping");
        let decoded: SemanticPlanReport =
            serde_json::from_value(legacy).expect("decode plan without grouping field");
        assert_eq!(decoded.grouping, SemanticGrouping::TopLevel);
    }

    #[test]
    fn semantic_publication_metadata_is_complete_and_bound_to_the_plan() {
        let id = "01arz3ndektsv4rrffq69g5fav";
        let mut plan = SemanticPlanReport {
            version: 6,
            plan_id: id.to_string(),
            status: SemanticPlanStatus::Applied,
            dry_run: false,
            agent: false,
            agent_identity: None,
            grouping: SemanticGrouping::TopLevel,
            vault: "/tmp/vault".into(),
            repository_key: "repository".to_string(),
            semantic_ref: "refs/heads/main".to_string(),
            proposal_ref: semantic_proposal_ref(id).expect("proposal ref").to_string(),
            remote: "origin".to_string(),
            live_ref: "refs/heads/__vulcan-sync/live".to_string(),
            source_revision: "0".repeat(40),
            target_revision: "1".repeat(40),
            proposal_tip: Some("2".repeat(40)),
            published_revision: Some("2".repeat(40)),
            published_remote_previous_revision: Some("0".repeat(40)),
            commits: Vec::new(),
            validation: SemanticPlanValidation {
                source_ref_matches: true,
                source_is_ancestor: true,
                target_is_accepted_live: true,
                final_tree_matches_target: true,
            },
        };
        validate_loaded_plan(id, Path::new("plan.json"), &plan).expect("valid publication");

        plan.published_remote_previous_revision = None;
        assert!(validate_loaded_plan(id, Path::new("plan.json"), &plan).is_err());
        plan.published_remote_previous_revision = Some("0".repeat(40));
        plan.published_revision = Some("3".repeat(40));
        assert!(validate_loaded_plan(id, Path::new("plan.json"), &plan).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn semantic_plan_loader_rejects_symlinked_records() {
        use std::os::unix::fs::symlink;

        let temporary = tempdir().expect("temporary directory");
        let store = SyncStateStore::at(temporary.path().join("state"));
        let id = "01arz3ndektsv4rrffq69g5fav";
        let path = semantic_plan_path(&store, id);
        fs::create_dir_all(path.parent().expect("plan parent")).expect("plan parent");
        let outside = temporary.path().join("outside.json");
        fs::write(&outside, b"{}\n").expect("outside record");
        symlink(&outside, &path).expect("plan symlink");

        let error =
            load_semantic_plan_with_state_store(id, &store).expect_err("symlinked plan must fail");
        assert!(error.to_string().contains("is not a regular file"));
    }
}
