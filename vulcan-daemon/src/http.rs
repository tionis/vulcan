//! Authenticated loopback HTTP/JSON and WebSocket companion transport.

use crate::companion::{
    CompanionCapabilities, CompanionError, CompanionErrorKind, CompanionOperation,
    CompanionService, ConflictResolveRequest, SemanticPlanRequest, COMPANION_PROTOCOL_VERSION,
};
use crate::credentials::CompanionCredential;
use crate::registry::{WikiId, WikiRegistry};
use crate::supervisor::SyncSupervisor;
#[cfg(test)]
use axum::body::Body;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    AUTHORIZATION, ORIGIN, SEC_WEBSOCKET_PROTOCOL,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use vulcan_app::sync_state::SyncStateStore;

pub const PROTOCOL_VERSION_HEADER: &str = "vulcan-protocol-version";
pub const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";
const WEBSOCKET_PROTOCOL: &str = "vulcan.v1";
const WEBSOCKET_BEARER_PREFIX: &str = "vulcan.bearer.";
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub struct CompanionHttpState {
    pub registry: Arc<WikiRegistry>,
    pub supervisor: Arc<SyncSupervisor>,
    pub state_store: Arc<SyncStateStore>,
    pub credential: Arc<CompanionCredential>,
}

impl CompanionHttpState {
    #[must_use]
    pub fn service(&self) -> CompanionService<'_> {
        CompanionService::new(&self.registry, &self.supervisor, &self.state_store)
    }
}

#[derive(Debug, Deserialize)]
struct VaultListQuery {
    group: Option<String>,
}

#[derive(Debug, Serialize)]
struct CompanionEventSnapshot {
    version: u32,
    event: &'static str,
    vaults: Vec<crate::registry::WikiRegistrationStatus>,
    statuses: Vec<crate::status::DaemonWikiSyncStatus>,
    jobs: Vec<crate::supervisor::SupervisedSyncJob>,
}

#[derive(Debug)]
struct ApiError(CompanionError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0.kind {
            CompanionErrorKind::InvalidRequest => StatusCode::BAD_REQUEST,
            CompanionErrorKind::NotFound => StatusCode::NOT_FOUND,
            CompanionErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
            CompanionErrorKind::Conflict => StatusCode::CONFLICT,
            CompanionErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(self.0)).into_response()
    }
}

impl From<CompanionError> for ApiError {
    fn from(error: CompanionError) -> Self {
        Self(error)
    }
}

pub fn companion_router(state: CompanionHttpState) -> Router {
    Router::new()
        .route("/capabilities", get(capabilities))
        .route("/vaults", get(list_vaults))
        .route("/{id}/sync/status", get(sync_status))
        .route("/{id}/sync", post(enqueue_sync))
        .route("/{id}/sync/pause", post(pause_sync))
        .route("/{id}/sync/resume", post(resume_sync))
        .route("/{id}/sync/conflicts", get(list_conflicts))
        .route("/{id}/sync/conflicts/{conflict}", get(conflict_detail))
        .route(
            "/{id}/sync/conflicts/{conflict}/resolve",
            post(resolve_conflict),
        )
        .route("/{id}/sync/semantic-plans", post(create_semantic_plan))
        .route("/jobs/{job}", get(job_status).delete(cancel_job))
        .route("/events", get(events))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            authorize_request,
        ))
        .with_state(state)
}

pub async fn serve_companion(
    listener: TcpListener,
    state: CompanionHttpState,
) -> Result<(), std::io::Error> {
    ensure_loopback(listener.local_addr()?)?;
    axum::serve(listener, companion_router(state)).await
}

pub async fn serve_companion_with_shutdown<F>(
    listener: TcpListener,
    state: CompanionHttpState,
    shutdown: F,
) -> Result<(), std::io::Error>
where
    F: Future<Output = ()> + Send + 'static,
{
    ensure_loopback(listener.local_addr()?)?;
    axum::serve(listener, companion_router(state))
        .with_graceful_shutdown(shutdown)
        .await
}

fn ensure_loopback(address: SocketAddr) -> Result<(), std::io::Error> {
    if address.ip().is_loopback() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("companion transport refuses non-loopback listener {address}"),
        ))
    }
}

