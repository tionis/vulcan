//! Authority binding for one Streamable HTTP MCP session.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use ulid::Ulid;
use vulcan_app::execution::ExecutionCancellationToken;

use crate::mcp_remote::McpRemoteId;
use crate::registry::WikiId;

pub const MAX_MCP_SSE_PENDING_EVENTS: usize = 32;
pub const MCP_HTTP_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

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
        "notifications/tools/list_changed" => Some("mcp:tools"),
        "notifications/resources/list_changed" => Some("mcp:resources"),
        "notifications/prompts/list_changed" => Some("mcp:prompts"),
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
    pub tool_packs: Vec<String>,
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
            .field("tool_packs", &self.tool_packs)
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
            && self.tool_packs == other.tool_packs
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
        tool_packs: Vec<String>,
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
            tool_packs,
            scopes,
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
        tool_packs: Vec<String>,
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
            tool_packs,
            scopes,
            credential_fingerprint: fingerprint(credential),
        }
    }

    #[must_use]
    pub fn matches(&self, request: &Self) -> bool {
        self == request
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
            vec!["notes-read".to_string()],
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
        assert!(!expected.matches(&authority(
            instance,
            "https://id.example/alice",
            grant,
            "token-b"
        )));
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
        assert_eq!(
            receiver.try_recv().expect("tool notification")["method"],
            "notifications/tools/list_changed"
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
}
