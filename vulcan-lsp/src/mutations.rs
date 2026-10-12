//! Renames and moves (`docs/specs/editor-integration.md`, "Mutation
//! contract"), run on the writer thread.
//!
//! A plan splits into closed files, published as one journaled batch, and
//! open documents, edited through `workspace/applyEdit`. The disk side goes
//! first because the server can undo it exactly: when the editor rejects its
//! part, the inverse batch restores every file still holding the planned
//! bytes and leaves files edited since alone.

use crate::convert;
use crate::handlers::{self, ResponseFailure};
use crossbeam_channel::{bounded, Sender};
use lsp_server::{ErrorCode, Message, Request, RequestId, Response};
use lsp_types as lsp;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vulcan_app::editor::{
    inverse_changes, EditorEditPlan, EditorMove, EditorSession, EditorTemplatePlacement,
};

/// How long the server waits for the client to apply an edit.
const APPLY_EDIT_TIMEOUT: Duration = Duration::from_secs(120);

/// Commands served through `workspace/executeCommand`.
pub(crate) const COMMANDS: &[&str] = &[
    "vulcan.scan",
    "vulcan.note.move",
    "vulcan.note.create",
    "vulcan.task.toggle",
    "vulcan.periodic.open",
    "vulcan.template.insert",
];

/// Server-to-client requests whose responses someone awaits.
pub(crate) struct ClientChannel {
    sender: Sender<Message>,
    next_id: AtomicI32,
    waiting: Mutex<HashMap<RequestId, Sender<Response>>>,
}

impl ClientChannel {
    pub(crate) fn new(sender: Sender<Message>) -> Self {
        Self {
            sender,
            next_id: AtomicI32::new(0),
            waiting: Mutex::new(HashMap::new()),
        }
    }

    /// Send a request without waiting for its response.
    pub(crate) fn send_request(&self, method: &str, params: Value) -> RequestId {
        let id = RequestId::from(format!(
            "vulcan/{}",
            self.next_id.fetch_add(1, Ordering::SeqCst) + 1
        ));
        let _ = self.sender.send(Message::Request(Request::new(
            id.clone(),
            method.to_string(),
            params,
        )));
        id
    }

    /// Send a request and wait for its response.
    fn call(&self, method: &str, params: Value) -> Option<Response> {
        let (sender, receiver) = bounded(1);
        let id = RequestId::from(format!(
            "vulcan/{}",
            self.next_id.fetch_add(1, Ordering::SeqCst) + 1
        ));
        lock(&self.waiting).insert(id.clone(), sender);
        let _ = self.sender.send(Message::Request(Request::new(
            id.clone(),
            method.to_string(),
            params,
        )));
        let response = receiver.recv_timeout(APPLY_EDIT_TIMEOUT).ok();
        lock(&self.waiting).remove(&id);
        response
    }

    /// Hand a client response to its waiter; returns it when nobody waits.
    pub(crate) fn deliver(&self, response: Response) -> Option<Response> {
        match lock(&self.waiting).remove(&response.id) {
            Some(waiter) => {
                let _ = waiter.send(response);
                None
            }
            None => Some(response),
        }
    }

    fn respond(&self, response: Response) {
        let _ = self.sender.send(Message::Response(response));
    }
}

/// What the client can do with a workspace edit.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EditSupport {
    /// `workspace/applyEdit` with versioned `documentChanges`.
    pub(crate) versioned_edits: bool,
    pub(crate) rename_files: bool,
    pub(crate) show_document: bool,
}

impl From<&lsp::ClientCapabilities> for EditSupport {
    fn from(capabilities: &lsp::ClientCapabilities) -> Self {
        let workspace = capabilities.workspace.as_ref();
        let edit = workspace.and_then(|workspace| workspace.workspace_edit.as_ref());
        Self {
            // Resource operations only exist inside `documentChanges`, so a
            // client listing them supports it even without saying so
            // (Neovim does this).
            versioned_edits: workspace.and_then(|workspace| workspace.apply_edit) == Some(true)
                && edit.is_some_and(|edit| {
                    edit.document_changes == Some(true)
                        || edit
                            .resource_operations
                            .as_ref()
                            .is_some_and(|operations| !operations.is_empty())
                }),
            rename_files: edit
                .and_then(|edit| edit.resource_operations.as_ref())
                .is_some_and(|operations| operations.contains(&lsp::ResourceOperationKind::Rename)),
            show_document: capabilities
                .window
                .as_ref()
                .and_then(|window| window.show_document.as_ref())
                .is_some_and(|show| show.support),
        }
    }
}

