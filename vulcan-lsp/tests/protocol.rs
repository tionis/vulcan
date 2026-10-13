//! Protocol-level tests: an in-memory client drives a real server thread.

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use serde_json::{json, Value};
use std::path::Path;
use std::thread::JoinHandle;
use std::time::Duration;
use vulcan_core::{scan_vault, ScanMode, VaultPaths};
use vulcan_lsp::LspOptions;

const HOME: &str = "# Home\n\nSee [[Alpha#Status]] and #proj.\n";
const ALPHA: &str = "---\naliases: [First]\n---\n# Alpha\n\n## Status\n\nGreen 😀 [[Home]]\n";

struct Client {
    connection: Connection,
    server: Option<JoinHandle<()>>,
    next_id: i32,
    /// Notifications received while waiting for responses.
    notifications: Vec<Notification>,
    /// Requests the server sent; each is answered with `null`, except
    /// `workspace/applyEdit`, which is answered with `accept_edits`.
    server_requests: Vec<Request>,
    accept_edits: bool,
    root: std::path::PathBuf,
}

impl Client {
    fn start(root: &Path, capabilities: &Value) -> (Self, Value) {
        Self::start_with(root, capabilities, &Value::Null)
    }

    fn start_with(root: &Path, capabilities: &Value, options: &Value) -> (Self, Value) {
        Self::start_full(root, capabilities, options, LspOptions::default())
    }

    fn start_full(
        root: &Path,
        capabilities: &Value,
        options: &Value,
        lsp_options: LspOptions,
    ) -> (Self, Value) {
        Self::start_initialize(
            root,
            json!({
                "processId": null,
                "rootUri": uri(root),
                "capabilities": capabilities,
                "initializationOptions": options,
            }),
            lsp_options,
        )
    }

    fn start_initialize(root: &Path, initialize: Value, lsp_options: LspOptions) -> (Self, Value) {
        let (server, client) = Connection::memory();
        let handle = std::thread::spawn(move || {
            vulcan_lsp::run(&server, &lsp_options).unwrap();
        });
        let mut client = Self {
            connection: client,
            server: Some(handle),
            next_id: 0,
            notifications: Vec::new(),
            server_requests: Vec::new(),
            accept_edits: true,
            root: root.to_path_buf(),
        };
        let result = client.request("initialize", initialize);
        client.notify("initialized", json!({}));
        (client, result)
    }

    fn uri(&self, path: &str) -> String {
        uri(&self.root.join(path))
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        self.connection
            .sender
            .send(Message::Request(Request::new(
                id.clone(),
                method.into(),
                params,
            )))
            .unwrap();
        loop {
            if let Some(response) = self.pump() {
                if response.id == id {
                    return match response.response_result {
                        Ok(value) => value,
                        Err(error) => json!({ "error": error.code, "data": error.data }),
                    };
                }
            }
        }
    }

    /// Receive one message: record notifications, answer server requests,
    /// and return responses.
    fn pump(&mut self) -> Option<lsp_server::Response> {
        match self.receive() {
            Message::Response(response) => Some(response),
            Message::Notification(notification) => {
                self.notifications.push(notification);
                None
            }
            Message::Request(request) => {
                let result = if request.method == "workspace/applyEdit" {
                    if self.accept_edits {
                        // Like an editor, perform file renames on disk.
                        for change in request.params["edit"]["documentChanges"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|change| change["kind"] == "rename")
                        {
                            let path = |key: &str| {
                                url::Url::parse(change[key].as_str().unwrap())
                                    .unwrap()
                                    .to_file_path()
                                    .unwrap()
                            };
                            let target = path("newUri");
                            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                            std::fs::rename(path("oldUri"), target).unwrap();
                        }
                    }
                    json!({ "applied": self.accept_edits })
                } else {
                    Value::Null
                };
                self.connection
                    .sender
                    .send(Message::Response(lsp_server::Response::new_ok(
                        request.id.clone(),
                        result,
                    )))
                    .unwrap();
                self.server_requests.push(request);
                None
            }
        }
    }

    /// Wait for (and remove) the first notification matching `wanted`.
    fn wait_for(&mut self, wanted: impl Fn(&Notification) -> bool) -> Notification {
        loop {
            if let Some(index) = self.notifications.iter().position(&wanted) {
                return self.notifications.remove(index);
            }
            self.pump();
        }
    }

    fn notify(&self, method: &str, params: Value) {
        self.connection
            .sender
            .send(Message::Notification(Notification::new(
                method.into(),
                params,
            )))
            .unwrap();
    }

    fn receive(&self) -> Message {
        self.connection
            .receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("server message")
    }

    /// The next diagnostics published for `path`.
    fn diagnostics(&mut self, path: &str) -> Vec<Value> {
        let uri = self.uri(path);
        let take = |notifications: &mut Vec<Notification>| {
            let index = notifications.iter().position(|notification| {
                notification.method == "textDocument/publishDiagnostics"
                    && notification.params["uri"] == uri
            })?;
            let notification = notifications.remove(index);
            Some(
                notification.params["diagnostics"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            )
        };
        loop {
            if let Some(diagnostics) = take(&mut self.notifications) {
                return diagnostics;
            }
            self.pump();
        }
    }

    /// Wait until the server reports an index change.
    fn index_changed(&mut self) -> Notification {
        self.wait_for(|notification| notification.method == "vulcan/indexChanged")
    }

    fn open(&mut self, path: &str, text: &str) {
        let uri = self.uri(path);
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": uri, "languageId": "markdown", "version": 1, "text": text,
            }}),
        );
    }

    fn at(&self, path: &str, line: u32, character: u32) -> Value {
        json!({
            "textDocument": { "uri": self.uri(path) },
            "position": { "line": line, "character": character },
        })
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        let _ = self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        if let Some(server) = self.server.take() {
            server.join().unwrap();
        }
    }
}

fn uri(path: &Path) -> String {
    url::Url::from_file_path(path).unwrap().to_string()
}

fn vault(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let target = dir.path().join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, text).unwrap();
    }
    let paths = VaultPaths::new(dir.path());
    vulcan_core::paths::initialize_vulcan_dir(&paths).unwrap();
    scan_vault(&paths, ScanMode::Full).unwrap();
    dir
}

