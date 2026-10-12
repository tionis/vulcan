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
        let (server, client) = Connection::memory();
        let handle = std::thread::spawn(move || {
            vulcan_lsp::run(&server, &LspOptions::default()).unwrap();
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
        let result = client.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": uri(root),
                "capabilities": capabilities,
                "initializationOptions": options,
            }),
        );
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
fn move_commands_support_dry_runs_and_refuse_unsaved_buffers() {
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

    client.open("Other.md", "Also [[Alpha]] (edited).\n");
    let refused = client.request(
        "workspace/executeCommand",
        json!({ "command": "vulcan.note.move", "arguments": [{
            "from": "notes/Alpha.md", "to": "Beta.md",
        }]}),
    );
    assert_eq!(refused["data"]["kind"], "conflict");
    assert!(refused["data"]["detail"]
        .as_str()
        .unwrap()
        .contains("Other.md"));
    assert!(dir.path().join("notes/Alpha.md").exists());
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
