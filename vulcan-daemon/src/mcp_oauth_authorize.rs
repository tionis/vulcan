//! MCP OAuth authorization start and `IndieAuth` callback shared by listener modes.

use std::collections::BTreeMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use vulcan_core::{exchange_indieauth_code, pkce_s256_challenge, LocalOAuthIssuer, OAuthError};

use crate::mcp_http_codec::McpHttpResponse;
use crate::mcp_oauth_browser::{
    begin_consent, begin_indieauth, redirect_to_indieauth, take_indieauth, BeginError, ConsentPage,
    IndieAuthConfig, PendingConsent, PendingConsentMap, PendingIndieAuth, PendingIndieAuthMap,
    TakeError,
};
use crate::mcp_oauth_clients::OAuthClientRegistry;
use crate::mcp_oauth_codes::McpAuthorizationCodeMap;
use crate::mcp_oauth_policy::{
    validate_mcp_authorize_request, McpAuthorizeRequest, McpOAuthPolicyError,
};
use crate::mcp_oauth_registration::json_error;
use crate::mcp_oauth_token::client_id_metadata_valid;
use crate::mcp_remote_runtime::NamedMcpRuntime;

pub type IndieAuthExchange = fn(&str, &str, &str, &str, &str) -> Result<String, OAuthError>;

pub struct McpAuthorizeEndpoint<'a> {
    pub issuer: &'a LocalOAuthIssuer,
    pub clients: &'a OAuthClientRegistry,
    pub codes: &'a McpAuthorizationCodeMap,
    pub pending_indieauth: &'a PendingIndieAuthMap,
    pub pending_consent: &'a PendingConsentMap,
    pub indieauth: Option<&'a IndieAuthConfig>,
    pub named_runtime: Option<&'a NamedMcpRuntime>,
    pub requested_profile: Option<String>,
    pub fallback_vault_root: String,
    pub allowed_redirect_hosts: &'a [String],
    pub exchange: IndieAuthExchange,
}

impl McpAuthorizeEndpoint<'_> {
    #[must_use]
    pub fn authorize(&self, method: &str, params: &BTreeMap<String, String>) -> McpHttpResponse {
        if method != "GET" {
            return plain_response(405, "method not allowed");
        }
        let McpAuthorizeRequest {
            client_id,
            redirect_uri,
            code_challenge,
            scopes,
            resource,
            state: client_state,
        } = match validate_mcp_authorize_request(
            params,
            self.issuer.public_url(),
            |client, redirect| self.client_redirect_allowed(client, redirect),
        ) {
            Ok(validated) => validated,
            Err(error) => return policy_error_response(error),
        };
        if let Some(indieauth) = self.indieauth {
            let verifier = match random_verifier() {
                Ok(verifier) => verifier,
                Err(error) => return browser_begin_error_response(error),
            };
            let challenge = pkce_s256_challenge(&verifier);
            let state = match begin_indieauth(
                self.pending_indieauth,
                PendingIndieAuth {
                    client_id,
                    redirect_uri,
                    code_challenge,
                    scopes,
                    resource,
                    indieauth_code_verifier: verifier,
                    state: client_state,
                    expires_at: Instant::now(),
                },
            ) {
                Ok(state) => state,
                Err(error) => return browser_begin_error_response(error),
            };
            return redirect_to_indieauth(indieauth, &state, &challenge);
        }
        // Every listener with OAuth signs its user in with IndieAuth.
        plain_response(503, "sign-in is not configured for this listener")
    }

    #[must_use]
    pub fn callback(&self, method: &str, params: &BTreeMap<String, String>) -> McpHttpResponse {
        if method != "GET" {
            return plain_response(405, "method not allowed");
        }
        let Some(indieauth) = self.indieauth else {
            return plain_response(404, "not found");
        };
        if let Some(error) = params.get("error") {
            return plain_response(400, &format!("IndieAuth failed: {error}"));
        }
        let Some(state) = params.get("state") else {
            return plain_response(400, "missing IndieAuth state");
        };
        let pending = match take_indieauth(self.pending_indieauth, state) {
            Ok(pending) => pending,
            Err(TakeError::Unknown) => return plain_response(400, "unknown IndieAuth state"),
            Err(TakeError::Expired) => return plain_response(400, "expired IndieAuth state"),
        };
        let Some(code) = params.get("code") else {
            return plain_response(400, "missing IndieAuth code");
        };
        let subject = match (self.exchange)(
            &indieauth.token_endpoint,
            code,
            &indieauth.redirect_uri,
            &indieauth.client_id,
            &pending.indieauth_code_verifier,
        ) {
            Ok(subject) => subject,
            Err(error) => return plain_response(400, &error.to_string()),
        };
        if !self.issuer.subject_allowed(&subject) {
            return subject_not_allowed_response(&subject);
        }
        self.begin_consent(pending, subject)
    }

    fn begin_consent(&self, pending: PendingIndieAuth, subject: String) -> McpHttpResponse {
        let consent = PendingConsent {
            client_id: pending.client_id,
            redirect_uri: pending.redirect_uri,
            code_challenge: pending.code_challenge,
            subject,
            scopes: pending.scopes,
            resource: pending.resource,
            state: pending.state,
            csrf_token: String::new(),
            expires_at: Instant::now(),
        };
        let (transaction_id, consent) = match begin_consent(self.pending_consent, consent) {
            Ok(transaction) => transaction,
            Err(error) => return browser_begin_error_response(error),
        };
        self.render_consent(&transaction_id, &consent)
    }

    #[must_use]
    pub fn render_consent(
        &self,
        transaction_id: &str,
        pending: &PendingConsent,
    ) -> McpHttpResponse {
        let profile = self
            .requested_profile
            .clone()
            .unwrap_or_else(|| "readonly".to_string());
        let client_name = self
            .clients
            .get(&pending.client_id)
            .ok()
            .flatten()
            .and_then(|client| client.client_name)
            .unwrap_or_else(|| pending.client_id.clone());
        crate::mcp_oauth_browser::render_consent_page(&ConsentPage {
            transaction_id,
            pending,
            client_name: &client_name,
            fallback_vault_root: &self.fallback_vault_root,
            profile: &profile,
            named_runtime: self.named_runtime,
            clock: crate::mcp_grant_expiry::GrantClock::local(current_unix_seconds()),
        })
    }

    #[must_use]
    pub fn client_redirect_allowed(&self, client_id: &str, redirect_uri: &str) -> bool {
        self.clients
            .get(client_id)
            .ok()
            .flatten()
            .is_some_and(|client| client.redirect_uris.iter().any(|uri| uri == redirect_uri))
            || client_id_metadata_valid(client_id, Some(redirect_uri), self.allowed_redirect_hosts)
    }
}

