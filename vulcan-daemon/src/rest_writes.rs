//! REST writes (Roadmap 10.3): the shared note, inbox, move, and scan
//! workflows behind the host's mutation gate, each with an idempotency key.
//! See `docs/specs/daemon-rest-api.md`.

use crate::http::{CompanionHttpState, IDEMPOTENCY_KEY_HEADER};
use crate::rest::{vault_relative, Principal, RestError};
use crate::rest_idempotency::{request_fingerprint, IdempotencyBegin};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use vulcan_app::commit::AutoCommitPolicy;
use vulcan_app::execution::{MutationGate, Ungated};
use vulcan_app::mcp_protocol::{
    McpIndexScanArgs, McpMethodError, McpNoteAppendArgs, McpNoteCreateArgs, McpNoteDeleteArgs,
    McpNotePatchArgs, McpNoteSetArgs,
};
use vulcan_core::{PermissionGuard, ProfilePermissionGuard, VaultPaths, Verbosity};

/// What a write workflow receives: the vault, the caller's guard and profile
/// name, and the request body as a JSON object.
struct WriteRequest<'a> {
    paths: &'a VaultPaths,
    guard: &'a ProfilePermissionGuard,
    profile: &'a str,
    body: Map<String, Value>,
}

type WriteWorkflow = Box<dyn FnOnce(WriteRequest<'_>) -> Result<Value, RestError> + Send>;

/// Runs a write for vault `id`: claims the request's idempotency key, waits
/// for the vault's mutation permit, runs `workflow`, and records its outcome
/// so a retry with the same key gets the same response.
#[allow(clippy::too_many_arguments)]
async fn write_vault(
    state: &CompanionHttpState,
    principal: &Principal,
    id: &str,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &Bytes,
    workflow: WriteWorkflow,
) -> Response {
    if !principal.sees_wiki(id) {
        return RestError::new(StatusCode::NOT_FOUND, format!("unknown vault `{id}`"))
            .into_response();
    }
    let Some(store) = state.idempotency.clone() else {
        return RestError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "this daemon does not accept REST writes",
        )
        .into_response();
    };
    let Some(key) = headers
        .get(IDEMPOTENCY_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
    else {
        return RestError::new(
            StatusCode::BAD_REQUEST,
            "writes need an Idempotency-Key header; reuse it only to retry the same request",
        )
        .into_response();
    };
    let scope = format!("{}:{id}", principal.idempotency_scope(&state.credential.id));
    let fingerprint = request_fingerprint(
        method.as_str(),
        uri.path_and_query()
            .map_or(uri.path(), |path| path.as_str()),
        body,
    );
    let registry = Arc::clone(&state.registry);
    let gate = state.write_gate.clone();
    let id = id.to_string();
    let body = body.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let ticket = match store.begin(&scope, &key, &fingerprint) {
            Ok(IdempotencyBegin::Run(ticket)) => ticket,
            Ok(IdempotencyBegin::Replay { status, body }) => return (status, body),
            Ok(IdempotencyBegin::Refused(reason)) => {
                return failure(&RestError::new(StatusCode::CONFLICT, reason));
            }
            Err(reason) => return failure(&RestError::new(StatusCode::BAD_REQUEST, reason)),
        };
        let (status, response) = match run_write(&registry, gate.as_deref(), &id, &body, workflow) {
            Ok(value) => (StatusCode::OK.as_u16(), value),
            Err(error) => failure(&error),
        };
        if let Err(error) = store.finish(&ticket, status, &response) {
            eprintln!("cannot record the outcome of a REST write for `{id}`: {error}");
        }
        (status, response)
    })
    .await;
    match outcome {
        Ok((status, body)) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(body),
        )
            .into_response(),
        Err(error) => RestError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the write's outcome is unknown: {error}"),
        )
        .into_response(),
    }
}

fn failure(error: &RestError) -> (u16, Value) {
    (
        error.status.as_u16(),
        serde_json::to_value(&error.error).unwrap_or(Value::Null),
    )
}