#[test]
fn initialize_negotiates_encoding_and_advertises_the_vulcan_protocol() {
    let dir = vault(&[("Home.md", HOME)]);
    let (_client, result) = Client::start(dir.path(), &json!({}));
    let capabilities = &result["capabilities"];
    assert_eq!(capabilities["positionEncoding"], "utf-16");
    assert_eq!(capabilities["experimental"]["vulcan"]["protocolVersion"], 1);
    assert_eq!(capabilities["textDocumentSync"]["change"], 2);

    let (_client, result) = Client::start(
        dir.path(),
        &json!({ "general": { "positionEncodings": ["utf-8", "utf-16"] } }),
    );
    assert_eq!(result["capabilities"]["positionEncoding"], "utf-8");

    // Neovim sends an empty Lua table as `[]`.
    let (_client, result) = Client::start_with(dir.path(), &json!({}), &json!([]));
    assert_eq!(
        result["capabilities"]["experimental"]["vulcan"]["protocolVersion"],
        1
    );
}

#[test]
fn navigates_links_backlinks_and_symbols() {
    let dir = vault(&[("Home.md", HOME), ("Alpha.md", ALPHA)]);
    let (mut client, _) = Client::start(dir.path(), &json!({}));
    client.open("Home.md", HOME);
    assert!(client.diagnostics("Home.md").is_empty());

    let definition = client.request("textDocument/definition", client.at("Home.md", 2, 8));
    assert_eq!(definition["uri"], client.uri("Alpha.md"));
    assert_eq!(
        definition["range"]["start"],
        json!({ "line": 5, "character": 0 })
    );

    let hover = client.request("textDocument/hover", client.at("Home.md", 2, 8));
    let markdown = hover["contents"]["value"].as_str().unwrap();
    assert!(markdown.contains("## Status"), "{markdown}");

    // Backlinks of Home: the link in Alpha, after an emoji (two UTF-16 units).
    let references = client.request("textDocument/references", {
        let mut params = client.at("Home.md", 1, 0);
        params["context"] = json!({ "includeDeclaration": false });
        params
    });
    assert_eq!(
        references,
        json!([{
            "uri": client.uri("Alpha.md"),
            "range": {
                "start": { "line": 7, "character": 9 },
                "end": { "line": 7, "character": 17 },
            },
        }])
    );

    let symbols = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": client.uri("Home.md") } }),
    );
    assert_eq!(symbols[0]["name"], "Home");
    assert_eq!(symbols[0]["detail"], "home@1");

    let found = client.request("workspace/symbol", json!({ "query": "first" }));
    assert_eq!(found[0]["name"], "First");
    assert_eq!(found[0]["location"]["uri"], client.uri("Alpha.md"));
}

#[test]
fn completes_and_diagnoses_unsaved_text_without_touching_the_cache() {
    let dir = vault(&[("Home.md", HOME), ("Alpha.md", ALPHA)]);
    let cache = std::fs::read(dir.path().join(".vulcan/cache.db")).unwrap();
    let (mut client, _) = Client::start(dir.path(), &json!({}));
    client.open("Draft.md", "[[Missing]]\n");
    let diagnostics = client.diagnostics("Draft.md");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["code"], "unresolved-link");

    // Typing `[[Al` at the end: completion offers Alpha, and the link being
    // typed is not reported as unresolved.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": client.uri("Draft.md"), "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": 1, "character": 0 },
                    "end": { "line": 1, "character": 0 },
                },
                "text": "[[Al",
            }],
        }),
    );
    let diagnostics = client.diagnostics("Draft.md");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    let completion = client.request("textDocument/completion", client.at("Draft.md", 1, 4));
    let item = &completion["items"][0];
    assert_eq!(item["label"], "Alpha");
    assert_eq!(item["textEdit"]["newText"], "Alpha");
    assert_eq!(
        item["textEdit"]["range"],
        json!({
            "start": { "line": 1, "character": 2 },
            "end": { "line": 1, "character": 4 },
        })
    );

    // An unsaved new note resolves for other buffers.
    client.open("Missing.md", "# Not saved yet\n");
    let _ = client.diagnostics("Missing.md");
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": client.uri("Draft.md"), "version": 3 },
            "contentChanges": [{ "text": "[[Missing]]\n" }],
        }),
    );
    assert!(client.diagnostics("Draft.md").is_empty());
    drop(client);
    assert_eq!(
        std::fs::read(dir.path().join(".vulcan/cache.db")).unwrap(),
        cache
    );
}

#[test]
fn saving_indexes_the_file_and_refreshes_open_diagnostics() {
    let dir = vault(&[("Home.md", "[[Later]]\n")]);
    let (mut client, _) = Client::start(dir.path(), &json!({}));
    client.open("Home.md", "[[Later]]\n");
    assert_eq!(client.diagnostics("Home.md").len(), 1);

    std::fs::write(dir.path().join("Later.md"), "# Later\n").unwrap();
    client.open("Later.md", "# Later\n");
    let _ = client.diagnostics("Later.md");
    client.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": client.uri("Later.md") } }),
    );
    // Every open document is re-diagnosed after the save is indexed.
    assert!(client.diagnostics("Home.md").is_empty());
    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": client.uri("Later.md") } }),
    );
    let status = client.request("vulcan/status", json!({}));
    assert_eq!(status["cache"], "fresh");
    assert_eq!(status["protocolVersion"], 1);
}

#[test]
fn reports_unsupported_methods_and_ignores_files_outside_the_vault() {
    let dir = vault(&[("Home.md", HOME)]);
    let (mut client, _) = Client::start(dir.path(), &json!({}));
    let response = client.request("vulcan/unknown", json!({}));
    assert_eq!(response["error"], -32601);
    assert_eq!(response["data"]["kind"], "invalid_request");

    let outside = tempfile::tempdir().unwrap();
    let outside_uri = uri(&outside.path().join("x.md"));
    let definition = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": outside_uri },
            "position": { "line": 0, "character": 0 },
        }),
    );
    assert_eq!(definition, Value::Null);
}

#[test]
fn warns_about_uninitialized_vaults_without_creating_one() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Home.md"), "[[Nowhere]]\n").unwrap();
    let (mut client, _) = Client::start(dir.path(), &json!({}));
    client.open("Home.md", "[[Nowhere]]\n");
    // No index means no link diagnostics rather than every link unresolved.
    assert!(client.diagnostics("Home.md").is_empty());
    assert!(client
        .notifications
        .iter()
        .any(|notification| notification.method == "window/showMessage"));
    drop(client);
    assert!(!dir.path().join(".vulcan").exists());
}

