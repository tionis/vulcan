//! The daemon REST API (Roadmap 10.3): request principals and the routes a
//! per-vault API token may reach. See `docs/specs/daemon-rest-api.md`.

use crate::companion::{CompanionError, CompanionErrorKind};
use crate::http::CompanionHttpState;
use crate::routed_reads::{RoutedReadError, RoutedVault};
use axum::extract::{Path, Query, State};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use ulid::Ulid;
use vulcan_app::routed::graph::{GraphAnswer, GraphRequest};
use vulcan_app::serve::{
    route_request_with_sessions, ServeHealthState, ServeRequest, ServeRouteOptions, ServeSessions,
};
use vulcan_core::{PermissionGuard, ProfilePermissionGuard};

/// Who a request acts for, resolved from its bearer credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// The companion credential: every route and registered vault.
    Device,
    /// An API token: the REST routes of one registration.
    Vault {
        wiki_id: String,
        registration_id: Ulid,
        token_id: Ulid,
    },
}

impl Principal {
    /// Whether this principal may see the registered wiki `wiki_id`.
    #[must_use]
    pub fn sees_wiki(&self, wiki_id: &str) -> bool {
        match self {
            Self::Device => true,
            Self::Vault { wiki_id: own, .. } => own == wiki_id,
        }
    }

    /// The scope idempotency keys of this principal live in.
    #[must_use]
    pub fn idempotency_scope(&self, companion_credential_id: &str) -> String {
        match self {
            Self::Device => companion_credential_id.to_string(),
            Self::Vault { token_id, .. } => format!("api-token:{token_id}"),
        }
    }
}

/// Daemon-wide routes an API token may call.
const VAULT_PRINCIPAL_DAEMON_ROUTES: &[&str] =
    &["/capabilities", "/health", "/vaults", "/openapi.json"];

/// The first path segment after `/{id}/` of every per-vault REST route.
pub const REST_VAULT_SEGMENTS: &[&str] = &[
    "health",
    "doctor",
    "search",
    "notes",
    "info",
    "links",
    "backlinks",
    "graph",
    "query",
    "related",
    "dataview",
    "bases",
    "mdbase",
    "history",
    "diff",
    "append",
    "update",
    "move",
    "inbox",
    "scan",
];

/// The REST surface this module serves, for `GET /capabilities`.
#[must_use]
pub(crate) fn capabilities() -> crate::companion::RestCapabilities {
    crate::companion::RestCapabilities {
        daemon_routes: ["/capabilities", "/health", "/vaults", "/openapi.json"]
            .map(str::to_string)
            .to_vec(),
        vault_route_groups: REST_VAULT_SEGMENTS
            .iter()
            .map(|segment| (*segment).to_string())
            .collect(),
        openapi: "/openapi.json".to_string(),
    }
}

/// The REST read routes, merged into the companion router so they share its
/// transport policy and principal middleware.
pub(crate) fn routes() -> Router<CompanionHttpState> {
    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi))
        .route("/{id}/health", get(vault_health))
        .route("/{id}/doctor", get(doctor))
        .route("/{id}/search", get(serve_route))
        .route(
            "/{id}/notes",
            get(serve_route).post(crate::rest_writes::create_note),
        )
        .route("/{id}/query", get(serve_route))
        .route("/{id}/related", get(serve_route))
        .route("/{id}/bases/eval", get(serve_route))
        .route("/{id}/dataview/{route}", get(serve_route))
        .route("/{id}/mdbase/{route}", get(serve_route))
        .route(
            "/{id}/notes/{*path}",
            get(note_get)
                .put(crate::rest_writes::set_note)
                .patch(crate::rest_writes::patch_note)
                .delete(crate::rest_writes::delete_note),
        )
        .route(
            "/{id}/append/{*path}",
            axum::routing::post(crate::rest_writes::append_note),
        )
        .route(
            "/{id}/inbox",
            axum::routing::post(crate::rest_writes::inbox),
        )
        .route(
            "/{id}/move",
            axum::routing::post(crate::rest_writes::move_note),
        )
        .route("/{id}/scan", axum::routing::post(crate::rest_writes::scan))
        .route(
            "/{id}/update",
            axum::routing::post(crate::rest_writes::update),
        )
        .route("/{id}/info/{*path}", get(note_info))
        .route("/{id}/links/{*path}", get(links))
        .route("/{id}/backlinks/{*path}", get(backlinks))
        .route("/{id}/graph/{report}", get(graph))
        .route("/{id}/history", get(history))
        .route("/{id}/history/{*path}", get(file_history))
        .route("/{id}/diff", get(diff))
        .route("/{id}/diff/{*path}", get(file_diff))
}

/// A REST failure, in the companion error shape.
pub(crate) struct RestError {
    pub(crate) status: StatusCode,
    pub(crate) error: CompanionError,
}

impl RestError {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        let kind = match status {
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                CompanionErrorKind::InvalidRequest
            }
            StatusCode::NOT_FOUND => CompanionErrorKind::NotFound,
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => {
                CompanionErrorKind::PermissionDenied
            }
            StatusCode::CONFLICT => CompanionErrorKind::Conflict,
            _ => CompanionErrorKind::Internal,
        };
        Self {
            status,
            error: CompanionError::new(kind, message),
        }
    }

    /// A workflow failure: denials are 403, missing notes 404, and anything
    /// else the caller's to correct.
    pub(crate) fn from_message(message: impl Into<String>) -> Self {
        let message = message.into();
        let lower = message.to_ascii_lowercase();
        let status = if lower.contains("denied") || lower.contains("permission profile") {
            StatusCode::FORBIDDEN
        } else if lower.contains("not found") || lower.contains("no note") {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        Self::new(status, message)
    }
}

impl From<RoutedReadError> for RestError {
    fn from(error: RoutedReadError) -> Self {
        match error {
            RoutedReadError::Unavailable(reason) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("the vault cannot be served right now: {reason}"),
            ),
            RoutedReadError::Failed(message) => Self::from_message(message),
        }
    }
}

impl IntoResponse for RestError {
    fn into_response(self) -> Response {
        (self.status, Json(self.error)).into_response()
    }
}

/// The served vault `id` names, as `principal` may see it.
pub(crate) fn served_vault(
    state: &CompanionHttpState,
    principal: &Principal,
    id: &str,
) -> Result<Arc<RoutedVault>, RestError> {
    let unknown = || RestError::new(StatusCode::NOT_FOUND, format!("unknown vault `{id}`"));
    if !principal.sees_wiki(id) {
        return Err(unknown());
    }
    if let Some(vault) = state
        .routed_reads
        .as_ref()
        .and_then(|routed| routed.get(id))
    {
        return Ok(vault);
    }
    let registered = state
        .registry
        .poll()
        .map(|config| config.vaults.iter().any(|wiki| wiki.id.as_str() == id))
        .unwrap_or(false);
    if registered {
        Err(RestError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "vault `{id}` is not served right now: its index is not running here (a files-only directory, or the daemon is still starting)"
            ),
        ))
    } else {
        Err(unknown())
    }
}

/// Runs `read` on a blocking thread against a served vault, under the
/// registration's permission profile after an index barrier.
async fn read_vault<T, F>(
    state: &CompanionHttpState,
    principal: &Principal,
    id: &str,
    read: F,
) -> Result<Json<Value>, RestError>
where
    T: serde::Serialize,
    F: FnOnce(&RoutedVault, &ProfilePermissionGuard) -> Result<T, RestError> + Send + 'static,
{
    let vault = served_vault(state, principal, id)?;
    let value = tokio::task::spawn_blocking(move || {
        let guard = vault.rest_read_guard()?;
        let value = read(&vault, &guard)?;
        serde_json::to_value(value)
            .map_err(|error| RestError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
    })
    .await
    .map_err(|error| RestError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))??;
    Ok(Json(value))
}

