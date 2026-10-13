//! Multi-root workspaces: one inner server per vault behind one connection.
//!
//! When the client's workspace folders span several vaults, each vault gets
//! an ordinary single-vault server (its own session, writer, and watcher)
//! on an in-memory connection, and this multiplexer routes between them:
//!
//! - document requests and notifications go to the vault holding the URI
//!   (files outside every vault go to the first vault, which ignores them);
//! - workspace-wide requests fan out and their results are merged;
//! - requests the inner servers send the client get fresh ids, and their
//!   progress tokens and capability registration ids are made unique;
//! - `workspace/didChangeWorkspaceFolders` starts and stops inner servers.

use crate::convert;
use crate::server::{self, LspError, LspOptions};
use crossbeam_channel::{select, unbounded, Receiver, Sender};
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types as lsp;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

/// The id of the `initialize` request sent to each inner server.
const INIT_ID: &str = "vulcan-mux/initialize";

/// The vault roots of the client's workspace folders, in folder order and
/// without duplicates.
pub(crate) fn folder_vaults(params: &lsp::InitializeParams) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for folder in params.workspace_folders.iter().flatten() {
        if let Some(root) = folder_vault(&folder.uri) {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
    }
    roots
}

fn folder_vault(uri: &lsp::Uri) -> Option<PathBuf> {
    let path = convert::file_path(uri)?;
    vulcan_core::vault_discovery::discover_vault_root(&path)
        .ok()
        .map(|discovery| discovery.root)
}

struct Vault {
    root: PathBuf,
    /// The root as the file system resolves it, for matching URIs that
    /// spell it differently (symlinks).
    canonical: PathBuf,
    sender: Sender<Message>,
    thread: JoinHandle<()>,
}

/// How the responses to a fanned-out request combine.
#[derive(Clone, Copy)]
enum Merge {
    /// One target; its response is the answer.
    Single,
    /// Concatenate array results.
    Array,
    /// Concatenate `items` of workspace diagnostic reports.
    Items,
    /// Concatenate `documentChanges` of workspace edits (`null` when none).
    DocumentChanges,
    /// The first successful result.
    First,
}

struct Pending {
    merge: Merge,
    vaults: Vec<usize>,
    results: Vec<Response>,
}

struct Mux<'c> {
    client: &'c Connection,
    options: LspOptions,
    /// The client's `initialize` parameters, reused for added vaults.
    params: Value,
    vaults: Vec<Option<Vault>>,
    merged_sender: Sender<(usize, Message)>,
    merged: Receiver<(usize, Message)>,
    /// Client requests awaiting inner responses.
    pending: HashMap<RequestId, Pending>,
    /// Requests to the client: our id to the inner server and its id.
    outgoing: HashMap<RequestId, (usize, RequestId)>,
    next_id: u64,
    /// Inner servers started after initialization, awaiting their
    /// `initialize` response, with the messages held for them until then
    /// (a server accepts nothing between `initialize` and `initialized`).
    starting: HashMap<usize, Vec<Message>>,
}

/// Serve `roots` (at least two) for a client whose `initialize` request
/// `id` with `params` has been received.
pub(crate) fn serve(
    client: &Connection,
    options: &LspOptions,
    id: RequestId,
    params: Value,
    roots: Vec<PathBuf>,
) -> Result<(), LspError> {
    let (merged_sender, merged) = unbounded();
    let mut mux = Mux {
        client,
        options: LspOptions {
            vault: None,
            permissions: options.permissions.clone(),
        },
        params,
        vaults: Vec::new(),
        merged_sender,
        merged,
        pending: HashMap::new(),
        outgoing: HashMap::new(),
        next_id: 0,
        starting: HashMap::new(),
    };
    for root in roots {
        mux.start_vault(root);
    }

    // Initialize every inner server before answering the client.
    let mut results = BTreeMap::new();
    let mut early = Vec::new();
    while results.len() < mux.vaults.len() {
        let (index, message) = mux
            .merged
            .recv()
            .map_err(|_| LspError::new("a vault server stopped during initialization"))?;
        match message {
            Message::Response(response) if response.id == RequestId::from(INIT_ID.to_string()) => {
                results.insert(index, response);
            }
            message => early.push((index, message)),
        }
    }
    let mut capabilities = None;
    for (index, response) in results {
        match response.response_result {
            Ok(result) => {
                capabilities.get_or_insert(result);
            }
            Err(error) => {
                let root = mux.stop_vault(index);
                let message = error.message;
                mux.show_warning(&format!(
                    "vulcan: not serving {}: {message}",
                    root.display()
                ));
            }
        }
    }
    let mut result =
        capabilities.ok_or_else(|| LspError::new("no workspace folder holds a usable vault"))?;
    result["capabilities"]["workspace"]["workspaceFolders"] =
        json!({ "supported": true, "changeNotifications": true });
    client.initialize_finish(id, result)?;
    for index in mux.live() {
        mux.send_to(index, Message::Notification(initialized()));
    }
    for (index, message) in early {
        mux.on_vault_message(index, message);
    }
    let outcome = mux.run();
    mux.stop_all();
    outcome
}