async fn authorize_request(
    State(state): State<CompanionHttpState>,
    request: Request,
    next: Next,
) -> Response {
    let origin = request
        .headers()
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    if !state.credential.allows_origin(origin.as_deref()) {
        return cors_response(
            api_error(
                StatusCode::FORBIDDEN,
                CompanionErrorKind::PermissionDenied,
                "request Origin is not allowed",
            ),
            None,
        );
    }

    if request.method() == Method::OPTIONS {
        return cors_response(StatusCode::NO_CONTENT.into_response(), origin.as_deref());
    }

    let is_capabilities = request.uri().path() == "/capabilities";
    let is_events = request.uri().path() == "/events";
    if !is_capabilities && !is_events && !has_protocol_version(request.headers()) {
        return cors_response(
            api_error(
                StatusCode::UPGRADE_REQUIRED,
                CompanionErrorKind::InvalidRequest,
                "missing or unsupported Vulcan protocol version",
            ),
            origin.as_deref(),
        );
    }

    if !is_events && !has_authorization(request.headers(), &state.credential) {
        return cors_response(
            api_error(
                StatusCode::UNAUTHORIZED,
                CompanionErrorKind::PermissionDenied,
                "missing or invalid companion bearer credential",
            ),
            origin.as_deref(),
        );
    }

    let response = next.run(request).await;
    cors_response(response, origin.as_deref())
}

fn cors_response(mut response: Response, origin: Option<&str>) -> Response {
    response.headers_mut().insert(
        HeaderName::from_static(PROTOCOL_VERSION_HEADER),
        HeaderValue::from_static("1"),
    );
    if let Some(origin) = origin.and_then(|origin| HeaderValue::from_str(origin).ok()) {
        response
            .headers_mut()
            .insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response.headers_mut().insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(
                "authorization, content-type, idempotency-key, vulcan-protocol-version",
            ),
        );
        response.headers_mut().insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
        );
    }
    response
}

fn has_protocol_version(headers: &HeaderMap) -> bool {
    headers
        .get(PROTOCOL_VERSION_HEADER)
        .is_some_and(|value| value.as_bytes() == b"1")
}

fn has_authorization(headers: &HeaderMap, credential: &CompanionCredential) -> bool {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| credential.authorizes(token))
}

fn idempotency_key(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get(IDEMPOTENCY_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ApiError(CompanionError::new(
                CompanionErrorKind::InvalidRequest,
                "missing valid Idempotency-Key header",
            ))
        })
}

fn parse_wiki_id(id: String) -> Result<WikiId, ApiError> {
    WikiId::parse(id).map_err(|error| {
        ApiError(CompanionError::new(
            CompanionErrorKind::InvalidRequest,
            error.to_string(),
        ))
    })
}

async fn blocking<T, F>(operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CompanionError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| {
            ApiError(CompanionError::new(
                CompanionErrorKind::Internal,
                format!("companion operation task failed: {error}"),
            ))
        })?
        .map_err(ApiError)
}

async fn capabilities(State(state): State<CompanionHttpState>) -> Json<CompanionCapabilities> {
    let mut capabilities = state.service().capabilities();
    capabilities.transports = vec!["http_json".to_string(), "websocket".to_string()];
    capabilities
        .operations
        .push(CompanionOperation::EventSubscribe);
    Json(capabilities)
}

async fn list_vaults(
    State(state): State<CompanionHttpState>,
    query: Result<Query<VaultListQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(request_rejection)?;
    let result = blocking(move || state.service().list_wikis(query.group.as_deref())).await?;
    Ok(Json(serde_json::to_value(result).map_err(json_error)?))
}

async fn sync_status(
    State(state): State<CompanionHttpState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_wiki_id(id)?;
    let result = blocking(move || state.service().sync_status(&id)).await?;
    Ok(Json(serde_json::to_value(result).map_err(json_error)?))
}

async fn enqueue_sync(
    State(state): State<CompanionHttpState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let id = parse_wiki_id(id)?;
    let key = idempotency_key(&headers)?.to_string();
    let scope = state.credential.id.clone();
    let result = blocking(move || state.service().enqueue_sync(&id, &scope, &key)).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(result).map_err(json_error)?),
    ))
}

