//! One shared HTTP session/route adapter for direct, foreground, and resident MCP.
//! The host supplies only resolved listener state and protocol-core construction.

use crate::mcp_execution::{process_http_request_with_timeout, HostedMcpExecution, McpRequestCore};
use crate::mcp_http_auth::McpHttpAuthError;
#[cfg(feature = "oauth")]
use crate::mcp_http_auth::McpOAuthMode;
use crate::mcp_http_codec::{write_mcp_http_response, McpHttpRequest, McpHttpResponse};
use crate::mcp_http_host::{McpHttpHost, McpHttpProtocolConfig};
use crate::mcp_http_routes::{
    dispatch_mcp_http_request, McpHttpRoute, McpHttpRouteHandler, McpHttpRouteOptions,
};
#[cfg(feature = "oauth")]
use crate::mcp_oauth_authorize::IndieAuthExchange;
use crate::mcp_session::{
    McpCancellationError, McpHttpSession, McpSessionAuthority, ResolvedMcpSession,
    SessionAdmissionError, SessionLookupError, SessionResolutionError,
};
use crate::mcp_sse::{serve_mcp_sse, McpSseEnd};
use serde_json::Value;
use std::io;
use std::net::TcpStream;
use std::ops::Deref;
use std::sync::Arc;
use vulcan_app::mcp_dispatch::{jsonrpc_error, request_id};

pub type McpHttpCoreFactory<C> = fn(McpHttpProtocolConfig) -> Result<C, String>;

/// Borrows one listener's actual authority and session stores; never creates a
/// parallel dispatcher or authorization state. Factory and exchange callbacks
/// are trusted host code, not values selectable by remote client requests.
pub struct McpHttpDriver<'a, C: McpRequestCore> {
    pub inner: &'a McpHttpHost<C>,
    pub hosted: Option<&'a HostedMcpExecution>,
    pub core_factory: McpHttpCoreFactory<C>,
    #[cfg(feature = "oauth")]
    pub indieauth_exchange: IndieAuthExchange,
}

impl<C: McpRequestCore> Deref for McpHttpDriver<'_, C> {
    type Target = McpHttpHost<C>;
    fn deref(&self) -> &Self::Target {
        self.inner
    }
}

#[must_use]
pub fn session_admission_response(error: SessionAdmissionError) -> McpHttpResponse {
    match error {
        SessionAdmissionError::Capacity => {
            let mut response = mcp_http_json_error_response(
                503,
                "MCP session limit reached; close unused sessions or retry later",
                Value::Null,
            );
            response
                .extra_headers
                .push(("Retry-After".to_string(), "60".to_string()));
            response
        }
        SessionAdmissionError::DuplicateId => {
            mcp_http_json_error_response(500, "MCP session ID collision", Value::Null)
        }
    }
}
pub fn authorized_mcp_http_session<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    session_id: &str,
    authority: &McpSessionAuthority,
    touch: bool,
) -> Result<Arc<McpHttpSession<C>>, McpHttpResponse> {
    context
        .sessions
        .authorized(session_id, authority, touch)
        .map_err(session_lookup_response)
}

