//! The message loop: protocol state, routing, and background work.
//!
//! Threads (`docs/specs/editor-integration.md`, "Process and threading
//! model"):
//!
//! - the main loop applies buffer changes inline, debounces diagnostics,
//!   and hands every request a snapshot of the session;
//! - a small reader pool answers requests from those snapshots;
//! - one writer thread runs every scan this process starts, so a vault
//!   write lock held elsewhere never stalls requests;
//! - a watcher thread runs only while this process holds the vault's
//!   editor watch lease; other processes follow the store clock.

use crate::convert;
use crate::handlers;
use crate::mutations::{self, ClientChannel, EditSupport, MutationContext, PendingRenames};
use crossbeam_channel::{after, never, select, unbounded, Receiver, Sender};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::Notification as _;
use lsp_types::{self as lsp, notification};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vulcan_app::editor::{EditorSession, EditorTextChange, EditorWatchLease};
use vulcan_app::AppError;
use vulcan_core::editor::TextPosition;
use vulcan_core::{ScanSummary, VaultPaths};

/// Threads answering requests.
const READER_THREADS: usize = 3;
/// Quiet time after an edit before its diagnostics are recomputed.
/// How often a process without the watch lease checks the store clock.
const CLOCK_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Token of the startup indexing progress report.
const INDEX_PROGRESS_TOKEN: &str = "vulcan/index";
const WATCHER_THREAD: &str = "vulcan-lsp-watcher";

/// Process-level options, from the command line.
#[derive(Debug, Clone, Default)]
pub struct LspOptions {
    /// An explicitly named vault, overriding discovery from the client's
    /// workspace.
    pub vault: Option<PathBuf>,
    /// The permission profile every read and write runs under.
    pub permissions: Option<String>,
}

/// A failure that ends the server.
#[derive(Debug)]
pub struct LspError(String);

impl fmt::Display for LspError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LspError {}

impl LspError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl From<lsp_server::ProtocolError> for LspError {
    fn from(error: lsp_server::ProtocolError) -> Self {
        Self(error.to_string())
    }
}

impl From<serde_json::Error> for LspError {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}

/// How the server learns about files changed outside the editor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum WatcherMode {
    /// One process per vault watches (the lease holder); others poll.
    #[default]
    Own,
    /// Rely on the client's `workspace/didChangeWatchedFiles`.
    Client,
    /// Only index saved files.
    Off,
}

/// `initializationOptions` the server understands.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializationOptions {
    vault: Option<PathBuf>,
    #[serde(default)]
    watcher: WatcherMode,
    /// The `DataviewJS` policy `vulcan/render` uses unless a request asks
    /// for another. Unset, trusted vaults get `on` and others `static`;
    /// `on` always falls back to `static` for an untrusted vault.
    #[serde(default)]
    dataview_js: Option<vulcan_core::DataviewJsRenderPolicy>,
}

/// Serve LSP over stdin and stdout until the client exits.
pub fn run_stdio(options: &LspOptions) -> Result<(), LspError> {
    let (connection, io_threads) = Connection::stdio();
    run(&connection, options)?;
    drop(connection);
    io_threads
        .join()
        .map_err(|error| LspError(error.to_string()))
}