async fn pause_sync(
    State(state): State<CompanionHttpState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_wiki_id(id)?;
    let result = blocking(move || state.service().pause_sync(&id)).await?;
    Ok(Json(serde_json::to_value(result).map_err(json_error)?))
}

async fn resume_sync(
    State(state): State<CompanionHttpState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let id = parse_wiki_id(id)?;
    let key = idempotency_key(&headers)?.to_string();
    let scope = state.credential.id.clone();
    let result = blocking(move || state.service().resume_sync(&id, &scope, &key)).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(result).map_err(json_error)?),
    ))
}

async fn list_conflicts(
    State(state): State<CompanionHttpState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_wiki_id(id)?;
    let result = blocking(move || state.service().list_conflicts(&id)).await?;
    Ok(Json(serde_json::to_value(result).map_err(json_error)?))
}

async fn conflict_detail(
    State(state): State<CompanionHttpState>,
    Path((id, conflict)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_wiki_id(id)?;
    let result = blocking(move || state.service().conflict_detail(&id, &conflict)).await?;
    Ok(Json(serde_json::to_value(result).map_err(json_error)?))
}

async fn resolve_conflict(
    State(state): State<CompanionHttpState>,
    Path((id, conflict)): Path<(String, String)>,
    request: Result<Json<ConflictResolveRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_wiki_id(id)?;
    let Json(request) = request.map_err(request_rejection)?;
    let result =
        blocking(move || state.service().resolve_conflict(&id, &conflict, &request)).await?;
    Ok(Json(serde_json::to_value(result).map_err(json_error)?))
}

async fn create_semantic_plan(
    State(state): State<CompanionHttpState>,
    Path(id): Path<String>,
    request: Result<Json<SemanticPlanRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_wiki_id(id)?;
    let Json(request) = request.map_err(request_rejection)?;
    let result = blocking(move || state.service().create_semantic_plan(&id, &request)).await?;
    Ok(Json(serde_json::to_value(result).map_err(json_error)?))
}

async fn job_status(
    State(state): State<CompanionHttpState>,
    Path(job): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let result = blocking(move || state.service().job(&job)).await?;
    Ok(Json(serde_json::to_value(result).map_err(json_error)?))
}

async fn cancel_job(
    State(state): State<CompanionHttpState>,
    Path(job): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let result = blocking(move || state.service().cancel_job(&job)).await?;
    Ok(Json(serde_json::to_value(result).map_err(json_error)?))
}

async fn events(
    State(state): State<CompanionHttpState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !websocket_authorized(&headers, &state.credential) {
        return api_error(
            StatusCode::UNAUTHORIZED,
            CompanionErrorKind::PermissionDenied,
            "missing or invalid WebSocket companion credential",
        );
    }
    upgrade
        .protocols([WEBSOCKET_PROTOCOL])
        .max_message_size(MAX_WEBSOCKET_MESSAGE_BYTES)
        .on_upgrade(move |socket| stream_events(socket, state))
}

fn websocket_authorized(headers: &HeaderMap, credential: &CompanionCredential) -> bool {
    let mut version = false;
    let mut authorized = false;
    for value in headers.get_all(SEC_WEBSOCKET_PROTOCOL) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        for protocol in value.split(',').map(str::trim) {
            version |= protocol == WEBSOCKET_PROTOCOL;
            if let Some(token) = protocol.strip_prefix(WEBSOCKET_BEARER_PREFIX) {
                authorized |= credential.authorizes(token);
            }
        }
    }
    version && authorized
}

async fn stream_events(mut socket: WebSocket, state: CompanionHttpState) {
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    let mut previous = None;
    loop {
        tokio::select! {
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    _ => {}
                }
            }
            _ = interval.tick() => {
                let snapshot_state = state.clone();
                let snapshot = tokio::task::spawn_blocking(move || event_snapshot(&snapshot_state)).await;
                let Ok(Ok(snapshot)) = snapshot else {
                    break;
                };
                let Ok(serialized) = serde_json::to_string(&snapshot) else {
                    break;
                };
                if previous.as_deref() == Some(serialized.as_str()) {
                    continue;
                }
                previous = Some(serialized.clone());
                if socket.send(Message::Text(serialized.into())).await.is_err() {
                    break;
                }
            }
        }
    }
}

