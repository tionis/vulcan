//! Inspection, recovery, and safe retirement of remote per-device sync backups.

use crate::sync_state::SyncStateStore;
use crate::AppError;
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use vulcan_core::VaultPaths;
use vulcan_sync::{
    device_recovery_live_ref, device_recovery_ref, sync_profile_key, GitEngine, GitRefDeleteResult,
    GitRefName, GitRemote, GitSyncDeviceId, GitSyncOptions, RepositoryLock,
    REMOTE_DEVICE_BRANCH_ROOT,
};

pub const SYNC_DEVICE_REPORT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncDeviceOptions {
    pub remote: GitRemote,
    pub live_ref: GitRefName,
}

impl From<&GitSyncOptions> for SyncDeviceOptions {
    fn from(options: &GitSyncOptions) -> Self {
        Self {
            remote: options.remote.clone(),
            live_ref: options.live_ref.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncDeviceBackupSummary {
    pub device_id: String,
    pub remote_ref: GitRefName,
    pub revision: String,
    pub current_device: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncDeviceListReport {
    pub version: u32,
    pub vault: PathBuf,
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_device_id: Option<String>,
    pub count: usize,
    pub backups: Vec<SyncDeviceBackupSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDeviceRelation {
    LiveUninitialized,
    Same,
    Integrated,
    ContainsLive,
    Diverged,
}

impl SyncDeviceRelation {
    #[must_use]
    pub const fn safe_to_remove(self) -> bool {
        matches!(self, Self::Same | Self::Integrated)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncDeviceFetchReport {
    pub version: u32,
    pub vault: PathBuf,
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub device_id: String,
    pub remote_device_ref: GitRefName,
    pub local_device_ref: GitRefName,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_live_ref: Option<GitRefName>,
    pub revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_revision: Option<String>,
    pub dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<SyncDeviceRelation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncDeviceRemoveReport {
    pub version: u32,
    pub vault: PathBuf,
    pub remote: GitRemote,
    pub device_id: String,
    pub remote_device_ref: GitRefName,
    pub revision: String,
    pub relation: SyncDeviceRelation,
    pub dry_run: bool,
    pub removed: bool,
    pub local_recovery_retained: GitRefName,
}

pub fn list_sync_device_backups(
    paths: &VaultPaths,
    options: &SyncDeviceOptions,
) -> Result<SyncDeviceListReport, AppError> {
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let profile = sync_profile_key(&options.remote, &options.live_ref);
    let prefix = GitRefName::parse(format!("{REMOTE_DEVICE_BRANCH_ROOT}/{profile}"))
        .map_err(AppError::operation)?;
    let current = SyncStateStore::user_default()?.load_or_create_device_id(false)?;
    let references = engine
        .list_remote_refs(&repository, &options.remote, &prefix)
        .map_err(AppError::operation)?;
    let mut backups = Vec::with_capacity(references.len());
    for reference in references {
        let device_id = reference
            .name
            .as_str()
            .strip_prefix(&format!("{}/", prefix.as_str()))
            .ok_or_else(|| AppError::operation("remote returned an out-of-namespace device ref"))?;
        let parsed = GitSyncDeviceId::parse(device_id).map_err(AppError::operation)?;
        backups.push(SyncDeviceBackupSummary {
            device_id: parsed.as_str().to_string(),
            remote_ref: reference.name,
            revision: reference.target.to_string(),
            current_device: current.as_ref() == Some(&parsed),
        });
    }
    Ok(SyncDeviceListReport {
        version: SYNC_DEVICE_REPORT_VERSION,
        vault,
        remote: options.remote.clone(),
        live_ref: options.live_ref.clone(),
        profile,
        current_device_id: current.map(|id| id.as_str().to_string()),
        count: backups.len(),
        backups,
    })
}

pub fn fetch_sync_device_backup(
    paths: &VaultPaths,
    options: &SyncDeviceOptions,
    device_id: &str,
    dry_run: bool,
) -> Result<SyncDeviceFetchReport, AppError> {
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let profile = sync_profile_key(&options.remote, &options.live_ref);
    let device_id = GitSyncDeviceId::parse(device_id).map_err(AppError::operation)?;
    let remote_device_ref = vulcan_sync::remote_device_ref(&profile, device_id.as_str())
        .map_err(AppError::operation)?;
    let local_device_ref =
        device_recovery_ref(&profile, device_id.as_str()).map_err(AppError::operation)?;
    let local_live_ref = device_recovery_live_ref(&profile).map_err(AppError::operation)?;
    let remote_revision = engine
        .remote_ref(&repository, &options.remote, &remote_device_ref)
        .map_err(AppError::operation)?
        .ok_or_else(|| AppError::operation("the remote device backup does not exist"))?;
    let mut live_revision = engine
        .remote_ref(&repository, &options.remote, &options.live_ref)
        .map_err(AppError::operation)?;
    if dry_run {
        return Ok(SyncDeviceFetchReport {
            version: SYNC_DEVICE_REPORT_VERSION,
            vault,
            remote: options.remote.clone(),
            live_ref: options.live_ref.clone(),
            device_id: device_id.as_str().to_string(),
            remote_device_ref,
            local_device_ref,
            local_live_ref: live_revision.as_ref().map(|_| local_live_ref),
            revision: remote_revision.to_string(),
            live_revision: live_revision.map(|oid| oid.to_string()),
            dry_run,
            relation: None,
            changed_paths: None,
        });
    }
    let _lock = RepositoryLock::acquire(&repository.git_dir).map_err(AppError::operation)?;
    let revision = engine
        .fetch_ref(
            &repository,
            &options.remote,
            &remote_device_ref,
            &local_device_ref,
        )
        .map_err(|error| {
            AppError::operation(format!(
                "cannot fetch device backup `{}`: {error}",
                device_id.as_str()
            ))
        })?;
    let (relation, local_live_ref, changed_paths) = if live_revision.is_some() {
        let fetched_live = engine
            .fetch_ref(
                &repository,
                &options.remote,
                &options.live_ref,
                &local_live_ref,
            )
            .map_err(AppError::operation)?;
        live_revision = Some(fetched_live.clone());
        let relation = classify_relation(&engine, &repository, &revision, &fetched_live)?;
        let paths = engine
            .changed_paths(&repository, &fetched_live, &revision)
            .map_err(AppError::operation)?;
        (relation, Some(local_live_ref), paths)
    } else {
        (SyncDeviceRelation::LiveUninitialized, None, Vec::new())
    };
    Ok(SyncDeviceFetchReport {
        version: SYNC_DEVICE_REPORT_VERSION,
        vault,
        remote: options.remote.clone(),
        live_ref: options.live_ref.clone(),
        device_id: device_id.as_str().to_string(),
        remote_device_ref,
        local_device_ref,
        local_live_ref,
        revision: revision.to_string(),
        live_revision: live_revision.map(|oid| oid.to_string()),
        dry_run,
        relation: Some(relation),
        changed_paths: Some(changed_paths),
    })
}

pub fn remove_sync_device_backup(
    paths: &VaultPaths,
    options: &SyncDeviceOptions,
    device_id: &str,
    dry_run: bool,
) -> Result<SyncDeviceRemoveReport, AppError> {
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let profile = sync_profile_key(&options.remote, &options.live_ref);
    let device_id = GitSyncDeviceId::parse(device_id).map_err(AppError::operation)?;
    let current = SyncStateStore::user_default()?.load_or_create_device_id(false)?;
    if current.as_ref() == Some(&device_id) {
        return Err(AppError::operation(
            "cannot remove this device's active backup; retire it from another device after this device stops syncing",
        ));
    }
    let remote_device_ref = vulcan_sync::remote_device_ref(&profile, device_id.as_str())
        .map_err(AppError::operation)?;
    let local_device_ref =
        device_recovery_ref(&profile, device_id.as_str()).map_err(AppError::operation)?;
    let local_live_ref = device_recovery_live_ref(&profile).map_err(AppError::operation)?;
    let candidate = engine
        .remote_ref(&repository, &options.remote, &remote_device_ref)
        .map_err(AppError::operation)?
        .ok_or_else(|| AppError::operation("the remote device backup does not exist"))?;
    let live = engine
        .remote_ref(&repository, &options.remote, &options.live_ref)
        .map_err(AppError::operation)?
        .ok_or_else(|| {
            AppError::operation("remote live is uninitialized; the backup cannot be safely removed")
        })?;
    if engine
        .read_ref(&repository, &local_device_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(&candidate)
        || engine
            .read_ref(&repository, &local_live_ref)
            .map_err(AppError::operation)?
            .as_ref()
            != Some(&live)
    {
        return Err(AppError::operation(format!(
            "recovery refs are missing or stale; run `vulcan sync devices fetch {}` before removal",
            device_id.as_str()
        )));
    }
    let relation = classify_relation(&engine, &repository, &candidate, &live)?;
    let same_tree = engine
        .tree_oid(&repository, &candidate)
        .map_err(AppError::operation)?
        == engine
            .tree_oid(&repository, &live)
            .map_err(AppError::operation)?;
    if !relation.safe_to_remove() && !same_tree {
        return Err(AppError::operation(format!(
            "device backup `{}` is {relation:?} from live and still contains unintegrated information; refusing removal",
            device_id.as_str()
        )));
    }
    let removed = if dry_run {
        false
    } else {
        let _lock = RepositoryLock::acquire(&repository.git_dir).map_err(AppError::operation)?;
        let current_candidate = engine
            .remote_ref(&repository, &options.remote, &remote_device_ref)
            .map_err(AppError::operation)?;
        let current_live = engine
            .remote_ref(&repository, &options.remote, &options.live_ref)
            .map_err(AppError::operation)?;
        if current_candidate.as_ref() != Some(&candidate) || current_live.as_ref() != Some(&live) {
            return Err(AppError::operation(
                "device backup or accepted live changed during removal; fetch again before retrying",
            ));
        }
        match engine
            .delete_remote_ref(&repository, &options.remote, &remote_device_ref, &candidate)
            .map_err(AppError::operation)?
        {
            GitRefDeleteResult::Deleted => true,
            GitRefDeleteResult::Missing => false,
            GitRefDeleteResult::Stale => {
                return Err(AppError::operation(
                    "device backup changed during removal; fetch it again before retrying",
                ));
            }
        }
    };
    Ok(SyncDeviceRemoveReport {
        version: SYNC_DEVICE_REPORT_VERSION,
        vault,
        remote: options.remote.clone(),
        device_id: device_id.as_str().to_string(),
        remote_device_ref,
        revision: candidate.to_string(),
        relation,
        dry_run,
        removed,
        local_recovery_retained: local_device_ref,
    })
}

fn classify_relation(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    candidate: &vulcan_sync::GitOid,
    live: &vulcan_sync::GitOid,
) -> Result<SyncDeviceRelation, AppError> {
    if candidate == live {
        return Ok(SyncDeviceRelation::Same);
    }
    if engine
        .is_ancestor(repository, candidate, live)
        .map_err(AppError::operation)?
    {
        return Ok(SyncDeviceRelation::Integrated);
    }
    if engine
        .is_ancestor(repository, live, candidate)
        .map_err(AppError::operation)?
    {
        return Ok(SyncDeviceRelation::ContainsLive);
    }
    Ok(SyncDeviceRelation::Diverged)
}

#[cfg(test)]
mod tests {
    use super::{
        fetch_sync_device_backup, list_sync_device_backups, remove_sync_device_backup,
        SyncDeviceOptions, SyncDeviceRelation,
    };
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;
    use vulcan_core::VaultPaths;
    use vulcan_sync::{GitRefName, GitRemote, DEFAULT_REMOTE_LIVE_REF};

    const DEVICE_A: &str = "01arz3ndektsv4rrffq69g5fav";
    const DEVICE_B: &str = "01arz3ndektsv4rrffq69g5faw";

    fn git(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(path)
            .args(args)
            .output()
            .expect("git should launch");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output")
            .trim()
            .to_string()
    }

    #[test]
    fn only_integrated_relations_are_safe_to_remove() {
        assert!(SyncDeviceRelation::Same.safe_to_remove());
        assert!(SyncDeviceRelation::Integrated.safe_to_remove());
        assert!(!SyncDeviceRelation::ContainsLive.safe_to_remove());
        assert!(!SyncDeviceRelation::Diverged.safe_to_remove());
        assert!(!SyncDeviceRelation::LiveUninitialized.safe_to_remove());
    }

    #[test]
    fn device_backups_can_be_listed_fetched_and_only_safely_removed() {
        let temporary = TempDir::new().expect("temporary directory");
        let remote = temporary.path().join("remote.git");
        let vault = temporary.path().join("vault");
        fs::create_dir(&remote).expect("remote directory");
        fs::create_dir(&vault).expect("vault directory");
        git(&remote, &["init", "--bare", "--quiet"]);
        git(&vault, &["init", "--quiet"]);
        git(&vault, &["config", "user.name", "Vulcan Test"]);
        git(&vault, &["config", "user.email", "vulcan@example.invalid"]);
        fs::write(vault.join("note.md"), "base\n").expect("base note");
        git(&vault, &["add", "note.md"]);
        git(&vault, &["commit", "--quiet", "-m", "base"]);
        let base = git(&vault, &["rev-parse", "HEAD"]);
        fs::write(vault.join("note.md"), "live\n").expect("live note");
        git(&vault, &["commit", "--quiet", "-am", "live"]);
        let live = git(&vault, &["rev-parse", "HEAD"]);
        let remote_path = remote.to_string_lossy();
        git(&vault, &["remote", "add", "origin", &remote_path]);

        let options = SyncDeviceOptions {
            remote: GitRemote::parse("origin").expect("remote"),
            live_ref: GitRefName::parse(DEFAULT_REMOTE_LIVE_REF).expect("live ref"),
        };
        let profile = vulcan_sync::sync_profile_key(&options.remote, &options.live_ref);
        let integrated_ref = format!("refs/heads/__vulcan-sync/devices/{profile}/{DEVICE_A}");
        let live_refspec = format!("{live}:{}", options.live_ref);
        let integrated_refspec = format!("{base}:{integrated_ref}");
        git(&vault, &["push", "--quiet", "origin", &live_refspec]);
        git(&vault, &["push", "--quiet", "origin", &integrated_refspec]);

        let paths = VaultPaths::new(&vault);
        let listed = list_sync_device_backups(&paths, &options).expect("list backups");
        assert_eq!(listed.count, 1);
        assert_eq!(listed.backups[0].device_id, DEVICE_A);
        let fetched = fetch_sync_device_backup(&paths, &options, DEVICE_A, false)
            .expect("fetch integrated backup");
        assert_eq!(fetched.relation, Some(SyncDeviceRelation::Integrated));
        let preview = remove_sync_device_backup(&paths, &options, DEVICE_A, true)
            .expect("preview safe removal");
        assert!(!preview.removed);
        assert!(
            remove_sync_device_backup(&paths, &options, DEVICE_A, false)
                .expect("remove integrated backup")
                .removed
        );

        git(&vault, &["checkout", "--quiet", "--detach", &base]);
        fs::write(vault.join("device-only.md"), "preserve me\n").expect("device note");
        git(&vault, &["add", "device-only.md"]);
        git(&vault, &["commit", "--quiet", "-m", "device-only"]);
        let divergent = git(&vault, &["rev-parse", "HEAD"]);
        let divergent_ref = format!("refs/heads/__vulcan-sync/devices/{profile}/{DEVICE_B}");
        let divergent_refspec = format!("{divergent}:{divergent_ref}");
        git(&vault, &["push", "--quiet", "origin", &divergent_refspec]);
        let fetched = fetch_sync_device_backup(&paths, &options, DEVICE_B, false)
            .expect("fetch divergent backup");
        assert_eq!(fetched.relation, Some(SyncDeviceRelation::Diverged));
        let error = remove_sync_device_backup(&paths, &options, DEVICE_B, true)
            .expect_err("unintegrated backup must be retained");
        assert!(error.to_string().contains("unintegrated information"));
    }
}
