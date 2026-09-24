//! Reusable read-only MCP tool workflows. Transport adapters own response framing.

#![allow(clippy::must_use_candidate)]

use serde_json::Value;
use vulcan_core::{
    search_vault_with_filter, PermissionGuard, ProfilePermissionGuard, SearchQuery, SearchSort,
    VaultPaths,
};

use crate::mcp_protocol::{McpMethodError, McpSearchArgs};

pub fn search(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: McpSearchArgs,
) -> Result<Value, McpMethodError> {
    if args.limit == 0 {
        return Err(McpMethodError::invalid_params(
            "`search.limit` must be at least 1",
        ));
    }
    let report = search_vault_with_filter(
        paths,
        &SearchQuery {
            text: args.query,
            tag: args.tag,
            path_prefix: args.path_prefix,
            has_property: args.has_property,
            filters: args.filters,
            provider: None,
            mode: parse_search_mode(args.mode.as_deref())?,
            sort: parse_search_sort(args.sort.as_deref())?,
            match_case: args.match_case.then_some(true),
            limit: Some(args.limit),
            context_size: args.context_size,
            raw_query: args.raw_query,
            fuzzy: args.fuzzy,
            explain: args.explain,
        },
        Some(&guard.read_filter()),
    )
    .map_err(|error| McpMethodError::tool(error.to_string()))?;
    serde_json::to_value(report).map_err(|error| {
        McpMethodError::internal(format!("failed to serialize `search` report: {error}"))
    })
}

fn parse_search_mode(
    mode: Option<&str>,
) -> Result<vulcan_core::search::SearchMode, McpMethodError> {
    match mode.unwrap_or("keyword") {
        "keyword" => Ok(vulcan_core::search::SearchMode::Keyword),
        "hybrid" => Ok(vulcan_core::search::SearchMode::Hybrid),
        other => Err(McpMethodError::invalid_params(format!(
            "unsupported `search.mode`: {other}"
        ))),
    }
}

fn parse_search_sort(sort: Option<&str>) -> Result<Option<SearchSort>, McpMethodError> {
    let value = match sort {
        None => return Ok(None),
        Some("relevance") => SearchSort::Relevance,
        Some("path_asc") => SearchSort::PathAsc,
        Some("path_desc") => SearchSort::PathDesc,
        Some("modified_newest") => SearchSort::ModifiedNewest,
        Some("modified_oldest") => SearchSort::ModifiedOldest,
        Some("created_newest") => SearchSort::CreatedNewest,
        Some("created_oldest") => SearchSort::CreatedOldest,
        Some(other) => {
            return Err(McpMethodError::invalid_params(format!(
                "unsupported `search.sort`: {other}"
            )));
        }
    };
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use vulcan_core::{resolve_permission_profile, scan_vault, ScanMode};

    #[test]
    fn search_validates_mcp_arguments_before_running() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        for arguments in [
            json!({"query": "alpha", "limit": 0}),
            json!({"query": "alpha", "mode": "unsupported"}),
            json!({"query": "alpha", "sort": "unsupported"}),
        ] {
            let args = serde_json::from_value(arguments).unwrap();
            assert!(matches!(
                search(&paths, &guard, args),
                Err(McpMethodError::JsonRpc { code: -32602, .. })
            ));
        }
    }

    #[test]
    fn search_respects_the_caller_read_filter() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir_all(temporary.path().join(".vulcan")).unwrap();
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        fs::write(
            temporary.path().join("Home.md"),
            "alpha searchable content\n",
        )
        .unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let readable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let blind = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).unwrap(),
        );
        let args = || serde_json::from_value(json!({"query": "alpha"})).unwrap();
        let visible = search(&paths, &readable, args()).unwrap();
        let hidden = search(&paths, &blind, args()).unwrap();
        assert!(visible.to_string().contains("Home.md"));
        assert!(!hidden.to_string().contains("Home.md"));
    }
}
