//! Shared authentication policy for foreground and resident MCP HTTP hosts.

use std::collections::BTreeMap;
use std::net::SocketAddr;
#[cfg(feature = "oauth")]
use std::sync::Arc;
#[cfg(feature = "oauth")]
use std::time::{SystemTime, UNIX_EPOCH};
use ulid::Ulid;
#[cfg(feature = "oauth")]
use vulcan_core::{LocalOAuthIssuer, OAuthResourceServer};

use crate::http_policy::mcp_origin_allowed;
use crate::mcp_oauth_policy::DEFAULT_MCP_OAUTH_SCOPES;
#[cfg(feature = "oauth")]
use crate::mcp_remote_runtime::{NamedMcpRuntime, NamedTokenRequest};
use crate::mcp_session::McpSessionAuthority;

#[cfg(feature = "oauth")]
#[derive(Debug, Clone)]
pub enum McpOAuthMode {
    External(Arc<OAuthResourceServer>),
    Local(Arc<LocalOAuthIssuer>),
}

#[cfg(feature = "oauth")]
impl McpOAuthMode {
    #[must_use]
    pub fn public_url(&self) -> &str {
        match self {
            Self::External(server) => server.public_url(),
            Self::Local(issuer) => issuer.public_url(),
        }
    }
}

pub struct McpHttpAuthOptions<'a> {
    pub instance_id: Ulid,
    pub bind_addr: SocketAddr,
    pub auth_token: Option<&'a str>,
    pub permission_profile: Option<&'a str>,
    pub packs: Vec<String>,
    #[cfg(feature = "oauth")]
    pub oauth: Option<&'a McpOAuthMode>,
    #[cfg(feature = "oauth")]
    pub named_runtime: Option<&'a NamedMcpRuntime>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum McpHttpAuthError {
    Http {
        status: u16,
        message: String,
    },
    #[cfg(feature = "oauth")]
    OAuth {
        message: String,
        rejected_bearer: bool,
    },
}

#[cfg(feature = "oauth")]
fn oauth_error(message: impl Into<String>, rejected_bearer: bool) -> McpHttpAuthError {
    McpHttpAuthError::OAuth {
        message: message.into(),
        rejected_bearer,
    }
}

/// Headers must use the HTTP codec's normalized lowercase names.
#[allow(clippy::too_many_lines)]
pub fn authenticate_mcp_http_request(
    options: McpHttpAuthOptions<'_>,
    headers: &BTreeMap<String, String>,
) -> Result<McpSessionAuthority, McpHttpAuthError> {
    let mut credential = "loopback-unauthenticated".to_string();
    #[allow(unused_mut)]
    let mut client_id = None;
    #[allow(unused_mut)]
    let mut subject = None;
    #[allow(unused_mut)]
    let mut permission_profile = options.permission_profile.map(str::to_owned);
    #[cfg(feature = "oauth")]
    let mut grant_id = None;
    #[allow(unused_mut)]
    let mut scopes = DEFAULT_MCP_OAUTH_SCOPES
        .iter()
        .map(|scope| (*scope).to_string())
        .collect();
    #[cfg(feature = "oauth")]
    if let Some(oauth) = options.oauth {
        let token = bearer_token(headers)
            .ok_or_else(|| oauth_error("missing OAuth bearer token", false))?;
        match oauth {
            McpOAuthMode::External(server) => {
                let identity = server
                    .validate_bearer_token(&token)
                    .map_err(|error| oauth_error(error.to_string(), true))?;
                subject = Some(identity.subject);
                scopes = identity.scopes;
            }
            McpOAuthMode::Local(issuer) => {
                let identity = issuer
                    .validate_bearer_token(&token)
                    .map_err(|error| oauth_error(error.to_string(), true))?;
                subject = Some(identity.subject);
                client_id = identity.client_id;
                scopes = identity.scopes;
                grant_id = identity.grant_id;
                if permission_profile.is_none() {
                    permission_profile = identity.permission_profile;
                }
            }
        }
        credential = token;
    }
    if let Some(expected) = options.auth_token {
        let actual = bearer_token(headers).or_else(|| headers.get("x-vulcan-token").cloned());
        if actual.as_deref() != Some(expected) {
            return Err(McpHttpAuthError::Http {
                status: 401,
                message: "missing or invalid authentication token".into(),
            });
        }
        credential = actual.expect("validated token is present");
    }
    if headers
        .get("origin")
        .is_some_and(|origin| !mcp_origin_allowed(origin, options.bind_addr))
    {
        return Err(McpHttpAuthError::Http {
            status: 403,
            message: "invalid Origin header".into(),
        });
    }
    #[cfg(feature = "oauth")]
    if let Some(named) = options.named_runtime {
        let oauth = options.oauth.ok_or_else(|| McpHttpAuthError::Http {
            status: 500,
            message: "named MCP runtime requires OAuth".into(),
        })?;
        let grant_id = grant_id
            .as_deref()
            .and_then(|value| value.parse::<Ulid>().ok())
            .ok_or_else(|| oauth_error("access token is not bound to a connection grant", false))?;
        let client_id = client_id
            .ok_or_else(|| oauth_error("access token has no OAuth client binding", false))?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| McpHttpAuthError::Http {
                status: 500,
                message: error.to_string(),
            })?
            .as_secs();
        return named
            .authorize_token(&NamedTokenRequest {
                remote_instance_id: options.instance_id,
                grant_id,
                client_id: &client_id,
                subject: subject.as_deref(),
                scopes: &scopes,
                resource: oauth.public_url(),
                credential: &credential,
                now,
            })
            .map_err(|error| oauth_error(error, false));
    }
    Ok(McpSessionAuthority::direct(
        options.instance_id,
        &credential,
        client_id,
        subject,
        permission_profile,
        options.packs,
        scopes,
    ))
}

