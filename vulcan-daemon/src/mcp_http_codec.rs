//! Bounded HTTP/1.1 framing for the foreground and resident MCP listener.
//! This transport codec is independent of CLI command dispatch and OAuth policy.

use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use ulid::Ulid;

pub const MAX_MCP_HTTP_BODY_BYTES: usize = 1024 * 1024;

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
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            message: message.into(),
        }
    }

    fn payload_too_large() -> Self {
        Self {
            status: 413,
            message: format!(
                "request body exceeds maximum size of {MAX_MCP_HTTP_BODY_BYTES} bytes"
            ),
        }
    }
}

pub fn read_mcp_http_request(stream: &mut impl Read) -> Result<McpHttpRequest, McpHttpReadError> {
    let mut buffer = Vec::new();
    let mut header_end = None;

    loop {
        let mut chunk = [0_u8; 1024];
        let bytes_read = stream
            .read(&mut chunk)
            .map_err(|error| McpHttpReadError::bad_request(error.to_string()))?;
        if bytes_read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if let Some(position) = find_bytes(&buffer, b"\r\n\r\n") {
            header_end = Some(position + 4);
            break;
        }
        if buffer.len() > 64 * 1024 {
            return Err(McpHttpReadError::bad_request(
                "request headers exceed 64 KiB",
            ));
        }
    }

    let header_end =
        header_end.ok_or_else(|| McpHttpReadError::bad_request("incomplete HTTP request"))?;
    let header_text = String::from_utf8(buffer[..header_end].to_vec())
        .map_err(|_| McpHttpReadError::bad_request("request headers are not valid UTF-8"))?;
    if header_text
        .split("\r\n")
        .any(|line| line.bytes().any(|byte| matches!(byte, b'\r' | b'\n')))
    {
        return Err(McpHttpReadError::bad_request(
            "request headers require CRLF line endings",
        ));
    }
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| McpHttpReadError::bad_request("missing HTTP request line"))?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| McpHttpReadError::bad_request("missing HTTP method"))?
        .to_string();
    let target = request_parts
        .next()
        .ok_or_else(|| McpHttpReadError::bad_request("missing HTTP request target"))?;
    if request_parts.next() != Some("HTTP/1.1") || request_parts.next().is_some() {
        return Err(McpHttpReadError::bad_request(
            "MCP listener requires an HTTP/1.1 request line",
        ));
    }
    if !target.starts_with('/') || target.contains('#') {
        return Err(McpHttpReadError::bad_request(
            "MCP listener requires an origin-form request target",
        ));
    }
    let (path, query) = target
        .split_once('?')
        .map_or((target, ""), |(path, query)| (path, query));

    if header_end > 64 * 1024 {
        return Err(McpHttpReadError::bad_request(
            "request headers exceed 64 KiB",
        ));
    }
    let (headers, content_length) = parse_request_headers(lines)?;

    let mut body = buffer[header_end..].to_vec();
    if body.len() > content_length {
        return Err(McpHttpReadError::bad_request(
            "request body exceeds declared Content-Length",
        ));
    }
    while body.len() < content_length {
        let mut chunk = [0_u8; 8192];
        let remaining = content_length - body.len();
        let read_length = remaining.min(chunk.len());
        let bytes_read = stream
            .read(&mut chunk[..read_length])
            .map_err(|error| McpHttpReadError::bad_request(error.to_string()))?;
        if bytes_read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..bytes_read]);
    }

    if body.len() < content_length {
        return Err(McpHttpReadError::bad_request(
            "incomplete HTTP request body",
        ));
    }

    Ok(McpHttpRequest {
        method,
        path: path.to_string(),
        query: query.to_string(),
        headers,
        body,
    })
}

fn parse_request_headers<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> Result<(BTreeMap<String, String>, usize), McpHttpReadError> {
    let mut headers = BTreeMap::new();
    for line in lines.take_while(|line| !line.trim().is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| McpHttpReadError::bad_request("malformed HTTP request header"))?;
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        {
            return Err(McpHttpReadError::bad_request(
                "invalid HTTP request header name",
            ));
        }
        if name == "transfer-encoding" {
            return Err(McpHttpReadError::bad_request(
                "Transfer-Encoding is not supported by the MCP HTTP listener",
            ));
        }
        if headers.contains_key(&name)
            && matches!(
                name.as_str(),
                "content-length"
                    | "authorization"
                    | "origin"
                    | "host"
                    | "mcp-session-id"
                    | "mcp-protocol-version"
                    | "x-vulcan-token"
            )
        {
            return Err(McpHttpReadError::bad_request(format!(
                "duplicate HTTP request header: {name}"
            )));
        }
        headers.insert(name, value.trim().to_string());
    }
    let content_length = match headers.get("content-length") {
        Some(value) if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
            value
                .parse::<usize>()
                .map_err(|_| McpHttpReadError::bad_request("invalid Content-Length"))?
        }
        Some(_) => return Err(McpHttpReadError::bad_request("invalid Content-Length")),
        None => 0,
    };
    if content_length > MAX_MCP_HTTP_BODY_BYTES {
        return Err(McpHttpReadError::payload_too_large());
    }
    Ok((headers, content_length))
}

