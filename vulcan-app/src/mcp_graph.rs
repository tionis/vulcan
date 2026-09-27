//! Permission-aware graph workflows shared by MCP transport adapters.

use serde_json::Value;
use vulcan_core::{
    accept_link_suggestion_with_guard, query_graph_communities_with_filter,
    reject_link_suggestion_with_guard, suggest_links, LinkSuggestionStatus, LinkSuggestionsReport,
    PermissionGuard, ProfilePermissionGuard, VaultPaths,
};

use crate::mcp_protocol::{McpGraphCommunitiesArgs, McpMethodError, McpSuggestLinksArgs};

/// Preserve the MCP graph report's selection fields beside core graph analytics.
pub fn graph_communities(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpGraphCommunitiesArgs,
) -> Result<Value, McpMethodError> {
    let report =
        query_graph_communities_with_filter(paths, Some(&guard.read_filter()), !args.dry_run)
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let mut value =
        serde_json::to_value(report).map_err(|error| McpMethodError::tool(error.to_string()))?;
    if let Value::Object(object) = &mut value {
        object.insert("selected_community".to_string(), args.community.into());
        object.insert("include_orphans".to_string(), args.orphans.into());
        object.insert("include_bridges".to_string(), args.bridges.into());
    }
    Ok(value)
}

/// Validate, execute, and read-filter link suggestions for one MCP authority.
pub fn link_suggestions(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpSuggestLinksArgs,
) -> Result<LinkSuggestionsReport, McpMethodError> {
    if args.accept.is_some() && args.reject.is_some() {
        return Err(McpMethodError::invalid_params(
            "`suggest_links` accepts either `accept` or `reject`, not both",
        ));
    }
    if let Some(id) = args.accept.as_deref() {
        guard
            .check_write_path(".vulcan/cache.db")
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
        let suggestion = accept_link_suggestion_with_guard(paths, id, guard)
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
        return Ok(LinkSuggestionsReport {
            suggestions: vec![suggestion],
        });
    }
    if let Some(id) = args.reject.as_deref() {
        guard
            .check_write_path(".vulcan/cache.db")
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
        let suggestion = reject_link_suggestion_with_guard(paths, id, guard)
            .map_err(|error| McpMethodError::tool(error.to_string()))?;
        return Ok(LinkSuggestionsReport {
            suggestions: vec![suggestion],
        });
    }
    let status = args.status.as_deref().map(parse_status).transpose()?;
    let mut report = suggest_links(
        paths,
        args.note.as_deref(),
        args.limit,
        args.min_score,
        status,
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    report.suggestions.retain(|suggestion| {
        guard.check_read_path(&suggestion.source_path).is_ok()
            && guard.check_read_path(&suggestion.target_path).is_ok()
    });
    Ok(report)
}

fn parse_status(value: &str) -> Result<LinkSuggestionStatus, McpMethodError> {
    match value {
        "pending" => Ok(LinkSuggestionStatus::Pending),
        "accepted" => Ok(LinkSuggestionStatus::Accepted),
        "rejected" => Ok(LinkSuggestionStatus::Rejected),
        other => Err(McpMethodError::invalid_params(format!(
            "unsupported `suggest_links.status`: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use vulcan_core::{resolve_permission_profile, scan_vault, ScanMode};

    fn args(value: Value) -> McpSuggestLinksArgs {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn link_suggestions_validate_mutation_and_status_before_cache_access() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        assert!(matches!(
            link_suggestions(&paths, &guard, &args(json!({"accept":"a", "reject":"b"}))),
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
        assert!(matches!(
            link_suggestions(&paths, &guard, &args(json!({"status":"unsupported"}))),
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
        assert!(matches!(
            link_suggestions(&paths, &guard, &args(json!({"accept":"a"}))),
            Err(McpMethodError::Tool { .. })
        ));
    }

    #[test]
    fn graph_report_respects_read_filter_and_preserves_mcp_selection_fields() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir_all(temporary.path().join(".vulcan")).unwrap();
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        fs::write(temporary.path().join("Home.md"), "# Home\n").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let readable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let blind = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).unwrap(),
        );
        let graph_args = || {
            serde_json::from_value(json!({
                "community": 1, "orphans": true, "bridges": true, "dry_run": true
            }))
            .unwrap()
        };
        let visible = graph_communities(&paths, &readable, &graph_args()).unwrap();
        let hidden = graph_communities(&paths, &blind, &graph_args()).unwrap();
        assert_eq!(visible["selected_community"], 1);
        assert_eq!(visible["include_orphans"], true);
        assert_eq!(visible["include_bridges"], true);
        assert_eq!(visible["persisted"], false);
        assert!(!visible["orphans"].as_array().unwrap().is_empty());
        assert!(hidden["orphans"].as_array().unwrap().is_empty());
    }
}
