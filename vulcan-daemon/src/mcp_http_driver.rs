//! One shared HTTP session/route adapter for direct, foreground, and resident MCP.
//! The host supplies only resolved listener state and protocol-core construction.

use crate::http_policy::mcp_host_allowed;
use crate::mcp_execution::{process_http_request_with_timeout, HostedMcpExecution, McpRequestCore};
use crate::mcp_http_auth::McpHttpAuthError;
#[cfg(feature = "oauth")]
use crate::mcp_http_auth::McpOAuthMode;
use crate::mcp_http_codec::{write_mcp_http_sse_event, write_mcp_http_sse_keepalive};
use crate::mcp_http_codec::{McpHttpRequest, McpHttpResponse};
use crate::mcp_http_host::{McpHttpHost, McpHttpProtocolConfig};
use crate::mcp_http_routes::{
    dispatch_mcp_http_request, McpHttpRoute, McpHttpRouteHandler, McpHttpRouteOptions,
};
#[cfg(feature = "oauth")]
use crate::mcp_oauth_authorize::IndieAuthExchange;
use crate::mcp_session::mcp_notification_scope;
use crate::mcp_session::{
    McpCancellationError, McpHttpSession, McpSessionAuthority, ResolvedMcpSession,
    SessionAdmissionError, SessionLookupError, SessionResolutionError,
};
use crate::mcp_sse::{serve_mcp_sse, McpSseEnd};
use crate::mcp_transport::McpHttpConnection;
use serde_json::Value;
use std::io;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;
use vulcan_app::execution::ExecutionCancellationToken;
use vulcan_app::mcp_core::{
    listen_request, requested_modern_version, subscription_acknowledged, subscription_closed,
    subscription_notification, MCP_HEADER_MISMATCH,
};
use vulcan_app::mcp_dispatch::{jsonrpc_error, request_id, McpHttpProcessResult};
use vulcan_app::mcp_operations::{McpOperationLookup, McpOwnedOperations};
use vulcan_app::mcp_progress::McpNotifier;
#[cfg(feature = "oauth")]
use vulcan_app::mcp_protocol::MCP_OAUTH_SCOPE;

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
    stream: &mut McpHttpConnection,
) -> io::Result<()> {
    if let Some(host) = request.headers.get("host") {
        if !mcp_host_allowed(host, context.bind_addr, context.public_url()) {
            return stream.respond(mcp_http_json_error_response(
                421,
                format!("this listener does not serve the host `{host}`"),
                Value::Null,
            ));
        }
    }
    #[cfg(feature = "oauth")]
    let oauth_enabled = context.oauth.is_some();
    #[cfg(not(feature = "oauth"))]
    let oauth_enabled = false;
    #[cfg(feature = "oauth")]
    let local_oauth = context.oauth.is_some();
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
        stream: &mut McpHttpConnection,
    ) -> io::Result<()> {
        handle_mcp_http_sse(self, request, authority, stream)
    }

    fn listen(
        &self,
        request: &McpHttpRequest,
        authority: &Self::Authority,
        payload: &Value,
        stream: &mut McpHttpConnection,
    ) -> io::Result<()> {
        handle_mcp_http_listen(self, request, authority, payload, stream)
    }

    fn delete(&self, request: &McpHttpRequest, authority: &Self::Authority) -> McpHttpResponse {
        handle_mcp_http_delete(self, request, authority)
    }

    fn post_with_progress(
        &self,
        request: &McpHttpRequest,
        authority: &Self::Authority,
        payload: &Value,
        stream: &mut McpHttpConnection,
    ) -> io::Result<()> {
        handle_mcp_http_post_with_progress(self, request, authority, payload, stream)
    }
}
#[cfg(feature = "oauth")]
#[must_use]
pub fn handle_named_mcp_operation_status<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    authority: &McpSessionAuthority,
    operation_id: &str,
) -> McpHttpResponse {
    if !authority.allows_scope(MCP_OAUTH_SCOPE) {
        return insufficient_scope_response(context, MCP_OAUTH_SCOPE);
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

#[must_use]
pub fn handle_mcp_http_post<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
    payload: &Value,
) -> McpHttpResponse {
    serve_mcp_http_post(context, request, authority, payload, None)
}

/// A POST, with progress delivered through `notifier` while it is served.
fn serve_mcp_http_post<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
    payload: &Value,
    notifier: Option<McpNotifier>,
) -> McpHttpResponse {
    if requested_modern_version(payload).is_some() {
        return serve_modern_mcp_http_post(context, request, authority, payload, notifier);
    }
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
            notifier,
        )
    };

    drop(active_request);
    context.sessions.finish_http_post(resolved, result)
}