/// Plans for client renames, published once the rename happened.
pub(crate) type PendingRenames = Arc<Mutex<HashMap<(String, String), EditorEditPlan>>>;

/// Everything the writer needs to run a mutation.
pub(crate) struct MutationContext {
    pub(crate) client: Arc<ClientChannel>,
    pub(crate) support: EditSupport,
    pub(crate) pending: PendingRenames,
}

/// Answer a mutating request. `session` is a snapshot from when it arrived.
pub(crate) fn handle(context: &MutationContext, session: &EditorSession, request: Request) {
    let id = request.id.clone();
    let result = match request.method.as_str() {
        "textDocument/rename" => params::<lsp::RenameParams>(request)
            .and_then(|params| rename(context, session, &params)),
        "workspace/willRenameFiles" => params::<lsp::RenameFilesParams>(request)
            .and_then(|params| will_rename(context, session, &params)),
        "workspace/executeCommand" => params::<lsp::ExecuteCommandParams>(request)
            .and_then(|params| execute_command(context, session, &params)),
        method => Err(ResponseFailure {
            code: ErrorCode::MethodNotFound as i32,
            message: format!("unsupported method {method}"),
            kind: "invalid_request".to_string(),
        }),
    };
    context.client.respond(match result {
        Ok(value) => Response::new_ok(id, value),
        Err(failure) => {
            crate::server::failure_response(id, failure.code, failure.message, &failure.kind)
        }
    });
}

/// Publish the closed-file rewrites planned for a client rename that has
/// now happened.
pub(crate) fn apply_pending(
    context: &MutationContext,
    session: &EditorSession,
    from: &str,
    to: &str,
) -> Option<String> {
    let plan = lock(&context.pending).remove(&(from.to_string(), to.to_string()))?;
    session
        .apply_disk_changes(&plan.disk, &BTreeMap::new())
        .err()
        .map(|error| format!("Rewriting links after renaming {from} failed: {error}"))
}

fn rename(
    context: &MutationContext,
    session: &EditorSession,
    params: &lsp::RenameParams,
) -> Result<Value, ResponseFailure> {
    let position = &params.text_document_position;
    let path = vault_path(session, &position.text_document.uri)?;
    let plan = session.plan_rename(
        &path,
        convert::position(position.position),
        &params.new_name,
    )?;
    session.authorize_plan(&plan)?;
    execute(context, session, &plan)?;
    // Everything was applied through `workspace/applyEdit`; the client has
    // nothing left to apply.
    Ok(json!({ "documentChanges": [] }))
}

fn will_rename(
    context: &MutationContext,
    session: &EditorSession,
    params: &lsp::RenameFilesParams,
) -> Result<Value, ResponseFailure> {
    let renames = params
        .files
        .iter()
        .filter_map(|rename| {
            Some(EditorMove {
                from: handlers::vault_path(session, &rename.old_uri.parse().ok()?)?,
                to: handlers::vault_path(session, &rename.new_uri.parse().ok()?)?,
            })
        })
        .collect::<Vec<_>>();
    let plans = session.plan_client_renames(&renames)?;
    for plan in &plans {
        session.authorize_plan(plan)?;
    }
    let mut changes = Vec::new();
    for (rename, plan) in renames.iter().zip(plans) {
        changes.extend(document_changes(session, &plan)?);
        lock(&context.pending).insert((rename.from.clone(), rename.to.clone()), plan);
    }
    // The client applies these edits itself, then renames the files.
    Ok(json!({ "documentChanges": changes }))
}