/// Serve LSP over `connection` until the client exits.
pub fn run(connection: &Connection, options: &LspOptions) -> Result<(), LspError> {
    let (id, raw_params) = connection.initialize_start()?;
    let params: lsp::InitializeParams = serde_json::from_value(raw_params.clone())?;
    // Clients encode "no options" variously: absent, `null`, or an empty
    // array (Neovim's empty Lua table).
    let initialization = params
        .initialization_options
        .clone()
        .filter(Value::is_object)
        .map(serde_json::from_value::<InitializationOptions>)
        .transpose()?
        .unwrap_or_default();
    if options.vault.is_none() && initialization.vault.is_none() {
        // Folders spanning several vaults get one server per vault.
        let roots = crate::multiroot::folder_vaults(&params);
        if roots.len() > 1 {
            return crate::multiroot::serve(connection, options, id, raw_params, roots);
        }
    }
    let (encoding, encoding_kind) = convert::negotiate_encoding(&params.capabilities);
    let vault_root = vault_root(options, initialization.vault.clone(), &params)?;
    let mut session = EditorSession::new(VaultPaths::new(&vault_root), encoding);
    if let Some(profile) = options.permissions.as_deref() {
        session
            .set_permission_profile(profile)
            .map_err(|error| LspError(error.to_string()))?;
    }

    let result = lsp::InitializeResult {
        capabilities: capabilities(encoding_kind, &session),
        server_info: Some(lsp::ServerInfo {
            name: "vulcan".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    };
    connection.initialize_finish(id, serde_json::to_value(result)?)?;

    let client = ClientFeatures::from(&params.capabilities);
    let mut server = Server::start(connection, session, initialization.watcher, client);
    server.dataview_js = initialization.dataview_js;
    let outcome = server.serve();
    server.stop();
    outcome
}

fn vault_root(
    options: &LspOptions,
    initialization_vault: Option<PathBuf>,
    params: &lsp::InitializeParams,
) -> Result<PathBuf, LspError> {
    if let Some(named) = options.vault.clone().or(initialization_vault) {
        return vulcan_core::vault_discovery::resolve_named_vault(&named)
            .map(|discovery| discovery.root)
            .map_err(|error| LspError(error.to_string()));
    }
    #[allow(deprecated)] // Older clients only send `rootUri`.
    let workspace = params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
        .map(|folder| folder.uri.clone())
        .or_else(|| params.root_uri.clone())
        .and_then(|uri| convert::file_path(&uri));
    let start = match workspace {
        Some(start) => start,
        None => std::env::current_dir().map_err(|error| LspError(error.to_string()))?,
    };
    vulcan_core::vault_discovery::discover_vault_root(&start)
        .map(|discovery| discovery.root)
        .map_err(|error| LspError(error.to_string()))
}

fn capabilities(
    encoding: lsp::PositionEncodingKind,
    session: &EditorSession,
) -> lsp::ServerCapabilities {
    let any_file = lsp::FileOperationRegistrationOptions {
        filters: vec![lsp::FileOperationFilter {
            scheme: Some("file".to_string()),
            pattern: lsp::FileOperationPattern {
                glob: "**/*".to_string(),
                matches: Some(lsp::FileOperationPatternKind::File),
                options: None,
            },
        }],
    };
    lsp::ServerCapabilities {
        position_encoding: Some(encoding),
        text_document_sync: Some(lsp::TextDocumentSyncCapability::Options(
            lsp::TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(lsp::TextDocumentSyncKind::INCREMENTAL),
                save: Some(lsp::TextDocumentSyncSaveOptions::SaveOptions(
                    lsp::SaveOptions {
                        include_text: Some(false),
                    },
                )),
                ..lsp::TextDocumentSyncOptions::default()
            },
        )),
        definition_provider: Some(lsp::OneOf::Left(true)),
        references_provider: Some(lsp::OneOf::Left(true)),
        hover_provider: Some(lsp::HoverProviderCapability::Simple(true)),
        completion_provider: Some(lsp::CompletionOptions {
            trigger_characters: Some(vec!["[".into(), "#".into(), "^".into()]),
            ..lsp::CompletionOptions::default()
        }),
        document_symbol_provider: Some(lsp::OneOf::Left(true)),
        workspace_symbol_provider: Some(lsp::OneOf::Left(true)),
        code_lens_provider: session
            .config()
            .editor
            .code_lens
            .then_some(lsp::CodeLensOptions {
                resolve_provider: Some(false),
            }),
        inlay_hint_provider: session
            .config()
            .editor
            .inlay_hints
            .then_some(lsp::OneOf::Left(true)),
        // Pull diagnostics cover closed notes; open ones are pushed.
        diagnostic_provider: Some(lsp::DiagnosticServerCapabilities::Options(
            lsp::DiagnosticOptions {
                identifier: Some("vulcan".to_string()),
                inter_file_dependencies: true,
                workspace_diagnostics: true,
                ..lsp::DiagnosticOptions::default()
            },
        )),
        semantic_tokens_provider: session.config().editor.semantic_tokens.then(|| {
            lsp::SemanticTokensOptions {
                legend: crate::handlers::semantic_token_legend(),
                full: Some(lsp::SemanticTokensFullOptions::Bool(true)),
                ..lsp::SemanticTokensOptions::default()
            }
            .into()
        }),
        rename_provider: Some(lsp::OneOf::Right(lsp::RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
        })),
        code_action_provider: Some(lsp::CodeActionProviderCapability::Options(
            lsp::CodeActionOptions {
                code_action_kinds: Some(vec![
                    lsp::CodeActionKind::QUICKFIX,
                    lsp::CodeActionKind::REFACTOR_REWRITE,
                    lsp::CodeActionKind::REFACTOR_EXTRACT,
                ]),
                ..lsp::CodeActionOptions::default()
            },
        )),
        execute_command_provider: Some(lsp::ExecuteCommandOptions {
            commands: mutations::COMMANDS
                .iter()
                .map(ToString::to_string)
                .collect(),
            work_done_progress_options: lsp::WorkDoneProgressOptions::default(),
        }),
        workspace: Some(lsp::WorkspaceServerCapabilities {
            workspace_folders: None,
            file_operations: Some(lsp::WorkspaceFileOperationsServerCapabilities {
                did_rename: Some(any_file.clone()),
                will_rename: Some(any_file),
                ..lsp::WorkspaceFileOperationsServerCapabilities::default()
            }),
        }),
        experimental: Some(json!({
            "vulcan": {
                "protocolVersion": crate::PROTOCOL_VERSION,
                "vault": {
                    "root": session.paths().vault_root(),
                    "cacheFresh": session.has_cache(),
                },
                "commands": mutations::COMMANDS,
            }
        })),
        ..lsp::ServerCapabilities::default()
    }
}

