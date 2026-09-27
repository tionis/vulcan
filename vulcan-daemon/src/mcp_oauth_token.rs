//! Local MCP OAuth token exchange shared by foreground and resident listeners.

use crate::http_policy::mcp_oauth_redirect_uri_allowed;
use crate::mcp_http_codec::{McpHttpRequest, McpHttpResponse};
use crate::mcp_oauth_clients::OAuthClientRegistry;
use crate::mcp_oauth_codes::{redeem_mcp_authorization_code, McpAuthorizationCodeMap};
use crate::mcp_oauth_policy::{
    parse_mcp_oauth_scopes, parse_mcp_token_client_credentials,
    registered_mcp_client_credentials_valid, McpTokenAuthMethod, McpTokenClientCredentials,
};
use crate::mcp_oauth_registration::json_error;
use crate::mcp_remote_runtime::{NamedInitialTokenRequest, NamedMcpRuntime, NamedRefreshRequest};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};
use ulid::Ulid;
use vulcan_core::{fetch_client_id_metadata, ClientIdMetadataDocument, LocalOAuthIssuer};

const ACCESS_TOKEN_LIFETIME_SECONDS: u64 = 900;

/// All authority and durable state needed by the local token endpoint.
pub struct McpLocalTokenEndpoint<'a> {
    pub issuer: &'a LocalOAuthIssuer,
    pub clients: &'a OAuthClientRegistry,
    pub codes: &'a McpAuthorizationCodeMap,
    pub named_runtime: Option<&'a NamedMcpRuntime>,
    pub instance_id: Ulid,
    pub allowed_redirect_hosts: &'a [String],
}