/// A 2026-07-28 request: served on its own, without a session. The protocol
/// core is built for this request's authority and dropped after it.
#[must_use]
pub fn handle_modern_mcp_http_post<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
    payload: &Value,
) -> McpHttpResponse {
    serve_modern_mcp_http_post(context, request, authority, payload, None)
}

fn serve_modern_mcp_http_post<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
    payload: &Value,
    notifier: Option<McpNotifier>,
) -> McpHttpResponse {
    if let Some(mismatch) = modern_header_mismatch(request, payload) {
        let id = request_id(payload).unwrap_or(Value::Null);
        return mcp_http_json_response(
            400,
            &jsonrpc_error(id, MCP_HEADER_MISMATCH, mismatch, None),
        );
    }
    let mut core = match create_mcp_http_core(context, authority) {
        Ok(core) => core,
        Err(response) => return response,
    };
    let result = process_http_request_with_timeout(
        &mut core,
        payload.clone(),
        context.request_timeout,
        context.inner,
        request,
        authority,
        &ExecutionCancellationToken::default(),
        context.hosted,
        notifier,
    );
    modern_http_response(result)
}

/// How often the connection thread looks for progress while a call runs.
const PROGRESS_RELAY_INTERVAL: Duration = Duration::from_millis(50);

/// A tool call that asked for progress. The call runs on its own thread
/// while this one relays its progress: the first notification turns the
/// response into an event stream, and the response is its last event. A
/// call that reports nothing is answered as an ordinary POST. Progress ends
/// with the call's response, also when the call becomes a task.
pub fn handle_mcp_http_post_with_progress<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
    payload: &Value,
    stream: &mut McpHttpConnection,
) -> io::Result<()> {
    let (sender, receiver) = mpsc::channel::<Value>();
    let open = Arc::new(AtomicBool::new(true));
    let notifier = {
        let open = Arc::clone(&open);
        let sender = Mutex::new(sender);
        McpNotifier(Arc::new(move |message| {
            if open.load(Ordering::Acquire) {
                let _ = sender
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .send(message);
            }
        }))
    };
    let mut streaming = false;
    let response = thread::scope(|scope| -> io::Result<McpHttpResponse> {
        let call = scope.spawn(|| {
            let response =
                serve_mcp_http_post(context, request, authority, payload, Some(notifier));
            open.store(false, Ordering::Release);
            response
        });
        loop {
            match receiver.recv_timeout(PROGRESS_RELAY_INTERVAL) {
                Ok(message) => {
                    if !streaming {
                        stream.start_event_stream()?;
                        streaming = true;
                    }
                    write_mcp_http_sse_event(stream, &message)?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) if !call.is_finished() => {}
                Err(_) => break,
            }
        }
        // What the call reported before it was answered still goes out.
        while let Ok(message) = receiver.try_recv() {
            if !streaming {
                stream.start_event_stream()?;
                streaming = true;
            }
            write_mcp_http_sse_event(stream, &message)?;
        }
        Ok(call.join().expect("MCP progress call should not panic"))
    })?;
    if !streaming {
        return stream.respond(response);
    }
    match serde_json::from_slice::<Value>(&response.body) {
        Ok(message) => write_mcp_http_sse_event(stream, &message),
        Err(_) => Ok(()),
    }
}

/// How often a subscription stream looks for list changes.
const LISTEN_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How often the change watcher looks at sessions with a GET stream.
const LIST_WATCH_INTERVAL: Duration = Duration::from_secs(2);
/// A comment line at least this often keeps proxies from closing an idle
/// stream, and finds a client that has gone.
const LISTEN_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// `subscriptions/listen`: an event stream that acknowledges what it will
/// deliver, then carries the list changes the client asked for, each tagged
/// with the subscription's id. It runs until the client closes it, or until
/// the authority it was opened under is no longer valid, when it ends with a
/// response to the listen request.
pub fn handle_mcp_http_listen<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
    payload: &Value,
    stream: &mut McpHttpConnection,
) -> io::Result<()> {
    handle_mcp_http_listen_with_timing(
        context,
        request,
        authority,
        payload,
        stream,
        LISTEN_POLL_INTERVAL,
        LISTEN_KEEPALIVE_INTERVAL,
    )
}