fn initialized() -> Notification {
    Notification::new("initialized".to_string(), json!({}))
}

impl Mux<'_> {
    fn run(&mut self) -> Result<(), LspError> {
        loop {
            select! {
                recv(self.client.receiver) -> message => {
                    let Ok(message) = message else { return Ok(()) };
                    if self.on_client_message(message) {
                        return Ok(());
                    }
                }
                recv(self.merged) -> message => {
                    if let Ok((index, message)) = message {
                        self.on_vault_message(index, message);
                    }
                }
            }
        }
    }

    fn live(&self) -> Vec<usize> {
        (0..self.vaults.len())
            .filter(|index| self.vaults[*index].is_some())
            .collect()
    }

    fn primary(&self) -> Option<usize> {
        self.live().first().copied()
    }

    fn start_vault(&mut self, root: PathBuf) -> usize {
        let index = self.vaults.len();
        let (outer, inner) = Connection::memory();
        let options = LspOptions {
            vault: Some(root.clone()),
            permissions: self.options.permissions.clone(),
        };
        let thread = std::thread::Builder::new()
            .name(format!("vulcan-lsp-vault-{index}"))
            .spawn(move || {
                let _ = server::run(&inner, &options);
            })
            .expect("spawning a vault server thread");
        let forward = self.merged_sender.clone();
        let receiver = outer.receiver.clone();
        std::thread::Builder::new()
            .name(format!("vulcan-lsp-vault-{index}-out"))
            .spawn(move || {
                for message in receiver {
                    if forward.send((index, message)).is_err() {
                        break;
                    }
                }
            })
            .expect("spawning a vault forwarding thread");

        let mut params = self.params.clone();
        if let Some(uri) = convert::file_uri(&root) {
            params["rootUri"] = json!(uri.as_str());
            params["rootPath"] = json!(root);
            params["workspaceFolders"] = json!([{ "uri": uri.as_str(), "name": root
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()) }]);
        }
        let _ = outer.sender.send(Message::Request(Request::new(
            RequestId::from(INIT_ID.to_string()),
            "initialize".to_string(),
            params,
        )));
        self.vaults.push(Some(Vault {
            canonical: root.canonicalize().unwrap_or_else(|_| root.clone()),
            root,
            sender: outer.sender,
            thread,
        }));
        index
    }

    /// Stop one inner server, returning its root.
    fn stop_vault(&mut self, index: usize) -> PathBuf {
        let vault = self.vaults[index].take().expect("a live vault");
        let _ = vault.sender.send(Message::Request(Request::new(
            RequestId::from("vulcan-mux/shutdown".to_string()),
            "shutdown".to_string(),
            Value::Null,
        )));
        let _ = vault.sender.send(Message::Notification(Notification::new(
            "exit".to_string(),
            Value::Null,
        )));
        // Requests it will no longer answer get an empty answer.
        let waiting = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.vaults.contains(&index))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in waiting {
            self.answer(index, Response::new_ok(id, Value::Null));
        }
        vault.root
    }

    fn stop_all(&mut self) {
        let mut threads = Vec::new();
        for vault in self.vaults.iter_mut().filter_map(Option::take) {
            let _ = vault.sender.send(Message::Notification(Notification::new(
                "exit".to_string(),
                Value::Null,
            )));
            threads.push(vault.thread);
        }
        for thread in threads {
            let _ = thread.join();
        }
    }

    fn send_to(&mut self, index: usize, message: Message) {
        if let Some(held) = self.starting.get_mut(&index) {
            held.push(message);
            return;
        }
        if let Some(vault) = &self.vaults[index] {
            let _ = vault.sender.send(message);
        }
    }

    fn send_client(&self, message: Message) {
        let _ = self.client.sender.send(message);
    }

    fn show_warning(&self, message: &str) {
        self.send_client(Message::Notification(Notification::new(
            "window/showMessage".to_string(),
            json!({ "type": 2, "message": message }),
        )));
    }

    /// The vault holding `uri`: the live vault with the longest root that
    /// contains it.
    fn vault_of(&self, uri: &str) -> Option<usize> {
        let path = convert::file_path(&uri.parse().ok()?)?;
        let canonical = canonical_existing(&path);
        self.live()
            .into_iter()
            .filter_map(|index| {
                let vault = self.vaults[index].as_ref()?;
                let length = if path.starts_with(&vault.root) {
                    vault.root.as_os_str().len()
                } else if canonical.starts_with(&vault.canonical) {
                    vault.canonical.as_os_str().len()
                } else {
                    return None;
                };
                Some((length, index))
            })
            .max_by_key(|(length, index)| (*length, std::cmp::Reverse(*index)))
            .map(|(_, index)| index)
    }

    fn vault_or_primary(&self, uri: Option<&str>) -> Option<usize> {
        uri.and_then(|uri| self.vault_of(uri))
            .or_else(|| self.primary())
    }

    /// Handle a client message; `true` ends the session.
    fn on_client_message(&mut self, message: Message) -> bool {
        match message {
            Message::Request(request) => self.client_request(request),
            Message::Response(response) => {
                if let Some((index, id)) = self.outgoing.remove(&response.id) {
                    self.send_to(index, Message::Response(Response { id, ..response }));
                }
            }
            Message::Notification(notification) => {
                if notification.method == "exit" {
                    return true;
                }
                self.client_notification(&notification);
            }
        }
        false
    }

    fn client_request(&mut self, request: Request) {
        let (merge, targets) = self.route_request(&request);
        if targets.is_empty() {
            self.send_client(Message::Response(Response::new_ok(request.id, Value::Null)));
            return;
        }
        self.pending.insert(
            request.id.clone(),
            Pending {
                merge,
                vaults: targets.iter().map(|(index, _)| *index).collect(),
                results: Vec::new(),
            },
        );
        for (index, params) in targets {
            self.send_to(
                index,
                Message::Request(Request::new(
                    request.id.clone(),
                    request.method.clone(),
                    params,
                )),
            );
        }
    }

    /// Where a client request goes, with the parameters each target gets.
    fn route_request(&self, request: &Request) -> (Merge, Vec<(usize, Value)>) {
        let params = &request.params;
        let all = || {
            self.live()
                .into_iter()
                .map(|index| (index, params.clone()))
                .collect::<Vec<_>>()
        };
        let one = |index: Option<usize>| {
            (
                Merge::Single,
                index
                    .map(|index| (index, params.clone()))
                    .into_iter()
                    .collect(),
            )
        };
        match request.method.as_str() {
            "shutdown" => (Merge::First, all()),
            "workspace/symbol" => (Merge::Array, all()),
            "workspace/diagnostic" => (
                Merge::Items,
                self.split(params, "previousResultIds", "uri", true),
            ),
            "workspace/willRenameFiles" => (
                Merge::DocumentChanges,
                self.split(params, "files", "oldUri", false),
            ),
            "workspace/executeCommand" => {
                let argument = &params["arguments"][0];
                let document = ["path", "from", "uri"]
                    .iter()
                    .find_map(|name| argument[name].as_str());
                if params["command"] == "vulcan.scan" {
                    (Merge::First, all())
                } else {
                    one(self.vault_or_primary(document))
                }
            }
            _ => one(self.vault_or_primary(document_uri(params))),
        }
    }

    /// Split the array `field` of `params` by the vault of each entry's
    /// `uri_field`. With `everyone`, vaults without entries still get the
    /// request (with an empty array).
    fn split(
        &self,
        params: &Value,
        field: &str,
        uri_field: &str,
        everyone: bool,
    ) -> Vec<(usize, Value)> {
        let mut groups = BTreeMap::<usize, Vec<Value>>::new();
        if everyone {
            for index in self.live() {
                groups.insert(index, Vec::new());
            }
        }
        for entry in params[field].as_array().into_iter().flatten() {
            if let Some(index) = self.vault_or_primary(entry[uri_field].as_str()) {
                groups.entry(index).or_default().push(entry.clone());
            }
        }
        groups
            .into_iter()
            .map(|(index, entries)| {
                let mut params = params.clone();
                params[field] = Value::Array(entries);
                (index, params)
            })
            .collect()
    }

    fn client_notification(&mut self, notification: &Notification) {
        let params = &notification.params;
        match notification.method.as_str() {
            "workspace/didChangeWorkspaceFolders" => self.change_folders(params),
            "workspace/didChangeWatchedFiles" => {
                for (index, params) in self.split(params, "changes", "uri", false) {
                    self.notify(index, &notification.method, params);
                }
            }
            "workspace/didRenameFiles" => {
                for (index, params) in self.split(params, "files", "oldUri", false) {
                    self.notify(index, &notification.method, params);
                }
            }
            "$/cancelRequest" => {
                let id = serde_json::from_value::<RequestId>(params["id"].clone()).ok();
                let vaults = id
                    .and_then(|id| self.pending.get(&id))
                    .map(|pending| pending.vaults.clone())
                    .unwrap_or_default();
                for index in vaults {
                    self.notify(index, &notification.method, params.clone());
                }
            }
            method if method.starts_with("textDocument/") => {
                if let Some(index) = self.vault_or_primary(document_uri(params)) {
                    self.notify(index, method, params.clone());
                }
            }
            method => {
                for index in self.live() {
                    self.notify(index, method, params.clone());
                }
            }
        }
    }

    fn notify(&mut self, index: usize, method: &str, params: Value) {
        self.send_to(
            index,
            Message::Notification(Notification::new(method.to_string(), params)),
        );
    }

    fn change_folders(&mut self, params: &Value) {
        for folder in params["event"]["added"].as_array().into_iter().flatten() {
            let Some(root) = folder["uri"]
                .as_str()
                .and_then(|uri| uri.parse().ok())
                .and_then(|uri| folder_vault(&uri))
            else {
                continue;
            };
            let served = self.vaults.iter().flatten().any(|vault| vault.root == root);
            if !served {
                let index = self.start_vault(root);
                self.starting.insert(index, Vec::new());
            }
        }
        for folder in params["event"]["removed"].as_array().into_iter().flatten() {
            let Some(root) = folder["uri"]
                .as_str()
                .and_then(|uri| uri.parse().ok())
                .and_then(|uri| folder_vault(&uri))
            else {
                continue;
            };
            // Keep the last vault: the session needs one to answer.
            if self.live().len() <= 1 {
                break;
            }
            if let Some(index) = self.live().into_iter().find(|index| {
                self.vaults[*index]
                    .as_ref()
                    .is_some_and(|vault| vault.root == root)
            }) {
                self.stop_vault(index);
            }
        }
    }

    fn on_vault_message(&mut self, index: usize, message: Message) {
        match message {
            Message::Response(response) => {
                if response.id == RequestId::from(INIT_ID.to_string()) {
                    if let Some(held) = self.starting.remove(&index) {
                        if response.response_result.is_err() {
                            let root = self.stop_vault(index);
                            self.show_warning(&format!("vulcan: not serving {}", root.display()));
                        } else {
                            self.send_to(index, Message::Notification(initialized()));
                            for message in held {
                                self.send_to(index, message);
                            }
                        }
                    }
                    return;
                }
                self.answer(index, response);
            }
            Message::Request(mut request) => {
                self.next_id += 1;
                let id = RequestId::from(format!("vulcan-mux/{}", self.next_id));
                self.outgoing
                    .insert(id.clone(), (index, std::mem::replace(&mut request.id, id)));
                unique_ids(index, &request.method, &mut request.params);
                self.send_client(Message::Request(request));
            }
            Message::Notification(mut notification) => {
                unique_ids(index, &notification.method, &mut notification.params);
                self.send_client(Message::Notification(notification));
            }
        }
    }

    /// Record an inner response to a client request, answering the client
    /// once every target has responded.
    fn answer(&mut self, index: usize, response: Response) {
        let id = response.id.clone();
        let Some(pending) = self.pending.get_mut(&id) else {
            return;
        };
        let Some(at) = pending.vaults.iter().position(|vault| *vault == index) else {
            return;
        };
        pending.vaults.remove(at);
        pending.results.push(response);
        if !pending.vaults.is_empty() {
            return;
        }
        let pending = self.pending.remove(&id).expect("pending request");
        self.send_client(Message::Response(merge(pending.merge, pending.results)));
    }
}

