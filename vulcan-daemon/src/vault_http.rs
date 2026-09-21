//! Reusable axum adapter for the single-vault cache-backed HTTP API.

use crate::host::{
    HostRuntimeError, RestartPolicy, ServiceDefinition, ServiceId, ServiceRegistration,
    ServiceScope,
};
use crate::http_policy::{
    apply_cors_headers, constant_time_secret_header, declared_body_exceeds, exact_origin_allowed,
    header_text, with_deadline, HeaderText, RequestAudit,
};
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::header::{HOST, ORIGIN};
use axum::http::{HeaderName, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::{Json, Router};
use serde_json::json;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vulcan_app::serve::{
    route_request, serve_route_paths, ServeHealthState, ServeRequest, ServeResponse,
    ServeRouteOptions,
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
    let mut router = Router::new();
    for path in serve_route_paths() {
        router = router.route(path, any(dispatch));
    }
    router
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultHttpServiceIdentity {
    id: ServiceId,
    scope: ServiceScope,
}

impl VaultHttpServiceIdentity {
    pub fn temporary() -> Result<Self, HostRuntimeError> {
        Ok(Self {
            id: ServiceId::parse("listener.vault-http")?,
            scope: ServiceScope::Instance {
                instance_id: "temporary-vault-http".to_string(),
            },
        })
    }

    pub fn resident(registration_id: &str) -> Result<Self, HostRuntimeError> {
        Ok(Self {
            id: ServiceId::parse(format!("listener.vault-http/{registration_id}"))?,
            scope: ServiceScope::Vault {
                registration_id: registration_id.to_string(),
            },
        })
    }
}

/// Adapts an already-bound listener to the shared host lifecycle. The
/// listener is single-use and therefore intentionally non-restarting.
pub fn vault_listener_service(
    listener: tokio::net::TcpListener,
    state: VaultHttpState,
    runtime: tokio::runtime::Handle,
    dependencies: Vec<ServiceId>,
) -> Result<ServiceRegistration, HostRuntimeError> {
    vault_listener_service_for(
        listener,
        state,
        runtime,
        dependencies,
        VaultHttpServiceIdentity::temporary()?,
    )
}

pub fn vault_listener_service_for(
    listener: tokio::net::TcpListener,
    state: VaultHttpState,
    runtime: tokio::runtime::Handle,
    dependencies: Vec<ServiceId>,
    identity: VaultHttpServiceIdentity,
) -> Result<ServiceRegistration, HostRuntimeError> {
    let listener = Arc::new(Mutex::new(Some(listener)));
    Ok(ServiceRegistration::new(
        ServiceDefinition {
            id: identity.id,
            service_kind: "listener".to_string(),
            scope: identity.scope,
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
            let mut ready = false;
            let result = watch_vault_until(
                &paths,
                &options,
                || service.stop().is_cancelled(),
                |report| {
                    if !ready {
                        service.ready()?;
                        ready = true;
                    }
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
    let audit = RequestAudit::capture("vault", request.method(), request.uri());
    let origin = match header_text(request.headers(), &ORIGIN) {
        HeaderText::Valid(origin) => Some(origin.to_string()),
        HeaderText::Absent | HeaderText::Invalid => None,
    };
    let response = authorize_inner(&state, request, next).await;
    let response = vault_cors_response(response, origin.as_deref());
    audit.emit(response.status());
    response
}

async fn authorize_inner(state: &VaultHttpState, request: Request, next: Next) -> Response {
    if declared_body_exceeds(request.headers(), VAULT_HTTP_MAX_REQUEST_BYTES) {
        return json_error(StatusCode::PAYLOAD_TOO_LARGE, "request body exceeds 32 KiB");
    }
    let host_allowed = matches!(
        header_text(request.headers(), &HOST),
        HeaderText::Valid(host)
            if state.security.allowed_authorities.iter().any(|allowed| allowed == host)
    );
    if !host_allowed {
        return json_error(StatusCode::FORBIDDEN, "forbidden Host header");
    }
    if !exact_origin_allowed(request.headers(), &state.security.allowed_origins) {
        return json_error(StatusCode::FORBIDDEN, "forbidden Origin header");
    }
    if request.method() == Method::OPTIONS {
        return StatusCode::NO_CONTENT.into_response();
    }
    let token_header = HeaderName::from_static(VAULT_HTTP_TOKEN_HEADER);
    if !constant_time_secret_header(
        request.headers(),
        &token_header,
        state.security.token.as_bytes(),
    ) {
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
    match with_deadline(state.request_deadline, operation).await {
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

fn vault_cors_response(mut response: Response, origin: Option<&str>) -> Response {
    apply_cors_headers(
        &mut response,
        origin,
        "content-type, x-vulcan-token",
        "GET, OPTIONS",
    );
    response
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
    use axum::http::header::CONTENT_LENGTH;
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

    async fn body_bytes(response: Response) -> axum::body::Bytes {
        axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body")
    }

    #[test]
    fn listener_identity_distinguishes_temporary_and_resident_ownership() {
        let temporary = VaultHttpServiceIdentity::temporary().expect("temporary identity");
        assert_eq!(temporary.id.as_str(), "listener.vault-http");
        assert_eq!(
            temporary.scope,
            ServiceScope::Instance {
                instance_id: "temporary-vault-http".to_string()
            }
        );

        let resident = VaultHttpServiceIdentity::resident("notes").expect("resident identity");
        assert_eq!(resident.id.as_str(), "listener.vault-http/notes");
        assert_eq!(
            resident.scope,
            ServiceScope::Vault {
                registration_id: "notes".to_string()
            }
        );
    }

    #[tokio::test]
    async fn temporary_and_resident_mounts_have_byte_equivalent_responses() {
        let (_vault, state) = fixture();
        let request = || {
            HttpRequest::builder()
                .uri("/search?q=needle")
                .header(HOST, "127.0.0.1:3210")
                .header(VAULT_HTTP_TOKEN_HEADER, "secret")
                .body(Body::empty())
                .expect("request")
        };
        let temporary = vault_router(state.clone())
            .oneshot(request())
            .await
            .expect("temporary response");
        let resident = vault_router(state)
            .oneshot(request())
            .await
            .expect("resident response");
        assert_eq!(temporary.status(), resident.status());
        assert_eq!(body_bytes(temporary).await, body_bytes(resident).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn temporary_and_resident_services_run_without_a_daemon_process() {
        use crate::host::HostSupervisor;
        use crate::shutdown::ShutdownSignal;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        async fn hosted_response(paths: VaultPaths, identity: VaultHttpServiceIdentity) -> Vec<u8> {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("listener");
            let address = listener.local_addr().expect("address");
            let state = VaultHttpState::new(
                paths,
                ServeRouteOptions {
                    permissions: None,
                    watch_enabled: false,
                },
                VaultHttpSecurity::new(
                    "secret",
                    vec![address.to_string()],
                    vec![format!("http://{address}")],
                ),
            )
            .expect("state");
            let registration = vault_listener_service_for(
                listener,
                state,
                tokio::runtime::Handle::current(),
                Vec::new(),
                identity,
            )
            .expect("registration");
            let stop = Arc::new(ShutdownSignal::default());
            let host = HostSupervisor::start_with_signal(
                vec![registration],
                Duration::from_secs(2),
                Arc::clone(&stop),
            )
            .expect("host");
            let mut stream = tokio::net::TcpStream::connect(address)
                .await
                .expect("connection");
            stream
                .write_all(
                    format!(
                        "GET /search?q=needle HTTP/1.1\r\nHost: {address}\r\nX-Vulcan-Token: secret\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .expect("request");
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.expect("response");
            host.shutdown().expect("shutdown");
            response
        }

        let (vault, _state) = fixture();
        let temporary = hosted_response(
            VaultPaths::new(vault.path()),
            VaultHttpServiceIdentity::temporary().unwrap(),
        )
        .await;
        let resident = hosted_response(
            VaultPaths::new(vault.path()),
            VaultHttpServiceIdentity::resident("notes").unwrap(),
        )
        .await;
        assert_eq!(temporary, resident);
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
    async fn router_publishes_exactly_the_routes_it_installs() {
        let (_vault, state) = fixture();
        let response = vault_router(state)
            .oneshot(
                HttpRequest::builder()
                    .uri("/")
                    .header(HOST, "127.0.0.1:3210")
                    .header(VAULT_HTTP_TOKEN_HEADER, "secret")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let value = body(response).await;
        let published = value["routes"]
            .as_array()
            .expect("routes")
            .iter()
            .map(|route| route["path"].as_str().expect("path"))
            .collect::<Vec<_>>();
        assert_eq!(published, serve_route_paths().collect::<Vec<_>>());
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

    #[tokio::test]
    async fn router_supports_browser_preflight_without_sharing_token_authority() {
        let (_vault, state) = fixture();
        let response = vault_router(state)
            .oneshot(
                HttpRequest::builder()
                    .method(Method::OPTIONS)
                    .uri("/search")
                    .header(HOST, "127.0.0.1:3210")
                    .header(ORIGIN, "http://127.0.0.1:3210")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response.headers()[axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "http://127.0.0.1:3210"
        );
        assert_eq!(
            response.headers()[axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS],
            "content-type, x-vulcan-token"
        );
    }
}