/// Optional client features the server adapts to.
#[derive(Debug, Clone, Copy)]
struct ClientFeatures {
    work_done_progress: bool,
    dynamic_file_watching: bool,
    edits: EditSupport,
    /// Which `workspace/*/refresh` requests the client accepts after index
    /// changes.
    refresh: RefreshSupport,
}

/// One flag per client capability, so plain bools read best.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default)]
struct RefreshSupport {
    code_lens: bool,
    inlay_hint: bool,
    semantic_tokens: bool,
    diagnostics: bool,
    workspace_diagnostics_pulled: bool,
}

impl From<&lsp::ClientCapabilities> for ClientFeatures {
    fn from(capabilities: &lsp::ClientCapabilities) -> Self {
        Self {
            work_done_progress: capabilities
                .window
                .as_ref()
                .and_then(|window| window.work_done_progress)
                .unwrap_or(false),
            dynamic_file_watching: capabilities
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.did_change_watched_files.as_ref())
                .and_then(|watched| watched.dynamic_registration)
                .unwrap_or(false),
            edits: EditSupport::from(capabilities),
            refresh: capabilities
                .workspace
                .as_ref()
                .map(|workspace| RefreshSupport {
                    code_lens: workspace
                        .code_lens
                        .as_ref()
                        .and_then(|lens| lens.refresh_support)
                        .unwrap_or(false),
                    inlay_hint: workspace
                        .inlay_hint
                        .as_ref()
                        .and_then(|hint| hint.refresh_support)
                        .unwrap_or(false),
                    semantic_tokens: workspace
                        .semantic_tokens
                        .as_ref()
                        .and_then(|tokens| tokens.refresh_support)
                        .unwrap_or(false),
                    diagnostics: workspace
                        .diagnostic
                        .as_ref()
                        .and_then(|diagnostic| diagnostic.refresh_support)
                        .unwrap_or(false),
                    workspace_diagnostics_pulled: false,
                })
                .unwrap_or_default(),
        }
    }
}

/// Work for the writer thread.
enum WriteJob {
    /// Bring the cache up to date with disk at startup.
    Refresh,
    /// Index changed paths, with rename hints (destination to source).
    Scan {
        paths: Vec<String>,
        renames: HashMap<String, String>,
    },
    /// Answer a rename, move, or command from a snapshot of the session.
    Mutate {
        session: Box<EditorSession>,
        request: Request,
    },
    /// Publish the link rewrites planned for a client rename that happened.
    ApplyPending { from: String, to: String },
}

/// An index change (or failure) reported to the main loop by the writer
/// or the watcher.
struct Indexed {
    startup: bool,
    paths: Vec<String>,
    summary: Option<ScanSummary>,
    error: Option<String>,
    /// Notes this server moved (source, destination), for clients that
    /// rename files without re-pointing their buffers.
    renames: Vec<(String, String)>,
}

/// How this process keeps up with external changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Freshness {
    Lease,
    Polling,
    Client,
    Off,
}

impl Freshness {
    fn name(self) -> &'static str {
        match self {
            Self::Lease => "lease",
            Self::Polling => "polling",
            Self::Client => "client",
            Self::Off => "off",
        }
    }
}

/// Startup indexing progress, which needs a client-acknowledged token.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Progress {
    None,
    /// `window/workDoneProgress/create` sent with this id.
    Requested(RequestId),
    Begun,
}

type Job = Box<dyn FnOnce() + Send>;