fn execute_command(
    context: &MutationContext,
    session: &EditorSession,
    params: &lsp::ExecuteCommandParams,
) -> Result<Value, ResponseFailure> {
    let argument = params.arguments.first().cloned().unwrap_or(Value::Null);
    let command = params.command.as_str();
    let text = |name: &str| {
        argument[name]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| invalid(format!("{command} needs a `{name}` string argument")))
    };
    // A document is named by a vault path or a file URI.
    let document = |name: &str| {
        text(name).map(|value| {
            value
                .parse::<lsp::Uri>()
                .ok()
                .and_then(|uri| handlers::vault_path(session, &uri))
                .unwrap_or(value)
        })
    };
    if command == "vulcan.scan" {
        let full = argument["full"].as_bool() == Some(true);
        let summary = session.rescan(full)?;
        return Ok(json!({ "action": "scan", "full": full, "summary": summary }));
    }
    let plan = match command {
        "vulcan.note.move" => session.plan_note_move(&document("from")?, &text("to")?)?,
        "vulcan.periodic.open" => session.plan_periodic_open(
            argument["period"].as_str().unwrap_or("daily"),
            argument["date"].as_str(),
        )?,
        "vulcan.template.insert" => {
            let placement = match (&argument["position"], argument["mode"].as_str()) {
                (position, _) if position.is_object() => {
                    let position = serde_json::from_value::<lsp::Position>(position.clone())
                        .map_err(|error| invalid(error.to_string()))?;
                    EditorTemplatePlacement::At(convert::position(position))
                }
                (_, Some("prepend")) => EditorTemplatePlacement::Prepend,
                (_, None | Some("append")) => EditorTemplatePlacement::Append,
                (_, Some(mode)) => return Err(invalid(format!("unknown insert mode {mode}"))),
            };
            session.plan_template_insert(&document("path")?, &text("template")?, placement)?
        }
        "vulcan.note.create" => session.plan_note_create(&text("path")?)?,
        "vulcan.task.toggle" => {
            let line = argument["line"]
                .as_u64()
                .and_then(|line| u32::try_from(line).ok())
                .ok_or_else(|| invalid(format!("{command} needs a `line` number argument")))?;
            let today = argument["today"].as_str().map_or_else(
                vulcan_app::periodic::current_local_date_string,
                str::to_string,
            );
            session.plan_task_toggle(&document("path")?, line, &today)?
        }
        command => return Err(invalid(format!("unknown command {command}"))),
    };
    session.authorize_plan(&plan)?;
    if argument["dryRun"].as_bool() == Some(true) {
        return Ok(report(&plan, true));
    }
    execute(context, session, &plan)?;
    let shown = match command {
        "vulcan.note.create" => plan.disk.first().map(|change| change.path.clone()),
        "vulcan.periodic.open" => plan.report["path"].as_str().map(str::to_string),
        _ => None,
    };
    if context.support.show_document {
        if let Some(uri) = shown.and_then(|path| handlers::uri(session, &path)) {
            context.client.send_request(
                "window/showDocument",
                json!({ "uri": uri.to_string(), "takeFocus": true }),
            );
        }
    }
    Ok(report(&plan, false))
}

/// Apply a plan: the disk batch, then the editor's part, undoing the disk
/// batch when the editor rejects.
fn execute(
    context: &MutationContext,
    session: &EditorSession,
    plan: &EditorEditPlan,
) -> Result<(), ResponseFailure> {
    let needs_editor = !plan.open.is_empty() || plan.editor_rename.is_some();
    if needs_editor && !context.support.versioned_edits {
        return Err(conflict(
            "the editor cannot apply versioned workspace edits; close the affected notes first"
                .to_string(),
        ));
    }
    if plan.editor_rename.is_some() && !context.support.rename_files {
        return Err(conflict(
            "the editor cannot rename files; close the note before moving it".to_string(),
        ));
    }
    session.apply_disk_changes(&plan.disk, &plan.disk_renames)?;
    if !needs_editor {
        return Ok(());
    }

    let edit = json!({ "documentChanges": document_changes(session, plan)? });
    let applied = context
        .client
        .call(
            "workspace/applyEdit",
            json!({ "label": label(plan), "edit": edit }),
        )
        .and_then(|response| response.response_result.ok())
        .is_some_and(|result| result["applied"] == json!(true));
    if !applied {
        let (inverse, renames) = inverse_changes(&plan.disk, &plan.disk_renames);
        let restored = plan
            .disk
            .iter()
            .map(|change| change.path.clone())
            .collect::<Vec<_>>();
        return Err(match session.apply_disk_changes(&inverse, &renames) {
            Ok(()) => ResponseFailure {
                code: ErrorCode::RequestFailed as i32,
                message: format!(
                    "the editor rejected the edit; the vault was restored ({})",
                    restored.join(", ")
                ),
                kind: "conflict".to_string(),
            },
            Err(error) => ResponseFailure {
                code: ErrorCode::RequestFailed as i32,
                message: format!(
                    "the editor rejected the edit, and restoring the vault failed because a file \
                     changed meanwhile ({error}); run `vulcan doctor`. Changed: {}",
                    restored.join(", ")
                ),
                kind: "conflict".to_string(),
            },
        });
    }
    if let Some(rename) = &plan.editor_rename {
        // The editor moved the file; keep its identity in the index.
        let renames = HashMap::from([(rename.to.clone(), rename.from.clone())]);
        session.scan_changed(&[], &renames)?;
    }
    Ok(())
}