#[test]
fn the_lease_holder_watches_and_other_processes_follow_the_store_clock() {
    let dir = vault(&[("Home.md", "[[Later]]\n")]);
    let (mut watching, _) = Client::start(dir.path(), &json!({}));
    let (mut polling, _) = Client::start(dir.path(), &json!({}));
    assert_eq!(
        watching.request("vulcan/status", json!({}))["watcher"],
        "lease"
    );
    assert_eq!(
        polling.request("vulcan/status", json!({}))["watcher"],
        "polling"
    );
    for client in [&mut watching, &mut polling] {
        client.open("Home.md", "[[Later]]\n");
        assert_eq!(client.diagnostics("Home.md").len(), 1);
    }

    // A file created outside any editor is indexed by the lease holder,
    // and the other process notices through the store clock.
    std::fs::write(dir.path().join("Later.md"), "# Later\n").unwrap();
    for client in [&mut watching, &mut polling] {
        let changed = client.index_changed();
        assert!(changed.params.is_object());
        assert!(client.diagnostics("Home.md").is_empty());
    }
}

#[test]
fn client_watching_and_renames_index_reported_paths() {
    let dir = vault(&[("Home.md", "[[Later]] [[Old]]\n"), ("Old.md", "# Old\n")]);
    let (mut client, _) = Client::start_with(
        dir.path(),
        &json!({ "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } } }),
        &json!({ "watcher": "client" }),
    );
    assert_eq!(
        client.request("vulcan/status", json!({}))["watcher"],
        "client"
    );
    assert!(client
        .server_requests
        .iter()
        .any(|request| request.method == "client/registerCapability"
            && request.params["registrations"][0]["method"] == "workspace/didChangeWatchedFiles"));
    client.open("Home.md", "[[Later]] [[Old]]\n");
    assert_eq!(client.diagnostics("Home.md").len(), 1);

    std::fs::write(dir.path().join("Later.md"), "# Later\n").unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": client.uri("Later.md"), "type": 1 }] }),
    );
    let changed = client.index_changed();
    assert_eq!(changed.params["paths"], json!(["Later.md"]));
    assert!(client.diagnostics("Home.md").is_empty());

    // A rename reported by the client is indexed with a rename hint, so the
    // note keeps its identity instead of being deleted and re-added.
    std::fs::rename(dir.path().join("Old.md"), dir.path().join("New.md")).unwrap();
    client.notify(
        "workspace/didRenameFiles",
        json!({ "files": [{ "oldUri": client.uri("Old.md"), "newUri": client.uri("New.md") }] }),
    );
    let changed = client.index_changed();
    assert_eq!(changed.params["summary"]["deleted"], 0);
    assert_eq!(changed.params["summary"]["added"], 0);
    assert_eq!(client.diagnostics("Home.md").len(), 1);
}

#[test]
fn startup_indexing_reports_progress_when_the_client_supports_it() {
    let dir = vault(&[("Home.md", HOME)]);
    let (mut client, _) = Client::start(
        dir.path(),
        &json!({ "window": { "workDoneProgress": true } }),
    );
    // Requests are answered in order, so the token request has been seen
    // by the time this status reply arrives.
    let _ = client.request("vulcan/status", json!({}));
    assert!(client
        .server_requests
        .iter()
        .any(|request| request.method == "window/workDoneProgress/create"));
    // Whether indexing outlives the token handshake is a race; when a report
    // begins, it must end.
    let began = client.notifications.iter().any(|notification| {
        notification.method == "$/progress" && notification.params["value"]["kind"] == "begin"
    });
    if began {
        client.wait_for(|notification| {
            notification.method == "$/progress" && notification.params["value"]["kind"] == "end"
        });
    }
}

#[test]
fn cancelling_a_finished_or_unknown_request_is_harmless() {
    let dir = vault(&[("Home.md", HOME)]);
    let (mut client, _) = Client::start(dir.path(), &json!({}));
    client.open("Home.md", HOME);
    let _ = client.diagnostics("Home.md");
    client.notify("$/cancelRequest", json!({ "id": 999 }));
    let symbols = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": client.uri("Home.md") } }),
    );
    client.notify("$/cancelRequest", json!({ "id": client.next_id }));
    assert_eq!(symbols[0]["name"], "Home");
    assert_eq!(
        client.request("vulcan/status", json!({}))["protocolVersion"],
        1
    );
}

#[test]
fn a_held_vault_write_lock_does_not_stall_requests() {
    let dir = vault(&[("Home.md", HOME), ("Alpha.md", ALPHA)]);
    let (mut client, _) = Client::start(dir.path(), &json!({}));
    // Let the startup refresh finish before another writer takes the lock.
    let _ = client.request("vulcan/status", json!({}));
    client.open("Home.md", HOME);
    let _ = client.diagnostics("Home.md");
    let paths = VaultPaths::new(dir.path());
    let lock = vulcan_core::write_lock::acquire_write_lock(&paths).unwrap();

    // The save scan now waits for the lock on the writer thread...
    client.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": client.uri("Home.md") } }),
    );
    // ...while requests are still answered.
    let started = std::time::Instant::now();
    let definition = client.request("textDocument/definition", client.at("Home.md", 2, 8));
    assert_eq!(definition["uri"], client.uri("Alpha.md"));
    let completion = client.request("workspace/symbol", json!({ "query": "alp" }));
    assert_eq!(completion[0]["name"], "Alpha");
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(lock);
}

/// Client capabilities of an editor that applies versioned edits and
/// renames files.
fn editing_client() -> Value {
    json!({ "workspace": {
        "applyEdit": true,
        "workspaceEdit": { "documentChanges": true, "resourceOperations": ["rename"] },
    }})
}

fn applied_edits(client: &Client) -> Vec<Value> {
    client
        .server_requests
        .iter()
        .filter(|request| request.method == "workspace/applyEdit")
        .map(|request| request.params["edit"]["documentChanges"].clone())
        .collect()
}

const LINKER: &str = "# Home\n\nSee [[Alpha]] and [[Alpha#Status]].\n";

fn rename_vault() -> tempfile::TempDir {
    vault(&[
        ("Home.md", LINKER),
        ("notes/Alpha.md", "# Alpha\n\n## Status\n\nGreen.\n"),
        ("Other.md", "Also [[Alpha]].\n"),
    ])
}

fn rename_at(client: &mut Client, line: u32, character: u32, new_name: &str) -> Value {
    let mut params = client.at("Home.md", line, character);
    params["newName"] = json!(new_name);
    client.request("textDocument/rename", params)
}