fn handle_mcp_http_listen_with_timing<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
    payload: &Value,
    stream: &mut McpHttpConnection,
    poll_interval: Duration,
    keepalive_interval: Duration,
) -> io::Result<()> {
    let id = request_id(payload).unwrap_or(Value::Null);
    if let Some(mismatch) = modern_header_mismatch(request, payload) {
        return stream.respond(mcp_http_json_response(
            400,
            &jsonrpc_error(id, MCP_HEADER_MISMATCH, mismatch, None),
        ));
    }
    let filter = match listen_request(payload.get("params")) {
        Ok(filter) => filter,
        Err(error) => {
            return stream.respond(modern_http_response(Err(error.into_jsonrpc_error(id))));
        }
    };
    let mut core = match create_mcp_http_core(context, authority) {
        Ok(core) => core,
        Err(response) => return stream.respond(response),
    };
    stream.start_event_stream()?;
    write_mcp_http_sse_event(stream, &subscription_acknowledged(&id, filter))?;
    let mut quiet = Duration::ZERO;
    loop {
        thread::sleep(poll_interval);
        let valid = authenticate_mcp_http_request(context, request)
            .is_ok_and(|current| authority.matches(&current))
            && core.attenuate_profile().is_ok();
        if !valid {
            return write_mcp_http_sse_event(stream, &subscription_closed(&id));
        }
        let mut sent = false;
        for notification in core.list_changes() {
            let permitted = mcp_notification_scope(&notification)
                .is_none_or(|scope| authority.allows_scope(scope));
            if permitted && filter.wants(&notification) {
                write_mcp_http_sse_event(stream, &subscription_notification(&id, notification))?;
                sent = true;
            }
        }
        quiet = if sent {
            Duration::ZERO
        } else {
            quiet + poll_interval
        };
        if quiet >= keepalive_interval {
            write_mcp_http_sse_keepalive(stream)?;
            quiet = Duration::ZERO;
        }
    }
}

/// The routing headers of a modern request must agree with its body: the
/// body is authoritative, and a contradiction is refused rather than guessed.
fn modern_header_mismatch(request: &McpHttpRequest, payload: &Value) -> Option<String> {
    let version = requested_modern_version(payload).and_then(Value::as_str);
    if let Some(header) = request.headers.get("mcp-protocol-version") {
        if Some(header.as_str()) != version {
            return Some(format!(
                "the MCP-Protocol-Version header `{header}` does not match the protocol version in `_meta`"
            ));
        }
    }
    let method = payload.get("method").and_then(Value::as_str);
    if let Some(header) = request.headers.get("mcp-method") {
        if Some(header.as_str()) != method {
            return Some(format!(
                "the Mcp-Method header `{header}` does not match the request's method"
            ));
        }
    }
    if let Some(header) = request.headers.get("mcp-name") {
        let params = payload.get("params");
        let name = match method {
            Some("tools/call" | "prompts/get") => params.and_then(|params| params.get("name")),
            Some("resources/read") => params.and_then(|params| params.get("uri")),
            _ => None,
        }
        .and_then(Value::as_str);
        if Some(header.as_str()) != name {
            return Some(format!(
                "the Mcp-Name header `{header}` does not match the request's name or URI"
            ));
        }
    }
    None
}

/// HTTP status for a modern response: errors in the request itself are 400,
/// an unknown method 404, and anything the server answered 200.
fn modern_http_response(result: Result<McpHttpProcessResult, Value>) -> McpHttpResponse {
    let message = match result {
        Ok(result) if result.accepted_notification => {
            return McpHttpResponse {
                status: 202,
                content_type: None,
                body: Vec::new(),
                extra_headers: Vec::new(),
            };
        }
        Ok(result) => result
            .response
            .expect("MCP HTTP requests should produce a JSON-RPC response"),
        Err(error) => error,
    };
    let status = match message
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_i64)
    {
        Some(-32601) => 404,
        Some(-32700 | -32600 | -32602 | -32020 | -32021 | -32022) => 400,
        Some(-32603) => 500,
        _ => 200,
    };
    mcp_http_json_response(status, &message)
}

fn mcp_http_json_response(status: u16, message: &Value) -> McpHttpResponse {
    McpHttpResponse {
        status,
        content_type: Some("application/json"),
        body: serde_json::to_vec(message).expect("JSON-RPC message should serialize"),
        extra_headers: vec![("Cache-Control".to_string(), "no-store".to_string())],
    }
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
        let mut response = oauth_error_response(oauth, &message, Some("insufficient_scope"));
        response.status = 403;
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
    let mut core = (context.core_factory)(config)
        .map_err(|error| mcp_http_json_error_response(500, error.clone(), Value::Null))?;
    core.offer_subscriptions();
    core.keep_operations(Arc::new(McpHttpOperations {
        owned: McpOwnedOperations {
            registry: Arc::clone(&context.operations),
            owner: authority.operation_owner(),
        },
        hosted: hosted_operations(context, authority),
    }));
    Ok(core)
}

