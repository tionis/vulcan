//! Shared Streamable HTTP SSE lifecycle for foreground and resident MCP hosts.
//!
//! The host supplies live authorization and protocol-list snapshots; the daemon
//! owns polling, scope-filtered event writes, keepalives, and close decisions.

use crate::mcp_http_codec::{
    write_mcp_http_sse_event, write_mcp_http_sse_headers, write_mcp_http_sse_keepalive,
};
use crate::mcp_session::{mcp_notification_scope, McpHttpSession};
use serde_json::Value;
use std::io::{self, Write};
use std::sync::mpsc;
use std::time::Duration;

const MCP_SSE_POLL_INTERVAL: Duration = Duration::from_millis(250);
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
    stream: &mut impl Write,
    authority_valid: impl FnMut() -> bool,
    list_changes: impl FnMut() -> Vec<Value>,
) -> io::Result<McpSseEnd> {
    serve_mcp_sse_with_timing(
        session,
        stream,
        authority_valid,
        list_changes,
        MCP_SSE_POLL_INTERVAL,
        MCP_SSE_KEEPALIVE_INTERVAL,
    )
}

fn serve_mcp_sse_with_timing<C>(
    session: &McpHttpSession<C>,
    stream: &mut impl Write,
    authority_valid: impl FnMut() -> bool,
    list_changes: impl FnMut() -> Vec<Value>,
    poll_interval: Duration,
    keepalive_interval: Duration,
) -> io::Result<McpSseEnd> {
    write_mcp_http_sse_headers(stream)?;
    let receiver = session.register_subscriber();
    run_mcp_sse(
        session,
        stream,
        &receiver,
        authority_valid,
        list_changes,
        poll_interval,
        keepalive_interval,
    )
}

fn run_mcp_sse<C>(
    session: &McpHttpSession<C>,
    stream: &mut impl Write,
    receiver: &mpsc::Receiver<Value>,
    mut authority_valid: impl FnMut() -> bool,
    mut list_changes: impl FnMut() -> Vec<Value>,
    poll_interval: Duration,
    keepalive_interval: Duration,
) -> io::Result<McpSseEnd> {
    let mut keepalive_elapsed = Duration::ZERO;
    loop {
        let next_event = receiver.recv_timeout(poll_interval);
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
                keepalive_elapsed = Duration::ZERO;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                for notification in list_changes() {
                    if mcp_notification_scope(&notification)
                        .is_none_or(|scope| session.authority.allows_scope(scope))
                    {
                        write_mcp_http_sse_event(stream, &notification)?;
                    }
                }
                keepalive_elapsed += poll_interval;
                if keepalive_elapsed >= keepalive_interval {
                    write_mcp_http_sse_keepalive(stream)?;
                    keepalive_elapsed = Duration::ZERO;
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
    use crate::mcp_session::McpSessionAuthority;
    use std::sync::Arc;
    use ulid::Ulid;

    fn session(scopes: &[&str]) -> Arc<McpHttpSession<()>> {
        Arc::new(McpHttpSession::new(
            (),
            McpSessionAuthority::direct(
                Ulid::new(),
                "credential",
                None,
                None,
                None,
                Vec::new(),
                scopes.iter().map(|scope| (*scope).to_string()).collect(),
            ),
        ))
    }

    #[test]
    fn queued_events_are_not_sent_after_authority_is_revoked() {
        let session = session(&["mcp:tools"]);
        let receiver = session.register_subscriber();
        session.broadcast(&[serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/tools/list_changed"
        })]);
        let mut output = Vec::new();
        write_mcp_http_sse_headers(&mut output).unwrap();
        let end = run_mcp_sse(
            &session,
            &mut output,
            &receiver,
            || false,
            Vec::new,
            Duration::from_millis(1),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(end, McpSseEnd::RetireSession);
        let output = String::from_utf8(output).unwrap();
        assert!(output.starts_with("HTTP/1.1 200 OK"));
        assert!(!output.contains("data: "));
    }

    #[test]
    fn polling_filters_catalog_events_and_emits_keepalive() {
        let session = session(&["mcp:tools"]);
        let mut output = Vec::new();
        let mut polls = 0;
        let end = serve_mcp_sse_with_timing(
            &session,
            &mut output,
            || {
                polls += 1;
                polls < 3
            },
            || {
                vec![
                    serde_json::json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}),
                    serde_json::json!({"jsonrpc":"2.0","method":"notifications/resources/list_changed"}),
                ]
            },
            Duration::from_millis(1),
            Duration::from_millis(2),
        )
        .unwrap();
        assert_eq!(end, McpSseEnd::RetireSession);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("notifications/tools/list_changed"));
        assert!(!output.contains("notifications/resources/list_changed"));
        assert!(output.contains(": keepalive"));
    }
}