struct Server<'c> {
    connection: &'c Connection,
    session: EditorSession,
    client: ClientFeatures,
    freshness: Freshness,
    /// The last edit position per open document, treated as the cursor.
    cursors: HashMap<String, TextPosition>,
    /// Documents with diagnostics due, and when.
    pending_diagnostics: HashMap<String, Instant>,
    jobs: Option<Sender<Job>>,
    writes: Option<Sender<WriteJob>>,
    indexed: Receiver<Indexed>,
    threads: Vec<std::thread::JoinHandle<()>>,
    /// Cancellation flags of requests in flight.
    in_flight: Arc<Mutex<HashMap<RequestId, Arc<AtomicBool>>>>,
    /// Advances whenever the index changes; a request that spans a change
    /// answers `ContentModified`.
    index_epoch: Arc<AtomicU64>,
    last_clock: Option<i64>,
    next_clock_poll: Instant,
    indexing: bool,
    progress: Progress,
    stop_watching: Arc<AtomicBool>,
    _lease: Option<EditorWatchLease>,
    channel: Arc<ClientChannel>,
    /// The client's `DataviewJS` preference for `vulcan/render`, if any.
    dataview_js: Option<vulcan_core::DataviewJsRenderPolicy>,
}

impl<'c> Server<'c> {
    fn start(
        connection: &'c Connection,
        session: EditorSession,
        watcher: WatcherMode,
        client: ClientFeatures,
    ) -> Self {
        let (indexed_sender, indexed) = unbounded();
        let mut threads = Vec::new();

        let (jobs, job_receiver) = unbounded::<Job>();
        for index in 0..READER_THREADS {
            let receiver = job_receiver.clone();
            threads.push(
                std::thread::Builder::new()
                    .name(format!("vulcan-lsp-reader-{index}"))
                    .spawn(move || {
                        for job in receiver {
                            job();
                        }
                    })
                    .expect("reader thread should spawn"),
            );
        }

        let channel = Arc::new(ClientChannel::new(connection.sender.clone()));
        let (writes, write_receiver) = unbounded::<WriteJob>();
        let writer_session = session.clone();
        let writer_events = indexed_sender.clone();
        let context = MutationContext {
            client: Arc::clone(&channel),
            support: client.edits,
            pending: PendingRenames::default(),
            moved: Mutex::new(Vec::new()),
        };
        threads.push(
            std::thread::Builder::new()
                .name("vulcan-lsp-writer".to_string())
                .spawn(move || {
                    run_writer(&writer_session, &context, &write_receiver, &writer_events);
                })
                .expect("writer thread should spawn"),
        );

        let stop_watching = Arc::new(AtomicBool::new(false));
        let mut lease = None;
        let freshness = match watcher {
            WatcherMode::Off => Freshness::Off,
            WatcherMode::Client => Freshness::Client,
            WatcherMode::Own => match EditorWatchLease::try_acquire(session.paths()) {
                Ok(Some(acquired)) => {
                    lease = Some(acquired);
                    threads.push(spawn_watcher(
                        session.paths().clone(),
                        Arc::clone(&stop_watching),
                        indexed_sender,
                    ));
                    Freshness::Lease
                }
                // Another editor process watches; follow the store clock.
                Ok(None) | Err(_) => Freshness::Polling,
            },
        };

        let mut server = Self {
            connection,
            last_clock: session.store_clock(),
            session,
            client,
            freshness,
            cursors: HashMap::new(),
            pending_diagnostics: HashMap::new(),
            jobs: Some(jobs),
            writes: Some(writes),
            indexed,
            threads,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            index_epoch: Arc::new(AtomicU64::new(0)),
            next_clock_poll: Instant::now() + CLOCK_POLL_INTERVAL,
            indexing: false,
            progress: Progress::None,
            stop_watching,
            _lease: lease,
            channel,
            dataview_js: None,
        };
        server.begin_startup();
        server
    }

    fn begin_startup(&mut self) {
        if !self.session.paths().vulcan_dir().is_dir() {
            self.show_message(
                lsp::MessageType::WARNING,
                format!(
                    "{} is not a Vulcan vault; run `vulcan init` there for link features",
                    self.session.paths().vault_root().display()
                ),
            );
            return;
        }
        self.indexing = true;
        if self.client.work_done_progress {
            let id = self.send_request(
                "window/workDoneProgress/create",
                json!({ "token": INDEX_PROGRESS_TOKEN }),
            );
            self.progress = Progress::Requested(id);
        }
        self.write(WriteJob::Refresh);
        if self.freshness == Freshness::Client && self.client.dynamic_file_watching {
            self.send_request(
                "client/registerCapability",
                json!({ "registrations": [{
                    "id": "vulcan/watched-files",
                    "method": "workspace/didChangeWatchedFiles",
                    "registerOptions": { "watchers": [{ "globPattern": "**/*" }] },
                }]}),
            );
        }
    }

