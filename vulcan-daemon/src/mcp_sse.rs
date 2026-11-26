//! A legacy session's GET event stream, for foreground and resident MCP hosts.
//!
//! The stream delivers what is broadcast to its session: list changes found by
//! the listener's change watcher (`McpSessionRegistry::watch_list_changes`) and
//! those noticed while answering requests. It owns keepalives and the decision
//! to close: when the session expires or its authority is no longer valid.

use crate::mcp_http_codec::{write_mcp_http_sse_event, write_mcp_http_sse_keepalive};
use crate::mcp_session::McpHttpSession;
use crate::mcp_transport::McpHttpConnection;
use serde_json::Value;
use std::io::{self, Write};
use std::sync::mpsc;
use std::time::Duration;

/// How often an idle stream checks its session and authority.
const MCP_SSE_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const MCP_SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSseEnd {
    /// The session expired or its original authority became invalid.
    RetireSession,
    /// The session was already closed and its subscriber channel disconnected.
    Disconnected,
}

pub fn serve_mcp_sse<C>(
    session: &McpHttpSession<C>,
    stream: &mut McpHttpConnection,
    authority_valid: impl FnMut() -> bool,
) -> io::Result<McpSseEnd> {
    stream.start_event_stream()?;
    let receiver = session.register_subscriber();
    run_mcp_sse(
        session,
        stream,
        &receiver,
        authority_valid,
        MCP_SSE_CHECK_INTERVAL,
        MCP_SSE_KEEPALIVE_INTERVAL,
    )
}

fn run_mcp_sse<C>(
    session: &McpHttpSession<C>,
    stream: &mut impl Write,
    receiver: &mpsc::Receiver<Value>,
    mut authority_valid: impl FnMut() -> bool,
    check_interval: Duration,
    keepalive_interval: Duration,
) -> io::Result<McpSseEnd> {
    let mut quiet = Duration::ZERO;
    loop {
        let next_event = receiver.recv_timeout(check_interval);
        if session.is_idle_expired() {
            return Ok(McpSseEnd::RetireSession);
        }
        if matches!(&next_event, Err(mpsc::RecvTimeoutError::Disconnected)) {
            return Ok(McpSseEnd::Disconnected);
        }
        // Check after receiving, before sending: a grant can be revoked while
        // the subscriber is parked or while an event is queued.
        if !authority_valid() {
            return Ok(McpSseEnd::RetireSession);
        }
        match next_event {
            Ok(message) => {
                write_mcp_http_sse_event(stream, &message)?;
                quiet = Duration::ZERO;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                quiet += check_interval;
                if quiet >= keepalive_interval {
                    write_mcp_http_sse_keepalive(stream)?;
                    quiet = Duration::ZERO;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Ok(McpSseEnd::Disconnected);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_session::{McpSessionAuthority, McpSessionRegistry};
    use std::sync::Arc;
    use std::time::Instant;
    use ulid::Ulid;

    fn authority(scopes: &[&str]) -> McpSessionAuthority {
        McpSessionAuthority::direct(
            Ulid::new(),
            "credential",
            None,
            None,
            None,
            scopes.iter().map(|scope| (*scope).to_string()).collect(),
        )
    }

    fn session(scopes: &[&str]) -> Arc<McpHttpSession<()>> {
        Arc::new(McpHttpSession::new((), authority(scopes)))
    }

    #[test]
    fn queued_events_are_not_sent_after_authority_is_revoked() {
        let session = session(&["mcp:tools"]);
        let receiver = session.register_subscriber();
        session.broadcast(&[serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/tools/list_changed"
        })]);
        let mut output = Vec::new();
        let end = run_mcp_sse(
            &session,
            &mut output,
            &receiver,
            || false,
            Duration::from_millis(1),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(end, McpSseEnd::RetireSession);
        assert!(output.is_empty());
    }

    #[test]
    fn broadcast_events_need_the_mcp_scope_and_idle_streams_keep_alive() {
        let changes = [
            serde_json::json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}),
            serde_json::json!({"jsonrpc":"2.0","method":"notifications/resources/list_changed"}),
        ];
        let unscoped = session(&["openid"]);
        let unscoped_receiver = unscoped.register_subscriber();
        unscoped.broadcast(&changes);
        assert!(unscoped_receiver.try_recv().is_err());

        let session = session(&["mcp"]);
        let receiver = session.register_subscriber();
        session.broadcast(&changes);
        let mut output = Vec::new();
        let mut checks = 0;
        let end = run_mcp_sse(
            &session,
            &mut output,
            &receiver,
            || {
                checks += 1;
                checks < 5
            },
            Duration::from_millis(1),
            Duration::from_millis(2),
        )
        .unwrap();
        assert_eq!(end, McpSseEnd::RetireSession);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("notifications/tools/list_changed"));
        assert!(output.contains("notifications/resources/list_changed"));
        assert!(output.contains(": keepalive"));
    }

    #[test]
    fn the_change_watcher_polls_only_sessions_with_a_stream() {
        let registry = Arc::new(McpSessionRegistry::<u32>::new());
        let streaming = Arc::new(McpHttpSession::new(0_u32, authority(&["mcp:tools"])));
        let idle = Arc::new(McpHttpSession::new(0_u32, authority(&["mcp:tools"])));
        registry
            .admit("streaming".into(), Arc::clone(&streaming))
            .unwrap();
        registry.admit("idle".into(), Arc::clone(&idle)).unwrap();
        let receiver = streaming.register_subscriber();
        registry.watch_list_changes(Duration::from_millis(5), |polls| {
            *polls += 1;
            vec![serde_json::json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"})]
        });
        // A second start is ignored.
        registry.watch_list_changes(Duration::from_millis(5), |_| Vec::new());
        let event = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the watcher should deliver a change");
        assert_eq!(event["method"], "notifications/tools/list_changed");
        let deadline = Instant::now() + Duration::from_secs(5);
        while *streaming.core.lock().unwrap() == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(*streaming.core.lock().unwrap() > 0);
        assert_eq!(*idle.core.lock().unwrap(), 0);
    }
}
