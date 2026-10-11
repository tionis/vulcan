//! Periodic cleanup of Vulcan's own refs that no longer protect anything.
//!
//! Sync maintenance runs the sweep at most once per [`REF_SWEEP_INTERVAL`] per
//! repository. With one remote listing it finds device backups of devices that
//! are no longer registered, fetches only those, and deletes the ones live
//! already contains with an exact lease each in one push. It then releases
//! local device recovery refs whose remote backup is gone and whose history
//! live contains, and drops remote-tracking copies of Vulcan refs the remote
//! no longer has. Backups with content live lacks are kept and recorded for
//! `sync doctor`.

use crate::durable_file;
use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vulcan_sync::{
    sync_profile_key, GitEngine, GitOid, GitRefDeleteResult, GitRefName, GitRepository,
    GitSyncOptions, LOCAL_VULCAN_REF_ROOT, REMOTE_DEVICE_BRANCH_ROOT,
    REMOTE_REGISTRATION_BRANCH_ROOT,
};

/// The sweep needs a remote listing and possibly a fetch and a push, so it
/// runs at most this often per repository.
pub const REF_SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Device-local record of the last sweep, beside the repository's sync state.
pub(crate) const REF_SWEEP_STATE_FILE: &str = "ref-sweep";
const VULCAN_REMOTE_ROOT: &str = "refs/heads/__vulcan-sync";

/// What one sweep did, and what it kept because live lacks its content.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefSweepReport {
    pub swept_at_unix_ms: u64,
    /// Device IDs whose remote backups were deleted.
    pub pruned_device_backups: Vec<String>,
    /// Unregistered devices whose backups hold content live does not have.
    pub retained_device_backups: Vec<String>,
    pub released_recovery_refs: usize,
    pub pruned_tracking_refs: usize,
}

/// Whether the last sweep recorded at `state` is older than the interval.
pub(crate) fn sweep_due(state: &Path) -> bool {
    fs::metadata(state)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_none_or(|age| age >= REF_SWEEP_INTERVAL)
}

/// The last sweep's report, for diagnostics.
#[must_use]
pub fn last_sweep(state: &Path) -> Option<RefSweepReport> {
    serde_json::from_slice(&fs::read(state).ok()?).ok()
}

/// Runs one sweep against `live` (the accepted live revision) and records it
/// at `state`. The caller holds the repository lock.
pub(crate) fn sweep_vulcan_refs(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    options: &GitSyncOptions,
    live: &GitOid,
    state: &Path,
) -> Result<RefSweepReport, AppError> {
    let remote = &options.remote;
    let root = GitRefName::parse(VULCAN_REMOTE_ROOT).map_err(AppError::operation)?;
    let listed = engine
        .list_remote_refs(repository, remote, &root)
        .map_err(AppError::operation)?;
    let registration_prefix = format!("{REMOTE_REGISTRATION_BRANCH_ROOT}/");
    let registered = listed
        .iter()
        .filter_map(|reference| reference.name.as_str().strip_prefix(&registration_prefix))
        .collect::<BTreeSet<_>>();
    let profile = sync_profile_key(remote, &options.live_ref);
    let device_prefix = format!("{REMOTE_DEVICE_BRANCH_ROOT}/{profile}/");
    let candidates = listed
        .iter()
        .filter_map(|reference| {
            let device = reference.name.as_str().strip_prefix(&device_prefix)?;
            (!registered.contains(device) && device != options.device_id.as_str())
                .then_some((device.to_string(), reference))
        })
        .collect::<Vec<_>>();

    let mut report = RefSweepReport::default();
    let mut pruned_remote = BTreeSet::new();
    if !candidates.is_empty() {
        let sweep_root = format!("{LOCAL_VULCAN_REF_ROOT}/sweep");
        let local = |device: &str| GitRefName::parse(format!("{sweep_root}/{device}"));
        let mappings = candidates
            .iter()
            .map(|(device, reference)| Ok((reference.name.clone(), local(device)?)))
            .collect::<Result<Vec<_>, vulcan_sync::GitEngineError>>()
            .map_err(AppError::operation)?;
        engine
            .fetch_refs(repository, remote, &mappings)
            .map_err(AppError::operation)?;
        let sweep_prefix = GitRefName::parse(&sweep_root).map_err(AppError::operation)?;
        let contained = engine
            .list_refs_contained_in(repository, &sweep_prefix, live)
            .map_err(AppError::operation)?
            .into_iter()
            .map(|reference| reference.name)
            .collect::<BTreeSet<_>>();
        let live_tree = engine
            .tree_oid(repository, live)
            .map_err(AppError::operation)?;
        let mut deletions = Vec::new();
        for ((device, reference), (_, local)) in candidates.iter().zip(&mappings) {
            // Same tree as live: nothing to lose even off live's history.
            let safe = contained.contains(local)
                || engine.tree_oid(repository, &reference.target).ok().as_ref() == Some(&live_tree);
            if safe {
                deletions.push((reference.name.clone(), reference.target.clone()));
            } else {
                report.retained_device_backups.push(device.clone());
            }
        }
        let results = engine
            .delete_remote_refs(repository, remote, &deletions)
            .map_err(AppError::operation)?;
        for ((name, _), result) in deletions.iter().zip(results) {
            if result == GitRefDeleteResult::Deleted {
                pruned_remote.insert(name.as_str().to_string());
                if let Some(device) = name.as_str().strip_prefix(&device_prefix) {
                    report.pruned_device_backups.push(device.to_string());
                }
            }
        }
        for reference in engine
            .list_refs(repository, &sweep_prefix)
            .map_err(AppError::operation)?
        {
            engine
                .delete_ref(repository, &reference.name, &reference.target)
                .map_err(AppError::operation)?;
        }
    }
    let remaining = listed
        .iter()
        .map(|reference| reference.name.as_str().to_string())
        .filter(|name| !pruned_remote.contains(name))
        .collect::<BTreeSet<_>>();
    report.released_recovery_refs = release_device_recovery_refs(
        engine,
        repository,
        &profile,
        &device_prefix,
        &remaining,
        live,
    )?;
    report.pruned_tracking_refs =
        prune_tracking_refs(engine, repository, remote.as_str(), &remaining)?;
    report.swept_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        });
    let bytes = serde_json::to_vec_pretty(&report).map_err(AppError::operation)?;
    durable_file::replace(state, &bytes)?;
    Ok(report)
}

