//! Publish and remove realtime sync notification advertisements.
//!
//! This wraps the [`vulcan_sync`] advertisement contract in the vault-oriented
//! workflow used by direct CLI commands: resolve the vault, discover its
//! repository, and report redacted endpoint identity. The complete subscribe
//! URL never appears in a report, log, or error.

use crate::AppError;
use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_sync::{
    preview_notification_advertisement, publish_notification_advertisement,
    refresh_notification_advertisement, remove_notification_advertisement, GitEngine, GitOid,
    GitRefDeleteResult, GitRemote, NotificationAdvertisementError, NOTIFICATION_ADVERTISEMENT_REF,
};

pub const SYNC_NOTIFICATION_REPORT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncNotificationPublishOptions {
    pub subscribe_url: String,
    pub remote: GitRemote,
    pub expected: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncNotificationPublishReport {
    pub version: u32,
    pub vault: PathBuf,
    pub remote: GitRemote,
    pub advertisement_ref: String,
    pub dry_run: bool,
    pub previous_revision: Option<String>,
    pub revision: Option<String>,
    pub origin: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncNotificationRemoveOptions {
    pub remote: GitRemote,
    pub expected: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncNotificationRemoveReport {
    pub version: u32,
    pub vault: PathBuf,
    pub remote: GitRemote,
    pub advertisement_ref: String,
    pub dry_run: bool,
    pub previous_revision: Option<String>,
    pub deleted: bool,
}

/// Publishes the notification advertisement for one vault's repository.
///
/// With `expected: None`, the currently advertised revision (when any) becomes
/// the compare-and-swap lease, so concurrent publishers fail instead of
/// overwriting each other. A dry run validates the URL and reports the current
/// remote revision without creating objects or pushing.
pub fn publish_sync_notification_advertisement(
    paths: &VaultPaths,
    options: &SyncNotificationPublishOptions,
) -> Result<SyncNotificationPublishReport, AppError> {
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let reference = vulcan_sync::GitRefName::parse(NOTIFICATION_ADVERTISEMENT_REF)
        .map_err(AppError::operation)?;
    let previous = engine
        .remote_ref(&repository, &options.remote, &reference)
        .map_err(AppError::operation)?;
    if options.dry_run {
        let advertisement = preview_notification_advertisement(&options.subscribe_url)
            .map_err(AppError::operation)?;
        return Ok(SyncNotificationPublishReport {
            version: SYNC_NOTIFICATION_REPORT_VERSION,
            vault,
            remote: options.remote.clone(),
            advertisement_ref: NOTIFICATION_ADVERTISEMENT_REF.to_string(),
            dry_run: true,
            previous_revision: previous.as_ref().map(GitOid::to_string),
            revision: None,
            origin: advertisement.endpoint.origin().to_string(),
            fingerprint: advertisement.endpoint.fingerprint().to_string(),
        });
    }
    let published = publish_notification_advertisement(
        &engine,
        &repository,
        &options.remote,
        &options.subscribe_url,
        parse_expected(options.expected.clone())?.as_ref(),
    )
    .map_err(AppError::operation)?;
    Ok(SyncNotificationPublishReport {
        version: SYNC_NOTIFICATION_REPORT_VERSION,
        vault,
        remote: options.remote.clone(),
        advertisement_ref: NOTIFICATION_ADVERTISEMENT_REF.to_string(),
        dry_run: false,
        previous_revision: previous.as_ref().map(GitOid::to_string),
        revision: Some(published.revision.to_string()),
        origin: published.advertisement.endpoint.origin().to_string(),
        fingerprint: published.advertisement.endpoint.fingerprint().to_string(),
    })
}

/// Removes the notification advertisement for one vault's repository under an
/// exact lease. With `expected: None`, the currently advertised revision (when
/// any) becomes the lease; an absent ref reports `deleted: false` without
/// mutation. A dry run reports the current remote revision without deleting.
pub fn remove_sync_notification_advertisement(
    paths: &VaultPaths,
    options: &SyncNotificationRemoveOptions,
) -> Result<SyncNotificationRemoveReport, AppError> {
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let reference = vulcan_sync::GitRefName::parse(NOTIFICATION_ADVERTISEMENT_REF)
        .map_err(AppError::operation)?;
    let previous = engine
        .remote_ref(&repository, &options.remote, &reference)
        .map_err(AppError::operation)?;
    if options.dry_run {
        return Ok(SyncNotificationRemoveReport {
            version: SYNC_NOTIFICATION_REPORT_VERSION,
            vault,
            remote: options.remote.clone(),
            advertisement_ref: NOTIFICATION_ADVERTISEMENT_REF.to_string(),
            dry_run: true,
            previous_revision: previous.as_ref().map(GitOid::to_string),
            deleted: false,
        });
    }
    let lease = match (parse_expected(options.expected.clone())?, &previous) {
        (Some(expected), _) => Some(expected),
        (None, Some(current)) => Some(current.clone()),
        (None, None) => None,
    };
    let Some(lease) = lease else {
        return Ok(SyncNotificationRemoveReport {
            version: SYNC_NOTIFICATION_REPORT_VERSION,
            vault,
            remote: options.remote.clone(),
            advertisement_ref: NOTIFICATION_ADVERTISEMENT_REF.to_string(),
            dry_run: false,
            previous_revision: None,
            deleted: false,
        });
    };
    let deleted = remove_notification_advertisement(&engine, &repository, &options.remote, &lease)
        .map_err(AppError::operation)?
        == GitRefDeleteResult::Deleted;
    if deleted {
        let stale = refresh_notification_advertisement(&engine, &repository, &options.remote)
            .map_err(AppError::operation)?;
        if stale.is_some() {
            return Err(AppError::operation(
                "the notification advertisement is still advertised after deletion",
            ));
        }
    }
    Ok(SyncNotificationRemoveReport {
        version: SYNC_NOTIFICATION_REPORT_VERSION,
        vault,
        remote: options.remote.clone(),
        advertisement_ref: NOTIFICATION_ADVERTISEMENT_REF.to_string(),
        dry_run: false,
        previous_revision: previous.as_ref().map(GitOid::to_string),
        deleted,
    })
}

fn parse_expected(expected: Option<String>) -> Result<Option<GitOid>, AppError> {
    expected
        .map(GitOid::parse)
        .transpose()
        .map_err(AppError::operation)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncNotificationStatusOptions {
    pub remote: GitRemote,
    /// Effective permission profile name. `None` resolves the default profile.
    pub permissions_profile: Option<String>,
    /// Registration pause state. `None` for direct-vault inspections.
    pub paused: Option<bool>,
    /// Whether the registration uses the Git backend. `None` for direct-vault.
    pub git_backend: Option<bool>,
    pub daemon_running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct SyncNotificationStatusReport {
    pub version: u32,
    pub vault: PathBuf,
    pub remote: GitRemote,
    pub advertisement_ref: String,
    pub advertised: bool,
    pub revision: Option<String>,
    pub origin: Option<String>,
    pub fingerprint: Option<String>,
    pub valid: bool,
    pub detail: String,
    pub git_allowed: bool,
    pub network_allowed: Option<bool>,
    pub paused: Option<bool>,
    pub git_backend: Option<bool>,
    pub daemon_running: bool,
    pub eligible: bool,
    pub would_listen: bool,
    pub reasons: Vec<String>,
}

/// Inspects whether Vulcan would use a notification server for one vault.
///
/// This fetches the advertisement ref through the configured remote — the
/// same device-local fetch the daemon performs — but never publishes.
/// Reasons are stable machine-readable codes; `detail` carries the human
/// message. Endpoint identity is origin and fingerprint only.
pub fn notification_status(
    paths: &VaultPaths,
    options: &SyncNotificationStatusOptions,
) -> Result<SyncNotificationStatusReport, AppError> {
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let selection = resolve_permission_profile(paths, options.permissions_profile.as_deref())
        .map_err(AppError::operation)?;
    let guard = ProfilePermissionGuard::new(paths, selection);
    let git_allowed = guard.check_git().is_ok();

    let mut report = SyncNotificationStatusReport {
        version: SYNC_NOTIFICATION_REPORT_VERSION,
        vault,
        remote: options.remote.clone(),
        advertisement_ref: NOTIFICATION_ADVERTISEMENT_REF.to_string(),
        advertised: false,
        revision: None,
        origin: None,
        fingerprint: None,
        valid: false,
        detail: String::new(),
        git_allowed,
        network_allowed: None,
        paused: options.paused,
        git_backend: options.git_backend,
        daemon_running: options.daemon_running,
        eligible: false,
        would_listen: false,
        reasons: Vec::new(),
    };

    match refresh_notification_advertisement(&engine, &repository, &options.remote) {
        Ok(Some(discovered)) => {
            report.advertised = true;
            report.valid = true;
            report.revision = Some(discovered.revision.to_string());
            report.origin = Some(discovered.advertisement.endpoint.origin().to_string());
            report.fingerprint = Some(discovered.advertisement.endpoint.fingerprint().to_string());
            report.detail = "the notification advertisement is valid".to_string();
            report.network_allowed = Some(
                guard
                    .check_network(discovered.advertisement.endpoint.origin())
                    .is_ok(),
            );
        }
        Ok(None) => {
            report.detail = format!(
                "no notification advertisement is advertised on `{}`",
                options.remote.as_str()
            );
        }
        Err(NotificationAdvertisementError::Invalid(detail)) => {
            report.advertised = true;
            report.valid = false;
            report.detail = detail;
            if let Ok(reference) = vulcan_sync::GitRefName::parse(NOTIFICATION_ADVERTISEMENT_REF) {
                if let Ok(Some(revision)) =
                    engine.remote_ref(&repository, &options.remote, &reference)
                {
                    report.revision = Some(revision.to_string());
                }
            }
        }
        Err(error) => return Err(AppError::operation(error)),
    }

    if !report.advertised {
        report.reasons.push("missing-advertisement".to_string());
    }
    if report.advertised && !report.valid {
        report.reasons.push("invalid-advertisement".to_string());
    }
    if !report.git_allowed {
        report.reasons.push("git-denied".to_string());
    }
    if report.network_allowed == Some(false) {
        report.reasons.push("network-denied".to_string());
    }
    if report.paused == Some(true) {
        report.reasons.push("paused".to_string());
    }
    if report.git_backend == Some(false) {
        report.reasons.push("non-git-backend".to_string());
    }
    report.eligible = report.advertised
        && report.valid
        && report.git_allowed
        && report.network_allowed == Some(true)
        && report.paused != Some(true)
        && report.git_backend != Some(false);
    if report.eligible && !report.daemon_running {
        report.reasons.push("daemon-stopped".to_string());
    }
    report.would_listen = report.eligible && report.daemon_running;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn publish_fixture() -> (tempfile::TempDir, VaultPaths, GitRemote) {
        let temporary = tempdir().expect("temporary directory");
        let remote = temporary.path().join("remote.git");
        let vault = temporary.path().join("vault");
        run_git(
            temporary.path(),
            &["init", "--bare", remote.to_str().expect("remote")],
        );
        std::fs::create_dir(&vault).expect("vault directory");
        run_git(&vault, &["init"]);
        run_git(&vault, &["config", "user.name", "Vulcan Tests"]);
        run_git(&vault, &["config", "user.email", "vulcan@example.invalid"]);
        run_git(
            &vault,
            &["remote", "add", "origin", remote.to_str().expect("remote")],
        );
        let paths = VaultPaths::new(vault);
        let remote = GitRemote::parse("origin").expect("remote name");
        (temporary, paths, remote)
    }

    fn run_git(directory: &std::path::Path, arguments: &[&str]) {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .output()
            .expect("run Git");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn publish_dry_run_validates_without_mutation() {
        let (_temporary, paths, remote) = publish_fixture();
        let report = publish_sync_notification_advertisement(
            &paths,
            &SyncNotificationPublishOptions {
                subscribe_url: "https://patch.example/h/secret-channel?pubsub=true".to_string(),
                remote,
                expected: None,
                dry_run: true,
            },
        )
        .expect("dry-run publish");
        assert!(report.dry_run);
        assert_eq!(report.previous_revision, None);
        assert_eq!(report.revision, None);
        assert_eq!(report.origin, "https://patch.example");
        assert!(!format!("{report:?}").contains("secret-channel"));

        let engine = vulcan_sync::GitCliEngine::default();
        let repository = engine
            .discover_repository(paths.vault_root())
            .expect("discover repository");
        let remote = GitRemote::parse("origin").expect("remote name");
        assert!(
            refresh_notification_advertisement(&engine, &repository, &remote)
                .expect("refresh")
                .is_none()
        );
    }

    #[test]
    fn publish_then_remove_round_trips_with_leases() {
        let (_temporary, paths, remote) = publish_fixture();
        let published = publish_sync_notification_advertisement(
            &paths,
            &SyncNotificationPublishOptions {
                subscribe_url: "https://patch.example/h/round-trip?pubsub=true".to_string(),
                remote: remote.clone(),
                expected: None,
                dry_run: false,
            },
        )
        .expect("publish");
        let revision = published.revision.clone().expect("published revision");
        assert_eq!(published.previous_revision, None);

        let rotated = publish_sync_notification_advertisement(
            &paths,
            &SyncNotificationPublishOptions {
                subscribe_url: "https://patch.example/h/rotated?pubsub=true".to_string(),
                remote: remote.clone(),
                expected: Some(revision.clone()),
                dry_run: false,
            },
        )
        .expect("rotate");
        assert_eq!(
            rotated.previous_revision.as_deref(),
            Some(revision.as_str())
        );
        assert_ne!(rotated.revision.as_deref(), Some(revision.as_str()));

        let removed = remove_sync_notification_advertisement(
            &paths,
            &SyncNotificationRemoveOptions {
                remote,
                expected: None,
                dry_run: false,
            },
        )
        .expect("remove");
        assert!(removed.deleted);
        assert_eq!(
            removed.previous_revision, rotated.revision,
            "removal should report the rotated revision"
        );
    }

    #[test]
    fn remove_absent_advertisement_reports_without_mutation() {
        let (_temporary, paths, remote) = publish_fixture();
        let removed = remove_sync_notification_advertisement(
            &paths,
            &SyncNotificationRemoveOptions {
                remote,
                expected: None,
                dry_run: false,
            },
        )
        .expect("remove absent");
        assert!(!removed.deleted);
        assert_eq!(removed.previous_revision, None);
    }

    fn status_options(remote: &GitRemote) -> SyncNotificationStatusOptions {
        SyncNotificationStatusOptions {
            remote: remote.clone(),
            permissions_profile: None,
            paused: None,
            git_backend: None,
            daemon_running: true,
        }
    }

    fn push_raw_advertisement(vault: &std::path::Path, contents: &str) {
        std::fs::write(vault.join("notification.json"), contents).expect("raw advertisement");
        run_git(vault, &["add", "notification.json"]);
        run_git(vault, &["commit", "-m", "raw advertisement"]);
        run_git(vault, &["push", "origin", "HEAD:refs/vulcan/notifications"]);
    }

    #[test]
    fn status_reports_missing_advertisement() {
        let (_temporary, paths, remote) = publish_fixture();
        let report =
            notification_status(&paths, &status_options(&remote)).expect("notification status");
        assert!(!report.advertised);
        assert!(!report.valid);
        assert!(!report.eligible);
        assert!(!report.would_listen);
        assert_eq!(report.reasons, ["missing-advertisement"]);
        assert!(report.detail.contains("origin"));
    }

    #[test]
    fn status_reports_valid_advertisement_as_listenable() {
        let (_temporary, paths, remote) = publish_fixture();
        let published = publish_sync_notification_advertisement(
            &paths,
            &SyncNotificationPublishOptions {
                subscribe_url: "https://patch.example/h/status-channel?pubsub=true".to_string(),
                remote: remote.clone(),
                expected: None,
                dry_run: false,
            },
        )
        .expect("publish");
        let report =
            notification_status(&paths, &status_options(&remote)).expect("notification status");
        assert!(report.advertised);
        assert!(report.valid);
        assert_eq!(
            report.revision, published.revision,
            "status should report the published revision"
        );
        assert_eq!(report.origin.as_deref(), Some("https://patch.example"));
        assert!(report.git_allowed);
        assert_eq!(report.network_allowed, Some(true));
        assert!(report.eligible);
        assert!(report.would_listen);
        assert!(report.reasons.is_empty());
        assert!(!format!("{report:?}").contains("status-channel"));

        let stopped = notification_status(
            &paths,
            &SyncNotificationStatusOptions {
                daemon_running: false,
                ..status_options(&remote)
            },
        )
        .expect("stopped status");
        assert!(stopped.eligible);
        assert!(!stopped.would_listen);
        assert_eq!(stopped.reasons, ["daemon-stopped"]);
    }

    #[test]
    fn status_reports_invalid_advertisement_with_revision() {
        let (_temporary, paths, remote) = publish_fixture();
        push_raw_advertisement(paths.vault_root(), "not json");
        let report =
            notification_status(&paths, &status_options(&remote)).expect("notification status");
        assert!(report.advertised);
        assert!(!report.valid);
        assert!(report.revision.is_some());
        assert!(!report.eligible);
        assert!(!report.would_listen);
        assert!(report
            .reasons
            .contains(&"invalid-advertisement".to_string()));
    }

    #[test]
    fn status_reports_pause_and_backend_blocks() {
        let (_temporary, paths, remote) = publish_fixture();
        publish_sync_notification_advertisement(
            &paths,
            &SyncNotificationPublishOptions {
                subscribe_url: "https://patch.example/h/paused?pubsub=true".to_string(),
                remote: remote.clone(),
                expected: None,
                dry_run: false,
            },
        )
        .expect("publish");
        let paused = notification_status(
            &paths,
            &SyncNotificationStatusOptions {
                paused: Some(true),
                ..status_options(&remote)
            },
        )
        .expect("paused status");
        assert!(!paused.eligible);
        assert!(paused.reasons.contains(&"paused".to_string()));

        let foreign = notification_status(
            &paths,
            &SyncNotificationStatusOptions {
                git_backend: Some(false),
                ..status_options(&remote)
            },
        )
        .expect("backend status");
        assert!(!foreign.eligible);
        assert!(foreign.reasons.contains(&"non-git-backend".to_string()));
    }

    #[test]
    fn status_reports_denied_git_and_network() {
        let (_temporary, paths, remote) = publish_fixture();
        let denied = notification_status(
            &paths,
            &SyncNotificationStatusOptions {
                permissions_profile: Some("readonly".to_string()),
                ..status_options(&remote)
            },
        )
        .expect("denied status");
        assert!(!denied.git_allowed);
        assert!(!denied.eligible);
        assert!(denied.reasons.contains(&"git-denied".to_string()));

        std::fs::create_dir_all(paths.vault_root().join(".vulcan")).expect("vulcan dir");
        std::fs::write(
            paths.vault_root().join(".vulcan/config.local.toml"),
            "[permissions.profiles.netdenied]\ngit = \"allow\"\nnetwork = \"deny\"\n",
        )
        .expect("custom profile");
        publish_sync_notification_advertisement(
            &paths,
            &SyncNotificationPublishOptions {
                subscribe_url: "https://patch.example/h/netted?pubsub=true".to_string(),
                remote: remote.clone(),
                expected: None,
                dry_run: false,
            },
        )
        .expect("publish");
        let netted = notification_status(
            &paths,
            &SyncNotificationStatusOptions {
                permissions_profile: Some("netdenied".to_string()),
                ..status_options(&remote)
            },
        )
        .expect("netted status");
        assert!(netted.git_allowed);
        assert_eq!(netted.network_allowed, Some(false));
        assert!(!netted.eligible);
        assert!(netted.reasons.contains(&"network-denied".to_string()));
    }
}