/// Vault-relative paths only: no absolute paths or parent components.
pub(crate) fn vault_relative(path: &str) -> Result<String, RestError> {
    let candidate = std::path::Path::new(path);
    if path.is_empty()
        || candidate.is_absolute()
        || path.starts_with('/')
        || path.contains('\\')
        || candidate
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(RestError::new(
            StatusCode::BAD_REQUEST,
            format!("`{path}` is not a vault-relative path"),
        ));
    }
    Ok(path.to_string())
}

fn query_pairs(uri: &Uri) -> HashMap<String, Vec<String>> {
    let mut pairs = HashMap::<String, Vec<String>>::new();
    if let Some(query) = uri.query() {
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            pairs
                .entry(key.into_owned())
                .or_default()
                .push(value.into_owned());
        }
    }
    pairs
}

/// The routes `vulcan serve` already answers, mounted per vault: the path
/// after `/{id}` is the serve route, and its `result` is the response.
async fn serve_route(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path(params): Path<HashMap<String, String>>,
    uri: Uri,
) -> Result<Json<Value>, RestError> {
    let id = params.get("id").cloned().unwrap_or_default();
    let path = uri
        .path()
        .strip_prefix(&format!("/{id}"))
        .unwrap_or_default()
        .to_string();
    let query = query_pairs(&uri);
    read_vault(&state, &principal, &id, move |vault, _guard| {
        let options = ServeRouteOptions {
            permissions: Some(
                vault
                    .permissions_profile
                    .clone()
                    .unwrap_or_else(|| "unrestricted".to_string()),
            ),
            watch_enabled: false,
        };
        let response = route_request_with_sessions(
            &vault.paths,
            &options,
            &ServeHealthState::default(),
            &ServeRequest {
                method: "GET".to_string(),
                path,
                query,
            },
            ServeSessions {
                mdbase: None,
                mdbase_changes: None,
                notes: Some(vault.session()),
            },
        );
        if response.status < 400 {
            return Ok(response
                .body
                .get("result")
                .cloned()
                .unwrap_or(response.body));
        }
        let message = response
            .body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("request failed")
            .to_string();
        Err(RestError::new(
            StatusCode::from_u16(response.status).unwrap_or(StatusCode::BAD_REQUEST),
            message,
        ))
    })
    .await
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteGetQuery {
    mode: Option<String>,
    section_id: Option<String>,
    heading: Option<String>,
    block_ref: Option<String>,
    lines: Option<String>,
    #[serde(rename = "match")]
    match_pattern: Option<String>,
    #[serde(default)]
    context: usize,
    #[serde(default)]
    no_frontmatter: bool,
    #[serde(default)]
    raw: bool,
    #[serde(default)]
    evaluate: bool,
    dataview_js: Option<String>,
}

/// `GET /{id}/notes/{path}`: one note, as `vulcan note get` reads it.
async fn note_get(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path((id, path)): Path<(String, String)>,
    query: Result<Query<NoteGetQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, RestError> {
    let Query(query) =
        query.map_err(|error| RestError::new(StatusCode::BAD_REQUEST, error.body_text()))?;
    let note = vault_relative(&path)?;
    read_vault(&state, &principal, &id, move |vault, guard| {
        vulcan_app::mcp_read_tools::note_get(
            &vault.paths,
            guard,
            &vulcan_app::mcp_protocol::McpNoteGetArgs {
                note,
                mode: query.mode,
                section_id: query.section_id,
                heading: query.heading,
                block_ref: query.block_ref,
                lines: query.lines,
                match_pattern: query.match_pattern,
                context: query.context,
                no_frontmatter: query.no_frontmatter,
                raw: query.raw,
                evaluate: query.evaluate,
                dataview_js: query.dataview_js,
            },
        )
        .map_err(mcp_error)
    })
    .await
}

/// `GET /{id}/info/{path}`: a note's metadata, as `vulcan note info` reports.
async fn note_info(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path((id, path)): Path<(String, String)>,
) -> Result<Json<Value>, RestError> {
    let note = vault_relative(&path)?;
    read_vault(&state, &principal, &id, move |vault, guard| {
        vulcan_app::mcp_read_tools::note_info(
            &vault.paths,
            guard,
            &vulcan_app::mcp_protocol::McpNoteInfoArgs { note },
        )
        .map_err(mcp_error)
    })
    .await
}

/// A shared workflow's failure: invalid parameters are 400, anything else is
/// classified by its message.
fn mcp_error(error: vulcan_app::mcp_protocol::McpMethodError) -> RestError {
    use vulcan_app::mcp_protocol::McpMethodError;
    match error {
        McpMethodError::JsonRpc {
            code: -32602,
            message,
            ..
        } => RestError::new(StatusCode::BAD_REQUEST, message),
        McpMethodError::JsonRpc { message, .. } | McpMethodError::Tool { message, .. } => {
            RestError::from_message(message)
        }
    }
}

/// The read filter `guard` imposes, if it restricts anything.
fn restricting_filter(guard: &ProfilePermissionGuard) -> Option<vulcan_core::PermissionFilter> {
    let filter = guard.read_filter();
    (!filter.path_permission().is_unrestricted()).then_some(filter)
}

/// `GET /{id}/links/{path}`: outgoing links, as `vulcan links` reports.
async fn links(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path((id, path)): Path<(String, String)>,
) -> Result<Json<Value>, RestError> {
    let note = vault_relative(&path)?;
    read_vault(&state, &principal, &id, move |vault, guard| {
        vulcan_core::query_links_with_filter(
            &vault.paths,
            &note,
            restricting_filter(guard).as_ref(),
        )
        .map_err(|error| RestError::from_message(error.to_string()))
    })
    .await
}

/// `GET /{id}/backlinks/{path}`: incoming links, as `vulcan backlinks` reports.
async fn backlinks(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path((id, path)): Path<(String, String)>,
) -> Result<Json<Value>, RestError> {
    let note = vault_relative(&path)?;
    read_vault(&state, &principal, &id, move |vault, guard| {
        vulcan_core::query_backlinks_with_filter(
            &vault.paths,
            &note,
            restricting_filter(guard).as_ref(),
        )
        .map_err(|error| RestError::from_message(error.to_string()))
    })
    .await
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphPathQuery {
    from: Option<String>,
    to: Option<String>,
}

/// `GET /{id}/graph/{stats|hubs|path}` from the vault's retained graph.
async fn graph(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path((id, report)): Path<(String, String)>,
    query: Result<Query<GraphPathQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, RestError> {
    let Query(query) =
        query.map_err(|error| RestError::new(StatusCode::BAD_REQUEST, error.body_text()))?;
    let request = match report.as_str() {
        "stats" => GraphRequest::Stats,
        "hubs" => GraphRequest::Hubs,
        "path" => match (query.from, query.to) {
            (Some(from), Some(to)) => GraphRequest::Path { from, to },
            _ => {
                return Err(RestError::new(
                    StatusCode::BAD_REQUEST,
                    "graph path needs `from` and `to`",
                ))
            }
        },
        "dead-ends" | "components" => {
            let dead_ends = report == "dead-ends";
            return read_vault(&state, &principal, &id, move |vault, guard| {
                let filter = restricting_filter(guard);
                if dead_ends {
                    vulcan_core::query_graph_dead_ends_with_filter(&vault.paths, filter.as_ref())
                        .map(|report| json!(report))
                } else {
                    vulcan_core::query_graph_components_with_filter(&vault.paths, filter.as_ref())
                        .map(|report| json!(report))
                }
                .map_err(|error| RestError::from_message(error.to_string()))
            })
            .await;
        }
        other => {
            return Err(RestError::new(
                StatusCode::NOT_FOUND,
                format!("unknown graph report `{other}`"),
            ))
        }
    };
    read_vault(&state, &principal, &id, move |vault, guard| {
        let answer = vault
            .graph()
            .run(
                vault.session(),
                &vault.paths,
                &request,
                restricting_filter(guard).as_ref(),
            )
            .map_err(|error| RestError::from_message(error.to_string()))?;
        Ok(match answer {
            GraphAnswer::Stats(report) => json!(report),
            GraphAnswer::Hubs(report) => json!(report),
            GraphAnswer::Path(report) => json!(report),
        })
    })
    .await
}

/// History and diffs work on any registered vault with Git, indexed or not:
/// they read Git, not the cache.
const MAX_HISTORY_LIMIT: usize = 500;

/// Runs `read` on a blocking thread for a registered vault under its
/// registration's profile, after checking that the profile allows Git.
async fn read_git<F>(
    state: &CompanionHttpState,
    principal: &Principal,
    id: &str,
    read: F,
) -> Result<Json<Value>, RestError>
where
    F: FnOnce(&vulcan_core::VaultPaths, &ProfilePermissionGuard) -> Result<Value, RestError>
        + Send
        + 'static,
{
    if !principal.sees_wiki(id) {
        return Err(RestError::new(
            StatusCode::NOT_FOUND,
            format!("unknown vault `{id}`"),
        ));
    }
    let registry = Arc::clone(&state.registry);
    let id = id.to_string();
    tokio::task::spawn_blocking(move || {
        let config = registry.poll().map_err(|error| {
            RestError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
        })?;
        let wiki = config
            .vaults
            .into_iter()
            .find(|wiki| wiki.id.as_str() == id)
            .ok_or_else(|| {
                RestError::new(StatusCode::NOT_FOUND, format!("unknown vault `{id}`"))
            })?;
        let paths = vulcan_core::VaultPaths::new(&wiki.path);
        let profile = vulcan_core::resolve_device_permission_profile(
            &paths,
            Some(
                wiki.permissions_profile
                    .as_deref()
                    .unwrap_or("unrestricted"),
            ),
        )
        .map_err(|error| RestError::from_message(error.to_string()))?;
        let guard = ProfilePermissionGuard::new(&paths, profile);
        guard
            .check_git()
            .map_err(|error| RestError::new(StatusCode::FORBIDDEN, error.to_string()))?;
        read(&paths, &guard).map(Json)
    })
    .await
    .map_err(|error| RestError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
}

/// Whole-vault history and diffs name every changed file, so they need a
/// profile that reads every note.
fn require_unrestricted_read(guard: &ProfilePermissionGuard) -> Result<(), RestError> {
    if guard.read_filter().path_permission().is_unrestricted() {
        Ok(())
    } else {
        Err(RestError::new(
            StatusCode::FORBIDDEN,
            "whole-vault history and diffs need a permission profile that reads every note; ask for one file instead",
        ))
    }
}

/// `GET /{id}/doctor`: the vault's diagnostics, as `vulcan doctor` reports
/// them; it never repairs anything. The report names every note it flags,
/// so it needs a profile that reads every note without a policy hook.
async fn doctor(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<Json<Value>, RestError> {
    read_vault(&state, &principal, &id, move |vault, guard| {
        if !guard.read_filter().path_permission().is_unrestricted() || guard.has_policy_hook() {
            return Err(RestError::new(
                StatusCode::FORBIDDEN,
                "the vault's diagnostics name every note they flag, so they need a permission profile that reads every note",
            ));
        }
        vulcan_app::browse::doctor_vault(&vault.paths)
            .map_err(|error| RestError::from_message(error.to_string()))
    })
    .await
}

fn require_readable(guard: &ProfilePermissionGuard, path: &str) -> Result<(), RestError> {
    guard
        .check_read_path(path)
        .map_err(|error| RestError::new(StatusCode::FORBIDDEN, error.to_string()))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    limit: Option<usize>,
    /// For one file: return its content at this commit instead of its log.
    at: Option<String>,
}

fn history_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(20).clamp(1, MAX_HISTORY_LIMIT)
}

/// `GET /{id}/history`: recent commits, as `vulcan git log` lists them.
async fn history(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    query: Result<Query<HistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, RestError> {
    let Query(query) =
        query.map_err(|error| RestError::new(StatusCode::BAD_REQUEST, error.body_text()))?;
    if query.at.is_some() {
        return Err(RestError::new(
            StatusCode::BAD_REQUEST,
            "`at` names a commit of one file: use /{id}/history/{path}?at=<commit>",
        ));
    }
    let limit = history_limit(query.limit);
    read_git(&state, &principal, &id, move |paths, guard| {
        require_unrestricted_read(guard)?;
        let entries = vulcan_core::git_recent_log(paths.vault_root(), limit)
            .map_err(|error| RestError::from_message(error.to_string()))?;
        Ok(json!({ "limit": limit, "entries": entries }))
    })
    .await
}

/// `GET /{id}/history/{path}`: one file's commits, or with `at`, its content
/// at that commit.
async fn file_history(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path((id, path)): Path<(String, String)>,
    query: Result<Query<HistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, RestError> {
    let Query(query) =
        query.map_err(|error| RestError::new(StatusCode::BAD_REQUEST, error.body_text()))?;
    let path = vault_relative(&path)?;
    let limit = history_limit(query.limit);
    read_git(&state, &principal, &id, move |paths, guard| {
        require_readable(guard, &path)?;
        if let Some(revision) = query.at {
            let content = vulcan_core::git_show_file_at(paths.vault_root(), &path, &revision)
                .map_err(|error| RestError::from_message(error.to_string()))?;
            return Ok(json!({ "path": path, "revision": revision, "content": content }));
        }
        let entries = vulcan_core::git_log(paths.vault_root(), &path, limit)
            .map_err(|error| RestError::from_message(error.to_string()))?;
        Ok(json!({ "path": path, "limit": limit, "entries": entries }))
    })
    .await
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffQuery {
    path: Option<String>,
}

/// `GET /{id}/diff`: uncommitted changes, as `vulcan git diff` shows them.
async fn diff(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    query: Result<Query<DiffQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, RestError> {
    let Query(query) =
        query.map_err(|error| RestError::new(StatusCode::BAD_REQUEST, error.body_text()))?;
    let path = query.path.as_deref().map(vault_relative).transpose()?;
    read_git(&state, &principal, &id, move |paths, guard| {
        match &path {
            Some(path) => require_readable(guard, path)?,
            None => require_unrestricted_read(guard)?,
        }
        let diff = vulcan_core::git_diff(paths.vault_root(), path.as_deref())
            .map_err(|error| RestError::from_message(error.to_string()))?;
        Ok(json!({ "path": path, "diff": diff }))
    })
    .await
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionRangeQuery {
    from: Option<String>,
    to: Option<String>,
}

/// `GET /{id}/diff/{path}?from=<commit>&to=<commit>`: one file between two
/// commits, or from `from` to the working tree when `to` is absent.
async fn file_diff(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path((id, path)): Path<(String, String)>,
    query: Result<Query<RevisionRangeQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, RestError> {
    let Query(query) =
        query.map_err(|error| RestError::new(StatusCode::BAD_REQUEST, error.body_text()))?;
    let path = vault_relative(&path)?;
    let Some(from) = query.from else {
        return Err(RestError::new(
            StatusCode::BAD_REQUEST,
            "a file diff needs `from` (and optionally `to`) commits; use /{id}/diff?path= for uncommitted changes",
        ));
    };
    let to = query.to;
    read_git(&state, &principal, &id, move |paths, guard| {
        require_readable(guard, &path)?;
        let diff = vulcan_core::git_diff_revisions(paths.vault_root(), &path, &from, to.as_deref())
            .map_err(|error| RestError::from_message(error.to_string()))?;
        Ok(json!({ "path": path, "from": from, "to": to, "diff": diff }))
    })
    .await
}

/// `GET /health`: whether the daemon serves each vault the principal sees.
async fn health(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, RestError> {
    let registry = Arc::clone(&state.registry);
    let routed = state.routed_reads.clone();
    let watcher_health = state.watcher_health.clone();
    let vaults = tokio::task::spawn_blocking(move || {
        registry.poll().map(|config| {
            config
                .vaults
                .into_iter()
                .filter(|wiki| principal.sees_wiki(wiki.id.as_str()))
                .map(|wiki| {
                    vault_health_report(wiki.id.as_str(), routed.as_ref(), watcher_health.as_ref())
                })
                .collect::<Vec<_>>()
        })
    })
    .await
    .map_err(|error| RestError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
    .map_err(|error| RestError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let healthy = vaults
        .iter()
        .all(|vault| vault["healthy"].as_bool().unwrap_or(false));
    Ok(Json(json!({
        "ok": true,
        "healthy": healthy,
        "protocol_version": crate::companion::COMPANION_PROTOCOL_VERSION,
        "vaults": vaults,
    })))
}

/// One vault's health: whether the daemon serves it, its cache state and
/// last scan error, and each observer and consumer with its last exit.
fn vault_health_report(
    id: &str,
    routed: Option<&crate::routed_reads::RoutedReadRegistry>,
    watcher_health: Option<&crate::watcher_health::WatcherHealthRegistry>,
) -> Value {
    let served = routed.and_then(|routed| routed.get(id));
    let watcher = watcher_health.and_then(|health| health.get(id));
    let cache = served.as_ref().map(|vault| vault.tracker.status());
    let cache_ok = cache.as_ref().is_none_or(|cache| {
        cache.error.is_none()
            && !matches!(cache.state, crate::scan_runtime::CacheFreshnessState::Error)
    });
    json!({
        "id": id,
        "served": served.is_some(),
        "healthy": cache_ok && watcher.as_ref().is_some_and(crate::watcher_health::VaultWatcherHealth::healthy),
        "cache": cache,
        "watcher": watcher,
    })
}

/// `GET /{id}/health`: one vault's watcher and cache health.
async fn vault_health(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<Json<Value>, RestError> {
    if !principal.sees_wiki(&id) {
        return Err(RestError::new(
            StatusCode::NOT_FOUND,
            format!("unknown vault `{id}`"),
        ));
    }
    let registered = state
        .registry
        .poll()
        .map(|config| config.vaults.iter().any(|wiki| wiki.id.as_str() == id))
        .map_err(|error| RestError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    if !registered {
        return Err(RestError::new(
            StatusCode::NOT_FOUND,
            format!("unknown vault `{id}`"),
        ));
    }
    Ok(Json(vault_health_report(
        &id,
        state.routed_reads.as_ref(),
        state.watcher_health.as_ref(),
    )))
}

/// The mounted `vulcan serve` routes, documented from their own query
/// schemas.
fn serve_route_documents(id_parameter: &Value) -> serde_json::Map<String, Value> {
    let mut paths = serde_json::Map::new();
    for capability in vulcan_app::serve::serve_route_capabilities() {
        if matches!(capability.path, "/" | "/health" | "/graph/stats") {
            continue;
        }
        let mut parameters = vec![id_parameter.clone()];
        let required = capability.query_schema["required"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(properties) = capability.query_schema["properties"].as_object() {
            for (name, schema) in properties {
                parameters.push(json!({
                    "name": name, "in": "query",
                    "required": required.iter().any(|value| value == name),
                    "schema": schema,
                }));
            }
        }
        paths.insert(
            format!("/{{id}}{}", capability.path),
            json!({ "get": {
                "parameters": parameters,
                "responses": { "200": { "description": "The CLI's JSON report" } }
            }}),
        );
    }
    paths
}

/// The routes this module adds beside the mounted serve routes, with their
/// summaries and parameters other than `id`.
fn native_route_documents(path_parameter: &Value) -> Vec<(&'static str, &'static str, Vec<Value>)> {
    vec![
        (
            "/{id}/notes/{path}",
            "One note, as `vulcan note get` reads it",
            vec![path_parameter.clone()],
        ),
        (
            "/{id}/info/{path}",
            "A note's metadata, as `vulcan note info` reports it",
            vec![path_parameter.clone()],
        ),
        (
            "/{id}/links/{path}",
            "Outgoing links of a note",
            vec![path_parameter.clone()],
        ),
        (
            "/{id}/backlinks/{path}",
            "Incoming links of a note",
            vec![path_parameter.clone()],
        ),
        (
            "/{id}/doctor",
            "Vault diagnostics, as `vulcan doctor` reports them (needs a profile that reads every note)",
            vec![],
        ),
        ("/{id}/graph/stats", "Graph analytics", vec![]),
        ("/{id}/graph/hubs", "The most connected notes", vec![]),
        ("/{id}/graph/dead-ends", "Notes without outgoing links", vec![]),
        (
            "/{id}/graph/components",
            "Connected components of the link graph",
            vec![],
        ),
        (
            "/{id}/history",
            "Recent commits, as `vulcan git log` lists them (needs a profile that reads every note)",
            vec![json!({"name": "limit", "in": "query", "required": false, "schema": {"type": "integer", "minimum": 1, "maximum": MAX_HISTORY_LIMIT}})],
        ),
        (
            "/{id}/history/{path}",
            "One file's commits, or with `at`, its content at that commit",
            vec![
                json!({"name": "path", "in": "path", "required": true, "schema": {"type": "string"}}),
                json!({"name": "limit", "in": "query", "required": false, "schema": {"type": "integer", "minimum": 1, "maximum": MAX_HISTORY_LIMIT}}),
                json!({"name": "at", "in": "query", "required": false, "schema": {"type": "string", "pattern": "^[0-9a-fA-F]{4,64}$"}}),
            ],
        ),
        (
            "/{id}/diff",
            "Uncommitted changes, for one file or (with a profile that reads every note) the vault",
            vec![json!({"name": "path", "in": "query", "required": false, "schema": {"type": "string"}})],
        ),
        (
            "/{id}/diff/{path}",
            "One file's diff between two commits, or from a commit to the working tree",
            vec![
                json!({"name": "path", "in": "path", "required": true, "schema": {"type": "string"}}),
                json!({"name": "from", "in": "query", "required": true, "schema": {"type": "string", "pattern": "^[0-9a-fA-F]{4,64}$"}}),
                json!({"name": "to", "in": "query", "required": false, "schema": {"type": "string", "pattern": "^[0-9a-fA-F]{4,64}$"}}),
            ],
        ),
        (
            "/{id}/graph/path",
            "Shortest link path between two notes",
            vec![
                json!({"name": "from", "in": "query", "required": true, "schema": {"type": "string"}}),
                json!({"name": "to", "in": "query", "required": true, "schema": {"type": "string"}}),
            ],
        ),
    ]
}

/// The write operations, each requiring an `Idempotency-Key`.
fn add_write_documents(paths: &mut serde_json::Map<String, Value>, id_parameter: &Value) {
    let key = json!({
        "name": "Idempotency-Key", "in": "header", "required": true,
        "schema": { "type": "string", "minLength": 1, "maxLength": 128 },
        "description": "Retrying with the same key and request returns the original response; reusing it for another request is refused"
    });
    let path_parameter = json!({
        "name": "path", "in": "path", "required": true, "schema": { "type": "string" }
    });
    for (route, method, summary, with_path) in [
        (
            "/{id}/notes",
            "post",
            "Create a note (`vulcan note create`): `path`, `body`, `template`, `frontmatter`",
            false,
        ),
        (
            "/{id}/notes/{path}",
            "put",
            "Replace a note's content (`vulcan note set`): `content`, `preserve_frontmatter`",
            true,
        ),
        (
            "/{id}/notes/{path}",
            "patch",
            "Find and replace in a note (`vulcan note patch`): `find`, `replace`, `all`, selectors",
            true,
        ),
        (
            "/{id}/notes/{path}",
            "delete",
            "Delete a note (`vulcan note delete`); `dry_run` previews",
            true,
        ),
        (
            "/{id}/append/{path}",
            "post",
            "Append to a note (`vulcan note append`): `text`, `mode`, `heading`",
            true,
        ),
        (
            "/{id}/inbox",
            "post",
            "Quick capture (`vulcan inbox`): `text`",
            false,
        ),
        (
            "/{id}/update",
            "post",
            "Set or remove properties on matching notes: `where`, `set`, `unset`, `dry_run`",
            false,
        ),
        (
            "/{id}/move",
            "post",
            "Move a note and rewrite its links: `source`, `destination`, `dry_run`",
            false,
        ),
        ("/{id}/scan", "post", "Rescan the vault: `full`", false),
    ] {
        let mut parameters = vec![id_parameter.clone(), key.clone()];
        if with_path {
            parameters.push(path_parameter.clone());
        }
        let operation = json!({
            "summary": summary,
            "parameters": parameters,
            "requestBody": { "content": { "application/json": { "schema": { "type": "object" } } } },
            "responses": {
                "200": { "description": "The CLI's JSON report" },
                "409": { "description": "The key was used for another request, or an earlier outcome is unknown" }
            }
        });
        let entry = paths.entry(route.to_string()).or_insert_with(|| json!({}));
        entry[method] = operation;
    }
}

/// `GET /openapi.json`: the REST routes as an `OpenAPI` 3.1 document.
async fn openapi() -> Json<Value> {
    Json(openapi_document())
}

/// The `OpenAPI` document: the per-vault serve routes with their query
/// schemas, plus the routes this module adds.
#[must_use]
pub fn openapi_document() -> Value {
    let id_parameter = json!({
        "name": "id", "in": "path", "required": true,
        "schema": { "type": "string" }, "description": "Registered wiki ID"
    });
    let path_parameter = json!({
        "name": "path", "in": "path", "required": true,
        "schema": { "type": "string" }, "description": "Vault-relative note path; may contain `/`"
    });
    let mut paths = serve_route_documents(&id_parameter);
    let native = native_route_documents(&path_parameter);
    for (route, summary, extra) in native {
        let mut parameters = vec![id_parameter.clone()];
        parameters.extend(extra);
        paths.insert(
            route.to_string(),
            json!({ "get": {
                "summary": summary,
                "parameters": parameters,
                "responses": { "200": { "description": "The CLI's JSON report" } }
            }}),
        );
    }
    add_write_documents(&mut paths, &id_parameter);
    for (route, summary) in [
        ("/health", "Daemon health and the vaults it serves"),
        ("/vaults", "Registered vaults visible to the caller"),
        ("/capabilities", "Protocol version and features"),
    ] {
        paths.insert(
            route.to_string(),
            json!({ "get": {
                "summary": summary,
                "responses": { "200": { "description": "JSON report" } }
            }}),
        );
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Vulcan daemon REST API",
            "version": crate::companion::COMPANION_PROTOCOL_VERSION.to_string(),
            "description": "Every request carries `Vulcan-Protocol-Version: 1` and a bearer credential: the companion credential, or an API token from `vulcan vault token create`. Each request runs under the registration's permission profile, which filters what reads return and denies operations it does not allow."
        },
        "servers": [{ "url": "http://127.0.0.1:3210" }],
        "components": { "securitySchemes": { "bearer": { "type": "http", "scheme": "bearer" } } },
        "security": [{ "bearer": [] }],
        "paths": paths,
    })
}

/// Whether a vault principal for `wiki_id` may call `method path`. Sync
/// administration, routed `ops`, jobs, events, and shutdown stay device-only,
/// and another vault's routes are out of reach.
#[must_use]
pub fn vault_principal_reaches(method: &Method, path: &str, wiki_id: &str) -> bool {
    if VAULT_PRINCIPAL_DAEMON_ROUTES.contains(&path) {
        return method == Method::GET;
    }
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    let Some((id, route)) = rest.split_once('/') else {
        return false;
    };
    let segment = route.split('/').next().unwrap_or_default();
    id == wiki_id && REST_VAULT_SEGMENTS.contains(&segment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AddWikiRequest, WikiId, WikiRegistry};
    use axum::body::Body;
    use axum::http::header::AUTHORIZATION;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    struct Served {
        temporary: tempfile::TempDir,
        state: CompanionHttpState,
        _watchers: Vec<crate::runtime::TestWatcher>,
        token: String,
    }

    /// Two vaults served by real watchers: `notes` unrestricted and
    /// `projects` registered under a profile that reads only `Projects/`.
    fn served() -> Served {
        let temporary = tempfile::tempdir().unwrap();
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let routed = crate::routed_reads::RoutedReadRegistry::default();
        let health = crate::watcher_health::WatcherHealthRegistry::default();
        let mut watchers = Vec::new();
        for (id, profile) in [("notes", None), ("projects", Some("projects_only"))] {
            let vault = temporary.path().join(id);
            std::fs::create_dir_all(vault.join("Projects")).unwrap();
            std::fs::create_dir_all(vault.join("Private")).unwrap();
            std::fs::create_dir_all(vault.join(".vulcan")).unwrap();
            std::fs::write(
                vault.join(".vulcan/config.local.toml"),
                "[permissions.profiles.projects_only]\nread = { allow = [\"folder:Projects/**\"] }\ngit = \"allow\"\n\n[permissions.profiles.no_git]\nread = \"all\"\n",
            )
            .unwrap();
            std::fs::write(
                vault.join("Projects/Plan.md"),
                "# Plan\n\nThe dashboard links [[Projects/Goals]].\n",
            )
            .unwrap();
            std::fs::write(
                vault.join("Projects/Goals.md"),
                "# Goals\n\nA dashboard goal.\n",
            )
            .unwrap();
            std::fs::write(
                vault.join("Private/Diary.md"),
                "# Diary\n\nA private dashboard, see [[Projects/Plan]].\n",
            )
            .unwrap();
            let registration = registry
                .add(
                    &AddWikiRequest {
                        profile: None,
                        id: WikiId::parse(id).unwrap(),
                        path: vault,
                        groups: vec![],
                        git_dir: None,
                        permissions_profile: profile.map(str::to_string),
                        sync_backend: Some("none".to_string()),
                        platform_profile: None,
                    },
                    false,
                )
                .unwrap();
            watchers.push(crate::runtime::watch_for_tests(
                registration,
                &temporary.path().join(format!("state-{id}")),
                &routed,
                &health,
            ));
        }
        // Requests start once each vault's first scan is done, as they would
        // against a daemon that has finished starting; slow runners take a
        // while to get there.
        for id in ["notes", "projects"] {
            routed
                .get(id)
                .expect("served vault")
                .tracker
                .wait_for_generation(1, std::time::Duration::from_secs(60))
                .expect("first scan");
        }
        let api_tokens = Arc::new(crate::api_tokens::ApiTokenStore::at(
            temporary.path().join("api-state"),
        ));
        let projects = registry.show(&WikiId::parse("projects").unwrap()).unwrap();
        let token = api_tokens
            .create(
                projects.registration.registration_id,
                "projects",
                None,
                false,
            )
            .unwrap()
            .token
            .unwrap();
        let state = CompanionHttpState {
            registry: Arc::new(registry),
            supervisor: Arc::new(
                crate::supervisor::SyncSupervisor::at(temporary.path().join("jobs.json")).unwrap(),
            ),
            state_store: Arc::new(vulcan_app::sync_state::SyncStateStore::at(
                temporary.path().join("sync-state"),
            )),
            credential: Arc::new(
                crate::credentials::CompanionCredential::generate(vec![]).unwrap(),
            ),
            resolution_agent: None,
            semantic_agent: None,
            shutdown: None,
            ingress_shutdown: None,
            routed_reads: Some(routed),
            watcher_health: Some(health),
            api_tokens: Some(api_tokens),
            write_gate: None,
            idempotency: Some(Arc::new(crate::rest_idempotency::IdempotencyStore::at(
                temporary.path().join("rest-state"),
            ))),
        };
        Served {
            temporary,
            state,
            _watchers: watchers,
            token,
        }
    }

    async fn get(served: &Served, bearer: &str, uri: &str) -> (StatusCode, Value) {
        let response = crate::http::companion_router(served.state.clone())
            .oneshot(
                HttpRequest::builder()
                    .uri(uri)
                    .header(AUTHORIZATION, format!("Bearer {bearer}"))
                    .header(crate::http::PROTOCOL_VERSION_HEADER, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    fn device(served: &Served) -> String {
        served.state.credential.token.clone()
    }

    fn hit_paths(report: &Value) -> Vec<String> {
        let mut paths = report["hits"]
            .as_array()
            .expect("search hits")
            .iter()
            .map(|hit| hit["document_path"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rest_reads_answer_like_the_cli_and_apply_the_registration_profile() {
        let served = served();
        let device = device(&served);

        let (status, all) = get(&served, &device, "/notes/search?q=dashboard").await;
        assert_eq!(status, StatusCode::OK, "{all}");
        assert_eq!(
            hit_paths(&all),
            ["Private/Diary.md", "Projects/Goals.md", "Projects/Plan.md"]
        );
        // The same search through the restricted registration sees only
        // `Projects/`, for the token and for the device alike.
        for bearer in [&served.token, &device] {
            let (status, restricted) = get(&served, bearer, "/projects/search?q=dashboard").await;
            assert_eq!(status, StatusCode::OK, "{restricted}");
            assert_eq!(
                hit_paths(&restricted),
                ["Projects/Goals.md", "Projects/Plan.md"]
            );
        }

        let (status, note) = get(&served, &device, "/notes/notes/Projects/Plan.md").await;
        assert_eq!(status, StatusCode::OK, "{note}");
        assert_eq!(note["path"], "Projects/Plan.md");
        assert!(note["content"].as_str().unwrap().contains("dashboard"));
        let (status, denied) =
            get(&served, &served.token, "/projects/notes/Private/Diary.md").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
        let (status, _) = get(&served, &device, "/notes/notes/../outside.md").await;
        assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND);

        let (status, backlinks) = get(&served, &device, "/notes/backlinks/Projects/Plan.md").await;
        assert_eq!(status, StatusCode::OK, "{backlinks}");
        assert_eq!(backlinks["backlinks"].as_array().unwrap().len(), 1);
        let (status, filtered) = get(
            &served,
            &served.token,
            "/projects/backlinks/Projects/Plan.md",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{filtered}");
        assert!(filtered["backlinks"].as_array().unwrap().is_empty());
        let (status, links) = get(&served, &device, "/notes/links/Projects/Plan.md").await;
        assert_eq!(status, StatusCode::OK, "{links}");
        assert_eq!(links["links"].as_array().unwrap().len(), 1);

        let (status, graph_stats) = get(&served, &device, "/notes/graph/stats").await;
        assert_eq!(status, StatusCode::OK, "{graph_stats}");
        assert_eq!(graph_stats["note_count"], 3);
        let (status, path) = get(
            &served,
            &device,
            "/notes/graph/path?from=Private/Diary.md&to=Projects/Goals.md",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let (status, _) = get(&served, &device, "/notes/graph/path?from=A").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, query) = get(&served, &device, "/notes/notes?limit=10").await;
        assert_eq!(status, StatusCode::OK, "{query}");
        let (status, missing) = get(&served, &device, "/missing/search?q=x").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    }

    fn git(root: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout).unwrap()
    }

    fn commit_vault(root: &std::path::Path) -> String {
        git(root, &["-c", "init.defaultBranch=main", "init", "-q"]);
        git(root, &["config", "user.name", "Vulcan Test"]);
        git(root, &["config", "user.email", "vulcan@example.com"]);
        std::fs::write(root.join(".gitignore"), ".vulcan/\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "Initial notes"]);
        git(root, &["rev-parse", "HEAD"]).trim().to_string()
    }

    /// `doctor` answers as `vulcan doctor` does for a profile that reads
    /// every note, refuses a narrower one, and is part of a vault token's
    /// reach; capabilities list it.
    #[tokio::test(flavor = "multi_thread")]
    async fn doctor_reports_diagnostics_only_to_a_profile_that_reads_everything() {
        let served = served();
        let device = device(&served);
        let root = served.temporary.path();
        std::fs::write(root.join("notes/Broken.md"), "[[Nowhere]]\n").unwrap();

        let (status, report) = get(&served, &device, "/notes/doctor").await;
        assert_eq!(status, StatusCode::OK, "{report}");
        let direct =
            vulcan_app::browse::doctor_vault(&vulcan_core::VaultPaths::new(root.join("notes")))
                .unwrap();
        assert_eq!(report, json!(direct));
        assert!(
            report["unresolved_links"]
                .as_array()
                .is_some_and(|links| !links.is_empty()),
            "{report}"
        );

        // `projects` reads only `Projects/`: the device and its own token
        // are both refused, since the report would name every note.
        for bearer in [device.as_str(), served.token.as_str()] {
            let (status, denied) = get(&served, bearer, "/projects/doctor").await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
            assert!(denied["detail"]
                .as_str()
                .unwrap()
                .contains("reads every note"));
        }

        let (status, capabilities) = get(&served, &device, "/capabilities").await;
        assert_eq!(status, StatusCode::OK);
        let groups = capabilities["rest"]["vault_route_groups"]
            .as_array()
            .expect("REST route groups");
        assert!(groups.contains(&json!("doctor")) && groups.contains(&json!("notes")));
        assert_eq!(capabilities["rest"]["openapi"], "/openapi.json");
        let (_, openapi) = get(&served, &device, "/openapi.json").await;
        assert!(openapi["paths"]["/{id}/doctor"]["get"].is_object());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn history_routes_read_git_under_the_registration_profile() {
        let served = served();
        let device = device(&served);
        let root = served.temporary.path();
        let notes_head = commit_vault(&root.join("notes"));
        let projects_head = commit_vault(&root.join("projects"));
        std::fs::write(
            root.join("projects/Projects/Plan.md"),
            "# Plan\n\nRevised.\n",
        )
        .unwrap();

        let (status, log) = get(&served, &device, "/notes/history?limit=5").await;
        assert_eq!(status, StatusCode::OK, "{log}");
        assert_eq!(log["entries"][0]["summary"], "Initial notes");
        assert_eq!(log["entries"][0]["commit"], notes_head.as_str());

        // The restricted registration cannot list the whole vault's history
        // or its uncommitted changes, but can read a file it may read.
        for uri in ["/projects/history", "/projects/diff"] {
            let (status, denied) = get(&served, &served.token, uri).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {denied}");
            assert!(denied["detail"]
                .as_str()
                .unwrap()
                .contains("reads every note"));
        }
        let (status, file_log) =
            get(&served, &served.token, "/projects/history/Projects/Plan.md").await;
        assert_eq!(status, StatusCode::OK, "{file_log}");
        assert_eq!(file_log["entries"].as_array().unwrap().len(), 1);
        let (status, at) = get(
            &served,
            &served.token,
            &format!("/projects/history/Projects/Plan.md?at={projects_head}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{at}");
        assert!(at["content"]
            .as_str()
            .unwrap()
            .contains("The dashboard links"));
        let (status, _) = get(&served, &served.token, "/projects/history/Private/Diary.md").await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, uncommitted) = get(
            &served,
            &served.token,
            "/projects/diff?path=Projects/Plan.md",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{uncommitted}");
        assert!(uncommitted["diff"].as_str().unwrap().contains("+Revised."));
        let (status, range) = get(
            &served,
            &served.token,
            &format!("/projects/diff/Projects/Plan.md?from={projects_head}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{range}");
        assert!(range["diff"].as_str().unwrap().contains("+Revised."));

        // A profile without Git access gets no history at all.
        served
            .state
            .registry
            .update(
                &WikiId::parse("notes").unwrap(),
                &crate::registry::UpdateWikiRequest {
                    profile: None,
                    groups_to_add: vec![],
                    groups_to_remove: vec![],
                    permissions_profile: Some(Some("no_git".to_string())),
                    sync_paused: None,
                    execution_trust: None,
                },
                false,
            )
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let (status, denied) = get(&served, &device, "/notes/history").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
        assert!(denied["detail"].as_str().unwrap().contains("git"));

        for uri in [
            "/projects/history/Projects/Plan.md?at=HEAD",
            "/projects/diff/Projects/Plan.md",
            "/projects/diff/Projects/Plan.md?from=--output=x",
            "/notes/history?at=abcd",
        ] {
            let (status, invalid) = get(&served, &served.token, uri).await;
            assert!(
                status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
                "{uri}: {status} {invalid}"
            );
        }
    }

    async fn send(
        served: &Served,
        bearer: &str,
        method: Method,
        uri: &str,
        key: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut request = HttpRequest::builder()
            .method(method)
            .uri(uri)
            .header(AUTHORIZATION, format!("Bearer {bearer}"))
            .header(crate::http::PROTOCOL_VERSION_HEADER, "1")
            .header(axum::http::header::CONTENT_TYPE, "application/json");
        if let Some(key) = key {
            request = request.header(crate::http::IDEMPOTENCY_KEY_HEADER, key);
        }
        let response = crate::http::companion_router(served.state.clone())
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rest_writes_need_a_key_and_replay_it_without_writing_twice() {
        let served = served();
        let device = device(&served);
        let root = served.temporary.path().join("notes");
        let create = json!({ "path": "Inbox/Idea", "body": "A fresh idea.\n" });

        let (status, missing_key) = send(
            &served,
            &device,
            Method::POST,
            "/notes/notes",
            None,
            create.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{missing_key}");
        let (status, created) = send(
            &served,
            &device,
            Method::POST,
            "/notes/notes",
            Some("create-1"),
            create.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert_eq!(created["path"], "Inbox/Idea.md");
        assert!(std::fs::read_to_string(root.join("Inbox/Idea.md"))
            .unwrap()
            .contains("A fresh idea."));
        // A retry with the same key replays the answer without creating twice;
        // the same key for a different request is refused.
        let (status, replayed) = send(
            &served,
            &device,
            Method::POST,
            "/notes/notes",
            Some("create-1"),
            create.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replayed, created);
        let (status, reused) = send(
            &served,
            &device,
            Method::POST,
            "/notes/notes",
            Some("create-1"),
            json!({ "path": "Inbox/Other" }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{reused}");
        assert!(!root.join("Inbox/Other.md").exists());

        // The restricted registration may not write.
        let (status, denied) = send(
            &served,
            &served.token,
            Method::POST,
            "/projects/notes",
            Some("create-2"),
            json!({ "path": "Projects/New" }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
        assert!(!served
            .temporary
            .path()
            .join("projects/Projects/New.md")
            .exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rest_writes_run_the_cli_workflows_and_reads_see_them() {
        let served = served();
        let device = device(&served);
        let root = served.temporary.path().join("notes");
        let (status, created) = send(
            &served,
            &device,
            Method::POST,
            "/notes/notes",
            Some("create-1"),
            json!({ "path": "Inbox/Idea", "body": "A fresh idea.\n" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let (status, set) = send(
            &served,
            &device,
            Method::PUT,
            "/notes/notes/Inbox/Idea.md",
            Some("set-1"),
            json!({ "content": "# Idea\n\nRevised idea.\n" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{set}");
        let (status, patched) = send(
            &served,
            &device,
            Method::PATCH,
            "/notes/notes/Inbox/Idea.md",
            Some("patch-1"),
            json!({ "find": "Revised", "replace": "Patched" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{patched}");
        let (status, appended) = send(
            &served,
            &device,
            Method::POST,
            "/notes/append/Inbox/Idea.md",
            Some("append-1"),
            json!({ "text": "One more line." }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{appended}");
        let idea = std::fs::read_to_string(root.join("Inbox/Idea.md")).unwrap();
        assert!(
            idea.contains("Patched idea.") && idea.contains("One more line."),
            "{idea}"
        );
        let (status, route_note) = send(
            &served,
            &device,
            Method::PATCH,
            "/notes/notes/Inbox/Idea.md",
            Some("patch-2"),
            json!({ "note": "Projects/Plan.md", "find": "a", "replace": "b" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{route_note}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rest_moves_captures_deletes_and_scans_reach_the_index() {
        let served = served();
        let device = device(&served);
        let root = served.temporary.path().join("notes");
        // A move rewrites the links to the note.
        let (status, moved) = send(
            &served,
            &device,
            Method::POST,
            "/notes/move",
            Some("move-1"),
            json!({ "source": "Projects/Goals.md", "destination": "Projects/Archive/Goals.md" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{moved}");
        assert!(root.join("Projects/Archive/Goals.md").exists());
        let plan = std::fs::read_to_string(root.join("Projects/Plan.md")).unwrap();
        // The link is rewritten in the vault's link style: the shortest
        // unique form of the new location.
        assert!(
            !plan.contains("[[Projects/Goals]]") && plan.contains("[[Goals]]"),
            "{plan}"
        );
        assert_eq!(moved["rewritten_files"][0]["path"], "Projects/Plan.md");

        let (status, captured) = send(
            &served,
            &device,
            Method::POST,
            "/notes/inbox",
            Some("inbox-1"),
            json!({ "text": "Call the plumber" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{captured}");
        let inbox = captured["path"].as_str().unwrap();
        assert!(std::fs::read_to_string(root.join(inbox))
            .unwrap()
            .contains("Call the plumber"));

        let (status, deleted) = send(
            &served,
            &device,
            Method::DELETE,
            "/notes/notes/Projects/Archive/Goals.md",
            Some("delete-1"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{deleted}");
        assert!(!root.join("Projects/Archive/Goals.md").exists());

        let (status, scanned) = send(
            &served,
            &device,
            Method::POST,
            "/notes/scan",
            Some("scan-1"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{scanned}");
        // Reads see the writes.
        let (status, found) = get(&served, &device, "/notes/search?q=plumber").await;
        assert_eq!(status, StatusCode::OK, "{found}");
        assert_eq!(hit_paths(&found), [inbox.to_string()]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bulk_updates_and_graph_reports_answer_like_the_cli() {
        let served = served();
        let device = device(&served);
        let root = served.temporary.path().join("notes");
        for (path, status) in [
            ("Tasks/One", "draft"),
            ("Tasks/Two", "draft"),
            ("Tasks/Three", "done"),
        ] {
            let (status_code, created) = send(
                &served,
                &device,
                Method::POST,
                "/notes/notes",
                Some(&format!("create-{path}")),
                json!({ "path": path, "frontmatter": { "status": status, "priority": "high" } }),
            )
            .await;
            assert_eq!(status_code, StatusCode::OK, "{created}");
        }
        let (status, missing_filter) = send(
            &served,
            &device,
            Method::POST,
            "/notes/update",
            Some("update-0"),
            json!({ "where": [], "set": { "status": "done" } }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{missing_filter}");
        let (status, updated) = send(
            &served,
            &device,
            Method::POST,
            "/notes/update",
            Some("update-1"),
            json!({ "where": ["status = draft"], "set": { "status": "done" }, "unset": ["priority"] }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert_eq!(updated["changes"][0]["files"].as_array().unwrap().len(), 2);
        let one = std::fs::read_to_string(root.join("Tasks/One.md")).unwrap();
        assert!(
            one.contains("status: done") && !one.contains("priority"),
            "{one}"
        );
        let three = std::fs::read_to_string(root.join("Tasks/Three.md")).unwrap();
        assert!(
            three.contains("priority"),
            "the unmatched note keeps its properties"
        );
        let (status, denied) = send(
            &served,
            &served.token,
            Method::POST,
            "/projects/update",
            Some("update-2"),
            json!({ "where": ["file.name = Plan"], "set": { "status": "done" } }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");

        let (status, dead_ends) = get(&served, &device, "/notes/graph/dead-ends").await;
        assert_eq!(status, StatusCode::OK, "{dead_ends}");
        let direct = vulcan_core::query_graph_dead_ends_with_filter(
            &vulcan_core::VaultPaths::new(&root),
            None,
        )
        .unwrap();
        assert_eq!(dead_ends, json!(direct));
        let (status, components) = get(&served, &device, "/notes/graph/components").await;
        assert_eq!(status, StatusCode::OK, "{components}");
        let (status, _) = get(&served, &device, "/notes/graph/unknown").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn health_and_openapi_describe_the_rest_surface() {
        let served = served();
        let (status, health) = get(&served, &served.token, "/health").await;
        assert_eq!(status, StatusCode::OK, "{health}");
        let ids = health["vaults"]
            .as_array()
            .unwrap()
            .iter()
            .map(|vault| vault["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["projects"]);
        assert_eq!(health["vaults"][0]["served"], true);
        let components = &health["vaults"][0]["watcher"]["components"];
        for component in ["observer", "index", "auto-commit"] {
            assert_eq!(
                components[component]["running"], true,
                "{component}: {health}"
            );
        }
        assert_eq!(health["vaults"][0]["healthy"], true, "{health}");
        assert_eq!(health["healthy"], true);
        // `/health` reports the cache as it is, without an index barrier, so a
        // watcher event that lands after the first scan shows as `dirty`.
        let cache_state = health["vaults"][0]["cache"]["state"].as_str().unwrap();
        assert!(matches!(cache_state, "fresh" | "dirty"), "{health}");

        let (status, one) = get(&served, &served.token, "/projects/health").await;
        assert_eq!(status, StatusCode::OK, "{one}");
        assert_eq!(one["id"], "projects");
        assert_eq!(one["watcher"]["components"]["index"]["running"], true);
        let (status, _) = get(&served, &served.token, "/notes/health").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = get(&served, &device(&served), "/missing/health").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, document) = get(&served, &served.token, "/openapi.json").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(document["openapi"], "3.1.0");
        for route in [
            "/{id}/search",
            "/{id}/notes",
            "/{id}/notes/{path}",
            "/{id}/backlinks/{path}",
            "/{id}/graph/path",
            "/health",
        ] {
            assert!(document["paths"].get(route).is_some(), "{route}");
        }
        assert_eq!(
            document["components"]["securitySchemes"]["bearer"]["scheme"],
            "bearer"
        );
        for (route, method) in [
            ("/{id}/notes", "post"),
            ("/{id}/notes/{path}", "put"),
            ("/{id}/notes/{path}", "patch"),
            ("/{id}/notes/{path}", "delete"),
            ("/{id}/notes/{path}", "get"),
            ("/{id}/move", "post"),
        ] {
            assert!(
                document["paths"][route].get(method).is_some(),
                "{method} {route}"
            );
        }
        // Every documented per-vault route is one a vault token can reach.
        for route in document["paths"].as_object().unwrap().keys() {
            let concrete = route.replace("{id}", "projects").replace("{path}", "A.md");
            assert!(
                vault_principal_reaches(&Method::GET, &concrete, "projects"),
                "{route}"
            );
        }
    }

    #[test]
    fn a_vault_principal_reaches_only_its_own_rest_routes() {
        let get = Method::GET;
        for path in ["/capabilities", "/health", "/vaults", "/openapi.json"] {
            assert!(vault_principal_reaches(&get, path, "notes"), "{path}");
        }
        assert!(!vault_principal_reaches(&Method::POST, "/vaults", "notes"));
        for path in [
            "/notes/search",
            "/notes/notes/Projects/Plan.md",
            "/notes/graph/stats",
            "/notes/history/Home.md",
        ] {
            assert!(vault_principal_reaches(&get, path, "notes"), "{path}");
        }
        for path in [
            "/other/search",
            "/notes/sync/status",
            "/notes/sync/pause",
            "/notes/ops/query",
            "/jobs/1",
            "/events",
            "/shutdown",
            "/sync",
            "/notes",
        ] {
            assert!(!vault_principal_reaches(&get, path, "notes"), "{path}");
        }
    }

    #[test]
    fn principals_see_their_wikis_and_scope_idempotency_keys() {
        let vault = Principal::Vault {
            wiki_id: "notes".to_string(),
            registration_id: Ulid::new(),
            token_id: Ulid::new(),
        };
        assert!(vault.sees_wiki("notes"));
        assert!(!vault.sees_wiki("other"));
        assert!(Principal::Device.sees_wiki("other"));
        assert_ne!(
            vault.idempotency_scope("companion"),
            Principal::Device.idempotency_scope("companion")
        );
    }
}
