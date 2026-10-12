//! Request handlers. Each answers from a session snapshot, so they run on
//! reader threads while the main loop keeps applying edits.

use crate::convert;
use lsp_server::{ErrorCode, Request, RequestId, Response};
use lsp_types::request::Request as _;
use lsp_types::{self as lsp, request};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::Path;
use vulcan_app::editor::{
    EditorAction, EditorActionKind, EditorCodeAction, EditorLocation, EditorSession,
};

/// Most workspace symbols returned for one query.
const WORKSPACE_SYMBOL_LIMIT: usize = 500;

/// A request that cannot be answered, as an LSP error response.
pub(crate) struct ResponseFailure {
    pub(crate) code: i32,
    pub(crate) message: String,
    pub(crate) kind: String,
}

impl ResponseFailure {
    fn method_not_found(method: &str) -> Self {
        Self {
            code: ErrorCode::MethodNotFound as i32,
            message: format!("unsupported method {method}"),
            kind: "invalid_request".to_string(),
        }
    }
}

type Answer = Result<Value, ResponseFailure>;

/// Answer one request from `session`.
pub(crate) fn handle(
    session: &EditorSession,
    request: Request,
    dataview_js: Option<vulcan_core::DataviewJsRenderPolicy>,
) -> Response {
    let id = request.id.clone();
    let result = match request.method.as_str() {
        request::GotoDefinition::METHOD => params(request).and_then(|p| definition(session, &p)),
        request::References::METHOD => params(request).and_then(|p| references(session, &p)),
        request::HoverRequest::METHOD => params(request).and_then(|p| hover(session, &p)),
        request::Completion::METHOD => params(request).and_then(|p| completion(session, &p)),
        request::DocumentSymbolRequest::METHOD => {
            params(request).map(|p| document_symbols(session, &p))
        }
        request::WorkspaceSymbolRequest::METHOD => {
            params(request).and_then(|p| workspace_symbols(session, &p))
        }
        request::PrepareRenameRequest::METHOD => {
            params(request).and_then(|p| prepare_rename(session, &p))
        }
        request::CodeActionRequest::METHOD => {
            params(request).and_then(|p| code_actions(session, &p))
        }
        "vulcan/render" => params(request).and_then(|p| render(session, &p, dataview_js)),
        method => Err(ResponseFailure::method_not_found(method)),
    };
    respond(id, result)
}

fn respond(id: RequestId, result: Answer) -> Response {
    match result {
        Ok(value) => Response::new_ok(id, value),
        Err(failure) => {
            crate::server::failure_response(id, failure.code, failure.message, &failure.kind)
        }
    }
}

fn definition(session: &EditorSession, params: &lsp::GotoDefinitionParams) -> Answer {
    let position = &params.text_document_position_params;
    let Some(path) = vault_path(session, &position.text_document.uri) else {
        return Ok(Value::Null);
    };
    let location = session.definition(&path, convert::position(position.position))?;
    Ok(location
        .and_then(|location| lsp_location(session, &location))
        .map_or(Value::Null, |location| {
            to_value(lsp::GotoDefinitionResponse::Scalar(location))
        }))
}

fn references(session: &EditorSession, params: &lsp::ReferenceParams) -> Answer {
    let position = &params.text_document_position;
    let Some(path) = vault_path(session, &position.text_document.uri) else {
        return Ok(Value::Null);
    };
    let locations = session
        .references(&path, convert::position(position.position))?
        .iter()
        .filter_map(|location| lsp_location(session, location))
        .collect::<Vec<_>>();
    Ok(to_value(locations))
}

fn hover(session: &EditorSession, params: &lsp::HoverParams) -> Answer {
    let position = &params.text_document_position_params;
    let Some(path) = vault_path(session, &position.text_document.uri) else {
        return Ok(Value::Null);
    };
    let hover = session.hover(&path, convert::position(position.position))?;
    Ok(hover.map_or(Value::Null, |hover| {
        to_value(lsp::Hover {
            contents: lsp::HoverContents::Markup(lsp::MarkupContent {
                kind: lsp::MarkupKind::Markdown,
                value: hover.markdown,
            }),
            range: Some(convert::lsp_range(hover.range)),
        })
    }))
}

fn completion(session: &EditorSession, params: &lsp::CompletionParams) -> Answer {
    let position = &params.text_document_position;
    let Some(path) = vault_path(session, &position.text_document.uri) else {
        return Ok(Value::Null);
    };
    let Some(completion) = session.completion(&path, convert::position(position.position))? else {
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
    Ok(to_value(lsp::CompletionResponse::List(
        lsp::CompletionList {
            is_incomplete: completion.incomplete,
            items,
        },
    )))
}

fn prepare_rename(session: &EditorSession, params: &lsp::TextDocumentPositionParams) -> Answer {
    let Some(path) = vault_path(session, &params.text_document.uri) else {
        return Ok(Value::Null);
    };
    let target = session.prepare_rename(&path, convert::position(params.position))?;
    Ok(target.map_or(Value::Null, |target| {
        to_value(lsp::PrepareRenameResponse::RangeWithPlaceholder {
            range: convert::lsp_range(target.range),
            placeholder: target.placeholder,
        })
    }))
}

/// Parameters of `vulcan/render`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenderParams {
    text_document: lsp::TextDocumentIdentifier,
    range: Option<lsp::Range>,
    dataview_js: Option<vulcan_core::DataviewJsRenderPolicy>,
}