#[test]
fn renaming_a_linked_note_moves_it_and_rewrites_every_link() {
    let dir = rename_vault();
    let (mut client, result) = Client::start(dir.path(), &editing_client());
    assert_eq!(
        result["capabilities"]["renameProvider"]["prepareProvider"],
        true
    );
    client.open("Home.md", LINKER);
    let _ = client.diagnostics("Home.md");

    let prepared = client.request("textDocument/prepareRename", client.at("Home.md", 2, 7));
    assert_eq!(prepared["placeholder"], "Alpha");
    assert_eq!(
        prepared["range"]["start"],
        json!({ "line": 2, "character": 6 })
    );

    let response = rename_at(&mut client, 2, 7, "Beta");
    assert_eq!(response, json!({ "documentChanges": [] }));
    // The closed note moved and the closed linker was rewritten on disk...
    assert!(!dir.path().join("notes/Alpha.md").exists());
    assert!(dir.path().join("notes/Beta.md").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Other.md")).unwrap(),
        "Also [[Beta]].\n"
    );
    // ...while the open buffer got a versioned edit and its file is untouched.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Home.md")).unwrap(),
        LINKER
    );
    let edits = applied_edits(&client);
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0][0]["textDocument"]["uri"], client.uri("Home.md"));
    assert_eq!(edits[0][0]["textDocument"]["version"], 1);
    // Applying the edits (ASCII, so columns are byte offsets) gives the
    // rewritten note.
    let mut lines = LINKER.lines().map(ToString::to_string).collect::<Vec<_>>();
    let mut edits = edits[0][0]["edits"].as_array().unwrap().clone();
    edits.sort_by_key(|edit| std::cmp::Reverse(edit["range"]["start"]["character"].as_u64()));
    for edit in edits {
        let range = &edit["range"];
        assert_eq!(range["start"]["line"], range["end"]["line"]);
        let line = &mut lines[usize::try_from(range["start"]["line"].as_u64().unwrap()).unwrap()];
        let column =
            |key: &str| usize::try_from(range[key]["character"].as_u64().unwrap()).unwrap();
        line.replace_range(
            column("start")..column("end"),
            edit["newText"].as_str().unwrap(),
        );
    }
    assert_eq!(lines[2], "See [[Beta]] and [[Beta#Status]].");
}

#[test]
fn a_rejected_edit_restores_the_vault() {
    let dir = rename_vault();
    let (mut client, _) = Client::start(dir.path(), &editing_client());
    client.accept_edits = false;
    client.open("Home.md", LINKER);
    let _ = client.diagnostics("Home.md");

    let response = rename_at(&mut client, 2, 7, "Beta");
    assert_eq!(response["data"]["kind"], "conflict");
    assert!(response["data"]["detail"]
        .as_str()
        .unwrap()
        .contains("vault was restored"));
    assert!(dir.path().join("notes/Alpha.md").exists());
    assert!(!dir.path().join("notes/Beta.md").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Other.md")).unwrap(),
        "Also [[Alpha]].\n"
    );
}

#[test]
fn moving_an_open_note_asks_the_editor_to_rename_it() {
    let dir = rename_vault();
    let (mut client, _) = Client::start(dir.path(), &editing_client());
    client.open("Home.md", LINKER);
    client.open("notes/Alpha.md", "# Alpha\n\n## Status\n\nGreen.\n");
    let _ = client.diagnostics("notes/Alpha.md");

    let result = client.request(
        "workspace/executeCommand",
        json!({ "command": "vulcan.note.move", "arguments": [{
            "from": client.uri("notes/Alpha.md"), "to": "archive/Alpha.md",
        }]}),
    );
    assert_eq!(
        result["moved"],
        json!({ "from": "notes/Alpha.md", "to": "archive/Alpha.md" })
    );
    assert!(result["editor"]
        .as_array()
        .unwrap()
        .contains(&json!("notes/Alpha.md")));
    let edits = applied_edits(&client);
    let rename = edits[0]
        .as_array()
        .unwrap()
        .iter()
        .find(|change| change["kind"] == "rename")
        .expect("the editor renames the open note");
    assert_eq!(rename["oldUri"], client.uri("notes/Alpha.md"));
    assert_eq!(rename["newUri"], client.uri("archive/Alpha.md"));
    // The editor moved the file; the index follows it.
    let found = client.request("workspace/symbol", json!({ "query": "archive" }));
    assert_eq!(found[0]["location"]["uri"], client.uri("archive/Alpha.md"));
}

#[test]
fn move_commands_support_dry_runs_and_rewrite_unsaved_buffers() {
    let dir = rename_vault();
    let (mut client, _) = Client::start(dir.path(), &editing_client());
    let dry_run = client.request(
        "workspace/executeCommand",
        json!({ "command": "vulcan.note.move", "arguments": [{
            "from": "notes/Alpha.md", "to": "Beta.md", "dryRun": true,
        }]}),
    );
    assert_eq!(dry_run["dryRun"], true);
    assert_eq!(dry_run["report"]["destination_path"], "Beta.md");
    assert!(dir.path().join("notes/Alpha.md").exists());

    // An unsaved buffer is edited from its own text, not refused.
    client.open("Other.md", "Also [[Alpha]] (edited).\n");
    let moved = client.request(
        "workspace/executeCommand",
        json!({ "command": "vulcan.note.move", "arguments": [{
            "from": "notes/Alpha.md", "to": "Beta.md",
        }]}),
    );
    assert_eq!(moved["editor"], json!(["Other.md"]), "{moved}");
    assert!(dir.path().join("Beta.md").exists());
    let edits = applied_edits(&client);
    let change = &edits[0][0];
    assert_eq!(change["textDocument"]["uri"], client.uri("Other.md"));
    assert_eq!(change["edits"][0]["newText"], "[[Beta]]");
    // The file keeps its saved text until the editor saves the buffer.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Other.md")).unwrap(),
        "Also [[Alpha]].\n"
    );
}

#[test]
fn neovim_capabilities_count_as_versioned_edit_support() {
    // Neovim lists resource operations but not `documentChanges`.
    let dir = rename_vault();
    let (mut client, _) = Client::start(
        dir.path(),
        &json!({ "workspace": {
            "applyEdit": true,
            "workspaceEdit": { "resourceOperations": ["rename", "create", "delete"] },
        }}),
    );
    client.open("Home.md", LINKER);
    let _ = client.diagnostics("Home.md");
    assert_eq!(
        rename_at(&mut client, 2, 7, "Beta"),
        json!({ "documentChanges": [] })
    );
    assert!(dir.path().join("notes/Beta.md").exists());
}

#[test]
fn editors_without_workspace_edits_cannot_rewrite_open_buffers() {
    let dir = rename_vault();
    let (mut client, _) = Client::start(dir.path(), &json!({}));
    client.open("Home.md", LINKER);
    let _ = client.diagnostics("Home.md");
    let response = rename_at(&mut client, 2, 7, "Beta");
    assert_eq!(response["data"]["kind"], "conflict");
    assert!(dir.path().join("notes/Alpha.md").exists());
}

