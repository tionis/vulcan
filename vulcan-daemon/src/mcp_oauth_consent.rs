//! Local MCP OAuth consent application shared by foreground and resident listeners.

use std::collections::BTreeMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ulid::Ulid;
use vulcan_core::LocalOAuthIssuer;

use crate::mcp_http_codec::McpHttpResponse;
use crate::mcp_oauth_browser::{
    client_redirect, consume_consent, percent_encode, ConsentError, PendingConsent,
    PendingConsentMap,
};
use crate::mcp_oauth_codes::{
    bind_mcp_authorization_code_grant, discard_mcp_authorization_code,
    issue_mcp_authorization_code, McpAuthorizationCode, McpAuthorizationCodeMap, McpCodeIssueError,
};
use crate::mcp_oauth_registration::json_error;
use crate::mcp_remote_runtime::{NamedConsentRequest, NamedMcpRuntime};

pub struct McpConsentEndpoint<'a> {
    pub issuer: &'a LocalOAuthIssuer,
    pub pending: &'a PendingConsentMap,
    pub codes: &'a McpAuthorizationCodeMap,
    pub named_runtime: Option<&'a NamedMcpRuntime>,
    pub instance_id: Ulid,
}

impl McpConsentEndpoint<'_> {
    #[must_use]
    pub fn handle(&self, method: &str, params: &BTreeMap<String, String>) -> McpHttpResponse {
        if method != "POST" {
            return plain_response(405, "consent requires POST");
        }
        let transaction_id = params.get("transaction").map_or("", String::as_str);
        let csrf_token = params.get("csrf_token").map_or("", String::as_str);
        let decision = params.get("decision").map_or("", String::as_str);
        let pending = match consume_consent(self.pending, transaction_id, csrf_token, decision) {
            Ok(pending) => pending,
            Err(ConsentError::Unknown) => {
                return plain_response(400, "unknown consent transaction");
            }
            Err(ConsentError::Expired) => {
                return plain_response(400, "expired consent transaction");
            }
            Err(ConsentError::InvalidCsrf) => {
                return plain_response(403, "invalid consent CSRF token");
            }
            Err(ConsentError::InvalidDecision) => {
                return plain_response(400, "consent decision must be approve or deny");
            }
        };
        if decision == "deny" {
            return client_redirect(
                &pending.redirect_uri,
                "error=access_denied",
                pending.state.as_deref(),
            );
        }
        let Some(user) = self.issuer.user_for_subject(&pending.subject) else {
            return plain_response(403, "consent subject is no longer authorized");
        };
        let code = match issue_mcp_authorization_code(
            self.codes,
            McpAuthorizationCode {
                client_id: pending.client_id.clone(),
                redirect_uri: pending.redirect_uri.clone(),
                code_challenge: pending.code_challenge.clone(),
                subject: user.subject,
                scopes: pending.scopes.clone(),
                resource: pending.resource.clone(),
                grant_id: None,
                grant_required: self.named_runtime.is_some(),
                expires_at: Instant::now(),
            },
        ) {
            Ok(code) => code,
            Err(McpCodeIssueError::Capacity) => {
                return json_error(
                    503,
                    "temporarily_unavailable",
                    "too many pending authorization codes",
                );
            }
            Err(McpCodeIssueError::Random) => {
                return json_error(500, "server_error", "could not generate authorization code");
            }
        };
        let grant_id = match self.create_named_grant(&pending, params) {
            Ok(grant_id) => grant_id,
            Err(response) => {
                discard_mcp_authorization_code(self.codes, &code);
                return response;
            }
        };
        if let Some(grant_id) = grant_id {
            if !bind_mcp_authorization_code_grant(self.codes, &code, grant_id.clone()) {
                if let (Some(named), Ok(id)) = (self.named_runtime, grant_id.parse()) {
                    let _ =
                        named
                            .authorization_store
                            .revoke_grant(id, current_unix_timestamp(), false);
                }
                return json_error(
                    500,
                    "server_error",
                    "authorization code expired before consent completed",
                );
            }
        }
        client_redirect(
            &pending.redirect_uri,
            &format!("code={}", percent_encode(&code)),
            pending.state.as_deref(),
        )
    }

    pub fn create_named_grant(
        &self,
        pending: &PendingConsent,
        params: &BTreeMap<String, String>,
    ) -> Result<Option<String>, McpHttpResponse> {
        let Some(named) = self.named_runtime else {
            return Ok(None);
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| plain_response(500, &error.to_string()))?
            .as_secs();
        let id = named
            .create_connection_grant(&NamedConsentRequest {
                remote_instance_id: self.instance_id,
                client_id: &pending.client_id,
                subject: &pending.subject,
                scopes: &pending.scopes,
                resource: &pending.resource,
                form: params,
                now,
            })
            .map_err(|error| plain_response(error.status, &error.message))?;
        Ok(Some(id.to_string()))
    }
}

fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn plain_response(status: u16, message: &str) -> McpHttpResponse {
    McpHttpResponse {
        status,
        content_type: Some("text/plain; charset=utf-8"),
        body: message.as_bytes().to_vec(),
        extra_headers: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use vulcan_core::LocalOAuthIssuerConfig;

    fn issuer() -> LocalOAuthIssuer {
        LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://remote.example/mcp".into(),
            client_id: "static-client".into(),
            client_secret: "client-secret".into(),
            signing_key: "signing-key".into(),
            approval_token: String::new(),
            subject: "https://identity.example/alice".into(),
            email: None,
            users: Vec::new(),
            dcr_enabled: true,
        })
        .expect("issuer")
    }

    fn pending() -> PendingConsent {
        PendingConsent {
            client_id: "static-client".into(),
            redirect_uri: "https://client.example/callback".into(),
            code_challenge: "challenge".into(),
            subject: "https://identity.example/alice".into(),
            scopes: vec!["mcp:tools".into()],
            resource: "https://remote.example/mcp".into(),
            state: Some("client-state".into()),
            csrf_token: "secret".into(),
            expires_at: Instant::now() + Duration::from_secs(60),
        }
    }

    #[test]
    fn approval_issues_one_code_and_replay_cannot_issue_another() {
        let issuer = issuer();
        let pending_map = PendingConsentMap::default();
        let codes = McpAuthorizationCodeMap::default();
        pending_map
            .lock()
            .unwrap()
            .insert("transaction".into(), pending());
        let endpoint = McpConsentEndpoint {
            issuer: &issuer,
            pending: &pending_map,
            codes: &codes,
            named_runtime: None,
            instance_id: Ulid::new(),
        };
        let params = BTreeMap::from([
            ("transaction".into(), "transaction".into()),
            ("csrf_token".into(), "secret".into()),
            ("decision".into(), "approve".into()),
        ]);
        let response = endpoint.handle("POST", &params);
        assert_eq!(response.status, 302);
        assert_eq!(codes.lock().unwrap().len(), 1);
        assert_eq!(endpoint.handle("POST", &params).status, 400);
        assert_eq!(codes.lock().unwrap().len(), 1);
    }

    #[test]
    fn invalid_csrf_retains_transaction_and_denial_issues_no_code() {
        let issuer = issuer();
        let pending_map = PendingConsentMap::default();
        let codes = McpAuthorizationCodeMap::default();
        pending_map
            .lock()
            .unwrap()
            .insert("transaction".into(), pending());
        let endpoint = McpConsentEndpoint {
            issuer: &issuer,
            pending: &pending_map,
            codes: &codes,
            named_runtime: None,
            instance_id: Ulid::new(),
        };
        let mut params = BTreeMap::from([
            ("transaction".into(), "transaction".into()),
            ("csrf_token".into(), "wrong".into()),
            ("decision".into(), "deny".into()),
        ]);
        assert_eq!(endpoint.handle("POST", &params).status, 403);
        assert_eq!(pending_map.lock().unwrap().len(), 1);
        params.insert("csrf_token".into(), "secret".into());
        assert_eq!(endpoint.handle("POST", &params).status, 302);
        assert!(codes.lock().unwrap().is_empty());
    }
}