fn event_snapshot(state: &CompanionHttpState) -> Result<CompanionEventSnapshot, CompanionError> {
    let vaults = state.service().list_wikis(None)?;
    let statuses = vaults
        .iter()
        .map(|vault| state.service().sync_status(&vault.registration.id))
        .collect::<Result<Vec<_>, CompanionError>>()?;
    Ok(CompanionEventSnapshot {
        version: COMPANION_PROTOCOL_VERSION,
        event: "state_snapshot",
        vaults,
        statuses,
        jobs: state.supervisor.list().map_err(|error| {
            CompanionError::new(CompanionErrorKind::Internal, error.to_string())
        })?,
    })
}

fn json_error(error: serde_json::Error) -> ApiError {
    let detail = error.to_string();
    drop(error);
    ApiError(CompanionError::new(CompanionErrorKind::Internal, detail))
}

fn request_rejection(error: impl std::fmt::Display) -> ApiError {
    ApiError(CompanionError::new(
        CompanionErrorKind::InvalidRequest,
        error.to_string(),
    ))
}

fn api_error(status: StatusCode, kind: CompanionErrorKind, detail: impl Into<String>) -> Response {
    (status, Json(CompanionError::new(kind, detail))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AddWikiRequest, WikiId};
    use axum::http::Request as HttpRequest;
    use serde_json::json;
    use tempfile::tempdir;
    use tower::ServiceExt;

    fn fixture() -> (tempfile::TempDir, CompanionHttpState) {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        std::fs::create_dir(&vault).expect("vault directory");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        registry
            .add(
                &AddWikiRequest {
                    id: WikiId::parse("notes").expect("wiki id"),
                    path: vault,
                    groups: vec!["personal".to_string()],
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("register wiki");
        let state = CompanionHttpState {
            registry: Arc::new(registry),
            supervisor: Arc::new(
                SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor"),
            ),
            state_store: Arc::new(SyncStateStore::at(temporary.path().join("sync-state"))),
            credential: Arc::new(
                CompanionCredential::generate(vec!["app://obsidian.md".to_string()])
                    .expect("credential"),
            ),
        };
        (temporary, state)
    }

    fn request(state: &CompanionHttpState, method: Method, uri: &str) -> HttpRequest<Body> {
        HttpRequest::builder()
            .method(method)
            .uri(uri)
            .header(AUTHORIZATION, format!("Bearer {}", state.credential.token))
            .header(PROTOCOL_VERSION_HEADER, "1")
            .body(Body::empty())
            .expect("request")
    }

    async fn body_json(response: Response) -> Value {
        let body = axum::body::to_bytes(response.into_body(), MAX_REQUEST_BYTES)
            .await
            .expect("response body");
        serde_json::from_slice(&body).expect("JSON response")
    }

    #[tokio::test]
    async fn capabilities_are_authenticated_versioned_and_transport_truthful() {
        let (_temporary, state) = fixture();
        let router = companion_router(state.clone());
        let unauthorized = router
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/capabilities")
                    .header(ORIGIN, "app://obsidian.md")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            unauthorized.headers()[ACCESS_CONTROL_ALLOW_ORIGIN],
            "app://obsidian.md"
        );

        let response = router
            .oneshot(request(&state, Method::GET, "/capabilities"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[PROTOCOL_VERSION_HEADER], "1");
        let value = body_json(response).await;
        assert_eq!(value["transports"], json!(["http_json", "websocket"]));
        assert!(value["operations"]
            .as_array()
            .expect("operations")
            .contains(&json!("event_subscribe")));
    }

    #[tokio::test]
    async fn manual_sync_requires_version_and_idempotency_and_replays() {
        let (_temporary, state) = fixture();
        let router = companion_router(state.clone());
        let mut missing_version = request(&state, Method::POST, "/notes/sync");
        missing_version
            .headers_mut()
            .remove(PROTOCOL_VERSION_HEADER);
        assert_eq!(
            router
                .clone()
                .oneshot(missing_version)
                .await
                .expect("response")
                .status(),
            StatusCode::UPGRADE_REQUIRED
        );
        assert_eq!(
            router
                .clone()
                .oneshot(request(&state, Method::POST, "/notes/sync"))
                .await
                .expect("response")
                .status(),
            StatusCode::BAD_REQUEST
        );

        let mut first = request(&state, Method::POST, "/notes/sync");
        first
            .headers_mut()
            .insert(IDEMPOTENCY_KEY_HEADER, HeaderValue::from_static("sync-1"));
        let first = router.clone().oneshot(first).await.expect("response");
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        assert_eq!(body_json(first).await["replay"], json!(false));
        let mut replay = request(&state, Method::POST, "/notes/sync");
        replay
            .headers_mut()
            .insert(IDEMPOTENCY_KEY_HEADER, HeaderValue::from_static("sync-1"));
        let replay = router.oneshot(replay).await.expect("response");
        assert_eq!(body_json(replay).await["replay"], json!(true));
    }

    #[tokio::test]
    async fn origin_policy_applies_to_http_and_preflight() {
        let (_temporary, state) = fixture();
        let router = companion_router(state.clone());
        let mut denied = request(&state, Method::GET, "/vaults");
        denied
            .headers_mut()
            .insert(ORIGIN, HeaderValue::from_static("https://example.com"));
        assert_eq!(
            router
                .clone()
                .oneshot(denied)
                .await
                .expect("response")
                .status(),
            StatusCode::FORBIDDEN
        );

        let preflight = HttpRequest::builder()
            .method(Method::OPTIONS)
            .uri("/notes/sync")
            .header(ORIGIN, "app://obsidian.md")
            .body(Body::empty())
            .expect("preflight");
        let response = router.oneshot(preflight).await.expect("response");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response.headers()[ACCESS_CONTROL_ALLOW_ORIGIN],
            "app://obsidian.md"
        );
    }

    #[tokio::test]
    async fn malformed_json_uses_the_versioned_error_contract() {
        let (_temporary, state) = fixture();
        let request = HttpRequest::builder()
            .method(Method::POST)
            .uri("/notes/sync/semantic-plans")
            .header(AUTHORIZATION, format!("Bearer {}", state.credential.token))
            .header(PROTOCOL_VERSION_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from("{"))
            .expect("request");
        let response = companion_router(state)
            .oneshot(request)
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let value = body_json(response).await;
        assert_eq!(value["version"], json!(1));
        assert_eq!(value["kind"], json!("invalid_request"));
    }

    #[test]
    fn websocket_subprotocol_carries_version_and_bearer_without_url_secrets() {
        let (_temporary, state) = fixture();
        let mut headers = HeaderMap::new();
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(&format!(
                "vulcan.v1, vulcan.bearer.{}",
                state.credential.token
            ))
            .expect("protocol header"),
        );
        assert!(websocket_authorized(&headers, &state.credential));
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("vulcan.v1, vulcan.bearer.wrong"),
        );
        assert!(!websocket_authorized(&headers, &state.credential));
    }

    #[tokio::test]
    async fn websocket_negotiates_version_in_subprotocol_without_custom_header() {
        let (_temporary, state) = fixture();
        let request = HttpRequest::builder()
            .uri("/events")
            .header(ORIGIN, "app://obsidian.md")
            .header(
                SEC_WEBSOCKET_PROTOCOL,
                format!("vulcan.v1, vulcan.bearer.{}", state.credential.token),
            )
            .body(Body::empty())
            .expect("request");
        let response = companion_router(state)
            .oneshot(request)
            .await
            .expect("response");
        assert_ne!(response.status(), StatusCode::UPGRADE_REQUIRED);
    }

    #[test]
    fn listener_must_be_loopback() {
        assert!(ensure_loopback("127.0.0.1:3210".parse().expect("address")).is_ok());
        assert!(ensure_loopback("[::1]:3210".parse().expect("address")).is_ok());
        assert!(ensure_loopback("0.0.0.0:3210".parse().expect("address")).is_err());
    }
}