#[test]
fn client_renames_rewrite_open_and_closed_links() {
    let dir = rename_vault();
    let (mut client, result) = Client::start(dir.path(), &editing_client());
    assert!(result["capabilities"]["workspace"]["fileOperations"]["willRename"].is_object());
    client.open("Home.md", LINKER);
    let _ = client.diagnostics("Home.md");
    let renamed = json!({ "files": [{
        "oldUri": client.uri("notes/Alpha.md"), "newUri": client.uri("notes/Beta.md"),
    }]});

    // The client asks first and applies the returned edit to its buffers...
    let edit = client.request("workspace/willRenameFiles", renamed.clone());
    assert_eq!(
        edit["documentChanges"][0]["textDocument"]["uri"],
        client.uri("Home.md")
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Other.md")).unwrap(),
        "Also [[Alpha]].\n",
        "closed files wait for the rename to happen"
    );
    // ...renames the file, and reports it; closed links are then rewritten.
    std::fs::rename(
        dir.path().join("notes/Alpha.md"),
        dir.path().join("notes/Beta.md"),
    )
    .unwrap();
    client.notify("workspace/didRenameFiles", renamed);
    client.wait_for(|notification| {
        notification.method == "vulcan/indexChanged"
            && notification.params["paths"] == json!(["notes/Beta.md"])
    });
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Other.md")).unwrap(),
        "Also [[Beta]].\n"
    );
}

fn code_actions(
    client: &mut Client,
    path: &str,
    line: u32,
    character: u32,
    only: Option<&str>,
) -> Vec<Value> {
    let mut params = client.at(path, line, character);
    let position = params["position"].clone();
    params["range"] = json!({ "start": position, "end": position });
    params["context"] = match only {
        Some(kind) => json!({ "diagnostics": [], "only": [kind] }),
        None => json!({ "diagnostics": [] }),
    };
    client
        .request("textDocument/codeAction", params)
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn code_actions_create_missing_notes_and_disambiguate_links() {
    let text = "[[Missing]] [[Same]]\n";
    let dir = vault(&[("a/Same.md", "x"), ("b/Same.md", "x"), ("Home.md", text)]);
    let (mut client, result) = Client::start(dir.path(), &editing_client());
    assert!(result["capabilities"]["codeActionProvider"].is_object());
    client.open("Home.md", text);
    assert_eq!(client.diagnostics("Home.md").len(), 2);

    let actions = code_actions(&mut client, "Home.md", 0, 3, None);
    assert_eq!(actions[0]["title"], "Create note Missing.md");
    assert_eq!(actions[0]["kind"], "quickfix");
    assert_eq!(actions[0]["isPreferred"], true);
    let command = actions[0]["command"].clone();
    assert_eq!(command["command"], "vulcan.note.create");
    let created = client.request(
        "workspace/executeCommand",
        json!({ "command": command["command"], "arguments": command["arguments"] }),
    );
    assert_eq!(created["action"], "create_note");
    assert!(dir.path().join("Missing.md").is_file());
    // The new note resolves once the server notices the index changed.
    loop {
        let diagnostics = client.diagnostics("Home.md");
        if diagnostics.len() == 1 {
            assert_eq!(diagnostics[0]["code"], "ambiguous-link");
            break;
        }
    }

    let actions = code_actions(&mut client, "Home.md", 0, 14, None);
    assert_eq!(actions.len(), 2);
    let change = &actions[1]["edit"]["documentChanges"][0];
    assert_eq!(change["textDocument"]["version"], 1);
    assert_eq!(change["edits"][0]["newText"], "b/Same");
    assert!(code_actions(&mut client, "Home.md", 0, 14, Some("refactor")).is_empty());
}

#[test]
fn toggling_a_task_edits_the_open_buffer_and_inserts_the_next_occurrence() {
    let text = "# Chores\n- [ ] Water plants 🔁 every week 📅 2026-10-12\n";
    let dir = vault(&[("Chores.md", text)]);
    let (mut client, _) = Client::start(dir.path(), &editing_client());
    client.open("Chores.md", text);
    let _ = client.diagnostics("Chores.md");

    let actions = code_actions(&mut client, "Chores.md", 1, 4, None);
    let toggle = actions
        .iter()
        .find(|action| action["title"] == "Complete task")
        .expect("a task line offers a toggle");
    let mut arguments = toggle["command"]["arguments"].clone();
    arguments[0]["today"] = json!("2026-10-13");
    let result = client.request(
        "workspace/executeCommand",
        json!({ "command": "vulcan.task.toggle", "arguments": arguments }),
    );
    assert_eq!(result["action"], "toggle_task");
    assert_eq!(result["report"]["completed"], true);
    assert_eq!(
        result["report"]["next_occurrence"],
        "- [ ] Water plants 🔁 every week 📅 2026-10-19"
    );
    assert_eq!(result["editor"], json!(["Chores.md"]));
    // The open buffer got the edit; the file on disk is untouched.
    let edits = applied_edits(&client);
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0][0]["textDocument"]["version"], 1);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Chores.md")).unwrap(),
        text
    );

    // Toggling in a file that does not exist reports it.
    let closed = client.request(
        "workspace/executeCommand",
        json!({ "command": "vulcan.task.toggle", "arguments": [{
            "path": "Other.md", "line": 0,
        }]}),
    );
    assert_eq!(closed["data"]["kind"], "not_found");
}

const DASHBOARD: &str = "---\nstatus: draft\n---\nStatus: `= this.status`\n\n```dataviewjs\ndv.paragraph(\"ran\")\n```\n\n```tasks\nnot done\n```\n";