/// Combine the responses of a fanned-out request.
fn merge(merge: Merge, mut results: Vec<Response>) -> Response {
    let id = results[0].id.clone();
    if matches!(merge, Merge::Single) || results.len() == 1 && !matches!(merge, Merge::Items) {
        return results.remove(0);
    }
    let successes = results
        .iter()
        .filter_map(|response| response.response_result.clone().ok())
        .collect::<Vec<_>>();
    if successes.is_empty() {
        return results.remove(0);
    }
    let combined = match merge {
        Merge::Single | Merge::First => successes[0].clone(),
        Merge::Array => Value::Array(
            successes
                .iter()
                .filter_map(Value::as_array)
                .flatten()
                .cloned()
                .collect(),
        ),
        Merge::Items => json!({
            "items": successes
                .iter()
                .filter_map(|result| result["items"].as_array())
                .flatten()
                .cloned()
                .collect::<Vec<_>>(),
        }),
        Merge::DocumentChanges => {
            let changes = successes
                .iter()
                .filter_map(|result| result["documentChanges"].as_array())
                .flatten()
                .cloned()
                .collect::<Vec<_>>();
            if changes.is_empty() {
                Value::Null
            } else {
                json!({ "documentChanges": changes })
            }
        }
    };
    Response::new_ok(id, combined)
}