pub fn write_mcp_http_response(
    stream: &mut impl Write,
    response: &McpHttpResponse,
) -> Result<(), io::Error> {
    let status_text = match response.status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        302 => "Found",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let mut headers = format!("HTTP/1.1 {} {}\r\n", response.status, status_text);
    if let Some(content_type) = response.content_type {
        headers.push_str("Content-Type: ");
        headers.push_str(content_type);
        headers.push_str("\r\n");
    }
    for (name, value) in &response.extra_headers {
        if name.chars().any(char::is_control)
            || value
                .chars()
                .any(|character| matches!(character, '\r' | '\n'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid HTTP response header",
            ));
        }
        headers.push_str(name);
        headers.push_str(": ");
        headers.push_str(value);
        headers.push_str("\r\n");
    }
    headers.push_str("Content-Length: ");
    headers.push_str(&response.body.len().to_string());
    headers.push_str("\r\nConnection: close\r\n\r\n");
    stream.write_all(headers.as_bytes())?;
    if !response.body.is_empty() {
        stream.write_all(&response.body)?;
    }
    stream.flush()
}

pub fn write_mcp_http_sse_headers(stream: &mut impl Write) -> Result<(), io::Error> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
    )?;
    stream.flush()
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

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_codec_keeps_query_headers_and_bounded_body() {
        let mut input = io::Cursor::new(
            b"POST /mcp?ticket=one HTTP/1.1\r\nContent-Length: 4\r\nX-Test: yes\r\n\r\ntest",
        );
        let request = read_mcp_http_request(&mut input).expect("request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/mcp");
        assert_eq!(request.query, "ticket=one");
        assert_eq!(request.headers["x-test"], "yes");
        assert_eq!(request.body, b"test");
    }

    #[test]
    fn request_codec_rejects_invalid_request_lines_and_mixed_line_endings() {
        for request in [
            "POST /mcp HTTP/1.0\r\n\r\n",
            "POST /mcp\r\n\r\n",
            "POST /mcp HTTP/1.1 extra\r\n\r\n",
            "POST https://example.test/mcp HTTP/1.1\r\n\r\n",
            "POST /mcp#fragment HTTP/1.1\r\n\r\n",
            "POST /mcp HTTP/1.1\nHost: localhost\r\n\r\n",
            "POST /mcp HTTP/1.1\r\nX-Test: a\rb\r\n\r\n",
        ] {
            assert_eq!(
                read_mcp_http_request(&mut io::Cursor::new(request))
                    .expect_err(request)
                    .status,
                400,
                "{request:?}"
            );
        }
    }

    #[test]
    fn request_codec_refuses_oversized_and_truncated_bodies() {
        let oversized = format!(
            "POST /mcp HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_MCP_HTTP_BODY_BYTES + 1
        );
        assert_eq!(
            read_mcp_http_request(&mut io::Cursor::new(oversized))
                .unwrap_err()
                .status,
            413
        );
        let truncated = b"POST /mcp HTTP/1.1\r\nContent-Length: 4\r\n\r\nno";
        assert_eq!(
            read_mcp_http_request(&mut io::Cursor::new(truncated))
                .unwrap_err()
                .status,
            400
        );
    }

    #[test]
    fn request_codec_rejects_ambiguous_framing_and_auth_headers() {
        for request in [
            "POST /mcp HTTP/1.1\r\nContent-Length: 4\r\ncontent-length: 4\r\n\r\ntest",
            "POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
            "POST /mcp HTTP/1.1\r\nContent-Length: +4\r\n\r\ntest",
            "POST /mcp HTTP/1.1\r\nContent-Length: 0\r\n\r\ntest",
            "POST /mcp HTTP/1.1\r\nAuthorization: Bearer one\r\nauthorization: Bearer two\r\n\r\n",
            "POST /mcp HTTP/1.1\r\nMalformed\r\n\r\n",
        ] {
            let error = read_mcp_http_request(&mut io::Cursor::new(request))
                .expect_err("ambiguous request must fail before dispatch");
            assert_eq!(error.status, 400, "{request}");
        }
    }

    #[test]
    fn response_codec_rejects_header_injection_before_writing() {
        let mut output = Vec::new();
        let response = McpHttpResponse {
            status: 200,
            content_type: Some("application/json"),
            body: b"{}".to_vec(),
            extra_headers: vec![("X-Test".to_string(), "ok\r\nBad: true".to_string())],
        };
        assert_eq!(
            write_mcp_http_response(&mut output, &response)
                .expect_err("header injection")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(output.is_empty());
    }

    #[test]
    fn sse_codec_writes_bounded_protocol_frames() {
        let mut output = Vec::new();
        write_mcp_http_sse_headers(&mut output).expect("headers");
        write_mcp_http_sse_event(&mut output, &serde_json::json!({"method": "ping"}))
            .expect("event");
        write_mcp_http_sse_keepalive(&mut output).expect("keepalive");
        let text = String::from_utf8(output).expect("UTF-8 frames");
        assert!(text.starts_with("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n"));
        let event = text.split("\r\n\r\n").nth(1).expect("event section");
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