fn session_lookup_response(error: SessionLookupError) -> McpHttpResponse {
    match error {
        SessionLookupError::Missing => {
            mcp_http_json_error_response(404, "unknown Mcp-Session-Id", Value::Null)
        }
        SessionLookupError::AuthorityMismatch => mcp_http_json_error_response(
            403,
            "MCP session authority does not match this request",
            Value::Null,
        ),
    }
}
pub fn handle_mcp_http_connection<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
    stream: &mut TcpStream,
) -> io::Result<()> {
    #[cfg(feature = "oauth")]
    let oauth_enabled = context.oauth.is_some();
    #[cfg(not(feature = "oauth"))]
    let oauth_enabled = false;
    #[cfg(feature = "oauth")]
    let local_oauth = matches!(context.oauth, Some(McpOAuthMode::Local(_)));
    #[cfg(not(feature = "oauth"))]
    let local_oauth = false;
    #[cfg(feature = "oauth")]
    let named_remote = context.named_runtime.is_some();
    #[cfg(not(feature = "oauth"))]
    let named_remote = false;
    dispatch_mcp_http_request(
        request,
        stream,
        &McpHttpRouteOptions {
            endpoint: &context.endpoint,
            oauth_enabled,
            local_oauth,
            named_remote,
        },
        context,
    )
}
impl<C: McpRequestCore> McpHttpRouteHandler for McpHttpDriver<'_, C> {
    type Authority = McpSessionAuthority;

    fn oauth(&self, request: &McpHttpRequest, route: McpHttpRoute<'_>) -> McpHttpResponse {
        #[cfg(feature = "oauth")]
        {
            handle_mcp_oauth_route(self, request, route)
        }
        #[cfg(not(feature = "oauth"))]
        {
            let _ = (request, route);
            mcp_http_json_error_response(404, "Not Found", Value::Null)
        }
    }

    fn authenticate(&self, request: &McpHttpRequest) -> Result<Self::Authority, McpHttpResponse> {
        authenticate_mcp_http_request(self, request)
    }

    fn authorize_scope(
        &self,
        authority: &Self::Authority,
        required: &str,
    ) -> Result<(), McpHttpResponse> {
        if authority.allows_scope(required) {
            Ok(())
        } else {
            Err(insufficient_scope_response(self, required))
        }
    }

    fn operation_status(&self, authority: &Self::Authority, operation_id: &str) -> McpHttpResponse {
        #[cfg(feature = "oauth")]
        {
            handle_named_mcp_operation_status(self, authority, operation_id)
        }
        #[cfg(not(feature = "oauth"))]
        {
            let _ = (authority, operation_id);
            mcp_http_json_error_response(404, "Not Found", Value::Null)
        }
    }

    fn post(
        &self,
        request: &McpHttpRequest,
        authority: &Self::Authority,
        payload: &Value,
    ) -> McpHttpResponse {
        handle_mcp_http_post(self, request, authority, payload)
    }

    fn sse(
        &self,
        request: &McpHttpRequest,
        authority: &Self::Authority,
        stream: &mut TcpStream,
    ) -> io::Result<()> {
        handle_mcp_http_sse(self, request, authority, stream)
    }

    fn delete(&self, request: &McpHttpRequest, authority: &Self::Authority) -> McpHttpResponse {
        handle_mcp_http_delete(self, request, authority)
    }
}
#[cfg(feature = "oauth")]
#[must_use]
pub fn handle_named_mcp_operation_status<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    authority: &McpSessionAuthority,
    operation_id: &str,
) -> McpHttpResponse {
    if !authority.allows_scope("mcp:tools") {
        return insufficient_scope_response(context, "mcp:tools");
    }
    let not_found = || mcp_http_json_error_response(404, "Not Found", Value::Null);
    let (Some(hosted), Some(named)) = (context.hosted.as_ref(), context.named_runtime.as_ref())
    else {
        return not_found();
    };
    let Some(report) = crate::mcp_hosted::named_mcp_operation_status(
        &hosted.executor.ledger(),
        named,
        authority,
        operation_id,
    ) else {
        return not_found();
    };
    McpHttpResponse {
        status: 200,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&report).expect("operation status should serialize"),
        extra_headers: vec![
            ("Cache-Control".to_string(), "no-store".to_string()),
            ("Vary".to_string(), "Authorization".to_string()),
        ],
    }
}

pub fn handle_mcp_http_post<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
    payload: &Value,
) -> McpHttpResponse {
    let resolved = match resolve_mcp_http_session(context, request, payload, authority) {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };
    let session = Arc::clone(&resolved.session);

    if payload.get("method").and_then(Value::as_str) == Some("notifications/cancelled") {
        return handle_mcp_cancellation_notification(payload, &session);
    }

    let Some(active_request) = session.start_request(request_id(payload).as_ref()) else {
        return mcp_http_json_error_response(
            409,
            "MCP request ID is already active in this session",
            Value::Null,
        );
    };

    let result = {
        let mut core = session
            .core
            .lock()
            .expect("mcp core lock should not be poisoned");
        process_http_request_with_timeout(
            &mut *core,
            payload.clone(),
            context.request_timeout,
            context.inner,
            request,
            authority,
            &active_request.cancellation(),
            context.hosted,
        )
    };

    drop(active_request);
    context.sessions.finish_http_post(resolved, result)
}

