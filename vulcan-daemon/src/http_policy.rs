//! Shared HTTP boundary policy primitives.
//!
//! Authentication remains surface-specific: these helpers deliberately do
//! not turn a companion credential, vault token, or MCP grant into a common
//! authority. They only centralize transport mechanics that must behave the
//! same at every listener boundary.

use axum::http::header::{AUTHORIZATION, CONTENT_LENGTH, ORIGIN};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::Response;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use url::{Host, Url};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderText<'a> {
    Absent,
    Valid(&'a str),
    Invalid,
}

#[must_use]
pub fn header_text<'a>(headers: &'a HeaderMap, name: &HeaderName) -> HeaderText<'a> {
    match headers.get(name) {
        None => HeaderText::Absent,
        Some(value) => value
            .to_str()
            .map_or(HeaderText::Invalid, HeaderText::Valid),
    }
}

#[must_use]
pub fn declared_body_exceeds(headers: &HeaderMap, maximum: usize) -> bool {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > maximum)
}

#[must_use]
pub fn exact_origin_allowed(headers: &HeaderMap, allowed: &[String]) -> bool {
    match header_text(headers, &ORIGIN) {
        HeaderText::Absent => true,
        HeaderText::Valid(origin) => allowed.iter().any(|candidate| candidate == origin),
        HeaderText::Invalid => false,
    }
}

#[must_use]
pub fn constant_time_secret_header(
    headers: &HeaderMap,
    name: &HeaderName,
    expected: &[u8],
) -> bool {
    headers.get(name).is_some_and(|actual| {
        actual.as_bytes().len() == expected.len() && bool::from(actual.as_bytes().ct_eq(expected))
    })
}

#[must_use]
pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

/// Accept only a syntactically valid browser Origin whose host is local to the
/// listener. The port may differ (for a local harness), but paths, credentials,
/// and opaque origins are never valid Origins for this boundary.
#[must_use]
pub fn mcp_origin_allowed(origin: &str, bind_addr: SocketAddr) -> bool {
    let Ok(parsed) = Url::parse(origin) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return false;
    }
    match parsed.host() {
        Some(Host::Domain("localhost")) => bind_addr.ip().is_loopback(),
        Some(Host::Ipv4(ip)) if bind_addr.ip().is_loopback() => ip == Ipv4Addr::LOCALHOST,
        Some(Host::Ipv6(ip)) if bind_addr.ip().is_loopback() => ip == Ipv6Addr::LOCALHOST,
        Some(Host::Ipv4(ip)) => bind_addr.ip() == IpAddr::V4(ip),
        Some(Host::Ipv6(ip)) => bind_addr.ip() == IpAddr::V6(ip),
        _ => false,
    }
}

/// OAuth redirect targets for the MCP issuer must be complete HTTPS URLs with
/// an unambiguous authority. Fragments cannot receive an authorization code.
#[must_use]
pub fn mcp_oauth_redirect_uri_valid(redirect_uri: &str) -> bool {
    mcp_oauth_redirect_host(redirect_uri).is_some()
}

#[must_use]
pub fn mcp_oauth_redirect_uri_allowed(redirect_uri: &str, allowed_hosts: &[String]) -> bool {
    let Some(host) = mcp_oauth_redirect_host(redirect_uri) else {
        return false;
    };
    allowed_hosts
        .iter()
        .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
}

fn mcp_oauth_redirect_host(redirect_uri: &str) -> Option<String> {
    if redirect_uri
        .chars()
        .any(|character| character.is_control() || character.is_whitespace() || character == '\\')
    {
        return None;
    }
    let parsed = Url::parse(redirect_uri).ok()?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    // Preserve the domain-only redirect contract; numeric IP targets have no
    // place in the hosted-client allowlist.
    let Host::Domain(host) = parsed.host()? else {
        return None;
    };
    Some(host.to_string())
}

pub fn apply_cors_headers(
    response: &mut Response,
    origin: Option<&str>,
    allow_headers: &'static str,
    allow_methods: &'static str,
) {
    let Some(origin) = origin.and_then(|origin| HeaderValue::from_str(origin).ok()) else {
        return;
    };
    response
        .headers_mut()
        .insert(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    response.headers_mut().insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(allow_headers),
    );
    response.headers_mut().insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(allow_methods),
    );
}

