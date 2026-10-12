//! The message loop and request routing.

use crate::convert;
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::Notification as _;
use lsp_types::request::Request as _;
use lsp_types::{self as lsp, notification, request};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use vulcan_app::editor::{EditorSession, EditorTextChange};
use vulcan_app::AppError;
use vulcan_core::editor::TextPosition;
use vulcan_core::VaultPaths;

/// Most workspace symbols returned for one query.
const WORKSPACE_SYMBOL_LIMIT: usize = 500;

/// Process-level options, from the command line.
#[derive(Debug, Clone, Default)]
pub struct LspOptions {
    /// An explicitly named vault, overriding discovery from the client's
    /// workspace.
    pub vault: Option<PathBuf>,
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

/// `initializationOptions` the server understands.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializationOptions {
    vault: Option<PathBuf>,
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
    let (id, params) = connection.initialize_start()?;
    let params: lsp::InitializeParams = serde_json::from_value(params)?;
    let (encoding, encoding_kind) = convert::negotiate_encoding(&params.capabilities);
    let vault_root = vault_root(options, &params)?;
    let session = EditorSession::new(VaultPaths::new(&vault_root), encoding);

    let result = lsp::InitializeResult {
        capabilities: capabilities(encoding_kind, &session),
        server_info: Some(lsp::ServerInfo {
            name: "vulcan".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    };
    connection.initialize_finish(id, serde_json::to_value(result)?)?;

    let mut server = Server {
        connection,
        session,
        cursors: HashMap::new(),
    };
    server.start();
    server.serve()
}

fn vault_root(options: &LspOptions, params: &lsp::InitializeParams) -> Result<PathBuf, LspError> {
    let initialization = params
        .initialization_options
        .clone()
        .map(serde_json::from_value::<InitializationOptions>)
        .transpose()?
        .unwrap_or_default();
    let named = options.vault.clone().or(initialization.vault);
    if let Some(named) = named {
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
        experimental: Some(json!({
            "vulcan": {
                "protocolVersion": crate::PROTOCOL_VERSION,
                "vault": {
                    "root": session.paths().vault_root(),
                    "cacheFresh": session.has_cache(),
                },
                "commands": Vec::<String>::new(),
            }
        })),
        ..lsp::ServerCapabilities::default()
    }
}

struct Server<'c> {
    connection: &'c Connection,
    session: EditorSession,
    /// The last edit position per open document, treated as the cursor.
    cursors: HashMap<String, TextPosition>,
}

impl Server<'_> {
    fn start(&mut self) {
        match self.session.refresh_index() {
            Ok(Some(_)) => {}
            Ok(None) => self.show_message(
                lsp::MessageType::WARNING,
                format!(
                    "{} is not a Vulcan vault; run `vulcan init` there for link features",
                    self.session.paths().vault_root().display()
                ),
            ),
            Err(error) => self.show_message(
                lsp::MessageType::ERROR,
                format!("Indexing the vault failed: {error}"),
            ),
        }
    }

    fn serve(&mut self) -> Result<(), LspError> {
        while let Ok(message) = self.connection.receiver.recv() {
            match message {
                Message::Request(request) => {
                    if self.connection.handle_shutdown(&request)? {
                        return Ok(());
                    }
                    let response = self.handle_request(request);
                    self.send(Message::Response(response));
                }
                Message::Notification(notification) => self.handle_notification(notification),
                Message::Response(_) => {}
            }
        }
        Ok(())
    }

    fn handle_request(&mut self, request: Request) -> Response {
        let id = request.id.clone();
        let result = match request.method.as_str() {
            request::GotoDefinition::METHOD => {
                params(request).and_then(|params| self.definition(&params))
            }
            request::References::METHOD => {
                params(request).and_then(|params| self.references(&params))
            }
            request::HoverRequest::METHOD => params(request).and_then(|params| self.hover(&params)),
            request::Completion::METHOD => {
                params(request).and_then(|params| self.completion(&params))
            }
            request::DocumentSymbolRequest::METHOD => {
                params(request).map(|params| self.document_symbols(&params))
            }
            request::WorkspaceSymbolRequest::METHOD => {
                params(request).and_then(|params| self.workspace_symbols(&params))
            }
            "vulcan/status" => Ok(json!({
                "protocolVersion": crate::PROTOCOL_VERSION,
                "vault": self.session.paths().vault_root(),
                "cache": if self.session.has_cache() { "fresh" } else { "missing" },
                "daemon": "absent",
                "watcher": "off",
            })),
            method => Err(ResponseFailure::method_not_found(method)),
        };
        match result {
            Ok(value) => Response::new_ok(id, value),
            Err(failure) => failure.into_response(id),
        }
    }

