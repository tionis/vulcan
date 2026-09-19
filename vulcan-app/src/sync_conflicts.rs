//! Durable device-local conflict records and preserved file artifacts.

use crate::durable_file::{self, DurableCreate};
use crate::scan::refresh_cache_incrementally;
use crate::sync::{load_validated_sync_config, validate_git_merge_tree};
use crate::sync_state::{same_work_tree, SyncStateStore};
use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use vulcan_core::{ScanSummary, VaultPaths};
use vulcan_sync::{
    conflict_recovery_ref, conflict_ref, conflict_resolved_ref, remote_conflict_ref,
    GitAutomaticMergeValidation, GitCaptureRequest, GitConflictClassification, GitConflictScope,
    GitConflictSide, GitContentMergeResolutionRequest, GitEngine, GitMergeResolutionRequest,
    GitOid, GitPushResult, GitRefName, GitRemote, GitRepository, GitResolvedPath, GitSyncConflict,
    GitSyncOptions, GitSyncRefs,
};

pub const SYNC_CONFLICT_RECORD_VERSION: u32 = 4;
pub const SYNC_CONFLICT_RESOLUTION_VERSION: u32 = 2;
pub const SYNC_CONFLICT_SUPERSESSION_VERSION: u32 = 1;
pub const SYNC_CONFLICT_BATCH_VERSION: u32 = 1;
/// Conflict records were originally written without enforcing the reader's
/// 1 MiB ceiling. Keep a bounded compatibility window large enough to recover
/// those records until the paged conflict-store format replaces monolithic
/// JSON records.
const MAX_CONFLICT_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CONFLICT_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_CONFLICT_PATH_PAGE_BYTES: u64 = 1024 * 1024;
const MAX_CONFLICT_PATHS_PER_PAGE: usize = 128;
const MAX_CONFLICT_RESOLUTION_BYTES: u64 = 1024 * 1024;
const MAX_CONFLICT_GROUPS_PER_BATCH: usize = 128;
/// Fully resolved conflicts keep their records and resolution metadata
/// forever, but only the newest few resolved conflicts retain the
/// device-local artifact copies; the immutable Git refs remain the durable
/// byte archive.
const MAX_RETAINED_RESOLVED_ARTIFACT_SETS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflictRecord {
    pub version: u32,
    pub id: String,
    pub repository_key: String,
    pub work_tree: PathBuf,
    pub base_revision: Option<String>,
    pub local_revision: String,
    pub remote_revision: String,
    #[serde(default = "default_conflict_scope")]
    pub scope: GitConflictScope,
    pub policy_version: u32,
    pub policy_hash: String,
    pub preserved_base_ref: Option<String>,
    pub preserved_local_ref: String,
    pub preserved_remote_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserved_record_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance_revision: Option<String>,
    /// Records written before the projection rename stored the same
    /// `tree`/`published`/`applied` payload under `materialization`; the
    /// obsolete `directory`/`copies` fields are ignored on load.
    #[serde(
        default,
        alias = "materialization",
        skip_serializing_if = "Option::is_none"
    )]
    pub projection: Option<SyncConflictProjectionRecord>,
    pub paths: Vec<SyncConflictPathRecord>,
    pub diagnostics: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflictProjectionRecord {
    pub tree: String,
    #[serde(default)]
    pub published: bool,
    #[serde(default)]
    pub applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflictPathRecord {
    pub path: String,
    /// Stable resolution-group identity. Records before version 4 are
    /// normalized on load so callers never need a legacy special case.
    #[serde(default)]
    pub group_id: String,
    #[serde(default)]
    pub group_kind: SyncConflictGroupKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<GitConflictClassification>,
    pub base: SyncConflictSideRecord,
    pub local: SyncConflictSideRecord,
    pub remote: SyncConflictSideRecord,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncConflictGroupKind {
    #[default]
    Path,
    Structural,
    WholeTree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflictGroup {
    pub id: String,
    pub kind: SyncConflictGroupKind,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncConflictGroupState {
    Pending,
    Prepared,
    Published,
    Applied,
    NeedsRebase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflictBatchRecord {
    pub version: u32,
    pub conflict_id: String,
    pub batch_id: String,
    pub group_ids: Vec<String>,
    /// Immutable path cardinality for every selected group. New records use
    /// this to compute global progress without rereading every evidence page.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub group_path_counts: BTreeMap<String, usize>,
    pub selection_digest: String,
    pub expected_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<SyncConflictResolutionSide>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    pub recovery_revision: String,
    pub resolved_tree: String,
    pub resolution_commit: String,
    #[serde(default)]
    pub published: bool,
    #[serde(default)]
    pub applied: bool,
    #[serde(default)]
    pub needs_rebase: bool,
}

impl SyncConflictBatchRecord {
    #[must_use]
    pub const fn state(&self) -> SyncConflictGroupState {
        if self.needs_rebase {
            SyncConflictGroupState::NeedsRebase
        } else if self.applied {
            SyncConflictGroupState::Applied
        } else if self.published {
            SyncConflictGroupState::Published
        } else {
            SyncConflictGroupState::Prepared
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflictGroupProgress {
    pub id: String,
    pub kind: SyncConflictGroupKind,
    pub paths: Vec<String>,
    pub state: SyncConflictGroupState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflictProgress {
    pub total_groups: usize,
    pub pending_groups: usize,
    pub prepared_groups: usize,
    pub published_groups: usize,
    pub applied_groups: usize,
    pub needs_rebase_groups: usize,
    pub total_paths: usize,
    pub pending_paths: usize,
    /// Number of group detail records included in this response. This can be
    /// smaller than `total_groups` for a paged conflict-detail request.
    pub returned_groups: usize,
    /// Whether `groups` contains the complete group inventory.
    pub groups_complete: bool,
    pub groups: Vec<SyncConflictGroupProgress>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SyncConflictPathPageRef {
    file: String,
    count: usize,
    digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SyncConflictPathPage {
    version: u32,
    index: usize,
    paths: Vec<SyncConflictPathRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct SyncConflictPathManifest {
    version: u32,
    path_count: usize,
    #[serde(default)]
    group_count: Option<usize>,
    paths_digest: String,
    path_pages: Vec<SyncConflictPathPageRef>,
}

/// Derives the stable, bounded group inventory from immutable path evidence.
/// Structural paths are deliberately kept together until Vulcan has enough
/// rename/collision topology to prove finer independence.
#[must_use]
pub fn conflict_groups(record: &SyncConflictRecord) -> Vec<SyncConflictGroup> {
    if effective_conflict_scope(record) == GitConflictScope::TreeValidation {
        return vec![SyncConflictGroup {
            id: conflict_group_id(SyncConflictGroupKind::WholeTree, &[]),
            kind: SyncConflictGroupKind::WholeTree,
            paths: record.paths.iter().map(|path| path.path.clone()).collect(),
        }];
    }
    let mut groups = BTreeMap::<String, SyncConflictGroup>::new();
    for path in &record.paths {
        groups
            .entry(path.group_id.clone())
            .or_insert_with(|| SyncConflictGroup {
                id: path.group_id.clone(),
                kind: path.group_kind,
                paths: Vec::new(),
            })
            .paths
            .push(path.path.clone());
    }
    groups
        .into_values()
        .map(|mut group| {
            group.paths.sort();
            group
        })
        .collect()
}

#[must_use]
pub fn conflict_group_selection_digest(group_ids: &[String]) -> String {
    let mut ids = group_ids.to_vec();
    ids.sort();
    ids.dedup();
    let mut input = b"vulcan-conflict-group-selection-v1".to_vec();
    for id in ids {
        input.push(0);
        input.extend_from_slice(id.as_bytes());
    }
    blake3::hash(&input).to_hex().to_string()
}

fn selected_group_path_counts(
    record: &SyncConflictRecord,
    group_ids: &[String],
) -> BTreeMap<String, usize> {
    let selected = group_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut counts = BTreeMap::new();
    for path in &record.paths {
        if selected.contains(path.group_id.as_str()) {
            *counts.entry(path.group_id.clone()).or_insert(0) += 1;
        }
    }
    counts
}

#[must_use]
pub fn conflict_batch_id(
    conflict_id: &str,
    group_ids: &[String],
    expected_revision: &str,
    method_identity: &str,
) -> String {
    let selection = conflict_group_selection_digest(group_ids);
    blake3::hash(
        format!(
            "vulcan-conflict-batch-v1\0{conflict_id}\0{selection}\0{expected_revision}\0{method_identity}"
        )
        .as_bytes(),
    )
    .to_hex()[..32]
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflictSummary {
    pub id: String,
    pub scope: GitConflictScope,
    pub paths: Vec<String>,
    pub path_count: usize,
    pub group_count: usize,
    pub pending_group_count: usize,
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
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflictSupersessionRecord {
    pub version: u32,
    pub conflict_id: String,
    pub current_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_conflict_id: Option<String>,
}

const fn default_conflict_scope() -> GitConflictScope {
    GitConflictScope::Paths
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
    pub group_ids: Vec<String>,
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflictResolutionRecord {
    pub version: u32,
    pub conflict_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<SyncConflictResolutionSide>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    pub base_revision: String,
    pub local_revision: String,
    pub remote_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_input_revision: Option<String>,
    pub recovery_revision: String,
    pub resolved_tree: String,
    pub resolution_commit: String,
    pub published: bool,
    pub applied: bool,
}

impl SyncConflictResolutionRecord {
    /// A resolution that never published and never applied is a failed
    /// attempt, not an in-progress resolution. Guards must not let it block
    /// rejection, side switches, or competing proposals; the next attempt
    /// overwrites the stale record.
    #[must_use]
    pub fn is_abandoned(&self) -> bool {
        !self.published && !self.applied
    }
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_groups: Option<usize>,
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
    /// Number of currently actionable unresolved conflicts. Retained
    /// superseded history is reported separately and excluded here.
    pub count: usize,
    pub superseded_count: usize,
    pub conflicts: Vec<SyncConflictSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflictDetailReport {
    pub record: SyncConflictRecord,
    pub resolution: SyncConflictResolutionState,
    pub progress: SyncConflictProgress,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_page: Option<SyncConflictPathPageInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersession: Option<SyncConflictSupersessionRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflictPathPageInfo {
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
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
    let states = records
        .into_iter()
        .map(|record| {
            let resolution = store.resolution_state(&repository_key, &record.id)?;
            let scope = effective_conflict_scope(&record);
            let progress = store.group_progress(&repository_key, &record)?;
            let path_count = record.paths.len();
            Ok((
                resolution,
                SyncConflictSummary {
                    id: record.id,
                    scope,
                    paths: record.paths.into_iter().map(|path| path.path).collect(),
                    path_count,
                    group_count: progress.total_groups,
                    pending_group_count: progress.pending_groups + progress.needs_rebase_groups,
                    base_revision: record.base_revision,
                    local_revision: record.local_revision,
                    remote_revision: record.remote_revision,
                    policy_version: record.policy_version,
                    resolution,
                },
            ))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let superseded_count = states
        .iter()
        .filter(|(state, _)| *state == SyncConflictResolutionState::Superseded)
        .count();
    let conflicts = states
        .into_iter()
        .filter_map(|(state, summary)| {
            (state == SyncConflictResolutionState::Unresolved).then_some(summary)
        })
        .collect::<Vec<_>>();
    Ok(SyncConflictListReport {
        vault: work_tree,
        repository_key,
        count: conflicts.len(),
        superseded_count,
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
    let progress = store.group_progress(&repository_key, &record)?;
    let resolution =
        store.resolution_state_with_progress(&repository_key, conflict_id, &progress)?;
    let supersession = store.get_supersession(&repository_key, conflict_id)?;
    Ok(SyncConflictDetailReport {
        record,
        resolution,
        progress,
        path_page: None,
        supersession,
    })
}

pub fn get_sync_conflict_page(
    paths: &vulcan_core::VaultPaths,
    conflict_id: &str,
    offset: usize,
    limit: usize,
) -> Result<SyncConflictDetailReport, AppError> {
    if limit == 0 || limit > 256 {
        return Err(AppError::operation(
            "sync conflict path page limit must be between 1 and 256",
        ));
    }
    let state_store = SyncStateStore::user_default()?;
    get_sync_conflict_page_with_state_store(paths, conflict_id, offset, limit, &state_store)
}

pub fn get_sync_conflict_page_with_state_store(
    paths: &vulcan_core::VaultPaths,
    conflict_id: &str,
    offset: usize,
    limit: usize,
    state_store: &SyncStateStore,
) -> Result<SyncConflictDetailReport, AppError> {
    if limit == 0 || limit > 256 {
        return Err(AppError::operation(
            "sync conflict path page limit must be between 1 and 256",
        ));
    }
    let work_tree = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let repository_key = crate::sync_state::repository_state_key(&work_tree);
    let store = SyncConflictStore::from_state_store(state_store);
    let (record, total, progress) =
        store.get_page_and_progress(&repository_key, conflict_id, offset, limit)?;
    let resolution =
        store.resolution_state_with_progress(&repository_key, conflict_id, &progress)?;
    let supersession = store.get_supersession(&repository_key, conflict_id)?;
    let next_offset = offset
        .saturating_add(record.paths.len())
        .lt(&total)
        .then(|| offset + record.paths.len());
    Ok(SyncConflictDetailReport {
        record,
        resolution,
        progress,
        path_page: Some(SyncConflictPathPageInfo {
            offset,
            limit,
            total,
            next_offset,
        }),
        supersession,
    })
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
    if !same_work_tree(&record.work_tree, &work_tree) {
        return Err(AppError::operation(
            "sync conflict record does not belong to the selected worktree",
        ));
    }
    if store.resolution_state(&repository_key, conflict_id)?
        == SyncConflictResolutionState::Superseded
    {
        return Err(AppError::operation(format!(
            "conflict `{conflict_id}` was superseded by later synchronization and is retained only as history; choose a currently unresolved record from `vulcan sync conflicts`"
        )));
    }
    if !options.group_ids.is_empty() {
        return resolve_sync_conflict_groups_with_state_store(
            paths,
            options,
            state_store,
            &store,
            &record,
            &context,
        );
    }
    let existing_resolution = store.get_effective_resolution(&repository_key, conflict_id)?;
    if let Some(existing) = &existing_resolution {
        if existing.side != Some(options.side) || existing.proposal_id.is_some() {
            return Err(AppError::operation(format!(
                "conflict `{conflict_id}` already has another resolution in progress"
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

#[allow(clippy::too_many_lines)]
fn resolve_sync_conflict_groups_with_state_store(
    paths: &VaultPaths,
    options: &ResolveSyncConflictOptions,
    state_store: &SyncStateStore,
    store: &SyncConflictStore,
    record: &SyncConflictRecord,
    context: &ResolutionContext,
) -> Result<ResolveSyncConflictReport, AppError> {
    let mut group_ids = options.group_ids.clone();
    group_ids.sort();
    group_ids.dedup();
    if group_ids.len() > MAX_CONFLICT_GROUPS_PER_BATCH {
        return Err(AppError::operation(format!(
            "one conflict batch may select at most {MAX_CONFLICT_GROUPS_PER_BATCH} groups"
        )));
    }
    let groups = conflict_groups(record);
    let by_id = groups
        .iter()
        .map(|group| (group.id.as_str(), group))
        .collect::<BTreeMap<_, _>>();
    let mut selected_paths = Vec::new();
    for group_id in &group_ids {
        validate_hex_id("conflict group ID", group_id)?;
        let group = by_id
            .get(group_id.as_str())
            .ok_or_else(|| AppError::operation(format!("unknown conflict group `{group_id}`")))?;
        if group.kind == SyncConflictGroupKind::WholeTree {
            return Err(AppError::operation(
                "whole-tree validation conflicts cannot be partially resolved",
            ));
        }
        selected_paths.extend(group.paths.iter().cloned());
    }
    selected_paths.sort();
    selected_paths.dedup();
    let progress = store.group_progress(&context.repository_key, record)?;
    let selected_states = progress
        .groups
        .iter()
        .filter(|group| group_ids.contains(&group.id))
        .map(|group| group.state)
        .collect::<Vec<_>>();
    let active_batch = store
        .list_batches(&context.repository_key, &record.id)?
        .into_iter()
        .find(|batch| {
            batch.group_ids == group_ids && batch.side == Some(options.side) && !batch.needs_rebase
        });
    if selected_states
        .iter()
        .all(|state| *state == SyncConflictGroupState::Applied)
    {
        return Ok(group_resolution_report(
            context,
            options,
            ResolveSyncConflictOutcome::AlreadyResolved,
            group_ids,
            active_batch.as_ref().map(|batch| batch.batch_id.clone()),
            progress.pending_groups + progress.needs_rebase_groups,
            None,
            None,
            None,
        ));
    }
    if active_batch.is_none()
        && selected_states.iter().any(|state| {
            !matches!(
                state,
                SyncConflictGroupState::Pending | SyncConflictGroupState::NeedsRebase
            )
        })
    {
        return Err(AppError::operation(
            "one or more selected groups already have an active resolution batch",
        ));
    }

    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&context.vault)
        .map_err(AppError::operation)?;
    verify_preserved_conflict_refs(&engine, &repository, record)?;
    let safety = engine
        .safety_state(&repository)
        .map_err(AppError::operation)?;
    reject_unsafe_resolution(&safety)?;
    let current = engine
        .remote_ref(&repository, &options.remote, &options.live_ref)
        .map_err(AppError::operation)?
        .ok_or_else(|| AppError::operation("the remote live ref is missing"))?;
    let prepared_frontier = active_batch
        .as_ref()
        .map(|batch| GitOid::parse(&batch.expected_revision).map_err(AppError::operation))
        .transpose()?
        .unwrap_or_else(|| current.clone());
    let mut replan_unpublished = false;
    let mut published_descendant = false;
    if let Some(batch) = active_batch.as_ref() {
        let commit = GitOid::parse(&batch.resolution_commit).map_err(AppError::operation)?;
        if current != prepared_frontier && current != commit {
            if batch.published
                && engine
                    .is_ancestor(&repository, &commit, &current)
                    .map_err(AppError::operation)?
            {
                published_descendant = true;
            } else if !batch.published {
                replan_unpublished = true;
            } else {
                return Err(AppError::operation(
                    "the remote live ref diverged from the published conflict batch",
                ));
            }
        }
    }
    let frontier = if replan_unpublished || published_descendant {
        current.clone()
    } else {
        prepared_frontier
    };
    if !published_descendant {
        ensure_group_frontier_unchanged(&engine, &repository, record, &frontier, &selected_paths)?;
    }
    let frontier_tree = engine
        .tree_oid(&repository, &frontier)
        .map_err(AppError::operation)?;
    let current_tree = engine
        .tree_oid(&repository, &current)
        .map_err(AppError::operation)?;
    let worktree_tree = engine
        .snapshot_worktree_tree(&repository, Some(&current))
        .map_err(AppError::operation)?;
    let prepared_tree = active_batch
        .as_ref()
        .map(|batch| GitOid::parse(&batch.resolved_tree).map_err(AppError::operation))
        .transpose()?;
    if worktree_tree != frontier_tree
        && worktree_tree != current_tree
        && prepared_tree.as_ref() != Some(&worktree_tree)
    {
        return Err(AppError::operation(
            "the worktree does not match the current accepted live tree; synchronize or preserve local edits before resolving groups",
        ));
    }
    let batch_id = active_batch
        .as_ref()
        .filter(|_| !replan_unpublished)
        .map_or_else(
            || {
                conflict_batch_id(
                    &record.id,
                    &group_ids,
                    current.as_str(),
                    &format!("side:{}", resolution_side_name(options.side)),
                )
            },
            |batch| batch.batch_id.clone(),
        );
    if options.dry_run {
        return Ok(group_resolution_report(
            context,
            options,
            ResolveSyncConflictOutcome::Planned,
            group_ids,
            Some(batch_id),
            progress.pending_groups + progress.needs_rebase_groups,
            None,
            None,
            None,
        ));
    }

    let _lock = vulcan_sync::RepositoryLock::acquire(&repository.git_dir)?;
    resolve_sync_conflict_group_batch(
        paths,
        options,
        state_store,
        store,
        record,
        context,
        &engine,
        &repository,
        &group_ids,
        &selected_paths,
        &current,
        &batch_id,
        active_batch,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn resolve_sync_conflict_group_batch(
    paths: &VaultPaths,
    options: &ResolveSyncConflictOptions,
    state_store: &SyncStateStore,
    store: &SyncConflictStore,
    record: &SyncConflictRecord,
    context: &ResolutionContext,
    engine: &dyn GitEngine,
    repository: &GitRepository,
    group_ids: &[String],
    selected_paths: &[String],
    current: &GitOid,
    batch_id: &str,
    mut existing_batch: Option<SyncConflictBatchRecord>,
) -> Result<ResolveSyncConflictReport, AppError> {
    verify_preserved_conflict_refs(engine, repository, record)?;
    let device_id = state_store
        .load_or_create_device_id(true)?
        .expect("mutating device identity creation returns an identity");
    let recovery_ref = conflict_recovery_ref(&record.id, &format!("batch-{batch_id}"))
        .map_err(AppError::operation)?;
    let capture = engine
        .capture_worktree(
            repository,
            &GitCaptureRequest {
                base: Some(current.clone()),
                target_ref: recovery_ref,
                target_before: None,
                message: format!(
                    "vulcan conflict batch recovery snapshot\n\nVulcan-Conflict: {}\nVulcan-Conflict-Batch: {batch_id}\nVulcan-Sync-Version: 2\nVulcan-Sync-Device: {}\nVulcan-Sync-Source: {current}\nVulcan-Sync-Semantic: false\n",
                    record.id,
                    device_id.as_str(),
                ),
            },
        )
        .map_err(AppError::operation)?;
    let immutable_recovery_ref =
        conflict_recovery_ref(&record.id, capture.commit.as_str()).map_err(AppError::operation)?;
    engine
        .update_ref(repository, &immutable_recovery_ref, &capture.commit)
        .map_err(AppError::operation)?;

    if let Some(batch) = existing_batch.as_mut() {
        let expected = GitOid::parse(&batch.expected_revision).map_err(AppError::operation)?;
        let commit = GitOid::parse(&batch.resolution_commit).map_err(AppError::operation)?;
        if *current != expected && *current != commit {
            if batch.published
                && engine
                    .is_ancestor(repository, &commit, current)
                    .map_err(AppError::operation)?
            {
                let recorded_tree =
                    GitOid::parse(&batch.resolved_tree).map_err(AppError::operation)?;
                let commit_tree = engine
                    .tree_oid(repository, &commit)
                    .map_err(AppError::operation)?;
                if commit_tree != recorded_tree {
                    return Err(AppError::operation(
                        "published conflict batch commit does not match its recorded tree",
                    ));
                }
                if capture.tree != commit_tree
                    && capture.tree
                        != engine
                            .tree_oid(repository, current)
                            .map_err(AppError::operation)?
                {
                    return Err(AppError::operation(
                        "the worktree changed while reconciling the published conflict batch; its recovery snapshot was retained",
                    ));
                }
                if capture.tree
                    != engine
                        .tree_oid(repository, current)
                        .map_err(AppError::operation)?
                {
                    engine
                        .apply_tree(repository, &capture.commit, current)
                        .map_err(AppError::operation)?;
                }
                update_resolution_sync_refs(engine, repository, options, current)?;
                let cache_refresh = if paths.cache_db().is_file() {
                    Some(refresh_cache_incrementally(paths)?)
                } else {
                    None
                };
                batch.recovery_revision = capture.commit.to_string();
                batch.applied = true;
                store.save_batch(&context.repository_key, batch)?;
                let progress = store.group_progress(&context.repository_key, record)?;
                return Ok(group_resolution_report(
                    context,
                    options,
                    ResolveSyncConflictOutcome::Resolved,
                    batch.group_ids.clone(),
                    Some(batch.batch_id.clone()),
                    progress.pending_groups + progress.needs_rebase_groups,
                    Some(batch.recovery_revision.clone()),
                    Some(batch.resolution_commit.clone()),
                    cache_refresh,
                ));
            }
            if !batch.published {
                batch.needs_rebase = true;
                batch.recovery_revision = capture.commit.to_string();
                store.save_batch(&context.repository_key, batch)?;
                existing_batch = None;
            }
        }
    }

    if let Some(mut batch) = existing_batch {
        let expected = GitOid::parse(&batch.expected_revision).map_err(AppError::operation)?;
        let expected_tree = engine
            .tree_oid(repository, &expected)
            .map_err(AppError::operation)?;
        let tree = GitOid::parse(&batch.resolved_tree).map_err(AppError::operation)?;
        let commit = GitOid::parse(&batch.resolution_commit).map_err(AppError::operation)?;
        if engine
            .tree_oid(repository, &commit)
            .map_err(AppError::operation)?
            != tree
        {
            return Err(AppError::operation(
                "prepared conflict batch commit does not match its recorded tree",
            ));
        }
        if capture.tree != expected_tree && capture.tree != tree {
            return Err(AppError::operation(
                "the worktree changed while the conflict batch was pending; its recovery snapshot was retained",
            ));
        }
        validate_conflict_group_tree(
            paths,
            engine,
            repository,
            record,
            &expected,
            &tree,
            selected_paths,
        )?;
        batch.recovery_revision = capture.commit.to_string();
        store.save_batch(&context.repository_key, &batch)?;
        return publish_and_apply_conflict_group_batch(
            paths, options, store, record, context, engine, repository, &capture, batch, &commit,
            &tree,
        );
    }

    let current_tree = engine
        .tree_oid(repository, current)
        .map_err(AppError::operation)?;
    if capture.tree != current_tree {
        return Err(AppError::operation(
            "the worktree changed while preparing the conflict batch; its recovery snapshot was retained",
        ));
    }

    let resolved_paths =
        selected_side_paths(engine, repository, record, options.side, selected_paths)?;
    let tree = engine
        .resolve_merge_tree_with_paths(
            repository,
            &GitContentMergeResolutionRequest {
                base: current.clone(),
                accepted_remote: current.clone(),
                local_candidate: current.clone(),
                paths: resolved_paths,
            },
        )
        .map_err(AppError::operation)?;
    validate_conflict_group_tree(
        paths,
        engine,
        repository,
        record,
        current,
        &tree,
        selected_paths,
    )?;
    let commit = engine
        .create_commit(
            repository,
            &tree,
            std::slice::from_ref(current),
            &format!(
                "vulcan conflict batch resolution\n\nVulcan-Conflict: {}\nVulcan-Conflict-Batch: {batch_id}\nVulcan-Conflict-Selection: {}\nVulcan-Resolution-Side: {}\nVulcan-Sync-Version: 2\nVulcan-Sync-Device: {}\nVulcan-Sync-Policy: {}:{}\nVulcan-Sync-Source: {current}\nVulcan-Sync-Semantic: false\n",
                record.id,
                conflict_group_selection_digest(group_ids),
                resolution_side_name(options.side),
                device_id.as_str(),
                record.policy_version,
                record.policy_hash,
            ),
        )
        .map_err(AppError::operation)?;
    let local_ref = conflict_ref(&record.id, &format!("resolved/batches/{batch_id}"))
        .map_err(AppError::operation)?;
    engine
        .update_ref(repository, &local_ref, &commit)
        .map_err(AppError::operation)?;
    let batch = SyncConflictBatchRecord {
        version: SYNC_CONFLICT_BATCH_VERSION,
        conflict_id: record.id.clone(),
        batch_id: batch_id.to_string(),
        selection_digest: conflict_group_selection_digest(group_ids),
        group_ids: group_ids.to_vec(),
        group_path_counts: selected_group_path_counts(record, group_ids),
        expected_revision: current.to_string(),
        side: Some(options.side),
        proposal_id: None,
        recovery_revision: capture.commit.to_string(),
        resolved_tree: tree.to_string(),
        resolution_commit: commit.to_string(),
        published: false,
        applied: false,
        needs_rebase: false,
    };
    store.save_batch(&context.repository_key, &batch)?;
    publish_and_apply_conflict_group_batch(
        paths, options, store, record, context, engine, repository, &capture, batch, &commit, &tree,
    )
}

fn validate_conflict_group_tree(
    paths: &VaultPaths,
    engine: &dyn GitEngine,
    repository: &GitRepository,
    record: &SyncConflictRecord,
    accepted: &GitOid,
    tree: &GitOid,
    selected_paths: &[String],
) -> Result<(), AppError> {
    let base = record
        .base_revision
        .as_deref()
        .ok_or_else(|| AppError::operation("group resolution requires one merge base"))
        .and_then(|value| GitOid::parse(value).map_err(AppError::operation))?;
    let local = GitOid::parse(&record.local_revision).map_err(AppError::operation)?;
    let config = load_validated_sync_config(paths)?;
    validate_git_merge_tree(
        &config,
        engine,
        &GitAutomaticMergeValidation {
            repository,
            base: &base,
            local_candidate: &local,
            accepted_remote: accepted,
            merged_tree: tree,
            resolved_paths: selected_paths,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_and_apply_conflict_group_batch(
    paths: &VaultPaths,
    options: &ResolveSyncConflictOptions,
    store: &SyncConflictStore,
    record: &SyncConflictRecord,
    context: &ResolutionContext,
    engine: &dyn GitEngine,
    repository: &GitRepository,
    capture: &vulcan_sync::GitCapture,
    mut batch: SyncConflictBatchRecord,
    commit: &GitOid,
    tree: &GitOid,
) -> Result<ResolveSyncConflictReport, AppError> {
    publish_conflict_group_batch(engine, repository, options, &mut batch)?;
    store.save_batch(&context.repository_key, &batch)?;
    if capture.tree != *tree {
        engine
            .apply_tree(repository, &capture.commit, commit)
            .map_err(AppError::operation)?;
    }
    update_resolution_sync_refs(engine, repository, options, commit)?;
    let cache_refresh = if paths.cache_db().is_file() {
        Some(refresh_cache_incrementally(paths)?)
    } else {
        None
    };
    batch.applied = true;
    store.save_batch(&context.repository_key, &batch)?;
    let progress = store.group_progress(&context.repository_key, record)?;
    Ok(group_resolution_report(
        context,
        options,
        ResolveSyncConflictOutcome::Resolved,
        batch.group_ids.clone(),
        Some(batch.batch_id.clone()),
        progress.pending_groups + progress.needs_rebase_groups,
        Some(batch.recovery_revision),
        Some(batch.resolution_commit),
        cache_refresh,
    ))
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
    let _lock = vulcan_sync::RepositoryLock::acquire(&repository.git_dir)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let device_id = state_store
        .load_or_create_device_id(true)?
        .expect("mutating device identity creation returns an identity");
    verify_preserved_conflict_refs(&engine, repository, record)?;
    let local = conflict_worktree_revision(record)?;
    let recovery_ref =
        conflict_recovery_ref(&context.conflict_id, "current").map_err(AppError::operation)?;
    let capture = engine
        .capture_worktree(
            repository,
            &GitCaptureRequest {
                base: Some(local.clone()),
                target_ref: recovery_ref,
                target_before: None,
                message: format!(
                    "vulcan conflict recovery snapshot\n\nVulcan-Conflict: {}\nVulcan-Sync-Version: 2\nVulcan-Sync-Device: {}\nVulcan-Sync-Source: {}\nVulcan-Sync-Semantic: false\n",
                    context.conflict_id,
                    device_id.as_str(),
                    local
                ),
            },
        )
        .map_err(AppError::operation)?;
    let immutable_recovery_ref =
        conflict_recovery_ref(&context.conflict_id, capture.commit.as_str())
            .map_err(AppError::operation)?;
    engine
        .update_ref(repository, &immutable_recovery_ref, &capture.commit)
        .map_err(AppError::operation)?;
    let safety = engine
        .safety_state(repository)
        .map_err(AppError::operation)?;
    reject_unsafe_resolution(&safety)?;

    let existing = store.get_effective_resolution(&context.repository_key, &context.conflict_id)?;
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
    let remote_before = resolution_live_input(&resolution)?;
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

    publish_remote_side_resolution(
        &engine,
        repository,
        &options.remote,
        &context.conflict_id,
        &resolution_commit,
    )?;

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
    engine
        .update_refs(
            repository,
            &[
                (&refs.local, &resolution_commit),
                (&refs.fetched, &resolution_commit),
                (&refs.pending, &resolution_commit),
            ],
        )
        .map_err(AppError::operation)?;
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

fn publish_remote_side_resolution(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    remote: &GitRemote,
    conflict_id: &str,
    resolution_commit: &GitOid,
) -> Result<(), AppError> {
    let resolved_ref =
        remote_conflict_ref(conflict_id, "resolved/side").map_err(AppError::operation)?;
    match engine
        .remote_ref(repository, remote, &resolved_ref)
        .map_err(AppError::operation)?
    {
        Some(existing) if existing == *resolution_commit => Ok(()),
        Some(_) => Err(AppError::operation(format!(
            "remote conflict resolution `{resolved_ref}` identifies a different commit"
        ))),
        None => {
            if engine
                .push_ref(repository, remote, resolution_commit, &resolved_ref, None)
                .map_err(AppError::operation)?
                == GitPushResult::Rejected
                && engine
                    .remote_ref(repository, remote, &resolved_ref)
                    .map_err(AppError::operation)?
                    .as_ref()
                    != Some(resolution_commit)
            {
                return Err(AppError::operation(format!(
                    "remote conflict resolution `{resolved_ref}` was created concurrently with a different commit"
                )));
            }
            Ok(())
        }
    }
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
            group_ids: options.group_ids.clone(),
            batch_id: None,
            remaining_groups: None,
            recovery_revision,
            resolution_commit,
            cache_refresh,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn group_resolution_report(
    context: &ResolutionContext,
    options: &ResolveSyncConflictOptions,
    outcome: ResolveSyncConflictOutcome,
    group_ids: Vec<String>,
    batch_id: Option<String>,
    remaining_groups: usize,
    recovery_revision: Option<String>,
    resolution_commit: Option<String>,
    cache_refresh: Option<ScanSummary>,
) -> ResolveSyncConflictReport {
    ResolveSyncConflictReport {
        vault: context.vault.clone(),
        repository_key: context.repository_key.clone(),
        conflict_id: context.conflict_id.clone(),
        side: options.side,
        dry_run: options.dry_run,
        outcome,
        group_ids,
        batch_id,
        remaining_groups: Some(remaining_groups),
        recovery_revision,
        resolution_commit,
        cache_refresh,
    }
}

pub(crate) fn verify_preserved_conflict_refs(
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
    let matches_input = remote.as_ref().map(GitOid::as_str) == Some(conflict_live_input(record)?);
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

pub(crate) fn conflict_live_input(record: &SyncConflictRecord) -> Result<&str, AppError> {
    if record
        .projection
        .as_ref()
        .is_some_and(|projection| projection.published)
    {
        record.provenance_revision.as_deref().ok_or_else(|| {
            AppError::operation("published conflict projection has no provenance revision")
        })
    } else {
        Ok(&record.remote_revision)
    }
}

pub(crate) fn conflict_worktree_revision(record: &SyncConflictRecord) -> Result<GitOid, AppError> {
    let revision = if record
        .projection
        .as_ref()
        .is_some_and(|projection| projection.applied)
    {
        record.provenance_revision.as_deref().ok_or_else(|| {
            AppError::operation("applied conflict projection has no provenance revision")
        })?
    } else {
        &record.local_revision
    };
    GitOid::parse(revision).map_err(AppError::operation)
}

pub(crate) fn conflict_worktree_tree(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    record: &SyncConflictRecord,
) -> Result<GitOid, AppError> {
    if let Some(projection) = record
        .projection
        .as_ref()
        .filter(|projection| projection.applied)
    {
        let tree = GitOid::parse(&projection.tree).map_err(AppError::operation)?;
        let revision = conflict_worktree_revision(record)?;
        if engine
            .tree_oid(repository, &revision)
            .map_err(AppError::operation)?
            != tree
        {
            return Err(AppError::operation(
                "conflict projection provenance tree no longer matches its durable record",
            ));
        }
        Ok(tree)
    } else {
        let revision = conflict_worktree_revision(record)?;
        engine
            .tree_oid(repository, &revision)
            .map_err(AppError::operation)
    }
}

fn resolution_live_input(resolution: &SyncConflictResolutionRecord) -> Result<GitOid, AppError> {
    GitOid::parse(
        resolution
            .live_input_revision
            .as_deref()
            .unwrap_or(&resolution.remote_revision),
    )
    .map_err(AppError::operation)
}

fn resolve_projected_conflict_tree(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    record: &SyncConflictRecord,
    side: SyncConflictResolutionSide,
    live_input: &GitOid,
) -> Result<GitOid, AppError> {
    let selected = match side {
        SyncConflictResolutionSide::Base => record
            .base_revision
            .as_deref()
            .ok_or_else(|| AppError::operation("this conflict has no merge base to select"))?,
        SyncConflictResolutionSide::Local => &record.local_revision,
        SyncConflictResolutionSide::Remote => &record.remote_revision,
    };
    let selected = GitOid::parse(selected).map_err(AppError::operation)?;
    let mut paths = Vec::new();
    for path in &record.paths {
        let object = engine
            .path_object(repository, &selected, &path.path)
            .map_err(AppError::operation)?;
        paths.push(object.map_or(
            GitResolvedPath {
                path: path.path.clone(),
                mode: None,
                data: None,
            },
            |object| GitResolvedPath {
                path: path.path.clone(),
                mode: Some(object.mode),
                data: object.data,
            },
        ));
    }
    engine
        .resolve_merge_tree_with_paths(
            repository,
            &GitContentMergeResolutionRequest {
                base: live_input.clone(),
                accepted_remote: live_input.clone(),
                local_candidate: live_input.clone(),
                paths,
            },
        )
        .map_err(AppError::operation)
}

fn selected_side_paths(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    record: &SyncConflictRecord,
    side: SyncConflictResolutionSide,
    paths: &[String],
) -> Result<Vec<GitResolvedPath>, AppError> {
    let selected = match side {
        SyncConflictResolutionSide::Base => record
            .base_revision
            .as_deref()
            .ok_or_else(|| AppError::operation("this conflict has no merge base to select"))?,
        SyncConflictResolutionSide::Local => &record.local_revision,
        SyncConflictResolutionSide::Remote => &record.remote_revision,
    };
    let selected = GitOid::parse(selected).map_err(AppError::operation)?;
    let objects = engine
        .path_objects(repository, &selected, paths)
        .map_err(AppError::operation)?;
    Ok(paths
        .iter()
        .map(|path| {
            objects.get(path).map_or(
                GitResolvedPath {
                    path: path.clone(),
                    mode: None,
                    data: None,
                },
                |object| GitResolvedPath {
                    path: path.clone(),
                    mode: Some(object.mode.clone()),
                    data: object.data.clone(),
                },
            )
        })
        .collect())
}

fn ensure_group_frontier_unchanged(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    record: &SyncConflictRecord,
    current: &GitOid,
    paths: &[String],
) -> Result<(), AppError> {
    let original = GitOid::parse(conflict_live_input(record)?).map_err(AppError::operation)?;
    if original == *current {
        return Ok(());
    }
    let original_objects = engine
        .path_objects(repository, &original, paths)
        .map_err(AppError::operation)?;
    let current_objects = engine
        .path_objects(repository, current, paths)
        .map_err(AppError::operation)?;
    if original_objects == current_objects {
        Ok(())
    } else {
        Err(AppError::operation(
            "one or more selected conflict groups changed on the accepted live frontier and require a fresh reconciliation",
        ))
    }
}

fn publish_conflict_group_batch(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    options: &ResolveSyncConflictOptions,
    batch: &mut SyncConflictBatchRecord,
) -> Result<(), AppError> {
    let expected = GitOid::parse(&batch.expected_revision).map_err(AppError::operation)?;
    let commit = GitOid::parse(&batch.resolution_commit).map_err(AppError::operation)?;
    match engine
        .remote_ref(repository, &options.remote, &options.live_ref)
        .map_err(AppError::operation)?
        .as_ref()
    {
        Some(current) if current == &commit => {}
        Some(current) if current == &expected => {
            if engine
                .push_ref(
                    repository,
                    &options.remote,
                    &commit,
                    &options.live_ref,
                    Some(&expected),
                )
                .map_err(AppError::operation)?
                == GitPushResult::Rejected
            {
                return Err(AppError::operation(
                    "the remote live ref changed while publishing the conflict batch",
                ));
            }
        }
        _ => {
            return Err(AppError::operation(
                "the remote live ref no longer matches the conflict batch frontier",
            ));
        }
    }
    let remote_ref = remote_conflict_ref(
        &batch.conflict_id,
        &format!("resolved/batches/{}", batch.batch_id),
    )
    .map_err(AppError::operation)?;
    match engine
        .remote_ref(repository, &options.remote, &remote_ref)
        .map_err(AppError::operation)?
    {
        Some(existing) if existing == commit => {}
        Some(_) => {
            return Err(AppError::operation(format!(
                "remote conflict batch ref `{remote_ref}` identifies a different commit"
            )));
        }
        None => {
            if engine
                .push_ref(repository, &options.remote, &commit, &remote_ref, None)
                .map_err(AppError::operation)?
                == GitPushResult::Rejected
            {
                return Err(AppError::operation(format!(
                    "remote conflict batch ref `{remote_ref}` was created concurrently"
                )));
            }
        }
    }
    batch.published = true;
    Ok(())
}

fn update_resolution_sync_refs(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    options: &ResolveSyncConflictOptions,
    commit: &GitOid,
) -> Result<(), AppError> {
    let refs = GitSyncRefs::for_options(&GitSyncOptions {
        remote: options.remote.clone(),
        live_ref: options.live_ref.clone(),
        ..GitSyncOptions::default()
    })
    .map_err(AppError::operation)?;
    engine
        .update_refs(
            repository,
            &[
                (&refs.local, commit),
                (&refs.fetched, commit),
                (&refs.pending, commit),
            ],
        )
        .map_err(AppError::operation)
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
    if effective_conflict_scope(record) == GitConflictScope::Paths {
        reject_synthesized_path_side_resolution(record)?;
    }
    let local = GitOid::parse(&record.local_revision).map_err(AppError::operation)?;
    let expected_tree = conflict_worktree_tree(engine, repository, record)?;
    if capture.tree != expected_tree {
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
    let live_input = GitOid::parse(conflict_live_input(record)?).map_err(AppError::operation)?;
    let tree = if effective_conflict_scope(record) == GitConflictScope::TreeValidation {
        resolve_tree_validation_conflict(engine, repository, record, options.side)?
    } else if record.projection.as_ref().is_some_and(|item| item.applied) {
        resolve_projected_conflict_tree(engine, repository, record, options.side, &live_input)?
    } else {
        engine
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
            .map_err(AppError::operation)?
    };
    let parents = if live_input == remote {
        vec![remote.clone(), local.clone()]
    } else {
        vec![live_input.clone()]
    };
    let commit = engine
        .create_commit(
            repository,
            &tree,
            &parents,
            &format!(
                "vulcan conflict resolution\n\nVulcan-Conflict: {}\nVulcan-Resolution-Side: {}\nVulcan-Sync-Version: 2\nVulcan-Sync-Device: {}\nVulcan-Sync-Policy: {}:{}\nVulcan-Sync-Source: {}+{}\nVulcan-Sync-Semantic: false\n",
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
    let resolved_ref = conflict_resolved_ref(&record.id).map_err(AppError::operation)?;
    engine
        .update_ref(repository, &resolved_ref, &commit)
        .map_err(AppError::operation)?;
    Ok(SyncConflictResolutionRecord {
        version: SYNC_CONFLICT_RESOLUTION_VERSION,
        conflict_id: record.id.clone(),
        side: Some(options.side),
        proposal_id: None,
        base_revision: base.to_string(),
        local_revision: local.to_string(),
        remote_revision: remote.to_string(),
        live_input_revision: Some(live_input.to_string()),
        recovery_revision: capture.commit.to_string(),
        resolved_tree: tree.to_string(),
        resolution_commit: commit.to_string(),
        published: false,
        applied: false,
    })
}

fn reject_synthesized_path_side_resolution(record: &SyncConflictRecord) -> Result<(), AppError> {
    if let Some(path) = record
        .paths
        .iter()
        .find(|path| path.local.object_id.is_none() && path.remote.object_id.is_none())
    {
        return Err(AppError::operation(format!(
            "structural conflict path `{}` was synthesized by Git and cannot be resolved by selecting a path side; the original local and remote revisions remain preserved",
            path.path
        )));
    }
    Ok(())
}

fn effective_conflict_scope(record: &SyncConflictRecord) -> GitConflictScope {
    if record.scope == GitConflictScope::TreeValidation || record.paths.is_empty() {
        GitConflictScope::TreeValidation
    } else {
        GitConflictScope::Paths
    }
}

fn resolve_tree_validation_conflict(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    record: &SyncConflictRecord,
    side: SyncConflictResolutionSide,
) -> Result<GitOid, AppError> {
    let selected = match side {
        SyncConflictResolutionSide::Base => record
            .base_revision
            .as_deref()
            .ok_or_else(|| AppError::operation("this conflict has no merge base to select"))?,
        SyncConflictResolutionSide::Local => &record.local_revision,
        SyncConflictResolutionSide::Remote => &record.remote_revision,
    };
    let selected = GitOid::parse(selected).map_err(AppError::operation)?;
    engine
        .tree_oid(repository, &selected)
        .map_err(AppError::operation)
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
    if resolution.side != Some(options.side)
        || resolution.proposal_id.is_some()
        || resolution.base_revision != record.base_revision.as_deref().unwrap_or_default()
        || resolution.local_revision != record.local_revision
        || resolution.remote_revision != record.remote_revision
        || resolution
            .live_input_revision
            .as_deref()
            .unwrap_or(&resolution.remote_revision)
            != conflict_live_input(record)?
    {
        return Err(AppError::operation(
            "prepared conflict resolution does not match the immutable conflict inputs",
        ));
    }
    let resolved = GitOid::parse(&resolution.resolution_commit).map_err(AppError::operation)?;
    let actual_tree = &capture.tree;
    let local_tree = conflict_worktree_tree(engine, repository, record)?;
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
        let base_objects = conflict
            .base
            .as_ref()
            .map(|revision| engine.path_objects(repository, revision, &conflict.paths))
            .transpose()
            .map_err(AppError::operation)?
            .unwrap_or_default();
        let local_objects = engine
            .path_objects(repository, &conflict.local, &conflict.paths)
            .map_err(AppError::operation)?;
        let remote_objects = engine
            .path_objects(repository, &conflict.remote, &conflict.paths)
            .map_err(AppError::operation)?;
        let classifications = conflict
            .classifications
            .iter()
            .map(|classification| (classification.path.as_str(), classification))
            .collect::<BTreeMap<_, _>>();
        let mut paths = Vec::with_capacity(conflict.paths.len());
        for (index, path) in conflict.paths.iter().enumerate() {
            paths.push(SyncConflictPathRecord {
                path: path.clone(),
                group_id: String::new(),
                group_kind: SyncConflictGroupKind::Path,
                classification: classifications
                    .get(path.as_str())
                    .map(|value| (*value).clone()),
                base: preserve_side(
                    &directory,
                    index,
                    "base",
                    conflict.base.as_ref(),
                    base_objects.get(path),
                )?,
                local: preserve_side(
                    &directory,
                    index,
                    "local",
                    Some(&conflict.local),
                    local_objects.get(path),
                )?,
                remote: preserve_side(
                    &directory,
                    index,
                    "remote",
                    Some(&conflict.remote),
                    remote_objects.get(path),
                )?,
            });
        }
        assign_conflict_groups(conflict.scope, &mut paths);
        let record = SyncConflictRecord {
            version: SYNC_CONFLICT_RECORD_VERSION,
            id: conflict.id.clone(),
            repository_key: repository_key.to_string(),
            work_tree,
            base_revision: conflict.base.as_ref().map(ToString::to_string),
            local_revision: conflict.local.to_string(),
            remote_revision: conflict.remote.to_string(),
            scope: conflict.scope,
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
            projection: conflict.projection.as_ref().map(|projection| {
                SyncConflictProjectionRecord {
                    tree: projection.tree.to_string(),
                    published: projection.published,
                    applied: projection.applied,
                }
            }),
            paths,
            diagnostics: conflict.diagnostics.clone(),
        };
        write_paged_record_noclobber(&directory, &record)?;
        self.prune_resolved_artifacts(repository_key)?;
        Ok(record)
    }

    /// Removes artifact copies of fully applied conflict resolutions beyond
    /// the newest retained sets. Records and resolution metadata are small
    /// and permanent; unresolved and in-progress resolutions are never
    /// pruned.
    pub fn prune_resolved_artifacts(&self, repository_key: &str) -> Result<usize, AppError> {
        validate_hex_id("repository key", repository_key)?;
        let root = self.root.join(repository_key).join("conflicts");
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(AppError::operation(error)),
        };
        let mut resolved = Vec::new();
        for entry in entries {
            let entry = entry.map_err(AppError::operation)?;
            if !entry.file_type().map_err(AppError::operation)?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            let Some(resolution) = self.get_resolution(repository_key, &id)? else {
                continue;
            };
            if !resolution.applied {
                continue;
            }
            let artifacts = entry.path().join("artifacts");
            if !artifacts.is_dir() {
                continue;
            }
            let modified = fs::metadata(entry.path().join("resolution.json"))
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            resolved.push((modified, artifacts));
        }
        resolved.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        let mut pruned = 0;
        for (_, artifacts) in resolved
            .into_iter()
            .skip(MAX_RETAINED_RESOLVED_ARTIFACT_SETS)
        {
            fs::remove_dir_all(&artifacts).map_err(AppError::operation)?;
            pruned += 1;
        }
        Ok(pruned)
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
        let source = fs::read(&path).map_err(AppError::operation)?;
        let value: serde_json::Value =
            serde_json::from_slice(&source).map_err(AppError::operation)?;
        let mut record = if value.get("path_pages").is_some() {
            if metadata.len() > MAX_CONFLICT_MANIFEST_BYTES {
                return Err(AppError::operation(format!(
                    "sync conflict manifest at {} exceeds the {} byte limit",
                    path.display(),
                    MAX_CONFLICT_MANIFEST_BYTES
                )));
            }
            load_paged_record(&path, value)?
        } else {
            serde_json::from_value(value).map_err(AppError::operation)?
        };
        assign_conflict_groups(record.scope, &mut record.paths);
        validate_record(&record, repository_key, conflict_id)?;
        Ok(record)
    }

    /// Loads only the requested path slice into the returned record. Paged
    /// records are scanned one bounded page at a time to validate their
    /// aggregate digest and compute global progress counters without ever
    /// retaining the complete path inventory in memory.
    pub fn get_page_and_progress(
        &self,
        repository_key: &str,
        conflict_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<(SyncConflictRecord, usize, SyncConflictProgress), AppError> {
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
        let source = fs::read(&path).map_err(AppError::operation)?;
        let value: serde_json::Value =
            serde_json::from_slice(&source).map_err(AppError::operation)?;
        if value.get("path_pages").is_none() {
            let mut record: SyncConflictRecord =
                serde_json::from_value(value).map_err(AppError::operation)?;
            assign_conflict_groups(record.scope, &mut record.paths);
            validate_record(&record, repository_key, conflict_id)?;
            let total = record.paths.len();
            let progress = self.group_progress(repository_key, &record)?;
            record.paths = record.paths.into_iter().skip(offset).take(limit).collect();
            let progress = progress_for_selected_paths(progress, &record.paths);
            return Ok((record, total, progress));
        }
        if metadata.len() > MAX_CONFLICT_MANIFEST_BYTES {
            return Err(AppError::operation(format!(
                "sync conflict manifest at {} exceeds the {} byte limit",
                path.display(),
                MAX_CONFLICT_MANIFEST_BYTES
            )));
        }
        let batches = self.list_batches(repository_key, conflict_id)?;
        load_paged_record_slice(&path, value, offset, limit, &batches).and_then(
            |(record, total, progress)| {
                validate_record(&record, repository_key, conflict_id)?;
                Ok((record, total, progress))
            },
        )
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
        if source.len() as u64 > MAX_CONFLICT_RESOLUTION_BYTES {
            return Err(AppError::operation(format!(
                "sync conflict resolution at {} exceeds the {} byte limit",
                path.display(),
                MAX_CONFLICT_RESOLUTION_BYTES
            )));
        }
        let mut resolution: SyncConflictResolutionRecord =
            serde_json::from_slice(&source).map_err(AppError::operation)?;
        if !(1..=SYNC_CONFLICT_RESOLUTION_VERSION).contains(&resolution.version)
            || resolution.conflict_id != conflict_id
        {
            return Err(AppError::operation(
                "sync conflict resolution version or identity mismatch",
            ));
        }
        // Resolution records are mutable crash-recovery state. Normalize a
        // supported legacy record in memory so a resumed publish/application
        // rewrites it with the current schema instead of failing the
        // current-version-only save guard.
        resolution.version = SYNC_CONFLICT_RESOLUTION_VERSION;
        Ok(Some(resolution))
    }

    /// Loads the durable resolution unless it is an abandoned attempt that
    /// never published and never applied. Guards must use this instead of
    /// `get_resolution` so a failed attempt cannot block rejection, side
    /// switches, or competing proposals.
    pub fn get_effective_resolution(
        &self,
        repository_key: &str,
        conflict_id: &str,
    ) -> Result<Option<SyncConflictResolutionRecord>, AppError> {
        Ok(self
            .get_resolution(repository_key, conflict_id)?
            .filter(|resolution| !resolution.is_abandoned()))
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

    pub fn list_batches(
        &self,
        repository_key: &str,
        conflict_id: &str,
    ) -> Result<Vec<SyncConflictBatchRecord>, AppError> {
        let directory = self
            .conflict_directory(repository_key, conflict_id)?
            .join("batches");
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(AppError::operation(error)),
        };
        let mut batches = Vec::new();
        for entry in entries {
            let entry = entry.map_err(AppError::operation)?;
            if !entry.file_type().map_err(AppError::operation)?.is_file() {
                continue;
            }
            let file_name = entry.file_name();
            let Some(batch_id) = file_name
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
            else {
                return Err(AppError::operation("invalid sync conflict batch filename"));
            };
            validate_hex_id("conflict batch ID", batch_id)?;
            let bytes = fs::read(entry.path()).map_err(AppError::operation)?;
            if bytes.len() as u64 > MAX_CONFLICT_RESOLUTION_BYTES {
                return Err(AppError::operation(format!(
                    "sync conflict batch `{batch_id}` exceeds the {MAX_CONFLICT_RESOLUTION_BYTES} byte limit"
                )));
            }
            let batch: SyncConflictBatchRecord =
                serde_json::from_slice(&bytes).map_err(AppError::operation)?;
            validate_batch_record(&batch, conflict_id, batch_id)?;
            batches.push(batch);
        }
        batches.sort_by(|left, right| left.batch_id.cmp(&right.batch_id));
        Ok(batches)
    }

    pub fn save_batch(
        &self,
        repository_key: &str,
        batch: &SyncConflictBatchRecord,
    ) -> Result<(), AppError> {
        validate_batch_record(batch, &batch.conflict_id, &batch.batch_id)?;
        let record = self.get(repository_key, &batch.conflict_id)?;
        let known = conflict_groups(&record)
            .into_iter()
            .map(|group| group.id)
            .collect::<BTreeSet<_>>();
        if batch.group_ids.iter().any(|id| !known.contains(id)) {
            return Err(AppError::operation(
                "sync conflict batch selects an unknown resolution group",
            ));
        }
        for existing in self.list_batches(repository_key, &batch.conflict_id)? {
            if existing.batch_id != batch.batch_id
                && !existing.needs_rebase
                && existing
                    .group_ids
                    .iter()
                    .any(|id| batch.group_ids.contains(id))
            {
                return Err(AppError::operation(format!(
                    "sync conflict batch overlaps active batch `{}`",
                    existing.batch_id
                )));
            }
        }
        let path = self
            .conflict_directory(repository_key, &batch.conflict_id)?
            .join("batches")
            .join(format!("{}.json", batch.batch_id));
        write_json_replace(&path, batch)
    }

    pub fn group_progress(
        &self,
        repository_key: &str,
        record: &SyncConflictRecord,
    ) -> Result<SyncConflictProgress, AppError> {
        let batches = self.list_batches(repository_key, &record.id)?;
        let mut assigned =
            BTreeMap::<String, (&SyncConflictBatchRecord, SyncConflictGroupState)>::new();
        for batch in &batches {
            let state = batch.state();
            for group_id in &batch.group_ids {
                let replace = assigned.get(group_id).is_none_or(|(_, previous)| {
                    group_state_priority(state) > group_state_priority(*previous)
                });
                if replace {
                    assigned.insert(group_id.clone(), (batch, state));
                }
            }
        }
        let groups = conflict_groups(record)
            .into_iter()
            .map(|group| {
                let assignment = assigned.get(&group.id);
                SyncConflictGroupProgress {
                    id: group.id,
                    kind: group.kind,
                    paths: group.paths,
                    state: assignment.map_or(SyncConflictGroupState::Pending, |(_, state)| *state),
                    batch_id: assignment.map(|(batch, _)| batch.batch_id.clone()),
                }
            })
            .collect::<Vec<_>>();
        Ok(summarize_group_progress(groups))
    }

    pub fn supersede_unresolved_except(
        &self,
        repository_key: &str,
        current_conflict_id: Option<&str>,
        current_revision: &str,
    ) -> Result<usize, AppError> {
        validate_hex_id("repository key", repository_key)?;
        if let Some(id) = current_conflict_id {
            validate_hex_id("conflict ID", id)?;
        }
        let replacement_paths = current_conflict_id
            .map(|id| self.get(repository_key, id))
            .transpose()?
            .map(|record| {
                record
                    .paths
                    .into_iter()
                    .map(|path| path.path)
                    .collect::<BTreeSet<_>>()
            });
        let mut superseded = 0;
        for record in self.list(repository_key)? {
            if current_conflict_id == Some(record.id.as_str()) {
                continue;
            }
            let progress = self.group_progress(repository_key, &record)?;
            if self.resolution_state_with_progress(repository_key, &record.id, &progress)?
                != SyncConflictResolutionState::Unresolved
            {
                continue;
            }
            let unfinished_paths = progress
                .groups
                .iter()
                .filter(|group| {
                    matches!(
                        group.state,
                        SyncConflictGroupState::Pending | SyncConflictGroupState::NeedsRebase
                    )
                })
                .flat_map(|group| group.paths.iter())
                .collect::<BTreeSet<_>>();
            let has_independent_obligation =
                replacement_paths
                    .as_ref()
                    .map_or(!unfinished_paths.is_empty(), |replacement| {
                        unfinished_paths
                            .iter()
                            .any(|path| !replacement.contains(path.as_str()))
                    });
            if has_independent_obligation
                || (current_conflict_id.is_none()
                    && conflict_live_input(&record)? == current_revision)
            {
                continue;
            }
            let supersession = SyncConflictSupersessionRecord {
                version: SYNC_CONFLICT_SUPERSESSION_VERSION,
                conflict_id: record.id.clone(),
                current_revision: current_revision.to_string(),
                replacement_conflict_id: current_conflict_id.map(str::to_string),
            };
            let path = self
                .conflict_directory(repository_key, &record.id)?
                .join("supersession.json");
            write_json_replace(&path, &supersession)?;
            superseded += 1;
        }
        Ok(superseded)
    }

    fn get_supersession(
        &self,
        repository_key: &str,
        conflict_id: &str,
    ) -> Result<Option<SyncConflictSupersessionRecord>, AppError> {
        let path = self
            .conflict_directory(repository_key, conflict_id)?
            .join("supersession.json");
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AppError::operation(error)),
        };
        let supersession: SyncConflictSupersessionRecord =
            serde_json::from_slice(&bytes).map_err(AppError::operation)?;
        if supersession.version != SYNC_CONFLICT_SUPERSESSION_VERSION
            || supersession.conflict_id != conflict_id
        {
            return Err(AppError::operation(
                "sync conflict supersession version or identity mismatch",
            ));
        }
        Ok(Some(supersession))
    }

    pub(crate) fn resolution_state(
        &self,
        repository_key: &str,
        conflict_id: &str,
    ) -> Result<SyncConflictResolutionState, AppError> {
        let record = self.get(repository_key, conflict_id)?;
        let progress = self.group_progress(repository_key, &record)?;
        self.resolution_state_with_progress(repository_key, conflict_id, &progress)
    }

    fn resolution_state_with_progress(
        &self,
        repository_key: &str,
        conflict_id: &str,
        progress: &SyncConflictProgress,
    ) -> Result<SyncConflictResolutionState, AppError> {
        if self
            .get_resolution(repository_key, conflict_id)?
            .is_some_and(|resolution| resolution.applied)
        {
            return Ok(SyncConflictResolutionState::Resolved);
        }
        if progress.total_groups > 0 && progress.applied_groups == progress.total_groups {
            return Ok(SyncConflictResolutionState::Resolved);
        }
        if self
            .get_supersession(repository_key, conflict_id)?
            .is_some()
        {
            return Ok(SyncConflictResolutionState::Superseded);
        }
        Ok(SyncConflictResolutionState::Unresolved)
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

fn assign_conflict_groups(scope: GitConflictScope, paths: &mut [SyncConflictPathRecord]) {
    if scope == GitConflictScope::TreeValidation {
        let group_id = conflict_group_id(SyncConflictGroupKind::WholeTree, &[]);
        for path in paths {
            path.group_id.clone_from(&group_id);
            path.group_kind = SyncConflictGroupKind::WholeTree;
        }
        return;
    }

    let mut structural_paths = paths
        .iter()
        .filter(|path| is_structural_conflict_path(path))
        .map(|path| path.path.as_str())
        .collect::<Vec<_>>();
    structural_paths.sort_unstable();
    let structural_id = (!structural_paths.is_empty())
        .then(|| conflict_group_id(SyncConflictGroupKind::Structural, &structural_paths));
    for path in paths {
        if is_structural_conflict_path(path) {
            path.group_id.clone_from(
                structural_id
                    .as_ref()
                    .expect("non-empty structural path set has an identity"),
            );
            path.group_kind = SyncConflictGroupKind::Structural;
        } else {
            path.group_id = conflict_group_id(SyncConflictGroupKind::Path, &[path.path.as_str()]);
            path.group_kind = SyncConflictGroupKind::Path;
        }
    }
}

fn is_structural_conflict_path(path: &SyncConflictPathRecord) -> bool {
    path.local.object_id.is_none() && path.remote.object_id.is_none()
        || path.classification.as_ref().is_some_and(|classification| {
            matches!(
                classification.class,
                vulcan_sync::GitConflictClass::RenameRename
                    | vulcan_sync::GitConflictClass::DirectoryFile
                    | vulcan_sync::GitConflictClass::CaseCollision
                    | vulcan_sync::GitConflictClass::Ambiguous
            )
        })
}

fn conflict_group_id(kind: SyncConflictGroupKind, paths: &[&str]) -> String {
    let mut input = format!("vulcan-conflict-group-v1\0{kind:?}").into_bytes();
    for path in paths {
        input.push(0);
        input.extend_from_slice(path.as_bytes());
    }
    blake3::hash(&input).to_hex()[..32].to_string()
}

fn validate_batch_record(
    batch: &SyncConflictBatchRecord,
    conflict_id: &str,
    batch_id: &str,
) -> Result<(), AppError> {
    validate_hex_id("conflict ID", conflict_id)?;
    validate_hex_id("conflict batch ID", batch_id)?;
    let mut canonical = batch.group_ids.clone();
    canonical.sort();
    canonical.dedup();
    let counted_groups = batch.group_path_counts.keys().cloned().collect::<Vec<_>>();
    if batch.version != SYNC_CONFLICT_BATCH_VERSION
        || batch.conflict_id != conflict_id
        || batch.batch_id != batch_id
        || canonical.is_empty()
        || canonical.len() > MAX_CONFLICT_GROUPS_PER_BATCH
        || canonical != batch.group_ids
        || (!batch.group_path_counts.is_empty()
            && (counted_groups != batch.group_ids
                || batch.group_path_counts.values().any(|count| *count == 0)))
        || batch.selection_digest != conflict_group_selection_digest(&canonical)
        || batch.side.is_some() == batch.proposal_id.is_some()
        || batch.applied && !batch.published
        || batch.needs_rebase && (batch.published || batch.applied)
    {
        return Err(AppError::operation(
            "sync conflict batch version, identity, selection, or state is invalid",
        ));
    }
    Ok(())
}

const fn group_state_priority(state: SyncConflictGroupState) -> u8 {
    match state {
        SyncConflictGroupState::Pending => 0,
        SyncConflictGroupState::NeedsRebase => 1,
        SyncConflictGroupState::Prepared => 2,
        SyncConflictGroupState::Published => 3,
        SyncConflictGroupState::Applied => 4,
    }
}

fn summarize_group_progress(groups: Vec<SyncConflictGroupProgress>) -> SyncConflictProgress {
    let mut progress = SyncConflictProgress {
        total_groups: groups.len(),
        pending_groups: 0,
        prepared_groups: 0,
        published_groups: 0,
        applied_groups: 0,
        needs_rebase_groups: 0,
        total_paths: 0,
        pending_paths: 0,
        returned_groups: groups.len(),
        groups_complete: true,
        groups,
    };
    for group in &progress.groups {
        progress.total_paths += group.paths.len();
        match group.state {
            SyncConflictGroupState::Pending => {
                progress.pending_groups += 1;
                progress.pending_paths += group.paths.len();
            }
            SyncConflictGroupState::Prepared => progress.prepared_groups += 1,
            SyncConflictGroupState::Published => progress.published_groups += 1,
            SyncConflictGroupState::Applied => progress.applied_groups += 1,
            SyncConflictGroupState::NeedsRebase => {
                progress.needs_rebase_groups += 1;
                progress.pending_paths += group.paths.len();
            }
        }
    }
    progress
}

fn preserve_side(
    conflict_directory: &Path,
    index: usize,
    side: &str,
    revision: Option<&GitOid>,
    object: Option<&vulcan_sync::GitPathObject>,
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
    let (artifact, content_hash, bytes) = if let Some(data) = object.data.as_deref() {
        let relative = PathBuf::from(format!("artifacts/{index:04}-{side}.bin"));
        let path = conflict_directory.join(&relative);
        write_bytes_noclobber(&path, data)?;
        (
            Some(relative),
            Some(blake3::hash(data).to_hex().to_string()),
            Some(data.len() as u64),
        )
    } else {
        (None, None, None)
    };
    Ok(SyncConflictSideRecord {
        revision: revision.to_string(),
        object_id: Some(object.oid.to_string()),
        mode: Some(object.mode.clone()),
        kind: Some(object.kind.clone()),
        artifact,
        content_hash,
        bytes,
    })
}

#[cfg(test)]
fn write_json_noclobber(path: &Path, value: &SyncConflictRecord) -> Result<(), AppError> {
    let bytes = serialize_conflict_record(value, MAX_CONFLICT_RECORD_BYTES)?;
    match durable_file::create(path, &bytes)? {
        DurableCreate::Created => Ok(()),
        DurableCreate::AlreadyExists => Err(AppError::operation(format!(
            "sync conflict record already exists at {}",
            path.display()
        ))),
    }
}

fn write_paged_record_noclobber(
    directory: &Path,
    record: &SyncConflictRecord,
) -> Result<(), AppError> {
    let pages_directory = directory.join("path-pages");
    fs::create_dir_all(&pages_directory).map_err(AppError::operation)?;
    let mut page_refs = Vec::new();
    for (index, paths) in record.paths.chunks(MAX_CONFLICT_PATHS_PER_PAGE).enumerate() {
        let page = SyncConflictPathPage {
            version: SYNC_CONFLICT_RECORD_VERSION,
            index,
            paths: paths.to_vec(),
        };
        let mut bytes = serde_json::to_vec_pretty(&page).map_err(AppError::operation)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_CONFLICT_PATH_PAGE_BYTES {
            return Err(AppError::operation(format!(
                "sync conflict path page {index} exceeds the {MAX_CONFLICT_PATH_PAGE_BYTES} byte limit"
            )));
        }
        let file = format!("paths-{index:06}.json");
        write_bytes_noclobber(&pages_directory.join(&file), &bytes)?;
        page_refs.push(SyncConflictPathPageRef {
            file,
            count: paths.len(),
            digest: blake3::hash(&bytes).to_hex().to_string(),
        });
    }

    let paths_bytes = serde_json::to_vec(&record.paths).map_err(AppError::operation)?;
    let mut manifest = serde_json::to_value(record).map_err(AppError::operation)?;
    let object = manifest
        .as_object_mut()
        .ok_or_else(|| AppError::operation("sync conflict record must serialize as an object"))?;
    object.remove("paths");
    object.insert(
        "path_count".to_string(),
        serde_json::json!(record.paths.len()),
    );
    object.insert(
        "group_count".to_string(),
        serde_json::json!(conflict_groups(record).len()),
    );
    object.insert(
        "paths_digest".to_string(),
        serde_json::json!(blake3::hash(&paths_bytes).to_hex().to_string()),
    );
    object.insert(
        "path_pages".to_string(),
        serde_json::to_value(page_refs).map_err(AppError::operation)?,
    );
    let mut bytes = serde_json::to_vec_pretty(&manifest).map_err(AppError::operation)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_CONFLICT_MANIFEST_BYTES {
        return Err(AppError::operation(format!(
            "sync conflict manifest exceeds the {MAX_CONFLICT_MANIFEST_BYTES} byte limit"
        )));
    }
    match durable_file::create(&directory.join("record.json"), &bytes)? {
        DurableCreate::Created => Ok(()),
        DurableCreate::AlreadyExists => Err(AppError::operation(format!(
            "sync conflict record already exists at {}",
            directory.join("record.json").display()
        ))),
    }
}

fn load_paged_record(
    manifest_path: &Path,
    mut value: serde_json::Value,
) -> Result<SyncConflictRecord, AppError> {
    let manifest: SyncConflictPathManifest =
        serde_json::from_value(value.clone()).map_err(AppError::operation)?;
    let directory = manifest_path
        .parent()
        .ok_or_else(|| AppError::operation("sync conflict manifest has no parent directory"))?;
    let mut paths = Vec::with_capacity(manifest.path_count);
    for (expected_index, page_ref) in manifest.path_pages.iter().enumerate() {
        let expected_file = format!("paths-{expected_index:06}.json");
        if page_ref.file != expected_file || page_ref.count > MAX_CONFLICT_PATHS_PER_PAGE {
            return Err(AppError::operation(format!(
                "sync conflict manifest contains an invalid path-page reference `{}`",
                page_ref.file
            )));
        }
        let page_path = directory.join("path-pages").join(&page_ref.file);
        let metadata = fs::metadata(&page_path).map_err(AppError::operation)?;
        if !metadata.is_file() || metadata.len() > MAX_CONFLICT_PATH_PAGE_BYTES {
            return Err(AppError::operation(format!(
                "sync conflict path page at {} is not a bounded regular file",
                page_path.display()
            )));
        }
        let bytes = fs::read(&page_path).map_err(AppError::operation)?;
        if blake3::hash(&bytes).to_hex().as_str() != page_ref.digest {
            return Err(AppError::operation(format!(
                "sync conflict path page at {} does not match its manifest digest",
                page_path.display()
            )));
        }
        let page: SyncConflictPathPage =
            serde_json::from_slice(&bytes).map_err(AppError::operation)?;
        if !(3..=SYNC_CONFLICT_RECORD_VERSION).contains(&manifest.version)
            || page.version != manifest.version
            || page.index != expected_index
            || page.paths.len() != page_ref.count
        {
            return Err(AppError::operation(format!(
                "sync conflict path page at {} has invalid identity or count",
                page_path.display()
            )));
        }
        paths.extend(page.paths);
    }
    if paths.len() != manifest.path_count {
        return Err(AppError::operation(
            "sync conflict path pages do not match the manifest count",
        ));
    }
    let paths_bytes = serde_json::to_vec(&paths).map_err(AppError::operation)?;
    if blake3::hash(&paths_bytes).to_hex().as_str() != manifest.paths_digest {
        return Err(AppError::operation(
            "sync conflict path pages do not match the manifest digest",
        ));
    }
    let object = value
        .as_object_mut()
        .ok_or_else(|| AppError::operation("sync conflict manifest must be an object"))?;
    object.remove("path_count");
    object.remove("group_count");
    object.remove("paths_digest");
    object.remove("path_pages");
    object.insert(
        "paths".to_string(),
        serde_json::to_value(paths).map_err(AppError::operation)?,
    );
    serde_json::from_value(value).map_err(AppError::operation)
}

#[allow(clippy::too_many_lines)]
fn load_paged_record_slice(
    manifest_path: &Path,
    mut value: serde_json::Value,
    offset: usize,
    limit: usize,
    batches: &[SyncConflictBatchRecord],
) -> Result<(SyncConflictRecord, usize, SyncConflictProgress), AppError> {
    let manifest: SyncConflictPathManifest =
        serde_json::from_value(value.clone()).map_err(AppError::operation)?;
    if !(3..=SYNC_CONFLICT_RECORD_VERSION).contains(&manifest.version) {
        return Err(AppError::operation(
            "sync conflict path manifest has an unsupported version",
        ));
    }
    let referenced_count = manifest
        .path_pages
        .iter()
        .try_fold(0usize, |total, page| total.checked_add(page.count))
        .ok_or_else(|| AppError::operation("sync conflict path-page count overflow"))?;
    if referenced_count != manifest.path_count {
        return Err(AppError::operation(
            "sync conflict path pages do not match the manifest count",
        ));
    }
    let directory = manifest_path
        .parent()
        .ok_or_else(|| AppError::operation("sync conflict manifest has no parent directory"))?;
    if let Some(group_count) = manifest.group_count {
        if batches.iter().all(|batch| {
            !batch.group_path_counts.is_empty()
                && batch.group_path_counts.len() == batch.group_ids.len()
        }) {
            return load_indexed_paged_record_slice(
                value,
                &manifest,
                directory,
                offset,
                limit,
                group_count,
                batches,
            );
        }
    }
    let assignments = group_batch_assignments(batches);
    let mut selected_paths = Vec::with_capacity(limit.min(manifest.path_count));
    let mut selected_groups = BTreeMap::<String, SyncConflictGroupProgress>::new();
    let mut all_groups = BTreeMap::<
        String,
        (
            SyncConflictGroupKind,
            usize,
            SyncConflictGroupState,
            Option<String>,
        ),
    >::new();
    let mut paths_hasher = blake3::Hasher::new();
    paths_hasher.update(b"[");
    let mut absolute_index = 0usize;
    for (expected_index, page_ref) in manifest.path_pages.iter().enumerate() {
        let page = read_conflict_path_page(directory, &manifest, page_ref, expected_index)?;
        for path in page.paths {
            if absolute_index > 0 {
                paths_hasher.update(b",");
            }
            paths_hasher.update(&serde_json::to_vec(&path).map_err(AppError::operation)?);
            let (state, batch_id) = assignments
                .get(&path.group_id)
                .cloned()
                .unwrap_or((SyncConflictGroupState::Pending, None));
            let aggregate = all_groups.entry(path.group_id.clone()).or_insert((
                path.group_kind,
                0,
                state,
                batch_id.clone(),
            ));
            if aggregate.0 != path.group_kind || aggregate.2 != state {
                return Err(AppError::operation(
                    "sync conflict group metadata is inconsistent across path pages",
                ));
            }
            aggregate.1 += 1;
            if absolute_index >= offset && absolute_index < offset.saturating_add(limit) {
                selected_groups
                    .entry(path.group_id.clone())
                    .or_insert_with(|| SyncConflictGroupProgress {
                        id: path.group_id.clone(),
                        kind: path.group_kind,
                        paths: Vec::new(),
                        state,
                        batch_id: batch_id.clone(),
                    })
                    .paths
                    .push(path.path.clone());
                selected_paths.push(path);
            }
            absolute_index += 1;
        }
    }
    paths_hasher.update(b"]");
    if absolute_index != manifest.path_count
        || paths_hasher.finalize().to_hex().as_str() != manifest.paths_digest
    {
        return Err(AppError::operation(
            "sync conflict path pages do not match the manifest count or digest",
        ));
    }

    let mut progress = SyncConflictProgress {
        total_groups: all_groups.len(),
        pending_groups: 0,
        prepared_groups: 0,
        published_groups: 0,
        applied_groups: 0,
        needs_rebase_groups: 0,
        total_paths: manifest.path_count,
        pending_paths: 0,
        returned_groups: 0,
        groups_complete: false,
        groups: selected_groups.into_values().collect(),
    };
    for (_, path_count, state, _) in all_groups.values() {
        match state {
            SyncConflictGroupState::Pending => {
                progress.pending_groups += 1;
                progress.pending_paths += path_count;
            }
            SyncConflictGroupState::Prepared => progress.prepared_groups += 1,
            SyncConflictGroupState::Published => progress.published_groups += 1,
            SyncConflictGroupState::Applied => progress.applied_groups += 1,
            SyncConflictGroupState::NeedsRebase => {
                progress.needs_rebase_groups += 1;
                progress.pending_paths += path_count;
            }
        }
    }
    progress.returned_groups = progress.groups.len();
    progress.groups_complete = progress.returned_groups == progress.total_groups;

    let object = value
        .as_object_mut()
        .ok_or_else(|| AppError::operation("sync conflict manifest must be an object"))?;
    object.remove("path_count");
    object.remove("group_count");
    object.remove("paths_digest");
    object.remove("path_pages");
    object.insert(
        "paths".to_string(),
        serde_json::to_value(selected_paths).map_err(AppError::operation)?,
    );
    let record = serde_json::from_value(value).map_err(AppError::operation)?;
    Ok((record, manifest.path_count, progress))
}

#[allow(clippy::too_many_lines)]
fn load_indexed_paged_record_slice(
    mut value: serde_json::Value,
    manifest: &SyncConflictPathManifest,
    directory: &Path,
    offset: usize,
    limit: usize,
    group_count: usize,
    batches: &[SyncConflictBatchRecord],
) -> Result<(SyncConflictRecord, usize, SyncConflictProgress), AppError> {
    let assignments = group_batch_assignments_with_counts(batches);
    if assignments.len() > group_count {
        return Err(AppError::operation(
            "sync conflict batch progress exceeds the manifest group count",
        ));
    }
    let mut progress = SyncConflictProgress {
        total_groups: group_count,
        pending_groups: group_count - assignments.len(),
        prepared_groups: 0,
        published_groups: 0,
        applied_groups: 0,
        needs_rebase_groups: 0,
        total_paths: manifest.path_count,
        pending_paths: manifest.path_count,
        returned_groups: 0,
        groups_complete: false,
        groups: Vec::new(),
    };
    for (state, path_count, _) in assignments.values() {
        if *path_count > manifest.path_count {
            return Err(AppError::operation(
                "sync conflict batch path count exceeds the manifest path count",
            ));
        }
        match state {
            SyncConflictGroupState::Pending => progress.pending_groups += 1,
            SyncConflictGroupState::Prepared => {
                progress.prepared_groups += 1;
                progress.pending_paths = progress
                    .pending_paths
                    .checked_sub(*path_count)
                    .ok_or_else(|| AppError::operation("invalid conflict batch path counts"))?;
            }
            SyncConflictGroupState::Published => {
                progress.published_groups += 1;
                progress.pending_paths = progress
                    .pending_paths
                    .checked_sub(*path_count)
                    .ok_or_else(|| AppError::operation("invalid conflict batch path counts"))?;
            }
            SyncConflictGroupState::Applied => {
                progress.applied_groups += 1;
                progress.pending_paths = progress
                    .pending_paths
                    .checked_sub(*path_count)
                    .ok_or_else(|| AppError::operation("invalid conflict batch path counts"))?;
            }
            SyncConflictGroupState::NeedsRebase => progress.needs_rebase_groups += 1,
        }
    }

    let end = offset.saturating_add(limit).min(manifest.path_count);
    let mut selected_paths = Vec::with_capacity(end.saturating_sub(offset));
    let mut selected_groups = BTreeMap::<String, SyncConflictGroupProgress>::new();
    let mut page_start = 0usize;
    for (expected_index, page_ref) in manifest.path_pages.iter().enumerate() {
        let page_end = page_start
            .checked_add(page_ref.count)
            .ok_or_else(|| AppError::operation("sync conflict path-page count overflow"))?;
        let expected_file = format!("paths-{expected_index:06}.json");
        if page_ref.file != expected_file || page_ref.count > MAX_CONFLICT_PATHS_PER_PAGE {
            return Err(AppError::operation(format!(
                "sync conflict manifest contains an invalid path-page reference `{}`",
                page_ref.file
            )));
        }
        if page_end > offset && page_start < end {
            let page = read_conflict_path_page(directory, manifest, page_ref, expected_index)?;
            let take_start = offset.saturating_sub(page_start);
            let take_end = end.min(page_end) - page_start;
            for path in page
                .paths
                .into_iter()
                .skip(take_start)
                .take(take_end.saturating_sub(take_start))
            {
                let (state, _, batch_id) = assignments.get(&path.group_id).cloned().unwrap_or((
                    SyncConflictGroupState::Pending,
                    0,
                    None,
                ));
                selected_groups
                    .entry(path.group_id.clone())
                    .or_insert_with(|| SyncConflictGroupProgress {
                        id: path.group_id.clone(),
                        kind: path.group_kind,
                        paths: Vec::new(),
                        state,
                        batch_id,
                    })
                    .paths
                    .push(path.path.clone());
                selected_paths.push(path);
            }
        }
        page_start = page_end;
    }
    if page_start != manifest.path_count || selected_paths.len() != end.saturating_sub(offset) {
        return Err(AppError::operation(
            "sync conflict path pages do not match the manifest count",
        ));
    }
    progress.groups = selected_groups.into_values().collect();
    progress.returned_groups = progress.groups.len();
    progress.groups_complete = progress.returned_groups == progress.total_groups;

    let object = value
        .as_object_mut()
        .ok_or_else(|| AppError::operation("sync conflict manifest must be an object"))?;
    object.remove("path_count");
    object.remove("group_count");
    object.remove("paths_digest");
    object.remove("path_pages");
    object.insert(
        "paths".to_string(),
        serde_json::to_value(selected_paths).map_err(AppError::operation)?,
    );
    let record = serde_json::from_value(value).map_err(AppError::operation)?;
    Ok((record, manifest.path_count, progress))
}

fn read_conflict_path_page(
    directory: &Path,
    manifest: &SyncConflictPathManifest,
    page_ref: &SyncConflictPathPageRef,
    expected_index: usize,
) -> Result<SyncConflictPathPage, AppError> {
    let expected_file = format!("paths-{expected_index:06}.json");
    if page_ref.file != expected_file || page_ref.count > MAX_CONFLICT_PATHS_PER_PAGE {
        return Err(AppError::operation(format!(
            "sync conflict manifest contains an invalid path-page reference `{}`",
            page_ref.file
        )));
    }
    let page_path = directory.join("path-pages").join(&page_ref.file);
    let metadata = fs::metadata(&page_path).map_err(AppError::operation)?;
    if !metadata.is_file() || metadata.len() > MAX_CONFLICT_PATH_PAGE_BYTES {
        return Err(AppError::operation(format!(
            "sync conflict path page at {} is not a bounded regular file",
            page_path.display()
        )));
    }
    let bytes = fs::read(&page_path).map_err(AppError::operation)?;
    if blake3::hash(&bytes).to_hex().as_str() != page_ref.digest {
        return Err(AppError::operation(format!(
            "sync conflict path page at {} does not match its manifest digest",
            page_path.display()
        )));
    }
    let page: SyncConflictPathPage = serde_json::from_slice(&bytes).map_err(AppError::operation)?;
    if page.version != manifest.version
        || page.index != expected_index
        || page.paths.len() != page_ref.count
    {
        return Err(AppError::operation(format!(
            "sync conflict path page at {} has invalid identity or count",
            page_path.display()
        )));
    }
    Ok(page)
}

fn group_batch_assignments(
    batches: &[SyncConflictBatchRecord],
) -> BTreeMap<String, (SyncConflictGroupState, Option<String>)> {
    let mut assignments = BTreeMap::new();
    for batch in batches {
        let state = batch.state();
        for group_id in &batch.group_ids {
            let replace = assignments.get(group_id).is_none_or(|(previous, _)| {
                group_state_priority(state) > group_state_priority(*previous)
            });
            if replace {
                assignments.insert(group_id.clone(), (state, Some(batch.batch_id.clone())));
            }
        }
    }
    assignments
}

fn group_batch_assignments_with_counts(
    batches: &[SyncConflictBatchRecord],
) -> BTreeMap<String, (SyncConflictGroupState, usize, Option<String>)> {
    let mut assignments = BTreeMap::new();
    for batch in batches {
        let state = batch.state();
        for group_id in &batch.group_ids {
            let replace = assignments.get(group_id).is_none_or(
                |(previous, _, _): &(SyncConflictGroupState, usize, Option<String>)| {
                    group_state_priority(state) > group_state_priority(*previous)
                },
            );
            if replace {
                assignments.insert(
                    group_id.clone(),
                    (
                        state,
                        batch.group_path_counts.get(group_id).copied().unwrap_or(0),
                        Some(batch.batch_id.clone()),
                    ),
                );
            }
        }
    }
    assignments
}

fn progress_for_selected_paths(
    mut progress: SyncConflictProgress,
    paths: &[SyncConflictPathRecord],
) -> SyncConflictProgress {
    let selected = paths
        .iter()
        .map(|path| path.group_id.as_str())
        .collect::<BTreeSet<_>>();
    progress
        .groups
        .retain(|group| selected.contains(group.id.as_str()));
    progress.returned_groups = progress.groups.len();
    progress.groups_complete = progress.returned_groups == progress.total_groups;
    progress
}

#[cfg(test)]
fn serialize_conflict_record(
    value: &SyncConflictRecord,
    maximum_bytes: u64,
) -> Result<Vec<u8>, AppError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(AppError::operation)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > maximum_bytes {
        return Err(AppError::operation(format!(
            "sync conflict record exceeds the {maximum_bytes} byte limit"
        )));
    }
    Ok(bytes)
}

fn write_json_replace<T: Serialize>(path: &Path, value: &T) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::operation("conflict resolution has no parent directory"))?;
    fs::create_dir_all(parent).map_err(AppError::operation)?;
    let mut bytes = serde_json::to_vec_pretty(value).map_err(AppError::operation)?;
    bytes.push(b'\n');
    durable_file::replace(path, &bytes)
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
    match durable_file::create(path, bytes)? {
        DurableCreate::Created => Ok(()),
        DurableCreate::AlreadyExists => {
            let existing = fs::read(path).map_err(AppError::operation)?;
            if existing == bytes {
                Ok(())
            } else {
                Err(AppError::operation(format!(
                    "immutable conflict artifact differs at {}",
                    path.display()
                )))
            }
        }
    }
}

fn validate_record(
    record: &SyncConflictRecord,
    repository_key: &str,
    conflict_id: &str,
) -> Result<(), AppError> {
    if !(1..=SYNC_CONFLICT_RECORD_VERSION).contains(&record.version)
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
        || effective_conflict_scope(record) != conflict.scope
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
        || !projection_matches(record.projection.as_ref(), conflict)
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

fn projection_matches(
    record: Option<&SyncConflictProjectionRecord>,
    conflict: &GitSyncConflict,
) -> bool {
    match (record, conflict.projection.as_ref()) {
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
        (Some(record), Some(projection)) => {
            record.tree == projection.tree.as_str()
                && record.published == projection.published
                && record.applied == projection.applied
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn absent_side(revision: &str) -> SyncConflictSideRecord {
        SyncConflictSideRecord {
            revision: revision.to_string(),
            object_id: None,
            mode: None,
            kind: None,
            artifact: None,
            content_hash: None,
            bytes: None,
        }
    }

    fn unresolved_record(id: &str, key: &str, work_tree: &Path) -> SyncConflictRecord {
        let mut record = SyncConflictRecord {
            version: SYNC_CONFLICT_RECORD_VERSION,
            id: id.to_string(),
            repository_key: key.to_string(),
            work_tree: work_tree.to_path_buf(),
            base_revision: Some("base".to_string()),
            local_revision: "local".to_string(),
            remote_revision: "remote".to_string(),
            scope: GitConflictScope::Paths,
            policy_version: 1,
            policy_hash: "policy".to_string(),
            preserved_base_ref: None,
            preserved_local_ref: "refs/local".to_string(),
            preserved_remote_ref: "refs/remote".to_string(),
            preserved_record_ref: None,
            provenance_revision: None,
            projection: None,
            paths: vec![SyncConflictPathRecord {
                path: "Home.md".to_string(),
                group_id: String::new(),
                group_kind: SyncConflictGroupKind::Path,
                classification: None,
                base: absent_side("base"),
                local: absent_side("local"),
                remote: absent_side("remote"),
            }],
            diagnostics: "conflict".to_string(),
        };
        assign_conflict_groups(record.scope, &mut record.paths);
        record
    }

    #[test]
    fn later_conflict_supersedes_unresolved_history_but_keeps_its_record() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let old_id = "b".repeat(32);
        let current_id = "c".repeat(32);
        let store = SyncConflictStore::at(temporary.path().join("state"));
        for id in [&old_id, &current_id] {
            let directory = store.conflict_directory(&key, id).expect("directory");
            fs::create_dir_all(&directory).expect("conflict directory");
            write_json_noclobber(
                &directory.join("record.json"),
                &unresolved_record(id, &key, temporary.path()),
            )
            .expect("record");
        }

        assert_eq!(
            store
                .supersede_unresolved_except(&key, Some(&current_id), "revision")
                .expect("supersede"),
            1
        );
        assert_eq!(
            store.resolution_state(&key, &old_id).expect("old state"),
            SyncConflictResolutionState::Superseded
        );
        assert_eq!(
            store
                .resolution_state(&key, &current_id)
                .expect("current state"),
            SyncConflictResolutionState::Unresolved
        );
        let supersession = store
            .get_supersession(&key, &old_id)
            .expect("supersession")
            .expect("superseded record");
        assert_eq!(supersession.current_revision, "revision");
        assert_eq!(
            supersession.replacement_conflict_id.as_deref(),
            Some(current_id.as_str())
        );
        assert_eq!(
            store.get(&key, &old_id).expect("immutable old record").id,
            old_id
        );
    }

    #[test]
    fn successful_sync_keeps_grouped_conflict_sessions_actionable() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let id = "b".repeat(32);
        let store = SyncConflictStore::at(temporary.path().join("state"));
        let directory = store.conflict_directory(&key, &id).expect("directory");
        fs::create_dir_all(&directory).expect("conflict directory");
        write_json_noclobber(
            &directory.join("record.json"),
            &unresolved_record(&id, &key, temporary.path()),
        )
        .expect("record");

        assert_eq!(
            store
                .supersede_unresolved_except(&key, None, "remote")
                .expect("same live input"),
            0
        );
        assert_eq!(
            store.resolution_state(&key, &id).expect("state"),
            SyncConflictResolutionState::Unresolved
        );
        assert_eq!(
            store
                .supersede_unresolved_except(&key, None, "later")
                .expect("later live input"),
            0
        );
        assert_eq!(
            store.resolution_state(&key, &id).expect("state"),
            SyncConflictResolutionState::Unresolved
        );

        let legacy_id = "c".repeat(32);
        let legacy_directory = store
            .conflict_directory(&key, &legacy_id)
            .expect("legacy directory");
        fs::create_dir_all(&legacy_directory).expect("legacy directory");
        let mut legacy = unresolved_record(&legacy_id, &key, temporary.path());
        legacy.version = 3;
        write_json_noclobber(&legacy_directory.join("record.json"), &legacy)
            .expect("legacy record");
        assert_eq!(
            store
                .supersede_unresolved_except(&key, None, "later")
                .expect("legacy later live input"),
            0
        );
        assert_eq!(
            store
                .resolution_state(&key, &legacy_id)
                .expect("legacy state"),
            SyncConflictResolutionState::Unresolved
        );
    }

    #[test]
    fn unrelated_replacement_conflict_keeps_pending_groups_actionable() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let old_id = "b".repeat(32);
        let current_id = "c".repeat(32);
        let store = SyncConflictStore::at(temporary.path().join("state"));
        let old_directory = store
            .conflict_directory(&key, &old_id)
            .expect("old directory");
        fs::create_dir_all(&old_directory).expect("old directory");
        write_json_noclobber(
            &old_directory.join("record.json"),
            &unresolved_record(&old_id, &key, temporary.path()),
        )
        .expect("old record");
        let current_directory = store
            .conflict_directory(&key, &current_id)
            .expect("current directory");
        fs::create_dir_all(&current_directory).expect("current directory");
        let mut current = unresolved_record(&current_id, &key, temporary.path());
        current.paths[0].path = "Other.md".to_string();
        assign_conflict_groups(current.scope, &mut current.paths);
        write_json_noclobber(&current_directory.join("record.json"), &current)
            .expect("current record");

        assert_eq!(
            store
                .supersede_unresolved_except(&key, Some(&current_id), "revision")
                .expect("reconcile sessions"),
            0
        );
        assert_eq!(
            store.resolution_state(&key, &old_id).expect("old state"),
            SyncConflictResolutionState::Unresolved
        );
        assert_eq!(
            store
                .resolution_state(&key, &current_id)
                .expect("current state"),
            SyncConflictResolutionState::Unresolved
        );
    }

    #[test]
    fn synthesized_structural_paths_reject_misleading_side_resolution() {
        let record = SyncConflictRecord {
            version: SYNC_CONFLICT_RECORD_VERSION,
            id: "a".repeat(32),
            repository_key: "b".repeat(32),
            work_tree: PathBuf::from("/vault"),
            base_revision: Some("base".to_string()),
            local_revision: "local".to_string(),
            remote_revision: "remote".to_string(),
            scope: GitConflictScope::Paths,
            policy_version: 1,
            policy_hash: "policy".to_string(),
            preserved_base_ref: None,
            preserved_local_ref: "refs/local".to_string(),
            preserved_remote_ref: "refs/remote".to_string(),
            preserved_record_ref: None,
            provenance_revision: None,
            projection: None,
            paths: vec![SyncConflictPathRecord {
                path: "New/remote.md".to_string(),
                group_id: String::new(),
                group_kind: SyncConflictGroupKind::Path,
                classification: None,
                base: absent_side("base"),
                local: absent_side("local"),
                remote: absent_side("remote"),
            }],
            diagnostics: "CONFLICT (file location)".to_string(),
        };

        let error = reject_synthesized_path_side_resolution(&record)
            .expect_err("synthesized location must fail closed");
        assert!(error.to_string().contains("synthesized by Git"));
        assert!(error.to_string().contains("New/remote.md"));
    }

    fn resolved_conflict(store: &SyncConflictStore, key: &str, id: &str, age_seconds: u64) {
        let directory = store.conflict_directory(key, id).expect("directory");
        fs::create_dir_all(directory.join("artifacts")).expect("artifacts");
        fs::write(directory.join("record.json"), b"{}").expect("record");
        let resolution = SyncConflictResolutionRecord {
            version: SYNC_CONFLICT_RESOLUTION_VERSION,
            conflict_id: id.to_string(),
            side: Some(SyncConflictResolutionSide::Local),
            proposal_id: None,
            base_revision: "base".to_string(),
            local_revision: "local".to_string(),
            remote_revision: "remote".to_string(),
            live_input_revision: None,
            recovery_revision: "recovery".to_string(),
            resolved_tree: "tree".to_string(),
            resolution_commit: "commit".to_string(),
            published: true,
            applied: true,
        };
        let resolution_path = directory.join("resolution.json");
        fs::write(
            &resolution_path,
            serde_json::to_vec(&resolution).expect("resolution"),
        )
        .expect("resolution file");
        fs::write(directory.join("artifacts/0000-local.bin"), b"bytes").expect("artifact");
        fs::OpenOptions::new()
            .write(true)
            .open(&resolution_path)
            .expect("resolution handle")
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(age_seconds))
            .expect("resolution mtime");
    }

    #[test]
    fn resolved_conflict_artifacts_are_pruned_beyond_the_retention_bound() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let store = SyncConflictStore::at(temporary.path().to_path_buf());
        let total = MAX_RETAINED_RESOLVED_ARTIFACT_SETS + 4;
        for index in 0..total {
            // Distinct ascending hex IDs: 00...00, 00...01, ...
            let id = format!("{index:032x}");
            resolved_conflict(&store, &key, &id, index as u64);
        }
        // An unresolved conflict must keep its artifacts forever.
        let unresolved = "f".repeat(32);
        let directory = store
            .conflict_directory(&key, &unresolved)
            .expect("directory");
        fs::create_dir_all(directory.join("artifacts")).expect("artifacts");
        fs::write(directory.join("artifacts/0000-local.bin"), b"bytes").expect("artifact");

        let pruned = store.prune_resolved_artifacts(&key).expect("prune");

        assert_eq!(pruned, 4);
        for index in 0..total {
            let id = format!("{index:032x}");
            let artifacts = store
                .conflict_directory(&key, &id)
                .expect("directory")
                .join("artifacts");
            if index < 4 {
                assert!(!artifacts.exists(), "oldest {id} should be pruned");
            } else {
                assert!(artifacts.exists(), "newest {id} should be retained");
            }
        }
        assert!(directory.join("artifacts").exists());
        // Re-running is a no-op.
        assert_eq!(store.prune_resolved_artifacts(&key).expect("re-prune"), 0);
    }

    fn write_version_one_record(
        store: &SyncConflictStore,
        key: &str,
        id: &str,
        work_tree: &Path,
    ) -> PathBuf {
        let directory = store.conflict_directory(key, id).expect("directory");
        fs::create_dir_all(&directory).expect("conflict directory");
        let record = serde_json::json!({
            "version": 1,
            "id": id,
            "repository_key": key,
            "work_tree": work_tree,
            "base_revision": "base",
            "local_revision": "local",
            "remote_revision": "remote",
            "scope": "paths",
            "policy_version": 1,
            "policy_hash": "policy",
            "preserved_base_ref": null,
            "preserved_local_ref": "refs/local",
            "preserved_remote_ref": "refs/remote",
            "provenance_revision": "provenance",
            "materialization": {
                "directory": format!(".sync-conflicts/{id}"),
                "tree": "tree",
                "copies": [{
                    "original_path": "Home.md",
                    "copy_path": format!(".sync-conflicts/{id}/local/Home.md"),
                    "object_id": "object",
                    "mode": "100644"
                }],
                "published": true,
                "applied": true
            },
            "paths": [{
                "path": "Home.md",
                "base": {"revision": "base"},
                "local": {"revision": "local"},
                "remote": {"revision": "remote"}
            }],
            "diagnostics": "conflict"
        });
        let record_path = directory.join("record.json");
        fs::write(
            &record_path,
            serde_json::to_vec_pretty(&record).expect("record"),
        )
        .expect("record file");
        record_path
    }

    #[test]
    fn version_one_records_migrate_their_materialization_into_projection() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let id = "b".repeat(32);
        let store = SyncConflictStore::at(temporary.path().join("state"));
        write_version_one_record(&store, &key, &id, temporary.path());

        let record = store.get(&key, &id).expect("legacy record loads");
        assert_eq!(record.version, 1);
        let projection = record.projection.as_ref().expect("projection migrated");
        assert_eq!(projection.tree, "tree");
        assert!(projection.published);
        assert!(projection.applied);
        assert_eq!(
            store
                .list(&key)
                .expect("list legacy records")
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>(),
            vec![id.as_str()]
        );
    }

    #[test]
    fn version_one_resolution_records_migrate_and_remain_writable() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let id = "b".repeat(32);
        let store = SyncConflictStore::at(temporary.path().join("state"));
        write_version_one_record(&store, &key, &id, temporary.path());
        let resolution = serde_json::json!({
            "version": 1,
            "conflict_id": id,
            "base_revision": "base",
            "local_revision": "local",
            "remote_revision": "remote",
            "recovery_revision": "recovery",
            "resolved_tree": "tree",
            "resolution_commit": "commit",
            "published": true,
            "applied": true
        });
        fs::write(
            store
                .conflict_directory(&key, &id)
                .expect("directory")
                .join("resolution.json"),
            serde_json::to_vec_pretty(&resolution).expect("resolution"),
        )
        .expect("resolution file");

        let loaded = store
            .get_resolution(&key, &id)
            .expect("legacy resolution loads")
            .expect("resolution present");
        assert_eq!(loaded.version, SYNC_CONFLICT_RESOLUTION_VERSION);
        assert_eq!(
            store.resolution_state(&key, &id).expect("state"),
            SyncConflictResolutionState::Resolved
        );
        store
            .save_resolution(&key, &loaded)
            .expect("migrated resolution remains writable");
        let persisted: serde_json::Value = serde_json::from_slice(
            &fs::read(
                store
                    .conflict_directory(&key, &id)
                    .expect("directory")
                    .join("resolution.json"),
            )
            .expect("migrated resolution source"),
        )
        .expect("migrated resolution JSON");
        assert_eq!(
            persisted["version"],
            serde_json::json!(SYNC_CONFLICT_RESOLUTION_VERSION)
        );
    }

    #[test]
    fn unsupported_future_record_versions_still_fail_closed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let id = "b".repeat(32);
        let store = SyncConflictStore::at(temporary.path().join("state"));
        let record_path = write_version_one_record(&store, &key, &id, temporary.path());
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&record_path).expect("record source"))
                .expect("record JSON");
        value["version"] = serde_json::json!(SYNC_CONFLICT_RECORD_VERSION + 1);
        fs::write(
            &record_path,
            serde_json::to_vec_pretty(&value).expect("record"),
        )
        .expect("tampered record");

        let error = store.get(&key, &id).expect_err("future version must fail");
        assert!(error.to_string().contains("version or identity mismatch"));
    }

    #[test]
    fn records_above_the_former_one_mebibyte_limit_remain_readable() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let id = "b".repeat(32);
        let store = SyncConflictStore::at(temporary.path().join("state"));
        let directory = store.conflict_directory(&key, &id).expect("directory");
        fs::create_dir_all(&directory).expect("conflict directory");
        let mut record = unresolved_record(&id, &key, temporary.path());
        record.diagnostics = "x".repeat(1024 * 1024);
        write_json_noclobber(&directory.join("record.json"), &record)
            .expect("large legacy-compatible record");

        let loaded = store.get(&key, &id).expect("large record remains readable");
        assert_eq!(loaded.diagnostics.len(), 1024 * 1024);
    }

    #[test]
    fn writer_enforces_the_same_bound_as_the_reader() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let record = unresolved_record(&"b".repeat(32), &"a".repeat(32), temporary.path());
        let serialized = serde_json::to_vec_pretty(&record).expect("record");

        let error = serialize_conflict_record(&record, serialized.len() as u64)
            .expect_err("newline must make the record exceed the bound");
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn paged_conflict_records_round_trip_large_path_sets() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let id = "b".repeat(32);
        let store = SyncConflictStore::at(temporary.path().join("state"));
        let directory = store.conflict_directory(&key, &id).expect("directory");
        fs::create_dir_all(&directory).expect("conflict directory");
        let mut record = unresolved_record(&id, &key, temporary.path());
        let prototype = record.paths[0].clone();
        record.paths = (0..10_000)
            .map(|index| SyncConflictPathRecord {
                path: format!("Notes/{index:05}.md"),
                ..prototype.clone()
            })
            .collect();
        assign_conflict_groups(record.scope, &mut record.paths);

        write_paged_record_noclobber(&directory, &record).expect("paged record");

        let manifest = fs::read(directory.join("record.json")).expect("manifest");
        assert!(
            u64::try_from(manifest.len()).expect("manifest length fits u64")
                < MAX_CONFLICT_MANIFEST_BYTES
        );
        let manifest: serde_json::Value = serde_json::from_slice(&manifest).expect("manifest JSON");
        assert!(manifest.get("paths").is_none());
        assert_eq!(manifest["path_count"], serde_json::json!(10_000));
        assert_eq!(
            manifest["path_pages"]
                .as_array()
                .expect("page references")
                .len(),
            79
        );
        let loaded = store.get(&key, &id).expect("paged record loads");
        assert_eq!(loaded, record);
    }

    #[test]
    fn conflict_detail_pages_report_explicit_bounds_and_progress() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).expect("vault directory");
        let canonical = fs::canonicalize(&vault).expect("canonical vault");
        let key = crate::sync_state::repository_state_key(&canonical);
        let id = "b".repeat(32);
        let state_store = SyncStateStore::at(temporary.path().join("state"));
        let store = SyncConflictStore::from_state_store(&state_store);
        let directory = store.conflict_directory(&key, &id).expect("directory");
        fs::create_dir_all(&directory).expect("conflict directory");
        let mut record = unresolved_record(&id, &key, &canonical);
        record.paths[0].local.object_id = Some("1".repeat(40));
        record.paths[0].remote.object_id = Some("2".repeat(40));
        let prototype = record.paths[0].clone();
        record.paths = (0..300)
            .map(|index| SyncConflictPathRecord {
                path: format!("Notes/{index:03}.md"),
                ..prototype.clone()
            })
            .collect();
        assign_conflict_groups(record.scope, &mut record.paths);
        write_paged_record_noclobber(&directory, &record).expect("record");

        let page = get_sync_conflict_page_with_state_store(
            &VaultPaths::new(&vault),
            &id,
            128,
            64,
            &state_store,
        )
        .expect("detail page");
        assert_eq!(page.record.paths.len(), 64);
        assert_eq!(page.record.paths[0].path, "Notes/128.md");
        assert_eq!(page.progress.total_paths, 300);
        assert_eq!(page.progress.total_groups, 300);
        assert_eq!(page.progress.returned_groups, 64);
        assert!(!page.progress.groups_complete);
        assert_eq!(page.progress.groups.len(), 64);
        assert!(page.progress.groups.iter().all(|group| group
            .paths
            .iter()
            .all(|path| path.as_str() >= "Notes/128.md" && path.as_str() < "Notes/192.md")));
        assert_eq!(
            page.path_page,
            Some(SyncConflictPathPageInfo {
                offset: 128,
                limit: 64,
                total: 300,
                next_offset: Some(192),
            })
        );

        fs::remove_file(directory.join("path-pages/paths-000002.json"))
            .expect("remove an unrequested page");
        let bounded = get_sync_conflict_page_with_state_store(
            &VaultPaths::new(&vault),
            &id,
            0,
            32,
            &state_store,
        )
        .expect("unrequested evidence pages are not read");
        assert_eq!(bounded.record.paths.len(), 32);
    }

    #[test]
    fn incomplete_or_tampered_path_pages_fail_closed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let id = "b".repeat(32);
        let store = SyncConflictStore::at(temporary.path().join("state"));
        let directory = store.conflict_directory(&key, &id).expect("directory");
        fs::create_dir_all(&directory).expect("conflict directory");
        let record = unresolved_record(&id, &key, temporary.path());
        write_paged_record_noclobber(&directory, &record).expect("paged record");
        fs::write(
            directory.join("path-pages/paths-000000.json"),
            b"tampered\n",
        )
        .expect("tamper page");

        let error = store.get(&key, &id).expect_err("tampered page must fail");
        assert!(error.to_string().contains("manifest digest"));
    }

    #[test]
    fn conflict_groups_are_deterministic_and_keep_structural_paths_atomic() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut record = unresolved_record(&"b".repeat(32), &"a".repeat(32), temporary.path());
        let mut ordinary = record.paths[0].clone();
        ordinary.path = "Notes/plain.md".to_string();
        ordinary.local.object_id = Some("1".repeat(40));
        ordinary.remote.object_id = Some("2".repeat(40));
        let mut structural_two = record.paths[0].clone();
        structural_two.path = "Moved/two.md".to_string();
        record.paths = vec![structural_two, ordinary, record.paths[0].clone()];
        assign_conflict_groups(record.scope, &mut record.paths);
        let first = conflict_groups(&record);

        record.paths.reverse();
        for path in &mut record.paths {
            std::mem::swap(&mut path.local, &mut path.remote);
        }
        assign_conflict_groups(record.scope, &mut record.paths);
        let second = conflict_groups(&record);

        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        let structural = first
            .iter()
            .find(|group| group.kind == SyncConflictGroupKind::Structural)
            .expect("structural group");
        assert_eq!(structural.paths, vec!["Home.md", "Moved/two.md"]);
    }

    #[test]
    fn batch_progress_is_durable_bounded_and_allows_replanning_stale_groups() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let id = "b".repeat(32);
        let store = SyncConflictStore::at(temporary.path().join("state"));
        let directory = store.conflict_directory(&key, &id).expect("directory");
        fs::create_dir_all(&directory).expect("conflict directory");
        let mut record = unresolved_record(&id, &key, temporary.path());
        record.paths[0].local.object_id = Some("1".repeat(40));
        record.paths[0].remote.object_id = Some("2".repeat(40));
        let mut second = record.paths[0].clone();
        second.path = "Second.md".to_string();
        record.paths.push(second);
        assign_conflict_groups(record.scope, &mut record.paths);
        write_paged_record_noclobber(&directory, &record).expect("record");
        let groups = conflict_groups(&record);

        let batch = |group_id: String,
                     expected: &str,
                     published: bool,
                     applied: bool,
                     needs_rebase: bool| {
            let group_ids = vec![group_id];
            let batch_id = conflict_batch_id(&id, &group_ids, expected, "side:local");
            SyncConflictBatchRecord {
                version: SYNC_CONFLICT_BATCH_VERSION,
                conflict_id: id.clone(),
                batch_id,
                selection_digest: conflict_group_selection_digest(&group_ids),
                group_ids,
                group_path_counts: BTreeMap::new(),
                expected_revision: expected.to_string(),
                side: Some(SyncConflictResolutionSide::Local),
                proposal_id: None,
                recovery_revision: "recovery".to_string(),
                resolved_tree: "tree".to_string(),
                resolution_commit: "commit".to_string(),
                published,
                applied,
                needs_rebase,
            }
        };
        store
            .save_batch(&key, &batch(groups[0].id.clone(), "one", true, true, false))
            .expect("applied batch");
        store
            .save_batch(
                &key,
                &batch(groups[1].id.clone(), "one", false, false, true),
            )
            .expect("stale batch");
        store
            .save_batch(
                &key,
                &batch(groups[1].id.clone(), "two", false, false, false),
            )
            .expect("replacement batch");

        let progress = store.group_progress(&key, &record).expect("progress");
        assert_eq!(progress.total_groups, 2);
        assert_eq!(progress.applied_groups, 1);
        assert_eq!(progress.prepared_groups, 1);
        assert_eq!(progress.pending_groups, 0);
        assert_eq!(progress.needs_rebase_groups, 0);
        assert_eq!(store.list_batches(&key, &id).expect("batches").len(), 3);
    }

    #[test]
    fn unsupported_future_resolution_versions_still_fail_closed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let id = "b".repeat(32);
        let store = SyncConflictStore::at(temporary.path().join("state"));
        write_version_one_record(&store, &key, &id, temporary.path());
        let resolution = serde_json::json!({
            "version": SYNC_CONFLICT_RESOLUTION_VERSION + 1,
            "conflict_id": id,
            "base_revision": "base",
            "local_revision": "local",
            "remote_revision": "remote",
            "recovery_revision": "recovery",
            "resolved_tree": "tree",
            "resolution_commit": "commit",
            "published": true,
            "applied": false
        });
        fs::write(
            store
                .conflict_directory(&key, &id)
                .expect("directory")
                .join("resolution.json"),
            serde_json::to_vec_pretty(&resolution).expect("resolution"),
        )
        .expect("resolution file");

        let error = store
            .get_resolution(&key, &id)
            .expect_err("future resolution version must fail");
        assert!(error.to_string().contains("version or identity mismatch"));
    }
}
