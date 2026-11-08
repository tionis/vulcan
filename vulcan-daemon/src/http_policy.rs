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

/// Where an MCP OAuth client asks for its authorization code to be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpOAuthRedirectTarget {
    /// An HTTPS site, such as a hosted client's callback.
    Web { host: String, origin: String },
    /// A listener on the user's own machine (RFC 8252 loopback redirect),
    /// as desktop and command-line clients use.
    Loopback { origin: String },
    /// An app registered for a private-use URI scheme (RFC 8252 §7.1).
    App { scheme: String },
}

impl McpOAuthRedirectTarget {
    /// A CSP source expression that allows navigating to this target, for
    /// the consent form's `form-action`.
    #[must_use]
    pub fn csp_source(&self) -> String {
        match self {
            Self::Web { origin, .. } | Self::Loopback { origin } => origin.clone(),
            Self::App { scheme } => format!("{scheme}:"),
        }
    }

    /// How the consent page names the destination.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Web { host, .. } => host.clone(),
            Self::Loopback { origin } => format!("this computer ({origin})"),
            Self::App { scheme } => format!("the app registered for {scheme}: links"),
        }
    }
}

/// Schemes that must never receive an authorization code: they run code,
/// address local files, or do not navigate to a client.
const FORBIDDEN_REDIRECT_SCHEMES: &[&str] = &[
    "about",
    "blob",
    "data",
    "file",
    "filesystem",
    "ftp",
    "javascript",
    "mailto",
    "sms",
    "tel",
    "vbscript",
    "ws",
    "wss",
];

/// Classifies an MCP OAuth redirect URI: any HTTPS URL, `http` only to a
/// loopback host, or a private-use app scheme. Fragments, embedded
/// credentials, whitespace, control characters and backslashes are refused.
#[must_use]
pub fn mcp_oauth_redirect_target(redirect_uri: &str) -> Option<McpOAuthRedirectTarget> {
    if redirect_uri.len() > 2048
        || redirect_uri.chars().any(|character| {
            character.is_control() || character.is_whitespace() || character == '\\'
        })
    {
        return None;
    }
    let parsed = Url::parse(redirect_uri).ok()?;
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.fragment().is_some() {
        return None;
    }
    let scheme = parsed.scheme();
    let loopback = match parsed.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    match scheme {
        "https" | "http" => {
            let host = parsed.host_str()?.to_ascii_lowercase();
            let origin = parsed.origin().ascii_serialization();
            if loopback {
                Some(McpOAuthRedirectTarget::Loopback { origin })
            } else if scheme == "https" {
                Some(McpOAuthRedirectTarget::Web { host, origin })
            } else {
                None
            }
        }
        // A one-letter scheme is a Windows drive path, not an app.
        _ if scheme.len() < 2 || FORBIDDEN_REDIRECT_SCHEMES.contains(&scheme) => None,
        _ => Some(McpOAuthRedirectTarget::App {
            scheme: scheme.to_string(),
        }),
    }
}

#[must_use]
pub fn mcp_oauth_redirect_uri_valid(redirect_uri: &str) -> bool {
    mcp_oauth_redirect_target(redirect_uri).is_some()
}

/// Whether a client may use `redirect_uri`. With no `allowed_hosts` every
/// valid redirect is allowed; consent shows where the code goes. A list
/// restricts redirects to HTTPS sites on those hosts or their subdomains.
#[must_use]
pub fn mcp_oauth_redirect_uri_allowed(redirect_uri: &str, allowed_hosts: &[String]) -> bool {
    let Some(target) = mcp_oauth_redirect_target(redirect_uri) else {
        return false;
    };
    if allowed_hosts.is_empty() {
        return true;
    }
    let McpOAuthRedirectTarget::Web { host, .. } = target else {
        return false;
    };
    allowed_hosts.iter().any(|allowed| {
        let allowed = allowed.to_ascii_lowercase();
        host == allowed || host.ends_with(&format!(".{allowed}"))
    })
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
    fn mcp_oauth_redirects_are_open_by_default_and_a_host_list_restricts_them() {
        for uri in [
            "https://chatgpt.com/callback",
            "https://claude.ai/api/mcp/auth_callback",
            "https://n8n.example.org/rest/oauth2-credential/callback",
            "https://192.0.2.10/callback",
            "http://127.0.0.1:33418/callback",
            "http://localhost:6274/oauth/callback",
            "http://[::1]:8080/cb",
            "cursor://anysphere.cursor-retrieval/oauth/callback",
            "com.example.app:/oauth2redirect",
        ] {
            assert!(mcp_oauth_redirect_uri_valid(uri), "{uri}");
            assert!(mcp_oauth_redirect_uri_allowed(uri, &[]), "{uri}");
        }
        for uri in [
            "http://chatgpt.com/callback",
            "http://192.0.2.10/callback",
            "https://chatgpt.com:bad/callback",
            "https://chatgpt.com/callback#fragment",
            "https://user@chatgpt.com/callback",
            "https://chatgpt.com\\@attacker.example/callback",
            "https://chatgpt.com/callback\r\nX-Injected: yes",
            "javascript:alert(1)",
            "data:text/html,hi",
            "file:///etc/passwd",
            "c:/callback",
            "not a url",
        ] {
            assert!(!mcp_oauth_redirect_uri_valid(uri), "{uri}");
            assert!(!mcp_oauth_redirect_uri_allowed(uri, &[]), "{uri}");
        }

        let allowed = vec!["chatgpt.com".to_string()];
        for uri in [
            "https://chatgpt.com/callback",
            "https://sub.chatgpt.com/callback?source=vulcan",
        ] {
            assert!(mcp_oauth_redirect_uri_allowed(uri, &allowed), "{uri}");
        }
        for uri in [
            "https://chatgpt.com.evil.example/callback",
            "https://claude.ai/api/mcp/auth_callback",
            "http://127.0.0.1:33418/callback",
            "cursor://anysphere.cursor-retrieval/oauth/callback",
        ] {
            assert!(!mcp_oauth_redirect_uri_allowed(uri, &allowed), "{uri}");
        }
    }

    #[test]
    fn redirect_targets_describe_themselves_and_give_csp_sources() {
        let web = mcp_oauth_redirect_target("https://Claude.ai/api/mcp/auth_callback").unwrap();
        assert_eq!(web.describe(), "claude.ai");
        assert_eq!(web.csp_source(), "https://claude.ai");
        let local = mcp_oauth_redirect_target("http://127.0.0.1:33418/callback").unwrap();
        assert_eq!(local.csp_source(), "http://127.0.0.1:33418");
        assert!(local.describe().starts_with("this computer"));
        let app = mcp_oauth_redirect_target("cursor://anysphere/cb").unwrap();
        assert_eq!(app.csp_source(), "cursor:");
        assert!(app.describe().contains("cursor:"));
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