fn bearer_token(headers: &BTreeMap<String, String>) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> McpHttpAuthOptions<'static> {
        McpHttpAuthOptions {
            instance_id: Ulid::nil(),
            bind_addr: "127.0.0.1:4321".parse().unwrap(),
            auth_token: Some("secret"),
            permission_profile: Some("readonly"),
            packs: vec!["notes-read".into()],
            #[cfg(feature = "oauth")]
            oauth: None,
            #[cfg(feature = "oauth")]
            named_runtime: None,
        }
    }

    #[test]
    fn shared_token_authentication_preserves_bearer_precedence() {
        let mut headers = BTreeMap::from([("x-vulcan-token".into(), "secret".into())]);
        assert!(authenticate_mcp_http_request(options(), &headers).is_ok());
        headers.insert("authorization".into(), "Bearer wrong".into());
        assert!(matches!(
            authenticate_mcp_http_request(options(), &headers),
            Err(McpHttpAuthError::Http { status: 401, .. })
        ));
        headers.insert("authorization".into(), "Bearer secret".into());
        assert!(authenticate_mcp_http_request(options(), &headers).is_ok());
    }

    #[cfg(feature = "oauth")]
    #[test]
    fn oauth_identity_scopes_and_credentials_bind_the_authority() {
        let subject = "https://identity.example.test/me";
        let issuer = Arc::new(
            LocalOAuthIssuer::from_config(vulcan_core::LocalOAuthIssuerConfig {
                public_url: "https://mcp.example.test/mcp".into(),
                client_id: "static-client".into(),
                client_secret: "static-secret".into(),
                signing_key: "test-signing-key".into(),
                approval_token: String::new(),
                subject: subject.into(),
                email: None,
                users: Vec::new(),
                dcr_enabled: true,
            })
            .unwrap(),
        );
        let scopes = vec!["mcp:tools".into()];
        let token = issuer
            .issue_access_token_for_authorization(subject, "client", &scopes, None)
            .unwrap();
        let oauth = McpOAuthMode::Local(issuer);
        let authenticate = |headers: &BTreeMap<String, String>| {
            let mut policy = options();
            policy.auth_token = None;
            policy.oauth = Some(&oauth);
            authenticate_mcp_http_request(policy, headers)
        };
        assert!(matches!(
            authenticate(&BTreeMap::new()),
            Err(McpHttpAuthError::OAuth {
                rejected_bearer: false,
                ..
            })
        ));
        let headers = BTreeMap::from([("authorization".into(), format!("Bearer {token}"))]);
        let authority = authenticate(&headers).unwrap();
        assert_eq!(authority.subject.as_deref(), Some(subject));
        assert_eq!(authority.client_id.as_deref(), Some("client"));
        assert_eq!(authority.scopes, scopes);
        assert_eq!(authority.permission_profile.as_deref(), Some("readonly"));
        assert_eq!(authority.tool_packs, vec!["notes-read"]);
        assert!(!format!("{authority:?}").contains(&token));
        let invalid = BTreeMap::from([("authorization".into(), "Bearer invalid".into())]);
        assert!(matches!(
            authenticate(&invalid),
            Err(McpHttpAuthError::OAuth {
                rejected_bearer: true,
                ..
            })
        ));
    }

    #[test]
    fn origin_policy_and_authentication_order_are_shared() {
        let mut headers = BTreeMap::from([("origin".into(), "https://evil.example".into())]);
        assert!(matches!(
            authenticate_mcp_http_request(options(), &headers),
            Err(McpHttpAuthError::Http { status: 401, .. })
        ));
        headers.insert("x-vulcan-token".into(), "secret".into());
        assert!(matches!(
            authenticate_mcp_http_request(options(), &headers),
            Err(McpHttpAuthError::Http { status: 403, .. })
        ));
        headers.insert("origin".into(), "http://127.0.0.1:4321".into());
        assert!(authenticate_mcp_http_request(options(), &headers).is_ok());
        let mut local = options();
        local.auth_token = None;
        assert!(authenticate_mcp_http_request(local, &BTreeMap::new()).is_ok());
    }
}