pub fn default_indieauth_exchange(
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    code_verifier: &str,
) -> Result<String, OAuthError> {
    exchange_indieauth_code(token_endpoint, code, redirect_uri, client_id, code_verifier)
}

fn random_verifier() -> Result<String, BeginError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| BeginError::Random)?;
    Ok(base64::prelude::BASE64_URL_SAFE_NO_PAD.encode(bytes))
}

fn browser_begin_error_response(error: BeginError) -> McpHttpResponse {
    match error {
        BeginError::Capacity => json_error(
            503,
            "temporarily_unavailable",
            "too many pending browser authorization transactions",
        ),
        BeginError::Random => json_error(
            500,
            "server_error",
            "could not create browser authorization transaction",
        ),
    }
}

fn policy_error_response(error: McpOAuthPolicyError) -> McpHttpResponse {
    match error {
        McpOAuthPolicyError::InvalidScope => json_error(
            400,
            "invalid_scope",
            "requested OAuth scope is empty, unsupported, or too large",
        ),
        McpOAuthPolicyError::InvalidAuthorizationRequest => {
            plain_response(400, "invalid OAuth authorization request")
        }
    }
}

fn plain_response(status: u16, message: &str) -> McpHttpResponse {
    McpHttpResponse {
        status,
        content_type: Some("text/plain; charset=utf-8"),
        body: message.as_bytes().to_vec(),
        extra_headers: Vec::new(),
    }
}

