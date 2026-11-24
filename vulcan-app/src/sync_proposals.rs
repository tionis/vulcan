//! The resolution agent: a provider that proposes merged text for conflicted
//! files, with bounded, permission-filtered read tools over the vault. The
//! daemon conflict worker uses it to replay a paused branch
//! ([`crate::branch_replay`]).

use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[cfg(feature = "web")]
use std::io::Read;
use std::path::Path;
use vulcan_core::search::SearchMode;
use vulcan_core::{
    execute_query_report_with_filter, paths::secure_read, query_backlinks_with_filter,
    query_links_with_filter, search_vault_with_filter, PermissionGuard, ProfilePermissionGuard,
    QueryAst, SearchQuery, VaultPaths,
};
use vulcan_sync::SyncCancellationToken;

pub const RESOLUTION_AGENT_TOOL_CONTRACT_VERSION: u32 = 3;
const MAX_AGENT_CONTEXT_FILE_BYTES: usize = 1024 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_AGENT_TOOL_CALLS: usize = 8;
const MAX_AGENT_TOOL_ARGUMENT_BYTES: usize = 8 * 1024;
const MAX_AGENT_TOOL_RESULT_BYTES: usize = 256 * 1024;
#[cfg(feature = "web")]
const MAX_AGENT_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionAgentIdentity {
    pub provider: String,
    pub model: String,
    pub prompt_contract_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionAgentSide {
    pub revision: Option<String>,
    pub mode: Option<String>,
    pub content: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionAgentFile {
    pub path: String,
    pub base: ResolutionAgentSide,
    pub local: ResolutionAgentSide,
    pub remote: ResolutionAgentSide,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionAgentContextFile {
    pub path: String,
    pub content_hash: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionAgentRequest {
    pub conflict_id: String,
    pub policy_version: u32,
    pub policy_hash: String,
    pub selection: Option<ResolutionProposalSelection>,
    pub files: Vec<ResolutionAgentFile>,
    pub focused_context: Vec<ResolutionAgentContextFile>,
    pub broad_context_allowed: bool,
    pub tool_contract_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionAgentPathOutput {
    pub path: String,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionAgentOutput {
    pub explanation: String,
    pub referenced_context: Vec<String>,
    pub paths: Vec<ResolutionAgentPathOutput>,
}

pub trait ResolutionAgentTools {
    fn call(&mut self, name: &str, arguments: &str) -> Result<String, AppError>;
}

pub(crate) struct VaultResolutionAgentTools {
    paths: VaultPaths,
    guard: ProfilePermissionGuard,
    broad_context_allowed: bool,
    explicit_paths: BTreeSet<String>,
    calls: Vec<ResolutionProposalToolCall>,
    referenced_paths: BTreeSet<String>,
}

impl VaultResolutionAgentTools {
    pub(crate) fn new(
        paths: &VaultPaths,
        guard: ProfilePermissionGuard,
        broad_context_allowed: bool,
        explicit_paths: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            paths: paths.clone(),
            guard,
            broad_context_allowed,
            explicit_paths: explicit_paths.into_iter().collect(),
            calls: Vec::new(),
            referenced_paths: BTreeSet::new(),
        }
    }

    fn record_result(
        &mut self,
        name: &str,
        arguments: &str,
        value: &impl Serialize,
        referenced_paths: Vec<String>,
    ) -> Result<String, AppError> {
        if self.calls.len() >= MAX_AGENT_TOOL_CALLS {
            return Err(AppError::operation("agent exceeded the tool-call limit"));
        }
        let result = serde_json::to_string(value).map_err(AppError::operation)?;
        if result.len() > MAX_AGENT_TOOL_RESULT_BYTES {
            return Err(AppError::operation(format!(
                "agent tool `{name}` result exceeds its byte limit"
            )));
        }
        let mut referenced_paths = referenced_paths;
        referenced_paths.sort();
        referenced_paths.dedup();
        self.referenced_paths
            .extend(referenced_paths.iter().cloned());
        self.calls.push(ResolutionProposalToolCall {
            name: name.to_string(),
            arguments_hash: blake3::hash(arguments.as_bytes()).to_hex().to_string(),
            result_hash: blake3::hash(result.as_bytes()).to_hex().to_string(),
            referenced_paths,
        });
        Ok(result)
    }

    fn authorize_references(&self, paths: &[String]) -> Result<(), AppError> {
        for path in paths {
            self.guard
                .check_read_path(path)
                .map_err(AppError::operation)?;
        }
        Ok(())
    }

    fn read(&mut self, arguments: &str) -> Result<String, AppError> {
        #[derive(Deserialize)]
        struct Arguments {
            path: String,
        }
        let arguments_value: Arguments = parse_tool_arguments("vault_read", arguments)?;
        if !valid_relative_path(&arguments_value.path)
            || is_internal_context_path(&arguments_value.path)
        {
            return Err(AppError::operation("vault_read received an invalid path"));
        }
        if !self.broad_context_allowed && !self.explicit_paths.contains(&arguments_value.path) {
            return Err(AppError::operation(
                "vault_read outside explicit context requires broad context access",
            ));
        }
        self.guard
            .check_read_path(&arguments_value.path)
            .map_err(AppError::operation)?;
        let bytes = secure_read(self.paths.vault_root(), Path::new(&arguments_value.path))
            .map_err(AppError::operation)?;
        if bytes.len() > MAX_AGENT_CONTEXT_FILE_BYTES {
            return Err(AppError::operation(
                "vault_read result exceeds its byte limit",
            ));
        }
        let content = String::from_utf8(bytes)
            .map_err(|_| AppError::operation("vault_read requires a UTF-8 text file"))?;
        let value = serde_json::json!({
            "path": arguments_value.path,
            "content_hash": blake3::hash(content.as_bytes()).to_hex().to_string(),
            "content": content,
        });
        self.record_result("vault_read", arguments, &value, vec![arguments_value.path])
    }

    fn search(&mut self, arguments: &str) -> Result<String, AppError> {
        #[derive(Deserialize)]
        struct Arguments {
            query: String,
        }
        let arguments_value: Arguments = parse_tool_arguments("vault_search", arguments)?;
        validate_text("vault_search query", &arguments_value.query)?;
        let query = SearchQuery {
            text: arguments_value.query,
            mode: SearchMode::Keyword,
            limit: Some(10),
            context_size: 8,
            ..SearchQuery::default()
        };
        let filter = self.guard.read_filter();
        let report = search_vault_with_filter(&self.paths, &query, Some(&filter))
            .map_err(AppError::operation)?;
        let referenced = report
            .hits
            .iter()
            .map(|hit| hit.document_path.clone())
            .collect::<Vec<_>>();
        self.authorize_references(&referenced)?;
        self.record_result("vault_search", arguments, &report, referenced)
    }

    fn query(&mut self, arguments: &str) -> Result<String, AppError> {
        #[derive(Deserialize)]
        struct Arguments {
            dsl: String,
        }
        let arguments_value: Arguments = parse_tool_arguments("vault_query", arguments)?;
        let mut query = QueryAst::from_dsl(&arguments_value.dsl).map_err(AppError::operation)?;
        query.limit = Some(query.limit.unwrap_or(10).min(10));
        query.offset = query.offset.min(1_000);
        let filter = self.guard.read_filter();
        let report = execute_query_report_with_filter(&self.paths, query, Some(&filter))
            .map_err(AppError::operation)?;
        let referenced = report
            .notes
            .iter()
            .map(|note| note.document_path.clone())
            .collect::<Vec<_>>();
        self.authorize_references(&referenced)?;
        self.record_result("vault_query", arguments, &report, referenced)
    }

    fn links(&mut self, arguments: &str) -> Result<String, AppError> {
        #[derive(Deserialize)]
        struct Arguments {
            path: String,
            #[serde(default)]
            direction: LinkDirection,
        }
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum LinkDirection {
            #[default]
            Outgoing,
            Incoming,
        }
        let arguments_value: Arguments = parse_tool_arguments("vault_links", arguments)?;
        self.guard
            .check_read_path(&arguments_value.path)
            .map_err(AppError::operation)?;
        let filter = self.guard.read_filter();
        match arguments_value.direction {
            LinkDirection::Outgoing => {
                let report =
                    query_links_with_filter(&self.paths, &arguments_value.path, Some(&filter))
                        .map_err(AppError::operation)?;
                let mut referenced = vec![report.note_path.clone()];
                referenced.extend(
                    report
                        .links
                        .iter()
                        .filter_map(|link| link.resolved_target_path.clone()),
                );
                self.authorize_references(&referenced)?;
                self.record_result("vault_links", arguments, &report, referenced)
            }
            LinkDirection::Incoming => {
                let report =
                    query_backlinks_with_filter(&self.paths, &arguments_value.path, Some(&filter))
                        .map_err(AppError::operation)?;
                let mut referenced = vec![report.note_path.clone()];
                referenced.extend(report.backlinks.iter().map(|link| link.source_path.clone()));
                self.authorize_references(&referenced)?;
                self.record_result("vault_links", arguments, &report, referenced)
            }
        }
    }
}

impl ResolutionAgentTools for VaultResolutionAgentTools {
    fn call(&mut self, name: &str, arguments: &str) -> Result<String, AppError> {
        if arguments.len() > MAX_AGENT_TOOL_ARGUMENT_BYTES {
            return Err(AppError::operation(
                "agent tool arguments exceed their byte limit",
            ));
        }
        match name {
            "vault_read" => self.read(arguments),
            "vault_search" => self.search(arguments),
            "vault_query" => self.query(arguments),
            "vault_links" => self.links(arguments),
            _ => Err(AppError::operation(format!(
                "agent requested unknown tool `{name}`"
            ))),
        }
    }
}

fn parse_tool_arguments<T: for<'de> Deserialize<'de>>(
    name: &str,
    arguments: &str,
) -> Result<T, AppError> {
    serde_json::from_str(arguments)
        .map_err(|error| AppError::operation(format!("invalid `{name}` arguments: {error}")))
}

pub trait ResolutionAgentProvider: Send + Sync {
    fn identity(&self) -> ResolutionAgentIdentity;

    fn network_endpoint(&self) -> Option<&str> {
        None
    }

    fn propose(
        &self,
        request: &ResolutionAgentRequest,
        tools: &mut dyn ResolutionAgentTools,
        cancellation: &SyncCancellationToken,
    ) -> Result<ResolutionAgentOutput, AppError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuppliedResolutionProvider {
    output: ResolutionAgentOutput,
}

impl SuppliedResolutionProvider {
    #[must_use]
    pub fn new(paths: Vec<ResolutionAgentPathOutput>) -> Self {
        Self {
            output: ResolutionAgentOutput {
                explanation: "Resolution content supplied explicitly by the user.".to_string(),
                referenced_context: Vec::new(),
                paths,
            },
        }
    }
}

impl ResolutionAgentProvider for SuppliedResolutionProvider {
    fn identity(&self) -> ResolutionAgentIdentity {
        ResolutionAgentIdentity {
            provider: "vulcan-manual".to_string(),
            model: "supplied-files-v1".to_string(),
            prompt_contract_version: 1,
        }
    }

    fn propose(
        &self,
        _request: &ResolutionAgentRequest,
        _tools: &mut dyn ResolutionAgentTools,
        cancellation: &SyncCancellationToken,
    ) -> Result<ResolutionAgentOutput, AppError> {
        cancellation_check(cancellation)?;
        Ok(self.output.clone())
    }
}

#[cfg(feature = "web")]
pub struct OpenAiCompatibleResolutionProvider {
    client: reqwest::blocking::Client,
    endpoint: reqwest::Url,
    model: String,
    api_key: Option<String>,
}

#[cfg(feature = "web")]
impl OpenAiCompatibleResolutionProvider {
    pub fn new(
        base_url: &str,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Result<Self, AppError> {
        let mut endpoint = reqwest::Url::parse(base_url).map_err(AppError::operation)?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(AppError::operation(
                "agent base URL must be an absolute HTTP(S) URL without credentials, query, or fragment",
            ));
        }
        crate::credential_transport::validate_credential_transport(
            &endpoint,
            api_key.is_some(),
            "agent",
        )?;
        let path = endpoint.path().trim_end_matches('/');
        endpoint.set_path(&format!("{path}/chat/completions"));
        let model = model.into();
        validate_text("agent model", &model)?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .redirect(if api_key.is_some() {
                reqwest::redirect::Policy::none()
            } else {
                reqwest::redirect::Policy::limited(10)
            })
            .build()
            .map_err(AppError::operation)?;
        Ok(Self {
            client,
            endpoint,
            model,
            api_key,
        })
    }
}

#[cfg(feature = "web")]
impl ResolutionAgentProvider for OpenAiCompatibleResolutionProvider {
    fn identity(&self) -> ResolutionAgentIdentity {
        ResolutionAgentIdentity {
            provider: "openai-compatible".to_string(),
            model: self.model.clone(),
            prompt_contract_version: 3,
        }
    }

    fn network_endpoint(&self) -> Option<&str> {
        Some(self.endpoint.as_str())
    }

    fn propose(
        &self,
        request: &ResolutionAgentRequest,
        tools: &mut dyn ResolutionAgentTools,
        cancellation: &SyncCancellationToken,
    ) -> Result<ResolutionAgentOutput, AppError> {
        let mut body = openai_resolution_request(&self.model, request)?;
        for _ in 0..=MAX_AGENT_TOOL_CALLS {
            cancellation_check(cancellation)?;
            let bytes = self.send(&body)?;
            cancellation_check(cancellation)?;
            match parse_openai_resolution_turn(&bytes)? {
                OpenAiResolutionTurn::Final(output) => return Ok(output),
                OpenAiResolutionTurn::Tools {
                    assistant_message,
                    calls,
                } => {
                    let messages = body["messages"]
                        .as_array_mut()
                        .expect("resolution request messages are an array");
                    messages.push(assistant_message);
                    for call in calls {
                        cancellation_check(cancellation)?;
                        let result = tools.call(&call.name, &call.arguments)?;
                        messages.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": call.id,
                            "content": result,
                        }));
                    }
                }
            }
        }
        Err(AppError::operation("agent exceeded the tool-call limit"))
    }
}

#[cfg(feature = "web")]
impl OpenAiCompatibleResolutionProvider {
    fn send(&self, body: &serde_json::Value) -> Result<Vec<u8>, AppError> {
        let mut builder = self.client.post(self.endpoint.clone()).json(body);
        if let Some(api_key) = self.api_key.as_deref() {
            builder = builder.bearer_auth(api_key);
        }
        let mut response = builder.send().map_err(AppError::operation)?;
        let status = response.status();
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take((MAX_AGENT_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(AppError::operation)?;
        if bytes.len() > MAX_AGENT_RESPONSE_BYTES {
            return Err(AppError::operation("agent response exceeds its byte limit"));
        }
        if !status.is_success() {
            return Err(AppError::operation(format!(
                "agent provider returned HTTP {status}"
            )));
        }
        Ok(bytes)
    }
}

#[cfg(feature = "web")]
fn openai_resolution_request(
    model: &str,
    request: &ResolutionAgentRequest,
) -> Result<serde_json::Value, AppError> {
    let files = request
        .files
        .iter()
        .map(|file| {
            Ok(serde_json::json!({
                "path": file.path,
                "base": agent_side_json(&file.base)?,
                "local": agent_side_json(&file.local)?,
                "remote": agent_side_json(&file.remote)?,
            }))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let input = serde_json::json!({
        "conflict_id": request.conflict_id,
        "policy_version": request.policy_version,
        "policy_hash": request.policy_hash,
        "selection": request.selection,
        "focused_context": request.focused_context.iter().map(|context| serde_json::json!({
            "path": context.path,
            "content_hash": context.content_hash,
            "content": context.content,
        })).collect::<Vec<_>>(),
        "broad_context_allowed": request.broad_context_allowed,
        "files": files,
    });
    Ok(serde_json::json!({
        "model": model,
        "temperature": 0,
        "response_format": { "type": "json_object" },
        "tools": openai_resolution_tools(),
        "messages": [
            {
                "role": "system",
                "content": "Resolve only the supplied conflicted files. You may use the bounded read-only vault tools for context. Return one JSON object with explanation (string), referenced_context (a deduplicated array containing only vault paths supplied initially or returned by tools), and paths (array of objects with path and complete UTF-8 content strings). Include every supplied conflict path exactly once, invent no output paths, delete no files, and emit no Markdown fence or commentary outside the JSON object. Preserve valid file syntax and use context only to understand intent."
            },
            {
                "role": "user",
                "content": serde_json::to_string(&input).map_err(AppError::operation)?
            }
        ]
    }))
}

#[cfg(feature = "web")]
fn openai_resolution_tools() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "vault_read",
                "description": "Read one permitted UTF-8 vault file. Paths outside explicitly supplied context require broad context access.",
                "parameters": {
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "vault_search",
                "description": "Run a bounded permission-filtered keyword search over the vault index.",
                "parameters": {
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "vault_query",
                "description": "Run a bounded permission-filtered canonical Vulcan query DSL expression.",
                "parameters": {
                    "type": "object",
                    "properties": { "dsl": { "type": "string" } },
                    "required": ["dsl"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "vault_links",
                "description": "Inspect bounded permission-filtered outgoing or incoming links for one note.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "direction": { "type": "string", "enum": ["outgoing", "incoming"] }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        }
    ])
}

#[cfg(feature = "web")]
fn agent_side_json(side: &ResolutionAgentSide) -> Result<serde_json::Value, AppError> {
    let content = side
        .content
        .as_deref()
        .map(std::str::from_utf8)
        .transpose()
        .map_err(|_| AppError::operation("agent provider inputs must be valid UTF-8"))?;
    Ok(serde_json::json!({
        "revision": side.revision,
        "mode": side.mode,
        "content": content,
    }))
}

#[cfg(feature = "web")]
enum OpenAiResolutionTurn {
    Final(ResolutionAgentOutput),
    Tools {
        assistant_message: serde_json::Value,
        calls: Vec<OpenAiToolCall>,
    },
}

#[cfg(feature = "web")]
struct OpenAiToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[cfg(feature = "web")]
fn parse_openai_resolution_turn(bytes: &[u8]) -> Result<OpenAiResolutionTurn, AppError> {
    #[derive(Deserialize)]
    struct Response {
        choices: Vec<Choice>,
    }
    #[derive(Deserialize)]
    struct Choice {
        message: serde_json::Value,
    }
    let response: Response = serde_json::from_slice(bytes).map_err(AppError::operation)?;
    let message = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| AppError::operation("agent response contained no choices"))?
        .message;
    if let Some(calls) = message
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
    {
        if calls.is_empty() {
            return Err(AppError::operation(
                "agent response contained an empty tool-call list",
            ));
        }
        let calls = calls
            .iter()
            .map(parse_openai_tool_call)
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(OpenAiResolutionTurn::Tools {
            assistant_message: message,
            calls,
        });
    }
    let content = message
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| AppError::operation("agent response contained no final JSON content"))?;
    Ok(OpenAiResolutionTurn::Final(parse_resolution_output(
        content,
    )?))
}

#[cfg(feature = "web")]
fn parse_openai_tool_call(value: &serde_json::Value) -> Result<OpenAiToolCall, AppError> {
    let text = |value: Option<&serde_json::Value>, label: &str| {
        value
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| AppError::operation(format!("agent tool call omitted {label}")))
    };
    Ok(OpenAiToolCall {
        id: text(value.get("id"), "its ID")?,
        name: text(value.pointer("/function/name"), "its function name")?,
        arguments: text(value.pointer("/function/arguments"), "its arguments")?,
    })
}

#[cfg(feature = "web")]
fn parse_resolution_output(content: &str) -> Result<ResolutionAgentOutput, AppError> {
    #[derive(Deserialize)]
    struct Output {
        explanation: String,
        #[serde(default)]
        referenced_context: Vec<String>,
        paths: Vec<OutputPath>,
    }
    #[derive(Deserialize)]
    struct OutputPath {
        path: String,
        content: String,
    }
    let output: Output = serde_json::from_str(content).map_err(|error| {
        AppError::operation(format!(
            "agent response content was not exact JSON: {error}"
        ))
    })?;
    Ok(ResolutionAgentOutput {
        explanation: output.explanation,
        referenced_context: output.referenced_context,
        paths: output
            .paths
            .into_iter()
            .map(|path| ResolutionAgentPathOutput {
                path: path.path,
                content: path.content.into_bytes(),
            })
            .collect(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionProposalToolCall {
    pub name: String,
    pub arguments_hash: String,
    pub result_hash: String,
    pub referenced_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionProposalSelection {
    pub group_ids: Vec<String>,
    pub selection_digest: String,
    pub accepted_revision: String,
}

fn is_internal_context_path(path: &str) -> bool {
    path == ".obsidian"
        || path.starts_with(".obsidian/")
        || path == ".vulcan"
        || path.starts_with(".vulcan/")
}

fn valid_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.bytes().any(|byte| byte == 0)
        && Path::new(value)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn validate_text(label: &str, value: &str) -> Result<(), AppError> {
    if value.is_empty() || value.len() > MAX_TEXT_BYTES || value.bytes().any(|byte| byte == 0) {
        Err(AppError::operation(format!(
            "{label} is empty or unbounded"
        )))
    } else {
        Ok(())
    }
}

pub(crate) fn cancellation_check(cancellation: &SyncCancellationToken) -> Result<(), AppError> {
    if cancellation.is_cancelled() {
        Err(AppError::operation(
            "resolution proposal generation was cancelled",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::{
        paths::initialize_vulcan_dir, resolve_permission_profile, scan_vault, ScanMode,
    };

    #[cfg(feature = "web")]
    #[test]
    fn resolution_provider_rejects_credentials_over_remote_http() {
        let Err(error) = OpenAiCompatibleResolutionProvider::new(
            "http://api.example.com/v1",
            "fixture-model",
            Some("secret".to_string()),
        ) else {
            panic!("remote cleartext endpoint must fail");
        };
        assert!(error.to_string().contains("credentials require HTTPS"));
    }

    #[cfg(feature = "web")]
    struct NoopTools;

    #[cfg(feature = "web")]
    #[derive(Default)]
    struct RecordingTools {
        calls: Vec<(String, String)>,
    }

    #[cfg(feature = "web")]
    impl ResolutionAgentTools for NoopTools {
        fn call(&mut self, name: &str, _arguments: &str) -> Result<String, AppError> {
            Err(AppError::operation(format!(
                "unexpected test tool call `{name}`"
            )))
        }
    }

    #[cfg(feature = "web")]
    impl ResolutionAgentTools for RecordingTools {
        fn call(&mut self, name: &str, arguments: &str) -> Result<String, AppError> {
            self.calls.push((name.to_string(), arguments.to_string()));
            Ok(r#"{"hits":[{"document_path":"Context.md"}]}"#.to_string())
        }
    }

    #[test]
    fn focused_tools_are_bounded_permission_filtered_and_auditable() {
        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(
            paths.config_file(),
            "[permissions.profiles.resolver]\nread = { allow = [\"note:A.md\"] }\n",
        )
        .expect("permission profile");
        fs::write(temporary.path().join("A.md"), "alpha [[B]]\n").expect("allowed note");
        fs::write(temporary.path().join("B.md"), "secret beta\n").expect("denied note");
        scan_vault(&paths, ScanMode::Full).expect("scan fixture");
        let selection = resolve_permission_profile(&paths, Some("resolver")).expect("profile");
        let guard = ProfilePermissionGuard::new(&paths, selection);
        let mut tools = VaultResolutionAgentTools::new(&paths, guard, false, ["A.md".to_string()]);

        let allowed = tools
            .call("vault_search", r#"{"query":"alpha"}"#)
            .expect("allowed search");
        assert!(allowed.contains("A.md"));
        let denied = tools
            .call("vault_search", r#"{"query":"secret"}"#)
            .expect("filtered search");
        assert!(!denied.contains("B.md"));
        let query = tools
            .call("vault_query", r#"{"dsl":"FROM notes"}"#)
            .expect("bounded query");
        assert!(query.contains("A.md"));
        assert!(!query.contains("B.md"));
        let links = tools
            .call("vault_links", r#"{"path":"A.md","direction":"outgoing"}"#)
            .expect("filtered links");
        assert!(!links.contains("\"resolved_target_path\":\"B.md\""));
        let read_error = tools
            .call("vault_read", r#"{"path":"B.md"}"#)
            .expect_err("broad read must remain disabled");
        assert!(read_error.to_string().contains("requires broad context"));
        let read = tools
            .call("vault_read", r#"{"path":"A.md"}"#)
            .expect("explicit read");
        assert!(read.contains("alpha"));
        assert_eq!(tools.calls.len(), 5);
        assert!(tools
            .calls
            .iter()
            .all(|call| { call.arguments_hash.len() == 64 && call.result_hash.len() == 64 }));
        assert_eq!(tools.referenced_paths, BTreeSet::from(["A.md".to_string()]));
    }

    #[cfg(feature = "web")]
    #[test]
    fn openai_compatible_provider_sends_bounded_exact_inputs_and_parses_json_output() {
        use std::io::{BufRead, BufReader, Read as _, Write as _};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("request");
            let mut reader = BufReader::new(stream);
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("header");
                if line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
            assert!(headers.contains("authorization: Bearer secret"));
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_string)
                })
                .and_then(|value| value.trim().parse::<usize>().ok())
                .expect("content length");
            let mut body = vec![0; length];
            reader.read_exact(&mut body).expect("request body");
            let body: serde_json::Value = serde_json::from_slice(&body).expect("request JSON");
            assert_eq!(body["model"], "fixture-model");
            assert!(body["messages"][1]["content"]
                .as_str()
                .is_some_and(|content| content.contains("local text")));
            assert!(body["messages"][1]["content"]
                .as_str()
                .is_some_and(|content| content.contains("context text")));
            assert!(body["messages"][1]["content"]
                .as_str()
                .is_some_and(|content| content.contains("content_hash")));
            let response_content = serde_json::json!({
                "explanation": "combined",
                "referenced_context": [],
                "paths": [{"path": "Home.md", "content": "resolved\n"}]
            })
            .to_string();
            let response = serde_json::json!({
                "choices": [{"message": {"content": response_content}}]
            })
            .to_string();
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            )
            .expect("response");
        });
        let provider = OpenAiCompatibleResolutionProvider::new(
            &format!("http://{address}/v1"),
            "fixture-model",
            Some("secret".to_string()),
        )
        .expect("provider");
        let output = provider
            .propose(
                &provider_request_fixture(),
                &mut NoopTools,
                &SyncCancellationToken::default(),
            )
            .expect("provider output");
        assert_eq!(output.explanation, "combined");
        assert_eq!(output.paths[0].content, b"resolved\n");
        server.join().expect("server thread");
    }

    #[cfg(feature = "web")]
    #[test]
    fn openai_compatible_provider_executes_bounded_tool_turns() {
        use std::io::{BufRead, BufReader, Read as _, Write as _};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server = std::thread::spawn(move || {
            for turn in 0..2 {
                let (stream, _) = listener.accept().expect("request");
                let mut reader = BufReader::new(stream);
                let mut length = None;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).expect("header");
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length: ")
                    {
                        length = value.trim().parse::<usize>().ok();
                    }
                }
                let mut bytes = vec![0; length.expect("content length")];
                reader.read_exact(&mut bytes).expect("request body");
                let body: serde_json::Value = serde_json::from_slice(&bytes).expect("request JSON");
                assert!(body["tools"]
                    .as_array()
                    .is_some_and(|tools| tools.len() == 4));
                let response = if turn == 0 {
                    serde_json::json!({
                        "choices": [{"message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "call_1",
                                "type": "function",
                                "function": {
                                    "name": "vault_search",
                                    "arguments": "{\"query\":\"context\"}"
                                }
                            }]
                        }}]
                    })
                } else {
                    assert!(body["messages"].as_array().is_some_and(|messages| {
                        messages.iter().any(|message| {
                            message["role"] == "tool"
                                && message["content"]
                                    .as_str()
                                    .is_some_and(|content| content.contains("Context.md"))
                        })
                    }));
                    let content = serde_json::json!({
                        "explanation": "used search context",
                        "referenced_context": ["Context.md"],
                        "paths": [{"path": "Home.md", "content": "resolved\n"}]
                    })
                    .to_string();
                    serde_json::json!({
                        "choices": [{"message": {"role": "assistant", "content": content}}]
                    })
                }
                .to_string();
                write!(
                    reader.get_mut(),
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                )
                .expect("response");
            }
        });
        let provider = OpenAiCompatibleResolutionProvider::new(
            &format!("http://{address}/v1"),
            "fixture-model",
            None,
        )
        .expect("provider");
        let mut tools = RecordingTools::default();
        let output = provider
            .propose(
                &provider_request_fixture(),
                &mut tools,
                &SyncCancellationToken::default(),
            )
            .expect("tool-assisted output");
        assert_eq!(tools.calls.len(), 1);
        assert_eq!(tools.calls[0].0, "vault_search");
        assert_eq!(output.referenced_context, ["Context.md"]);
        server.join().expect("server thread");
    }

    #[cfg(feature = "web")]
    fn provider_request_fixture() -> ResolutionAgentRequest {
        let side = |content: &str| ResolutionAgentSide {
            revision: Some("a".repeat(40)),
            mode: Some("100644".to_string()),
            content: Some(content.as_bytes().to_vec()),
        };
        ResolutionAgentRequest {
            conflict_id: "b".repeat(32),
            policy_version: 1,
            policy_hash: "c".repeat(64),
            selection: None,
            files: vec![ResolutionAgentFile {
                path: "Home.md".to_string(),
                base: side("base text"),
                local: side("local text"),
                remote: side("remote text"),
            }],
            focused_context: vec![ResolutionAgentContextFile {
                path: "Context.md".to_string(),
                content_hash: blake3::hash(b"context text").to_hex().to_string(),
                content: "context text".to_string(),
            }],
            broad_context_allowed: false,
            tool_contract_version: RESOLUTION_AGENT_TOOL_CONTRACT_VERSION,
        }
    }
}