pub async fn with_deadline<F, T>(deadline: Duration, future: F) -> Result<T, DeadlineExceeded>
where
    F: Future<Output = T>,
{
    tokio::time::timeout(deadline, future)
        .await
        .map_err(|_| DeadlineExceeded)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeadlineExceeded;

/// Secret-minimal access record. Query strings and all headers are excluded.
pub struct RequestAudit {
    surface: &'static str,
    method: Method,
    path: String,
    started: Instant,
}

impl RequestAudit {
    #[must_use]
    pub fn capture(surface: &'static str, method: &Method, uri: &Uri) -> Self {
        Self {
            surface,
            method: method.clone(),
            path: uri.path().to_string(),
            started: Instant::now(),
        }
    }

    #[must_use]
    pub fn finish(&self, status: StatusCode) -> HttpAccessRecord<'_> {
        HttpAccessRecord {
            surface: self.surface,
            method: &self.method,
            path: &self.path,
            status,
            elapsed: self.started.elapsed(),
        }
    }

    pub fn emit(&self, status: StatusCode) {
        if status.is_client_error() || status.is_server_error() {
            eprintln!("{}", self.finish(status));
        }
    }
}

pub struct HttpAccessRecord<'a> {
    surface: &'static str,
    method: &'a Method,
    path: &'a str,
    status: StatusCode,
    elapsed: Duration,
}

impl Display for HttpAccessRecord<'_> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        let level = if self.status.is_server_error() {
            "error"
        } else if self.status.is_client_error() {
            "warning"
        } else {
            "info"
        };
        write!(
            formatter,
            "level={level} event=http_request surface={} method={} path={} status={} elapsed_ms={}",
            self.surface,
            self.method,
            self.path,
            self.status.as_u16(),
            self.elapsed.as_millis()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    #[test]
    fn mcp_oauth_redirects_require_valid_https_domain_urls_without_fragments() {
        let allowed = vec!["chatgpt.com".to_string()];
        for uri in [
            "https://chatgpt.com/callback",
            "https://sub.chatgpt.com/callback?source=vulcan",
        ] {
            assert!(mcp_oauth_redirect_uri_valid(uri), "{uri}");
            assert!(mcp_oauth_redirect_uri_allowed(uri, &allowed), "{uri}");
        }
        for uri in [
            "http://chatgpt.com/callback",
            "https://chatgpt.com:bad/callback",
            "https://chatgpt.com/callback#fragment",
            "https://user@chatgpt.com/callback",
            "https://chatgpt.com\\@attacker.example/callback",
            "https://chatgpt.com.evil.example/callback",
            "https://127.0.0.1/callback",
        ] {
            assert!(!mcp_oauth_redirect_uri_allowed(uri, &allowed), "{uri}");
        }
        assert!(!mcp_oauth_redirect_uri_valid(
            "https://chatgpt.com:bad/callback"
        ));
    }

    #[test]
    fn exact_origin_and_secret_helpers_fail_closed() {
        let request = Request::builder()
            .header(ORIGIN, "https://allowed.example")
            .header("x-secret", "correct")
            .body(())
            .unwrap();
        assert!(exact_origin_allowed(
            request.headers(),
            &["https://allowed.example".to_string()]
        ));
        assert!(constant_time_secret_header(
            request.headers(),
            &HeaderName::from_static("x-secret"),
            b"correct"
        ));
        assert!(!constant_time_secret_header(
            request.headers(),
            &HeaderName::from_static("x-secret"),
            b"wrong"
        ));
    }

    #[test]
    fn mcp_origin_check_handles_ipv6_and_rejects_malformed_origins() {
        let bind = SocketAddr::from(([127, 0, 0, 1], 8765));
        for origin in [
            "http://localhost:3000",
            "https://127.0.0.1",
            "http://[::1]:3000",
        ] {
            assert!(mcp_origin_allowed(origin, bind), "{origin}");
        }
        for origin in [
            "null",
            "localhost",
            "http://localhost.evil",
            "http://user@localhost",
            "http://localhost/path",
            "http://localhost?query=1",
            "http://localhost#fragment",
            "javascript://localhost",
        ] {
            assert!(!mcp_origin_allowed(origin, bind), "{origin}");
        }
        let remote = SocketAddr::from(([192, 0, 2, 10], 8765));
        assert!(mcp_origin_allowed("https://192.0.2.10", remote));
        assert!(!mcp_origin_allowed("https://localhost", remote));
        assert!(!mcp_origin_allowed("https://192.0.2.11", remote));
    }

    #[test]
    fn access_records_exclude_queries_and_headers() {
        let request = Request::builder()
            .uri("/search?q=secret-value")
            .header(AUTHORIZATION, "Bearer more-secret-value")
            .body(())
            .unwrap();
        let audit = RequestAudit::capture("test", request.method(), request.uri());
        let rendered = audit.finish(StatusCode::UNAUTHORIZED).to_string();
        assert!(rendered.starts_with("level=warning"));
        assert!(rendered.contains("path=/search"));
        assert!(!rendered.contains("secret-value"));
        assert!(!rendered.contains("authorization"));
    }

    #[tokio::test]
    async fn deadline_helper_bounds_async_work() {
        let result = with_deadline(Duration::from_millis(1), async {
            tokio::time::sleep(Duration::from_secs(1)).await;
        })
        .await;
        assert_eq!(result, Err(DeadlineExceeded));
    }
}
