//! Shared OAuth HTTP route dispatch for direct and named MCP listeners.

use std::collections::BTreeMap;

use serde_json::Value;
use vulcan_core::{LocalOAuthIssuer, OAuthResourceServer};

use crate::mcp_http_codec::{McpHttpRequest, McpHttpResponse};
use crate::mcp_http_routes::McpHttpRoute;
use crate::mcp_oauth_authorize::McpAuthorizeEndpoint;
use crate::mcp_oauth_consent::McpConsentEndpoint;
use crate::mcp_oauth_policy::SUPPORTED_MCP_OAUTH_SCOPES;
use crate::mcp_oauth_registration::register_mcp_oauth_client;
use crate::mcp_oauth_token::McpLocalTokenEndpoint;

pub enum McpOAuthRoutes<'a> {
    Local(Box<McpLocalOAuthRoutes<'a>>),
    External(&'a OAuthResourceServer),
}

pub struct McpLocalOAuthRoutes<'a> {
    pub issuer: &'a LocalOAuthIssuer,
    pub authorize: McpAuthorizeEndpoint<'a>,
    pub consent: McpConsentEndpoint<'a>,
    pub token: McpLocalTokenEndpoint<'a>,
    pub dcr_enabled: bool,
    pub allowed_redirect_hosts: &'a [String],
    pub refresh_supported: bool,
}

impl McpOAuthRoutes<'_> {
    #[must_use]
    pub fn handle(&self, request: &McpHttpRequest, route: McpHttpRoute<'_>) -> McpHttpResponse {
        match route {
            McpHttpRoute::AuthorizationServerMetadata => self.authorization_server_metadata(),
            McpHttpRoute::ProtectedResourceMetadata => self.protected_resource_metadata(),
            McpHttpRoute::LocalOAuthRegister => {
                let Self::Local(local) = self else {
                    unreachable!("local OAuth route requires local issuer");
                };
                let McpLocalOAuthRoutes {
                    authorize,
                    dcr_enabled,
                    allowed_redirect_hosts,
                    refresh_supported,
                    ..
                } = local.as_ref();
                register_mcp_oauth_client(
                    request,
                    authorize.clients,
                    *dcr_enabled,
                    allowed_redirect_hosts,
                    *refresh_supported,
                )
            }
            McpHttpRoute::LocalOAuthAuthorize => self
                .local_authorize()
                .authorize(&request.method, &parse_oauth_params(&request.query)),
            McpHttpRoute::LocalOAuthIndieAuthCallback => self
                .local_authorize()
                .callback(&request.method, &parse_oauth_params(&request.query)),
            McpHttpRoute::LocalOAuthToken => {
                let Self::Local(local) = self else {
                    unreachable!("local OAuth route requires local issuer");
                };
                local.token.handle(request, &parse_form(&request.body))
            }
            McpHttpRoute::LocalOAuthConsent => {
                let Self::Local(local) = self else {
                    unreachable!("local OAuth route requires local issuer");
                };
                local
                    .consent
                    .handle(&request.method, &parse_form(&request.body))
            }
            _ => unreachable!("non-OAuth route passed to OAuth handler"),
        }
    }

    fn local_authorize(&self) -> &McpAuthorizeEndpoint<'_> {
        let Self::Local(local) = self else {
            unreachable!("local OAuth route requires local issuer");
        };
        &local.authorize
    }

    fn authorization_server_metadata(&self) -> McpHttpResponse {
        let body = match self {
            Self::External(server) => server.authorization_server_metadata().clone(),
            Self::Local(local) => {
                let mut metadata = local.issuer.authorization_server_metadata().clone();
                if !local.refresh_supported {
                    metadata["grant_types_supported"] = serde_json::json!(["authorization_code"]);
                }
                metadata
            }
        };
        json_response(&body)
    }

    fn protected_resource_metadata(&self) -> McpHttpResponse {
        let (resource, authorization_server) = match self {
            Self::External(server) => (server.public_url(), server.authorization_server_issuer()),
            Self::Local(local) => (local.issuer.public_url(), local.issuer.public_url()),
        };
        json_response(&serde_json::json!({
            "resource": resource,
            "authorization_servers": [authorization_server],
            "bearer_methods_supported": ["header"],
            "scopes_supported": SUPPORTED_MCP_OAUTH_SCOPES,
        }))
    }
}

fn json_response(body: &Value) -> McpHttpResponse {
    McpHttpResponse {
        status: 200,
        content_type: Some("application/json"),
        body: serde_json::to_vec(body).expect("OAuth metadata JSON"),
        extra_headers: Vec::new(),
    }
}

fn parse_form(body: &[u8]) -> BTreeMap<String, String> {
    std::str::from_utf8(body).map_or_else(|_| BTreeMap::new(), parse_oauth_params)
}

#[must_use]
pub fn parse_oauth_params(input: &str) -> BTreeMap<String, String> {
    input
        .split('&')
        .filter(|part| !part.is_empty())
        .filter_map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            Some((percent_decode(key)?, percent_decode(value)?))
        })
        .collect()
}

fn percent_decode(value: &str) -> Option<String> {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                output.push(hex_value(bytes[index + 1])? * 16 + hex_value(bytes[index + 2])?);
                index += 3;
            }
            b'%' => return None,
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(output).ok()
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_decoding_preserves_form_semantics_and_ignores_invalid_pairs() {
        assert_eq!(
            parse_oauth_params("state=a%2Bb&client_id=hello+world&bad=%GG&empty="),
            BTreeMap::from([
                ("client_id".to_string(), "hello world".to_string()),
                ("empty".to_string(), String::new()),
                ("state".to_string(), "a+b".to_string()),
            ])
        );
        assert!(parse_form(&[0xff]).is_empty());
    }
}
