//! Request and response values for the MCP HTTP listener, the checks every MCP
//! POST and GET passes, and server-sent event frames. hyper does the framing
//! (`mcp_transport`); this module is independent of dispatch and OAuth policy.

use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{self, Write};
use ulid::Ulid;

pub const MAX_MCP_HTTP_BODY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct McpHttpRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct McpHttpResponse {
    pub status: u16,
    pub content_type: Option<&'static str>,
    pub body: Vec<u8>,
    pub extra_headers: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct McpHttpReadError {
    pub status: u16,
    pub message: String,
}

impl McpHttpReadError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn payload_too_large() -> Self {
        Self {
            status: 413,
            message: format!(
                "request body exceeds maximum size of {MAX_MCP_HTTP_BODY_BYTES} bytes"
            ),
        }
    }
}

/// Validate the HTTP envelope and decode one MCP JSON-RPC POST body.
/// Authentication and OAuth-scope checks remain separate host policy.
pub fn parse_mcp_http_post(request: &McpHttpRequest) -> Result<Value, McpHttpReadError> {
    if !request
        .headers
        .get("content-type")
        .is_some_and(|value| value.contains("application/json"))
    {
        return Err(McpHttpReadError::bad_request(
            "MCP POST requests require Content-Type: application/json",
        ));
    }
    if !request.headers.get("accept").is_some_and(|value| {
        value.contains("application/json") && value.contains("text/event-stream")
    }) {
        return Err(McpHttpReadError::bad_request(
            "MCP POST requests require Accept: application/json, text/event-stream",
        ));
    }
    serde_json::from_slice(&request.body)
        .map_err(|error| McpHttpReadError::bad_request(format!("Parse error: {error}")))
}

pub fn validate_mcp_protocol_version(request: &McpHttpRequest) -> Result<(), McpHttpReadError> {
    let Some(version) = request.headers.get("mcp-protocol-version") else {
        return Ok(());
    };
    if vulcan_app::mcp_core::is_supported_version(version) {
        Ok(())
    } else {
        Err(McpHttpReadError::bad_request(format!(
            "unsupported MCP-Protocol-Version `{version}`"
        )))
    }
}

pub fn validate_mcp_sse_accept(request: &McpHttpRequest) -> Result<(), McpHttpReadError> {
    if request
        .headers
        .get("accept")
        .is_some_and(|value| value.contains("text/event-stream"))
    {
        Ok(())
    } else {
        Err(McpHttpReadError {
            status: 405,
            message: "MCP GET requests require Accept: text/event-stream".to_string(),
        })
    }
}

pub fn write_mcp_http_sse_event(stream: &mut impl Write, message: &Value) -> Result<(), io::Error> {
    let payload = serde_json::to_string(message).expect("sse payload should serialize");
    let event_id = Ulid::new().to_string();
    let frame = format!("id: {event_id}\nevent: message\ndata: {payload}\n\n");
    stream.write_all(frame.as_bytes())?;
    stream.flush()
}

pub fn write_mcp_http_sse_keepalive(stream: &mut impl Write) -> Result<(), io::Error> {
    stream.write_all(b": keepalive\n\n")?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_and_sse_preflight_preserve_http_error_contract() {
        let mut request = McpHttpRequest {
            method: "POST".to_string(),
            path: "/mcp".to_string(),
            query: String::new(),
            headers: BTreeMap::new(),
            body: br#"{"method":"tools/list"}"#.to_vec(),
        };
        let error = parse_mcp_http_post(&request).unwrap_err();
        assert_eq!(error.status, 400);
        assert_eq!(
            error.message,
            "MCP POST requests require Content-Type: application/json"
        );
        request
            .headers
            .insert("content-type".into(), "application/json".into());
        assert!(parse_mcp_http_post(&request)
            .unwrap_err()
            .message
            .contains("require Accept"));
        request.headers.insert(
            "accept".into(),
            "application/json, text/event-stream".into(),
        );
        assert_eq!(
            parse_mcp_http_post(&request).unwrap()["method"],
            "tools/list"
        );
        request.body = b"{".to_vec();
        assert!(parse_mcp_http_post(&request)
            .unwrap_err()
            .message
            .starts_with("Parse error:"));
        assert!(validate_mcp_protocol_version(&request).is_ok());
        request
            .headers
            .insert("mcp-protocol-version".into(), "other".into());
        assert!(validate_mcp_protocol_version(&request)
            .unwrap_err()
            .message
            .contains("unsupported MCP-Protocol-Version"));
        request
            .headers
            .insert("accept".into(), "application/json".into());
        assert_eq!(validate_mcp_sse_accept(&request).unwrap_err().status, 405);
        request
            .headers
            .insert("accept".into(), "text/event-stream".into());
        assert!(validate_mcp_sse_accept(&request).is_ok());
    }

    #[test]
    fn sse_codec_writes_bounded_protocol_frames() {
        let mut output = Vec::new();
        write_mcp_http_sse_event(&mut output, &serde_json::json!({"method": "ping"}))
            .expect("event");
        write_mcp_http_sse_keepalive(&mut output).expect("keepalive");
        let text = String::from_utf8(output).expect("UTF-8 frames");
        let event = text.as_str();
        let id = event
            .lines()
            .next()
            .expect("event ID")
            .strip_prefix("id: ")
            .expect("ID prefix");
        assert!(id.parse::<Ulid>().is_ok());
        assert!(event.contains("event: message\ndata: {\"method\":\"ping\"}\n\n"));
        assert!(text.ends_with(": keepalive\n\n"));
    }
}
