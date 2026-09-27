//! Transport-neutral policy for the local MCP OAuth authorization server.

use std::collections::{BTreeMap, BTreeSet};

pub const DEFAULT_MCP_OAUTH_SCOPES: &[&str] = &["mcp:prompts", "mcp:resources", "mcp:tools"];
pub const SUPPORTED_MCP_OAUTH_SCOPES: &[&str] = &[
    "openid",
    "email",
    "profile",
    "mcp:tools",
    "mcp:resources",
    "mcp:prompts",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpOAuthPolicyError {
    InvalidScope,
    InvalidAuthorizationRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAuthorizeRequest {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub scopes: Vec<String>,
    pub resource: String,
    pub state: Option<String>,
}

pub fn parse_mcp_oauth_scopes(scope: Option<&str>) -> Result<Vec<String>, McpOAuthPolicyError> {
    let values = scope.map_or_else(
        || {
            DEFAULT_MCP_OAUTH_SCOPES
                .iter()
                .map(|scope| (*scope).to_string())
                .collect::<Vec<_>>()
        },
        |scope| {
            scope
                .split_ascii_whitespace()
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        },
    );
    if values.is_empty()
        || values.len() > 16
        || values
            .iter()
            .any(|scope| !SUPPORTED_MCP_OAUTH_SCOPES.contains(&scope.as_str()))
    {
        return Err(McpOAuthPolicyError::InvalidScope);
    }
    Ok(values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

pub fn validate_mcp_authorize_request(
    params: &BTreeMap<String, String>,
    public_url: &str,
    client_redirect_allowed: impl FnOnce(&str, &str) -> bool,
) -> Result<McpAuthorizeRequest, McpOAuthPolicyError> {
    let client_id = params.get("client_id").cloned().unwrap_or_default();
    let redirect_uri = params.get("redirect_uri").cloned().unwrap_or_default();
    let code_challenge = params.get("code_challenge").cloned().unwrap_or_default();
    let resource = params
        .get("resource")
        .cloned()
        .unwrap_or_else(|| public_url.to_string());
    let scopes = parse_mcp_oauth_scopes(params.get("scope").map(String::as_str))?;
    if !client_redirect_allowed(&client_id, &redirect_uri)
        || params.get("response_type").map(String::as_str) != Some("code")
        || code_challenge.is_empty()
        || params.get("code_challenge_method").map(String::as_str) != Some("S256")
        || resource != public_url
    {
        return Err(McpOAuthPolicyError::InvalidAuthorizationRequest);
    }
    Ok(McpAuthorizeRequest {
        client_id,
        redirect_uri,
        code_challenge,
        scopes,
        resource,
        state: params.get("state").cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_params() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("client_id".to_string(), "client-a".to_string()),
            (
                "redirect_uri".to_string(),
                "https://client.example/callback".to_string(),
            ),
            ("response_type".to_string(), "code".to_string()),
            ("code_challenge".to_string(), "challenge".to_string()),
            ("code_challenge_method".to_string(), "S256".to_string()),
            ("state".to_string(), "client-state".to_string()),
        ])
    }

    #[test]
    fn scopes_have_a_bounded_supported_catalog_and_stable_defaults() {
        assert_eq!(
            parse_mcp_oauth_scopes(None).expect("defaults"),
            vec!["mcp:prompts", "mcp:resources", "mcp:tools"]
        );
        assert_eq!(
            parse_mcp_oauth_scopes(Some("mcp:tools mcp:resources mcp:tools"))
                .expect("deduplicated scopes"),
            vec!["mcp:resources", "mcp:tools"]
        );
        assert_eq!(
            parse_mcp_oauth_scopes(Some("mcp:tools vault:admin")),
            Err(McpOAuthPolicyError::InvalidScope)
        );
        assert_eq!(
            parse_mcp_oauth_scopes(Some(" ")),
            Err(McpOAuthPolicyError::InvalidScope)
        );
    }

    #[test]
    fn authorization_requires_exact_resource_pkce_and_registered_client_redirect() {
        let public_url = "https://mcp.example/personal";
        let allowed = |client: &str, redirect: &str| {
            client == "client-a" && redirect == "https://client.example/callback"
        };
        let params = valid_params();
        let request = validate_mcp_authorize_request(&params, public_url, allowed)
            .expect("valid authorization request");
        assert_eq!(request.resource, public_url);
        assert_eq!(request.state.as_deref(), Some("client-state"));
        for (key, bad) in [
            ("response_type", "token"),
            ("code_challenge_method", "plain"),
            ("code_challenge", ""),
            ("resource", "https://mcp.example/other"),
        ] {
            let mut invalid = valid_params();
            invalid.insert(key.to_string(), bad.to_string());
            assert_eq!(
                validate_mcp_authorize_request(&invalid, public_url, allowed),
                Err(McpOAuthPolicyError::InvalidAuthorizationRequest),
                "{key}"
            );
        }
        assert_eq!(
            validate_mcp_authorize_request(&params, public_url, |_, _| false),
            Err(McpOAuthPolicyError::InvalidAuthorizationRequest)
        );
    }
}