/// The document a request or notification is about.
fn document_uri(params: &Value) -> Option<&str> {
    params["textDocument"]["uri"]
        .as_str()
        .or_else(|| params["uri"].as_str())
}

/// Make progress tokens and capability registration ids from vault `index`
/// distinct from other vaults' (the first vault keeps its own).
fn unique_ids(index: usize, method: &str, params: &mut Value) {
    if index == 0 {
        return;
    }
    let suffix = |value: &mut Value| {
        if let Some(text) = value.as_str() {
            *value = json!(format!("{text}#{index}"));
        }
    };
    match method {
        "window/workDoneProgress/create" | "$/progress" => suffix(&mut params["token"]),
        "client/registerCapability" => {
            for registration in params["registrations"].as_array_mut().into_iter().flatten() {
                suffix(&mut registration["id"]);
            }
        }
        "client/unregisterCapability" => {
            // The protocol spells it this way.
            for registration in params["unregisterations"]
                .as_array_mut()
                .into_iter()
                .flatten()
            {
                suffix(&mut registration["id"]);
            }
        }
        _ => {}
    }
}

/// `path`, or its deepest existing ancestor, resolved by the file system.
fn canonical_existing(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        if let Ok(canonical) = current.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(canonical, |path: PathBuf, part| path.join(part));
        }
        match (current.parent(), current.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_os_string());
                current = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_vaults_get_distinct_progress_tokens_and_registrations() {
        let mut progress = json!({ "token": "vulcan/index" });
        unique_ids(0, "$/progress", &mut progress);
        assert_eq!(progress["token"], "vulcan/index");
        unique_ids(2, "$/progress", &mut progress);
        assert_eq!(progress["token"], "vulcan/index#2");
        let mut registration = json!({ "registrations": [{ "id": "vulcan/watched-files" }] });
        unique_ids(1, "client/registerCapability", &mut registration);
        assert_eq!(
            registration["registrations"][0]["id"],
            "vulcan/watched-files#1"
        );
    }

    #[test]
    fn merges_fanned_out_results() {
        let id = RequestId::from(7);
        let response = |value: Value| Response::new_ok(id.clone(), value);
        let symbols = merge(
            Merge::Array,
            vec![
                response(json!([1])),
                response(Value::Null),
                response(json!([2, 3])),
            ],
        );
        assert_eq!(symbols.response_result.unwrap(), json!([1, 2, 3]));
        let reports = merge(Merge::Items, vec![response(json!({ "items": [1] }))]);
        assert_eq!(reports.response_result.unwrap(), json!({ "items": [1] }));
        let edits = merge(
            Merge::DocumentChanges,
            vec![response(Value::Null), response(Value::Null)],
        );
        assert_eq!(edits.response_result.unwrap(), Value::Null);
        let failed = merge(
            Merge::First,
            vec![
                Response::new_err(id.clone(), -32603, "no".into()),
                response(json!(1)),
            ],
        );
        assert_eq!(failed.response_result.unwrap(), json!(1));
    }
}
