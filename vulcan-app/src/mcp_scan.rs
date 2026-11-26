//! Permission-aware MCP index scan workflow.

use vulcan_core::Verbosity;
use vulcan_core::{
    PermissionGuard, ProfilePermissionGuard, ScanMode, ScanPhase, ScanProgress, ScanSummary,
    VaultPaths,
};

use crate::commit::AutoCommitPolicy;
use crate::mcp_progress::McpProgress;
use crate::mcp_protocol::{McpIndexScanArgs, McpMethodError};
use crate::scan::scan_vault_with_automation;

/// Scan progress as MCP progress: files processed of files discovered while
/// scanning, then the finishing phases, ending at the total.
fn report_scan_progress(progress: &McpProgress, event: &ScanProgress) {
    #[allow(clippy::cast_precision_loss)]
    let (done, total) = (event.processed as f64, event.discovered as f64);
    let (value, message) = match event.phase {
        ScanPhase::PreparingFiles => (0.0, "Preparing files"),
        ScanPhase::ScanningFiles => (done, "Scanning files"),
        ScanPhase::RefreshingPropertyCatalog => (total + 1.0, "Refreshing the property catalog"),
        ScanPhase::ResolvingLinks => (total + 2.0, "Resolving links"),
        ScanPhase::Completed => (total + 3.0, "Scan complete"),
    };
    progress.report(value, Some(total + 3.0), Some(message));
}

pub fn index_scan(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    args: &McpIndexScanArgs,
    progress: Option<&McpProgress>,
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
        Verbosity::Quiet,
        |event| {
            if let Some(progress) = progress {
                report_scan_progress(progress, &event);
            }
        },
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
            index_scan(&paths, &readonly, "readonly", &args, None),
            Err(McpMethodError::Tool { .. })
        ));
        let full =
            index_scan(&paths, &allowed, "daily-wiki-agent", &args, None).expect("full scan");
        assert_eq!(full.added, 1);
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&sent);
        let notifier = crate::mcp_progress::McpNotifier(std::sync::Arc::new(move |message| {
            sink.lock().unwrap().push(message);
        }));
        let progress = McpProgress::for_request(
            Some(&serde_json::json!({"_meta": {"progressToken": "scan"}})),
            Some(&notifier),
        );
        let incremental = index_scan(
            &paths,
            &allowed,
            "daily-wiki-agent",
            &McpIndexScanArgs {
                full: false,
                no_commit: true,
            },
            progress.as_ref(),
        )
        .expect("incremental scan");
        assert_eq!(incremental.added, 0);
        // Progress starts and ends at the total, whatever was rate-limited
        // between.
        let sent = sent.lock().unwrap();
        let first = &sent.first().expect("progress")["params"];
        let last = &sent.last().expect("progress")["params"];
        assert_eq!(first["progressToken"], "scan");
        assert_eq!(first["progress"], 0.0);
        assert_eq!(last["progress"], last["total"]);
        assert_eq!(last["message"], "Scan complete");
        assert_eq!(incremental.updated, 0);
    }
}
