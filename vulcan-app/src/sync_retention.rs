//! Read-only planning for Git live-history retention epochs.
//!
//! Planning is deliberately separate from rollover and expiry application.
//! A checkpoint ref can be classified as expirable, but no ref is deleted and
//! no canonical live tip is rewritten by this module.

use crate::AppError;
use fs2::FileExt;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use vulcan_core::VaultPaths;
use vulcan_sync::{
    GitEngine, GitOid, GitRefDeleteResult, GitRefName, GitReference, GitRemote, GitSyncOptions,
    GitSyncRefs,
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
    pub semantic_refs_changed: bool,
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
) -> Result<SyncRetentionApplyReport, AppError> {
    if dry_run {
        return Ok(retention_apply_report(
            true,
            plan_sync_retention(paths, options)?,
            Vec::new(),
        ));
    }
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let _lock = RetentionLock::acquire(&repository)?;
    let plan = plan_sync_retention(paths, options)?;
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
    Ok(retention_apply_report(false, plan, deleted))
}

fn retention_apply_report(
    dry_run: bool,
    plan: SyncRetentionPlanReport,
    released_recovery_checkpoints: Vec<SyncRetentionRefPlan>,
) -> SyncRetentionApplyReport {
    SyncRetentionApplyReport {
        version: SYNC_RETENTION_PLAN_VERSION,
        dry_run,
        plan,
        released_recovery_checkpoints,
        epoch_rollover_applied: false,
        semantic_refs_changed: false,
    }
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
