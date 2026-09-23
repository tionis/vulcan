//! Inspection, recovery, and safe retirement of remote per-device sync backups.

use crate::durable_file;
use crate::sync_state::SyncStateStore;
use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use vulcan_core::VaultPaths;
use vulcan_sync::{
    device_recovery_live_ref, device_recovery_ref, sync_profile_key, GitEngine, GitRefDeleteResult,
    GitRefName, GitReference, GitRemote, GitSyncDeviceId, GitSyncOptions, RepositoryLock,
    REMOTE_DEVICE_BRANCH_ROOT,
};

pub const SYNC_DEVICE_REPORT_VERSION: u32 = 1;
const DEVICE_NAME_VERSION: u32 = 1;
const MAX_DEVICE_NAME_BYTES: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceNameRecord {
    version: u32,
    device_id: String,
    name: String,
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub remote_ref: GitRefName,
    pub revision: String,
    pub current_device: bool,
    pub recovery_ref: GitRefName,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_revision: Option<String>,
    pub recovery_status: SyncDeviceRecoveryStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDeviceRecoveryStatus {
    NotFetched,
    Current,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncDeviceLocalRecoverySummary {
    pub device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub recovery_ref: GitRefName,
    pub revision: String,
    pub current_device: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncDeviceNamedSummary {
    pub device_id: String,
    pub name: String,
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
    pub retained_recovery: Vec<SyncDeviceLocalRecoverySummary>,
    pub named_without_backup: Vec<SyncDeviceNamedSummary>,
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
    let recovery_prefix = GitRefName::parse(format!("refs/vulcan/recovery/devices/{profile}"))
        .map_err(AppError::operation)?;
    let recovery_namespace = format!("{}/", recovery_prefix.as_str());
    let mut recovery_refs: BTreeMap<String, GitReference> = engine
        .list_refs(&repository, &recovery_prefix)
        .map_err(AppError::operation)?
        .into_iter()
        .filter_map(|reference| {
            let id = reference
                .name
                .as_str()
                .strip_prefix(&recovery_namespace)?
                .to_string();
            (id != "live").then_some((id, reference))
        })
        .collect();
    let mut backups = Vec::with_capacity(references.len());
    let mut seen = BTreeSet::new();
    for reference in references {
        let device_id = reference
            .name
            .as_str()
            .strip_prefix(&format!("{}/", prefix.as_str()))
            .ok_or_else(|| AppError::operation("remote returned an out-of-namespace device ref"))?;
        let parsed = GitSyncDeviceId::parse(device_id).map_err(AppError::operation)?;
        seen.insert(parsed.as_str().to_string());
        let recovery_ref =
            device_recovery_ref(&profile, parsed.as_str()).map_err(AppError::operation)?;
        let recovery_revision = recovery_refs
            .remove(parsed.as_str())
            .map(|reference| reference.target.to_string());
        let recovery_status = match recovery_revision.as_deref() {
            None => SyncDeviceRecoveryStatus::NotFetched,
            Some(revision) if revision == reference.target.as_str() => {
                SyncDeviceRecoveryStatus::Current
            }
            Some(_) => SyncDeviceRecoveryStatus::Stale,
        };
        backups.push(SyncDeviceBackupSummary {
            device_id: parsed.as_str().to_string(),
            name: read_device_name(&vault, &parsed)?,
            remote_ref: reference.name,
            revision: reference.target.to_string(),
            current_device: current.as_ref() == Some(&parsed),
            recovery_ref,
            recovery_revision,
            recovery_status,
        });
    }
    let mut retained_recovery = Vec::new();
    for (device_id, reference) in recovery_refs {
        let parsed = GitSyncDeviceId::parse(device_id).map_err(AppError::operation)?;
        seen.insert(parsed.as_str().to_string());
        retained_recovery.push(SyncDeviceLocalRecoverySummary {
            device_id: parsed.as_str().to_string(),
            name: read_device_name(&vault, &parsed)?,
            recovery_ref: reference.name,
            revision: reference.target.to_string(),
            current_device: current.as_ref() == Some(&parsed),
        });
    }
    let named_without_backup = list_device_names(&vault)?
        .into_iter()
        .filter(|(id, _)| !seen.contains(id))
        .map(|(device_id, name)| SyncDeviceNamedSummary {
            current_device: current.as_ref().is_some_and(|id| id.as_str() == device_id),
            device_id,
            name,
        })
        .collect();
    Ok(SyncDeviceListReport {
        version: SYNC_DEVICE_REPORT_VERSION,
        vault,
        remote: options.remote.clone(),
        live_ref: options.live_ref.clone(),
        profile,
        current_device_id: current.map(|id| id.as_str().to_string()),
        count: backups.len(),
        backups,
        retained_recovery,
        named_without_backup,
    })
}

/// Store a display-only label in the shared vault. The device ID remains the
/// stable identity used by refs and recovery commands.
pub fn set_sync_device_name(
    paths: &VaultPaths,
    device_id: &str,
    name: Option<&str>,
    dry_run: bool,
) -> Result<(), AppError> {
    let device_id = GitSyncDeviceId::parse(device_id).map_err(AppError::operation)?;
    if let Some(name) = name {
        validate_device_name(name)?;
    }
    check_device_name_directory(paths.vault_root())?;
    let path = device_name_path(paths.vault_root(), &device_id);
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(AppError::operation("device name path is a symlink"));
    }
    if dry_run {
        return Ok(());
    }
    if let Some(name) = name {
        fs::create_dir_all(path.parent().expect("device name path has parent"))
            .map_err(AppError::operation)?;
        let record = DeviceNameRecord {
            version: DEVICE_NAME_VERSION,
            device_id: device_id.as_str().to_string(),
            name: name.to_string(),
        };
        let mut bytes = serde_json::to_vec_pretty(&record).map_err(AppError::operation)?;
        bytes.push(b'\n');
        durable_file::replace(&path, &bytes)
    } else {
        if path.exists() {
            durable_file::remove(&path)?;
        }
        Ok(())
    }
}

fn device_name_path(vault: &Path, device_id: &GitSyncDeviceId) -> PathBuf {
    vault
        .join(".vulcan/device-names")
        .join(format!("{}.json", device_id.as_str()))
}

fn check_device_name_directory(vault: &Path) -> Result<(), AppError> {
    for directory in [vault.join(".vulcan"), vault.join(".vulcan/device-names")] {
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(AppError::operation(format!(
                    "device name directory is not a regular directory: {}",
                    directory.display()
                )))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(AppError::operation(error)),
        }
    }
    Ok(())
}

fn validate_device_name(name: &str) -> Result<(), AppError> {
    if name.trim() != name
        || name.is_empty()
        || name.len() > MAX_DEVICE_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        return Err(AppError::operation(
            "device name must be 1–80 UTF-8 bytes with no surrounding whitespace or control characters",
        ));
    }
    Ok(())
}