/// Releases `refs/vulcan/recovery/devices/<profile>/<device>` refs whose
/// remote backup is gone and whose history live contains, then the profile's
/// live review anchor once no device anchors remain.
fn release_device_recovery_refs(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    profile: &str,
    device_prefix: &str,
    remaining_remote: &BTreeSet<String>,
    live: &GitOid,
) -> Result<usize, AppError> {
    let local_root = format!("{LOCAL_VULCAN_REF_ROOT}/recovery/devices/{profile}");
    let prefix = GitRefName::parse(&local_root).map_err(AppError::operation)?;
    let anchor = format!("{local_root}/live");
    let contained = engine
        .list_refs_contained_in(repository, &prefix, live)
        .map_err(AppError::operation)?
        .into_iter()
        .map(|reference| reference.name.as_str().to_string())
        .collect::<BTreeSet<_>>();
    let mut released = 0;
    let mut devices_left = false;
    let mut anchor_ref = None;
    for reference in engine
        .list_refs(repository, &prefix)
        .map_err(AppError::operation)?
    {
        let name = reference.name.as_str().to_string();
        if name == anchor {
            anchor_ref = Some(reference);
            continue;
        }
        let device = name
            .strip_prefix(&format!("{local_root}/"))
            .unwrap_or_default();
        let backup_gone = !remaining_remote.contains(&format!("{device_prefix}{device}"));
        if backup_gone
            && contained.contains(&name)
            && engine
                .delete_ref(repository, &reference.name, &reference.target)
                .map_err(AppError::operation)?
                == GitRefDeleteResult::Deleted
        {
            released += 1;
        } else {
            devices_left = true;
        }
    }
    if let Some(anchor) = anchor_ref.filter(|_| !devices_left) {
        if engine
            .delete_ref(repository, &anchor.name, &anchor.target)
            .map_err(AppError::operation)?
            == GitRefDeleteResult::Deleted
        {
            released += 1;
        }
    }
    Ok(released)
}

/// Drops remote-tracking copies (`refs/remotes/<remote>/__vulcan-sync/...`)
/// of Vulcan refs the remote no longer has. Git creates them when Vulcan
/// pushes to a remote whose fetch refspec covers `refs/heads/*`, and only a
/// pruning fetch would ever remove them.
fn prune_tracking_refs(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    remote: &str,
    remaining_remote: &BTreeSet<String>,
) -> Result<usize, AppError> {
    let tracking_root = format!("refs/remotes/{remote}/__vulcan-sync");
    let Ok(prefix) = GitRefName::parse(&tracking_root) else {
        return Ok(0);
    };
    let mut pruned = 0;
    for reference in engine
        .list_refs(repository, &prefix)
        .map_err(AppError::operation)?
    {
        let Some(rest) = reference.name.as_str().strip_prefix(&tracking_root) else {
            continue;
        };
        if !remaining_remote.contains(&format!("{VULCAN_REMOTE_ROOT}{rest}"))
            && engine
                .delete_ref(repository, &reference.name, &reference.target)
                .map_err(AppError::operation)?
                == GitRefDeleteResult::Deleted
        {
            pruned += 1;
        }
    }
    Ok(pruned)
}
