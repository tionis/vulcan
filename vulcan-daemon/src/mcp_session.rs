//! Authority binding for one Streamable HTTP MCP session.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use ulid::Ulid;
use vulcan_app::execution::ExecutionCancellationToken;
use vulcan_app::mcp_dispatch::http_initialize_request;
use vulcan_app::mcp_dispatch::McpHttpProcessResult;
use vulcan_app::mcp_protocol::{normalize_mcp_scopes, MCP_OAUTH_SCOPE};

use crate::mcp_http_codec::McpHttpResponse;
use crate::mcp_remote::McpRemoteId;
use crate::registry::WikiId;

pub const MAX_MCP_SSE_PENDING_EVENTS: usize = 32;
pub const MAX_MCP_HTTP_SESSIONS: usize = 256;
pub const MCP_HTTP_SESSION_IDLE_TIMEOUT: Duration = Duration::from_mins(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpCancellationError {
    MissingRequestId,
    InvalidRequestId,
}

/// Removes a request ID from its session when the HTTP handler exits, including
/// early error and timeout responses.
#[must_use]
pub struct McpActiveRequest<C> {
    session: Arc<McpHttpSession<C>>,
    id: Option<Value>,
    cancellation: ExecutionCancellationToken,
}

impl<C> McpActiveRequest<C> {
    #[must_use]
    pub fn cancellation(&self) -> ExecutionCancellationToken {
        self.cancellation.clone()
    }
}

impl<C> Drop for McpActiveRequest<C> {
    fn drop(&mut self) {
        if let Some(id) = self.id.as_ref() {
            self.session.finish_request(id);
        }
    }
}

/// Transport-owned lifecycle state; the protocol handler remains host supplied.
#[derive(Debug)]
pub struct McpHttpSession<C> {
    pub authority: McpSessionAuthority,
    pub core: Mutex<C>,
    active_requests: Mutex<BTreeMap<String, ExecutionCancellationToken>>,
    subscribers: Mutex<Vec<mpsc::SyncSender<Value>>>,
    idle_deadline: Mutex<Instant>,
    idle_timeout: Duration,
    closed: AtomicBool,
}

impl<C> McpHttpSession<C> {
    /// Register one active JSON-RPC request and tie cleanup to a scoped guard.
    /// A notification has no request ID but still receives a cancellation token.
    pub fn start_request(self: &Arc<Self>, id: Option<&Value>) -> Option<McpActiveRequest<C>> {
        let cancellation = ExecutionCancellationToken::default();
        if id.is_some_and(|id| !self.register_request(id, cancellation.clone())) {
            return None;
        }
        Some(McpActiveRequest {
            session: Arc::clone(self),
            id: id.cloned(),
            cancellation,
        })
    }

    /// Parse a cancellation notification without waiting for the protocol core.
    pub fn cancel_notification(&self, payload: &Value) -> Result<(), McpCancellationError> {
        let target = payload
            .pointer("/params/requestId")
            .ok_or(McpCancellationError::MissingRequestId)?;
        if mcp_request_key(target).is_none() {
            return Err(McpCancellationError::InvalidRequestId);
        }
        self.cancel_request(target);
        Ok(())
    }

    pub fn new(core: C, authority: McpSessionAuthority) -> Self {
        Self::new_with_idle_timeout(core, authority, MCP_HTTP_SESSION_IDLE_TIMEOUT)
    }

    pub fn new_with_idle_timeout(
        core: C,
        authority: McpSessionAuthority,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            authority,
            core: Mutex::new(core),
            active_requests: Mutex::new(BTreeMap::new()),
            subscribers: Mutex::new(Vec::new()),
            idle_deadline: Mutex::new(Instant::now() + idle_timeout),
            idle_timeout,
            closed: AtomicBool::new(false),
        }
    }

    pub fn touch(&self) {
        *self
            .idle_deadline
            .lock()
            .expect("mcp session deadline lock should not be poisoned") =
            Instant::now() + self.idle_timeout;
    }

    #[must_use]
    pub fn is_idle_expired(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
            || *self
                .idle_deadline
                .lock()
                .expect("mcp session deadline lock should not be poisoned")
                <= Instant::now()
    }

    /// Whether a GET stream is open on this session.
    #[must_use]
    pub fn has_subscribers(&self) -> bool {
        !self
            .subscribers
            .lock()
            .expect("mcp subscribers lock should not be poisoned")
            .is_empty()
    }

    #[must_use]
    pub fn register_subscriber(&self) -> mpsc::Receiver<Value> {
        let (tx, rx) = mpsc::sync_channel(MAX_MCP_SSE_PENDING_EVENTS);
        self.subscribers
            .lock()
            .expect("mcp subscribers lock should not be poisoned")
            .push(tx);
        rx
    }

    pub fn broadcast(&self, messages: &[Value]) {
        if messages.is_empty() || self.closed.load(Ordering::SeqCst) {
            return;
        }
        let visible = messages
            .iter()
            .filter(|message| {
                mcp_notification_scope(message)
                    .is_none_or(|scope| self.authority.allows_scope(scope))
            })
            .collect::<Vec<_>>();
        if visible.is_empty() {
            return;
        }
        let mut subscribers = self
            .subscribers
            .lock()
            .expect("mcp subscribers lock should not be poisoned");
        subscribers.retain(|sender| {
            visible
                .iter()
                .all(|message| sender.try_send((*message).clone()).is_ok())
        });
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let mut active = self
            .active_requests
            .lock()
            .expect("mcp active requests lock should not be poisoned");
        for cancellation in active.values() {
            cancellation.cancel();
        }
        active.clear();
        self.subscribers
            .lock()
            .expect("mcp subscribers lock should not be poisoned")
            .clear();
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn register_request(&self, id: &Value, cancellation: ExecutionCancellationToken) -> bool {
        let Some(key) = mcp_request_key(id) else {
            return false;
        };
        let mut active = self
            .active_requests
            .lock()
            .expect("mcp active requests lock should not be poisoned");
        if self.is_closed() || active.contains_key(&key) {
            return false;
        }
        active.insert(key, cancellation);
        true
    }

    pub fn finish_request(&self, id: &Value) {
        if let Some(key) = mcp_request_key(id) {
            self.active_requests
                .lock()
                .expect("mcp active requests lock should not be poisoned")
                .remove(&key);
        }
    }

    pub fn cancel_request(&self, id: &Value) {
        if let Some(key) = mcp_request_key(id) {
            if let Some(cancellation) = self
                .active_requests
                .lock()
                .expect("mcp active requests lock should not be poisoned")
                .get(&key)
            {
                cancellation.cancel();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAdmissionError {
    Capacity,
    DuplicateId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLookupError {
    Missing,
    AuthorityMismatch,
}

#[derive(Debug)]
pub enum SessionResolutionError<E> {
    InvalidInitialize(Value),
    MissingSessionId,
    Admission(SessionAdmissionError),
    Lookup(SessionLookupError),
    Create(E),
}

#[derive(Debug)]
pub struct ResolvedMcpSession<C> {
    pub id: String,
    pub session: Arc<McpHttpSession<C>>,
    pub created: bool,
}

/// Per-listener session ownership and bounded admission for both hosting modes.
#[derive(Debug)]
pub struct McpSessionRegistry<C> {
    sessions: Mutex<BTreeMap<String, Arc<McpHttpSession<C>>>>,
    max_sessions: usize,
    watching: AtomicBool,
}

impl<C> McpSessionRegistry<C> {
    /// Apply one completed protocol result to its session and render the
    /// Streamable HTTP response. A timed-out/stale worker must not broadcast
    /// events or leave its original session reusable.
    pub fn finish_http_post(
        &self,
        resolved: ResolvedMcpSession<C>,
        result: Result<McpHttpProcessResult, Value>,
    ) -> McpHttpResponse {
        let ResolvedMcpSession {
            id,
            session,
            created,
        } = resolved;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                if created {
                    self.retire(&id);
                }
                return McpHttpResponse {
                    status: 400,
                    content_type: Some("application/json"),
                    body: serde_json::to_vec(&error).expect("JSON-RPC error should serialize"),
                    extra_headers: Vec::new(),
                };
            }
        };
        if result.session_stale {
            self.retire(&id);
        } else {
            session.broadcast(&result.notifications);
        }
        if result.accepted_notification {
            return McpHttpResponse {
                status: 202,
                content_type: None,
                body: Vec::new(),
                extra_headers: Vec::new(),
            };
        }
        let response = result
            .response
            .expect("MCP HTTP requests should produce a JSON-RPC response");
        let mut extra_headers = Vec::new();
        if created && !result.session_stale {
            extra_headers.push(("Mcp-Session-Id".to_string(), id));
        }
        McpHttpResponse {
            status: 200,
            content_type: Some("application/json"),
            body: serde_json::to_vec(&response).expect("JSON-RPC response should serialize"),
            extra_headers,
        }
    }

    /// Resolve a decoded HTTP POST against one authority. The host supplies
    /// only protocol-core construction for a valid initialization request.
    pub fn resolve_http<E>(
        &self,
        payload: &Value,
        session_header: Option<&str>,
        authority: &McpSessionAuthority,
        create_core: impl FnOnce() -> Result<C, E>,
    ) -> Result<ResolvedMcpSession<C>, SessionResolutionError<E>> {
        if http_initialize_request(payload).map_err(SessionResolutionError::InvalidInitialize)? {
            let core = create_core().map_err(SessionResolutionError::Create)?;
            let id = Ulid::new().to_string();
            let session = Arc::new(McpHttpSession::new(core, authority.clone()));
            self.admit(id.clone(), Arc::clone(&session))
                .map_err(SessionResolutionError::Admission)?;
            return Ok(ResolvedMcpSession {
                id,
                session,
                created: true,
            });
        }
        let id = session_header.ok_or(SessionResolutionError::MissingSessionId)?;
        let session = self
            .authorized(id, authority, true)
            .map_err(SessionResolutionError::Lookup)?;
        Ok(ResolvedMcpSession {
            id: id.to_string(),
            session,
            created: false,
        })
    }

    #[must_use]
    pub fn new() -> Self {
        Self::with_limit(MAX_MCP_HTTP_SESSIONS)
    }

    #[must_use]
    pub fn with_limit(max_sessions: usize) -> Self {
        Self {
            sessions: Mutex::new(BTreeMap::new()),
            max_sessions,
            watching: AtomicBool::new(false),
        }
    }

    pub fn admit(
        &self,
        session_id: String,
        session: Arc<McpHttpSession<C>>,
    ) -> Result<(), SessionAdmissionError> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("mcp sessions lock should not be poisoned");
        sessions.retain(|_, existing| {
            if existing.is_idle_expired() {
                existing.close();
                false
            } else {
                true
            }
        });
        if sessions.contains_key(&session_id) {
            return Err(SessionAdmissionError::DuplicateId);
        }
        if sessions.len() >= self.max_sessions {
            return Err(SessionAdmissionError::Capacity);
        }
        sessions.insert(session_id, session);
        Ok(())
    }

    #[must_use]
    pub fn live(&self, session_id: &str) -> Option<Arc<McpHttpSession<C>>> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("mcp sessions lock should not be poisoned");
        let session = sessions.get(session_id)?.clone();
        if session.is_idle_expired() {
            sessions.remove(session_id);
            session.close();
            None
        } else {
            Some(session)
        }
    }

    pub fn authorized(
        &self,
        session_id: &str,
        authority: &McpSessionAuthority,
        touch: bool,
    ) -> Result<Arc<McpHttpSession<C>>, SessionLookupError> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("mcp sessions lock should not be poisoned");
        let session = sessions
            .get(session_id)
            .cloned()
            .ok_or(SessionLookupError::Missing)?;
        if session.is_idle_expired() {
            sessions.remove(session_id);
            session.close();
            return Err(SessionLookupError::Missing);
        }
        if !session.authority.matches(authority) {
            return Err(SessionLookupError::AuthorityMismatch);
        }
        if touch {
            session.touch();
        }
        Ok(session)
    }

    pub fn retire(&self, session_id: &str) -> Option<Arc<McpHttpSession<C>>> {
        let session = self
            .sessions
            .lock()
            .expect("mcp sessions lock should not be poisoned")
            .remove(session_id);
        if let Some(session) = session.as_ref() {
            session.close();
        }
        session
    }

    /// Start, once, the listener's change watcher: every `interval` it asks
    /// each session with an open stream what changed in its lists (`poll`)
    /// and sends that to the stream. A session busy with a request is asked
    /// next time. The watcher stops once the registry is dropped.
    pub fn watch_list_changes(self: &Arc<Self>, interval: Duration, poll: fn(&mut C) -> Vec<Value>)
    where
        C: Send + 'static,
    {
        if self.watching.swap(true, Ordering::SeqCst) {
            return;
        }
        let registry = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("vulcan-mcp-list-watch".to_string())
            .spawn(move || loop {
                std::thread::sleep(interval);
                let Some(registry) = registry.upgrade() else {
                    return;
                };
                let streaming = registry
                    .sessions
                    .lock()
                    .expect("mcp sessions lock should not be poisoned")
                    .values()
                    .filter(|session| !session.is_closed() && session.has_subscribers())
                    .cloned()
                    .collect::<Vec<_>>();
                drop(registry);
                for session in streaming {
                    let Ok(mut core) = session.core.try_lock() else {
                        continue;
                    };
                    let changes = poll(&mut core);
                    drop(core);
                    session.broadcast(&changes);
                }
            });
        if spawned.is_err() {
            self.watching.store(false, Ordering::SeqCst);
        }
    }

    pub fn close_all(&self) {
        let mut sessions = self
            .sessions
            .lock()
            .expect("mcp sessions lock should not be poisoned");
        for (_, session) in std::mem::take(&mut *sessions) {
            session.close();
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions
            .lock()
            .expect("mcp sessions lock should not be poisoned")
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<C> Default for McpSessionRegistry<C> {
    fn default() -> Self {
        Self::new()
    }
}

#[must_use]
pub fn mcp_request_key(id: &Value) -> Option<String> {
    match id {
        Value::String(_) | Value::Number(_) => serde_json::to_string(id).ok(),
        _ => None,
    }
}

#[must_use]
pub fn mcp_notification_scope(message: &Value) -> Option<&'static str> {
    match message.get("method")?.as_str()? {
        "notifications/tools/list_changed"
        | "notifications/resources/list_changed"
        | "notifications/prompts/list_changed" => Some(MCP_OAUTH_SCOPE),
        _ => None,
    }
}

#[derive(Clone, Eq, Serialize)]
pub struct McpSessionAuthority {
    pub remote_id: Option<McpRemoteId>,
    pub remote_instance_id: Ulid,
    pub grant_id: Option<Ulid>,
    pub client_id: Option<String>,
    pub subject: Option<String>,
    pub wiki_id: Option<WikiId>,
    pub audience: Option<String>,
    pub permission_profile: Option<String>,
    pub scopes: Vec<String>,
    #[serde(skip)]
    credential_fingerprint: String,
}

impl std::fmt::Debug for McpSessionAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpSessionAuthority")
            .field("remote_id", &self.remote_id)
            .field("remote_instance_id", &self.remote_instance_id)
            .field("grant_id", &self.grant_id)
            .field("client_id", &self.client_id)
            .field("subject", &self.subject)
            .field("wiki_id", &self.wiki_id)
            .field("audience", &self.audience)
            .field("permission_profile", &self.permission_profile)
            .field("scopes", &self.scopes)
            .field("credential_fingerprint", &"[REDACTED]")
            .finish()
    }
}

impl PartialEq for McpSessionAuthority {
    fn eq(&self, other: &Self) -> bool {
        self.remote_id == other.remote_id
            && self.remote_instance_id == other.remote_instance_id
            && self.grant_id == other.grant_id
            && self.client_id == other.client_id
            && self.subject == other.subject
            && self.wiki_id == other.wiki_id
            && self.audience == other.audience
            && self.permission_profile == other.permission_profile
            && self.scopes == other.scopes
            && bool::from(
                self.credential_fingerprint
                    .as_bytes()
                    .ct_eq(other.credential_fingerprint.as_bytes()),
            )
    }
}

impl McpSessionAuthority {
    #[must_use]
    pub fn direct(
        remote_instance_id: Ulid,
        credential: &str,
        client_id: Option<String>,
        subject: Option<String>,
        permission_profile: Option<String>,
        scopes: Vec<String>,
    ) -> Self {
        Self {
            remote_id: None,
            remote_instance_id,
            grant_id: None,
            client_id,
            subject,
            wiki_id: None,
            audience: None,
            permission_profile,
            scopes: normalize_mcp_scopes(scopes),
            credential_fingerprint: fingerprint(credential),
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn granted(
        remote_id: McpRemoteId,
        remote_instance_id: Ulid,
        grant_id: Ulid,
        client_id: String,
        subject: String,
        wiki_id: WikiId,
        audience: String,
        permission_profile: String,
        scopes: Vec<String>,
        credential: &str,
    ) -> Self {
        Self {
            remote_id: Some(remote_id),
            remote_instance_id,
            grant_id: Some(grant_id),
            client_id: Some(client_id),
            subject: Some(subject),
            wiki_id: Some(wiki_id),
            audience: Some(audience),
            permission_profile: Some(permission_profile),
            scopes: normalize_mcp_scopes(scopes),
            credential_fingerprint: fingerprint(credential),
        }
    }

    /// Whether a request may use a session admitted under this authority.
    ///
    /// A session bound to a grant follows the grant, not the access token: a
    /// client that refreshes its token keeps its session, as long as every
    /// other boundary (remote, listener, client, subject, vault, audience,
    /// profile and scopes) is unchanged. Other sessions need the same
    /// credential.
    #[must_use]
    pub fn matches(&self, request: &Self) -> bool {
        if self.grant_id.is_some() && self.grant_id == request.grant_id {
            let mut same_credential = request.clone();
            same_credential
                .credential_fingerprint
                .clone_from(&self.credential_fingerprint);
            return *self == same_credential;
        }
        self == request
    }

    /// Whether this authority comes from a stored grant.
    #[must_use]
    pub fn is_granted(&self) -> bool {
        self.grant_id.is_some()
    }

    /// Who owns the operations this caller starts: the connection grant when
    /// there is one (it survives token refresh), otherwise the credential, on
    /// this listener and vault.
    #[must_use]
    pub fn operation_owner(&self) -> String {
        let caller = self.grant_id.map_or_else(
            || self.credential_fingerprint.clone(),
            |grant| grant.to_string(),
        );
        let wiki = self.wiki_id.as_ref().map_or_default(ToString::to_string);
        format!("{}/{wiki}/{caller}", self.remote_instance_id)
    }

    #[must_use]
    pub fn allows_scope(&self, required: &str) -> bool {
        self.scopes.iter().any(|scope| scope == required)
    }
}

fn fingerprint(credential: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(credential.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority(
        instance: Ulid,
        subject: &str,
        grant: Ulid,
        credential: &str,
    ) -> McpSessionAuthority {
        McpSessionAuthority::granted(
            McpRemoteId::parse("chatgpt").expect("remote"),
            instance,
            grant,
            "client".to_string(),
            subject.to_string(),
            WikiId::parse("personal").expect("wiki"),
            "https://mcp.example.test/personal".to_string(),
            "readonly".to_string(),
            vec!["mcp:tools".to_string()],
            credential,
        )
    }

    #[test]
    fn session_authority_isolated_by_every_security_boundary() {
        let instance = Ulid::new();
        let grant = Ulid::new();
        let expected = authority(instance, "https://id.example/alice", grant, "token-a");
        assert!(expected.matches(&authority(
            instance,
            "https://id.example/alice",
            grant,
            "token-a"
        )));
        assert!(!expected.matches(&authority(
            instance,
            "https://id.example/bob",
            grant,
            "token-a"
        )));
        assert!(!expected.matches(&authority(
            Ulid::new(),
            "https://id.example/alice",
            grant,
            "token-a"
        )));
        assert!(!expected.matches(&authority(
            instance,
            "https://id.example/alice",
            Ulid::new(),
            "token-a"
        )));
        // A refreshed token for the same grant keeps the session.
        assert!(expected.matches(&authority(
            instance,
            "https://id.example/alice",
            grant,
            "token-b"
        )));
        assert!(!expected.matches(&authority(
            Ulid::new(),
            "https://id.example/alice",
            grant,
            "token-b"
        )));
    }

    #[test]
    fn a_refreshed_token_reaches_its_grants_session() {
        let instance = Ulid::new();
        let grant = Ulid::new();
        let registry = McpSessionRegistry::<()>::new();
        let original = authority(instance, "https://id.example/alice", grant, "token-a");
        registry
            .admit(
                "session".into(),
                Arc::new(McpHttpSession::new((), original)),
            )
            .unwrap();
        let refreshed = authority(instance, "https://id.example/alice", grant, "token-b");
        assert!(registry.authorized("session", &refreshed, true).is_ok());
        let other_grant = authority(instance, "https://id.example/alice", Ulid::new(), "token-b");
        assert!(matches!(
            registry.authorized("session", &other_grant, true),
            Err(SessionLookupError::AuthorityMismatch)
        ));
    }

    #[test]
    fn sessions_without_a_grant_need_the_same_credential() {
        let instance = Ulid::new();
        let direct = |credential: &str| {
            McpSessionAuthority::direct(instance, credential, None, None, None, vec!["mcp".into()])
        };
        assert!(direct("token-a").matches(&direct("token-a")));
        assert!(!direct("token-a").matches(&direct("token-b")));
        assert!(!direct("token-a").is_granted());
    }

    #[test]
    fn reports_never_serialize_the_credential_fingerprint() {
        let value = serde_json::to_string(&authority(
            Ulid::new(),
            "https://id.example/alice",
            Ulid::new(),
            "raw-secret",
        ))
        .expect("serialize");
        assert!(!value.contains("raw-secret"));
        assert!(!value.contains("credential_fingerprint"));
    }

    #[test]
    fn hosted_session_filters_notifications_and_cancels_registered_requests() {
        let session = McpHttpSession::new(
            (),
            authority(
                Ulid::new(),
                "https://id.example/alice",
                Ulid::new(),
                "token",
            ),
        );
        let receiver = session.register_subscriber();
        session.broadcast(&[
            serde_json::json!({"method": "notifications/tools/list_changed"}),
            serde_json::json!({"method": "notifications/resources/list_changed"}),
        ]);
        // One scope covers every list.
        assert_eq!(
            receiver.try_recv().expect("tool notification")["method"],
            "notifications/tools/list_changed"
        );
        assert_eq!(
            receiver.try_recv().expect("resource notification")["method"],
            "notifications/resources/list_changed"
        );
        assert!(receiver.try_recv().is_err());

        let cancellation = ExecutionCancellationToken::default();
        let id = serde_json::json!(17);
        assert!(session.register_request(&id, cancellation.clone()));
        assert!(!session.register_request(&id, ExecutionCancellationToken::default()));
        session.cancel_request(&id);
        assert!(cancellation.is_cancelled());
        session.finish_request(&id);
        assert!(session.register_request(&id, ExecutionCancellationToken::default()));
        session.close();
        assert!(session.is_closed());
        assert!(session.is_idle_expired());
        assert!(!session.register_request(&id, ExecutionCancellationToken::default()));
    }

    #[test]
    fn active_request_guard_cleans_up_and_cancellation_validates_target_id() {
        let session = Arc::new(McpHttpSession::new(
            (),
            authority(
                Ulid::new(),
                "https://id.example/alice",
                Ulid::new(),
                "token",
            ),
        ));
        let id = serde_json::json!(17);
        let active = session.start_request(Some(&id)).expect("register request");
        let cancellation = active.cancellation();
        assert!(session.start_request(Some(&id)).is_none());
        assert!(session
            .start_request(Some(&serde_json::json!({})))
            .is_none());
        assert_eq!(
            session.cancel_notification(&serde_json::json!({"params": {}})),
            Err(McpCancellationError::MissingRequestId)
        );
        assert_eq!(
            session.cancel_notification(&serde_json::json!({"params": {"requestId": null}})),
            Err(McpCancellationError::InvalidRequestId)
        );
        assert!(!cancellation.is_cancelled());
        session
            .cancel_notification(&serde_json::json!({"params": {"requestId": 17}}))
            .expect("cancel active request");
        assert!(cancellation.is_cancelled());
        drop(active);
        assert!(session.start_request(Some(&id)).is_some());
    }

    #[test]
    fn registry_bounds_admission_and_reclaims_expired_sessions() {
        let registry = McpSessionRegistry::with_limit(1);
        let authority = authority(
            Ulid::new(),
            "https://id.example/alice",
            Ulid::new(),
            "token",
        );
        let first = Arc::new(McpHttpSession::new((), authority.clone()));
        registry
            .admit("first".to_string(), Arc::clone(&first))
            .expect("first session");
        let second = Arc::new(McpHttpSession::new((), authority.clone()));
        assert_eq!(
            registry.admit("second".to_string(), Arc::clone(&second)),
            Err(SessionAdmissionError::Capacity)
        );
        assert_eq!(
            registry.admit("first".to_string(), Arc::clone(&second)),
            Err(SessionAdmissionError::DuplicateId)
        );
        registry.retire("first").expect("retired session");
        assert!(first.is_closed());
        let expiring = Arc::new(McpHttpSession::new_with_idle_timeout(
            (),
            authority.clone(),
            Duration::ZERO,
        ));
        registry
            .admit("expiring".to_string(), Arc::clone(&expiring))
            .expect("expiring session");
        registry
            .admit("second".to_string(), Arc::clone(&second))
            .expect("expired session reclaimed");
        assert!(expiring.is_closed());
        assert!(registry.live("expiring").is_none());
        assert!(registry.live("second").is_some());
        let other = self::authority(
            Ulid::new(),
            "https://id.example/bob",
            Ulid::new(),
            "other-token",
        );
        assert!(matches!(
            registry.authorized("second", &other, true),
            Err(SessionLookupError::AuthorityMismatch)
        ));
        assert!(registry.authorized("second", &authority, true).is_ok());
        assert!(matches!(
            registry.authorized("missing", &authority, true),
            Err(SessionLookupError::Missing)
        ));
        registry.close_all();
        assert!(second.is_closed());
        assert!(registry.is_empty());
    }

    #[test]
    fn http_resolution_admits_only_valid_initialization_and_reuses_exact_authority() {
        let registry = McpSessionRegistry::with_limit(1);
        let instance = Ulid::new();
        let grant = Ulid::new();
        let owner = authority(instance, "https://id.example/alice", grant, "token-a");
        let invalid = serde_json::json!({"jsonrpc":"2.0","method":"initialize"});
        assert!(matches!(
            registry.resolve_http(&invalid, None, &owner, || Ok::<_, ()>(())),
            Err(SessionResolutionError::InvalidInitialize(_))
        ));
        assert!(registry.is_empty());

        let initialize = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
        assert!(matches!(
            registry.resolve_http(&initialize, None, &owner, || Err::<(), _>(
                "core unavailable"
            )),
            Err(SessionResolutionError::Create("core unavailable"))
        ));
        assert!(registry.is_empty());

        let admitted = registry
            .resolve_http(&initialize, None, &owner, || Ok::<_, ()>(()))
            .expect("initialize session");
        assert!(admitted.created);
        assert_eq!(registry.len(), 1);
        assert!(matches!(
            registry.resolve_http(&initialize, None, &owner, || Ok::<_, ()>(())),
            Err(SessionResolutionError::Admission(
                SessionAdmissionError::Capacity
            ))
        ));

        let request = serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
        assert!(matches!(
            registry.resolve_http(&request, None, &owner, || Ok::<_, ()>(())),
            Err(SessionResolutionError::MissingSessionId)
        ));
        let other = authority(instance, "https://id.example/bob", grant, "token-b");
        assert!(matches!(
            registry.resolve_http(&request, Some(&admitted.id), &other, || Ok::<_, ()>(())),
            Err(SessionResolutionError::Lookup(
                SessionLookupError::AuthorityMismatch
            ))
        ));
        let resumed = registry
            .resolve_http(&request, Some(&admitted.id), &owner, || Err::<(), _>(()))
            .expect("resume owned session");
        assert!(!resumed.created);
        assert!(Arc::ptr_eq(&admitted.session, &resumed.session));
    }

    #[test]
    fn http_finalization_broadcasts_or_retires_and_preserves_response_contract() {
        let registry = McpSessionRegistry::new();
        let owner = authority(
            Ulid::new(),
            "https://id.example/alice",
            Ulid::new(),
            "token",
        );
        let initialize = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
        let resolved = registry
            .resolve_http(&initialize, None, &owner, || Ok::<_, ()>(()))
            .expect("initialize session");
        let id = resolved.id.clone();
        let receiver = resolved.session.register_subscriber();
        let response = registry.finish_http_post(
            resolved,
            Ok(McpHttpProcessResult {
                response: Some(serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}})),
                notifications: vec![
                    serde_json::json!({"method":"notifications/tools/list_changed"}),
                ],
                accepted_notification: false,
                session_stale: false,
            }),
        );
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, Some("application/json"));
        assert_eq!(
            response.extra_headers,
            [("Mcp-Session-Id".to_string(), id.clone())]
        );
        assert_eq!(
            receiver.try_recv().expect("broadcast")["method"],
            "notifications/tools/list_changed"
        );
        assert!(registry.live(&id).is_some());

        let request = serde_json::json!({"jsonrpc":"2.0","id":2,"method":"ping"});
        let resumed = registry
            .resolve_http(&request, Some(&id), &owner, || Err::<(), _>(()))
            .expect("resume session");
        let accepted = registry.finish_http_post(
            resumed,
            Ok(McpHttpProcessResult {
                response: None,
                notifications: Vec::new(),
                accepted_notification: true,
                session_stale: false,
            }),
        );
        assert_eq!(accepted.status, 202);
        assert!(accepted.extra_headers.is_empty());

        let resumed = registry
            .resolve_http(&request, Some(&id), &owner, || Err::<(), _>(()))
            .expect("resume session");
        let stale = registry.finish_http_post(
            resumed,
            Ok(McpHttpProcessResult {
                response: Some(serde_json::json!({"jsonrpc":"2.0","id":2,"result":{}})),
                notifications: vec![
                    serde_json::json!({"method":"notifications/tools/list_changed"}),
                ],
                accepted_notification: false,
                session_stale: true,
            }),
        );
        assert_eq!(stale.status, 200);
        assert!(stale.extra_headers.is_empty());
        assert!(registry.live(&id).is_none());
        assert!(receiver.try_recv().is_err());

        let failed = registry
            .resolve_http(&initialize, None, &owner, || Ok::<_, ()>(()))
            .expect("new session");
        let failed_id = failed.id.clone();
        let invalid = registry.finish_http_post(
            failed,
            Err(serde_json::json!({"jsonrpc":"2.0","id":1,"error":{"code":-32600}})),
        );
        assert_eq!(invalid.status, 400);
        assert!(invalid.extra_headers.is_empty());
        assert!(registry.live(&failed_id).is_none());
    }
}
