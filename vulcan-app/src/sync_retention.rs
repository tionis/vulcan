//! Planning and explicitly leased application for Git live-history retention.
//!
//! Planning is mutation-free. Application can independently expire recovery
//! checkpoint refs and, when explicitly requested, archive an over-limit live
//! epoch before replacing it with a same-tree root commit.

use crate::AppError;
use fs2::FileExt;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use vulcan_core::VaultPaths;
use vulcan_sync::{
    git_live_epoch_id, GitEngine, GitOid, GitPushResult, GitRefCreateResult, GitRefDeleteResult,
    GitRefName, GitReference, GitRemote, GitSyncOptions, GitSyncRefs,
};

pub const SYNC_RETENTION_PLAN_VERSION: u32 = 1;
const MAX_RETENTION_BOUND: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncRetentionPolicy {
    pub live_epoch_max_commits: usize,
    pub recovery_checkpoints_keep: usize,
}

impl Default for SyncRetentionPolicy {
    fn default() -> Self {
        Self {
            live_epoch_max_commits: 256,
            recovery_checkpoints_keep: 16,
        }
    }
}

impl SyncRetentionPolicy {
    fn validate(&self) -> Result<(), AppError> {
        if self.live_epoch_max_commits == 0 || self.live_epoch_max_commits > MAX_RETENTION_BOUND {
            return Err(AppError::operation(format!(
                "live epoch commit limit must be between 1 and {MAX_RETENTION_BOUND}"
            )));
        }
        if self.recovery_checkpoints_keep > MAX_RETENTION_BOUND {
            return Err(AppError::operation(format!(
                "recovery checkpoint retention must not exceed {MAX_RETENTION_BOUND}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRetentionPlanOptions {
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub policy: SyncRetentionPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncRetentionEpochPlan {
    pub max_commits: usize,
    pub observed_commits: usize,
    pub observation_truncated: bool,
    pub rollover_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncRetentionRefPlan {
    pub reference: GitRefName,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncRetentionCheckpointPlan {
    pub keep: usize,
    pub retained: Vec<SyncRetentionRefPlan>,
    pub expirable: Vec<SyncRetentionRefPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncRetentionPlanReport {
    pub version: u32,
    pub vault: PathBuf,
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub accepted_revision: String,
    pub policy: SyncRetentionPolicy,
    pub active_epoch: SyncRetentionEpochPlan,
    pub recovery_checkpoints: SyncRetentionCheckpointPlan,
    pub permanent_semantic_checkpoints: Vec<SyncRetentionRefPlan>,
    pub retained_epoch_archives: Vec<SyncRetentionRefPlan>,
    pub mutation_free: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncRetentionApplyReport {
    pub version: u32,
    pub dry_run: bool,
    pub plan: SyncRetentionPlanReport,
    pub released_recovery_checkpoints: Vec<SyncRetentionRefPlan>,
    pub epoch_rollover_applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epoch_rollover: Option<SyncEpochRolloverReport>,
    pub semantic_refs_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncEpochRolloverReport {
    pub epoch_id: String,
    pub previous_revision: String,
    pub root_revision: String,
    pub local_archive_ref: GitRefName,
    pub remote_archive_ref: GitRefName,
    pub tree_unchanged: bool,
}

/// Builds a bounded, mutation-free retention plan for one synchronized vault.
pub fn plan_sync_retention(
    paths: &VaultPaths,
    options: &SyncRetentionPlanOptions,
) -> Result<SyncRetentionPlanReport, AppError> {
    options.policy.validate()?;
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let refs = GitSyncRefs::for_options(&GitSyncOptions {
        remote: options.remote.clone(),
        live_ref: options.live_ref.clone(),
        ..GitSyncOptions::default()
    })
    .map_err(AppError::operation)?;
    let accepted = accepted_revision(&engine, &repository, &refs)?;
    let remote = engine
        .remote_ref(&repository, &options.remote, &options.live_ref)
        .map_err(AppError::operation)?;
    if remote.as_ref() != Some(&accepted) {
        return Err(AppError::operation(
            "the remote live ref does not match locally accepted sync refs; synchronize before planning retention",
        ));
    }

    let observed = engine
        .first_parent_history(
            &repository,
            &accepted,
            options.policy.live_epoch_max_commits + 1,
        )
        .map_err(AppError::operation)?;
    let observation_truncated = observed.len() > options.policy.live_epoch_max_commits;
    let recovery = engine
        .list_refs(
            &repository,
            &GitRefName::parse("refs/vulcan/checkpoints/recovery").map_err(AppError::operation)?,
        )
        .map_err(AppError::operation)?;
    let semantic = engine
        .list_refs(
            &repository,
            &GitRefName::parse("refs/vulcan/checkpoints/semantic").map_err(AppError::operation)?,
        )
        .map_err(AppError::operation)?;
    let epochs = engine
        .list_refs(
            &repository,
            &GitRefName::parse("refs/vulcan/epochs/live").map_err(AppError::operation)?,
        )
        .map_err(AppError::operation)?;
    let (retained, expirable) =
        partition_recovery_refs(recovery, options.policy.recovery_checkpoints_keep);

    Ok(SyncRetentionPlanReport {
        version: SYNC_RETENTION_PLAN_VERSION,
        vault,
        remote: options.remote.clone(),
        live_ref: options.live_ref.clone(),
        accepted_revision: accepted.to_string(),
        policy: options.policy.clone(),
        active_epoch: SyncRetentionEpochPlan {
            max_commits: options.policy.live_epoch_max_commits,
            observed_commits: observed.len(),
            observation_truncated,
            rollover_required: observation_truncated,
        },
        recovery_checkpoints: SyncRetentionCheckpointPlan {
            keep: options.policy.recovery_checkpoints_keep,
            retained: retained.into_iter().map(ref_plan).collect(),
            expirable: expirable.into_iter().map(ref_plan).collect(),
        },
        permanent_semantic_checkpoints: semantic.into_iter().map(ref_plan).collect(),
        retained_epoch_archives: epochs.into_iter().map(ref_plan).collect(),
        mutation_free: true,
    })
}

/// Applies only the checkpoint-expiry portion of a retention plan.
///
/// Every deletion uses the object ID observed by a freshly recomputed plan as
/// its lease. A partial interruption is safe to retry: already deleted refs no
/// longer appear in the next plan. Live epoch and semantic refs are untouched.
pub fn apply_sync_retention(
    paths: &VaultPaths,
    options: &SyncRetentionPlanOptions,
    dry_run: bool,
    rollover: bool,
) -> Result<SyncRetentionApplyReport, AppError> {
    if dry_run {
        return Ok(retention_apply_report(
            true,
            plan_sync_retention(paths, options)?,
            Vec::new(),
            None,
        ));
    }
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let _lock = RetentionLock::acquire(&repository)?;
    let plan = plan_sync_retention(paths, options)?;
    let epoch_rollover = if rollover && plan.active_epoch.rollover_required {
        Some(rollover_live_epoch(&engine, &repository, options, &plan)?)
    } else {
        None
    };
    let mut deleted = Vec::new();
    for candidate in &plan.recovery_checkpoints.expirable {
        let expected = GitOid::parse(&candidate.revision).map_err(AppError::operation)?;
        match engine
            .delete_ref(&repository, &candidate.reference, &expected)
            .map_err(AppError::operation)?
        {
            GitRefDeleteResult::Deleted | GitRefDeleteResult::Missing => {
                deleted.push(candidate.clone());
            }
            GitRefDeleteResult::Stale => {
                return Err(AppError::operation(format!(
                    "recovery checkpoint {} moved while retention was being applied; rerun retention-plan",
                    candidate.reference
                )))
            }
        }
    }
    Ok(retention_apply_report(false, plan, deleted, epoch_rollover))
}

fn retention_apply_report(
    dry_run: bool,
    plan: SyncRetentionPlanReport,
    released_recovery_checkpoints: Vec<SyncRetentionRefPlan>,
    epoch_rollover: Option<SyncEpochRolloverReport>,
) -> SyncRetentionApplyReport {
    SyncRetentionApplyReport {
        version: SYNC_RETENTION_PLAN_VERSION,
        dry_run,
        plan,
        released_recovery_checkpoints,
        epoch_rollover_applied: epoch_rollover.is_some(),
        epoch_rollover,
        semantic_refs_changed: false,
    }
}

fn rollover_live_epoch(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    options: &SyncRetentionPlanOptions,
    plan: &SyncRetentionPlanReport,
) -> Result<SyncEpochRolloverReport, AppError> {
    let previous = GitOid::parse(&plan.accepted_revision).map_err(AppError::operation)?;
    let previous_tree = engine
        .tree_oid(repository, &previous)
        .map_err(AppError::operation)?;
    if engine
        .snapshot_worktree_tree(repository, Some(&previous))
        .map_err(AppError::operation)?
        != previous_tree
    {
        return Err(AppError::operation(
            "the worktree differs from the accepted live tree; synchronize before rolling over the epoch",
        ));
    }
    let safety = engine
        .safety_state(repository)
        .map_err(AppError::operation)?;
    if safety.staged_changes || safety.operation.is_some() {
        return Err(AppError::operation(
            "cannot roll over a live epoch while staged changes or a Git operation are present",
        ));
    }
    let refs = GitSyncRefs::for_options(&GitSyncOptions {
        remote: options.remote.clone(),
        live_ref: options.live_ref.clone(),
        ..GitSyncOptions::default()
    })
    .map_err(AppError::operation)?;
    let profile = refs
        .local
        .as_str()
        .split('/')
        .nth(3)
        .ok_or_else(|| AppError::operation("sync profile ref has no profile component"))?;
    let epoch_id = git_live_epoch_id(profile, &previous);
    let local_archive_ref =
        GitRefName::parse(format!("refs/vulcan/epochs/live/{profile}/{epoch_id}"))
            .map_err(AppError::operation)?;
    let remote_archive_ref = GitRefName::parse(format!(
        "refs/heads/__vulcan-sync/epochs/{profile}/{epoch_id}"
    ))
    .map_err(AppError::operation)?;
    ensure_epoch_archive(
        engine,
        repository,
        &options.remote,
        &previous,
        &local_archive_ref,
        &remote_archive_ref,
    )?;
    let root = engine
        .create_reproducible_commit(
            repository,
            &previous_tree,
            &[],
            &format!(
                "vulcan live epoch root\n\nVulcan-Sync-Version: 1\nVulcan-Sync-Epoch: {epoch_id}\nVulcan-Sync-Previous-Epoch: {previous}\nVulcan-Sync-Epoch-Archive: {remote_archive_ref}\nVulcan-Sync-Profile: {profile}\nVulcan-Sync-Semantic: false\n"
            ),
        )
        .map_err(AppError::operation)?;
    publish_epoch_root(engine, repository, options, &previous, &root)?;
    engine
        .update_ref(repository, &refs.local, &root)
        .map_err(AppError::operation)?;
    engine
        .update_ref(repository, &refs.fetched, &root)
        .map_err(AppError::operation)?;
    engine
        .update_ref(repository, &refs.pending, &root)
        .map_err(AppError::operation)?;
    Ok(SyncEpochRolloverReport {
        epoch_id,
        previous_revision: previous.to_string(),
        root_revision: root.to_string(),
        local_archive_ref,
        remote_archive_ref,
        tree_unchanged: true,
    })
}

fn ensure_epoch_archive(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    remote: &GitRemote,
    previous: &GitOid,
    local_archive_ref: &GitRefName,
    remote_archive_ref: &GitRefName,
) -> Result<(), AppError> {
    match engine
        .create_ref(repository, local_archive_ref, previous)
        .map_err(AppError::operation)?
    {
        GitRefCreateResult::Created => {}
        GitRefCreateResult::Exists => {
            if engine
                .read_ref(repository, local_archive_ref)
                .map_err(AppError::operation)?
                .as_ref()
                != Some(previous)
            {
                return Err(AppError::operation(format!(
                    "epoch archive ref {local_archive_ref} does not identify the expected previous live tip"
                )));
            }
        }
    }
    match engine
        .remote_ref(repository, remote, remote_archive_ref)
        .map_err(AppError::operation)?
    {
        Some(current) if current != *previous => {
            return Err(AppError::operation(format!(
                "remote epoch archive {remote_archive_ref} identifies an unexpected object"
            )))
        }
        Some(_) => {}
        None => {
            if engine
                .push_ref(repository, remote, previous, remote_archive_ref, None)
                .map_err(AppError::operation)?
                != GitPushResult::Updated
            {
                return Err(AppError::operation(
                    "remote epoch archive creation was rejected; the live ref was not changed",
                ));
            }
        }
    }
    Ok(())
}

fn publish_epoch_root(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    options: &SyncRetentionPlanOptions,
    previous: &GitOid,
    root: &GitOid,
) -> Result<(), AppError> {
    match engine
        .push_ref(
            repository,
            &options.remote,
            root,
            &options.live_ref,
            Some(previous),
        )
        .map_err(AppError::operation)?
    {
        GitPushResult::Updated => {}
        GitPushResult::Rejected => {
            if engine
                .remote_ref(repository, &options.remote, &options.live_ref)
                .map_err(AppError::operation)?
                .as_ref()
                != Some(root)
            {
                return Err(AppError::operation(
                    "live ref changed while the epoch rollover lease was being applied",
                ));
            }
        }
    }
    Ok(())
}

fn accepted_revision(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    refs: &GitSyncRefs,
) -> Result<GitOid, AppError> {
    let local = engine
        .read_ref(repository, &refs.local)
        .map_err(AppError::operation)?;
    let fetched = engine
        .read_ref(repository, &refs.fetched)
        .map_err(AppError::operation)?;
    let pending = engine
        .read_ref(repository, &refs.pending)
        .map_err(AppError::operation)?;
    match (local, fetched, pending) {
        (Some(local), Some(fetched), Some(pending)) if local == fetched && local == pending => {
            Ok(local)
        }
        _ => Err(AppError::operation(
            "local, fetched, and pending sync refs do not identify one accepted revision",
        )),
    }
}

fn partition_recovery_refs(
    mut references: Vec<GitReference>,
    keep: usize,
) -> (Vec<GitReference>, Vec<GitReference>) {
    references.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
    let retained_at = references.len().saturating_sub(keep);
    let retained = references.split_off(retained_at);
    (retained, references)
}

fn ref_plan(reference: GitReference) -> SyncRetentionRefPlan {
    SyncRetentionRefPlan {
        reference: reference.name,
        revision: reference.target.to_string(),
    }
}

struct RetentionLock {
    _file: File,
}

impl RetentionLock {
    fn acquire(repository: &vulcan_sync::GitRepository) -> Result<Self, AppError> {
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
    use super::partition_recovery_refs;
    use vulcan_sync::{GitOid, GitRefName, GitReference};

    fn reference(name: &str, oid: &str) -> GitReference {
        GitReference {
            name: GitRefName::parse(name).expect("ref"),
            target: GitOid::parse(oid).expect("OID"),
        }
    }

    #[test]
    fn recovery_partition_keeps_newest_refs_and_expires_oldest() {
        let oid = "0123456789012345678901234567890123456789";
        let (retained, expirable) = partition_recovery_refs(
            vec![
                reference("refs/vulcan/checkpoints/recovery/03", oid),
                reference("refs/vulcan/checkpoints/recovery/01", oid),
                reference("refs/vulcan/checkpoints/recovery/02", oid),
            ],
            2,
        );
        assert_eq!(
            retained
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            [
                "refs/vulcan/checkpoints/recovery/02",
                "refs/vulcan/checkpoints/recovery/03"
            ]
        );
        assert_eq!(
            expirable[0].name.as_str(),
            "refs/vulcan/checkpoints/recovery/01"
        );
    }
}
