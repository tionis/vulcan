//! Transport-level route classification for hosted MCP HTTP requests.
//!
//! The daemon owns path and method routing. Authentication, OAuth policy, and
//! protocol operations are supplied by the host behind this transport boundary.

use crate::mcp_http_codec::{
    parse_mcp_http_post, validate_mcp_sse_accept, McpHttpRequest, McpHttpResponse,
};
use crate::mcp_transport::McpHttpConnection;
use serde_json::Value;
use std::io;
use vulcan_app::mcp_core::{is_supported_version, unsupported_version_error};
use vulcan_app::mcp_dispatch::{jsonrpc_error, request_id};
use vulcan_app::mcp_protocol::required_scope_for_request;

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

pub struct McpHttpRouteOptions<'a> {
    pub endpoint: &'a str,
    pub oauth_enabled: bool,
    pub local_oauth: bool,
    pub named_remote: bool,
}

pub trait McpHttpRouteHandler {
    type Authority;

    fn oauth(&self, request: &McpHttpRequest, route: McpHttpRoute<'_>) -> McpHttpResponse;
    fn authenticate(&self, request: &McpHttpRequest) -> Result<Self::Authority, McpHttpResponse>;
    fn authorize_scope(
        &self,
        authority: &Self::Authority,
        required: &str,
    ) -> Result<(), McpHttpResponse>;
    fn operation_status(&self, authority: &Self::Authority, operation_id: &str) -> McpHttpResponse;
    fn post(
        &self,
        request: &McpHttpRequest,
        authority: &Self::Authority,
        payload: &Value,
    ) -> McpHttpResponse;
    fn sse(
        &self,
        request: &McpHttpRequest,
        authority: &Self::Authority,
        stream: &mut McpHttpConnection,
    ) -> io::Result<()>;
    /// A `subscriptions/listen` request, answered with an event stream.
    fn listen(
        &self,
        request: &McpHttpRequest,
        authority: &Self::Authority,
        payload: &Value,
        stream: &mut McpHttpConnection,
    ) -> io::Result<()>;
    fn delete(&self, request: &McpHttpRequest, authority: &Self::Authority) -> McpHttpResponse;
    /// A tool call that asked for progress, which may stream progress
    /// notifications before its response. Without streaming support it is
    /// an ordinary POST.
    fn post_with_progress(
        &self,
        request: &McpHttpRequest,
        authority: &Self::Authority,
        payload: &Value,
        stream: &mut McpHttpConnection,
    ) -> io::Result<()> {
        stream.respond(self.post(request, authority, payload))
    }
}

/// Whether a request is a tool call that asked for progress notifications.
fn wants_progress(payload: &Value) -> bool {
    payload.get("method").and_then(Value::as_str) == Some("tools/call")
        && payload.pointer("/params/_meta/progressToken").is_some()
}

/// Route one already-decoded request. Endpoint authentication precedes method
/// rejection, while operation-status method rejection precedes authentication;
/// both preserve the existing hosted HTTP contract.
pub fn dispatch_mcp_http_request<H: McpHttpRouteHandler>(
    request: &McpHttpRequest,
    stream: &mut McpHttpConnection,
    options: &McpHttpRouteOptions<'_>,
    handler: &H,
) -> io::Result<()> {
    let route = classify_mcp_http_route(
        request,
        options.endpoint,
        options.oauth_enabled,
        options.local_oauth,
        options.named_remote,
    );
    let response = match route {
        McpHttpRoute::LocalOAuthRegister
        | McpHttpRoute::LocalOAuthAuthorize
        | McpHttpRoute::LocalOAuthToken
        | McpHttpRoute::LocalOAuthIndieAuthCallback
        | McpHttpRoute::LocalOAuthConsent
        | McpHttpRoute::AuthorizationServerMetadata
        | McpHttpRoute::ProtectedResourceMetadata => handler.oauth(request, route),
        McpHttpRoute::OperationStatus(_) if request.method != "GET" => {
            route_error(405, "Method Not Allowed")
        }
        McpHttpRoute::OperationStatus(operation_id) => match handler.authenticate(request) {
            Ok(authority) => handler.operation_status(&authority, operation_id),
            Err(response) => response,
        },
        McpHttpRoute::NotFound => route_error(404, "Not Found"),
        McpHttpRoute::McpEndpoint => {
            let authority = match handler.authenticate(request) {
                Ok(authority) => authority,
                Err(response) => return stream.respond(response),
            };
            match request.method.as_str() {
                "POST" => match preflight_mcp_post(request, &authority, handler) {
                    Ok(payload)
                        if payload.get("method").and_then(Value::as_str)
                            == Some("subscriptions/listen") =>
                    {
                        return handler.listen(request, &authority, &payload, stream);
                    }
                    Ok(payload) if wants_progress(&payload) => {
                        return handler.post_with_progress(request, &authority, &payload, stream);
                    }
                    Ok(payload) => handler.post(request, &authority, &payload),
                    Err(response) => response,
                },
                "GET" => match preflight_mcp_sse(request) {
                    Ok(()) => return handler.sse(request, &authority, stream),
                    Err(response) => response,
                },
                "DELETE" => match require_mcp_session_header(request) {
                    Ok(()) => handler.delete(request, &authority),
                    Err(response) => response,
                },
                _ => route_error(405, "Method Not Allowed"),
            }
        }
    };
    stream.respond(response)
}

fn preflight_mcp_sse(request: &McpHttpRequest) -> Result<(), McpHttpResponse> {
    validate_mcp_sse_accept(request).map_err(|error| route_error(error.status, &error.message))?;
    require_mcp_session_header(request)
}

fn require_mcp_session_header(request: &McpHttpRequest) -> Result<(), McpHttpResponse> {
    if request.headers.contains_key("mcp-session-id") {
        Ok(())
    } else {
        Err(route_error(400, "missing Mcp-Session-Id header"))
    }
}

/// Decode a POST and check its scope and protocol version before dispatch.
fn preflight_mcp_post<H: McpHttpRouteHandler>(
    request: &McpHttpRequest,
    authority: &H::Authority,
    handler: &H,
) -> Result<Value, McpHttpResponse> {
    let payload =
        parse_mcp_http_post(request).map_err(|error| route_error(error.status, &error.message))?;
    if let Some(required) = required_scope_for_request(&payload) {
        handler.authorize_scope(authority, required)?;
    }
    if let Some(version) = request.headers.get("mcp-protocol-version") {
        if !is_supported_version(version) {
            let id = request_id(&payload).unwrap_or(Value::Null);
            return Err(McpHttpResponse {
                status: 400,
                content_type: Some("application/json"),
                body: serde_json::to_vec(&unsupported_version_error(id, version))
                    .expect("JSON-RPC error should serialize"),
                extra_headers: Vec::new(),
            });
        }
    }
    Ok(payload)
}

fn route_error(status: u16, message: &str) -> McpHttpResponse {
    McpHttpResponse {
        status,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&jsonrpc_error(
            Value::Null,
            -32600,
            message.to_string(),
            None,
        ))
        .expect("JSON-RPC error should serialize"),
        extra_headers: Vec::new(),
    }
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
    use std::cell::RefCell;
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