impl McpLocalTokenEndpoint<'_> {
    /// Parse an already bounded form body, authenticate the client, and exchange or refresh.
    #[must_use]
    pub fn handle(
        &self,
        request: &McpHttpRequest,
        params: &BTreeMap<String, String>,
    ) -> McpHttpResponse {
        if request.method != "POST" {
            return json_error(405, "invalid_request", "method not allowed");
        }
        let Some(credentials) = parse_mcp_token_client_credentials(
            request.headers.get("authorization").map(String::as_str),
            params,
        ) else {
            return json_error(401, "invalid_client", "missing OAuth client credentials");
        };
        if !self.client_valid(&credentials) {
            return json_error(401, "invalid_client", "invalid OAuth client credentials");
        }
        if params.get("grant_type").map(String::as_str) == Some("refresh_token") {
            return self.refresh(&credentials.client_id, params);
        }
        if params.get("grant_type").map(String::as_str) != Some("authorization_code") {
            return json_error(400, "unsupported_grant_type", "unsupported grant type");
        }
        let Some(code) = params.get("code") else {
            return json_error(400, "invalid_request", "missing authorization code");
        };
        let Some(verifier) = params.get("code_verifier") else {
            return json_error(400, "invalid_request", "missing PKCE verifier");
        };
        let record = match redeem_mcp_authorization_code(
            self.codes,
            code,
            &credentials.client_id,
            params.get("redirect_uri").map(String::as_str),
            verifier,
        ) {
            Ok(record) => record,
            Err(error) => return json_error(400, "invalid_grant", error.description()),
        };
        let access_token = match self.issuer.issue_access_token_for_authorization(
            &record.subject,
            &record.client_id,
            &record.scopes,
            record.grant_id.clone(),
        ) {
            Ok(token) => token,
            Err(error) => return json_error(500, "server_error", error.to_string()),
        };
        let refresh_token = match (self.named_runtime, &record.grant_id) {
            (Some(named), Some(grant_id)) => {
                let Ok(grant_id) = grant_id.parse::<Ulid>() else {
                    return json_error(500, "server_error", "invalid stored connection grant ID");
                };
                match named.issue_initial_refresh_token(&NamedInitialTokenRequest {
                    remote_instance_id: self.instance_id,
                    grant_id,
                    client_id: &record.client_id,
                    subject: &record.subject,
                    scopes: &record.scopes,
                    resource: &record.resource,
                    now: unix_timestamp(),
                }) {
                    Ok(token) => Some(format!("{}.{}", token.family_id, token.secret.expose())),
                    Err(error) => return json_error(400, "invalid_grant", error.to_string()),
                }
            }
            _ => None,
        };
        token_response(
            &access_token,
            refresh_token,
            &record.scopes,
            &record.resource,
        )
    }

    /// Refresh a named connection after the caller has authenticated its OAuth client.
    #[must_use]
    pub fn refresh(&self, client_id: &str, params: &BTreeMap<String, String>) -> McpHttpResponse {
        let Some(named) = self.named_runtime else {
            return json_error(
                400,
                "unsupported_grant_type",
                "refresh tokens are only available for named remotes",
            );
        };
        let Some((family, secret)) = params
            .get("refresh_token")
            .and_then(|token| token.split_once('.'))
        else {
            return json_error(400, "invalid_grant", "invalid refresh token");
        };
        let Ok(family_id) = family.parse::<Ulid>() else {
            return json_error(400, "invalid_grant", "invalid refresh token");
        };
        let requested_scopes = match params.get("scope") {
            Some(scope) => match parse_mcp_oauth_scopes(Some(scope)) {
                Ok(scopes) => Some(scopes),
                Err(_) => {
                    return json_error(
                        400,
                        "invalid_scope",
                        "requested OAuth scope is empty, unsupported, or too large",
                    );
                }
            },
            None => None,
        };
        let refreshed = match named.refresh_connection(&NamedRefreshRequest {
            remote_instance_id: self.instance_id,
            family_id,
            secret,
            client_id,
            resource: self.issuer.public_url(),
            requested_resource: params.get("resource").map(String::as_str),
            requested_scopes: requested_scopes.as_deref(),
            now: unix_timestamp(),
        }) {
            Ok(refreshed) => refreshed,
            Err(error) => return json_error(400, error.code, error.message),
        };
        let access_token = match self.issuer.issue_access_token_for_authorization(
            &refreshed.subject,
            &refreshed.client_id,
            &refreshed.scopes,
            Some(refreshed.grant_id.to_string()),
        ) {
            Ok(token) => token,
            Err(error) => return json_error(500, "server_error", error.to_string()),
        };
        token_response(
            &access_token,
            Some(format!(
                "{}.{}",
                refreshed.refresh_token.family_id,
                refreshed.refresh_token.secret.expose()
            )),
            &refreshed.scopes,
            &refreshed.audience,
        )
    }

    #[must_use]
    pub fn client_valid(&self, credentials: &McpTokenClientCredentials) -> bool {
        (credentials.method != McpTokenAuthMethod::None
            && self
                .issuer
                .verify_client(&credentials.client_id, &credentials.client_secret))
            || self
                .clients
                .get(&credentials.client_id)
                .ok()
                .flatten()
                .is_some_and(|client| {
                    registered_mcp_client_credentials_valid(
                        credentials,
                        &client.token_endpoint_auth_method,
                        &client.client_secret,
                    )
                })
            || (credentials.method == McpTokenAuthMethod::None
                && client_id_metadata_valid(
                    &credentials.client_id,
                    None,
                    self.allowed_redirect_hosts,
                ))
    }
}

#[must_use]
pub fn client_id_metadata_valid(
    client_id: &str,
    redirect_uri: Option<&str>,
    allowed_redirect_hosts: &[String],
) -> bool {
    fetch_client_id_metadata(client_id).is_ok_and(|metadata| {
        validate_client_id_metadata(client_id, redirect_uri, &metadata, allowed_redirect_hosts)
    })
}

#[must_use]
pub fn validate_client_id_metadata(
    client_id: &str,
    redirect_uri: Option<&str>,
    metadata: &ClientIdMetadataDocument,
    allowed_redirect_hosts: &[String],
) -> bool {
    metadata.client_id == client_id
        && metadata.token_endpoint_auth_method == "none"
        && !metadata.redirect_uris.is_empty()
        && metadata
            .redirect_uris
            .iter()
            .all(|uri| mcp_oauth_redirect_uri_allowed(uri, allowed_redirect_hosts))
        && redirect_uri
            .is_none_or(|redirect| metadata.redirect_uris.iter().any(|uri| uri == redirect))
}

