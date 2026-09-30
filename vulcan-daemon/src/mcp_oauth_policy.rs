//! Transport-neutral policy for the local MCP OAuth authorization server.

use crate::http_policy::mcp_oauth_redirect_uri_allowed;
use base64::prelude::{Engine, BASE64_STANDARD};
use serde_json::Value;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTokenAuthMethod {
    ClientSecretBasic,
    ClientSecretPost,
    None,
}

impl McpTokenAuthMethod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClientSecretBasic => "client_secret_basic",
            Self::ClientSecretPost => "client_secret_post",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTokenClientCredentials {
    pub client_id: String,
    pub client_secret: String,
    pub method: McpTokenAuthMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpDcrError {
    InvalidRedirectUri,
    InvalidAuthMethod,
    InvalidClientName,
    InvalidGrantTypes,
    InvalidResponseTypes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpDcrRegistration {
    pub redirect_uris: Vec<String>,
    pub client_name: Option<String>,
    pub token_endpoint_auth_method: String,
}

pub fn validate_mcp_dcr_registration(
    payload: &Value,
    allowed_redirect_hosts: &[String],
    allow_refresh: bool,
) -> Result<McpDcrRegistration, McpDcrError> {
    let uris = payload
        .get("redirect_uris")
        .and_then(Value::as_array)
        .ok_or(McpDcrError::InvalidRedirectUri)?;
    if uris.is_empty() || uris.len() > 16 {
        return Err(McpDcrError::InvalidRedirectUri);
    }
    let mut redirect_uris = Vec::with_capacity(uris.len());
    let mut unique = BTreeSet::new();
    for value in uris {
        let uri = value.as_str().ok_or(McpDcrError::InvalidRedirectUri)?;
        if !mcp_oauth_redirect_uri_allowed(uri, allowed_redirect_hosts) || !unique.insert(uri) {
            return Err(McpDcrError::InvalidRedirectUri);
        }
        redirect_uris.push(uri.to_string());
    }
    let auth_method = match payload.get("token_endpoint_auth_method") {
        Some(value) => value.as_str().ok_or(McpDcrError::InvalidAuthMethod)?,
        None => "client_secret_basic",
    };
    if !matches!(
        auth_method,
        "none" | "client_secret_basic" | "client_secret_post"
    ) {
        return Err(McpDcrError::InvalidAuthMethod);
    }
    let client_name = match payload.get("client_name") {
        Some(value) => {
            let name = value.as_str().ok_or(McpDcrError::InvalidClientName)?;
            if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                return Err(McpDcrError::InvalidClientName);
            }
            Some(name.to_string())
        }
        None => None,
    };
    validate_dcr_string_array(payload, "response_types", &["code"], false)
        .map_err(|()| McpDcrError::InvalidResponseTypes)?;
    let allowed_grants = if allow_refresh {
        &["authorization_code", "refresh_token"][..]
    } else {
        &["authorization_code"][..]
    };
    validate_dcr_string_array(payload, "grant_types", allowed_grants, true)
        .map_err(|()| McpDcrError::InvalidGrantTypes)?;
    Ok(McpDcrRegistration {
        redirect_uris,
        client_name,
        token_endpoint_auth_method: auth_method.to_string(),
    })
}

fn validate_dcr_string_array(
    payload: &Value,
    field: &str,
    supported: &[&str],
    require_authorization_code: bool,
) -> Result<(), ()> {
    let Some(value) = payload.get(field) else {
        return Ok(());
    };
    let values = value.as_array().ok_or(())?;
    if values.is_empty() || values.len() > supported.len() {
        return Err(());
    }
    let mut seen = BTreeSet::new();
    for value in values {
        let item = value.as_str().ok_or(())?;
        if !supported.contains(&item) || !seen.insert(item) {
            return Err(());
        }
    }
    if require_authorization_code && !seen.contains("authorization_code") {
        return Err(());
    }
    Ok(())
}

/// A token request uses exactly one client authentication method. A malformed
/// Authorization header never falls back to body credentials.
#[must_use]
pub fn parse_mcp_token_client_credentials(
    authorization: Option<&str>,
    params: &BTreeMap<String, String>,
) -> Option<McpTokenClientCredentials> {
    // Signed assertions are not implemented. Never silently reinterpret one
    // as a public-client request or mix it with a supported secret method.
    if params.contains_key("client_assertion") || params.contains_key("client_assertion_type") {
        return None;
    }
    if let Some(authorization) = authorization {
        if params.contains_key("client_id") || params.contains_key("client_secret") {
            return None;
        }
        let encoded = authorization.strip_prefix("Basic ")?;
        let decoded = String::from_utf8(BASE64_STANDARD.decode(encoded).ok()?).ok()?;
        let (client_id, client_secret) = decoded.split_once(':')?;
        if client_id.is_empty() || client_secret.is_empty() {
            return None;
        }
        return Some(McpTokenClientCredentials {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            method: McpTokenAuthMethod::ClientSecretBasic,
        });
    }
    let client_id = params.get("client_id")?;
    if client_id.is_empty() {
        return None;
    }
    let (method, client_secret) = match params.get("client_secret") {
        Some(secret) if !secret.is_empty() => {
            (McpTokenAuthMethod::ClientSecretPost, secret.clone())
        }
        Some(_) => return None,
        None => (McpTokenAuthMethod::None, String::new()),
    };
    Some(McpTokenClientCredentials {
        client_id: client_id.clone(),
        client_secret,
        method,
    })
}

#[must_use]
pub fn registered_mcp_client_credentials_valid(
    credentials: &McpTokenClientCredentials,
    declared_method: &str,
    stored_secret: &str,
) -> bool {
    credentials.method.as_str() == declared_method
        && match credentials.method {
            McpTokenAuthMethod::None => stored_secret.is_empty(),
            _ => {
                !credentials.client_secret.is_empty() && credentials.client_secret == stored_secret
            }
        }
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

    #[test]
    fn dcr_metadata_rejects_partial_or_unsupported_registration() {
        let hosts = vec!["chatgpt.com".to_string()];
        let valid = serde_json::json!({
            "redirect_uris": ["https://chatgpt.com/callback"],
            "client_name": "Example client",
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
        });
        let registration =
            validate_mcp_dcr_registration(&valid, &hosts, true).expect("valid public registration");
        assert_eq!(registration.token_endpoint_auth_method, "none");
        for (field, invalid, error) in [
            (
                "redirect_uris",
                serde_json::json!(["https://chatgpt.com/callback", 7]),
                McpDcrError::InvalidRedirectUri,
            ),
            (
                "redirect_uris",
                serde_json::json!([
                    "https://chatgpt.com/callback",
                    "https://chatgpt.com/callback"
                ]),
                McpDcrError::InvalidRedirectUri,
            ),
            (
                "token_endpoint_auth_method",
                serde_json::json!(7),
                McpDcrError::InvalidAuthMethod,
            ),
            (
                "client_name",
                serde_json::json!(7),
                McpDcrError::InvalidClientName,
            ),
            (
                "grant_types",
                serde_json::json!(["implicit"]),
                McpDcrError::InvalidGrantTypes,
            ),
            (
                "response_types",
                serde_json::json!(["token"]),
                McpDcrError::InvalidResponseTypes,
            ),
        ] {
            let mut payload = valid.clone();
            payload[field] = invalid;
            assert_eq!(
                validate_mcp_dcr_registration(&payload, &hosts, true),
                Err(error),
                "{field}"
            );
        }
        assert_eq!(
            validate_mcp_dcr_registration(&valid, &hosts, false),
            Err(McpDcrError::InvalidGrantTypes)
        );
    }

    #[test]
    fn unsupported_client_assertions_never_fall_back_to_public_or_secret_authentication() {
        let basic = format!("Basic {}", BASE64_STANDARD.encode("client:secret"));
        for (authorization, mut params) in [
            (
                None,
                BTreeMap::from([("client_id".into(), "client".into())]),
            ),
            (
                None,
                BTreeMap::from([
                    ("client_id".into(), "client".into()),
                    ("client_secret".into(), "secret".into()),
                ]),
            ),
            (Some(basic.as_str()), BTreeMap::new()),
        ] {
            assert!(parse_mcp_token_client_credentials(authorization, &params).is_some());
            for field in ["client_assertion", "client_assertion_type"] {
                params.insert(field.into(), "unsupported-assertion".into());
                assert!(parse_mcp_token_client_credentials(authorization, &params).is_none());
                params.remove(field);
            }
        }
    }

    #[test]
    fn token_client_authentication_rejects_mixed_and_mismatched_methods() {
        let basic = format!("Basic {}", BASE64_STANDARD.encode("client-a:secret"));
        let basic_credentials = parse_mcp_token_client_credentials(Some(&basic), &BTreeMap::new())
            .expect("basic credentials");
        assert_eq!(
            basic_credentials.method,
            McpTokenAuthMethod::ClientSecretBasic
        );
        assert!(registered_mcp_client_credentials_valid(
            &basic_credentials,
            "client_secret_basic",
            "secret"
        ));
        assert!(!registered_mcp_client_credentials_valid(
            &basic_credentials,
            "client_secret_post",
            "secret"
        ));
        let post = BTreeMap::from([
            ("client_id".to_string(), "client-a".to_string()),
            ("client_secret".to_string(), "secret".to_string()),
        ]);
        let post_credentials =
            parse_mcp_token_client_credentials(None, &post).expect("post credentials");
        assert_eq!(
            post_credentials.method,
            McpTokenAuthMethod::ClientSecretPost
        );
        assert!(registered_mcp_client_credentials_valid(
            &post_credentials,
            "client_secret_post",
            "secret"
        ));
        assert!(parse_mcp_token_client_credentials(Some(&basic), &post).is_none());
        assert!(parse_mcp_token_client_credentials(Some("Bearer invalid"), &post).is_none());

        let public = BTreeMap::from([("client_id".to_string(), "client-public".to_string())]);
        let public_credentials =
            parse_mcp_token_client_credentials(None, &public).expect("public credentials");
        assert_eq!(public_credentials.method, McpTokenAuthMethod::None);
        assert!(registered_mcp_client_credentials_valid(
            &public_credentials,
            "none",
            ""
        ));
        assert!(!registered_mcp_client_credentials_valid(
            &post_credentials,
            "none",
            ""
        ));
    }

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
