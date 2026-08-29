//! Isolated, review-first agent resolution proposals for preserved Git conflicts.

use crate::sync_conflicts::{
    verify_preserved_conflict_refs, SyncConflictRecord, SyncConflictStore,
};
use crate::sync_state::{repository_state_key, SyncStateStore};
use crate::AppError;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use vulcan_core::VaultPaths;
use vulcan_core::{resolve_permission_profile, PermissionGuard, ProfilePermissionGuard};
use vulcan_sync::{
    GitContentMergeResolutionRequest, GitEngine, GitOid, GitResolvedPath, SyncCancellationToken,
};

pub const RESOLUTION_PROPOSAL_VERSION: u32 = 1;
pub const RESOLUTION_AGENT_TOOL_CONTRACT_VERSION: u32 = 1;
const MAX_AGENT_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_AGENT_TOTAL_BYTES: usize = 32 * 1024 * 1024;
const MAX_PROPOSAL_RECORD_BYTES: usize = 32 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_CONTEXT_PATHS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionAgentIdentity {
    pub provider: String,
    pub model: String,
    pub prompt_contract_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionAgentSide {
    pub revision: Option<String>,
    pub mode: Option<String>,
    pub content: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionAgentFile {
    pub path: String,
    pub base: ResolutionAgentSide,
    pub local: ResolutionAgentSide,
    pub remote: ResolutionAgentSide,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionAgentRequest {
    pub conflict_id: String,
    pub policy_version: u32,
    pub policy_hash: String,
    pub files: Vec<ResolutionAgentFile>,
    pub focused_context: Vec<String>,
    pub broad_context_allowed: bool,
    pub tool_contract_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionAgentPathOutput {
    pub path: String,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionAgentOutput {
    pub explanation: String,
    pub referenced_context: Vec<String>,
    pub paths: Vec<ResolutionAgentPathOutput>,
}

pub trait ResolutionAgentProvider {
    fn identity(&self) -> ResolutionAgentIdentity;

    fn propose(
        &self,
        request: &ResolutionAgentRequest,
        cancellation: &SyncCancellationToken,
    ) -> Result<ResolutionAgentOutput, AppError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionProposalOptions {
    pub permission_profile: String,
    pub focused_context: Vec<String>,
    pub allow_broad_context: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionProposalStatus {
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionProposalValidationCheck {
    ConflictInputsPreserved,
    PermissionProfileNamed,
    FocusedContextBounded,
    OutputPathsExact,
    OutputBytesBounded,
    NoFileDeletion,
    ExactTreeObjects,
    WorktreeUnchanged,
    RefsUnchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionProposalPath {
    pub path: String,
    pub mode: String,
    pub content_hash: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionProposal {
    pub version: u32,
    pub proposal_id: String,
    pub status: ResolutionProposalStatus,
    pub conflict_id: String,
    pub repository_key: String,
    pub base_revision: String,
    pub local_revision: String,
    pub remote_revision: String,
    pub policy_version: u32,
    pub policy_hash: String,
    pub provider: String,
    pub model: String,
    pub prompt_contract_version: u32,
    pub tool_contract_version: u32,
    pub permission_profile: String,
    pub broad_context_allowed: bool,
    pub explanation: String,
    pub referenced_context: Vec<String>,
    pub proposal_tree: String,
    pub patch: String,
    pub paths: Vec<ResolutionProposalPath>,
    pub validation: Vec<ResolutionProposalValidationCheck>,
}

pub fn create_resolution_proposal_with_provider(
    paths: &VaultPaths,
    conflict_id: &str,
    options: &ResolutionProposalOptions,
    provider: &dyn ResolutionAgentProvider,
    cancellation: &SyncCancellationToken,
    state_store: &SyncStateStore,
) -> Result<ResolutionProposal, AppError> {
    validate_options(options)?;
    let selection = resolve_permission_profile(paths, Some(&options.permission_profile))
        .map_err(AppError::operation)?;
    let permission_guard = ProfilePermissionGuard::new(paths, selection);
    permission_guard.check_git().map_err(AppError::operation)?;
    for path in &options.focused_context {
        permission_guard
            .check_read_path(path)
            .map_err(AppError::operation)?;
    }
    cancellation_check(cancellation)?;
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let repository_key = repository_state_key(&vault);
    let conflict_store = SyncConflictStore::from_state_store(state_store);
    let record = conflict_store.get(&repository_key, conflict_id)?;
    validate_agent_conflict_scope(&record)?;
    for path in &record.paths {
        permission_guard
            .check_read_path(&path.path)
            .map_err(AppError::operation)?;
    }
    let base_revision = record
        .base_revision
        .as_deref()
        .ok_or_else(|| AppError::operation("agent resolution requires one merge base"))?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let _lock = ProposalLock::acquire(&repository)?;
    ensure_no_existing_proposal(state_store, &repository_key, conflict_id)?;
    verify_preserved_conflict_refs(&engine, &repository, &record)?;
    let refs_before = preserved_ref_snapshot(&engine, &repository, &record)?;
    let local_revision = GitOid::parse(&record.local_revision).map_err(AppError::operation)?;
    let worktree_before = engine
        .snapshot_worktree_tree(&repository, Some(&local_revision))
        .map_err(AppError::operation)?;
    let request = build_agent_request(&engine, &repository, &record, options)?;
    cancellation_check(cancellation)?;
    let identity = provider.identity();
    validate_identity(&identity)?;
    let output = provider.propose(&request, cancellation)?;
    cancellation_check(cancellation)?;
    let prepared = prepare_output(&engine, &repository, &record, output)?;
    let proposal_tree = engine
        .resolve_merge_tree_with_paths(
            &repository,
            &GitContentMergeResolutionRequest {
                base: GitOid::parse(base_revision).map_err(AppError::operation)?,
                accepted_remote: GitOid::parse(&record.remote_revision)
                    .map_err(AppError::operation)?,
                local_candidate: GitOid::parse(&record.local_revision)
                    .map_err(AppError::operation)?,
                paths: prepared.git_paths.clone(),
            },
        )
        .map_err(AppError::operation)?;
    verify_tree_objects(&engine, &repository, &proposal_tree, &prepared.git_paths)?;
    verify_no_external_mutation(
        &engine,
        &repository,
        &record,
        &worktree_before,
        &refs_before,
    )?;
    let patch = engine
        .diff_patch(
            &repository,
            &GitOid::parse(&record.remote_revision).map_err(AppError::operation)?,
            &proposal_tree,
            &record
                .paths
                .iter()
                .map(|path| path.path.clone())
                .collect::<Vec<_>>(),
        )
        .map_err(AppError::operation)?;
    let proposal = assemble_proposal(
        &record,
        repository_key,
        identity,
        options,
        prepared,
        &proposal_tree,
        patch,
    )?;
    save_proposal(state_store, &proposal)?;
    Ok(proposal)
}

pub fn load_resolution_proposal(
    state_store: &SyncStateStore,
    repository_key: &str,
    conflict_id: &str,
    proposal_id: &str,
) -> Result<ResolutionProposal, AppError> {
    for (label, value) in [
        ("repository key", repository_key),
        ("conflict ID", conflict_id),
        ("proposal ID", proposal_id),
    ] {
        validate_hex_id(label, value)?;
    }
    let path = proposal_path(state_store, repository_key, conflict_id, proposal_id);
    let metadata = fs::metadata(&path).map_err(AppError::operation)?;
    if metadata.len() > MAX_PROPOSAL_RECORD_BYTES as u64 {
        return Err(AppError::operation(
            "resolution proposal exceeds its byte limit",
        ));
    }
    let proposal: ResolutionProposal =
        serde_json::from_slice(&fs::read(&path).map_err(AppError::operation)?)
            .map_err(AppError::operation)?;
    if proposal.version != RESOLUTION_PROPOSAL_VERSION
        || proposal.repository_key != repository_key
        || proposal.conflict_id != conflict_id
        || proposal.proposal_id != proposal_id
    {
        return Err(AppError::operation(
            "resolution proposal identity or version mismatch",
        ));
    }
    Ok(proposal)
}

fn assemble_proposal(
    record: &SyncConflictRecord,
    repository_key: String,
    identity: ResolutionAgentIdentity,
    options: &ResolutionProposalOptions,
    prepared: PreparedOutput,
    proposal_tree: &GitOid,
    patch: String,
) -> Result<ResolutionProposal, AppError> {
    let proposal_id = proposal_id(record, &identity, &prepared.paths, proposal_tree)?;
    Ok(ResolutionProposal {
        version: RESOLUTION_PROPOSAL_VERSION,
        proposal_id,
        status: ResolutionProposalStatus::Ready,
        conflict_id: record.id.clone(),
        repository_key,
        base_revision: record
            .base_revision
            .clone()
            .expect("proposal creation validated the merge base"),
        local_revision: record.local_revision.clone(),
        remote_revision: record.remote_revision.clone(),
        policy_version: record.policy_version,
        policy_hash: record.policy_hash.clone(),
        provider: identity.provider,
        model: identity.model,
        prompt_contract_version: identity.prompt_contract_version,
        tool_contract_version: RESOLUTION_AGENT_TOOL_CONTRACT_VERSION,
        permission_profile: options.permission_profile.clone(),
        broad_context_allowed: options.allow_broad_context,
        explanation: prepared.explanation,
        referenced_context: prepared.referenced_context,
        proposal_tree: proposal_tree.to_string(),
        patch,
        paths: prepared.paths,
        validation: vec![
            ResolutionProposalValidationCheck::ConflictInputsPreserved,
            ResolutionProposalValidationCheck::PermissionProfileNamed,
            ResolutionProposalValidationCheck::FocusedContextBounded,
            ResolutionProposalValidationCheck::OutputPathsExact,
            ResolutionProposalValidationCheck::OutputBytesBounded,
            ResolutionProposalValidationCheck::NoFileDeletion,
            ResolutionProposalValidationCheck::ExactTreeObjects,
            ResolutionProposalValidationCheck::WorktreeUnchanged,
            ResolutionProposalValidationCheck::RefsUnchanged,
        ],
    })
}

struct PreparedOutput {
    explanation: String,
    referenced_context: Vec<String>,
    git_paths: Vec<GitResolvedPath>,
    paths: Vec<ResolutionProposalPath>,
}

fn build_agent_request(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
    options: &ResolutionProposalOptions,
) -> Result<ResolutionAgentRequest, AppError> {
    let base = record
        .base_revision
        .as_deref()
        .ok_or_else(|| AppError::operation("agent resolution requires one merge base"))?;
    let mut total = 0_usize;
    let mut files = Vec::with_capacity(record.paths.len());
    for path in &record.paths {
        let mut side = |revision: Option<&str>| -> Result<ResolutionAgentSide, AppError> {
            let Some(revision) = revision else {
                return Ok(ResolutionAgentSide {
                    revision: None,
                    mode: None,
                    content: None,
                });
            };
            let revision_oid = GitOid::parse(revision).map_err(AppError::operation)?;
            let object = engine
                .path_object(repository, &revision_oid, &path.path)
                .map_err(AppError::operation)?;
            let content = object.as_ref().and_then(|object| object.data.clone());
            if content
                .as_ref()
                .is_some_and(|data| data.len() > MAX_AGENT_FILE_BYTES)
            {
                return Err(AppError::operation(format!(
                    "conflict input `{}` exceeds the per-file agent limit",
                    path.path
                )));
            }
            total = total.saturating_add(content.as_ref().map_or(0, Vec::len));
            Ok(ResolutionAgentSide {
                revision: Some(revision.to_string()),
                mode: object.as_ref().map(|object| object.mode.clone()),
                content,
            })
        };
        files.push(ResolutionAgentFile {
            path: path.path.clone(),
            base: side(Some(base))?,
            local: side(Some(&record.local_revision))?,
            remote: side(Some(&record.remote_revision))?,
        });
    }
    if total > MAX_AGENT_TOTAL_BYTES {
        return Err(AppError::operation(
            "conflict inputs exceed the total agent byte limit",
        ));
    }
    Ok(ResolutionAgentRequest {
        conflict_id: record.id.clone(),
        policy_version: record.policy_version,
        policy_hash: record.policy_hash.clone(),
        files,
        focused_context: options.focused_context.clone(),
        broad_context_allowed: options.allow_broad_context,
        tool_contract_version: RESOLUTION_AGENT_TOOL_CONTRACT_VERSION,
    })
}

fn prepare_output(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
    output: ResolutionAgentOutput,
) -> Result<PreparedOutput, AppError> {
    validate_text("proposal explanation", &output.explanation)?;
    if output.referenced_context.len() > MAX_CONTEXT_PATHS {
        return Err(AppError::operation(
            "proposal referenced too many context paths",
        ));
    }
    if output
        .referenced_context
        .iter()
        .any(|path| !valid_relative_path(path))
    {
        return Err(AppError::operation(
            "proposal referenced an invalid context path",
        ));
    }
    let expected = record
        .paths
        .iter()
        .map(|path| path.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut supplied = BTreeMap::new();
    let mut total = 0_usize;
    for path in output.paths {
        if !expected.contains(path.path.as_str()) || supplied.contains_key(&path.path) {
            return Err(AppError::operation(format!(
                "agent output path `{}` is duplicate or outside the conflict",
                path.path
            )));
        }
        if path.content.len() > MAX_AGENT_FILE_BYTES {
            return Err(AppError::operation(format!(
                "agent output `{}` exceeds the per-file limit",
                path.path
            )));
        }
        total = total.saturating_add(path.content.len());
        supplied.insert(path.path, path.content);
    }
    if supplied.len() != expected.len() || total > MAX_AGENT_TOTAL_BYTES {
        return Err(AppError::operation(
            "agent output must resolve every conflict path within the total byte limit",
        ));
    }
    let mut git_paths = Vec::with_capacity(record.paths.len());
    let mut paths = Vec::with_capacity(record.paths.len());
    for conflict_path in &record.paths {
        let content = supplied
            .remove(&conflict_path.path)
            .expect("validated exact path set");
        let mode = resolved_mode(conflict_path)?;
        let resolved = GitResolvedPath {
            path: conflict_path.path.clone(),
            mode: Some(mode.clone()),
            data: Some(content.clone()),
        };
        // Exercise the engine's path and blob validation before tree construction.
        engine
            .path_object(
                repository,
                &GitOid::parse(&record.local_revision).map_err(AppError::operation)?,
                &conflict_path.path,
            )
            .map_err(AppError::operation)?;
        paths.push(ResolutionProposalPath {
            path: conflict_path.path.clone(),
            mode,
            content_hash: blake3::hash(&content).to_hex().to_string(),
            bytes: content.len() as u64,
        });
        git_paths.push(resolved);
    }
    Ok(PreparedOutput {
        explanation: output.explanation,
        referenced_context: output.referenced_context,
        git_paths,
        paths,
    })
}

fn resolved_mode(path: &crate::sync_conflicts::SyncConflictPathRecord) -> Result<String, AppError> {
    let base = path.base.mode.as_deref();
    let local = path.local.mode.as_deref();
    let remote = path.remote.mode.as_deref();
    if local == remote {
        local
    } else if local == base {
        remote
    } else if remote == base {
        local
    } else {
        None
    }
    .filter(|mode| *mode == "100644" || *mode == "100755" || *mode == "120000")
    .map(str::to_string)
    .ok_or_else(|| {
        AppError::operation(format!(
            "conflict path `{}` has an ambiguous mode",
            path.path
        ))
    })
}

fn verify_tree_objects(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    tree: &GitOid,
    paths: &[GitResolvedPath],
) -> Result<(), AppError> {
    for path in paths {
        let actual = engine
            .path_object(repository, tree, &path.path)
            .map_err(AppError::operation)?
            .ok_or_else(|| AppError::operation(format!("proposal tree omitted `{}`", path.path)))?;
        if actual.kind != "blob"
            || actual.mode != path.mode.as_deref().unwrap_or_default()
            || actual.data.as_ref() != path.data.as_ref()
        {
            return Err(AppError::operation(format!(
                "proposal tree object for `{}` differs from provider output",
                path.path
            )));
        }
    }
    Ok(())
}

fn preserved_ref_snapshot(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
) -> Result<Vec<(String, Option<String>)>, AppError> {
    [
        record.preserved_base_ref.as_deref(),
        Some(record.preserved_local_ref.as_str()),
        Some(record.preserved_remote_ref.as_str()),
        record.preserved_record_ref.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(|reference| {
        let parsed = vulcan_sync::GitRefName::parse(reference).map_err(AppError::operation)?;
        Ok((
            reference.to_string(),
            engine
                .read_ref(repository, &parsed)
                .map_err(AppError::operation)?
                .map(|oid| oid.to_string()),
        ))
    })
    .collect()
}

fn verify_no_external_mutation(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
    expected_tree: &GitOid,
    refs_before: &[(String, Option<String>)],
) -> Result<(), AppError> {
    let local_revision = GitOid::parse(&record.local_revision).map_err(AppError::operation)?;
    let current = engine
        .snapshot_worktree_tree(repository, Some(&local_revision))
        .map_err(AppError::operation)?;
    if &current != expected_tree {
        return Err(AppError::operation(
            "worktree changed while the resolution proposal was generated",
        ));
    }
    if preserved_ref_snapshot(engine, repository, record)? != refs_before {
        return Err(AppError::operation(
            "preserved conflict refs changed while the proposal was generated",
        ));
    }
    Ok(())
}

fn proposal_id(
    record: &SyncConflictRecord,
    identity: &ResolutionAgentIdentity,
    paths: &[ResolutionProposalPath],
    tree: &GitOid,
) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(&(record.id.as_str(), identity, paths, tree.as_str()))
        .map_err(AppError::operation)?;
    Ok(blake3::hash(&bytes).to_hex()[..32].to_string())
}

fn save_proposal(store: &SyncStateStore, proposal: &ResolutionProposal) -> Result<(), AppError> {
    let directory = store
        .root()
        .join(&proposal.repository_key)
        .join("conflicts")
        .join(&proposal.conflict_id)
        .join("proposals");
    fs::create_dir_all(&directory).map_err(AppError::operation)?;
    let path = directory.join(format!("{}.json", proposal.proposal_id));
    let bytes = serde_json::to_vec_pretty(proposal).map_err(AppError::operation)?;
    if bytes.len() > MAX_PROPOSAL_RECORD_BYTES {
        return Err(AppError::operation(
            "resolution proposal record exceeds its byte limit",
        ));
    }
    let mut temporary = NamedTempFile::new_in(&directory).map_err(AppError::operation)?;
    temporary.write_all(&bytes).map_err(AppError::operation)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AppError::operation)?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read(&path).map_err(AppError::operation)?;
            if existing == bytes {
                Ok(())
            } else {
                Err(AppError::operation("resolution proposal ID collision"))
            }
        }
        Err(error) => Err(AppError::operation(error.error)),
    }
}

fn ensure_no_existing_proposal(
    store: &SyncStateStore,
    repository_key: &str,
    conflict_id: &str,
) -> Result<(), AppError> {
    let directory = store
        .root()
        .join(repository_key)
        .join("conflicts")
        .join(conflict_id)
        .join("proposals");
    match fs::read_dir(directory) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                Err(AppError::operation(format!(
                    "conflict `{conflict_id}` already has a retained resolution proposal"
                )))
            } else {
                Ok(())
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::operation(error)),
    }
}

fn proposal_path(
    store: &SyncStateStore,
    repository_key: &str,
    conflict_id: &str,
    proposal_id: &str,
) -> PathBuf {
    store
        .root()
        .join(repository_key)
        .join("conflicts")
        .join(conflict_id)
        .join("proposals")
        .join(format!("{proposal_id}.json"))
}

fn validate_hex_id(label: &str, value: &str) -> Result<(), AppError> {
    if value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(AppError::operation(format!("invalid {label} `{value}`")))
    }
}

fn validate_options(options: &ResolutionProposalOptions) -> Result<(), AppError> {
    validate_text("permission profile", &options.permission_profile)?;
    if options.focused_context.len() > MAX_CONTEXT_PATHS
        || options
            .focused_context
            .iter()
            .any(|path| !valid_relative_path(path))
    {
        return Err(AppError::operation(
            "focused context paths are invalid or unbounded",
        ));
    }
    Ok(())
}

fn validate_agent_conflict_scope(record: &SyncConflictRecord) -> Result<(), AppError> {
    for path in &record.paths {
        let internal = path.path == ".obsidian"
            || path.path.starts_with(".obsidian/")
            || path.path == ".vulcan"
            || path.path.starts_with(".vulcan/");
        let unsupported = path.classification.as_ref().is_some_and(|classification| {
            matches!(
                classification.file_kind,
                vulcan_sync::MergeFileKind::Binary
                    | vulcan_sync::MergeFileKind::ObsidianState
                    | vulcan_sync::MergeFileKind::Missing
            )
        });
        if internal || unsupported {
            return Err(AppError::operation(format!(
                "conflict path `{}` is not eligible for agent input",
                path.path
            )));
        }
    }
    Ok(())
}

fn valid_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.bytes().any(|byte| byte == 0)
        && Path::new(value).components().all(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
}

fn validate_identity(identity: &ResolutionAgentIdentity) -> Result<(), AppError> {
    validate_text("provider", &identity.provider)?;
    validate_text("model", &identity.model)?;
    if identity.prompt_contract_version == 0 {
        return Err(AppError::operation(
            "prompt contract version must be positive",
        ));
    }
    Ok(())
}

fn validate_text(label: &str, value: &str) -> Result<(), AppError> {
    if value.is_empty() || value.len() > MAX_TEXT_BYTES || value.bytes().any(|byte| byte == 0) {
        Err(AppError::operation(format!(
            "{label} is empty or unbounded"
        )))
    } else {
        Ok(())
    }
}

fn cancellation_check(cancellation: &SyncCancellationToken) -> Result<(), AppError> {
    if cancellation.is_cancelled() {
        Err(AppError::operation(
            "resolution proposal generation was cancelled",
        ))
    } else {
        Ok(())
    }
}

struct ProposalLock {
    _file: File,
}

impl ProposalLock {
    fn acquire(repository: &vulcan_sync::GitRepository) -> Result<Self, AppError> {
        let path = repository.git_dir.join("vulcan-sync/sync.lock");
        fs::create_dir_all(
            path.parent()
                .expect("the proposal lock path always has a parent"),
        )
        .map_err(AppError::operation)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(AppError::operation)?;
        file.try_lock_exclusive()
            .map_err(|_| AppError::operation("another repository mutation is in progress"))?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::sync_git_vault_with_state_store;
    use std::process::Command;
    use tempfile::{tempdir, TempDir};
    use vulcan_sync::{GitCliEngine, GitSyncOptions};

    struct FakeProvider {
        cancel: bool,
    }

    impl ResolutionAgentProvider for FakeProvider {
        fn identity(&self) -> ResolutionAgentIdentity {
            ResolutionAgentIdentity {
                provider: "fake".to_string(),
                model: "fixture-v1".to_string(),
                prompt_contract_version: 1,
            }
        }

        fn propose(
            &self,
            request: &ResolutionAgentRequest,
            cancellation: &SyncCancellationToken,
        ) -> Result<ResolutionAgentOutput, AppError> {
            assert_eq!(request.files.len(), 1);
            assert_eq!(request.files[0].path, "Home.md");
            assert_eq!(
                request.files[0].base.content.as_deref(),
                Some(b"base\n".as_slice())
            );
            if self.cancel {
                cancellation.cancel();
            }
            Ok(ResolutionAgentOutput {
                explanation: "Combine the two intended edits.".to_string(),
                referenced_context: request.focused_context.clone(),
                paths: vec![ResolutionAgentPathOutput {
                    path: "Home.md".to_string(),
                    content: b"agent resolution\n".to_vec(),
                }],
            })
        }
    }

    struct ConflictFixture {
        _temporary: TempDir,
        store: SyncStateStore,
        reader: PathBuf,
        record: SyncConflictRecord,
    }

    fn conflict_fixture() -> ConflictFixture {
        let temporary = tempdir().expect("temporary directory");
        let remote = temporary.path().join("remote.git");
        git(
            temporary.path(),
            &["init", "--quiet", "--bare", path(&remote)],
        );
        let writer = temporary.path().join("writer");
        fs::create_dir(&writer).expect("writer directory");
        git(
            &writer,
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        configure_git(&writer);
        git(&writer, &["remote", "add", "origin", path(&remote)]);
        fs::write(writer.join("Home.md"), "base\n").expect("base note");
        commit_all(&writer, "base");
        let store = SyncStateStore::at(temporary.path().join("state"));
        sync_git_vault_with_state_store(
            &VaultPaths::new(&writer),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("bootstrap sync");
        let reader = temporary.path().join("reader");
        git(
            temporary.path(),
            &["clone", "--quiet", path(&writer), path(&reader)],
        );
        git(&reader, &["remote", "set-url", "origin", path(&remote)]);
        sync_git_vault_with_state_store(
            &VaultPaths::new(&reader),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("reader baseline");
        fs::write(writer.join("Home.md"), "writer\n").expect("writer edit");
        fs::write(reader.join("Home.md"), "reader\n").expect("reader edit");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&writer),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("writer sync");
        let report = sync_git_vault_with_state_store(
            &VaultPaths::new(&reader),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("conflicted sync");
        ConflictFixture {
            _temporary: temporary,
            store,
            reader,
            record: report.conflict_record.expect("conflict record"),
        }
    }

    #[test]
    fn provider_proposal_is_bounded_persisted_and_does_not_mutate_refs_or_worktree() {
        let fixture = conflict_fixture();
        let refs_before = git_stdout(
            &fixture.reader,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/vulcan",
            ],
        );
        let cancellation = SyncCancellationToken::default();
        let proposal = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: vec!["Home.md".to_string()],
                allow_broad_context: false,
            },
            &FakeProvider { cancel: false },
            &cancellation,
            &fixture.store,
        )
        .expect("proposal");
        assert_eq!(proposal.status, ResolutionProposalStatus::Ready);
        assert_eq!(proposal.provider, "fake");
        assert_eq!(
            proposal.paths[0].content_hash,
            blake3::hash(b"agent resolution\n").to_hex().to_string()
        );
        assert!(proposal.patch.contains("agent resolution"));
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Home.md")).expect("note"),
            "reader\n"
        );
        assert_eq!(
            git_stdout(
                &fixture.reader,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/vulcan"
                ],
            ),
            refs_before
        );
        let proposal_path = fixture
            .store
            .root()
            .join(&fixture.record.repository_key)
            .join("conflicts")
            .join(&fixture.record.id)
            .join("proposals")
            .join(format!("{}.json", proposal.proposal_id));
        assert!(proposal_path.is_file());
        assert_eq!(
            load_resolution_proposal(
                &fixture.store,
                &fixture.record.repository_key,
                &fixture.record.id,
                &proposal.proposal_id,
            )
            .expect("stored proposal"),
            proposal
        );
        let duplicate = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
            },
            &FakeProvider { cancel: false },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect_err("only one retained proposal job is allowed");
        assert!(duplicate.to_string().contains("already has"));
        let repository = GitCliEngine::default()
            .discover_repository(&fixture.reader)
            .expect("repository");
        let object = GitCliEngine::default()
            .path_object(
                &repository,
                &GitOid::parse(&proposal.proposal_tree).expect("proposal tree"),
                "Home.md",
            )
            .expect("tree lookup")
            .expect("proposal object");
        assert_eq!(
            object.data.as_deref(),
            Some(b"agent resolution\n".as_slice())
        );
    }

    #[test]
    fn cancellation_after_provider_output_preserves_originals_without_a_proposal_record() {
        let fixture = conflict_fixture();
        let cancellation = SyncCancellationToken::default();
        let error = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
            },
            &FakeProvider { cancel: true },
            &cancellation,
            &fixture.store,
        )
        .expect_err("cancelled proposal");
        assert!(error.to_string().contains("cancelled"));
        assert!(fixture.record.preserved_record_ref.is_some());
        assert!(!fixture
            .store
            .root()
            .join(&fixture.record.repository_key)
            .join("conflicts")
            .join(&fixture.record.id)
            .join("proposals")
            .exists());
    }

    fn path(path: &Path) -> &str {
        path.to_str().expect("UTF-8 test path")
    }

    fn configure_git(repository: &Path) {
        git(repository, &["config", "user.name", "Vulcan Test"]);
        git(
            repository,
            &["config", "user.email", "vulcan@example.invalid"],
        );
    }

    fn commit_all(repository: &Path, message: &str) {
        git(repository, &["add", "-A"]);
        git(repository, &["commit", "--quiet", "-m", message]);
    }

    fn git(repository: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .current_dir(repository)
            .args(arguments)
            .status()
            .expect("Git should launch");
        assert!(status.success(), "Git failed: {arguments:?}");
    }

    fn git_stdout(repository: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(repository)
            .args(arguments)
            .output()
            .expect("Git should launch");
        assert!(output.status.success(), "Git failed: {arguments:?}");
        String::from_utf8(output.stdout)
            .expect("UTF-8 Git output")
            .trim()
            .to_string()
    }
}
