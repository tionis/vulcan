//! Permission-aware graph workflows shared by MCP transport adapters.

use serde_json::Value;
use vulcan_core::{
    accept_link_suggestion_with_guard, query_graph_communities_with_filter,
    reject_link_suggestion_with_guard, suggest_links, LinkSuggestionStatus, LinkSuggestionsReport,
    PermissionGuard, ProfilePermissionGuard, VaultPaths,
};

use crate::mcp_protocol::{
    McpGraphCommunitiesArgs, McpLinkSuggestionResolveArgs, McpLinkSuggestionsArgs, McpMethodError,
};

/// Preserve the MCP graph report's selection fields beside core graph analytics.
pub fn graph_communities(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpGraphCommunitiesArgs,
) -> Result<Value, McpMethodError> {
    let report = query_graph_communities_with_filter(paths, Some(&guard.read_filter()), false)
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

/// List link suggestions for one MCP authority, read-filtered on both ends.
pub fn link_suggestions(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpLinkSuggestionsArgs,
) -> Result<LinkSuggestionsReport, McpMethodError> {
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

/// Accept or reject one link suggestion.
pub fn link_suggestion_resolve(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpLinkSuggestionResolveArgs,
) -> Result<LinkSuggestionsReport, McpMethodError> {
    let resolve = match args.action.as_str() {
        "accept" => accept_link_suggestion_with_guard,
        "reject" => reject_link_suggestion_with_guard,
        other => {
            return Err(McpMethodError::invalid_params(format!(
                "unsupported `link_suggestion_resolve.action`: {other}; use `accept` or `reject`"
            )))
        }
    };
    guard
        .check_write_path(".vulcan/cache.db")
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let suggestion =
        resolve(paths, &args.id, guard).map_err(|error| McpMethodError::tool(error.to_string()))?;
    Ok(LinkSuggestionsReport {
        suggestions: vec![suggestion],
    })
}

fn parse_status(value: &str) -> Result<LinkSuggestionStatus, McpMethodError> {
    match value {
        "pending" => Ok(LinkSuggestionStatus::Pending),
        "accepted" => Ok(LinkSuggestionStatus::Accepted),
        "rejected" => Ok(LinkSuggestionStatus::Rejected),
        other => Err(McpMethodError::invalid_params(format!(
            "unsupported `link_suggestions.status`: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use vulcan_core::{resolve_permission_profile, scan_vault, ScanMode};

    #[test]
    fn link_suggestion_tools_validate_status_action_and_write_access_before_cache_access() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let list =
            |value: Value| -> McpLinkSuggestionsArgs { serde_json::from_value(value).unwrap() };
        let resolve = |value: Value| -> McpLinkSuggestionResolveArgs {
            serde_json::from_value(value).unwrap()
        };
        assert!(matches!(
            link_suggestions(&paths, &guard, &list(json!({"status":"unsupported"}))),
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
        assert!(matches!(
            link_suggestion_resolve(&paths, &guard, &resolve(json!({"id":"a","action":"maybe"}))),
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
        assert!(matches!(
            link_suggestion_resolve(
                &paths,
                &guard,
                &resolve(json!({"id":"a","action":"accept"}))
            ),
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
                "community": 1, "orphans": true, "bridges": true
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
