//! Transport-level route classification for hosted MCP HTTP requests.
//!
//! The daemon owns path selection; OAuth authorization and MCP method handling
//! remain behind the host callback until their application workflows migrate.

use crate::mcp_http_codec::McpHttpRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpHttpRoute<'a> {
    LocalOAuthRegister,
    LocalOAuthAuthorize,
    LocalOAuthToken,
    LocalOAuthIndieAuthCallback,
    LocalOAuthConsent,
    AuthorizationServerMetadata,
    ProtectedResourceMetadata,
    OperationStatus(&'a str),
    McpEndpoint,
    NotFound,
}

#[must_use]
pub fn classify_mcp_http_route<'a>(
    request: &'a McpHttpRequest,
    endpoint: &str,
    oauth_enabled: bool,
    local_oauth: bool,
    named_remote: bool,
) -> McpHttpRoute<'a> {
    if local_oauth {
        match request.path.as_str() {
            "/oauth/register" => return McpHttpRoute::LocalOAuthRegister,
            "/oauth/authorize" => return McpHttpRoute::LocalOAuthAuthorize,
            "/oauth/token" => return McpHttpRoute::LocalOAuthToken,
            "/oauth/indieauth/callback" => return McpHttpRoute::LocalOAuthIndieAuthCallback,
            "/oauth/consent" => return McpHttpRoute::LocalOAuthConsent,
            _ => {}
        }
    }
    if oauth_enabled && request.method == "GET" {
        if is_authorization_server_metadata_path(&request.path, endpoint) {
            return McpHttpRoute::AuthorizationServerMetadata;
        }
        if is_protected_resource_metadata_path(&request.path, endpoint) {
            return McpHttpRoute::ProtectedResourceMetadata;
        }
    }
    if named_remote {
        let prefix = format!("{}/operations/", endpoint.trim_end_matches('/'));
        if let Some(operation_id) = request.path.strip_prefix(&prefix) {
            return McpHttpRoute::OperationStatus(operation_id);
        }
    }
    if request.path == endpoint {
        McpHttpRoute::McpEndpoint
    } else {
        McpHttpRoute::NotFound
    }
}

fn is_protected_resource_metadata_path(path: &str, endpoint: &str) -> bool {
    path == "/.well-known/oauth-protected-resource"
        || path == format!("/.well-known/oauth-protected-resource{endpoint}")
}

fn is_authorization_server_metadata_path(path: &str, endpoint: &str) -> bool {
    path == "/.well-known/oauth-authorization-server"
        || path == format!("/.well-known/oauth-authorization-server{endpoint}")
        || path == "/.well-known/openid-configuration"
        || path == format!("/.well-known/openid-configuration{endpoint}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn request(method: &str, path: &str) -> McpHttpRequest {
        McpHttpRequest {
            method: method.to_string(),
            path: path.to_string(),
            query: String::new(),
            headers: BTreeMap::new(),
            body: Vec::new(),
        }
    }

    #[test]
    fn metadata_routes_accept_root_endpoint_and_oidc_forms_only_for_get() {
        for path in [
            "/.well-known/oauth-authorization-server",
            "/.well-known/oauth-authorization-server/mcp",
            "/.well-known/openid-configuration",
            "/.well-known/openid-configuration/mcp",
        ] {
            assert_eq!(
                classify_mcp_http_route(&request("GET", path), "/mcp", true, false, false),
                McpHttpRoute::AuthorizationServerMetadata
            );
            assert_eq!(
                classify_mcp_http_route(&request("POST", path), "/mcp", true, false, false),
                McpHttpRoute::NotFound
            );
        }
        for path in [
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-protected-resource/mcp",
        ] {
            assert_eq!(
                classify_mcp_http_route(&request("GET", path), "/mcp", true, false, false),
                McpHttpRoute::ProtectedResourceMetadata
            );
        }
        assert_eq!(
            classify_mcp_http_route(
                &request("GET", "/.well-known/oauth-protected-resource"),
                "/mcp",
                false,
                false,
                false,
            ),
            McpHttpRoute::NotFound
        );
    }

    #[test]
    fn local_oauth_and_named_operation_routes_are_instance_gated() {
        for (path, route) in [
            ("/oauth/register", McpHttpRoute::LocalOAuthRegister),
            ("/oauth/authorize", McpHttpRoute::LocalOAuthAuthorize),
            ("/oauth/token", McpHttpRoute::LocalOAuthToken),
            (
                "/oauth/indieauth/callback",
                McpHttpRoute::LocalOAuthIndieAuthCallback,
            ),
            ("/oauth/consent", McpHttpRoute::LocalOAuthConsent),
        ] {
            assert_eq!(
                classify_mcp_http_route(&request("POST", path), "/mcp", true, true, false),
                route
            );
            assert_eq!(
                classify_mcp_http_route(&request("POST", path), "/mcp", true, false, false),
                McpHttpRoute::NotFound
            );
        }
        let operation = request("GET", "/mcp/operations/01AB");
        assert_eq!(
            classify_mcp_http_route(&operation, "/mcp", true, true, true),
            McpHttpRoute::OperationStatus("01AB")
        );
        assert_eq!(
            classify_mcp_http_route(&operation, "/mcp", true, true, false),
            McpHttpRoute::NotFound
        );
        assert_eq!(
            classify_mcp_http_route(&request("POST", "/mcp"), "/mcp", false, false, false),
            McpHttpRoute::McpEndpoint
        );
    }
}