#[test]
fn render_returns_evaluated_regions_with_sandboxed_dataview_js() {
    let dir = vault(&[
        ("Dashboard.md", DASHBOARD),
        ("Todo.md", "- [ ] Open item\n"),
    ]);
    let (mut client, _) = Client::start(dir.path(), &json!({}));
    client.open("Dashboard.md", DASHBOARD);
    let _ = client.diagnostics("Dashboard.md");
    let document = json!({ "uri": client.uri("Dashboard.md") });

    // A temporary vault is never trusted, so DataviewJS runs in the strict
    // sandbox by default.
    let rendered = client.request("vulcan/render", json!({ "textDocument": document }));
    assert_eq!(rendered["dataviewJs"], "static");
    let regions = rendered["regions"].as_array().unwrap();
    let sources = regions
        .iter()
        .map(|region| region["source"].as_str().unwrap())
        .collect::<Vec<_>>();
    if cfg!(feature = "js_runtime") {
        assert_eq!(
            sources,
            vec!["inline", "dataviewjs", "tasks"],
            "{regions:?}"
        );
        assert_eq!(regions[1]["markdown"], "ran");
    } else {
        // Without a JavaScript engine the block renders as a notice.
        assert_eq!(sources, vec!["inline", "notice", "tasks"], "{regions:?}");
    }
    assert_eq!(regions[0]["markdown"], "draft");
    assert_eq!(regions[0]["kind"], "inline");
    assert_eq!(
        regions[0]["range"]["start"],
        json!({ "line": 3, "character": 8 })
    );
    assert!(regions[2]["markdown"]
        .as_str()
        .unwrap()
        .contains("Open item"));

    // Full evaluation needs `vulcan trust`; an untrusted vault stays in the
    // strict sandbox.
    let rendered = client.request(
        "vulcan/render",
        json!({ "textDocument": document, "dataviewJs": "on" }),
    );
    assert_eq!(rendered["dataviewJs"], "static");

    // A client can still turn DataviewJS off.
    let rendered = client.request(
        "vulcan/render",
        json!({ "textDocument": document, "dataviewJs": "off" }),
    );
    assert_eq!(rendered["dataviewJs"], "off");
    assert_eq!(rendered["regions"][1]["source"], "notice");
}

#[test]
fn scan_periodic_and_template_commands() {
    let dir = vault(&[("Note.md", "# Note\n\nbody\n")]);
    std::fs::create_dir_all(dir.path().join(".vulcan/templates")).unwrap();
    std::fs::write(dir.path().join(".vulcan/templates/Sig.md"), "-- {{title}}").unwrap();
    std::fs::write(
        dir.path().join(".vulcan/config.toml"),
        "[periodic.daily]\nfolder = \"Journal\"\nformat = \"YYYY-MM-DD\"\n",
    )
    .unwrap();
    let mut capabilities = editing_client();
    capabilities["window"] = json!({ "showDocument": { "support": true } });
    let (mut client, _) = Client::start(dir.path(), &capabilities);

    let scanned = client.request(
        "workspace/executeCommand",
        json!({ "command": "vulcan.scan", "arguments": [{ "full": true }] }),
    );
    assert_eq!(scanned["summary"]["mode"], "full");

    let opened = client.request(
        "workspace/executeCommand",
        json!({ "command": "vulcan.periodic.open", "arguments": [{ "date": "2026-10-13" }] }),
    );
    assert_eq!(opened["report"]["path"], "Journal/2026-10-13.md");
    assert_eq!(opened["report"]["created"], true);
    assert!(dir.path().join("Journal/2026-10-13.md").is_file());
    assert!(client.server_requests.iter().any(|request| {
        request.method == "window/showDocument"
            && request.params["uri"] == client.uri("Journal/2026-10-13.md")
    }));

    client.open("Note.md", "# Note\n\nbody\n");
    let _ = client.diagnostics("Note.md");
    let inserted = client.request(
        "workspace/executeCommand",
        json!({ "command": "vulcan.template.insert", "arguments": [{
            "path": client.uri("Note.md"),
            "template": "Sig",
            "position": { "line": 2, "character": 4 },
        }]}),
    );
    assert_eq!(inserted["action"], "template_insert");
    let edits = applied_edits(&client);
    let new_text = edits[0][0]["edits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|edit| edit["newText"].as_str().unwrap())
        .collect::<String>();
    assert!(new_text.contains("-- Note"), "{new_text}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Note.md")).unwrap(),
        "# Note\n\nbody\n"
    );
}

#[test]
fn permission_profiles_limit_what_the_editor_can_change() {
    let dir = vault(&[
        ("Projects/Plan.md", "- [ ] Ship\n"),
        ("Private/Diary.md", "- [ ] Secret\n"),
    ]);
    std::fs::write(
        dir.path().join(".vulcan/config.toml"),
        "[permissions.profiles.projects]\nread = \"all\"\nwrite = { allow = [\"folder:Projects/**\"] }\nindex = \"deny\"\n",
    )
    .unwrap();
    let (mut client, _) = Client::start_full(
        dir.path(),
        &editing_client(),
        &Value::Null,
        LspOptions {
            vault: None,
            permissions: Some("projects".to_string()),
        },
    );
    let toggle = |client: &mut Client, path: &str| {
        client.request(
            "workspace/executeCommand",
            json!({ "command": "vulcan.task.toggle", "arguments": [{
                "path": path, "line": 0, "dryRun": true,
            }]}),
        )
    };
    assert_eq!(toggle(&mut client, "Projects/Plan.md")["dryRun"], true);
    assert_eq!(
        toggle(&mut client, "Private/Diary.md")["data"]["kind"],
        "permission_denied"
    );
    let scan = client.request(
        "workspace/executeCommand",
        json!({ "command": "vulcan.scan", "arguments": [{}] }),
    );
    assert_eq!(scan["data"]["kind"], "permission_denied");
}

/// Copy a shared fixture vault from `tests/fixtures/vaults/` and index it.
fn fixture_vault(name: &str) -> tempfile::TempDir {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/vaults")
        .join(name);
    let dir = tempfile::tempdir().unwrap();
    copy(&source, dir.path());
    let paths = VaultPaths::new(dir.path());
    vulcan_core::paths::initialize_vulcan_dir(&paths).unwrap();
    scan_vault(&paths, ScanMode::Full).unwrap();
    dir
}

#[test]
fn outline_and_backlinks_match_the_cli_reports_on_fixture_vaults() {
    let dir = fixture_vault("basic");
    let paths = VaultPaths::new(dir.path());
    let (mut client, _) = Client::start(dir.path(), &json!({}));
    for path in ["Home.md", "Projects/Alpha.md", "People/Bob.md"] {
        let text = std::fs::read_to_string(dir.path().join(path)).unwrap();
        client.open(path, &text);
        let _ = client.diagnostics(path);
        let document = json!({ "textDocument": { "uri": client.uri(path) } });

        let outline = client.request("vulcan/outline", document.clone());
        let cli_outline = vulcan_app::notes::read_note_outline(&paths, path, None, None).unwrap();
        assert_eq!(
            outline,
            serde_json::to_value(cli_outline).unwrap(),
            "{path}"
        );

        let backlinks = client.request("vulcan/backlinks", document);
        let cli_backlinks = vulcan_core::query_backlinks(&paths, path).unwrap();
        assert_eq!(
            backlinks,
            serde_json::to_value(cli_backlinks).unwrap(),
            "{path}"
        );
    }

    // The outline follows unsaved headings; a never-saved note has no
    // backlinks report.
    client.open("Draft.md", "# Draft\n\n## Unsaved section\n");
    let _ = client.diagnostics("Draft.md");
    let document = json!({ "textDocument": { "uri": client.uri("Draft.md") } });
    let outline = client.request("vulcan/outline", document.clone());
    assert_eq!(outline["sections"][1]["heading"], "Unsaved section");
    assert_eq!(client.request("vulcan/backlinks", document), Value::Null);
}

