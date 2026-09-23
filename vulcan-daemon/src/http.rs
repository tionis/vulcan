//! Authenticated loopback HTTP/JSON and WebSocket companion transport.

use crate::companion::{
    CompanionCapabilities, CompanionError, CompanionErrorKind, CompanionOperation,
    CompanionResolutionAgent, CompanionSemanticAgent, CompanionService,
    ConflictProposalApprovalRequest, ConflictProposalRejectionRequest, ConflictProposalRequest,
    ConflictResolveRequest, SemanticPlanRequest, SyncSelectionRequest, COMPANION_PROTOCOL_VERSION,
};
use crate::credentials::CompanionCredential;
use crate::http_policy::{
    apply_cors_headers, bearer_token, declared_body_exceeds, header_text, with_deadline,
    HeaderText, RequestAudit,
};
use crate::registry::{WikiId, WikiRegistry};
use crate::shutdown::ShutdownSignal;
use crate::supervisor::SyncSupervisor;
#[cfg(test)]
use axum::body::Body;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
#[cfg(test)]
use axum::http::header::{ACCESS_CONTROL_ALLOW_ORIGIN, AUTHORIZATION};
use axum::http::header::{ORIGIN, SEC_WEBSOCKET_PROTOCOL};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::net::TcpListener;
use vulcan_app::sync_state::SyncStateStore;

pub const PROTOCOL_VERSION_HEADER: &str = "vulcan-protocol-version";
pub const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";
const WEBSOCKET_PROTOCOL: &str = "vulcan.v1";
const WEBSOCKET_BEARER_PREFIX: &str = "vulcan.bearer.";
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 16 * 1024;
const COMPANION_HTTP_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct CompanionHttpState {
    pub registry: Arc<WikiRegistry>,
    pub supervisor: Arc<SyncSupervisor>,
    pub state_store: Arc<SyncStateStore>,
    pub credential: Arc<CompanionCredential>,
    pub resolution_agent: Option<Arc<CompanionResolutionAgent>>,
    pub semantic_agent: Option<Arc<CompanionSemanticAgent>>,
    pub shutdown: Option<Arc<ShutdownSignal>>,
    /// Stops accepting new requests before final synchronization begins.
    pub ingress_shutdown: Option<Arc<ShutdownSignal>>,
}

impl CompanionHttpState {
    #[must_use]
    pub fn service(&self) -> CompanionService<'_> {
        let mut service =
            CompanionService::new(&self.registry, &self.supervisor, &self.state_store);
        if let Some(agent) = self.resolution_agent.as_deref() {
            service = service.with_resolution_agent(agent);
        }
        if let Some(agent) = self.semantic_agent.as_deref() {
            service = service.with_semantic_agent(agent);
        }
        service
    }
}