    fn serve(&mut self) -> Result<(), LspError> {
        let receiver = self.connection.receiver.clone();
        let indexed = self.indexed.clone();
        loop {
            let wait = self.next_wakeup().map_or_else(never, |deadline| {
                after(deadline.saturating_duration_since(Instant::now()))
            });
            select! {
                recv(receiver) -> message => {
                    let Ok(message) = message else { return Ok(()) };
                    if self.handle_message(message)? {
                        return Ok(());
                    }
                }
                recv(indexed) -> event => {
                    if let Ok(event) = event {
                        self.handle_indexed(event);
                    }
                }
                recv(wait) -> _ => {}
            }
            self.flush_diagnostics();
            self.poll_clock();
        }
    }

    fn next_wakeup(&self) -> Option<Instant> {
        let diagnostics = self.pending_diagnostics.values().min().copied();
        let poll = (self.freshness == Freshness::Polling).then_some(self.next_clock_poll);
        match (diagnostics, poll) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left, right) => left.or(right),
        }
    }

    /// Handle one client message; `true` ends the loop.
    fn handle_message(&mut self, message: Message) -> Result<bool, LspError> {
        match message {
            Message::Request(request) => {
                if self.connection.handle_shutdown(&request)? {
                    return Ok(true);
                }
                self.dispatch(request);
            }
            Message::Notification(notification) => self.handle_notification(notification),
            Message::Response(response) => {
                if let Some(response) = self.channel.deliver(response) {
                    self.handle_response(&response);
                }
            }
        }
        Ok(false)
    }

    fn handle_response(&mut self, response: &Response) {
        let Progress::Requested(id) = &self.progress else {
            return;
        };
        if &response.id != id {
            return;
        }
        if response.response_result.is_ok() && self.indexing {
            self.progress = Progress::Begun;
            self.send_progress(&json!({
                "kind": "begin",
                "title": "Indexing vault",
                "cancellable": false,
            }));
        } else {
            self.progress = Progress::None;
        }
    }

    fn dispatch(&mut self, request: Request) {
        if matches!(
            request.method.as_str(),
            "textDocument/rename" | "workspace/willRenameFiles" | "workspace/executeCommand"
        ) {
            // Mutations run one at a time on the writer thread.
            self.write(WriteJob::Mutate {
                session: Box::new(self.session.clone()),
                request,
            });
            return;
        }
        if request.method == "workspace/diagnostic" {
            // Refreshing makes clients re-pull every problem in the vault,
            // so only clients that asked for them once get refreshes.
            self.client.refresh.workspace_diagnostics_pulled = true;
        }
        let id = request.id.clone();
        if request.method == "vulcan/status" {
            let status = json!({
                "protocolVersion": crate::PROTOCOL_VERSION,
                "vault": self.session.paths().vault_root(),
                "cache": if self.indexing {
                    "indexing"
                } else if self.session.has_cache() {
                    "fresh"
                } else {
                    "missing"
                },
                "daemon": "absent",
                "watcher": self.freshness.name(),
            });
            self.send(Message::Response(Response::new_ok(id, status)));
            return;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        lock(&self.in_flight).insert(id.clone(), Arc::clone(&cancelled));
        let session = self.session.clone();
        let dataview_js = self.dataview_js;
        let sender = self.connection.sender.clone();
        let in_flight = Arc::clone(&self.in_flight);
        let index_epoch = Arc::clone(&self.index_epoch);
        let job: Job = Box::new(move || {
            let epoch = index_epoch.load(Ordering::SeqCst);
            let response = if cancelled.load(Ordering::SeqCst) {
                cancelled_response(id.clone())
            } else {
                let response = handlers::handle(&session, request, dataview_js);
                if cancelled.load(Ordering::SeqCst) {
                    cancelled_response(id.clone())
                } else if index_epoch.load(Ordering::SeqCst) != epoch {
                    failure_response(
                        id.clone(),
                        ErrorCode::ContentModified as i32,
                        "the index changed while the request ran".to_string(),
                        "conflict",
                    )
                } else {
                    response
                }
            };
            lock(&in_flight).remove(&id);
            let _ = sender.send(Message::Response(response));
        });
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(job);
        }
    }

    fn handle_notification(&mut self, notification: Notification) {
        let params = notification.params;
        match notification.method.as_str() {
            "$/cancelRequest" => {
                if let Ok(params) = serde_json::from_value::<lsp::CancelParams>(params) {
                    let id = match params.id {
                        lsp::NumberOrString::Number(id) => RequestId::from(id),
                        lsp::NumberOrString::String(id) => RequestId::from(id),
                    };
                    if let Some(flag) = lock(&self.in_flight).get(&id) {
                        flag.store(true, Ordering::SeqCst);
                    }
                }
            }
            notification::DidOpenTextDocument::METHOD => {
                if let Ok(params) = serde_json::from_value(params) {
                    self.did_open(params);
                }
            }
            notification::DidChangeTextDocument::METHOD => {
                if let Ok(params) = serde_json::from_value(params) {
                    self.did_change(params);
                }
            }
            notification::DidSaveTextDocument::METHOD => {
                if let Ok(params) = serde_json::from_value::<lsp::DidSaveTextDocumentParams>(params)
                {
                    if let Some(path) = self.vault_path(&params.text_document.uri) {
                        self.cursors.remove(&path);
                        self.write(WriteJob::Scan {
                            paths: vec![path],
                            renames: HashMap::new(),
                        });
                    }
                }
            }
            notification::DidCloseTextDocument::METHOD => {
                if let Ok(params) = serde_json::from_value(params) {
                    self.did_close(params);
                }
            }
            notification::DidRenameFiles::METHOD => {
                if let Ok(params) = serde_json::from_value(params) {
                    self.did_rename(&params);
                }
            }
            notification::DidChangeWatchedFiles::METHOD => {
                if let Ok(params) = serde_json::from_value(params) {
                    self.did_change_watched_files(&params);
                }
            }
            _ => {}
        }
    }

    fn did_open(&mut self, params: lsp::DidOpenTextDocumentParams) {
        let Some(path) = self.vault_path(&params.text_document.uri) else {
            return;
        };
        self.session.open(
            &path,
            params.text_document.version,
            params.text_document.text,
        );
        // An opened document is diagnosed at once.
        self.pending_diagnostics.insert(path, Instant::now());
    }

    fn did_change(&mut self, params: lsp::DidChangeTextDocumentParams) {
        let Some(path) = self.vault_path(&params.text_document.uri) else {
            return;
        };
        let changes = params
            .content_changes
            .into_iter()
            .map(|change| EditorTextChange {
                range: change.range.map(convert::range),
                text: change.text,
            })
            .collect();
        match self
            .session
            .change(&path, params.text_document.version, changes)
        {
            Ok(Some(cursor)) => {
                self.cursors.insert(path.clone(), cursor);
            }
            Ok(None) => {
                self.cursors.remove(&path);
            }
            Err(_) => return,
        }
        let delay = Duration::from_millis(self.session.config().editor.diagnostic_delay_ms);
        self.pending_diagnostics
            .insert(path, Instant::now() + delay);
    }

    fn did_close(&mut self, params: lsp::DidCloseTextDocumentParams) {
        let Some(path) = self.vault_path(&params.text_document.uri) else {
            return;
        };
        self.session.close(&path);
        self.cursors.remove(&path);
        self.pending_diagnostics.remove(&path);
        // Clear what was published for the closed buffer.
        self.send_notification::<notification::PublishDiagnostics>(lsp::PublishDiagnosticsParams {
            uri: params.text_document.uri,
            diagnostics: Vec::new(),
            version: None,
        });
    }

    /// Files renamed by the client keep their identity through rename
    /// hints to the scan.
    fn did_rename(&self, params: &lsp::RenameFilesParams) {
        let renames = params
            .files
            .iter()
            .filter_map(|rename| {
                let old = self.vault_path(&rename.old_uri.parse().ok()?)?;
                let new = self.vault_path(&rename.new_uri.parse().ok()?)?;
                Some((new, old))
            })
            .collect::<HashMap<_, _>>();
        if renames.is_empty() {
            return;
        }
        // Index the rename first so documents keep their identity, then
        // publish the link rewrites planned when the client announced it.
        let pairs = renames
            .iter()
            .map(|(to, from)| (from.clone(), to.clone()))
            .collect::<Vec<_>>();
        self.write(WriteJob::Scan {
            paths: Vec::new(),
            renames,
        });
        for (from, to) in pairs {
            self.write(WriteJob::ApplyPending { from, to });
        }
    }

    fn did_change_watched_files(&self, params: &lsp::DidChangeWatchedFilesParams) {
        if self.freshness != Freshness::Client {
            return;
        }
        let paths = params
            .changes
            .iter()
            .filter_map(|change| self.vault_path(&change.uri))
            .filter(|path| path != ".vulcan" && !path.starts_with(".vulcan/"))
            .collect::<BTreeSet<_>>();
        if !paths.is_empty() {
            self.write(WriteJob::Scan {
                paths: paths.into_iter().collect(),
                renames: HashMap::new(),
            });
        }
    }

    fn handle_indexed(&mut self, event: Indexed) {
        if event.startup {
            self.indexing = false;
            match std::mem::replace(&mut self.progress, Progress::None) {
                Progress::Begun => self.send_progress(&json!({ "kind": "end" })),
                // A late token acknowledgement must not begin a report.
                Progress::Requested(_) | Progress::None => {}
            }
        }
        if let Some(error) = event.error {
            self.show_message(
                lsp::MessageType::WARNING,
                format!("Indexing the vault failed: {error}"),
            );
        }
        let clock = self.session.store_clock();
        // A scan that found nothing to change (such as the watcher's first
        // pass) must not invalidate requests in flight.
        let changed = !event.renames.is_empty()
            || match &event.summary {
                Some(summary) => summary.added + summary.updated + summary.deleted > 0,
                None => clock != self.last_clock,
            };
        if changed {
            self.index_changed(&event.paths, event.summary.as_ref(), clock, &event.renames);
        } else {
            self.last_clock = clock;
        }
    }

    /// Republish diagnostics after the index changed, and tell the client.
    fn index_changed(
        &mut self,
        paths: &[String],
        summary: Option<&ScanSummary>,
        clock: Option<i64>,
        renames: &[(String, String)],
    ) {
        self.last_clock = clock;
        self.index_epoch.fetch_add(1, Ordering::SeqCst);
        // Backlink-derived state of every open document may have changed.
        let now = Instant::now();
        let open = self
            .session
            .open_paths()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        for path in open {
            self.pending_diagnostics.entry(path).or_insert(now);
        }
        let mut params = json!({ "paths": paths, "summary": summary });
        if !renames.is_empty() {
            params["renames"] = renames
                .iter()
                .map(|(from, to)| json!({ "from": from, "to": to }))
                .collect();
        }
        self.send(Message::Notification(Notification::new(
            "vulcan/indexChanged".to_string(),
            params,
        )));
        // Backlink counts, due dates, and link resolution may have changed.
        let refresh = self.client.refresh;
        let editor = &self.session.config().editor;
        let requests = [
            (
                refresh.code_lens && editor.code_lens,
                "workspace/codeLens/refresh",
            ),
            (
                refresh.inlay_hint && editor.inlay_hints,
                "workspace/inlayHint/refresh",
            ),
            (
                refresh.semantic_tokens && editor.semantic_tokens,
                "workspace/semanticTokens/refresh",
            ),
            (
                refresh.diagnostics && refresh.workspace_diagnostics_pulled,
                "workspace/diagnostic/refresh",
            ),
        ];
        for (enabled, method) in requests {
            if enabled {
                self.send_request(method, Value::Null);
            }
        }
    }

    fn flush_diagnostics(&mut self) {
        let now = Instant::now();
        let due = self
            .pending_diagnostics
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        for path in due {
            self.pending_diagnostics.remove(&path);
            self.publish_diagnostics(&path);
        }
    }

    fn poll_clock(&mut self) {
        if self.freshness != Freshness::Polling || Instant::now() < self.next_clock_poll {
            return;
        }
        self.next_clock_poll = Instant::now() + CLOCK_POLL_INTERVAL;
        let clock = self.session.store_clock();
        if clock != self.last_clock {
            self.index_changed(&[], None, clock, &[]);
        }
    }

    fn publish_diagnostics(&self, path: &str) {
        let Some(uri) = handlers::uri(&self.session, path) else {
            return;
        };
        let version = self.session.document(path).map(|document| document.version);
        let cursor = self.cursors.get(path).copied();
        let diagnostics = match self.session.diagnostics(path, cursor) {
            Ok(Some(diagnostics)) => diagnostics.into_iter().map(convert::diagnostic).collect(),
            Ok(None) | Err(_) => return,
        };
        self.send_notification::<notification::PublishDiagnostics>(lsp::PublishDiagnosticsParams {
            uri,
            diagnostics,
            version,
        });
    }

    fn write(&self, job: WriteJob) {
        if let Some(writes) = &self.writes {
            let _ = writes.send(job);
        }
    }

    fn stop(&mut self) {
        self.stop_watching.store(true, Ordering::SeqCst);
        // Closing the queues ends the reader and writer threads once their
        // current job finishes. The watcher stops at its next stop check;
        // it holds no client state, so it is not awaited.
        self.jobs.take();
        self.writes.take();
        for thread in self.threads.drain(..) {
            if thread.thread().name() != Some(WATCHER_THREAD) {
                let _ = thread.join();
            }
        }
    }

    fn show_message(&self, typ: lsp::MessageType, message: String) {
        self.send_notification::<notification::ShowMessage>(lsp::ShowMessageParams {
            typ,
            message,
        });
    }

    fn send_progress(&self, value: &Value) {
        self.send(Message::Notification(Notification::new(
            "$/progress".to_string(),
            json!({ "token": INDEX_PROGRESS_TOKEN, "value": value }),
        )));
    }

    fn send_request(&self, method: &str, params: Value) -> RequestId {
        self.channel.send_request(method, params)
    }

    fn send_notification<N: notification::Notification>(&self, params: N::Params) {
        self.send(Message::Notification(Notification::new(
            N::METHOD.to_string(),
            params,
        )));
    }

    fn send(&self, message: Message) {
        // A closed channel means the client went away; the loop then ends.
        let _ = self.connection.sender.send(message);
    }

    fn vault_path(&self, uri: &lsp::Uri) -> Option<String> {
        handlers::vault_path(&self.session, uri)
    }
}

