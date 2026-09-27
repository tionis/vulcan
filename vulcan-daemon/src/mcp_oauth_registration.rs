//! HTTP dynamic-client registration shared by foreground and resident MCP listeners.

use crate::mcp_http_codec::{McpHttpRequest, McpHttpResponse};
use crate::mcp_oauth_clients::{OAuthClientRegistry, RegisteredOAuthClient};
use crate::mcp_oauth_policy::{validate_mcp_dcr_registration, McpDcrError};
use base64::prelude::{Engine, BASE64_URL_SAFE_NO_PAD};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use ulid::Ulid;

const CLIENT_SECRET_BYTES: usize = 32;

/// Register a client only after validating metadata and durably publishing its credentials.
#[must_use]
pub fn register_mcp_oauth_client(
    request: &McpHttpRequest,
    clients: &OAuthClientRegistry,
    enabled: bool,
    allowed_redirect_hosts: &[String],
    refresh_supported: bool,
) -> McpHttpResponse {
    if !enabled {
        return json_error(404, "invalid_request", "DCR is not enabled");
    }
    if request.method != "POST" {
        return json_error(405, "invalid_request", "method not allowed");
    }
    let Ok(payload) = serde_json::from_slice::<Value>(&request.body) else {
        return json_error(400, "invalid_client_metadata", "invalid JSON");
    };
    let registration =
        match validate_mcp_dcr_registration(&payload, allowed_redirect_hosts, refresh_supported) {
            Ok(registration) => registration,
            Err(McpDcrError::InvalidRedirectUri) => {
                return json_error(400, "invalid_redirect_uri", "redirect URI is not allowed");
            }
            Err(error) => {
                let message = match error {
                    McpDcrError::InvalidAuthMethod => "unsupported token endpoint auth method",
                    McpDcrError::InvalidClientName => "invalid client name",
                    McpDcrError::InvalidGrantTypes => "unsupported grant types",
                    McpDcrError::InvalidResponseTypes => "unsupported response types",
                    McpDcrError::InvalidRedirectUri => unreachable!(),
                };
                return json_error(400, "invalid_client_metadata", message);
            }
        };
    let client_secret = if registration.token_endpoint_auth_method == "none" {
        String::new()
    } else {
        let mut bytes = [0_u8; CLIENT_SECRET_BYTES];
        if let Err(error) = getrandom::fill(&mut bytes) {
            return json_error(500, "server_error", error.to_string());
        }
        BASE64_URL_SAFE_NO_PAD.encode(bytes)
    };
    let client = RegisteredOAuthClient {
        client_id: format!("vulcan-dcr-{}", Ulid::new()),
        client_secret,
        redirect_uris: registration.redirect_uris,
        client_name: registration.client_name,
        token_endpoint_auth_method: registration.token_endpoint_auth_method,
        client_id_issued_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs()),
    };
    if let Err(error) = clients.register(client.clone()) {
        return json_error(500, "server_error", error.to_string());
    }
    let grant_types = if refresh_supported {
        vec!["authorization_code", "refresh_token"]
    } else {
        vec!["authorization_code"]
    };
    let mut body = serde_json::json!({
        "client_id": client.client_id,
        "client_id_issued_at": client.client_id_issued_at,
        "redirect_uris": client.redirect_uris,
        "grant_types": grant_types,
        "response_types": ["code"],
        "token_endpoint_auth_method": client.token_endpoint_auth_method,
    });
    if !client.client_secret.is_empty() {
        body["client_secret"] = Value::String(client.client_secret);
        body["client_secret_expires_at"] = Value::from(0);
    }
    McpHttpResponse {
        status: 201,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&body).expect("registration response JSON"),
        extra_headers: vec![("Cache-Control".to_string(), "no-store".to_string())],
    }
}

pub(crate) fn json_error(
    status: u16,
    error: &str,
    description: impl Into<String>,
) -> McpHttpResponse {
    McpHttpResponse {
        status,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&serde_json::json!({
            "error": error,
            "error_description": description.into(),
        }))
        .expect("OAuth error JSON"),
        extra_headers: vec![("Cache-Control".to_string(), "no-store".to_string())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn request(method: &str, auth_method: &str) -> McpHttpRequest {
        McpHttpRequest {
            method: method.to_string(),
            path: "/oauth/register".to_string(),
            query: String::new(),
            headers: BTreeMap::new(),
            body: serde_json::to_vec(&serde_json::json!({
                "redirect_uris": ["https://client.example.test/callback"],
                "token_endpoint_auth_method": auth_method,
            }))
            .expect("request JSON"),
        }
    }

    #[test]
    fn confidential_registration_persists_full_strength_secret_and_refresh_metadata() {
        let clients = OAuthClientRegistry::ephemeral();
        let response = register_mcp_oauth_client(
            &request("POST", "client_secret_basic"),
            &clients,
            true,
            &["client.example.test".to_string()],
            true,
        );
        assert_eq!(response.status, 201);
        let body: Value = serde_json::from_slice(&response.body).expect("response JSON");
        assert_eq!(
            body["grant_types"],
            serde_json::json!(["authorization_code", "refresh_token"])
        );
        let secret = body["client_secret"].as_str().expect("secret");
        assert_eq!(
            BASE64_URL_SAFE_NO_PAD
                .decode(secret)
                .expect("base64 secret")
                .len(),
            CLIENT_SECRET_BYTES
        );
        let persisted = clients
            .get(body["client_id"].as_str().expect("client ID"))
            .expect("client lookup")
            .expect("registered client");
        assert_eq!(persisted.client_secret, secret);
    }

    #[test]
    fn public_registration_has_no_secret_and_disabled_or_invalid_requests_do_not_persist() {
        let clients = OAuthClientRegistry::ephemeral();
        let hosts = ["client.example.test".to_string()];
        assert_eq!(
            register_mcp_oauth_client(&request("POST", "none"), &clients, false, &hosts, true)
                .status,
            404
        );
        assert_eq!(
            register_mcp_oauth_client(&request("GET", "none"), &clients, true, &hosts, true).status,
            405
        );
        assert!(clients.list().expect("clients").is_empty());
        let response =
            register_mcp_oauth_client(&request("POST", "none"), &clients, true, &hosts, false);
        assert_eq!(response.status, 201);
        let body: Value = serde_json::from_slice(&response.body).expect("response JSON");
        assert!(body.get("client_secret").is_none());
        assert_eq!(
            body["grant_types"],
            serde_json::json!(["authorization_code"])
        );
    }
}