    fn handle_notification(&mut self, notification: Notification) {
        match notification.method.as_str() {
            notification::DidOpenTextDocument::METHOD => {
                if let Ok(params) =
                    serde_json::from_value::<lsp::DidOpenTextDocumentParams>(notification.params)
                {
                    let Some(path) = self.vault_path(&params.text_document.uri) else {
                        return;
                    };
                    self.session.open(
                        &path,
                        params.text_document.version,
                        params.text_document.text,
                    );
                    self.publish_diagnostics(&path);
                }
            }
            notification::DidChangeTextDocument::METHOD => {
                if let Ok(params) =
                    serde_json::from_value::<lsp::DidChangeTextDocumentParams>(notification.params)
                {
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
                    self.publish_diagnostics(&path);
                }
            }
            notification::DidSaveTextDocument::METHOD => {
                if let Ok(params) =
                    serde_json::from_value::<lsp::DidSaveTextDocumentParams>(notification.params)
                {
                    let Some(path) = self.vault_path(&params.text_document.uri) else {
                        return;
                    };
                    self.cursors.remove(&path);
                    if let Err(error) = self.session.scan_saved(&[path]) {
                        self.show_message(
                            lsp::MessageType::WARNING,
                            format!("Indexing the saved file failed: {error}"),
                        );
                    }
                    // Backlink-derived state of every open document may change.
                    let open = self
                        .session
                        .open_paths()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>();
                    for path in open {
                        self.publish_diagnostics(&path);
                    }
                }
            }
            notification::DidCloseTextDocument::METHOD => {
                if let Ok(params) =
                    serde_json::from_value::<lsp::DidCloseTextDocumentParams>(notification.params)
                {
                    let Some(path) = self.vault_path(&params.text_document.uri) else {
                        return;
                    };
                    self.session.close(&path);
                    self.cursors.remove(&path);
                    // Clear what was published for the closed buffer.
                    self.send_notification::<notification::PublishDiagnostics>(
                        lsp::PublishDiagnosticsParams {
                            uri: params.text_document.uri,
                            diagnostics: Vec::new(),
                            version: None,
                        },
                    );
                }
            }
            _ => {}
        }
    }

    fn definition(&self, params: &lsp::GotoDefinitionParams) -> Result<Value, ResponseFailure> {
        let position = &params.text_document_position_params;
        let Some(path) = self.vault_path(&position.text_document.uri) else {
            return Ok(Value::Null);
        };
        let location = self
            .session
            .definition(&path, convert::position(position.position))?;
        Ok(location
            .and_then(|location| self.location(&location))
            .map_or(Value::Null, |location| {
                serde_json::to_value(lsp::GotoDefinitionResponse::Scalar(location))
                    .unwrap_or(Value::Null)
            }))
    }

    fn references(&self, params: &lsp::ReferenceParams) -> Result<Value, ResponseFailure> {
        let position = &params.text_document_position;
        let Some(path) = self.vault_path(&position.text_document.uri) else {
            return Ok(Value::Null);
        };
        let locations = self
            .session
            .references(&path, convert::position(position.position))?
            .iter()
            .filter_map(|location| self.location(location))
            .collect::<Vec<_>>();
        Ok(serde_json::to_value(locations).unwrap_or(Value::Null))
    }

    fn hover(&self, params: &lsp::HoverParams) -> Result<Value, ResponseFailure> {
        let position = &params.text_document_position_params;
        let Some(path) = self.vault_path(&position.text_document.uri) else {
            return Ok(Value::Null);
        };
        let hover = self
            .session
            .hover(&path, convert::position(position.position))?;
        Ok(hover.map_or(Value::Null, |hover| {
            serde_json::to_value(lsp::Hover {
                contents: lsp::HoverContents::Markup(lsp::MarkupContent {
                    kind: lsp::MarkupKind::Markdown,
                    value: hover.markdown,
                }),
                range: Some(convert::lsp_range(hover.range)),
            })
            .unwrap_or(Value::Null)
        }))
    }