fn run_writer(
    session: &EditorSession,
    context: &MutationContext,
    jobs: &Receiver<WriteJob>,
    events: &Sender<Indexed>,
) {
    for job in jobs {
        let mut renames = Vec::new();
        let (startup, paths, result) = match job {
            WriteJob::Refresh => (true, Vec::new(), session.refresh_index()),
            WriteJob::Scan { paths, renames } => {
                let mut changed = paths.clone();
                changed.extend(renames.keys().cloned());
                (false, changed, session.scan_changed(&paths, &renames))
            }
            WriteJob::Mutate { session, request } => {
                mutations::handle(context, &session, request);
                renames = std::mem::take(&mut *lock(&context.moved));
                // Whatever the mutation changed, the main loop sees through
                // the store clock.
                (false, Vec::new(), Ok(None))
            }
            WriteJob::ApplyPending { from, to } => {
                match mutations::apply_pending(context, session, &from, &to) {
                    Some(error) => (false, Vec::new(), Err(AppError::operation(error))),
                    None => (false, vec![to], Ok(None)),
                }
            }
        };
        let (summary, error) = match result {
            Ok(summary) => (summary, None),
            Err(error) => (None, Some(error.to_string())),
        };
        let _ = events.send(Indexed {
            startup,
            paths,
            summary,
            error,
            renames,
        });
    }
}