fn read_device_name(vault: &Path, device_id: &GitSyncDeviceId) -> Result<Option<String>, AppError> {
    check_device_name_directory(vault)?;
    let path = device_name_path(vault, device_id);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::operation(error)),
    };
    if !metadata.is_file() || metadata.len() > 512 {
        return Err(AppError::operation(format!(
            "invalid device name file at {}",
            path.display()
        )));
    }
    let record: DeviceNameRecord =
        serde_json::from_slice(&fs::read(&path).map_err(AppError::operation)?)
            .map_err(AppError::operation)?;
    if record.version != DEVICE_NAME_VERSION || record.device_id != device_id.as_str() {
        return Err(AppError::operation(format!(
            "invalid device name record at {}",
            path.display()
        )));
    }
    validate_device_name(&record.name)?;
    Ok(Some(record.name))
}

fn list_device_names(vault: &Path) -> Result<Vec<(String, String)>, AppError> {
    check_device_name_directory(vault)?;
    let directory = vault.join(".vulcan/device-names");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AppError::operation(error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(AppError::operation)?;
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let device_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| AppError::operation("invalid device name filename"))?;
        let parsed = GitSyncDeviceId::parse(device_id).map_err(AppError::operation)?;
        let name = read_device_name(vault, &parsed)?
            .ok_or_else(|| AppError::operation("device name file disappeared while listing"))?;
        names.push((parsed.as_str().to_string(), name));
    }
    names.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(names)
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
        set_sync_device_name, SyncDeviceOptions, SyncDeviceRecoveryStatus, SyncDeviceRelation,
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

    fn check_name_inventory(paths: &VaultPaths, options: &SyncDeviceOptions, vault: &Path) {
        set_sync_device_name(paths, DEVICE_A, Some("Living room laptop"), true)
            .expect("preview name");
        assert!(!vault
            .join(format!(".vulcan/device-names/{DEVICE_A}.json"))
            .exists());
        set_sync_device_name(paths, DEVICE_A, Some("Living room laptop"), false).expect("set name");
        assert_eq!(
            list_sync_device_backups(paths, options)
                .expect("list named backup")
                .backups[0]
                .name
                .as_deref(),
            Some("Living room laptop")
        );
        assert!(set_sync_device_name(paths, DEVICE_A, Some("bad\nname"), false).is_err());
        set_sync_device_name(paths, DEVICE_A, None, false).expect("clear name");
        assert_eq!(
            list_sync_device_backups(paths, options)
                .expect("list unnamed backup")
                .backups[0]
                .name,
            None
        );
    }

    fn check_stale_recovery_inventory(
        paths: &VaultPaths,
        options: &SyncDeviceOptions,
        vault: &Path,
        base: &str,
        live: &str,
        integrated_ref: &str,
    ) {
        let fetched_list = list_sync_device_backups(paths, options).expect("list fetched backup");
        assert_eq!(
            fetched_list.backups[0].recovery_status,
            SyncDeviceRecoveryStatus::Current
        );
        assert_eq!(
            fetched_list.backups[0].recovery_revision.as_deref(),
            Some(base)
        );
        let advanced_refspec = format!("{live}:{integrated_ref}");
        git(vault, &["push", "--quiet", "origin", &advanced_refspec]);
        assert_eq!(
            list_sync_device_backups(paths, options)
                .expect("list stale recovery")
                .backups[0]
                .recovery_status,
            SyncDeviceRecoveryStatus::Stale
        );
        let restored_refspec = format!("+{base}:{integrated_ref}");
        git(vault, &["push", "--quiet", "origin", &restored_refspec]);
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
        assert_eq!(listed.backups[0].name, None);
        assert_eq!(
            listed.backups[0].recovery_status,
            SyncDeviceRecoveryStatus::NotFetched
        );
        check_name_inventory(&paths, &options, &vault);
        let fetched = fetch_sync_device_backup(&paths, &options, DEVICE_A, false)
            .expect("fetch integrated backup");
        assert_eq!(fetched.relation, Some(SyncDeviceRelation::Integrated));
        check_stale_recovery_inventory(&paths, &options, &vault, &base, &live, &integrated_ref);
        let preview = remove_sync_device_backup(&paths, &options, DEVICE_A, true)
            .expect("preview safe removal");
        assert!(!preview.removed);
        assert!(
            remove_sync_device_backup(&paths, &options, DEVICE_A, false)
                .expect("remove integrated backup")
                .removed
        );
        let retained = list_sync_device_backups(&paths, &options).expect("list retained recovery");
        assert_eq!(retained.count, 0);
        assert_eq!(retained.retained_recovery.len(), 1);
        assert_eq!(retained.retained_recovery[0].device_id, DEVICE_A);
        set_sync_device_name(&paths, DEVICE_B, Some("Travel tablet"), false)
            .expect("name unbacked device");
        let named = list_sync_device_backups(&paths, &options).expect("list named-only device");
        assert_eq!(named.named_without_backup.len(), 1);
        assert_eq!(named.named_without_backup[0].name, "Travel tablet");

        git(&vault, &["checkout", "--quiet", "--detach", &base]);
        fs::write(vault.join("device-only.md"), "preserve me\n").expect("device note");
        git(&vault, &["add", "device-only.md"]);
        git(&vault, &["commit", "--quiet", "-m", "device-only"]);
        let divergent = git(&vault, &["rev-parse", "HEAD"]);
        let divergent_ref = format!("refs/heads/__vulcan-sync/devices/{profile}/{DEVICE_B}");
        let divergent_refspec = format!("{divergent}:{divergent_ref}");
        git(&vault, &["push", "--quiet", "origin", &divergent_refspec]);
        let listed = list_sync_device_backups(&paths, &options).expect("list new backup");
        assert!(listed.named_without_backup.is_empty());
        assert_eq!(listed.backups[0].name.as_deref(), Some("Travel tablet"));
        let fetched = fetch_sync_device_backup(&paths, &options, DEVICE_B, false)
            .expect("fetch divergent backup");
        assert_eq!(fetched.relation, Some(SyncDeviceRelation::Diverged));
        let error = remove_sync_device_backup(&paths, &options, DEVICE_B, true)
            .expect_err("unintegrated backup must be retained");
        assert!(error.to_string().contains("unintegrated information"));
    }
}
