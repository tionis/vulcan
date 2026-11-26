//! REST routes for Meta Bind (`vulcan metabind`): a note's fields, and the
//! `set`, `apply`, and `press` writes behind the host's mutation gate. The
//! responses are the CLI's `--output json` reports. See
//! `docs/specs/daemon-rest-api.md`.

use crate::http::CompanionHttpState;
use crate::rest::{read_vault, vault_relative, Principal, RestError};
use crate::rest_writes::{arguments, finish_mutation, report, write_vault, WriteRequest};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use vulcan_app::metabind::{
    list_metabind_fields, plan_metabind_apply, plan_metabind_set, press_metabind_button,
    ButtonSelector, MetaBindFileWrite, MetaBindPressOptions, MetaBindWritePlan, MetaBindWriter,
};
use vulcan_app::AppError;
use vulcan_core::{PermissionGuard, VaultConfig, VaultPaths, Verbosity};

fn read_note(
    paths: &VaultPaths,
    guard: &impl PermissionGuard,
    note: &str,
) -> Result<(VaultConfig, String), RestError> {
    guard
        .check_read_path(note)
        .map_err(|error| RestError::new(StatusCode::FORBIDDEN, error.to_string()))?;
    let source = std::fs::read_to_string(paths.vault_root().join(note))
        .map_err(|_| RestError::new(StatusCode::NOT_FOUND, format!("no note `{note}`")))?;
    Ok((vulcan_core::load_vault_config(paths).config, source))
}

#[allow(clippy::needless_pass_by_value)] // for `map_err(operation)`
fn operation(error: AppError) -> RestError {
    RestError::from_message(error.to_string())
}

/// `GET /{id}/metabind/fields/{path}`: a note's fields with their values
/// (`vulcan metabind list`).
pub(crate) async fn fields(
    State(state): State<CompanionHttpState>,
    Extension(principal): Extension<Principal>,
    Path((id, path)): Path<(String, String)>,
) -> Result<Json<Value>, RestError> {
    let note = vault_relative(&path)?;
    read_vault(&state, &principal, &id, move |vault, guard| {
        let (config, source) = read_note(&vault.paths, guard, &note)?;
        list_metabind_fields(
            &vault.paths,
            &config,
            &note,
            &source,
            vault.permissions_profile.as_deref(),
        )
        .map_err(operation)
    })
    .await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetArgs {
    target: String,
    /// Text, as an input takes it (`on`, `4`, `a, b`).
    value: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyArgs {
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PressArgs {
    button: Option<String>,
    /// A one-based line of the button's block.
    line: Option<usize>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    no_commit: bool,
}

/// Writes `writes` as `vulcan metabind` does, after checking each against
/// the caller's guard.
fn write_files(request: &WriteRequest<'_>, writes: &[MetaBindFileWrite]) -> Result<(), RestError> {
    for write in writes {
        request
            .guard
            .check_write_path(&write.path)
            .map_err(|error| RestError::new(StatusCode::FORBIDDEN, error.to_string()))?;
        vulcan_app::notes::write_note_content(
            request.paths,
            &write.path,
            (!write.created).then_some(write.before.as_str()),
            &write.after,
            "metabind",
            Some(request.profile),
            Verbosity::Quiet,
        )
        .map_err(operation)?;
    }
    Ok(())
}

fn finish_plan(
    request: &WriteRequest<'_>,
    note: &str,
    plan: &MetaBindWritePlan,
    dry_run: bool,
    no_commit: bool,
) -> Result<Value, RestError> {
    if !dry_run && !plan.writes.is_empty() {
        write_files(request, &plan.writes)?;
        let changed = plan
            .writes
            .iter()
            .map(|write| write.path.clone())
            .collect::<Vec<_>>();
        finish_mutation(
            request.paths,
            request.profile,
            "metabind",
            &changed,
            no_commit,
        )?;
    }
    Ok(json!({
        "path": note,
        "dry_run": dry_run,
        "changed": !plan.writes.is_empty(),
        "changes": plan.changes,
        "skipped": plan.skipped,
    }))
}

macro_rules! write_route {
    ($name:ident, $doc:literal, $workflow:expr) => {
        #[doc = $doc]
        pub(crate) async fn $name(
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
                Err(error) => return axum::response::IntoResponse::into_response(error),
            };
            write_vault(
                &state,
                &principal,
                &id,
                &method,
                &uri,
                &headers,
                &body,
                Box::new(move |request| $workflow(request, &note)),
            )
            .await
        }
    };
}

write_route!(
    set,
    "`POST /{id}/metabind/set/{path}`: write a bound value (`vulcan metabind set`).",
    |request: WriteRequest<'_>, note: &str| {
        let (config, source) = read_note(request.paths, request.guard, note)?;
        let args: SetArgs = arguments(request.body.clone(), &[])?;
        let plan = plan_metabind_set(
            request.paths,
            &config,
            note,
            &source,
            &args.target,
            &args.value,
            Some(request.profile),
        )
        .map_err(|error| RestError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
        finish_plan(&request, note, &plan, args.dry_run, args.no_commit)
    }
);

write_route!(
    apply,
    "`POST /{id}/metabind/apply/{path}`: write view results (`vulcan metabind apply`).",
    |request: WriteRequest<'_>, note: &str| {
        let (config, source) = read_note(request.paths, request.guard, note)?;
        let args: ApplyArgs = arguments(request.body.clone(), &[])?;
        let plan =
            plan_metabind_apply(request.paths, &config, note, &source, Some(request.profile))
                .map_err(operation)?;
        finish_plan(&request, note, &plan, args.dry_run, args.no_commit)
    }
);

/// Writes a pressed button's edits through [`write_files`].
struct RestWriter<'r, 'a> {
    request: &'r WriteRequest<'a>,
    written: Vec<String>,
}

impl MetaBindWriter for RestWriter<'_, '_> {
    fn write(&mut self, write: &MetaBindFileWrite) -> Result<(), AppError> {
        write_files(self.request, std::slice::from_ref(write))
            .map_err(|error| AppError::operation(error.error.detail.clone()))?;
        if !self.written.contains(&write.path) {
            self.written.push(write.path.clone());
        }
        Ok(())
    }
}

write_route!(
    press,
    "`POST /{id}/metabind/press/{path}`: press a button (`vulcan metabind press`).",
    |request: WriteRequest<'_>, note: &str| {
        let (config, source) = read_note(request.paths, request.guard, note)?;
        let args: PressArgs = arguments(request.body.clone(), &[])?;
        let selector = match (args.button, args.line) {
            (Some(id), _) => ButtonSelector::Id(id),
            (None, Some(line)) => ButtonSelector::Line(line),
            (None, None) => {
                return Err(RestError::new(
                    StatusCode::BAD_REQUEST,
                    "a press needs a `button` id or a `line`",
                ))
            }
        };
        let mut writer = RestWriter {
            request: &request,
            written: Vec::new(),
        };
        let pressed = press_metabind_button(
            request.paths,
            &config,
            note,
            &source,
            &selector,
            MetaBindPressOptions {
                dry_run: args.dry_run,
                permission_profile: Some(request.profile),
            },
            &mut writer,
        );
        let written = writer.written;
        if !written.is_empty() {
            finish_mutation(
                request.paths,
                request.profile,
                "metabind",
                &written,
                args.no_commit,
            )?;
        }
        report(pressed.map_err(operation)?)
    }
);