fn spawn_watcher(
    paths: VaultPaths,
    stop: Arc<AtomicBool>,
    events: Sender<Indexed>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(WATCHER_THREAD.to_string())
        .spawn(move || {
            let result = vulcan_core::watch::watch_vault_until(
                &paths,
                &vulcan_core::watch::WatchOptions::default(),
                || stop.load(Ordering::SeqCst),
                |report| {
                    events
                        .send(Indexed {
                            startup: false,
                            paths: report.paths,
                            summary: Some(report.summary),
                            error: None,
                            renames: Vec::new(),
                        })
                        .map_err(|_| "editor session ended")
                },
            );
            if let Err(error) = result {
                let _ = events.send(Indexed {
                    startup: false,
                    paths: Vec::new(),
                    summary: None,
                    error: Some(format!("the file watcher stopped: {error}")),
                    renames: Vec::new(),
                });
            }
        })
        .expect("watcher thread should spawn")
}

fn cancelled_response(id: RequestId) -> Response {
    failure_response(
        id,
        ErrorCode::RequestCanceled as i32,
        "the request was cancelled".to_string(),
        "cancelled",
    )
}

pub(crate) fn failure_response(id: RequestId, code: i32, message: String, kind: &str) -> Response {
    Response {
        id,
        response_result: Err(lsp_server::ResponseError {
            code,
            data: Some(json!({ "kind": kind, "detail": message })),
            message,
        }),
    }
}

impl From<AppError> for handlers::ResponseFailure {
    fn from(error: AppError) -> Self {
        Self {
            code: ErrorCode::InternalError as i32,
            kind: error.code().unwrap_or("internal").to_string(),
            message: error.to_string(),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