fn render(
    session: &EditorSession,
    params: &RenderParams,
    default_dataview_js: Option<vulcan_core::DataviewJsRenderPolicy>,
) -> Answer {
    let Some(path) = vault_path(session, &params.text_document.uri) else {
        return Ok(Value::Null);
    };
    let Some(rendered) = session.render(
        &path,
        params.range.map(convert::range),
        params.dataview_js.or(default_dataview_js),
    )?
    else {
        return Ok(Value::Null);
    };
    Ok(serde_json::json!({
        "regions": rendered
            .regions
            .into_iter()
            .map(|region| serde_json::json!({
                "range": convert::lsp_range(region.range),
                "kind": region.kind,
                "source": region.source,
                "markdown": region.markdown,
            }))
            .collect::<Vec<_>>(),
        "diagnostics": rendered.diagnostics,
        "dataviewJs": rendered.dataview_js,
    }))
}

fn code_actions(session: &EditorSession, params: &lsp::CodeActionParams) -> Answer {
    let Some(path) = vault_path(session, &params.text_document.uri) else {
        return Ok(Value::Null);
    };
    let actions = session
        .code_actions(&path, convert::position(params.range.start))?
        .into_iter()
        .filter_map(|action| code_action(session, params, action))
        .collect::<Vec<_>>();
    Ok(to_value(actions))
}

fn code_action(
    session: &EditorSession,
    params: &lsp::CodeActionParams,
    action: EditorCodeAction,
) -> Option<lsp::CodeActionOrCommand> {
    let kind = match action.kind {
        EditorActionKind::QuickFix => lsp::CodeActionKind::QUICKFIX,
        EditorActionKind::Refactor => lsp::CodeActionKind::REFACTOR_REWRITE,
    };
    if let Some(only) = &params.context.only {
        if !only
            .iter()
            .any(|wanted| kind.as_str().starts_with(wanted.as_str()))
        {
            return None;
        }
    }
    // The client's diagnostics this action fixes.
    let diagnostics = action.fixes.map(|code| {
        params
            .context
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.code == Some(lsp::NumberOrString::String(code.to_string()))
            })
            .cloned()
            .collect::<Vec<_>>()
    });
    let (edit, command) = match action.action {
        EditorAction::Edit(document) => {
            let edit = lsp::TextDocumentEdit {
                text_document: lsp::OptionalVersionedTextDocumentIdentifier {
                    uri: uri(session, &document.path)?,
                    version: Some(document.version),
                },
                edits: document
                    .edits
                    .into_iter()
                    .map(|edit| {
                        lsp::OneOf::Left(lsp::TextEdit {
                            range: convert::lsp_range(edit.range),
                            new_text: edit.new_text,
                        })
                    })
                    .collect(),
            };
            (
                Some(lsp::WorkspaceEdit {
                    document_changes: Some(lsp::DocumentChanges::Edits(vec![edit])),
                    ..lsp::WorkspaceEdit::default()
                }),
                None,
            )
        }
        EditorAction::CreateNote { path } => (
            None,
            Some(lsp::Command {
                title: action.title.clone(),
                command: "vulcan.note.create".to_string(),
                arguments: Some(vec![serde_json::json!({ "path": path })]),
            }),
        ),
        EditorAction::ToggleTask { path, line } => (
            None,
            Some(lsp::Command {
                title: action.title.clone(),
                command: "vulcan.task.toggle".to_string(),
                arguments: Some(vec![serde_json::json!({
                    "path": uri(session, &path)?.to_string(),
                    "line": line,
                })]),
            }),
        ),
    };
    Some(lsp::CodeActionOrCommand::CodeAction(lsp::CodeAction {
        title: action.title,
        kind: Some(kind),
        diagnostics,
        edit,
        command,
        is_preferred: action.preferred.then_some(true),
        ..lsp::CodeAction::default()
    }))
}

fn document_symbols(session: &EditorSession, params: &lsp::DocumentSymbolParams) -> Value {
    let Some(path) = vault_path(session, &params.text_document.uri) else {
        return Value::Null;
    };
    let symbols = session.document_symbols(&path).map(|symbols| {
        lsp::DocumentSymbolResponse::Nested(
            symbols.into_iter().map(convert::document_symbol).collect(),
        )
    });
    to_value(symbols)
}

fn workspace_symbols(session: &EditorSession, params: &lsp::WorkspaceSymbolParams) -> Answer {
    let symbols = session
        .workspace_symbols(&params.query, WORKSPACE_SYMBOL_LIMIT)?
        .into_iter()
        .filter_map(|symbol| {
            Some(lsp::WorkspaceSymbol {
                location: lsp::OneOf::Left(lsp::Location {
                    uri: uri(session, &symbol.path)?,
                    range: lsp::Range::default(),
                }),
                name: symbol.name,
                kind: lsp::SymbolKind::FILE,
                tags: None,
                container_name: symbol.alias_of.map(|name| format!("alias of {name}")),
                data: None,
            })
        })
        .collect();
    Ok(to_value(lsp::WorkspaceSymbolResponse::Nested(symbols)))
}

/// The vault-relative path of a document URI, if it is inside the vault.
pub(crate) fn vault_path(session: &EditorSession, uri: &lsp::Uri) -> Option<String> {
    session.relative_path(&convert::file_path(uri)?)
}

/// The URI of a vault-relative path.
pub(crate) fn uri(session: &EditorSession, path: &str) -> Option<lsp::Uri> {
    convert::file_uri(&session.paths().vault_root().join(Path::new(path)))
}

fn lsp_location(session: &EditorSession, location: &EditorLocation) -> Option<lsp::Location> {
    Some(lsp::Location {
        uri: uri(session, &location.path)?,
        range: convert::lsp_range(location.range),
    })
}

fn to_value(value: impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn params<P: DeserializeOwned>(request: Request) -> Result<P, ResponseFailure> {
    serde_json::from_value(request.params).map_err(|error| ResponseFailure {
        code: ErrorCode::InvalidParams as i32,
        message: error.to_string(),
        kind: "invalid_request".to_string(),
    })
}