#[test]
fn diagnostics_on_fixture_vaults_use_doctor_categories() {
    let dir = fixture_vault("ambiguous-links");
    let paths = VaultPaths::new(dir.path());
    let doctor = vulcan_core::doctor_vault(&paths).unwrap();
    let issue = &doctor.ambiguous_links[0];
    let path = issue.document_path.clone().unwrap();
    let (mut client, _) = Client::start(dir.path(), &json!({}));
    let text = std::fs::read_to_string(dir.path().join(&path)).unwrap();
    client.open(&path, &text);
    let diagnostics = client.diagnostics(&path);
    let ambiguous = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["code"] == "ambiguous-link")
        .expect("the editor reports the ambiguous link doctor reports");
    assert_eq!(
        ambiguous["data"]["candidates"],
        serde_json::to_value(&issue.matches).unwrap()
    );
    // Doctor's 1-based line is the editor's 0-based line plus one.
    assert_eq!(
        ambiguous["range"]["start"]["line"]
            .as_u64()
            .map(|line| line + 1),
        issue.line.map(|line| line as u64)
    );
}

#[test]
fn code_lenses_count_backlinks_and_inlay_hints_show_due_dates() {
    let dir = vault(&[
        (
            "Home.md",
            "# Home\n- [ ] due 📅 2000-01-01\n- [x] done 📅 2000-01-01\n",
        ),
        ("A.md", "[[Home]]\n"),
    ]);
    let (mut client, result) = Client::start(dir.path(), &json!({}));
    assert!(result["capabilities"]["codeLensProvider"].is_object());
    assert_eq!(result["capabilities"]["inlayHintProvider"], true);
    client.open(
        "Home.md",
        "# Home\n- [ ] due 📅 2000-01-01\n- [x] done 📅 2000-01-01\n",
    );
    let _ = client.diagnostics("Home.md");
    let document = json!({ "uri": client.uri("Home.md") });

    let lenses = client.request("textDocument/codeLens", json!({ "textDocument": document }));
    assert_eq!(lenses[0]["command"]["title"], "1 backlink");
    assert_eq!(lenses[0]["command"]["command"], "vulcan.showBacklinks");

    let hints = client.request(
        "textDocument/inlayHint",
        json!({ "textDocument": document, "range": {
            "start": { "line": 0, "character": 0 }, "end": { "line": 10, "character": 0 },
        }}),
    );
    let hints = hints.as_array().unwrap();
    assert_eq!(hints.len(), 1, "{hints:?}");
    assert!(hints[0]["label"]
        .as_str()
        .unwrap()
        .starts_with("overdue by "));
    assert_eq!(hints[0]["position"]["line"], 1);
}

#[test]
fn editor_config_turns_features_off() {
    let dir = vault(&[("Home.md", "# Home\n")]);
    std::fs::write(
        dir.path().join(".vulcan/config.toml"),
        "[editor]\ncode_lens = false\ninlay_hints = false\nsemantic_tokens = false\n",
    )
    .unwrap();
    let (_client, result) = Client::start(dir.path(), &json!({}));
    assert!(result["capabilities"]["codeLensProvider"].is_null());
    assert!(result["capabilities"]["inlayHintProvider"].is_null());
    assert!(result["capabilities"]["semanticTokensProvider"].is_null());
}

#[test]
fn semantic_tokens_mark_unresolved_links() {
    let text = "[[Home]] [[Gone]]\n#tag\n";
    let dir = vault(&[("Home.md", "# Home\n"), ("Note.md", text)]);
    let (mut client, result) = Client::start(dir.path(), &json!({}));
    let legend = &result["capabilities"]["semanticTokensProvider"]["legend"];
    assert_eq!(legend["tokenTypes"][0], "wikiLink");
    assert_eq!(legend["tokenTypes"][3], "tag");
    assert_eq!(legend["tokenModifiers"], json!(["unresolved", "ambiguous"]));
    client.open("Note.md", text);
    let _ = client.diagnostics("Note.md");
    let tokens = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": client.uri("Note.md") } }),
    );
    // [[Home]] at 0:0, [[Gone]] (unresolved) at 0:9, #tag at 1:0.
    assert_eq!(
        tokens["data"],
        json!([0, 0, 8, 0, 0, 0, 9, 8, 0, 1, 1, 0, 4, 3, 0])
    );
}

#[test]
fn workspace_diagnostics_report_closed_notes() {
    let dir = vault(&[
        ("Home.md", "# Home\n"),
        ("Broken.md", "ok\n[[Gone]]\n"),
        ("Open.md", "[[Gone]]\n"),
    ]);
    let (mut client, result) = Client::start(dir.path(), &json!({}));
    assert_eq!(
        result["capabilities"]["diagnosticProvider"]["workspaceDiagnostics"],
        true
    );
    client.open("Open.md", "[[Gone]]\n");
    let pushed = client.diagnostics("Open.md");
    assert_eq!(pushed.len(), 1);

    // An open document's problems are pushed, not repeated in a pull.
    let document = client.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": client.uri("Open.md") } }),
    );
    assert_eq!(document, json!({ "kind": "full", "items": [] }));

    let workspace = client.request(
        "workspace/diagnostic",
        json!({ "previousResultIds": [
            { "uri": client.uri("Open.md"), "value": "vulcan" },
        ]}),
    );
    let items = workspace["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0]["uri"], json!(client.uri("Broken.md")));
    assert_eq!(items[0]["items"][0]["code"], "unresolved-link");
    assert_eq!(items[0]["items"][0]["range"]["start"]["line"], 1);
    // Open.md was reported before and is open now: cleared.
    assert_eq!(items[1]["uri"], json!(client.uri("Open.md")));
    assert_eq!(items[1]["items"], json!([]));
}