#[must_use]
pub fn subject_not_allowed_response(subject: &str) -> McpHttpResponse {
    plain_response(
        403,
        &format!(
            "IndieAuth returned subject {subject:?}, but this remote accepts only its own \
             identity. If {subject:?} is the right identity, set it with \
             `vulcan mcp remote set <name> --identity {subject:?}`."
        ),
    )
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulcan_core::LocalOAuthIssuerConfig;

    fn issuer() -> LocalOAuthIssuer {
        LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://remote.example/mcp".into(),
            client_id: "static-client".into(),
            client_secret: "client-secret".into(),
            signing_key: "signing-key".into(),
            subject: "https://identity.example/alice".into(),
            dcr_enabled: true,
        })
        .expect("issuer")
    }

    fn authorize_params() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("response_type".into(), "code".into()),
            ("client_id".into(), "registered-client".into()),
            (
                "redirect_uri".into(),
                "https://client.example/callback".into(),
            ),
            ("code_challenge".into(), "challenge".into()),
            ("code_challenge_method".into(), "S256".into()),
            ("resource".into(), "https://remote.example/mcp".into()),
            ("state".into(), "original-state".into()),
        ])
    }

    fn clients() -> OAuthClientRegistry {
        let clients = OAuthClientRegistry::ephemeral();
        clients
            .register(crate::mcp_oauth_clients::RegisteredOAuthClient {
                client_id: "registered-client".into(),
                client_secret: String::new(),
                redirect_uris: vec!["https://client.example/callback".into()],
                client_name: None,
                token_endpoint_auth_method: "none".into(),
                client_id_issued_at: 0,
            })
            .expect("register client");
        clients
    }

    fn fake_exchange(_: &str, code: &str, _: &str, _: &str, _: &str) -> Result<String, OAuthError> {
        if code == "reject" {
            Err(OAuthError::Token("upstream denied code".into()))
        } else {
            Ok("https://identity.example/alice".into())
        }
    }

    #[test]
    fn authorization_checks_the_redirect_and_needs_sign_in() {
        let issuer = issuer();
        let clients = clients();
        let codes = McpAuthorizationCodeMap::default();
        let pending_indieauth = PendingIndieAuthMap::default();
        let pending_consent = PendingConsentMap::default();
        let endpoint = McpAuthorizeEndpoint {
            issuer: &issuer,
            clients: &clients,
            codes: &codes,
            pending_indieauth: &pending_indieauth,
            pending_consent: &pending_consent,
            indieauth: None,
            named_runtime: None,
            requested_profile: Some("readonly".into()),
            fallback_vault_root: "/vault".into(),
            allowed_redirect_hosts: &[],
            exchange: fake_exchange,
        };
        let mut params = authorize_params();
        params.insert(
            "redirect_uri".into(),
            "https://other.example/callback".into(),
        );
        assert_eq!(endpoint.authorize("GET", &params).status, 400);
        assert!(codes.lock().unwrap().is_empty());
        // There is no approval-token form: without IndieAuth nothing is issued.
        let mut params = authorize_params();
        params.insert("approval_token".into(), "approval-secret".into());
        assert_eq!(endpoint.authorize("GET", &params).status, 503);
        assert!(codes.lock().unwrap().is_empty());
    }

    #[test]
    fn indieauth_callback_preserves_client_state_and_requires_known_upstream_state() {
        let issuer = issuer();
        let clients = clients();
        let codes = McpAuthorizationCodeMap::default();
        let pending_indieauth = PendingIndieAuthMap::default();
        let pending_consent = PendingConsentMap::default();
        let indieauth = IndieAuthConfig {
            authorization_endpoint: "https://identity.example/authorize".into(),
            token_endpoint: "https://identity.example/token".into(),
            client_id: "https://remote.example".into(),
            redirect_uri: "https://remote.example/oauth/indieauth/callback".into(),
            me: None,
        };
        let endpoint = McpAuthorizeEndpoint {
            issuer: &issuer,
            clients: &clients,
            codes: &codes,
            pending_indieauth: &pending_indieauth,
            pending_consent: &pending_consent,
            indieauth: Some(&indieauth),
            named_runtime: None,
            requested_profile: Some("readonly".into()),
            fallback_vault_root: "/vault".into(),
            allowed_redirect_hosts: &[],
            exchange: fake_exchange,
        };
        assert_eq!(endpoint.authorize("GET", &authorize_params()).status, 302);
        assert_eq!(pending_indieauth.lock().unwrap().len(), 1);
        let wrong = BTreeMap::from([
            ("state".into(), "wrong".into()),
            ("code".into(), "upstream-code".into()),
        ]);
        assert_eq!(endpoint.callback("GET", &wrong).status, 400);
        assert_eq!(pending_indieauth.lock().unwrap().len(), 1);
        let state = pending_indieauth
            .lock()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        let callback = BTreeMap::from([
            ("state".into(), state.clone()),
            ("code".into(), "upstream-code".into()),
        ]);
        let response = endpoint.callback("GET", &callback);
        assert_eq!(response.status, 200);
        assert!(pending_indieauth.lock().unwrap().is_empty());
        let consent = pending_consent.lock().unwrap();
        assert_eq!(consent.len(), 1);
        assert_eq!(
            consent.values().next().unwrap().state.as_deref(),
            Some("original-state")
        );
        drop(consent);
        assert_eq!(endpoint.callback("GET", &callback).status, 400);
    }
}
