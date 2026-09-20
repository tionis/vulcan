//! Reusable axum adapter for the single-vault cache-backed HTTP API.

use crate::host::{
    HostRuntimeError, RestartPolicy, ServiceDefinition, ServiceId, ServiceRegistration,
    ServiceScope,
};
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::header::{CONTENT_LENGTH, HOST, ORIGIN};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::{Json, Router};
use serde_json::json;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use subtle::ConstantTimeEq;
use vulcan_app::serve::{
    route_request, ServeHealthState, ServeRequest, ServeResponse, ServeRouteOptions,
};
use vulcan_core::{watch_vault_until, VaultPaths, WatchOptions};

pub const VAULT_HTTP_MAX_REQUEST_BYTES: usize = 32 * 1024;
pub const VAULT_HTTP_TOKEN_HEADER: &str = "x-vulcan-token";
pub const DEFAULT_VAULT_HTTP_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct VaultHttpState {
    paths: Arc<VaultPaths>,
    route_options: ServeRouteOptions,
    health: Arc<Mutex<ServeHealthState>>,
    security: VaultHttpSecurity,
    request_deadline: Duration,
}

impl VaultHttpState {
    pub fn new(
        paths: VaultPaths,
        route_options: ServeRouteOptions,
        security: VaultHttpSecurity,
    ) -> Result<Self, VaultHttpConfigurationError> {
        security.validate()?;
        Ok(Self {
            paths: Arc::new(paths),
            route_options,
            health: Arc::new(Mutex::new(ServeHealthState::default())),
            security,
            request_deadline: DEFAULT_VAULT_HTTP_DEADLINE,
        })
    }

    #[must_use]
    pub fn with_request_deadline(mut self, deadline: Duration) -> Self {
        self.request_deadline = deadline;
        self
    }

    #[must_use]
    pub fn health_handle(&self) -> Arc<Mutex<ServeHealthState>> {
        Arc::clone(&self.health)
    }
}

#[derive(Clone)]
pub struct VaultHttpSecurity {
    token: Arc<str>,
    allowed_authorities: Arc<[String]>,
    allowed_origins: Arc<[String]>,
}

impl VaultHttpSecurity {
    #[must_use]
    pub fn new(
        token: impl Into<String>,
        allowed_authorities: Vec<String>,
        allowed_origins: Vec<String>,
    ) -> Self {
        Self {
            token: Arc::from(token.into()),
            allowed_authorities: allowed_authorities.into(),
            allowed_origins: allowed_origins.into(),
        }
    }

    fn validate(&self) -> Result<(), VaultHttpConfigurationError> {
        if self.token.is_empty() {
            return Err(VaultHttpConfigurationError::EmptyToken);
        }
        if self.allowed_authorities.is_empty()
            || self.allowed_authorities.iter().any(String::is_empty)
        {
            return Err(VaultHttpConfigurationError::InvalidAuthorities);
        }
        if self.allowed_origins.iter().any(String::is_empty) {
            return Err(VaultHttpConfigurationError::InvalidOrigins);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultHttpConfigurationError {
    EmptyToken,
    InvalidAuthorities,
    InvalidOrigins,
}

impl std::fmt::Display for VaultHttpConfigurationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyToken => formatter.write_str("vault HTTP token must not be empty"),
            Self::InvalidAuthorities => {
                formatter.write_str("vault HTTP requires at least one non-empty Host authority")
            }
            Self::InvalidOrigins => {
                formatter.write_str("vault HTTP allowed origins must not contain empty values")
            }
        }
    }
}

impl std::error::Error for VaultHttpConfigurationError {}