#[test]
fn workspace_diagnostics_refresh_only_after_a_pull() {
    let dir = vault(&[("Home.md", "# Home\n")]);
    let (mut client, _) = Client::start(
        dir.path(),
        &json!({ "workspace": { "diagnostic": { "refreshSupport": true } } }),
    );
    let _ = client.request("vulcan/status", json!({}));
    let refreshes = |client: &Client| {
        client
            .server_requests
            .iter()
            .filter(|request| request.method == "workspace/diagnostic/refresh")
            .count()
    };
    let save = |client: &mut Client, text: &str| {
        std::fs::write(dir.path().join("Home.md"), text).unwrap();
        client.notify(
            "textDocument/didSave",
            json!({ "textDocument": { "uri": client.uri("Home.md") } }),
        );
        let _ = client.index_changed();
        // Replies arrive in order, after any refresh sent with the change.
        let _ = client.request("vulcan/status", json!({}));
    };

    save(&mut client, "# Home\n[[Gone]]\n");
    assert_eq!(refreshes(&client), 0);
    let _ = client.request("workspace/diagnostic", json!({ "previousResultIds": [] }));
    save(&mut client, "# Home\n");
    assert_eq!(refreshes(&client), 1);
}

#[test]
fn extracts_a_section_through_a_code_action() {
    let source = "# Home\n\n## Plan\n\nSteps\n\n## Done\n";
    let dir = vault(&[("Home.md", source), ("Other.md", "[[Home#Plan]]\n")]);
    let (mut client, _) = Client::start(dir.path(), &editing_client());
    client.open("Home.md", source);
    let _ = client.diagnostics("Home.md");
    let actions = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": client.uri("Home.md") },
            "range": { "start": { "line": 2, "character": 3 }, "end": { "line": 2, "character": 3 } },
            "context": { "diagnostics": [] },
        }),
    );
    let extract = actions
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["kind"] == "refactor.extract")
        .expect("an extract action on a heading");
    let command = &extract["command"];
    assert_eq!(command["command"], "vulcan.section.extract");

    let report = client.request(
        "workspace/executeCommand",
        json!({ "command": command["command"], "arguments": command["arguments"] }),
    );
    assert_eq!(report["dryRun"], false, "{report}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Plan.md")).unwrap(),
        "Steps\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Other.md")).unwrap(),
        "[[Plan]]\n"
    );
    let edits = applied_edits(&client);
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0][0]["textDocument"]["uri"],
        json!(client.uri("Home.md"))
    );
}

#[test]
fn workspace_folders_in_several_vaults_get_a_server_each() {
    let first = vault(&[("Home.md", "[[Alpha]]\n"), ("Alpha.md", "# Alpha\n")]);
    let second = vault(&[("Note.md", "[[Gone]]\n"), ("Beta.md", "# Beta\n")]);
    let third = vault(&[("Gamma.md", "[[Home]]\n")]);
    let folder = |dir: &tempfile::TempDir| json!({ "uri": uri(dir.path()), "name": "v" });
    let (mut client, result) = Client::start_initialize(
        Path::new("/"),
        json!({
            "processId": null,
            "rootUri": uri(first.path()),
            "workspaceFolders": [folder(&first), folder(&second)],
            "capabilities": editing_client(),
        }),
        LspOptions::default(),
    );
    assert_eq!(
        result["capabilities"]["workspace"]["workspaceFolders"]["changeNotifications"],
        true
    );
    let path = |dir: &tempfile::TempDir, name: &str| dir.path().join(name).display().to_string();

    // Each document is diagnosed and resolved in its own vault.
    client.open(&path(&first, "Home.md"), "[[Alpha]]\n");
    client.open(&path(&second, "Note.md"), "[[Gone]] [[Beta]]\n");
    assert!(client.diagnostics(&path(&first, "Home.md")).is_empty());
    assert_eq!(client.diagnostics(&path(&second, "Note.md")).len(), 1);
    let definition = client.request(
        "textDocument/definition",
        client.at(&path(&second, "Note.md"), 0, 12),
    );
    assert_eq!(
        definition["uri"],
        json!(uri(&second.path().join("Beta.md")))
    );

    // Workspace requests merge every vault's answer.
    let symbols = client.request("workspace/symbol", json!({ "query": "" }));
    let names = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(names.contains(&"Alpha".to_string()) && names.contains(&"Beta".to_string()));

    // Edits a vault server asks the client to apply reach it and back.
    let moved = client.request(
        "workspace/executeCommand",
        json!({ "command": "vulcan.note.move", "arguments": [{
            "from": uri(&second.path().join("Beta.md")), "to": "Moved.md",
        }]}),
    );
    assert_eq!(moved["dryRun"], false, "{moved}");
    assert!(second.path().join("Moved.md").exists());
    let edits = applied_edits(&client);
    assert_eq!(
        edits.last().unwrap()[0]["textDocument"]["uri"],
        json!(uri(&second.path().join("Note.md")))
    );

    // Requests about a document, and commands naming one, reach its vault.
    let status = client.request(
        "vulcan/status",
        json!({ "textDocument": { "uri": uri(&second.path().join("Note.md")) } }),
    );
    assert_eq!(status["vault"], json!(second.path()));
    let captured = client.request(
        "workspace/executeCommand",
        json!({ "command": "vulcan.capture", "arguments": [{
            "text": "an idea", "uri": uri(&second.path().join("Note.md")),
        }]}),
    );
    assert_eq!(captured["dryRun"], false, "{captured}");
    assert!(!first.path().join("Inbox.md").exists());
    assert!(std::fs::read_to_string(second.path().join("Inbox.md"))
        .unwrap()
        .contains("an idea"));

    // Folders can be added later, and removed.
    client.notify(
        "workspace/didChangeWorkspaceFolders",
        json!({ "event": { "added": [folder(&third)], "removed": [folder(&second)] } }),
    );
    client.open(&path(&third, "Gamma.md"), "[[Home]]\n");
    assert_eq!(client.diagnostics(&path(&third, "Gamma.md")).len(), 1);
    let symbols = client.request("workspace/symbol", json!({ "query": "" }));
    let names = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(names.contains(&"Gamma".to_string()));
    assert!(!names.contains(&"Beta".to_string()), "{names:?}");
}

#[test]
fn index_changes_after_a_move_name_the_moved_note() {
    let dir = rename_vault();
    let (mut client, _) = Client::start(dir.path(), &editing_client());
    client.open("Home.md", LINKER);
    let _ = client.diagnostics("Home.md");
    let moved = client.request(
        "workspace/executeCommand",
        json!({ "command": "vulcan.note.move", "arguments": [{
            "from": "Home.md", "to": "archive/Home.md",
        }]}),
    );
    assert_eq!(moved["dryRun"], false, "{moved}");
    let changed = client.wait_for(|notification| {
        notification.method == "vulcan/indexChanged" && !notification.params["renames"].is_null()
    });
    assert_eq!(
        changed.params["renames"],
        json!([{ "from": "Home.md", "to": "archive/Home.md" }])
    );
}