fn run_write(
    registry: &crate::registry::WikiRegistry,
    gate: Option<&crate::worker_gate::HostedWorkerGate>,
    id: &str,
    body: &Bytes,
    workflow: WriteWorkflow,
) -> Result<Value, RestError> {
    let wiki = registry
        .poll()
        .map_err(|error| RestError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .vaults
        .into_iter()
        .find(|wiki| wiki.id.as_str() == id)
        .ok_or_else(|| RestError::new(StatusCode::NOT_FOUND, format!("unknown vault `{id}`")))?;
    let body = if body.is_empty() {
        Map::new()
    } else {
        match serde_json::from_slice::<Value>(body) {
            Ok(Value::Object(body)) => body,
            Ok(_) => {
                return Err(RestError::new(
                    StatusCode::BAD_REQUEST,
                    "the request body must be a JSON object",
                ))
            }
            Err(error) => return Err(RestError::new(StatusCode::BAD_REQUEST, error.to_string())),
        }
    };
    let paths = VaultPaths::new(&wiki.path);
    let profile = wiki
        .permissions_profile
        .clone()
        .unwrap_or_else(|| "unrestricted".to_string());
    let selection = vulcan_core::resolve_device_permission_profile(&paths, Some(&profile))
        .map_err(|error| RestError::from_message(error.to_string()))?;
    let guard = ProfilePermissionGuard::new(&paths, selection);
    let vault_gate = gate.map(|gate| gate.for_file_writes(&wiki));
    let gate: &dyn MutationGate = match vault_gate.as_ref() {
        Some(gate) => gate,
        None => &Ungated,
    };
    let _permit = gate
        .enter()
        .map_err(|error| RestError::new(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    workflow(WriteRequest {
        paths: &paths,
        guard: &guard,
        profile: &profile,
        body,
    })
}

/// Decodes the body into a workflow's arguments, with `note` taken from the
/// route and any `extra` fields the route fixes.
fn arguments<T: DeserializeOwned>(
    mut body: Map<String, Value>,
    fixed: &[(&str, Value)],
) -> Result<T, RestError> {
    for (name, value) in fixed {
        if body.contains_key(*name) {
            return Err(RestError::new(
                StatusCode::BAD_REQUEST,
                format!("`{name}` comes from the route, not the body"),
            ));
        }
        body.insert((*name).to_string(), value.clone());
    }
    serde_json::from_value(Value::Object(body))
        .map_err(|error| RestError::new(StatusCode::BAD_REQUEST, error.to_string()))
}

fn mcp_failure(error: McpMethodError) -> RestError {
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

fn report(value: impl serde::Serialize) -> Result<Value, RestError> {
    serde_json::to_value(value)
        .map_err(|error| RestError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

/// `POST /{id}/notes`: create a note (`vulcan note create`).
pub(crate) async fn create_note(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    write_vault(
        &state,
        &principal,
        &id,
        &method,
        &uri,
        &headers,
        &body,
        Box::new(|request| {
            let args: McpNoteCreateArgs = arguments(request.body, &[])?;
            report(
                vulcan_app::mcp_notes::note_create(
                    request.paths,
                    request.guard,
                    request.profile,
                    args,
                )
                .map_err(mcp_failure)?,
            )
        }),
    )
    .await
}

/// `PUT /{id}/notes/{path}`: replace a note's content (`vulcan note set`).
pub(crate) async fn set_note(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path((id, path)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let note = match vault_relative(&path) {
        Ok(note) => note,
        Err(error) => return error.into_response(),
    };
    write_vault(
        &state,
        &principal,
        &id,
        &method,
        &uri,
        &headers,
        &body,
        Box::new(move |request| {
            // A PUT is an explicit replacement, so it confirms by itself.
            let args: McpNoteSetArgs = arguments(
                request.body,
                &[("note", json!(note)), ("confirm", json!(true))],
            )?;
            report(
                vulcan_app::mcp_notes::note_set(
                    request.paths,
                    request.guard,
                    request.profile,
                    args,
                )
                .map_err(mcp_failure)?,
            )
        }),
    )
    .await
}

/// `PATCH /{id}/notes/{path}`: find and replace in a note (`vulcan note patch`).
pub(crate) async fn patch_note(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path((id, path)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let note = match vault_relative(&path) {
        Ok(note) => note,
        Err(error) => return error.into_response(),
    };
    write_vault(
        &state,
        &principal,
        &id,
        &method,
        &uri,
        &headers,
        &body,
        Box::new(move |request| {
            let args: McpNotePatchArgs = arguments(request.body, &[("note", json!(note))])?;
            report(
                vulcan_app::mcp_notes::note_patch(
                    request.paths,
                    request.guard,
                    request.profile,
                    args,
                )
                .map_err(mcp_failure)?,
            )
        }),
    )
    .await
}

/// `DELETE /{id}/notes/{path}`: delete a note (`vulcan note delete`).
pub(crate) async fn delete_note(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path((id, path)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let note = match vault_relative(&path) {
        Ok(note) => note,
        Err(error) => return error.into_response(),
    };
    write_vault(
        &state,
        &principal,
        &id,
        &method,
        &uri,
        &headers,
        &body,
        Box::new(move |request| {
            // DELETE names its target explicitly, so it confirms by itself.
            let args: McpNoteDeleteArgs = arguments(
                request.body,
                &[("note", json!(note)), ("confirm", json!(true))],
            )?;
            report(
                vulcan_app::mcp_notes::note_delete(
                    request.paths,
                    request.guard,
                    request.profile,
                    args,
                )
                .map_err(mcp_failure)?,
            )
        }),
    )
    .await
}

/// `POST /{id}/append/{path}`: append to a note (`vulcan note append`).
pub(crate) async fn append_note(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path((id, path)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let note = match vault_relative(&path) {
        Ok(note) => note,
        Err(error) => return error.into_response(),
    };
    write_vault(
        &state,
        &principal,
        &id,
        &method,
        &uri,
        &headers,
        &body,
        Box::new(move |request| {
            let args: McpNoteAppendArgs = arguments(request.body, &[("note", json!(note))])?;
            report(
                vulcan_app::mcp_notes::note_append(
                    request.paths,
                    request.guard,
                    request.profile,
                    args,
                )
                .map_err(mcp_failure)?,
            )
        }),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxArgs {
    text: String,
    #[serde(default)]
    no_commit: bool,
}

/// `POST /{id}/inbox`: quick capture (`vulcan inbox`).
pub(crate) async fn inbox(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    write_vault(
        &state,
        &principal,
        &id,
        &method,
        &uri,
        &headers,
        &body,
        Box::new(|request| {
            let args: InboxArgs = arguments(request.body, &[])?;
            if args.text.trim().is_empty() {
                return Err(RestError::new(
                    StatusCode::BAD_REQUEST,
                    "inbox text must not be empty",
                ));
            }
            let path = vulcan_app::inbox::inbox_path(
                &vulcan_core::load_vault_config(request.paths).config.inbox,
            )
            .map_err(|error| RestError::from_message(error.to_string()))?;
            request
                .guard
                .check_write_path(&path)
                .map_err(|error| RestError::new(StatusCode::FORBIDDEN, error.to_string()))?;
            let path = vulcan_app::inbox::write_inbox_capture(
                request.paths,
                &args.text,
                Some(request.profile),
                Verbosity::Quiet,
            )
            .map_err(|error| RestError::from_message(error.to_string()))?;
            finish_mutation(
                request.paths,
                request.profile,
                "inbox",
                std::slice::from_ref(&path),
                args.no_commit,
            )?;
            Ok(json!({ "path": path, "appended": true }))
        }),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveArgs {
    source: String,
    destination: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    no_commit: bool,
}

/// `POST /{id}/move`: move or rename a note and rewrite links to it
/// (`vulcan refactor move`). The profile must let the caller refactor the
/// note and write every file the move changes.
pub(crate) async fn move_note(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    write_vault(
        &state,
        &principal,
        &id,
        &method,
        &uri,
        &headers,
        &body,
        Box::new(|request| {
            let args: MoveArgs = arguments(request.body, &[])?;
            let source = vault_relative(&args.source)?;
            let destination = vault_relative(&args.destination)?;
            let deny = |error: vulcan_core::PermissionError| {
                RestError::new(StatusCode::FORBIDDEN, error.to_string())
            };
            request.guard.check_refactor_path(&source).map_err(deny)?;
            let plan = vulcan_app::browse::move_note_with_profile(
                request.paths,
                &source,
                &destination,
                true,
                Some(request.profile),
            )
            .map_err(|error| RestError::from_message(error.to_string()))?;
            let mut changed = vec![plan.source_path.clone(), plan.destination_path.clone()];
            changed.extend(plan.rewritten_files.iter().map(|file| file.path.clone()));
            for path in &changed {
                request.guard.check_write_path(path).map_err(deny)?;
            }
            if args.dry_run {
                return report(plan);
            }
            let summary = vulcan_app::browse::move_note_with_profile(
                request.paths,
                &source,
                &destination,
                false,
                Some(request.profile),
            )
            .map_err(|error| RestError::from_message(error.to_string()))?;
            finish_mutation(
                request.paths,
                request.profile,
                "move",
                &changed,
                args.no_commit,
            )?;
            report(summary)
        }),
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    /// Property filters, as `vulcan note update --where` takes them.
    #[serde(rename = "where")]
    filters: Vec<String>,
    /// Properties to set; non-string JSON values are written as YAML text.
    #[serde(default)]
    set: serde_json::Map<String, Value>,
    /// Properties to remove.
    #[serde(default)]
    unset: Vec<String>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    no_commit: bool,
}

/// `POST /{id}/update`: set or remove properties on every note matching a
/// filter (`vulcan note update` and `note unset`).
pub(crate) async fn update(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    write_vault(
        &state,
        &principal,
        &id,
        &method,
        &uri,
        &headers,
        &body,
        Box::new(|request| {
            let args: UpdateArgs = arguments(request.body, &[])?;
            if args.filters.is_empty() {
                return Err(RestError::new(
                    StatusCode::BAD_REQUEST,
                    "`where` needs at least one filter; a bulk update never applies to every note implicitly",
                ));
            }
            let mut changes = args
                .set
                .into_iter()
                .map(|(key, value)| vulcan_app::properties::PropertyChange {
                    key,
                    value: Some(match value {
                        Value::String(text) => text,
                        other => other.to_string(),
                    }),
                })
                .collect::<Vec<_>>();
            changes.extend(args.unset.into_iter().map(|key| {
                vulcan_app::properties::PropertyChange { key, value: None }
            }));
            if changes.is_empty() {
                return Err(RestError::new(
                    StatusCode::BAD_REQUEST,
                    "an update needs `set` or `unset`",
                ));
            }
            let reports = vulcan_app::properties::update_properties_where(
                request.paths,
                &args.filters,
                &changes,
                args.dry_run,
                args.no_commit,
                Some(request.profile),
                Verbosity::Quiet,
            )
            .map_err(|error| RestError::from_message(error.to_string()))?;
            Ok(json!({ "dry_run": args.dry_run, "changes": reports }))
        }),
    )
    .await
}

/// `POST /{id}/scan`: an incremental (or with `full`, a full) scan.
pub(crate) async fn scan(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    write_vault(
        &state,
        &principal,
        &id,
        &method,
        &uri,
        &headers,
        &body,
        Box::new(|request| {
            let args: McpIndexScanArgs = arguments(request.body, &[])?;
            report(
                vulcan_app::mcp_scan::index_scan(
                    request.paths,
                    request.guard,
                    request.profile,
                    &args,
                )
                .map_err(mcp_failure)?,
            )
        }),
    )
    .await
}

/// Refreshes the cache after a write and auto-commits it, as the CLI does.
fn finish_mutation(
    paths: &VaultPaths,
    profile: &str,
    action: &str,
    changed: &[String],
    no_commit: bool,
) -> Result<(), RestError> {
    vulcan_app::scan::refresh_cache_incrementally(paths)
        .map_err(|error| RestError::from_message(error.to_string()))?;
    AutoCommitPolicy::for_mutation(paths, no_commit)
        .commit(paths, action, changed, Some(profile), Verbosity::Quiet)
        .map_err(RestError::from_message)?;
    Ok(())
}
