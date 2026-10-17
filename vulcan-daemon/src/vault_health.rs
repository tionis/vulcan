//! One state per registered vault for `vulcan vault list` and `vault show`,
//! folded from its registration, its sync status (live from a running
//! daemon, otherwise reconstructed from the job ledger, journal, and
//! conflict records), and the daemon's cache state. Every input is durable
//! or optional, so this works without a daemon.

use crate::process::DaemonWikiOperationalStatus;
use crate::registry::WikiRegistrationStatus;
use crate::scan_runtime::CacheFreshnessState;
use serde::Serialize;
use vulcan_sync::SyncState;

/// What most needs attention about a vault, most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultHealthState {
    /// Its directory is gone.
    Missing,
    /// A knowledge vault without a cache; run `vulcan scan`.
    Unindexed,
    /// Preserved sync conflicts wait for resolution.
    Conflicted,
    /// An interrupted sync left state to recover.
    RecoveryRequired,
    /// Automatic sync is paused.
    Paused,
    /// The last sync or the daemon's last scan failed.
    Failing,
    /// The last sync could not reach the remote.
    Offline,
    /// A sync is queued or in progress.
    Syncing,
    Healthy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VaultHealth {
    pub state: VaultHealthState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The sync state behind `state`, absent for a vault without sync.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_state: Option<SyncState>,
    pub unresolved_conflicts: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync_attempt_unix_ms: Option<u64>,
    /// The daemon's view of the cache, absent when it never indexed the
    /// vault or does not index it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheFreshnessState>,
}

/// The health of `wiki`, given its operational status if one was read.
#[must_use]
pub fn vault_health(
    wiki: &WikiRegistrationStatus,
    status: Option<&DaemonWikiOperationalStatus>,
) -> VaultHealth {
    let sync = status.and_then(|status| status.sync.as_ref());
    let cache = status.map(|status| status.cache.state).filter(|state| {
        !matches!(
            state,
            CacheFreshnessState::Disabled | CacheFreshnessState::Unknown
        )
    });
    let mut health = VaultHealth {
        state: VaultHealthState::Healthy,
        detail: None,
        sync_state: sync.map(|sync| sync.status.state),
        unresolved_conflicts: sync.map_or(0, |sync| sync.status.unresolved_conflicts),
        last_sync_attempt_unix_ms: sync.and_then(|sync| sync.last_attempt_unix_ms),
        cache,
    };
    let (state, detail) = if !wiki.available {
        (VaultHealthState::Missing, None)
    } else if wiki.registration.capabilities().markdown_index && !wiki.indexed {
        (
            VaultHealthState::Unindexed,
            Some("no cache yet; run `vulcan scan`".to_string()),
        )
    } else if let Some(sync) = sync {
        let detail = sync.status.detail.clone();
        match sync.status.state {
            _ if health.unresolved_conflicts > 0 => (VaultHealthState::Conflicted, detail),
            SyncState::Conflicted => (VaultHealthState::Conflicted, detail),
            _ if sync.recovery_required => (VaultHealthState::RecoveryRequired, detail),
            SyncState::Paused => (VaultHealthState::Paused, detail),
            SyncState::Error => (VaultHealthState::Failing, detail),
            SyncState::Offline => (VaultHealthState::Offline, detail),
            SyncState::CapturePending
            | SyncState::Capturing
            | SyncState::Fetching
            | SyncState::Merging
            | SyncState::Pushing
            | SyncState::Applying => (VaultHealthState::Syncing, detail),
            SyncState::Clean
            | SyncState::Dirty
            | SyncState::CapturedUnpushed
            | SyncState::Fetched => scan_failure(status),
        }
    } else if let Some(error) = status.and_then(|status| status.sync_error.clone()) {
        (VaultHealthState::Failing, Some(error))
    } else {
        scan_failure(status)
    };
    health.state = state;
    health.detail = detail;
    health
}