#[derive(Debug, Deserialize)]
struct VaultListQuery {
    group: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ConflictDetailQuery {
    path_offset: Option<usize>,
    path_limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct CompanionEventSnapshot {
    version: u32,
    event: &'static str,
    vaults: Vec<crate::registry::WikiRegistrationStatus>,
    statuses: Vec<crate::status::DaemonWikiSyncStatus>,
    jobs: Vec<crate::supervisor::SupervisedSyncJob>,
    aggregates: Vec<crate::supervisor::AggregateSyncJob>,
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
        .route("/sync", post(enqueue_sync_selection))
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
        .route(
            "/{id}/sync/conflicts/{conflict}/proposals",
            post(create_conflict_proposal),
        )
        .route(
            "/{id}/sync/conflicts/{conflict}/proposals/approve",
            post(approve_conflict_proposal),
        )
        .route(
            "/{id}/sync/conflicts/{conflict}/proposals/reject",
            post(reject_conflict_proposal),
        )
        .route("/{id}/sync/semantic-plans", post(create_semantic_plan))
        .route("/jobs/{job}", get(job_status).delete(cancel_job))
        .route(
            "/aggregate-jobs/{job}",
            get(aggregate_job_status).delete(cancel_aggregate_job),
        )
        .route("/events", get(events))
        .route("/shutdown", post(shutdown))
        .layer(Extension(Arc::new(SnapshotHub::default())))
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
    let audit = RequestAudit::capture("companion", request.method(), request.uri());
    let (origin, origin_valid) = match header_text(request.headers(), &ORIGIN) {
        HeaderText::Valid(origin) => (Some(origin.to_string()), true),
        HeaderText::Absent => (None, true),
        HeaderText::Invalid => (None, false),
    };
    let response =
        authorize_request_inner(&state, request, next, origin.as_deref(), origin_valid).await;
    let response = cors_response(response, origin.as_deref());
    audit.emit(response.status());
    response
}

async fn authorize_request_inner(
    state: &CompanionHttpState,
    request: Request,
    next: Next,
    origin: Option<&str>,
    origin_valid: bool,
) -> Response {
    if declared_body_exceeds(request.headers(), MAX_REQUEST_BYTES) {
        return api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            CompanionErrorKind::InvalidRequest,
            "request body exceeds 1 MiB",
        );
    }
    if !origin_valid || !state.credential.allows_origin(origin) {
        return api_error(
            StatusCode::FORBIDDEN,
            CompanionErrorKind::PermissionDenied,
            "request Origin is not allowed",
        );
    }

    if request.method() == Method::OPTIONS {
        return StatusCode::NO_CONTENT.into_response();
    }

    let is_capabilities = request.uri().path() == "/capabilities";
    let is_events = request.uri().path() == "/events";
    if !is_capabilities && !is_events && !has_protocol_version(request.headers()) {
        return api_error(
            StatusCode::UPGRADE_REQUIRED,
            CompanionErrorKind::InvalidRequest,
            "missing or unsupported Vulcan protocol version",
        );
    }

    if !is_events && !has_authorization(request.headers(), &state.credential) {
        return api_error(
            StatusCode::UNAUTHORIZED,
            CompanionErrorKind::PermissionDenied,
            "missing or invalid companion bearer credential",
        );
    }

    let mutation = matches!(*request.method(), Method::POST | Method::DELETE);
    let Ok(response) = with_deadline(COMPANION_HTTP_DEADLINE, next.run(request)).await else {
        return api_error(
            StatusCode::GATEWAY_TIMEOUT,
            CompanionErrorKind::Internal,
            "companion request deadline exceeded; operation outcome may be unknown",
        );
    };
    if mutation && response.status().is_success() {
        state.supervisor.notify_change();
    }
    response
}

fn cors_response(mut response: Response, origin: Option<&str>) -> Response {
    response.headers_mut().insert(
        HeaderName::from_static(PROTOCOL_VERSION_HEADER),
        HeaderValue::from_static("1"),
    );
    apply_cors_headers(
        &mut response,
        origin,
        "authorization, content-type, idempotency-key, vulcan-protocol-version",
        "GET, POST, DELETE, OPTIONS",
    );
    response
}

fn has_protocol_version(headers: &HeaderMap) -> bool {
    headers
        .get(PROTOCOL_VERSION_HEADER)
        .is_some_and(|value| value.as_bytes() == b"1")
}

fn has_authorization(headers: &HeaderMap, credential: &CompanionCredential) -> bool {
    bearer_token(headers).is_some_and(|token| credential.authorizes(token))
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
    if state.shutdown.is_some() {
        capabilities
            .operations
            .push(CompanionOperation::DaemonShutdown);
    }
    Json(capabilities)
}

async fn shutdown(State(state): State<CompanionHttpState>) -> Result<Json<Value>, ApiError> {
    let shutdown = state.shutdown.ok_or_else(|| {
        ApiError(CompanionError::new(
            CompanionErrorKind::NotFound,
            "daemon shutdown is not available on this companion service",
        ))
    })?;
    if shutdown.begin_shutdown() {
        if let Some(ingress) = state.ingress_shutdown {
            ingress.cancel();
        }
    }
    Ok(Json(serde_json::json!({
        "version": COMPANION_PROTOCOL_VERSION,
        "stopping": true,
        "final_sync": true
    })))
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

async fn enqueue_sync_selection(
    State(state): State<CompanionHttpState>,
    headers: HeaderMap,
    request: Result<Json<SyncSelectionRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let key = idempotency_key(&headers)?.to_string();
    let scope = state.credential.id.clone();
    let Json(request) = request.map_err(request_rejection)?;
    let result = blocking(move || {
        state
            .service()
            .enqueue_sync_selection(&request, &scope, &key)
    })
    .await?;
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
    query: Result<Query<ConflictDetailQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_wiki_id(id)?;
    let Query(query) = query.map_err(request_rejection)?;
    if query.path_offset.is_some() && query.path_limit.is_none() {
        return Err(ApiError(CompanionError::new(
            CompanionErrorKind::InvalidRequest,
            "path_offset requires path_limit",
        )));
    }
    let result = blocking(move || {
        if let Some(limit) = query.path_limit {
            state.service().conflict_detail_page(
                &id,
                &conflict,
                query.path_offset.unwrap_or_default(),
                limit,
            )
        } else {
            state.service().conflict_detail(&id, &conflict)
        }
    })
    .await?;
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

async fn create_conflict_proposal(
    State(state): State<CompanionHttpState>,
    Path((id, conflict)): Path<(String, String)>,
    request: Result<Json<ConflictProposalRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_wiki_id(id)?;
    let Json(request) = request.map_err(request_rejection)?;
    let result = blocking(move || {
        state
            .service()
            .create_conflict_proposal(&id, &conflict, &request)
    })
    .await?;
    Ok(Json(serde_json::to_value(result).map_err(json_error)?))
}

async fn approve_conflict_proposal(
    State(state): State<CompanionHttpState>,
    Path((id, conflict)): Path<(String, String)>,
    request: Result<Json<ConflictProposalApprovalRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_wiki_id(id)?;
    let Json(request) = request.map_err(request_rejection)?;
    let result = blocking(move || {
        state
            .service()
            .approve_conflict_proposal(&id, &conflict, &request)
    })
    .await?;
    Ok(Json(serde_json::to_value(result).map_err(json_error)?))
}

async fn reject_conflict_proposal(
    State(state): State<CompanionHttpState>,
    Path((id, conflict)): Path<(String, String)>,
    request: Result<Json<ConflictProposalRejectionRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let id = parse_wiki_id(id)?;
    let Json(request) = request.map_err(request_rejection)?;
    let result = blocking(move || {
        state
            .service()
            .reject_conflict_proposal(&id, &conflict, &request)
    })
    .await?;
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

async fn aggregate_job_status(
    State(state): State<CompanionHttpState>,
    Path(job): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let result = blocking(move || state.service().aggregate_job(&job)).await?;
    Ok(Json(serde_json::to_value(result).map_err(json_error)?))
}

async fn cancel_aggregate_job(
    State(state): State<CompanionHttpState>,
    Path(job): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let result = blocking(move || state.service().cancel_aggregate_job(&job)).await?;
    Ok(Json(serde_json::to_value(result).map_err(json_error)?))
}

async fn events(
    State(state): State<CompanionHttpState>,
    Extension(hub): Extension<Arc<SnapshotHub>>,
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
        .on_upgrade(move |socket| stream_events(socket, state, hub))
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

#[derive(Default)]
struct SnapshotHub(tokio::sync::Mutex<Weak<SnapshotFeed>>);

struct SnapshotFeed {
    receiver: tokio::sync::watch::Receiver<Option<Arc<str>>>,
    task: tokio::task::AbortHandle,
}

impl Drop for SnapshotFeed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl SnapshotHub {
    async fn subscribe(&self, state: CompanionHttpState) -> Arc<SnapshotFeed> {
        let mut current = self.0.lock().await;
        if let Some(feed) = current.upgrade() {
            return feed;
        }
        let (sender, receiver) = tokio::sync::watch::channel(None);
        let task = tokio::spawn(publish_snapshots(state, sender, Duration::from_secs(30)));
        let feed = Arc::new(SnapshotFeed {
            receiver,
            task: task.abort_handle(),
        });
        *current = Arc::downgrade(&feed);
        feed
    }
}

async fn publish_snapshots(
    state: CompanionHttpState,
    sender: tokio::sync::watch::Sender<Option<Arc<str>>>,
    reconciliation: Duration,
) {
    // Subscribe before the first read so mutations during reconstruction are
    // retained. Every connection shares this producer; none means no producer.
    let mut changes = state.supervisor.subscribe_changes();
    let mut previous: Option<Arc<str>> = None;
    loop {
        let snapshot_state = state.clone();
        let snapshot = tokio::task::spawn_blocking(move || {
            event_snapshot(&snapshot_state).and_then(|snapshot| {
                serde_json::to_string(&snapshot).map_err(|error| {
                    CompanionError::new(CompanionErrorKind::Internal, error.to_string())
                })
            })
        })
        .await;
        let Ok(Ok(serialized)) = snapshot else {
            return;
        };
        if previous.as_deref() != Some(serialized.as_str()) {
            let serialized: Arc<str> = serialized.into();
            sender.send_replace(Some(Arc::clone(&serialized)));
            previous = Some(serialized);
        }
        tokio::select! {
            result = changes.changed() => { if result.is_err() { return; } }
            () = tokio::time::sleep(reconciliation) => {}
            () = sender.closed() => return,
        }
    }
}

async fn stream_events(mut socket: WebSocket, state: CompanionHttpState, hub: Arc<SnapshotHub>) {
    let feed = hub.subscribe(state).await;
    let mut snapshots = feed.receiver.clone();
    let initial = snapshots.borrow_and_update().clone();
    if let Some(initial) = initial {
        if socket
            .send(Message::Text(initial.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    loop {
        tokio::select! {
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    _ => {}
                }
            }
            result = snapshots.changed() => {
                if result.is_err() { break; }
                let snapshot = snapshots.borrow_and_update().clone();
                let Some(snapshot) = snapshot else { continue; };
                if socket.send(Message::Text(snapshot.to_string().into())).await.is_err() {
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
        aggregates: state.supervisor.list_aggregates().map_err(|error| {
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
                    profile: None,
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
            resolution_agent: None,
            semantic_agent: None,
            shutdown: None,
            ingress_shutdown: None,
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
    async fn event_feed_is_shared_wakes_on_jobs_and_releases_on_disconnect() {
        let (_temporary, state) = fixture();
        let hub = SnapshotHub::default();
        let first = hub.subscribe(state.clone()).await;
        let second = hub.subscribe(state.clone()).await;
        assert!(Arc::ptr_eq(&first, &second));
        let mut receiver = first.receiver.clone();
        tokio::time::timeout(Duration::from_secs(2), receiver.wait_for(Option::is_some))
            .await
            .unwrap()
            .unwrap();
        receiver.borrow_and_update();
        let registration = state.registry.load().unwrap().vaults.remove(0);
        state
            .supervisor
            .enqueue(
                registration.id.as_str(),
                &registration.path,
                vulcan_sync::SyncJobTrigger::Manual,
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        let value: Value =
            serde_json::from_str(receiver.borrow_and_update().as_deref().unwrap()).unwrap();
        assert_eq!(value["jobs"].as_array().unwrap().len(), 1);
        assert!(Arc::ptr_eq(
            receiver.borrow().as_ref().unwrap(),
            second.receiver.borrow().as_ref().unwrap()
        ));
        let weak = Arc::downgrade(&first);
        drop(first);
        drop(second);
        assert!(weak.upgrade().is_none());
        tokio::time::timeout(Duration::from_secs(2), receiver.changed())
            .await
            .unwrap()
            .unwrap_err();
    }

    #[tokio::test]
    async fn event_feed_reconciles_changes_made_outside_the_daemon() {
        let (_temporary, state) = fixture();
        let (sender, mut receiver) = tokio::sync::watch::channel(None);
        let worker = tokio::spawn(publish_snapshots(
            state.clone(),
            sender,
            Duration::from_millis(20),
        ));
        tokio::time::timeout(Duration::from_secs(2), receiver.wait_for(Option::is_some))
            .await
            .unwrap()
            .unwrap();
        receiver.borrow_and_update();
        let registration = state.registry.load().unwrap().vaults.remove(0);
        // This mutates the registry directly, without a supervisor notification.
        state
            .registry
            .update(
                &registration.id,
                &crate::registry::UpdateWikiRequest {
                    profile: None,
                    sync_paused: Some(true),
                    groups_to_add: vec![],
                    groups_to_remove: vec![],
                    permissions_profile: None,
                },
                false,
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(receiver.borrow().as_deref().unwrap()).unwrap();
        assert_eq!(value["vaults"][0]["sync_paused"], true);
        drop(receiver);
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap();
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
    async fn shutdown_quiesces_ingress_before_final_sync_finishes() {
        let (_temporary, mut state) = fixture();
        let shutdown = Arc::new(ShutdownSignal::default());
        let ingress = Arc::new(ShutdownSignal::default());
        state.shutdown = Some(Arc::clone(&shutdown));
        state.ingress_shutdown = Some(Arc::clone(&ingress));

        let response = companion_router(state.clone())
            .oneshot(request(&state, Method::POST, "/shutdown"))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(ingress.is_cancelled());
        assert!(
            !shutdown.begin_shutdown(),
            "final sync already owns shutdown"
        );
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
    async fn selection_sync_returns_a_monitorable_aggregate_job() {
        let (_temporary, state) = fixture();
        let router = companion_router(state.clone());
        let enqueue_request = HttpRequest::builder()
            .method(Method::POST)
            .uri("/sync")
            .header(AUTHORIZATION, format!("Bearer {}", state.credential.token))
            .header(PROTOCOL_VERSION_HEADER, "1")
            .header(IDEMPOTENCY_KEY_HEADER, "group-1")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"group":"personal"}"#))
            .expect("request");
        let response = router
            .clone()
            .oneshot(enqueue_request)
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let value = body_json(response).await;
        assert_eq!(value["aggregate"]["selection"], json!("group:personal"));
        assert_eq!(value["aggregate"]["total"], json!(1));
        let aggregate_id = value["aggregate"]["id"].as_str().expect("aggregate ID");

        let response = router
            .oneshot(request(
                &state,
                Method::GET,
                &format!("/aggregate-jobs/{aggregate_id}"),
            ))
            .await
            .expect("status response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["state"], json!("queued"));
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
    async fn declared_oversized_requests_fail_before_authentication_or_dispatch() {
        let (_temporary, state) = fixture();
        let response = companion_router(state)
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/sync")
                    .header(axum::http::header::CONTENT_LENGTH, MAX_REQUEST_BYTES + 1)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body_json(response).await["kind"], json!("invalid_request"));
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

    #[tokio::test]
    async fn conflict_proposal_endpoint_fails_closed_without_a_configured_provider() {
        let (_temporary, state) = fixture();
        let request = HttpRequest::builder()
            .method(Method::POST)
            .uri("/notes/sync/conflicts/0123456789abcdef0123456789abcdef/proposals")
            .header(AUTHORIZATION, format!("Bearer {}", state.credential.token))
            .header(PROTOCOL_VERSION_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"proposal_contract_version":2,"context":[],"allow_broad_context":false}"#,
            ))
            .expect("request");
        let response = companion_router(state)
            .oneshot(request)
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let value = body_json(response).await;
        assert_eq!(value["kind"], json!("not_found"));
        assert!(value["detail"]
            .as_str()
            .expect("detail")
            .contains("no resolution agent is configured"));
    }

    #[tokio::test]
    async fn semantic_agent_endpoint_fails_closed_without_a_configured_provider() {
        let (_temporary, state) = fixture();
        let request = HttpRequest::builder()
            .method(Method::POST)
            .uri("/notes/sync/semantic-plans")
            .header(AUTHORIZATION, format!("Bearer {}", state.credential.token))
            .header(PROTOCOL_VERSION_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"from":"main","to":"accepted","semantic_ref":"refs/heads/main","agent":true,"dry_run":true}"#,
            ))
            .expect("request");
        let response = companion_router(state)
            .oneshot(request)
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let value = body_json(response).await;
        assert_eq!(value["kind"], json!("not_found"));
        assert!(value["detail"]
            .as_str()
            .expect("detail")
            .contains("no semantic planning agent is configured"));
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