/// `operation_get` for one HTTP caller: a write registered in a named
/// remote's durable ledger, or a call recorded by this listener, in either
/// case only one this caller made.
#[derive(Debug)]
struct McpHttpOperations {
    owned: McpOwnedOperations,
    hosted: Option<HostedOperations>,
}

#[derive(Debug)]
struct HostedOperations {
    ledger: Arc<crate::hosted_jobs::HostedJobLedger>,
    named: crate::mcp_remote_runtime::NamedMcpRuntime,
    authority: McpSessionAuthority,
}

impl McpOperationLookup for McpHttpOperations {
    fn get(&self, operation_id: &str) -> Option<Value> {
        self.hosted
            .as_ref()
            .and_then(|hosted| {
                crate::mcp_hosted::named_mcp_operation_status(
                    &hosted.ledger,
                    &hosted.named,
                    &hosted.authority,
                    operation_id,
                )
            })
            .and_then(|report| serde_json::to_value(report).ok())
            .or_else(|| self.owned.get(operation_id))
    }

    // Tasks are the calls this listener records; hosted mutations keep their
    // durable ledger and `status_path` instead.
    fn task(&self, operation_id: &str) -> Option<Value> {
        self.owned.task(operation_id)
    }

    fn cancel(&self, operation_id: &str) -> bool {
        self.owned.cancel(operation_id)
    }
}

