//! MCP web-tool validation and permission-checked reports shared by transports.

use serde_json::Value;
use vulcan_core::{ProfilePermissionGuard, VaultPaths};

use crate::mcp_protocol::{McpMethodError, McpWebFetchArgs, McpWebSearchArgs};
#[cfg(feature = "web")]
use crate::web::{
    apply_web_fetch_report_with_permissions, build_web_search_report_with_permissions,
    WebFetchMode, WebFetchRequest, WebSearchRequest,
};
#[cfg(feature = "web")]
use vulcan_core::SearchBackendKind;

pub fn web_search(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: McpWebSearchArgs,
) -> Result<Value, McpMethodError> {
    if args.limit == 0 {
        return Err(McpMethodError::invalid_params(
            "`web_search.limit` must be at least 1",
        ));
    }
    let backend = parse_search_backend(args.backend.as_deref())?;
    #[cfg(feature = "web")]
    {
        let report = build_web_search_report_with_permissions(
            paths,
            &WebSearchRequest {
                query: args.query,
                backend,
                limit: args.limit,
            },
            Some(guard),
        )
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
        serde_json::to_value(report).map_err(|error| McpMethodError::internal(error.to_string()))
    }
    #[cfg(not(feature = "web"))]
    {
        let _ = (paths, guard, args.query, backend);
        Err(McpMethodError::tool(
            "web search requires a build with the `web` feature enabled",
        ))
    }
}

pub fn web_fetch(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: McpWebFetchArgs,
) -> Result<Value, McpMethodError> {
    let mode = parse_web_fetch_mode(args.mode.as_deref())?;
    #[cfg(feature = "web")]
    {
        let report = apply_web_fetch_report_with_permissions(
            paths,
            &WebFetchRequest {
                url: args.url,
                mode,
                save: None,
            },
            Some(guard),
        )
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
        serde_json::to_value(report).map_err(|error| McpMethodError::internal(error.to_string()))
    }
    #[cfg(not(feature = "web"))]
    {
        let _ = (paths, guard, args.url, mode);
        Err(McpMethodError::tool(
            "web fetch requires a build with the `web` feature enabled",
        ))
    }
}

#[cfg(feature = "web")]
fn parse_search_backend(
    backend: Option<&str>,
) -> Result<Option<SearchBackendKind>, McpMethodError> {
    match backend {
        None => Ok(None),
        Some("disabled") => Ok(Some(SearchBackendKind::Disabled)),
        Some("auto") => Ok(Some(SearchBackendKind::Auto)),
        Some("duckduckgo") => Ok(Some(SearchBackendKind::Duckduckgo)),
        Some("kagi") => Ok(Some(SearchBackendKind::Kagi)),
        Some("exa") => Ok(Some(SearchBackendKind::Exa)),
        Some("tavily") => Ok(Some(SearchBackendKind::Tavily)),
        Some("brave") => Ok(Some(SearchBackendKind::Brave)),
        Some("ollama") => Ok(Some(SearchBackendKind::Ollama)),
        Some(other) => Err(McpMethodError::invalid_params(format!(
            "unsupported `web_search.backend`: {other}"
        ))),
    }
}

#[cfg(not(feature = "web"))]
fn parse_search_backend(backend: Option<&str>) -> Result<Option<&str>, McpMethodError> {
    match backend {
        None
        | Some(
            "disabled" | "auto" | "duckduckgo" | "kagi" | "exa" | "tavily" | "brave" | "ollama",
        ) => Ok(backend),
        Some(other) => Err(McpMethodError::invalid_params(format!(
            "unsupported `web_search.backend`: {other}"
        ))),
    }
}

#[cfg(feature = "web")]
fn parse_web_fetch_mode(mode: Option<&str>) -> Result<WebFetchMode, McpMethodError> {
    match mode.unwrap_or("markdown") {
        "markdown" => Ok(WebFetchMode::Markdown),
        "html" => Ok(WebFetchMode::Html),
        "raw" => Ok(WebFetchMode::Raw),
        other => Err(McpMethodError::invalid_params(format!(
            "unsupported `web_fetch.mode`: {other}"
        ))),
    }
}

#[cfg(not(feature = "web"))]
fn parse_web_fetch_mode(mode: Option<&str>) -> Result<&str, McpMethodError> {
    match mode.unwrap_or("markdown") {
        value @ ("markdown" | "html" | "raw") => Ok(value),
        other => Err(McpMethodError::invalid_params(format!(
            "unsupported `web_fetch.mode`: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use vulcan_core::{resolve_permission_profile, ProfilePermissionGuard};

    #[test]
    fn web_args_validate_before_network_or_feature_errors() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let search = serde_json::from_value(json!({
            "query": "test", "limit": 0
        }))
        .unwrap();
        assert!(matches!(
            web_search(&paths, &guard, search),
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
        let fetch = serde_json::from_value(json!({
            "url": "https://example.com", "mode": "unsupported"
        }))
        .unwrap();
        assert!(matches!(
            web_fetch(&paths, &guard, fetch),
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
    }
}
