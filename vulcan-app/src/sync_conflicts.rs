//! Durable device-local conflict records and preserved file artifacts.

use crate::scan::refresh_cache_incrementally;
use crate::sync_state::SyncStateStore;
use crate::AppError;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use vulcan_core::{ScanSummary, VaultPaths};
use vulcan_sync::{
    GitCaptureRequest, GitConflictClassification, GitConflictSide, GitEngine,
    GitMergeResolutionRequest, GitOid, GitPushResult, GitRefName, GitRemote, GitRepository,
    GitSyncConflict, GitSyncOptions, GitSyncRefs,
};

pub const SYNC_CONFLICT_RECORD_VERSION: u32 = 1;
pub const SYNC_CONFLICT_RESOLUTION_VERSION: u32 = 1;
const MAX_CONFLICT_RECORD_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflictRecord {
    pub version: u32,
    pub id: String,
    pub repository_key: String,
    pub work_tree: PathBuf,
    pub base_revision: Option<String>,
    pub local_revision: String,
    pub remote_revision: String,
    pub policy_version: u32,
    pub policy_hash: String,
    pub preserved_base_ref: Option<String>,
    pub preserved_local_ref: String,
    pub preserved_remote_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserved_record_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance_revision: Option<String>,
    pub paths: Vec<SyncConflictPathRecord>,
    pub diagnostics: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflictPathRecord {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<GitConflictClassification>,
    pub base: SyncConflictSideRecord,
    pub local: SyncConflictSideRecord,
    pub remote: SyncConflictSideRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflictSideRecord {
    pub revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflictSummary {
    pub id: String,
    pub paths: Vec<String>,
    pub base_revision: Option<String>,
    pub local_revision: String,
    pub remote_revision: String,
    pub policy_version: u32,
    pub resolution: SyncConflictResolutionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncConflictResolutionState {
    Unresolved,
    Resolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncConflictResolutionSide {
    Base,
    Local,
    Remote,
}

impl From<SyncConflictResolutionSide> for GitConflictSide {
    fn from(side: SyncConflictResolutionSide) -> Self {
        match side {
            SyncConflictResolutionSide::Base => Self::Base,
            SyncConflictResolutionSide::Local => Self::Local,
            SyncConflictResolutionSide::Remote => Self::Remote,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveSyncConflictOptions {
    pub side: SyncConflictResolutionSide,
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflictResolutionRecord {
    pub version: u32,
    pub conflict_id: String,
    pub side: SyncConflictResolutionSide,
    pub base_revision: String,
    pub local_revision: String,
    pub remote_revision: String,
    pub recovery_revision: String,
    pub resolved_tree: String,
    pub resolution_commit: String,
    pub published: bool,
    pub applied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolveSyncConflictOutcome {
    Planned,
    Resolved,
    AlreadyResolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolveSyncConflictReport {
    pub vault: PathBuf,
    pub repository_key: String,
    pub conflict_id: String,
    pub side: SyncConflictResolutionSide,
    pub dry_run: bool,
    pub outcome: ResolveSyncConflictOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_refresh: Option<ScanSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflictListReport {
    pub vault: PathBuf,
    pub repository_key: String,
    pub count: usize,
    pub conflicts: Vec<SyncConflictSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflictDetailReport {
    pub record: SyncConflictRecord,
    pub resolution: SyncConflictResolutionState,
}

pub fn list_sync_conflicts(
    paths: &vulcan_core::VaultPaths,
) -> Result<SyncConflictListReport, AppError> {
    let state_store = SyncStateStore::user_default()?;
    list_sync_conflicts_with_state_store(paths, &state_store)
}

pub fn list_sync_conflicts_with_state_store(
    paths: &vulcan_core::VaultPaths,
    state_store: &SyncStateStore,
) -> Result<SyncConflictListReport, AppError> {
    let work_tree = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let repository_key = crate::sync_state::repository_state_key(&work_tree);
    let records = SyncConflictStore::from_state_store(state_store).list(&repository_key)?;
    let store = SyncConflictStore::from_state_store(state_store);
    let conflicts = records
        .into_iter()
        .map(|record| {
            let resolution = store.resolution_state(&repository_key, &record.id)?;
            Ok(
                (resolution == SyncConflictResolutionState::Unresolved).then_some(
                    SyncConflictSummary {
                        id: record.id,
                        paths: record.paths.into_iter().map(|path| path.path).collect(),
                        base_revision: record.base_revision,
                        local_revision: record.local_revision,
                        remote_revision: record.remote_revision,
                        policy_version: record.policy_version,
                        resolution,
                    },
                ),
            )
        })
        .collect::<Result<Vec<_>, AppError>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    Ok(SyncConflictListReport {
        vault: work_tree,
        repository_key,
        count: conflicts.len(),
        conflicts,
    })
}

pub fn get_sync_conflict(
    paths: &vulcan_core::VaultPaths,
    conflict_id: &str,
) -> Result<SyncConflictDetailReport, AppError> {
    let state_store = SyncStateStore::user_default()?;
    get_sync_conflict_with_state_store(paths, conflict_id, &state_store)
}

pub fn get_sync_conflict_with_state_store(
    paths: &vulcan_core::VaultPaths,
    conflict_id: &str,
    state_store: &SyncStateStore,
) -> Result<SyncConflictDetailReport, AppError> {
    let work_tree = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let repository_key = crate::sync_state::repository_state_key(&work_tree);
    let store = SyncConflictStore::from_state_store(state_store);
    let record = store.get(&repository_key, conflict_id)?;
    let resolution = store.resolution_state(&repository_key, conflict_id)?;
    Ok(SyncConflictDetailReport { record, resolution })
}

pub fn resolve_sync_conflict(
    paths: &VaultPaths,
    conflict_id: &str,
    options: &ResolveSyncConflictOptions,
) -> Result<ResolveSyncConflictReport, AppError> {
    let state_store = SyncStateStore::user_default()?;
    resolve_sync_conflict_with_state_store(paths, conflict_id, options, &state_store)
}

pub fn resolve_sync_conflict_with_state_store(
    paths: &VaultPaths,
    conflict_id: &str,
    options: &ResolveSyncConflictOptions,
    state_store: &SyncStateStore,
) -> Result<ResolveSyncConflictReport, AppError> {
    let work_tree = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let repository_key = crate::sync_state::repository_state_key(&work_tree);
    let context = ResolutionContext {
        vault: work_tree.clone(),
        repository_key: repository_key.clone(),
        conflict_id: conflict_id.to_string(),
    };
    let store = SyncConflictStore::from_state_store(state_store);
    let record = store.get(&repository_key, conflict_id)?;
    if record.work_tree != work_tree {
        return Err(AppError::operation(
            "sync conflict record does not belong to the selected worktree",
        ));
    }
    let existing_resolution = store.get_resolution(&repository_key, conflict_id)?;
    if let Some(existing) = &existing_resolution {
        if existing.side != options.side {
            return Err(AppError::operation(format!(
                "conflict `{conflict_id}` already has a {:?} resolution in progress",
                existing.side
            )));
        }
        if existing.applied {
            return Ok(context.report(
                options,
                ResolveSyncConflictOutcome::AlreadyResolved,
                Some(existing.recovery_revision.clone()),
                Some(existing.resolution_commit.clone()),
                None,
            ));
        }
    }

    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&work_tree)
        .map_err(AppError::operation)?;
    verify_preserved_conflict_refs(&engine, &repository, &record)?;
    let safety = engine
        .safety_state(&repository)
        .map_err(AppError::operation)?;
    if options.dry_run {
        verify_resolution_preconditions(
            &engine,
            &repository,
            &record,
            options,
            &safety,
            existing_resolution.as_ref(),
        )?;
        return Ok(context.report(
            options,
            ResolveSyncConflictOutcome::Planned,
            None,
            None,
            None,
        ));
    }

    resolve_sync_conflict_locked(
        paths,
        options,
        state_store,
        &store,
        &record,
        &repository,
        &context,
    )
}

fn resolve_sync_conflict_locked(
    paths: &VaultPaths,
    options: &ResolveSyncConflictOptions,
    state_store: &SyncStateStore,
    store: &SyncConflictStore,
    record: &SyncConflictRecord,
    repository: &GitRepository,
    context: &ResolutionContext,
) -> Result<ResolveSyncConflictReport, AppError> {
    let _lock = ConflictResolutionLock::acquire(repository)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let device_id = state_store
        .load_or_create_device_id(true)?
        .expect("mutating device identity creation returns an identity");
    verify_preserved_conflict_refs(&engine, repository, record)?;
    let local = GitOid::parse(&record.local_revision).map_err(AppError::operation)?;
    let recovery_ref = GitRefName::parse(format!(
        "refs/vulcan/conflicts/{}/recovery/current",
        context.conflict_id
    ))
    .map_err(AppError::operation)?;
    let capture = engine
        .capture_worktree(
            repository,
            &GitCaptureRequest {
                base: Some(local.clone()),
                target_ref: recovery_ref,
                message: format!(
                    "vulcan conflict recovery snapshot\n\nVulcan-Conflict: {}\nVulcan-Sync-Version: 1\nVulcan-Sync-Device: {}\nVulcan-Sync-Source: {}\nVulcan-Sync-Semantic: false\n",
                    context.conflict_id,
                    device_id.as_str(),
                    local
                ),
            },
        )
        .map_err(AppError::operation)?;
    let immutable_recovery_ref = GitRefName::parse(format!(
        "refs/vulcan/conflicts/{}/recovery/{}",
        context.conflict_id, capture.commit
    ))
    .map_err(AppError::operation)?;
    engine
        .update_ref(repository, &immutable_recovery_ref, &capture.commit)
        .map_err(AppError::operation)?;
    let safety = engine
        .safety_state(repository)
        .map_err(AppError::operation)?;
    reject_unsafe_resolution(&safety)?;

    let existing = store.get_resolution(&context.repository_key, &context.conflict_id)?;
    verify_remote_for_resolution(&engine, repository, record, options, existing.as_ref())?;
    let resolution = if let Some(existing) = existing {
        resume_resolution(&engine, repository, record, &capture, options, existing)?
    } else {
        prepare_resolution(&engine, repository, record, &capture, options, &device_id)?
    };
    store.save_resolution(&context.repository_key, &resolution)?;
    publish_and_apply_resolution(
        paths, options, store, repository, context, &capture, resolution,
    )
}

fn publish_and_apply_resolution(
    paths: &VaultPaths,
    options: &ResolveSyncConflictOptions,
    store: &SyncConflictStore,
    repository: &GitRepository,
    context: &ResolutionContext,
    capture: &vulcan_sync::GitCapture,
    mut resolution: SyncConflictResolutionRecord,
) -> Result<ResolveSyncConflictReport, AppError> {
    let engine = vulcan_sync::GitCliEngine::default();
    let resolution_commit =
        GitOid::parse(&resolution.resolution_commit).map_err(AppError::operation)?;
    let remote_before = GitOid::parse(&resolution.remote_revision).map_err(AppError::operation)?;
    let current_remote = engine
        .remote_ref(repository, &options.remote, &options.live_ref)
        .map_err(AppError::operation)?;
    match current_remote.as_ref() {
        Some(current) if current == &resolution_commit => {}
        Some(current) if current == &remote_before => {
            if engine
                .push_ref(
                    repository,
                    &options.remote,
                    &resolution_commit,
                    &options.live_ref,
                    Some(&remote_before),
                )
                .map_err(AppError::operation)?
                == GitPushResult::Rejected
            {
                return Err(AppError::operation(
                    "the remote live ref changed while publishing the resolution; preserved state remains available",
                ));
            }
        }
        _ => {
            return Err(AppError::operation(
                "the remote live ref no longer matches the preserved conflict input or prepared resolution",
            ));
        }
    }
    resolution.published = true;
    store.save_resolution(&context.repository_key, &resolution)?;

    let resolved_tree = GitOid::parse(&resolution.resolved_tree).map_err(AppError::operation)?;
    if capture.tree != resolved_tree {
        let _application = engine
            .apply_tree(repository, &capture.commit, &resolution_commit)
            .map_err(AppError::operation)?;
    }
    let refs = GitSyncRefs::for_options(&GitSyncOptions {
        remote: options.remote.clone(),
        live_ref: options.live_ref.clone(),
        ..GitSyncOptions::default()
    })
    .map_err(AppError::operation)?;
    for reference in [&refs.local, &refs.fetched, &refs.pending] {
        engine
            .update_ref(repository, reference, &resolution_commit)
            .map_err(AppError::operation)?;
    }
    let cache_refresh = if paths.cache_db().is_file() {
        Some(refresh_cache_incrementally(paths)?)
    } else {
        None
    };
    resolution.applied = true;
    store.save_resolution(&context.repository_key, &resolution)?;
    Ok(context.report(
        options,
        ResolveSyncConflictOutcome::Resolved,
        Some(resolution.recovery_revision),
        Some(resolution.resolution_commit),
        cache_refresh,
    ))
}

struct ResolutionContext {
    vault: PathBuf,
    repository_key: String,
    conflict_id: String,
}

impl ResolutionContext {
    fn report(
        &self,
        options: &ResolveSyncConflictOptions,
        outcome: ResolveSyncConflictOutcome,
        recovery_revision: Option<String>,
        resolution_commit: Option<String>,
        cache_refresh: Option<ScanSummary>,
    ) -> ResolveSyncConflictReport {
        ResolveSyncConflictReport {
            vault: self.vault.clone(),
            repository_key: self.repository_key.clone(),
            conflict_id: self.conflict_id.clone(),
            side: options.side,
            dry_run: options.dry_run,
            outcome,
            recovery_revision,
            resolution_commit,
            cache_refresh,
        }
    }
}

fn verify_preserved_conflict_refs(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    record: &SyncConflictRecord,
) -> Result<(), AppError> {
    verify_preserved_ref(
        engine,
        repository,
        record.preserved_base_ref.as_deref(),
        record.base_revision.as_deref(),
        "base",
    )?;
    verify_preserved_ref(
        engine,
        repository,
        Some(&record.preserved_local_ref),
        Some(&record.local_revision),
        "local",
    )?;
    verify_preserved_ref(
        engine,
        repository,
        Some(&record.preserved_remote_ref),
        Some(&record.remote_revision),
        "remote",
    )?;
    verify_preserved_ref(
        engine,
        repository,
        record.preserved_record_ref.as_deref(),
        record.provenance_revision.as_deref(),
        "provenance",
    )
}

fn verify_preserved_ref(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    reference: Option<&str>,
    expected: Option<&str>,
    side: &str,
) -> Result<(), AppError> {
    match (reference, expected) {
        (None, None) => Ok(()),
        (Some(reference), Some(expected)) => {
            let reference = GitRefName::parse(reference).map_err(AppError::operation)?;
            let actual = engine
                .read_ref(repository, &reference)
                .map_err(AppError::operation)?;
            if actual.as_ref().map(GitOid::as_str) == Some(expected) {
                Ok(())
            } else {
                Err(AppError::operation(format!(
                    "preserved {side} ref `{reference}` no longer matches conflict record"
                )))
            }
        }
        _ => Err(AppError::operation(format!(
            "preserved {side} ref metadata is incomplete"
        ))),
    }
}

fn verify_resolution_preconditions(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    record: &SyncConflictRecord,
    options: &ResolveSyncConflictOptions,
    safety: &vulcan_sync::GitSafetyState,
    existing: Option<&SyncConflictResolutionRecord>,
) -> Result<(), AppError> {
    reject_unsafe_resolution(safety)?;
    if record.base_revision.is_none() {
        return Err(AppError::operation(
            "this conflict has no unique merge base and cannot use side resolution",
        ));
    }
    verify_remote_for_resolution(engine, repository, record, options, existing)
}

fn verify_remote_for_resolution(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    record: &SyncConflictRecord,
    options: &ResolveSyncConflictOptions,
    existing: Option<&SyncConflictResolutionRecord>,
) -> Result<(), AppError> {
    let remote = engine
        .remote_ref(repository, &options.remote, &options.live_ref)
        .map_err(AppError::operation)?;
    let matches_input =
        remote.as_ref().map(GitOid::as_str) == Some(record.remote_revision.as_str());
    let matches_prepared = existing.is_some_and(|resolution| {
        remote.as_ref().map(GitOid::as_str) == Some(resolution.resolution_commit.as_str())
    });
    if matches_input || matches_prepared {
        Ok(())
    } else {
        Err(AppError::operation(
            "the remote live ref no longer matches the preserved conflict input or prepared resolution",
        ))
    }
}

fn reject_unsafe_resolution(safety: &vulcan_sync::GitSafetyState) -> Result<(), AppError> {
    if safety.staged_changes {
        return Err(AppError::operation(
            "cannot apply a conflict resolution while the normal Git index has staged changes; the current worktree was preserved",
        ));
    }
    if let Some(operation) = &safety.operation {
        return Err(AppError::operation(format!(
            "cannot apply a conflict resolution while Git {operation} is in progress; the current worktree was preserved"
        )));
    }
    Ok(())
}

fn prepare_resolution(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    record: &SyncConflictRecord,
    capture: &vulcan_sync::GitCapture,
    options: &ResolveSyncConflictOptions,
    device_id: &vulcan_sync::GitSyncDeviceId,
) -> Result<SyncConflictResolutionRecord, AppError> {
    let local = GitOid::parse(&record.local_revision).map_err(AppError::operation)?;
    if capture.tree
        != engine
            .tree_oid(repository, &local)
            .map_err(AppError::operation)?
    {
        return Err(AppError::operation(
            "the worktree changed after the conflict was preserved; its recovery snapshot was retained and the resolution was not applied",
        ));
    }
    let base = record
        .base_revision
        .as_deref()
        .ok_or_else(|| {
            AppError::operation(
                "this conflict has no unique merge base and cannot use side resolution",
            )
        })
        .and_then(|value| GitOid::parse(value).map_err(AppError::operation))?;
    let remote = GitOid::parse(&record.remote_revision).map_err(AppError::operation)?;
    let tree = engine
        .resolve_merge_tree(
            repository,
            &GitMergeResolutionRequest {
                base: base.clone(),
                accepted_remote: remote.clone(),
                local_candidate: local.clone(),
                paths: record.paths.iter().map(|path| path.path.clone()).collect(),
                side: options.side.into(),
            },
        )
        .map_err(AppError::operation)?;
    let commit = engine
        .create_commit(
            repository,
            &tree,
            &[remote.clone(), local.clone()],
            &format!(
                "vulcan conflict resolution\n\nVulcan-Conflict: {}\nVulcan-Resolution-Side: {}\nVulcan-Sync-Version: 1\nVulcan-Sync-Device: {}\nVulcan-Sync-Policy: {}:{}\nVulcan-Sync-Source: {}+{}\nVulcan-Sync-Semantic: false\n",
                record.id,
                resolution_side_name(options.side),
                device_id.as_str(),
                record.policy_version,
                record.policy_hash,
                remote,
                local
            ),
        )
        .map_err(AppError::operation)?;
    let resolved_ref = GitRefName::parse(format!("refs/vulcan/conflicts/{}/resolved", record.id))
        .map_err(AppError::operation)?;
    engine
        .update_ref(repository, &resolved_ref, &commit)
        .map_err(AppError::operation)?;
    Ok(SyncConflictResolutionRecord {
        version: SYNC_CONFLICT_RESOLUTION_VERSION,
        conflict_id: record.id.clone(),
        side: options.side,
        base_revision: base.to_string(),
        local_revision: local.to_string(),
        remote_revision: remote.to_string(),
        recovery_revision: capture.commit.to_string(),
        resolved_tree: tree.to_string(),
        resolution_commit: commit.to_string(),
        published: false,
        applied: false,
    })
}

const fn resolution_side_name(side: SyncConflictResolutionSide) -> &'static str {
    match side {
        SyncConflictResolutionSide::Base => "base",
        SyncConflictResolutionSide::Local => "local",
        SyncConflictResolutionSide::Remote => "remote",
    }
}

fn resume_resolution(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    record: &SyncConflictRecord,
    capture: &vulcan_sync::GitCapture,
    options: &ResolveSyncConflictOptions,
    mut resolution: SyncConflictResolutionRecord,
) -> Result<SyncConflictResolutionRecord, AppError> {
    if resolution.side != options.side
        || resolution.base_revision != record.base_revision.as_deref().unwrap_or_default()
        || resolution.local_revision != record.local_revision
        || resolution.remote_revision != record.remote_revision
    {
        return Err(AppError::operation(
            "prepared conflict resolution does not match the immutable conflict inputs",
        ));
    }
    let local = GitOid::parse(&record.local_revision).map_err(AppError::operation)?;
    let resolved = GitOid::parse(&resolution.resolution_commit).map_err(AppError::operation)?;
    let actual_tree = &capture.tree;
    let local_tree = engine
        .tree_oid(repository, &local)
        .map_err(AppError::operation)?;
    let resolved_tree = engine
        .tree_oid(repository, &resolved)
        .map_err(AppError::operation)?;
    if actual_tree != &local_tree && actual_tree != &resolved_tree {
        return Err(AppError::operation(
            "the worktree changed while a conflict resolution was pending; its recovery snapshot was retained",
        ));
    }
    resolution.recovery_revision = capture.commit.to_string();
    Ok(resolution)
}

struct ConflictResolutionLock {
    _file: File,
}

impl ConflictResolutionLock {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConflictStore {
    root: PathBuf,
}

impl SyncConflictStore {
    #[must_use]
    pub fn from_state_store(state_store: &SyncStateStore) -> Self {
        Self {
            root: state_store.root().to_path_buf(),
        }
    }

    #[must_use]
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn persist(
        &self,
        engine: &dyn GitEngine,
        repository: &GitRepository,
        repository_key: &str,
        conflict: &GitSyncConflict,
    ) -> Result<SyncConflictRecord, AppError> {
        validate_hex_id("repository key", repository_key)?;
        validate_hex_id("conflict ID", &conflict.id)?;
        let directory = self.conflict_directory(repository_key, &conflict.id)?;
        let record_path = directory.join("record.json");
        if record_path.exists() {
            let existing = self.get(repository_key, &conflict.id)?;
            verify_record_inputs(&existing, conflict)?;
            return Ok(existing);
        }
        let work_tree = repository.work_tree.clone().ok_or_else(|| {
            AppError::operation("cannot preserve a sync conflict for a bare repository")
        })?;
        fs::create_dir_all(directory.join("artifacts")).map_err(AppError::operation)?;
        let mut paths = Vec::with_capacity(conflict.paths.len());
        for (index, path) in conflict.paths.iter().enumerate() {
            paths.push(SyncConflictPathRecord {
                path: path.clone(),
                classification: conflict
                    .classifications
                    .iter()
                    .find(|classification| classification.path == *path)
                    .cloned(),
                base: preserve_side(
                    engine,
                    repository,
                    &directory,
                    index,
                    "base",
                    conflict.base.as_ref(),
                    path,
                )?,
                local: preserve_side(
                    engine,
                    repository,
                    &directory,
                    index,
                    "local",
                    Some(&conflict.local),
                    path,
                )?,
                remote: preserve_side(
                    engine,
                    repository,
                    &directory,
                    index,
                    "remote",
                    Some(&conflict.remote),
                    path,
                )?,
            });
        }
        let record = SyncConflictRecord {
            version: SYNC_CONFLICT_RECORD_VERSION,
            id: conflict.id.clone(),
            repository_key: repository_key.to_string(),
            work_tree,
            base_revision: conflict.base.as_ref().map(ToString::to_string),
            local_revision: conflict.local.to_string(),
            remote_revision: conflict.remote.to_string(),
            policy_version: conflict.policy_version,
            policy_hash: conflict.policy_hash.clone(),
            preserved_base_ref: conflict
                .preserved_refs
                .base
                .as_ref()
                .map(ToString::to_string),
            preserved_local_ref: conflict.preserved_refs.local.to_string(),
            preserved_remote_ref: conflict.preserved_refs.remote.to_string(),
            preserved_record_ref: Some(conflict.preserved_refs.record.to_string()),
            provenance_revision: Some(conflict.provenance_revision.to_string()),
            paths,
            diagnostics: conflict.diagnostics.clone(),
        };
        write_json_noclobber(&record_path, &record)?;
        Ok(record)
    }

    pub fn list(&self, repository_key: &str) -> Result<Vec<SyncConflictRecord>, AppError> {
        validate_hex_id("repository key", repository_key)?;
        let root = self.root.join(repository_key).join("conflicts");
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(AppError::operation(error)),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(AppError::operation)?;
            if !entry.file_type().map_err(AppError::operation)?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            validate_hex_id("conflict ID", &id)?;
            records.push(self.get(repository_key, &id)?);
        }
        records.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(records)
    }

    pub fn get(
        &self,
        repository_key: &str,
        conflict_id: &str,
    ) -> Result<SyncConflictRecord, AppError> {
        let path = self
            .conflict_directory(repository_key, conflict_id)?
            .join("record.json");
        let metadata = fs::metadata(&path).map_err(AppError::operation)?;
        if metadata.len() > MAX_CONFLICT_RECORD_BYTES {
            return Err(AppError::operation(format!(
                "sync conflict record at {} exceeds the {} byte limit",
                path.display(),
                MAX_CONFLICT_RECORD_BYTES
            )));
        }
        let record: SyncConflictRecord =
            serde_json::from_slice(&fs::read(&path).map_err(AppError::operation)?)
                .map_err(AppError::operation)?;
        validate_record(&record, repository_key, conflict_id)?;
        Ok(record)
    }

    pub fn get_resolution(
        &self,
        repository_key: &str,
        conflict_id: &str,
    ) -> Result<Option<SyncConflictResolutionRecord>, AppError> {
        let path = self
            .conflict_directory(repository_key, conflict_id)?
            .join("resolution.json");
        let source = match fs::read(&path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AppError::operation(error)),
        };
        if source.len() as u64 > MAX_CONFLICT_RECORD_BYTES {
            return Err(AppError::operation(format!(
                "sync conflict resolution at {} exceeds the {} byte limit",
                path.display(),
                MAX_CONFLICT_RECORD_BYTES
            )));
        }
        let resolution: SyncConflictResolutionRecord =
            serde_json::from_slice(&source).map_err(AppError::operation)?;
        if resolution.version != SYNC_CONFLICT_RESOLUTION_VERSION
            || resolution.conflict_id != conflict_id
        {
            return Err(AppError::operation(
                "sync conflict resolution version or identity mismatch",
            ));
        }
        Ok(Some(resolution))
    }

    pub fn save_resolution(
        &self,
        repository_key: &str,
        resolution: &SyncConflictResolutionRecord,
    ) -> Result<(), AppError> {
        if resolution.version != SYNC_CONFLICT_RESOLUTION_VERSION {
            return Err(AppError::operation(
                "cannot save an unsupported sync conflict resolution version",
            ));
        }
        let path = self
            .conflict_directory(repository_key, &resolution.conflict_id)?
            .join("resolution.json");
        write_json_replace(&path, resolution)
    }

    fn resolution_state(
        &self,
        repository_key: &str,
        conflict_id: &str,
    ) -> Result<SyncConflictResolutionState, AppError> {
        Ok(match self.get_resolution(repository_key, conflict_id)? {
            Some(resolution) if resolution.applied => SyncConflictResolutionState::Resolved,
            _ => SyncConflictResolutionState::Unresolved,
        })
    }

    fn conflict_directory(
        &self,
        repository_key: &str,
        conflict_id: &str,
    ) -> Result<PathBuf, AppError> {
        validate_hex_id("repository key", repository_key)?;
        validate_hex_id("conflict ID", conflict_id)?;
        Ok(self
            .root
            .join(repository_key)
            .join("conflicts")
            .join(conflict_id))
    }
}

fn preserve_side(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    conflict_directory: &Path,
    index: usize,
    side: &str,
    revision: Option<&GitOid>,
    path: &str,
) -> Result<SyncConflictSideRecord, AppError> {
    let Some(revision) = revision else {
        return Ok(SyncConflictSideRecord {
            revision: "absent".to_string(),
            object_id: None,
            mode: None,
            kind: None,
            artifact: None,
            content_hash: None,
            bytes: None,
        });
    };
    let object = engine
        .path_object(repository, revision, path)
        .map_err(AppError::operation)?;
    let Some(object) = object else {
        return Ok(SyncConflictSideRecord {
            revision: revision.to_string(),
            object_id: None,
            mode: None,
            kind: None,
            artifact: None,
            content_hash: None,
            bytes: None,
        });
    };
    let (artifact, content_hash, bytes) = if let Some(data) = object.data {
        let relative = PathBuf::from(format!("artifacts/{index:04}-{side}.bin"));
        let path = conflict_directory.join(&relative);
        write_bytes_noclobber(&path, &data)?;
        (
            Some(relative),
            Some(blake3::hash(&data).to_hex().to_string()),
            Some(data.len() as u64),
        )
    } else {
        (None, None, None)
    };
    Ok(SyncConflictSideRecord {
        revision: revision.to_string(),
        object_id: Some(object.oid.to_string()),
        mode: Some(object.mode),
        kind: Some(object.kind),
        artifact,
        content_hash,
        bytes,
    })
}

fn write_json_noclobber(path: &Path, value: &SyncConflictRecord) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::operation("conflict record has no parent directory"))?;
    let bytes = serde_json::to_vec_pretty(value).map_err(AppError::operation)?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(AppError::operation)?;
    temporary.write_all(&bytes).map_err(AppError::operation)?;
    temporary.write_all(b"\n").map_err(AppError::operation)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AppError::operation)?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| AppError::operation(error.error))?;
    Ok(())
}

fn write_json_replace(path: &Path, value: &SyncConflictResolutionRecord) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::operation("conflict resolution has no parent directory"))?;
    fs::create_dir_all(parent).map_err(AppError::operation)?;
    let bytes = serde_json::to_vec_pretty(value).map_err(AppError::operation)?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(AppError::operation)?;
    temporary.write_all(&bytes).map_err(AppError::operation)?;
    temporary.write_all(b"\n").map_err(AppError::operation)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AppError::operation)?;
    temporary
        .persist(path)
        .map_err(|error| AppError::operation(error.error))?;
    Ok(())
}

fn write_bytes_noclobber(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    if path.exists() {
        let existing = fs::read(path).map_err(AppError::operation)?;
        if existing == bytes {
            return Ok(());
        }
        return Err(AppError::operation(format!(
            "immutable conflict artifact differs at {}",
            path.display()
        )));
    }
    let parent = path
        .parent()
        .ok_or_else(|| AppError::operation("conflict artifact has no parent directory"))?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(AppError::operation)?;
    temporary.write_all(bytes).map_err(AppError::operation)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AppError::operation)?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| AppError::operation(error.error))?;
    Ok(())
}

fn validate_record(
    record: &SyncConflictRecord,
    repository_key: &str,
    conflict_id: &str,
) -> Result<(), AppError> {
    if record.version != SYNC_CONFLICT_RECORD_VERSION
        || record.repository_key != repository_key
        || record.id != conflict_id
    {
        return Err(AppError::operation(
            "sync conflict record version or identity mismatch",
        ));
    }
    Ok(())
}

fn verify_record_inputs(
    record: &SyncConflictRecord,
    conflict: &GitSyncConflict,
) -> Result<(), AppError> {
    if record.base_revision.as_deref() != conflict.base.as_ref().map(GitOid::as_str)
        || record.local_revision != conflict.local.as_str()
        || record.remote_revision != conflict.remote.as_str()
        || record.policy_version != conflict.policy_version
        || record.policy_hash != conflict.policy_hash
        || record
            .provenance_revision
            .as_deref()
            .is_some_and(|revision| revision != conflict.provenance_revision.as_str())
        || record
            .preserved_record_ref
            .as_deref()
            .is_some_and(|reference| reference != conflict.preserved_refs.record.as_str())
        || record
            .paths
            .iter()
            .map(|path| &path.path)
            .ne(conflict.paths.iter())
        || (record
            .paths
            .iter()
            .any(|path| path.classification.is_some())
            && record
                .paths
                .iter()
                .filter_map(|path| path.classification.as_ref())
                .ne(conflict.classifications.iter()))
    {
        return Err(AppError::operation(format!(
            "immutable conflict record `{}` does not match the current conflict inputs",
            conflict.id
        )));
    }
    Ok(())
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