fn token_response(
    access_token: &str,
    refresh_token: Option<String>,
    scopes: &[String],
    resource: &str,
) -> McpHttpResponse {
    let mut body = serde_json::json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": ACCESS_TOKEN_LIFETIME_SECONDS,
        "scope": scopes.join(" "),
        "resource": resource,
    });
    if let Some(refresh_token) = refresh_token {
        body["refresh_token"] = Value::String(refresh_token);
    }
    McpHttpResponse {
        status: 200,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&body).expect("token response JSON"),
        extra_headers: vec![("Cache-Control".to_string(), "no-store".to_string())],
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_oauth_codes::{issue_mcp_authorization_code, McpAuthorizationCode};
    use std::time::Instant;
    use vulcan_core::LocalOAuthIssuerConfig;

    fn issuer() -> LocalOAuthIssuer {
        LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://mcp.example.test/mcp".to_string(),
            client_id: "static-client".to_string(),
            client_secret: "static-secret".to_string(),
            signing_key: "test-signing-key".to_string(),
            approval_token: String::new(),
            subject: "https://identity.example.test/me".to_string(),
            email: None,
            users: Vec::new(),
            dcr_enabled: true,
        })
        .expect("issuer")
    }

    fn request() -> McpHttpRequest {
        McpHttpRequest {
            method: "POST".to_string(),
            path: "/oauth/token".to_string(),
            query: String::new(),
            headers: BTreeMap::new(),
            body: Vec::new(),
        }
    }

    #[test]
    fn code_exchange_is_one_time_and_keeps_scope_resource_response() {
        let issuer = issuer();
        let clients = OAuthClientRegistry::ephemeral();
        let codes = McpAuthorizationCodeMap::default();
        let endpoint = McpLocalTokenEndpoint {
            issuer: &issuer,
            clients: &clients,
            codes: &codes,
            named_runtime: None,
            instance_id: Ulid::new(),
            allowed_redirect_hosts: &[],
        };
        let verifier = "test-verifier";
        let code = issue_mcp_authorization_code(
            &codes,
            McpAuthorizationCode {
                client_id: "static-client".to_string(),
                redirect_uri: "https://client.example.test/callback".to_string(),
                code_challenge: vulcan_core::pkce_s256_challenge(verifier),
                subject: "https://identity.example.test/me".to_string(),
                scopes: vec!["mcp:tools".to_string()],
                resource: "https://mcp.example.test/mcp".to_string(),
                grant_id: None,
                grant_required: false,
                expires_at: Instant::now(),
            },
        )
        .expect("code");
        let params = BTreeMap::from([
            ("client_id".to_string(), "static-client".to_string()),
            ("client_secret".to_string(), "static-secret".to_string()),
            ("grant_type".to_string(), "authorization_code".to_string()),
            ("code".to_string(), code),
            ("code_verifier".to_string(), verifier.to_string()),
            (
                "redirect_uri".to_string(),
                "https://client.example.test/callback".to_string(),
            ),
        ]);
        let success = endpoint.handle(&request(), &params);
        assert_eq!(success.status, 200);
        let body: Value = serde_json::from_slice(&success.body).expect("token JSON");
        assert_eq!(body["scope"], "mcp:tools");
        assert_eq!(body["resource"], "https://mcp.example.test/mcp");
        assert!(body.get("refresh_token").is_none());
        assert_eq!(endpoint.handle(&request(), &params).status, 400);
    }

    #[test]
    fn client_id_metadata_requires_public_method_exact_id_and_allowed_redirects() {
        let hosts = ["client.example.test".to_string()];
        let metadata = ClientIdMetadataDocument {
            client_id: "https://client.example.test/client.json".to_string(),
            redirect_uris: vec!["https://client.example.test/callback".to_string()],
            token_endpoint_auth_method: "none".to_string(),
        };
        assert!(validate_client_id_metadata(
            &metadata.client_id,
            Some("https://client.example.test/callback"),
            &metadata,
            &hosts,
        ));
        assert!(!validate_client_id_metadata(
            "https://other.example.test/client.json",
            Some("https://client.example.test/callback"),
            &metadata,
            &hosts,
        ));
        assert!(!validate_client_id_metadata(
            &metadata.client_id,
            Some("https://client.example.test/other"),
            &metadata,
            &hosts,
        ));
    }
}