pub fn handle_mcp_cancellation_notification<C: McpRequestCore>(
    payload: &Value,
    session: &McpHttpSession<C>,
) -> McpHttpResponse {
    if let Err(error) = session.cancel_notification(payload) {
        let message = match error {
            McpCancellationError::MissingRequestId => "MCP cancellation requires params.requestId",
            McpCancellationError::InvalidRequestId => {
                "MCP cancellation requestId must be a string or number"
            }
        };
        return mcp_http_json_error_response(400, message, Value::Null);
    }
    McpHttpResponse {
        status: 202,
        content_type: None,
        body: Vec::new(),
        extra_headers: Vec::new(),
    }
}

#[must_use]
pub fn insufficient_scope_response<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    required: &str,
) -> McpHttpResponse {
    let message = format!("OAuth token does not grant required scope `{required}`");
    #[cfg(feature = "oauth")]
    if let Some(oauth) = context.oauth.as_ref() {
        let mut response = oauth_error_response(oauth, &message, "insufficient_scope");
        response.status = 403;
        if let Some((_, challenge)) = response
            .extra_headers
            .iter_mut()
            .find(|(name, _)| name == "WWW-Authenticate")
        {
            challenge.push_str(", scope=\"");
            challenge.push_str(required);
            challenge.push('"');
        }
        return response;
    }
    #[cfg(not(feature = "oauth"))]
    let _ = context;
    mcp_http_json_error_response(403, message, Value::Null)
}

pub fn resolve_mcp_http_session<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
    payload: &Value,
    authority: &McpSessionAuthority,
) -> Result<ResolvedMcpSession<C>, McpHttpResponse> {
    let resolved = context
        .sessions
        .resolve_http(
            payload,
            request.headers.get("mcp-session-id").map(String::as_str),
            authority,
            || create_mcp_http_core(context, authority),
        )
        .map_err(|error| match error {
            SessionResolutionError::InvalidInitialize(error) => McpHttpResponse {
                status: 400,
                content_type: Some("application/json"),
                body: serde_json::to_vec(&error).expect("JSON-RPC error should serialize"),
                extra_headers: Vec::new(),
            },
            SessionResolutionError::MissingSessionId => {
                mcp_http_json_error_response(400, "missing Mcp-Session-Id header", Value::Null)
            }
            SessionResolutionError::Admission(error) => session_admission_response(error),
            SessionResolutionError::Lookup(error) => session_lookup_response(error),
            SessionResolutionError::Create(response) => response,
        })?;
    Ok(resolved)
}

pub fn create_mcp_http_core<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    authority: &McpSessionAuthority,
) -> Result<C, McpHttpResponse> {
    let config = context
        .inner
        .protocol_config(authority)
        .map_err(|error| mcp_http_json_error_response(error.status, error.message, Value::Null))?;
    (context.core_factory)(config)
        .map_err(|error| mcp_http_json_error_response(500, error.to_string(), Value::Null))
}

#[must_use]
pub fn handle_mcp_http_delete<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
) -> McpHttpResponse {
    let session_id = request
        .headers
        .get("mcp-session-id")
        .expect("daemon route preflight requires MCP session header");
    if let Err(response) = authorized_mcp_http_session(context, session_id, authority, false) {
        return response;
    }
    context.sessions.retire(session_id);
    McpHttpResponse {
        status: 204,
        content_type: None,
        body: Vec::new(),
        extra_headers: Vec::new(),
    }
}

pub fn handle_mcp_http_sse<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
    stream: &mut TcpStream,
) -> io::Result<()> {
    let session_id = request
        .headers
        .get("mcp-session-id")
        .expect("daemon route preflight requires MCP session header");
    let session = match authorized_mcp_http_session(context, session_id, authority, true) {
        Ok(session) => session,
        Err(response) => {
            write_mcp_http_response(stream, &response)?;
            return Ok(());
        }
    };

    let end = serve_mcp_sse(
        &session,
        stream,
        || {
            authenticate_mcp_http_request(context, request)
                .is_ok_and(|current| session.authority.matches(&current))
                && session
                    .core
                    .lock()
                    .expect("mcp core lock should not be poisoned")
                    .attenuate_profile()
                    .is_ok()
        },
        || {
            session
                .core
                .lock()
                .expect("mcp core lock should not be poisoned")
                .list_changed_notifications()
        },
    )?;
    if end == McpSseEnd::RetireSession {
        context.sessions.retire(session_id);
    }
    Ok(())
}