    fn completion(&self, params: &lsp::CompletionParams) -> Result<Value, ResponseFailure> {
        let position = &params.text_document_position;
        let Some(path) = self.vault_path(&position.text_document.uri) else {
            return Ok(Value::Null);
        };
        let Some(completion) = self
            .session
            .completion(&path, convert::position(position.position))?
        else {
            return Ok(Value::Null);
        };
        let range = convert::lsp_range(completion.range);
        let items = completion
            .items
            .into_iter()
            .map(|item| lsp::CompletionItem {
                label: item.label,
                kind: Some(convert::completion_kind(item.kind)),
                detail: item.detail,
                filter_text: item.filter_text,
                text_edit: Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit {
                    range,
                    new_text: item.insert_text,
                })),
                ..lsp::CompletionItem::default()
            })
            .collect();
        Ok(
            serde_json::to_value(lsp::CompletionResponse::List(lsp::CompletionList {
                is_incomplete: completion.incomplete,
                items,
            }))
            .unwrap_or(Value::Null),
        )
    }

    fn document_symbols(&self, params: &lsp::DocumentSymbolParams) -> Value {
        let Some(path) = self.vault_path(&params.text_document.uri) else {
            return Value::Null;
        };
        let symbols = self.session.document_symbols(&path).map(|symbols| {
            lsp::DocumentSymbolResponse::Nested(
                symbols.into_iter().map(convert::document_symbol).collect(),
            )
        });
        serde_json::to_value(symbols).unwrap_or(Value::Null)
    }

    fn workspace_symbols(
        &self,
        params: &lsp::WorkspaceSymbolParams,
    ) -> Result<Value, ResponseFailure> {
        let symbols = self
            .session
            .workspace_symbols(&params.query, WORKSPACE_SYMBOL_LIMIT)?
            .into_iter()
            .filter_map(|symbol| {
                let uri = self.uri(&symbol.path)?;
                Some(lsp::WorkspaceSymbol {
                    name: symbol.name,
                    kind: lsp::SymbolKind::FILE,
                    tags: None,
                    container_name: symbol.alias_of.map(|name| format!("alias of {name}")),
                    location: lsp::OneOf::Left(lsp::Location {
                        uri,
                        range: lsp::Range::default(),
                    }),
                    data: None,
                })
            })
            .collect();
        Ok(
            serde_json::to_value(lsp::WorkspaceSymbolResponse::Nested(symbols))
                .unwrap_or(Value::Null),
        )
    }

    fn publish_diagnostics(&self, path: &str) {
        let Some(uri) = self.uri(path) else {
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

    fn show_message(&self, typ: lsp::MessageType, message: String) {
        self.send_notification::<notification::ShowMessage>(lsp::ShowMessageParams {
            typ,
            message,
        });
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
        let path = convert::file_path(uri)?;
        self.session.relative_path(&path)
    }

    fn uri(&self, path: &str) -> Option<lsp::Uri> {
        convert::file_uri(&self.session.paths().vault_root().join(Path::new(path)))
    }

    fn location(&self, location: &vulcan_app::editor::EditorLocation) -> Option<lsp::Location> {
        Some(lsp::Location {
            uri: self.uri(&location.path)?,
            range: convert::lsp_range(location.range),
        })
    }
}

/// A request that cannot be answered, as an LSP error response.
struct ResponseFailure {
    code: i32,
    message: String,
    kind: String,
}

impl ResponseFailure {
    fn method_not_found(method: &str) -> Self {
        Self {
            code: ErrorCode::MethodNotFound as i32,
            message: format!("unsupported method {method}"),
            kind: "invalid_request".to_string(),
        }
    }

    fn into_response(self, id: RequestId) -> Response {
        Response {
            id,
            response_result: Err(lsp_server::ResponseError {
                code: self.code,
                data: Some(json!({ "kind": self.kind, "detail": self.message })),
                message: self.message,
            }),
        }
    }
}

impl From<AppError> for ResponseFailure {
    fn from(error: AppError) -> Self {
        Self {
            code: ErrorCode::InternalError as i32,
            kind: error.code().unwrap_or("internal").to_string(),
            message: error.to_string(),
        }
    }
}

fn params<P: DeserializeOwned>(request: Request) -> Result<P, ResponseFailure> {
    serde_json::from_value(request.params).map_err(|error| ResponseFailure {
        code: ErrorCode::InvalidParams as i32,
        message: error.to_string(),
        kind: "invalid_request".to_string(),
    })
}
