//! Deliberate Git-reachable checkpoints for accepted live synchronization state.

use crate::AppError;
use fs2::FileExt;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use ulid::Ulid;
use vulcan_core::VaultPaths;
use vulcan_sync::{
    checkpoint_ref as namespace_checkpoint_ref, GitEngine, GitOid, GitRefCreateResult, GitRefName,
    GitRemote, GitRepository, GitSyncOptions, GitSyncRefs,
};

pub const SYNC_CHECKPOINT_REPORT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncCheckpointKind {
    Recovery,
    Semantic,
}

impl SyncCheckpointKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Recovery => "recovery",
            Self::Semantic => "semantic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncCheckpointOptions {
    pub kind: SyncCheckpointKind,
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncCheckpointReport {
    pub version: u32,
    pub vault: PathBuf,
    pub kind: SyncCheckpointKind,
    pub dry_run: bool,
    pub checkpoint_ref: GitRefName,
    pub revision: String,
}

/// Creates a unique local ref to the accepted live commit without copying objects.
pub fn create_sync_checkpoint(
    paths: &VaultPaths,
    options: &SyncCheckpointOptions,
) -> Result<SyncCheckpointReport, AppError> {
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
            "the remote live ref does not match the locally accepted sync refs; run sync before checkpointing",
        ));
    }

    if options.dry_run {
        return Ok(checkpoint_report(
            vault,
            options,
            checkpoint_ref(options.kind, Ulid::new())?,
            &accepted,
        ));
    }
    let _lock = CheckpointLock::acquire(&repository)?;
    let accepted = accepted_revision(&engine, &repository, &refs)?;
    let remote = engine
        .remote_ref(&repository, &options.remote, &options.live_ref)
        .map_err(AppError::operation)?;
    if remote.as_ref() != Some(&accepted) {
        return Err(AppError::operation(
            "the accepted sync revision changed while preparing the checkpoint",
        ));
    }
    for _ in 0..4 {
        let reference = checkpoint_ref(options.kind, Ulid::new())?;
        if engine
            .create_ref(&repository, &reference, &accepted)
            .map_err(AppError::operation)?
            == GitRefCreateResult::Created
        {
            return Ok(checkpoint_report(vault, options, reference, &accepted));
        }
    }
    Err(AppError::operation(
        "could not allocate a unique sync checkpoint ref after four attempts",
    ))
}

fn accepted_revision(
    engine: &dyn GitEngine,
    repository: &GitRepository,
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
            "local, fetched, and pending sync refs do not identify one accepted revision; run sync before checkpointing",
        )),
    }
}

fn checkpoint_ref(kind: SyncCheckpointKind, id: Ulid) -> Result<GitRefName, AppError> {
    namespace_checkpoint_ref(kind.as_str(), &id.to_string().to_ascii_lowercase())
        .map_err(AppError::operation)
}

fn checkpoint_report(
    vault: PathBuf,
    options: &SyncCheckpointOptions,
    checkpoint_ref: GitRefName,
    revision: &GitOid,
) -> SyncCheckpointReport {
    SyncCheckpointReport {
        version: SYNC_CHECKPOINT_REPORT_VERSION,
        vault,
        kind: options.kind,
        dry_run: options.dry_run,
        checkpoint_ref,
        revision: revision.to_string(),
    }
}

struct CheckpointLock {
    _file: File,
}

impl CheckpointLock {
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
            if error.kind() == fs2::lock_contended_error().kind() {
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
    use super::{checkpoint_ref, SyncCheckpointKind};
    use ulid::Ulid;

    #[test]
    fn checkpoint_refs_are_kind_scoped_and_valid() {
        let id = Ulid::from_string("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("ULID");
        assert_eq!(
            checkpoint_ref(SyncCheckpointKind::Recovery, id)
                .expect("checkpoint ref")
                .as_str(),
            "refs/vulcan/checkpoints/recovery/01arz3ndektsv4rrffq69g5fav"
        );
    }
}