pub fn authenticate_mcp_http_request<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
) -> Result<McpSessionAuthority, McpHttpResponse> {
    context
        .inner
        .authenticate(&request.headers)
        .map_err(|error| match error {
            McpHttpAuthError::Http { status, message } => {
                mcp_http_json_error_response(status, message, Value::Null)
            }
            #[cfg(feature = "oauth")]
            McpHttpAuthError::OAuth {
                message,
                rejected_bearer,
            } => {
                if rejected_bearer {
                    eprintln!("MCP OAuth bearer token rejected: {message}");
                }
                oauth_error_response(
                    context
                        .oauth
                        .as_ref()
                        .expect("OAuth rejection has an issuer"),
                    message,
                    "invalid_token",
                )
            }
        })
}
#[cfg(feature = "oauth")]
#[must_use]
pub fn handle_mcp_oauth_route<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
    route: McpHttpRoute<'_>,
) -> McpHttpResponse {
    context
        .inner
        .oauth_routes(context.indieauth_exchange)
        .expect("OAuth route requires issuer")
        .handle(request, route)
}
#[cfg(feature = "oauth")]
fn oauth_error_response(
    oauth: &McpOAuthMode,
    message: impl Into<String>,
    error: &str,
) -> McpHttpResponse {
    let message = message.into();
    let error_description = escape_www_authenticate_value(&message);
    let mut response = mcp_http_json_error_response(401, message, Value::Null);
    response.extra_headers.push((
        "WWW-Authenticate".to_string(),
        format!(
            "Bearer error=\"{error}\", error_description=\"{}\", resource_metadata=\"{}\"",
            error_description,
            oauth_protected_resource_metadata_url(oauth),
        ),
    ));
    response
}

#[cfg(feature = "oauth")]
fn oauth_protected_resource_metadata_url(oauth: &McpOAuthMode) -> &str {
    match oauth {
        McpOAuthMode::External(external) => external.protected_resource_metadata_url(),
        McpOAuthMode::Local(local) => local.protected_resource_metadata_url(),
    }
}

