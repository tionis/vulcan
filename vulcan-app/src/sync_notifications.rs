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
use vulcan_core::VaultPaths;
use vulcan_sync::{
    preview_notification_advertisement, publish_notification_advertisement,
    refresh_notification_advertisement, remove_notification_advertisement, GitEngine, GitOid,
    GitRefDeleteResult, GitRemote, NOTIFICATION_ADVERTISEMENT_REF,
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
}