pub fn vault_router(state: VaultHttpState) -> Router {
    Router::new()
        .fallback(any(dispatch))
        .layer(DefaultBodyLimit::max(VAULT_HTTP_MAX_REQUEST_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .with_state(state)
}

pub async fn serve_vault_with_shutdown<F>(
    listener: tokio::net::TcpListener,
    state: VaultHttpState,
    shutdown: F,
) -> Result<(), std::io::Error>
where
    F: Future<Output = ()> + Send + 'static,
{
    axum::serve(listener, vault_router(state))
        .with_graceful_shutdown(shutdown)
        .await
}

/// Adapts an already-bound listener to the shared host lifecycle. The
/// listener is single-use and therefore intentionally non-restarting.
pub fn vault_listener_service(
    listener: tokio::net::TcpListener,
    state: VaultHttpState,
    runtime: tokio::runtime::Handle,
    dependencies: Vec<ServiceId>,
) -> Result<ServiceRegistration, HostRuntimeError> {
    let id = ServiceId::parse("listener.vault-http")?;
    let listener = Arc::new(Mutex::new(Some(listener)));
    Ok(ServiceRegistration::new(
        ServiceDefinition {
            id,
            service_kind: "listener".to_string(),
            scope: ServiceScope::Instance {
                instance_id: "temporary-vault-http".to_string(),
            },
            enabled: true,
            required: true,
            dependencies,
            restart: RestartPolicy::Never,
        },
        move |service| {
            let listener = listener
                .lock()
                .map_err(|_| "vault HTTP listener state is unavailable".to_string())?
                .take()
                .ok_or_else(|| "vault HTTP listener was already consumed".to_string())?;
            service.ready()?;
            let stop = Arc::clone(service.stop());
            runtime
                .block_on(serve_vault_with_shutdown(
                    listener,
                    state.clone(),
                    async move { stop.cancelled().await },
                ))
                .map_err(|error| format!("vault HTTP listener failed: {error}"))
        },
    ))
}

/// Wraps the legacy single-vault watcher in the shared service lifecycle until
/// the temporary host consumes the common observation fanout directly.
pub fn vault_watch_service(
    paths: VaultPaths,
    health: Arc<Mutex<ServeHealthState>>,
    options: WatchOptions,
) -> Result<ServiceRegistration, HostRuntimeError> {
    let id = ServiceId::parse("observation.vault/temporary-http")?;
    Ok(ServiceRegistration::new(
        ServiceDefinition {
            id,
            service_kind: "observation".to_string(),
            scope: ServiceScope::Instance {
                instance_id: "temporary-vault-http".to_string(),
            },
            enabled: true,
            required: true,
            dependencies: Vec::new(),
            restart: RestartPolicy::Never,
        },
        move |service| {
            service.ready()?;
            let result = watch_vault_until(
                &paths,
                &options,
                || service.stop().is_cancelled(),
                |report| {
                    let mut state = health
                        .lock()
                        .map_err(|_| "vault HTTP health state is unavailable".to_string())?;
                    state.last_watch_report = Some(report);
                    state.watch_error = None;
                    Ok::<_, String>(())
                },
            );
            if let Err(error) = &result {
                if let Ok(mut state) = health.lock() {
                    state.watch_error = Some(error.to_string());
                }
            }
            result.map_err(|error| format!("vault observation failed: {error}"))
        },
    ))
}

async fn authorize(State(state): State<VaultHttpState>, request: Request, next: Next) -> Response {
    let oversized = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > VAULT_HTTP_MAX_REQUEST_BYTES);
    if oversized {
        return json_error(StatusCode::PAYLOAD_TOO_LARGE, "request body exceeds 32 KiB");
    }
    let host_allowed = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| {
            state
                .security
                .allowed_authorities
                .iter()
                .any(|allowed| allowed == host)
        });
    if !host_allowed {
        return json_error(StatusCode::FORBIDDEN, "forbidden Host header");
    }
    let origin_allowed = request
        .headers()
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|origin| {
            state
                .security
                .allowed_origins
                .iter()
                .any(|allowed| allowed == origin)
        });
    if !origin_allowed {
        return json_error(StatusCode::FORBIDDEN, "forbidden Origin header");
    }
    let authorized = request
        .headers()
        .get(VAULT_HTTP_TOKEN_HEADER)
        .map(HeaderValue::as_bytes)
        .is_some_and(|actual| constant_time_equal(actual, state.security.token.as_bytes()));
    if !authorized {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "missing or invalid X-Vulcan-Token header",
        );
    }
    next.run(request).await
}