#[cfg(feature = "oauth")]
fn escape_www_authenticate_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
pub fn mcp_http_json_error_response(
    status: u16,
    message: impl Into<String>,
    id: Value,
) -> McpHttpResponse {
    let body = jsonrpc_error(id, -32600, message.into(), None);
    McpHttpResponse {
        status,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&body).expect("json should serialize"),
        extra_headers: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_session::McpSessionRegistry;
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;
    use ulid::Ulid;
    use vulcan_app::mcp_catalog::{McpToolPack, McpToolPackMode};
    use vulcan_app::mcp_session_protocol::{McpProtocolCore, McpProtocolHost};
    use vulcan_core::VaultPaths;

    fn factory(config: McpHttpProtocolConfig) -> Result<McpProtocolCore, String> {
        McpProtocolCore::new(
            &config.paths,
            config.permission_profile.as_deref(),
            config.selected_tool_packs,
            config.tool_pack_mode,
            McpProtocolHost {
                registry_options: vulcan_app::tools::CustomToolRegistryOptions::default,
                command_help: |_| Err("No host-specific help".into()),
                help_candidates: |_| Vec::new(),
                server_version: "test",
            },
        )
        .map_err(|error| error.to_string())
    }

    fn host(paths: &VaultPaths) -> McpHttpHost<McpProtocolCore> {
        McpHttpHost {
            paths: paths.clone(),
            requested_profile: Some("readonly".into()),
            selected_tool_packs: BTreeSet::from([McpToolPack::NotesRead, McpToolPack::Search]),
            tool_pack_mode: McpToolPackMode::Static,
            endpoint: "/mcp".into(),
            auth_token: None,
            bind_addr: "127.0.0.1:4321".parse().unwrap(),
            instance_id: Ulid::new(),
            sessions: Arc::new(McpSessionRegistry::new()),
            request_timeout: Duration::from_secs(30),
            #[cfg(feature = "oauth")]
            oauth: None,
            #[cfg(feature = "oauth")]
            oauth_codes: Arc::default(),
            #[cfg(feature = "oauth")]
            oauth_clients: Arc::new(crate::mcp_oauth_clients::OAuthClientRegistry::ephemeral()),
            #[cfg(feature = "oauth")]
            oauth_pending_indieauth: Arc::default(),
            #[cfg(feature = "oauth")]
            oauth_pending_consent: Arc::default(),
            #[cfg(feature = "oauth")]
            oauth_dcr_enabled: false,
            #[cfg(feature = "oauth")]
            oauth_dcr_allowed_redirect_hosts: Vec::new(),
            #[cfg(feature = "oauth")]
            oauth_local_redirect_uris: Vec::new(),
            #[cfg(feature = "oauth")]
            oauth_indieauth: None,
            #[cfg(feature = "oauth")]
            named_runtime: None,
        }
    }

    fn driver(host: &McpHttpHost<McpProtocolCore>) -> McpHttpDriver<'_, McpProtocolCore> {
        McpHttpDriver {
            inner: host,
            hosted: None,
            core_factory: factory,
            #[cfg(feature = "oauth")]
            indieauth_exchange: crate::mcp_oauth_authorize::default_indieauth_exchange,
        }
    }

    fn request(session: Option<&str>) -> McpHttpRequest {
        McpHttpRequest {
            method: "POST".into(),
            path: "/mcp".into(),
            query: String::new(),
            headers: session
                .map(|id| BTreeMap::from([("mcp-session-id".into(), id.into())]))
                .unwrap_or_default(),
            body: Vec::new(),
        }
    }

    fn initialize() -> Value {
        json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
            "protocolVersion":vulcan_app::mcp_protocol::MCP_PROTOCOL_VERSION,
            "clientInfo":{"name":"driver-test","version":"1"}, "capabilities":{}
        }})
    }

    #[test]
    fn app_core_sessions_preserve_authority_cancellation_and_independent_cleanup() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        vulcan_core::initialize_vulcan_dir(&paths).unwrap();
        let host = host(&paths);
        let driver = driver(&host);
        let inbound = request(None);
        let authority = authenticate_mcp_http_request(&driver, &inbound).unwrap();
        let start = handle_mcp_http_post(&driver, &inbound, &authority, &initialize());
        assert_eq!(start.status, 200);
        let id = &start
            .extra_headers
            .iter()
            .find(|(key, _)| key == "Mcp-Session-Id")
            .unwrap()
            .1;
        let bound = request(Some(id));
        let initialized = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert_eq!(
            handle_mcp_http_post(&driver, &bound, &authority, &initialized).status,
            202
        );
        let list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
        let response = handle_mcp_http_post(&driver, &bound, &authority, &list);
        assert_eq!(response.status, 200);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        let names: Vec<_> = body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(names.contains(&"note_get"));
        assert!(!names.contains(&"note_create"));
        let mut foreign = authority.clone();
        foreign.remote_instance_id = Ulid::new();
        assert_eq!(
            handle_mcp_http_post(&driver, &bound, &foreign, &list).status,
            403
        );
        let session = host.sessions.live(id).unwrap();
        let active = session.start_request(Some(&json!(2))).unwrap();
        assert_eq!(
            handle_mcp_http_post(&driver, &bound, &authority, &list).status,
            409
        );
        drop(active);
        let invalid =
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":{}}});
        assert_eq!(
            handle_mcp_http_post(&driver, &bound, &authority, &invalid).status,
            400
        );
        let valid =
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}});
        assert_eq!(
            handle_mcp_http_post(&driver, &bound, &authority, &valid).status,
            202
        );
        let second = handle_mcp_http_post(&driver, &inbound, &authority, &initialize());
        let second_id = &second
            .extra_headers
            .iter()
            .find(|(key, _)| key == "Mcp-Session-Id")
            .unwrap()
            .1;
        assert_ne!(id, second_id);
        assert_eq!(
            handle_mcp_http_delete(&driver, &bound, &foreign).status,
            403
        );
        assert!(host.sessions.live(id).is_some());
        assert_eq!(
            handle_mcp_http_delete(&driver, &bound, &authority).status,
            204
        );
        assert_eq!(
            handle_mcp_http_post(&driver, &bound, &authority, &list).status,
            404
        );
        assert!(host.sessions.live(second_id).is_some());
    }

    #[test]
    fn failed_core_construction_never_admits_a_session() {
        let temporary = tempfile::tempdir().unwrap();
        let host = host(&VaultPaths::new(temporary.path()));
        let mut driver = driver(&host);
        driver.core_factory = |_| Err("Core unavailable".into());
        let inbound = request(None);
        let authority = authenticate_mcp_http_request(&driver, &inbound).unwrap();
        let response = handle_mcp_http_post(&driver, &inbound, &authority, &initialize());
        assert_eq!(response.status, 500);
        assert!(response.extra_headers.is_empty());
    }
}
