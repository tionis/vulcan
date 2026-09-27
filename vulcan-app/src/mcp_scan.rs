//! Permission-aware MCP index scan workflow.

use vulcan_core::{PermissionGuard, ProfilePermissionGuard, ScanMode, ScanSummary, VaultPaths};

use crate::commit::AutoCommitPolicy;
use crate::mcp_protocol::{McpIndexScanArgs, McpMethodError};
use crate::scan::scan_vault_with_automation;

pub fn index_scan(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: &McpIndexScanArgs,
) -> Result<ScanSummary, McpMethodError> {
    guard
        .check_index()
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let auto_commit = AutoCommitPolicy::for_scan(paths, args.no_commit);
    scan_vault_with_automation(
        paths,
        if args.full {
            ScanMode::Full
        } else {
            ScanMode::Incremental
        },
        &auto_commit,
        Some(profile_name),
        true,
        |_| {},
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use vulcan_core::{initialize_vulcan_dir, resolve_permission_profile};

    #[test]
    fn scan_denies_before_index_access_and_supports_full_and_incremental_modes() {
        let temporary = tempfile::tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Home.md"), "# Home\n").expect("note");
        let readonly = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).expect("readonly profile"),
        );
        let allowed = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("daily-wiki-agent")).expect("index profile"),
        );
        let args = McpIndexScanArgs {
            full: true,
            no_commit: true,
        };
        assert!(matches!(
            index_scan(&paths, &readonly, "readonly", &args),
            Err(McpMethodError::Tool { .. })
        ));
        let full = index_scan(&paths, &allowed, "daily-wiki-agent", &args).expect("full scan");
        assert_eq!(full.added, 1);
        let incremental = index_scan(
            &paths,
            &allowed,
            "daily-wiki-agent",
            &McpIndexScanArgs {
                full: false,
                no_commit: true,
            },
        )
        .expect("incremental scan");
        assert_eq!(incremental.added, 0);
        assert_eq!(incremental.updated, 0);
    }
}