async fn dispatch(State(state): State<VaultHttpState>, request: Request<Body>) -> Response {
    let app_request = ServeRequest {
        method: request.method().to_string(),
        path: request.uri().path().to_string(),
        query: parse_query(request.uri().query().unwrap_or_default()),
    };
    let paths = Arc::clone(&state.paths);
    let options = state.route_options.clone();
    let health = state.health.lock().map_or_else(
        |_| ServeHealthState {
            watch_error: Some("vault HTTP health state is unavailable".to_string()),
            last_watch_report: None,
        },
        |health| health.clone(),
    );
    let operation = tokio::task::spawn_blocking(move || {
        route_request(paths.as_ref(), &options, &health, &app_request)
    });
    match tokio::time::timeout(state.request_deadline, operation).await {
        Ok(Ok(response)) => app_response(response),
        Ok(Err(error)) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("vault HTTP operation task failed: {error}"),
        ),
        Err(_) => json_error(
            StatusCode::GATEWAY_TIMEOUT,
            "vault HTTP request deadline exceeded",
        ),
    }
}

fn parse_query(query: &str) -> HashMap<String, Vec<String>> {
    let mut parameters = HashMap::<String, Vec<String>>::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        parameters
            .entry(key.into_owned())
            .or_default()
            .push(value.into_owned());
    }
    parameters
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

fn app_response(response: ServeResponse) -> Response {
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(response.body)).into_response()
}

fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "ok": false,
            "error": message.into(),
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request as HttpRequest;
    use serde_json::Value;
    use tower::ServiceExt;
    use vulcan_core::{scan_vault, ScanMode};

    fn fixture() -> (tempfile::TempDir, VaultHttpState) {
        let vault = tempfile::tempdir().expect("vault");
        std::fs::create_dir(vault.path().join(".vulcan")).expect("config directory");
        std::fs::write(vault.path().join("Home.md"), "# Home\n\nneedle\n").expect("note");
        let paths = VaultPaths::new(vault.path());
        scan_vault(&paths, ScanMode::Full).expect("scan");
        let state = VaultHttpState::new(
            paths,
            ServeRouteOptions {
                permissions: None,
                watch_enabled: false,
            },
            VaultHttpSecurity::new(
                "secret",
                vec!["127.0.0.1:3210".to_string(), "localhost:3210".to_string()],
                vec!["http://127.0.0.1:3210".to_string()],
            ),
        )
        .expect("state");
        (vault, state)
    }

    async fn body(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[tokio::test]
    async fn router_preserves_single_vault_response_shape_and_repeated_queries() {
        let (_vault, state) = fixture();
        let router = vault_router(state);
        for path in ["/search?q=needle", "/graph/stats"] {
            let response = router
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .uri(path)
                        .header(HOST, "127.0.0.1:3210")
                        .header(VAULT_HTTP_TOKEN_HEADER, "secret")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(body(response).await["ok"], true);
        }
    }

    #[tokio::test]
    async fn router_keeps_authority_origin_and_token_boundaries_separate() {
        let (_vault, state) = fixture();
        let router = vault_router(state);
        for (host, origin, token, expected) in [
            ("evil.example", None, "secret", StatusCode::FORBIDDEN),
            (
                "127.0.0.1:3210",
                Some("https://evil.example"),
                "secret",
                StatusCode::FORBIDDEN,
            ),
            ("127.0.0.1:3210", None, "wrong", StatusCode::UNAUTHORIZED),
        ] {
            let mut builder = HttpRequest::builder()
                .uri("/health")
                .header(HOST, host)
                .header(VAULT_HTTP_TOKEN_HEADER, token);
            if let Some(origin) = origin {
                builder = builder.header(ORIGIN, origin);
            }
            let response = router
                .clone()
                .oneshot(builder.body(Body::empty()).expect("request"))
                .await
                .expect("response");
            assert_eq!(response.status(), expected);
            assert_eq!(body(response).await["ok"], false);
        }
    }

    #[tokio::test]
    async fn router_rejects_unsupported_methods_with_the_legacy_json_contract() {
        let (_vault, state) = fixture();
        let response = vault_router(state)
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/search")
                    .header(HOST, "127.0.0.1:3210")
                    .header(VAULT_HTTP_TOKEN_HEADER, "secret")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            body(response).await["error"],
            "only GET requests are supported"
        );
    }

    #[tokio::test]
    async fn router_rejects_declared_oversized_requests_before_dispatch() {
        let (_vault, state) = fixture();
        let response = vault_router(state)
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/search")
                    .header(HOST, "127.0.0.1:3210")
                    .header(VAULT_HTTP_TOKEN_HEADER, "secret")
                    .header(CONTENT_LENGTH, VAULT_HTTP_MAX_REQUEST_BYTES + 1)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body(response).await["ok"], false);
    }
}