/// The plan's editor part as LSP `documentChanges`: versioned text edits,
/// then the rename of a moved open document.
fn document_changes(
    session: &EditorSession,
    plan: &EditorEditPlan,
) -> Result<Vec<Value>, ResponseFailure> {
    let uri = |path: &str| {
        handlers::uri(session, path)
            .map(|uri| uri.to_string())
            .ok_or_else(|| invalid(format!("{path} has no file URI")))
    };
    let mut changes = Vec::new();
    for document in &plan.open {
        let edits = document
            .edits
            .iter()
            .map(|edit| {
                json!({
                    "range": convert::lsp_range(edit.range),
                    "newText": edit.new_text,
                })
            })
            .collect::<Vec<_>>();
        changes.push(json!({
            "textDocument": { "uri": uri(&document.path)?, "version": document.version },
            "edits": edits,
        }));
    }
    if let Some(rename) = &plan.editor_rename {
        changes.push(json!({
            "kind": "rename",
            "oldUri": uri(&rename.from)?,
            "newUri": uri(&rename.to)?,
        }));
    }
    Ok(changes)
}

fn report(plan: &EditorEditPlan, dry_run: bool) -> Value {
    let mut editor = plan
        .open
        .iter()
        .map(|document| document.path.clone())
        .collect::<Vec<_>>();
    if let Some(rename) = &plan.editor_rename {
        editor.push(rename.from.clone());
    }
    editor.sort();
    editor.dedup();
    json!({
        "dryRun": dry_run,
        "action": plan.action,
        "report": plan.report,
        "disk": plan.disk.iter().map(|change| &change.path).collect::<Vec<_>>(),
        "editor": editor,
        "moved": plan.moved,
    })
}

fn label(plan: &EditorEditPlan) -> String {
    match &plan.moved {
        Some(moved) => format!("Move {} to {}", moved.from, moved.to),
        None => match plan.action {
            "rename_heading" => "Rename heading".to_string(),
            "rename_block_ref" => "Rename block id".to_string(),
            "toggle_task" => "Toggle task".to_string(),
            "create_note" => "Create note".to_string(),
            "periodic_open" => "Create periodic note".to_string(),
            "template_insert" => "Insert template".to_string(),
            action => action.to_string(),
        },
    }
}

fn vault_path(session: &EditorSession, uri: &lsp::Uri) -> Result<String, ResponseFailure> {
    handlers::vault_path(session, uri)
        .ok_or_else(|| invalid("the document is outside the vault".to_string()))
}

fn invalid(message: String) -> ResponseFailure {
    ResponseFailure {
        code: ErrorCode::InvalidParams as i32,
        message,
        kind: "invalid_request".to_string(),
    }
}

fn conflict(message: String) -> ResponseFailure {
    ResponseFailure {
        code: ErrorCode::RequestFailed as i32,
        message,
        kind: "conflict".to_string(),
    }
}

fn params<P: serde::de::DeserializeOwned>(request: Request) -> Result<P, ResponseFailure> {
    serde_json::from_value(request.params).map_err(|error| invalid(error.to_string()))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
