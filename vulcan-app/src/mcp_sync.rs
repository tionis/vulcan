//! Permission-aware, read-only MCP sync reports shared across host transports.

use serde::Serialize;
use serde_json::Value;
use vulcan_core::{PermissionGuard, ProfilePermissionGuard, VaultPaths};

use crate::mcp_protocol::{
    McpMethodError, McpSyncConflictsArgs, McpSyncDoctorArgs, McpSyncTargetArgs,
};
use crate::sync::{
    doctor_git_vault_for_platform, sync_git_vault, GitPlatformProfile, GitRefName, GitRemote,
    GitSyncOptions,
};
use crate::sync_conflicts::{get_sync_conflict, list_sync_conflicts};

pub fn sync_preview(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpSyncTargetArgs,
) -> Result<Value, McpMethodError> {
    guard
        .check_git()
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let options = git_sync_options(args)?;
    let report =
        sync_git_vault(paths, &options).map_err(|error| McpMethodError::tool(error.to_string()))?;
    report_value(report)
}

pub fn sync_doctor(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpSyncDoctorArgs,
) -> Result<Value, McpMethodError> {
    guard
        .check_git()
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let target = McpSyncTargetArgs {
        remote: args.remote.clone(),
        live_ref: args.live_ref.clone(),
    };
    let options = git_sync_options(&target)?;
    let platform = args
        .platform
        .as_deref()
        .map(GitPlatformProfile::parse)
        .transpose()
        .map_err(|error| McpMethodError::invalid_params(error.to_string()))?
        .unwrap_or_else(GitPlatformProfile::native);
    report_value(doctor_git_vault_for_platform(paths, &options, platform))
}

pub fn sync_conflicts(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpSyncConflictsArgs,
) -> Result<Value, McpMethodError> {
    guard
        .check_git()
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    if let Some(conflict_id) = args.conflict_id.as_deref() {
        report_value(
            get_sync_conflict(paths, conflict_id)
                .map_err(|error| McpMethodError::tool(error.to_string()))?,
        )
    } else {
        report_value(
            list_sync_conflicts(paths).map_err(|error| McpMethodError::tool(error.to_string()))?,
        )
    }
}

fn git_sync_options(args: &McpSyncTargetArgs) -> Result<GitSyncOptions, McpMethodError> {
    let mut options = GitSyncOptions::default();
    if let Some(remote) = args.remote.as_deref() {
        options.remote = GitRemote::parse(remote)
            .map_err(|error| McpMethodError::invalid_params(error.to_string()))?;
    }
    if let Some(live_ref) = args.live_ref.as_deref() {
        options.live_ref = GitRefName::parse(live_ref)
            .map_err(|error| McpMethodError::invalid_params(error.to_string()))?;
    }
    options.dry_run = true;
    Ok(options)
}

fn report_value(report: impl Serialize) -> Result<Value, McpMethodError> {
    serde_json::to_value(report).map_err(|error| McpMethodError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulcan_core::resolve_permission_profile;

    #[test]
    fn sync_options_are_dry_run_by_default_and_accept_valid_overrides() {
        let default = git_sync_options(&McpSyncTargetArgs {
            remote: None,
            live_ref: None,
        })
        .expect("default sync options");
        assert!(default.dry_run);
        let explicit = git_sync_options(&McpSyncTargetArgs {
            remote: Some("backup".to_string()),
            live_ref: Some("refs/vulcan/live".to_string()),
        })
        .expect("explicit sync options");
        assert!(explicit.dry_run);
        assert_eq!(explicit.remote.as_str(), "backup");
        assert_eq!(explicit.live_ref.as_str(), "refs/vulcan/live");
    }

    #[test]
    fn invalid_sync_remote_and_ref_are_invalid_mcp_parameters() {
        for args in [
            McpSyncTargetArgs {
                remote: Some("-unsafe".to_string()),
                live_ref: None,
            },
            McpSyncTargetArgs {
                remote: None,
                live_ref: Some("not-a-ref".to_string()),
            },
        ] {
            assert!(matches!(
                git_sync_options(&args),
                Err(McpMethodError::JsonRpc { code: -32602, .. })
            ));
        }
    }

    #[test]
    fn every_sync_report_denies_without_git_authority_before_accessing_a_vault() {
        let temporary = tempfile::tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).expect("readonly profile"),
        );
        assert!(matches!(
            sync_preview(&paths, &guard, &McpSyncTargetArgs::default()),
            Err(McpMethodError::Tool { .. })
        ));
        assert!(matches!(
            sync_doctor(&paths, &guard, &McpSyncDoctorArgs::default()),
            Err(McpMethodError::Tool { .. })
        ));
        assert!(matches!(
            sync_conflicts(&paths, &guard, &McpSyncConflictsArgs::default()),
            Err(McpMethodError::Tool { .. })
        ));
        assert!(!temporary.path().join(".git").exists());
        assert!(!temporary.path().join(".vulcan").exists());
    }
}