/// `Failing` when the daemon's last scan of the vault failed.
fn scan_failure(
    status: Option<&DaemonWikiOperationalStatus>,
) -> (VaultHealthState, Option<String>) {
    match status.map(|status| &status.cache) {
        Some(cache) if cache.state == CacheFreshnessState::Error => {
            (VaultHealthState::Failing, cache.error.clone())
        }
        _ => (VaultHealthState::Healthy, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{DaemonNotificationDiscoveryState, DaemonNotificationDiscoveryStatus};
    use crate::registry::{
        ManagedDirectoryProfile, MaterializationProfile, WikiId, WikiRegistration,
    };
    use crate::scan_runtime::ScanCompletion;
    use crate::status::{DaemonSyncStatusSource, DaemonWikiSyncStatus, DAEMON_SYNC_STATUS_VERSION};
    use vulcan_sync::SyncStatus;

    fn wiki(root: &std::path::Path, profile: ManagedDirectoryProfile) -> WikiRegistrationStatus {
        WikiRegistrationStatus::from_registration(&WikiRegistration {
            profile,
            profile_version: None,
            materialization: MaterializationProfile::Full,
            id: WikiId::parse("notes").unwrap(),
            registration_id: ulid::Ulid::new(),
            path: root.to_path_buf(),
            work_tree: None,
            groups: vec![],
            git_dir: None,
            permissions_profile: None,
            sync_backend: Some("git".to_string()),
            platform_profile: None,
            sync_paused: false,
            execution_trust: None,
        })
    }

    fn operational(
        state: SyncState,
        conflicts: usize,
        recovery_required: bool,
        cache: ScanCompletion,
    ) -> DaemonWikiOperationalStatus {
        DaemonWikiOperationalStatus {
            wiki_id: "notes".to_string(),
            path: std::path::PathBuf::from("/vault"),
            sync: Some(DaemonWikiSyncStatus {
                version: DAEMON_SYNC_STATUS_VERSION,
                wiki_id: "notes".to_string(),
                device_id: None,
                paused: state == SyncState::Paused,
                source: DaemonSyncStatusSource::Job,
                recovery_required,
                last_attempt_unix_ms: Some(42),
                status: SyncStatus {
                    state,
                    backend: "git".to_string(),
                    vault: std::path::PathBuf::from("/vault"),
                    local_revision: None,
                    remote_revision: None,
                    accepted_revision: None,
                    unresolved_conflicts: conflicts,
                    detail: Some(format!("{state:?}")),
                },
                transaction_id: None,
                job: None,
            }),
            sync_error: None,
            cache,
            notification: DaemonNotificationDiscoveryStatus {
                state: DaemonNotificationDiscoveryState::NotDiscovered,
                origin: None,
                fingerprint: None,
                revision: None,
            },
        }
    }

    #[test]
    fn health_names_what_most_needs_attention() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let fresh = || ScanCompletion::unknown();
        // No directory, then no cache, outrank everything else.
        let gone = wiki(&root.join("gone"), ManagedDirectoryProfile::Knowledge);
        let broken = operational(SyncState::Error, 0, false, fresh());
        assert_eq!(
            vault_health(&gone, Some(&broken)).state,
            VaultHealthState::Missing
        );
        let unscanned = wiki(root, ManagedDirectoryProfile::Knowledge);
        assert_eq!(
            vault_health(&unscanned, None).state,
            VaultHealthState::Unindexed
        );
        // A files-only directory has no cache to miss.
        let files = wiki(root, ManagedDirectoryProfile::FilesOnly);
        assert_eq!(vault_health(&files, None).state, VaultHealthState::Healthy);

        std::fs::create_dir_all(root.join(".vulcan")).unwrap();
        std::fs::write(root.join(".vulcan/cache.db"), b"").unwrap();
        let indexed = wiki(root, ManagedDirectoryProfile::Knowledge);
        assert_eq!(
            vault_health(&indexed, None).state,
            VaultHealthState::Healthy
        );
        for (status, expected) in [
            (
                operational(SyncState::Clean, 2, false, fresh()),
                VaultHealthState::Conflicted,
            ),
            (
                operational(SyncState::Conflicted, 0, true, fresh()),
                VaultHealthState::Conflicted,
            ),
            (
                operational(SyncState::Clean, 0, true, fresh()),
                VaultHealthState::RecoveryRequired,
            ),
            (
                operational(SyncState::Paused, 0, false, fresh()),
                VaultHealthState::Paused,
            ),
            (
                operational(SyncState::Error, 0, false, fresh()),
                VaultHealthState::Failing,
            ),
            (
                operational(SyncState::Offline, 0, false, fresh()),
                VaultHealthState::Offline,
            ),
            (
                operational(SyncState::Pushing, 0, false, fresh()),
                VaultHealthState::Syncing,
            ),
            (
                operational(SyncState::Dirty, 0, false, fresh()),
                VaultHealthState::Healthy,
            ),
            (
                operational(
                    SyncState::Clean,
                    0,
                    false,
                    ScanCompletion::inspection_error("scan failed"),
                ),
                VaultHealthState::Failing,
            ),
        ] {
            let health = vault_health(&indexed, Some(&status));
            assert_eq!(health.state, expected, "{status:?}");
            assert_eq!(health.last_sync_attempt_unix_ms, Some(42));
        }
        let conflicted = vault_health(
            &indexed,
            Some(&operational(SyncState::Clean, 2, false, fresh())),
        );
        assert_eq!(conflicted.unresolved_conflicts, 2);
        assert_eq!(
            serde_json::to_value(&conflicted).unwrap()["state"],
            "conflicted"
        );
    }
}