fn hosted_operations<C: McpRequestCore>(
    context: &McpHttpDriver<'_, C>,
    authority: &McpSessionAuthority,
) -> Option<HostedOperations> {
    #[cfg(feature = "oauth")]
    {
        let (hosted, named) = (context.hosted?, context.named_runtime.as_ref()?);
        Some(HostedOperations {
            ledger: hosted.executor.ledger(),
            named: named.clone(),
            authority: authority.clone(),
        })
    }
    #[cfg(not(feature = "oauth"))]
    {
        let _ = (context, authority);
        None
    }
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
    stream: &mut McpHttpConnection,
) -> io::Result<()> {
    let session_id = request
        .headers
        .get("mcp-session-id")
        .expect("daemon route preflight requires MCP session header");
    let session = match authorized_mcp_http_session(context, session_id, authority, true) {
        Ok(session) => session,
        Err(response) => {
            return stream.respond(response);
        }
    };

    context
        .sessions
        .watch_list_changes(LIST_WATCH_INTERVAL, C::list_changed_notifications);
    let end = serve_mcp_sse(&session, stream, || {
        authenticate_mcp_http_request(context, request)
            .is_ok_and(|current| session.authority.matches(&current))
            && session
                .core
                .lock()
                .expect("mcp core lock should not be poisoned")
                .attenuate_profile()
                .is_ok()
    })?;
    // A grant-bound session outlives the token its stream was opened with:
    // the client reconnects with a refreshed token, and every POST checks
    // the grant again. Other sessions end with their credential.
    let retire = match end {
        McpSseEnd::RetireSession => true,
        McpSseEnd::AuthorityLapsed => !session.authority.is_granted(),
        McpSseEnd::Disconnected => false,
    };
    if retire {
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
                missing_token,
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
                    (!missing_token).then_some("invalid_token"),
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
/// A 401 with the bearer challenge: where the resource metadata is and the
/// scope to ask for, and the error only when a token was sent (RFC 6750).
fn oauth_error_response(
    oauth: &McpOAuthMode,
    message: impl Into<String>,
    error: Option<&str>,
) -> McpHttpResponse {
    let message = message.into();
    let error = error.map_or_else(String::new, |error| {
        format!(
            "error=\"{error}\", error_description=\"{}\", ",
            escape_www_authenticate_value(&message)
        )
    });
    let challenge = format!(
        "Bearer {error}resource_metadata=\"{}\", scope=\"{MCP_OAUTH_SCOPE}\"",
        oauth_protected_resource_metadata_url(oauth),
    );
    let mut response = mcp_http_json_error_response(401, message, Value::Null);
    response
        .extra_headers
        .push(("WWW-Authenticate".to_string(), challenge));
    response
}

#[cfg(feature = "oauth")]
fn oauth_protected_resource_metadata_url(oauth: &McpOAuthMode) -> &str {
    oauth.protected_resource_metadata_url()
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
    use std::collections::BTreeMap;
    use std::time::Duration;
    use ulid::Ulid;
    use vulcan_app::mcp_session_protocol::{McpProtocolCore, McpProtocolHost};
    use vulcan_core::VaultPaths;

    // The driver hands each factory its configuration by value.
    #[allow(clippy::needless_pass_by_value)]
    fn factory(config: McpHttpProtocolConfig) -> Result<McpProtocolCore, String> {
        McpProtocolCore::new(
            &config.paths,
            config.permission_profile.as_deref(),
            McpProtocolHost {
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
            endpoint: "/mcp".into(),
            auth_token: None,
            bind_addr: "127.0.0.1:4321".parse().unwrap(),
            instance_id: Ulid::new(),
            sessions: Arc::new(McpSessionRegistry::new()),
            operations: Arc::default(),
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
                .map_or_default(|id| BTreeMap::from([("mcp-session-id".into(), id.into())])),
            body: Vec::new(),
        }
    }

    fn initialize() -> Value {
        json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
            "protocolVersion":"2025-11-25",
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

    fn modern(id: u64, method: &str, params: Value) -> Value {
        let mut params = params;
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {"name": "modern-test", "version": "1"},
        });
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    fn body(response: &McpHttpResponse) -> Value {
        serde_json::from_slice(&response.body).unwrap()
    }

    #[test]
    fn modern_requests_are_served_without_a_session() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        vulcan_core::initialize_vulcan_dir(&paths).unwrap();
        let host = host(&paths);
        let driver = driver(&host);
        let mut inbound = request(None);
        let authority = authenticate_mcp_http_request(&driver, &inbound).unwrap();

        let list = handle_mcp_http_post(
            &driver,
            &inbound,
            &authority,
            &modern(1, "tools/list", json!({})),
        );
        assert_eq!(list.status, 200);
        assert!(list
            .extra_headers
            .iter()
            .all(|(name, _)| name != "Mcp-Session-Id"));
        let list = body(&list);
        assert_eq!(list["result"]["resultType"], "complete");
        assert!(list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "note_get"));
        // HTTP keeps operation records, so operation_get is offered.
        assert!(list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "operation_get"));
        let missing = handle_mcp_http_post(
            &driver,
            &inbound,
            &authority,
            &modern(
                5,
                "tools/call",
                json!({"name": "operation_get", "arguments": {"id": "01NOSUCHOPERATION"}}),
            ),
        );
        assert_eq!(
            body(&missing)["result"]["structuredContent"]["error"]["code"],
            "not_found"
        );
        assert!(host.sessions.is_empty());

        let unknown = handle_mcp_http_post(
            &driver,
            &inbound,
            &authority,
            &modern(2, "no/such", json!({})),
        );
        assert_eq!(unknown.status, 404);

        inbound
            .headers
            .insert("mcp-method".into(), "tools/call".into());
        let mismatch = handle_mcp_http_post(
            &driver,
            &inbound,
            &authority,
            &modern(3, "tools/list", json!({})),
        );
        assert_eq!(mismatch.status, 400);
        assert_eq!(body(&mismatch)["error"]["code"], MCP_HEADER_MISMATCH);

        inbound.headers.insert("mcp-name".into(), "note_get".into());
        let call = modern(
            4,
            "tools/call",
            json!({"name": "note_get", "arguments": {"note": "Missing"}}),
        );
        let named = handle_mcp_http_post(&driver, &inbound, &authority, &call);
        assert_eq!(named.status, 200, "{}", body(&named));
        assert_eq!(body(&named)["result"]["isError"], true);
        inbound.headers.insert("mcp-name".into(), "note_set".into());
        assert_eq!(
            handle_mcp_http_post(&driver, &inbound, &authority, &call).status,
            400
        );
        inbound.headers.remove("mcp-name");
        inbound.headers.remove("mcp-method");
        inbound
            .headers
            .insert("mcp-protocol-version".into(), "2025-11-25".into());
        assert_eq!(
            handle_mcp_http_post(&driver, &inbound, &authority, &call).status,
            400
        );
        assert!(host.sessions.is_empty());
    }

    #[test]
    fn requests_for_a_foreign_host_are_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let host = host(&paths);
        let driver = driver(&host);
        let mut inbound = request(None);
        inbound.headers.insert("host".into(), "evil.example".into());
        let (mut connection, capture) = McpHttpConnection::for_test();
        handle_mcp_http_connection(&driver, &inbound, &mut connection).unwrap();
        assert_eq!(capture.response().0, 421);
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
