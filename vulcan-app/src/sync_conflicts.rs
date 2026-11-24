//! Durable device-local conflict records and preserved file artifacts.

#[cfg(test)]
use crate::durable_file::DurableCreate;
use crate::durable_file::{self};
use crate::sync_state::SyncStateStore;
use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use vulcan_sync::{
    conflict_archive_ref, GitConflictClassification, GitConflictScope, GitEngine, GitOid,
    GitRefName, GitRemote, GitRepository, GitSyncOptions, GitTreeEntry, LOCAL_VULCAN_REF_ROOT,
    REMOTE_CONFLICT_BRANCH_ROOT,
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
const MAX_CONFLICT_PATH_COUNT: usize = 1_000_000;
const MAX_CONFLICT_DIAGNOSTICS_BYTES: usize = 1024 * 1024;
const MAX_CONFLICT_RESOLUTION_BYTES: u64 = 1024 * 1024;
const MAX_CONFLICT_GROUPS_PER_BATCH: usize = 128;
const MAX_CONFLICT_SUMMARY_PATHS: usize = 16;
/// Beside a repository's `conflicts/`: one empty marker per conflict that may
/// still be unresolved, so sync maintenance reads only those records instead
/// of every conflict ever recorded.
const OPEN_CONFLICTS_DIR: &str = "open-conflicts";
/// Beside `open-conflicts/`: one empty marker per conflict found resolved or
/// superseded. Those states are final, so a full re-index skips these
/// conflicts by a marker check instead of reading their records again.
const CLOSED_CONFLICTS_DIR: &str = "closed-conflicts";
/// Closed conflicts move to the archive this long after they were found
/// resolved or superseded.
pub const CONFLICT_ARCHIVE_AFTER: std::time::Duration = std::time::Duration::from_hours(720);
/// At most this many conflicts move per archive run, bounding each run.
const MAX_ARCHIVED_PER_RUN: usize = 256;
/// Device-local mirror of the archive's conflict count, so listings report
/// it without reading Git.
const ARCHIVED_COUNT_FILE: &str = "archived-conflicts";
/// The archive tree's own conflict count.
const ARCHIVE_COUNT_PATH: &str = "count";
/// Holds the modification time of `conflicts/` when the index last covered
/// every conflict directory. Creating a conflict directory, by this or any
/// other version, changes that time and triggers one full re-index.
const OPEN_INDEX_STAMP: &str = ".indexed-at";
/// A directory time this recent is not trusted as a stamp: a coarse clock
/// could give a conflict created in the same tick the same time.
const OPEN_INDEX_SETTLE: std::time::Duration = std::time::Duration::from_secs(2);

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
    /// The original conflict this one descends from when a sync carried it
    /// forward onto a later live version; a chain of replacements always
    /// names the original, and intermediate replacements are removed. Such a record's local side is
    /// already an ancestor of its remote side, so it resolves by overlaying
    /// its groups on the live tree rather than by re-running the merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried_from: Option<String>,
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
    pub paths_returned: usize,
    pub paths_complete: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncConflictListReport {
    pub vault: PathBuf,
    pub repository_key: String,
    /// Number of currently actionable unresolved conflicts. Retained
    /// superseded history is reported separately and excluded here.
    pub count: usize,
    pub superseded_count: usize,
    /// Closed conflicts moved to the archive; still readable by ID.
    pub archived_count: usize,
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
    let work_tree = crate::sync_state::sync_work_tree(paths.vault_root())?;
    let repository_key = crate::sync_state::repository_state_key(&work_tree);
    let store = SyncConflictStore::from_state_store(state_store);
    let states = store
        .list_ids(&repository_key)?
        .into_iter()
        .map(|conflict_id| {
            let (record, path_count, progress) = store.get_page_and_progress(
                &repository_key,
                &conflict_id,
                0,
                MAX_CONFLICT_SUMMARY_PATHS,
            )?;
            let resolution =
                store.resolution_state_with_progress(&repository_key, &record.id, &progress)?;
            let scope = effective_conflict_scope(&record);
            let paths = record
                .paths
                .into_iter()
                .map(|path| path.path)
                .collect::<Vec<_>>();
            Ok((
                resolution,
                SyncConflictSummary {
                    id: record.id,
                    scope,
                    paths_returned: paths.len(),
                    paths_complete: paths.len() == path_count,
                    paths,
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
    store.reconcile_open_index(
        &repository_key,
        &states
            .iter()
            .map(|(state, summary)| (*state, summary.id.clone()))
            .collect::<Vec<_>>(),
    )?;
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
    let archived_count = store.archived_count(&repository_key);
    Ok(SyncConflictListReport {
        vault: work_tree,
        repository_key,
        count: conflicts.len(),
        superseded_count,
        archived_count,
        conflicts,
    })
}

/// Moves conflicts closed for at least `older_than` into the conflict archive
/// now. With `include_open`, also archives conflicts still open: records left
/// by the removed whole-worktree sync, which nothing can resolve any more and
/// which would otherwise report the vault as conflicted indefinitely. A dry
/// run reports what would move without taking locks or changing anything.
pub fn archive_sync_conflicts(
    paths: &vulcan_core::VaultPaths,
    older_than: std::time::Duration,
    include_open: bool,
    dry_run: bool,
) -> Result<ConflictArchiveReport, AppError> {
    let state_store = SyncStateStore::user_default()?;
    archive_sync_conflicts_with_state_store(paths, older_than, include_open, dry_run, &state_store)
}

pub fn archive_sync_conflicts_with_state_store(
    paths: &vulcan_core::VaultPaths,
    older_than: std::time::Duration,
    include_open: bool,
    dry_run: bool,
    state_store: &SyncStateStore,
) -> Result<ConflictArchiveReport, AppError> {
    let work_tree = crate::sync_state::sync_work_tree(paths.vault_root())?;
    let repository_key = crate::sync_state::repository_state_key(&work_tree);
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&work_tree)
        .map_err(AppError::operation)?;
    let _vault_lock = (!dry_run && paths.vulcan_dir().is_dir())
        .then(|| vulcan_core::write_lock::acquire_write_lock(paths))
        .transpose()
        .map_err(AppError::operation)?;
    let _repository_lock = (!dry_run)
        .then(|| vulcan_sync::RepositoryLock::acquire(&repository.git_dir))
        .transpose()?;
    let remote =
        crate::sync::configured_git_sync_options(paths, &GitSyncOptions::default())?.remote;
    SyncConflictStore::from_state_store(state_store).archive_closed(
        &engine,
        &repository,
        &repository_key,
        older_than,
        include_open,
        Some(&remote),
        dry_run,
    )
}

/// The number of unresolved conflicts, read through the open-conflict index:
/// cost follows the open conflicts, not every recently closed one, so status
/// polling stays cheap. Use [`list_sync_conflicts_with_state_store`] for the
/// summaries themselves.
pub fn open_sync_conflict_count_with_state_store(
    paths: &vulcan_core::VaultPaths,
    state_store: &SyncStateStore,
) -> Result<usize, AppError> {
    let work_tree = crate::sync_state::sync_work_tree(paths.vault_root())?;
    let repository_key = crate::sync_state::repository_state_key(&work_tree);
    Ok(SyncConflictStore::from_state_store(state_store)
        .open_records(&repository_key)?
        .len())
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
    let work_tree = crate::sync_state::sync_work_tree(paths.vault_root())?;
    let repository_key = crate::sync_state::repository_state_key(&work_tree);
    let store = SyncConflictStore::from_state_store(state_store);
    if !store.is_live(&repository_key, conflict_id)? {
        if let Some((_temporary, archived)) =
            archived_conflict_store(&work_tree, &repository_key, conflict_id)?
        {
            return conflict_detail(&archived, &repository_key, conflict_id);
        }
    }
    conflict_detail(&store, &repository_key, conflict_id)
}

fn conflict_detail(
    store: &SyncConflictStore,
    repository_key: &str,
    conflict_id: &str,
) -> Result<SyncConflictDetailReport, AppError> {
    let repository_key = repository_key.to_string();
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
    let work_tree = crate::sync_state::sync_work_tree(paths.vault_root())?;
    let repository_key = crate::sync_state::repository_state_key(&work_tree);
    let live = SyncConflictStore::from_state_store(state_store);
    let archived = if live.is_live(&repository_key, conflict_id)? {
        None
    } else {
        archived_conflict_store(&work_tree, &repository_key, conflict_id)?
    };
    let store = archived.as_ref().map_or(&live, |(_, archived)| archived);
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConflictArchiveReport {
    pub dry_run: bool,
    /// Conflicts moved (or, in a dry run, that would move) this run.
    pub archived: Vec<String>,
    /// Conflicts in the archive after this run.
    pub archived_total: usize,
    /// Remote conflict refs (records and published resolutions) of the
    /// archived conflicts deleted from the remote.
    pub remote_refs_released: usize,
}

/// Where one conflict lives inside the archive tree: sharded by its first
/// two hex digits, so each run rewrites only the touched shards.
fn archive_prefix(conflict_id: &str) -> String {
    format!("{}/{conflict_id}", &conflict_id[..2])
}

/// Every file of a conflict directory except the artifact copies, whose
/// contents the archive references as the original Git blobs instead.
fn archive_files(directory: &Path) -> Result<Vec<(String, Vec<u8>)>, AppError> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).map_err(AppError::operation)? {
            let entry = entry.map_err(AppError::operation)?;
            let path = entry.path();
            let relative = path
                .strip_prefix(directory)
                .map_err(AppError::operation)?
                .to_string_lossy()
                .replace('\\', "/");
            let kind = entry.file_type().map_err(AppError::operation)?;
            if kind.is_dir() {
                if relative != "artifacts" {
                    pending.push(path);
                }
            } else if kind.is_file() {
                files.push((relative, fs::read(&path).map_err(AppError::operation)?));
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Archive file contents keyed by their path inside the archive tree.
type ArchiveFiles = Vec<(String, Vec<u8>)>;

/// Materializes one archived conflict into a temporary store so the ordinary
/// readers can serve it; `None` when the archive does not hold it.
fn archived_conflict_store(
    work_tree: &Path,
    repository_key: &str,
    conflict_id: &str,
) -> Result<Option<(tempfile::TempDir, SyncConflictStore)>, AppError> {
    validate_hex_id("conflict ID", conflict_id)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let Ok(repository) = engine.discover_repository(work_tree) else {
        return Ok(None);
    };
    let reference = conflict_archive_ref().map_err(AppError::operation)?;
    let Some(archive) = engine
        .read_ref(&repository, &reference)
        .map_err(AppError::operation)?
    else {
        return Ok(None);
    };
    let segments = archive_segments(&engine, &repository, &archive)?;
    let prefixes = segments
        .iter()
        .map(|(_, location)| match location {
            Some(segment) => format!("{segment}/{}", archive_prefix(conflict_id)),
            None => archive_prefix(conflict_id),
        })
        .collect::<Vec<_>>();
    let records = prefixes
        .iter()
        .map(|prefix| format!("{prefix}/record.json"))
        .collect::<Vec<_>>();
    let Some(prefix) = engine
        .resolve_paths(&repository, &archive, &records)
        .map_err(AppError::operation)?
        .iter()
        .position(Option::is_some)
        .map(|index| prefixes[index].clone())
    else {
        return Ok(None);
    };
    let entries = engine
        .tree_entries_under(&repository, &archive, &prefix)
        .map_err(AppError::operation)?
        .into_iter()
        .filter(|entry| !entry.path.starts_with(&format!("{prefix}/sides/")))
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Ok(None);
    }
    let blobs = engine
        .read_blobs(
            &repository,
            &entries
                .iter()
                .map(|entry| entry.oid.clone())
                .collect::<Vec<_>>(),
        )
        .map_err(AppError::operation)?;
    let temporary = tempfile::tempdir().map_err(AppError::operation)?;
    let store = SyncConflictStore::at(temporary.path().to_path_buf());
    let directory = store.conflict_directory(repository_key, conflict_id)?;
    for entry in &entries {
        let relative = &entry.path[prefix.len() + 1..];
        let target = directory.join(relative);
        fs::create_dir_all(target.parent().unwrap_or(&directory)).map_err(AppError::operation)?;
        let bytes = blobs
            .get(&entry.oid)
            .ok_or_else(|| AppError::operation("the conflict archive is missing an object"))?;
        fs::write(&target, bytes).map_err(AppError::operation)?;
    }
    Ok(Some((temporary, store)))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // One fallible contract across platforms.
fn set_carried_mode(_path: &Path, _mode: Option<&str>) -> Result<(), AppError> {
    Ok(())
}

/// Deletes the remote conflict refs (`.../conflicts/<id>/record` and
/// published resolution refs) of `conflict_ids` with one listing and one
/// leased push. Nothing reads them back, and the commits they name are in
/// live history. Returns how many were deleted.
fn release_remote_conflict_refs(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    remote: &GitRemote,
    conflict_ids: &[String],
) -> Result<usize, AppError> {
    if conflict_ids.is_empty() {
        return Ok(0);
    }
    let root = GitRefName::parse(REMOTE_CONFLICT_BRANCH_ROOT).map_err(AppError::operation)?;
    let archived = conflict_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let deletions = engine
        .list_remote_refs(repository, remote, &root)
        .map_err(AppError::operation)?
        .into_iter()
        .filter(|reference| {
            reference
                .name
                .as_str()
                .strip_prefix(REMOTE_CONFLICT_BRANCH_ROOT)
                .and_then(|rest| rest.strip_prefix('/'))
                .and_then(|rest| rest.split('/').next())
                .is_some_and(|id| archived.contains(id))
        })
        .map(|reference| (reference.name, reference.target))
        .collect::<Vec<_>>();
    Ok(engine
        .delete_remote_refs(repository, remote, &deletions)
        .map_err(AppError::operation)?
        .into_iter()
        .filter(|result| *result == vulcan_sync::GitRefDeleteResult::Deleted)
        .count())
}

/// The archive root after adding `segment`: the existing segments as they
/// are, the new one under the next name, and the updated `count`.
fn archive_root(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    current: Option<&GitOid>,
    segment: GitOid,
    total: usize,
) -> Result<GitOid, AppError> {
    let mut root = match current {
        Some(commit) => archive_segments(engine, repository, commit)?
            .into_iter()
            .map(|(entry, _)| entry)
            .collect(),
        None => Vec::new(),
    };
    let next = root
        .iter()
        .filter_map(|entry| entry.path.strip_prefix('s')?.parse::<u64>().ok())
        .max()
        .map_or(1, |last| last + 1);
    root.push(GitTreeEntry {
        path: format!("s{next:06}"),
        oid: segment,
        mode: "040000".to_string(),
        kind: "tree".to_string(),
    });
    root.push(GitTreeEntry {
        path: ARCHIVE_COUNT_PATH.to_string(),
        oid: engine
            .write_blob(repository, total.to_string().as_bytes())
            .map_err(AppError::operation)?,
        mode: "100644".to_string(),
        kind: "blob".to_string(),
    });
    engine
        .make_tree(repository, &root)
        .map_err(AppError::operation)
}

/// The archive's segments, each as its root entry and where it is read from:
/// `Some(name)` for a segment subtree, `None` for an archive written before
/// segments, whose whole root is one segment. Building wraps such a root
/// unchanged as segment `s000000`.
fn archive_segments(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    archive: &GitOid,
) -> Result<Vec<(GitTreeEntry, Option<String>)>, AppError> {
    let children = engine
        .tree_children(repository, archive)
        .map_err(AppError::operation)?;
    let legacy = children.iter().any(|entry| {
        entry.kind == "tree"
            && entry.path.len() == 2
            && entry.path.bytes().all(|byte| byte.is_ascii_hexdigit())
    });
    if legacy {
        return Ok(vec![(
            GitTreeEntry {
                path: "s000000".to_string(),
                oid: engine
                    .tree_oid(repository, archive)
                    .map_err(AppError::operation)?,
                mode: "040000".to_string(),
                kind: "tree".to_string(),
            },
            None,
        )]);
    }
    Ok(children
        .into_iter()
        .filter(|entry| entry.kind == "tree" && entry.path.starts_with('s'))
        .map(|entry| {
            let name = entry.path.clone();
            (entry, Some(name))
        })
        .collect())
}

/// The conflict count recorded in an archive commit's tree.
fn archive_count(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    archive: &GitOid,
) -> Result<usize, AppError> {
    let entries = engine
        .tree_entries_for_paths(repository, archive, &[ARCHIVE_COUNT_PATH.to_string()])
        .map_err(AppError::operation)?;
    let Some(entry) = entries.first() else {
        return Ok(0);
    };
    let blobs = engine
        .read_blobs(repository, std::slice::from_ref(&entry.oid))
        .map_err(AppError::operation)?;
    Ok(blobs
        .get(&entry.oid)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0))
}

fn effective_conflict_scope(record: &SyncConflictRecord) -> GitConflictScope {
    if record.scope == GitConflictScope::TreeValidation || record.paths.is_empty() {
        GitConflictScope::TreeValidation
    } else {
        GitConflictScope::Paths
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

    /// The records of unresolved conflicts, read through the open index:
    /// cost follows the conflicts still open, not the repository's conflict
    /// history. Conflicts found resolved or superseded leave the index; those
    /// states are final. Builds the index from a full listing once.
    pub(crate) fn open_records(
        &self,
        repository_key: &str,
    ) -> Result<Vec<SyncConflictRecord>, AppError> {
        validate_hex_id("repository key", repository_key)?;
        let index = self.root.join(repository_key).join(OPEN_CONFLICTS_DIR);
        fs::create_dir_all(&index).map_err(AppError::operation)?;
        let stamp_path = index.join(OPEN_INDEX_STAMP);
        let directory_time = conflicts_directory_time(&self.root.join(repository_key))?;
        let stamp = fs::read_to_string(&stamp_path).ok();
        if directory_time.as_ref().map(|(stamp, _)| stamp) != stamp.as_ref() {
            // Conflicts appeared since the last full pass. Index all of them
            // not already known closed; the pass below drops the ones
            // resolved or superseded since.
            let closed = self.root.join(repository_key).join(CLOSED_CONFLICTS_DIR);
            for id in self.list_ids(repository_key)? {
                if !closed.join(&id).is_file() {
                    self.mark_open(repository_key, &id)?;
                }
            }
            match directory_time {
                Some((stamp, settled)) if settled => {
                    durable_file::replace(&stamp_path, stamp.as_bytes())?;
                }
                _ => {
                    durable_file::remove(&stamp_path)?;
                }
            }
        }
        let mut ids = Vec::new();
        for entry in fs::read_dir(&index).map_err(AppError::operation)? {
            let id = entry
                .map_err(AppError::operation)?
                .file_name()
                .to_string_lossy()
                .to_string();
            if validate_hex_id("conflict ID", &id).is_ok() {
                ids.push(id);
            }
        }
        ids.sort();
        let mut records = Vec::with_capacity(ids.len());
        for id in ids {
            let directory = self.conflict_directory(repository_key, &id)?;
            if !directory.join("record.json").is_file() {
                // Still being recorded, or never completed.
                continue;
            }
            let record = self.get(repository_key, &id)?;
            let progress = self.group_progress(repository_key, &record)?;
            if self.resolution_state_with_progress(repository_key, &id, &progress)?
                == SyncConflictResolutionState::Unresolved
            {
                records.push(record);
            } else {
                self.mark_closed(repository_key, &id)?;
            }
        }
        Ok(records)
    }

    /// The per-repository state directory. Every path below it is built
    /// from a validated key, so no caller-supplied value can leave the store.
    fn repository_directory(&self, repository_key: &str) -> Result<PathBuf, AppError> {
        validate_hex_id("repository key", repository_key)?;
        Ok(self.root.join(repository_key))
    }

    fn mark_open(&self, repository_key: &str, conflict_id: &str) -> Result<(), AppError> {
        validate_hex_id("conflict ID", conflict_id)?;
        let index = self
            .repository_directory(repository_key)?
            .join(OPEN_CONFLICTS_DIR);
        fs::create_dir_all(&index).map_err(AppError::operation)?;
        durable_file::create(&index.join(conflict_id), b"").map(drop)
    }

    fn mark_closed(&self, repository_key: &str, conflict_id: &str) -> Result<(), AppError> {
        validate_hex_id("conflict ID", conflict_id)?;
        let repository = self.repository_directory(repository_key)?;
        let closed = repository.join(CLOSED_CONFLICTS_DIR);
        fs::create_dir_all(&closed).map_err(AppError::operation)?;
        // Closed before leaving the open index, so a crash never loses both.
        durable_file::create(&closed.join(conflict_id), b"")?;
        durable_file::remove(&repository.join(OPEN_CONFLICTS_DIR).join(conflict_id)).map(drop)
    }

    /// Repairs the open index from a full listing's states, for conflicts
    /// recorded by versions that did not maintain it.
    fn reconcile_open_index(
        &self,
        repository_key: &str,
        states: &[(SyncConflictResolutionState, String)],
    ) -> Result<(), AppError> {
        let index = self
            .repository_directory(repository_key)?
            .join(OPEN_CONFLICTS_DIR);
        if !index.is_dir() {
            return Ok(());
        }
        for (state, id) in states {
            validate_hex_id("conflict ID", id)?;
            if *state == SyncConflictResolutionState::Unresolved {
                if !index.join(id).is_file() {
                    self.mark_open(repository_key, id)?;
                }
            } else if index.join(id).is_file() {
                self.mark_closed(repository_key, id)?;
            }
        }
        Ok(())
    }

    pub fn list(&self, repository_key: &str) -> Result<Vec<SyncConflictRecord>, AppError> {
        let mut records = self
            .list_ids(repository_key)?
            .into_iter()
            .map(|id| self.get(repository_key, &id))
            .collect::<Result<Vec<_>, _>>()?;
        records.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(records)
    }

    pub fn list_ids(&self, repository_key: &str) -> Result<Vec<String>, AppError> {
        validate_hex_id("repository key", repository_key)?;
        let root = self.root.join(repository_key).join("conflicts");
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(AppError::operation(error)),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(AppError::operation)?;
            if !entry.file_type().map_err(AppError::operation)?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            validate_hex_id("conflict ID", &id)?;
            ids.push(id);
        }
        ids.sort();
        Ok(ids)
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

    /// Moves conflicts closed (resolved or superseded) for at least
    /// `older_than` into the archive: one local ref,
    /// `refs/vulcan/conflict-archive`, naming a parentless commit whose tree
    /// holds each conflict's record, pages, decisions, and proposals as blobs
    /// under `<first two hex digits>/<id>/`, plus its conflicted file versions
    /// as entries pointing at the existing Git blobs under `sides/`. Once the
    /// ref moves, the conflict's directory, markers, and local refs go away;
    /// readers then serve it from the archive. At most a bounded batch moves
    /// per run. The caller holds the vault and repository locks.
    #[allow(clippy::too_many_arguments)]
    pub fn archive_closed(
        &self,
        engine: &dyn GitEngine,
        repository: &GitRepository,
        repository_key: &str,
        older_than: std::time::Duration,
        include_open: bool,
        remote: Option<&GitRemote>,
        dry_run: bool,
    ) -> Result<ConflictArchiveReport, AppError> {
        validate_hex_id("repository key", repository_key)?;
        let mut candidates = self.archive_candidates(repository_key, older_than)?;
        if include_open {
            for record in self.open_records(repository_key)? {
                if !candidates.contains(&record.id) {
                    candidates.push(record.id);
                }
            }
        }
        if candidates.is_empty() && !dry_run {
            // Nothing to move: answer without reading Git.
            return Ok(ConflictArchiveReport {
                dry_run,
                archived: candidates,
                archived_total: self.archived_count(repository_key),
                remote_refs_released: 0,
            });
        }
        let reference = conflict_archive_ref().map_err(AppError::operation)?;
        let current = engine
            .read_ref(repository, &reference)
            .map_err(AppError::operation)?;
        let previous_total = match &current {
            Some(commit) => archive_count(engine, repository, commit)?,
            None => 0,
        };
        if dry_run || candidates.is_empty() {
            return Ok(ConflictArchiveReport {
                dry_run,
                archived_total: previous_total + if dry_run { candidates.len() } else { 0 },
                archived: candidates,
                remote_refs_released: 0,
            });
        }
        let (files, sides) = self.archive_entries(repository_key, &candidates)?;
        let total = previous_total + candidates.len();
        let blobs = engine
            .write_blobs(
                repository,
                &files
                    .iter()
                    .map(|(_, bytes)| bytes.as_slice())
                    .collect::<Vec<_>>(),
            )
            .map_err(AppError::operation)?;
        let mut entries = files
            .into_iter()
            .zip(blobs)
            .map(|((path, _), oid)| GitTreeEntry {
                path,
                oid,
                mode: "100644".to_string(),
                kind: "blob".to_string(),
            })
            .collect::<Vec<_>>();
        entries.extend(sides);
        // This run's conflicts become one new, immutable segment; earlier
        // segments are reused as they are, so a run costs its batch plus the
        // number of segments, never the size of the archive.
        let segment = engine
            .tree_with_entries(repository, None, &entries)
            .map_err(AppError::operation)?;
        let tree = archive_root(engine, repository, current.as_ref(), segment, total)?;
        // Parentless: the newest root names every segment, so earlier archive
        // roots are garbage-collected instead of accumulating.
        let commit = engine
            .create_commit(repository, &tree, &[], "vulcan conflict archive\n")
            .map_err(AppError::operation)?;
        if engine
            .compare_and_swap_ref(repository, &reference, &commit, current.as_ref())
            .map_err(AppError::operation)?
            == vulcan_sync::GitRefUpdateResult::Stale
        {
            return Err(AppError::operation(
                "the conflict archive changed concurrently; the next run retries",
            ));
        }
        durable_file::replace(
            &self.root.join(repository_key).join(ARCHIVED_COUNT_FILE),
            total.to_string().as_bytes(),
        )?;
        for id in &candidates {
            self.remove_archived(engine, repository, repository_key, id)?;
        }
        // Best effort: the archive is complete locally, and an offline remote
        // keeps its refs until a later run archives more conflicts.
        let remote_refs_released = remote
            .and_then(|remote| {
                release_remote_conflict_refs(engine, repository, remote, &candidates).ok()
            })
            .unwrap_or(0);
        Ok(ConflictArchiveReport {
            dry_run,
            archived: candidates,
            archived_total: total,
            remote_refs_released,
        })
    }

    /// The archive entries for `candidates`: each conflict's files (artifact
    /// copies excluded) and its conflicted versions as the original blobs.
    fn archive_entries(
        &self,
        repository_key: &str,
        candidates: &[String],
    ) -> Result<(ArchiveFiles, Vec<GitTreeEntry>), AppError> {
        let mut files = Vec::new();
        let mut sides = Vec::new();
        for id in candidates {
            let prefix = archive_prefix(id);
            let record = self.get(repository_key, id)?;
            for (index, path) in record.paths.iter().enumerate() {
                for (role, side) in [
                    ("base", &path.base),
                    ("local", &path.local),
                    ("remote", &path.remote),
                ] {
                    let (Some(object), Some(mode)) = (&side.object_id, &side.mode) else {
                        continue;
                    };
                    if side.kind.as_deref() != Some("blob")
                        || !matches!(mode.as_str(), "100644" | "100755" | "120000")
                    {
                        continue;
                    }
                    sides.push(GitTreeEntry {
                        path: format!("{prefix}/sides/{index:06}-{role}"),
                        oid: GitOid::parse(object).map_err(AppError::operation)?,
                        mode: mode.clone(),
                        kind: "blob".to_string(),
                    });
                }
            }
            let directory = self.conflict_directory(repository_key, id)?;
            for (relative, bytes) in archive_files(&directory)? {
                files.push((format!("{prefix}/{relative}"), bytes));
            }
        }
        Ok((files, sides))
    }

    /// Closed conflicts old enough to archive, oldest closure first, at most
    /// one run's batch. Reads only the closed markers, never the records.
    fn archive_candidates(
        &self,
        repository_key: &str,
        older_than: std::time::Duration,
    ) -> Result<Vec<String>, AppError> {
        let closed = self
            .repository_directory(repository_key)?
            .join(CLOSED_CONFLICTS_DIR);
        let entries = match fs::read_dir(&closed) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(AppError::operation(error)),
        };
        let cutoff = std::time::SystemTime::now()
            .checked_sub(older_than)
            .unwrap_or(std::time::UNIX_EPOCH);
        let mut candidates = Vec::new();
        for entry in entries {
            let entry = entry.map_err(AppError::operation)?;
            let id = entry.file_name().to_string_lossy().to_string();
            if validate_hex_id("conflict ID", &id).is_err() {
                continue;
            }
            let closed_at = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .map_err(AppError::operation)?;
            let directory = self.conflict_directory(repository_key, &id)?;
            if closed_at <= cutoff && directory.join("record.json").is_file() {
                candidates.push((closed_at, id));
            }
        }
        candidates.sort();
        Ok(candidates
            .into_iter()
            .take(MAX_ARCHIVED_PER_RUN)
            .map(|(_, id)| id)
            .collect())
    }

    /// Drops an archived conflict's local refs, directory, and markers.
    fn remove_archived(
        &self,
        engine: &dyn GitEngine,
        repository: &GitRepository,
        repository_key: &str,
        conflict_id: &str,
    ) -> Result<(), AppError> {
        let prefix = GitRefName::parse(format!("{LOCAL_VULCAN_REF_ROOT}/conflicts/{conflict_id}"))
            .map_err(AppError::operation)?;
        for reference in engine
            .list_refs(repository, &prefix)
            .map_err(AppError::operation)?
        {
            engine
                .delete_ref(repository, &reference.name, &reference.target)
                .map_err(AppError::operation)?;
        }
        let directory = self.conflict_directory(repository_key, conflict_id)?;
        if directory.exists() {
            fs::remove_dir_all(&directory).map_err(AppError::operation)?;
        }
        let root = self.repository_directory(repository_key)?;
        durable_file::remove(&root.join(OPEN_CONFLICTS_DIR).join(conflict_id))?;
        durable_file::remove(&root.join(CLOSED_CONFLICTS_DIR).join(conflict_id))?;
        Ok(())
    }

    /// Whether the conflict's record is in the live store (not archived).
    pub fn is_live(&self, repository_key: &str, conflict_id: &str) -> Result<bool, AppError> {
        Ok(self
            .conflict_directory(repository_key, conflict_id)?
            .join("record.json")
            .is_file())
    }

    /// Conflicts in the archive, from the device-local mirror of its count.
    #[must_use]
    pub fn archived_count(&self, repository_key: &str) -> usize {
        self.repository_directory(repository_key)
            .ok()
            .and_then(|directory| fs::read_to_string(directory.join(ARCHIVED_COUNT_FILE)).ok())
            .and_then(|count| count.trim().parse().ok())
            .unwrap_or(0)
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

#[cfg(test)]
fn write_paged_record_noclobber(
    directory: &Path,
    record: &SyncConflictRecord,
) -> Result<(), AppError> {
    if record.paths.len() > MAX_CONFLICT_PATH_COUNT {
        return Err(AppError::operation(format!(
            "sync conflict contains more than {MAX_CONFLICT_PATH_COUNT} paths"
        )));
    }
    if record.diagnostics.len() > MAX_CONFLICT_DIAGNOSTICS_BYTES {
        return Err(AppError::operation(format!(
            "sync conflict diagnostics exceed the {MAX_CONFLICT_DIAGNOSTICS_BYTES} byte limit"
        )));
    }
    let pages_directory = directory.join("path-pages");
    fs::create_dir_all(&pages_directory).map_err(AppError::operation)?;
    let mut page_refs = Vec::new();
    let mut paths = Vec::new();
    for path in &record.paths {
        paths.push(path.clone());
        if paths.len() == MAX_CONFLICT_PATHS_PER_PAGE
            || serialized_path_page_len(page_refs.len(), &paths)? > MAX_CONFLICT_PATH_PAGE_BYTES
        {
            let overflow = (serialized_path_page_len(page_refs.len(), &paths)?
                > MAX_CONFLICT_PATH_PAGE_BYTES)
                .then(|| paths.pop())
                .flatten();
            if paths.is_empty() {
                return Err(AppError::operation(format!(
                    "one sync conflict path exceeds the {MAX_CONFLICT_PATH_PAGE_BYTES} byte page limit"
                )));
            }
            write_conflict_path_page(&pages_directory, page_refs.len(), &paths, &mut page_refs)?;
            paths.clear();
            if let Some(path) = overflow {
                paths.push(path);
            }
        }
    }
    if !paths.is_empty() {
        write_conflict_path_page(&pages_directory, page_refs.len(), &paths, &mut page_refs)?;
    }

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
        serde_json::json!(conflict_paths_digest(&record.paths)?),
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

#[cfg(test)]
fn serialized_path_page_len(
    index: usize,
    paths: &[SyncConflictPathRecord],
) -> Result<u64, AppError> {
    let page = SyncConflictPathPage {
        version: SYNC_CONFLICT_RECORD_VERSION,
        index,
        paths: paths.to_vec(),
    };
    Ok(serde_json::to_vec_pretty(&page)
        .map_err(AppError::operation)?
        .len() as u64
        + 1)
}

#[cfg(test)]
fn write_conflict_path_page(
    pages_directory: &Path,
    index: usize,
    paths: &[SyncConflictPathRecord],
    page_refs: &mut Vec<SyncConflictPathPageRef>,
) -> Result<(), AppError> {
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
    Ok(())
}

fn conflict_paths_digest(paths: &[SyncConflictPathRecord]) -> Result<String, AppError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"[");
    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            hasher.update(b",");
        }
        hasher.update(&serde_json::to_vec(path).map_err(AppError::operation)?);
    }
    hasher.update(b"]");
    Ok(hasher.finalize().to_hex().to_string())
}

fn load_paged_record(
    manifest_path: &Path,
    mut value: serde_json::Value,
) -> Result<SyncConflictRecord, AppError> {
    let manifest: SyncConflictPathManifest =
        serde_json::from_value(value.clone()).map_err(AppError::operation)?;
    validate_path_manifest_limits(&manifest)?;
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
    if conflict_paths_digest(&paths)? != manifest.paths_digest {
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
    validate_path_manifest_limits(&manifest)?;
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

fn validate_path_manifest_limits(manifest: &SyncConflictPathManifest) -> Result<(), AppError> {
    if manifest.path_count > MAX_CONFLICT_PATH_COUNT {
        return Err(AppError::operation(format!(
            "sync conflict manifest exceeds the {MAX_CONFLICT_PATH_COUNT} path limit"
        )));
    }
    let maximum_pages = manifest.path_count;
    if manifest.path_pages.len() > maximum_pages {
        return Err(AppError::operation(format!(
            "sync conflict manifest exceeds the {maximum_pages} page limit"
        )));
    }
    Ok(())
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

#[cfg(test)]
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
    if record.paths.len() > MAX_CONFLICT_PATH_COUNT
        || record.diagnostics.len() > MAX_CONFLICT_DIAGNOSTICS_BYTES
    {
        return Err(AppError::operation(
            "sync conflict record exceeds its path or diagnostics limit",
        ));
    }
    Ok(())
}

/// The modification time of a repository's `conflicts/` directory as a
/// stamp, and whether it is old enough to trust; `None` before any conflict.
fn conflicts_directory_time(repository: &Path) -> Result<Option<(String, bool)>, AppError> {
    let modified = match fs::metadata(repository.join("conflicts")) {
        Ok(metadata) => metadata.modified().map_err(AppError::operation)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::operation(error)),
    };
    let since_epoch = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(AppError::operation)?;
    let settled = modified
        .elapsed()
        .is_ok_and(|elapsed| elapsed >= OPEN_INDEX_SETTLE);
    Ok(Some((since_epoch.as_nanos().to_string(), settled)))
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

    use vulcan_core::VaultPaths;

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
            carried_from: None,
        };
        assign_conflict_groups(record.scope, &mut record.paths);
        record
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
    fn paged_conflict_storage_splits_by_bytes_and_rejects_unbounded_metadata() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let id = "b".repeat(32);
        let store = SyncConflictStore::at(temporary.path().join("state"));
        let directory = store.conflict_directory(&key, &id).expect("directory");
        fs::create_dir_all(&directory).expect("conflict directory");
        let mut record = unresolved_record(&id, &key, temporary.path());
        let prototype = record.paths[0].clone();
        record.paths = (0..129)
            .map(|index| SyncConflictPathRecord {
                path: format!("Notes/{index:03}-{}.md", "x".repeat(20_000)),
                ..prototype.clone()
            })
            .collect();
        assign_conflict_groups(record.scope, &mut record.paths);
        write_paged_record_noclobber(&directory, &record).expect("byte-aware pages");
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("record.json")).expect("manifest"))
                .expect("manifest JSON");
        assert!(
            manifest["path_pages"]
                .as_array()
                .expect("page references")
                .len()
                > 2
        );
        assert_eq!(store.get(&key, &id).expect("round trip"), record);

        let oversized_id = "c".repeat(32);
        let oversized_directory = store
            .conflict_directory(&key, &oversized_id)
            .expect("oversized directory");
        fs::create_dir_all(&oversized_directory).expect("oversized directory");
        let mut oversized = unresolved_record(&oversized_id, &key, temporary.path());
        oversized.diagnostics = "x".repeat(MAX_CONFLICT_DIAGNOSTICS_BYTES + 1);
        let error = write_paged_record_noclobber(&oversized_directory, &oversized)
            .expect_err("oversized diagnostics");
        assert!(error.to_string().contains("diagnostics exceed"));
        assert!(!oversized_directory.join("record.json").exists());
    }

    #[test]
    fn paged_conflict_reader_rejects_impossible_counts_before_loading_pages() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let key = "a".repeat(32);
        let id = "b".repeat(32);
        let store = SyncConflictStore::at(temporary.path().join("state"));
        let directory = store.conflict_directory(&key, &id).expect("directory");
        fs::create_dir_all(&directory).expect("conflict directory");
        let record = unresolved_record(&id, &key, temporary.path());
        write_paged_record_noclobber(&directory, &record).expect("record");
        let manifest_path = directory.join("record.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("manifest"))
                .expect("manifest JSON");
        manifest["path_count"] = serde_json::json!(MAX_CONFLICT_PATH_COUNT + 1);
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).expect("manifest bytes"),
        )
        .expect("tampered manifest");

        let error = store.get(&key, &id).expect_err("impossible count");
        assert!(error.to_string().contains("path limit"));
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
        let listed = list_sync_conflicts_with_state_store(&VaultPaths::new(&vault), &state_store)
            .expect("bounded conflict list");
        assert_eq!(listed.conflicts[0].path_count, 300);
        assert_eq!(
            listed.conflicts[0].paths_returned,
            MAX_CONFLICT_SUMMARY_PATHS
        );
        assert!(!listed.conflicts[0].paths_complete);
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

    #[test]
    fn index_helpers_refuse_keys_that_would_leave_the_store() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let state = SyncStateStore::at(temporary.path().join("state"));
        let store = SyncConflictStore::from_state_store(&state);
        let conflict_id = "0".repeat(32);
        fs::create_dir_all(temporary.path().join("escape")).expect("outside directory");
        fs::write(
            temporary.path().join("escape").join(ARCHIVED_COUNT_FILE),
            "7",
        )
        .expect("outside count");

        for key in ["../escape", "", "ABCDEF0123456789ABCDEF0123456789"] {
            assert!(store.mark_open(key, &conflict_id).is_err(), "{key}");
            assert!(store.mark_closed(key, &conflict_id).is_err(), "{key}");
            assert_eq!(store.archived_count(key), 0, "{key}");
        }
        let key = "f".repeat(32);
        fs::create_dir_all(state.root().join(&key).join(OPEN_CONFLICTS_DIR)).expect("index");
        let states = [(SyncConflictResolutionState::Unresolved, "../x".to_string())];
        assert!(store.reconcile_open_index(&key, &states).is_err());
        assert!(!temporary
            .path()
            .join("escape")
            .join(OPEN_CONFLICTS_DIR)
            .exists());
    }
}