    fn post_request(method: &str) -> McpHttpRequest {
        let mut request = request("POST", "/mcp");
        request
            .headers
            .insert("content-type".to_string(), "application/json".to_string());
        request.headers.insert(
            "accept".to_string(),
            "application/json, text/event-stream".to_string(),
        );
        request.body = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": method
        }))
        .unwrap();
        request
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

    #[derive(Default)]
    struct RecordingHandler {
        calls: RefCell<Vec<&'static str>>,
        deny_auth: bool,
        deny_scope: bool,
    }

    impl McpHttpRouteHandler for RecordingHandler {
        type Authority = ();

        fn oauth(&self, _: &McpHttpRequest, _: McpHttpRoute<'_>) -> McpHttpResponse {
            self.calls.borrow_mut().push("oauth");
            route_error(200, "oauth")
        }

        fn authenticate(&self, _: &McpHttpRequest) -> Result<(), McpHttpResponse> {
            self.calls.borrow_mut().push("authenticate");
            if self.deny_auth {
                Err(route_error(401, "Unauthorized"))
            } else {
                Ok(())
            }
        }

        fn authorize_scope(&self, (): &(), required: &str) -> Result<(), McpHttpResponse> {
            assert_eq!(required, "mcp");
            self.calls.borrow_mut().push("scope");
            if self.deny_scope {
                Err(route_error(403, "insufficient_scope"))
            } else {
                Ok(())
            }
        }

        fn operation_status(&self, (): &(), _: &str) -> McpHttpResponse {
            self.calls.borrow_mut().push("operation");
            route_error(200, "operation")
        }

        fn post(&self, _: &McpHttpRequest, (): &(), payload: &Value) -> McpHttpResponse {
            assert_eq!(payload["method"], "tools/list");
            self.calls.borrow_mut().push("post");
            route_error(200, "post")
        }

        fn sse(
            &self,
            _: &McpHttpRequest,
            (): &(),
            stream: &mut McpHttpConnection,
        ) -> io::Result<()> {
            self.calls.borrow_mut().push("sse");
            stream.respond(route_error(200, "sse"))
        }

        fn listen(
            &self,
            _: &McpHttpRequest,
            (): &(),
            _: &Value,
            stream: &mut McpHttpConnection,
        ) -> io::Result<()> {
            self.calls.borrow_mut().push("listen");
            stream.respond(route_error(200, "listen"))
        }

        fn delete(&self, _: &McpHttpRequest, (): &()) -> McpHttpResponse {
            self.calls.borrow_mut().push("delete");
            route_error(200, "delete")
        }
    }

    #[test]
    fn dispatch_preserves_route_authentication_and_method_order() {
        let options = McpHttpRouteOptions {
            endpoint: "/mcp",
            oauth_enabled: true,
            local_oauth: true,
            named_remote: true,
        };
        for (method, path, status, calls) in [
            ("GET", "/oauth/authorize", 200, &["oauth"][..]),
            ("POST", "/missing", 404, &[][..]),
            ("POST", "/mcp/operations/01AB", 405, &[][..]),
            (
                "GET",
                "/mcp/operations/01AB",
                200,
                &["authenticate", "operation"][..],
            ),
            ("OPTIONS", "/mcp", 405, &["authenticate"][..]),
            ("POST", "/mcp", 200, &["authenticate", "scope", "post"][..]),
            ("GET", "/mcp", 200, &["authenticate", "sse"][..]),
            ("DELETE", "/mcp", 200, &["authenticate", "delete"][..]),
        ] {
            let (mut server, capture) = McpHttpConnection::for_test();
            let handler = RecordingHandler::default();
            let mut inbound = if method == "POST" && path == "/mcp" {
                post_request("tools/list")
            } else {
                request(method, path)
            };
            if path == "/mcp" && matches!(method, "GET" | "DELETE") {
                inbound
                    .headers
                    .insert("mcp-session-id".to_string(), "session".to_string());
            }
            if path == "/mcp" && method == "GET" {
                inbound
                    .headers
                    .insert("accept".to_string(), "text/event-stream".to_string());
            }
            dispatch_mcp_http_request(&inbound, &mut server, &options, &handler).unwrap();
            let (answered, _, response) = capture.response();
            assert_eq!(answered, status, "{response}");
            assert_eq!(*handler.calls.borrow(), calls, "{method} {path}");
        }
    }

    #[test]
    fn post_preflight_decodes_then_checks_scope_then_version_before_host_dispatch() {
        let handler = RecordingHandler::default();
        let malformed = request("POST", "/mcp");
        assert_eq!(
            preflight_mcp_post(&malformed, &(), &handler)
                .unwrap_err()
                .status,
            400
        );
        assert!(handler.calls.borrow().is_empty());

        let denied = RecordingHandler {
            deny_scope: true,
            ..RecordingHandler::default()
        };
        let valid = post_request("tools/list");
        assert_eq!(
            preflight_mcp_post(&valid, &(), &denied).unwrap_err().status,
            403
        );
        assert_eq!(*denied.calls.borrow(), ["scope"]);

        let mut wrong_version = post_request("tools/list");
        wrong_version.headers.insert(
            "mcp-protocol-version".to_string(),
            "unsupported".to_string(),
        );
        assert_eq!(
            preflight_mcp_post(&wrong_version, &(), &handler)
                .unwrap_err()
                .status,
            400
        );
        assert_eq!(*handler.calls.borrow(), ["scope"]);
    }

    #[test]
    fn listen_requests_are_routed_to_the_stream_handler() {
        let handler = RecordingHandler::default();
        let (mut server, capture) = McpHttpConnection::for_test();
        let options = McpHttpRouteOptions {
            endpoint: "/mcp",
            oauth_enabled: false,
            local_oauth: false,
            named_remote: false,
        };
        dispatch_mcp_http_request(
            &post_request("subscriptions/listen"),
            &mut server,
            &options,
            &handler,
        )
        .unwrap();
        assert_eq!(capture.response().0, 200);
        assert_eq!(*handler.calls.borrow(), ["authenticate", "scope", "listen"]);
    }

    #[test]
    fn sse_and_delete_preflight_require_session_header_after_authentication() {
        let options = McpHttpRouteOptions {
            endpoint: "/mcp",
            oauth_enabled: false,
            local_oauth: false,
            named_remote: false,
        };
        for (method, accept) in [
            ("GET", None),
            ("GET", Some("text/event-stream")),
            ("DELETE", None),
        ] {
            let (mut server, capture) = McpHttpConnection::for_test();
            let mut inbound = request(method, "/mcp");
            if let Some(accept) = accept {
                inbound
                    .headers
                    .insert("accept".to_string(), accept.to_string());
            }
            let handler = RecordingHandler::default();
            dispatch_mcp_http_request(&inbound, &mut server, &options, &handler).unwrap();
            let (answered, _, response) = capture.response();
            let status = if method == "GET" && accept.is_none() {
                405
            } else {
                400
            };
            assert_eq!(answered, status, "{response}");
            assert_eq!(*handler.calls.borrow(), ["authenticate"]);
            let expected = if method == "GET" && accept.is_none() {
                "MCP GET requests require Accept: text/event-stream"
            } else {
                "missing Mcp-Session-Id header"
            };
            assert!(response.contains(expected), "{response}");
        }
    }

    #[test]
    fn endpoint_denies_unauthenticated_requests_before_method_dispatch() {
        let (mut server, capture) = McpHttpConnection::for_test();
        let handler = RecordingHandler {
            deny_auth: true,
            ..RecordingHandler::default()
        };
        dispatch_mcp_http_request(
            &request("OPTIONS", "/mcp"),
            &mut server,
            &McpHttpRouteOptions {
                endpoint: "/mcp",
                oauth_enabled: false,
                local_oauth: false,
                named_remote: false,
            },
            &handler,
        )
        .unwrap();
        let (answered, _, response) = capture.response();
        assert_eq!(answered, 401, "{response}");
        assert_eq!(*handler.calls.borrow(), ["authenticate"]);
    }
}
