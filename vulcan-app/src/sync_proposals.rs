//! Isolated, review-first agent resolution proposals for preserved Git conflicts.

use crate::scan::refresh_cache_incrementally;
use crate::sync::{load_validated_sync_config, validate_git_merge_tree};
use crate::sync_conflicts::{
    conflict_live_input, conflict_worktree_revision, conflict_worktree_tree,
    verify_preserved_conflict_refs, SyncConflictRecord, SyncConflictResolutionRecord,
    SyncConflictStore, SYNC_CONFLICT_RESOLUTION_VERSION,
};
use crate::sync_state::{repository_state_key, SyncStateStore};
use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
#[cfg(feature = "web")]
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use vulcan_core::search::SearchMode;
use vulcan_core::{
    execute_query_report_with_filter, paths::secure_read, query_backlinks_with_filter,
    query_links_with_filter, resolve_permission_profile, search_vault_with_filter, PermissionGuard,
    ProfilePermissionGuard, QueryAst, ScanSummary, SearchQuery, VaultPaths,
};
use vulcan_sync::{
    conflict_proposal_resolution_ref, conflict_recovery_ref, GitAutomaticMergeValidation,
    GitCaptureRequest, GitContentMergeResolutionRequest, GitEngine, GitOid, GitPushResult,
    GitRefName, GitRemote, GitResolvedPath, GitSyncOptions, GitSyncRefs, SyncCancellationToken,
};

pub const RESOLUTION_PROPOSAL_VERSION: u32 = 3;
pub const RESOLUTION_AGENT_TOOL_CONTRACT_VERSION: u32 = 3;
pub const RESOLUTION_PROPOSAL_AUDIT_VERSION: u32 = 1;
const MAX_AGENT_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_AGENT_CONTEXT_FILE_BYTES: usize = 1024 * 1024;
const MAX_AGENT_TOTAL_BYTES: usize = 32 * 1024 * 1024;
const MAX_PROPOSAL_RECORD_BYTES: usize = 32 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_CONTEXT_PATHS: usize = 64;
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

struct VaultResolutionAgentTools {
    paths: VaultPaths,
    guard: ProfilePermissionGuard,
    broad_context_allowed: bool,
    explicit_paths: BTreeSet<String>,
    calls: Vec<ResolutionProposalToolCall>,
    referenced_paths: BTreeSet<String>,
}

impl VaultResolutionAgentTools {
    fn new(
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
        let path = endpoint.path().trim_end_matches('/');
        endpoint.set_path(&format!("{path}/chat/completions"));
        let model = model.into();
        validate_text("agent model", &model)?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionProposalOptions {
    pub permission_profile: String,
    pub focused_context: Vec<String>,
    pub allow_broad_context: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionProposalStatus {
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionProposalValidationCheck {
    ConflictInputsPreserved,
    PermissionProfileNamed,
    FocusedContextBounded,
    FocusedToolsBounded,
    OutputPathsExact,
    OutputBytesBounded,
    NoFileDeletion,
    ExactTreeObjects,
    WorktreeUnchanged,
    RefsUnchanged,
    WholeTreeLinksValid,
    MassDeletionPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionProposalPath {
    pub path: String,
    pub mode: String,
    pub content_hash: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionProposalContext {
    pub path: String,
    pub content_hash: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionProposalToolCall {
    pub name: String,
    pub arguments_hash: String,
    pub result_hash: String,
    pub referenced_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionProposal {
    pub version: u32,
    pub proposal_id: String,
    pub status: ResolutionProposalStatus,
    pub conflict_id: String,
    pub repository_key: String,
    pub base_revision: String,
    pub local_revision: String,
    pub remote_revision: String,
    pub policy_version: u32,
    pub policy_hash: String,
    pub provider: String,
    pub model: String,
    pub prompt_contract_version: u32,
    pub tool_contract_version: u32,
    pub permission_profile: String,
    pub broad_context_allowed: bool,
    #[serde(default)]
    pub focused_context: Vec<ResolutionProposalContext>,
    #[serde(default)]
    pub tool_calls: Vec<ResolutionProposalToolCall>,
    pub explanation: String,
    pub referenced_context: Vec<String>,
    pub proposal_tree: String,
    pub patch: String,
    pub paths: Vec<ResolutionProposalPath>,
    pub validation: Vec<ResolutionProposalValidationCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApproveResolutionProposalOptions {
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub dry_run: bool,
    pub automatic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionProposalAuditAction {
    Approved,
    AutoAccepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionProposalAuditRecord {
    pub version: u32,
    pub event_id: String,
    pub repository_key: String,
    pub conflict_id: String,
    pub proposal_id: String,
    pub action: ResolutionProposalAuditAction,
    pub provider: String,
    pub model: String,
    pub prompt_contract_version: u32,
    pub tool_contract_version: u32,
    pub proposal_tree: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_commit: Option<String>,
    pub validation: Vec<ResolutionProposalValidationCheck>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectResolutionProposalOutcome {
    Planned,
    Rejected,
    AlreadyRejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RejectResolutionProposalReport {
    pub vault: PathBuf,
    pub repository_key: String,
    pub conflict_id: String,
    pub proposal_id: String,
    pub dry_run: bool,
    pub outcome: RejectResolutionProposalOutcome,
    pub event_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApproveResolutionProposalOutcome {
    Planned,
    Applied,
    AlreadyApplied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApproveResolutionProposalReport {
    pub vault: PathBuf,
    pub repository_key: String,
    pub conflict_id: String,
    pub proposal_id: String,
    pub dry_run: bool,
    pub outcome: ApproveResolutionProposalOutcome,
    pub proposal_tree: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_refresh: Option<ScanSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutoAcceptResolutionProposalReport {
    pub proposal: ResolutionProposal,
    pub approval: ApproveResolutionProposalReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SuppliedResolutionPreviewReport {
    pub vault: PathBuf,
    pub repository_key: String,
    pub conflict_id: String,
    pub dry_run: bool,
    pub outcome: ApproveResolutionProposalOutcome,
    pub paths: Vec<ResolutionProposalPath>,
    pub validation: Vec<ResolutionProposalValidationCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PatchResolutionPreviewReport {
    pub vault: PathBuf,
    pub repository_key: String,
    pub conflict_id: String,
    pub dry_run: bool,
    pub outcome: ApproveResolutionProposalOutcome,
    pub paths: Vec<String>,
    pub validation: Vec<ResolutionProposalValidationCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorResolutionFile {
    pub path: String,
    pub initial_content: Vec<u8>,
    pub initial_hash: String,
    pub marker_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorResolutionPlan {
    pub vault: PathBuf,
    pub repository_key: String,
    pub conflict_id: String,
    pub files: Vec<EditorResolutionFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditorResolutionPreviewReport {
    pub vault: PathBuf,
    pub repository_key: String,
    pub conflict_id: String,
    pub dry_run: bool,
    pub outcome: ApproveResolutionProposalOutcome,
    pub paths: Vec<String>,
    pub validation: Vec<ResolutionProposalValidationCheck>,
}

pub fn preview_supplied_resolution(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    supplied: Vec<ResolutionAgentPathOutput>,
) -> Result<SuppliedResolutionPreviewReport, AppError> {
    let state_store = SyncStateStore::user_default()?;
    preview_supplied_resolution_with_state_store(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        supplied,
        &state_store,
    )
}

pub fn preview_supplied_resolution_with_state_store(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    supplied: Vec<ResolutionAgentPathOutput>,
    state_store: &SyncStateStore,
) -> Result<SuppliedResolutionPreviewReport, AppError> {
    if !approval_options.dry_run {
        return Err(AppError::operation(
            "supplied-resolution preview requires dry-run mode",
        ));
    }
    let manual = prepare_manual_resolution_scope(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        state_store,
    )?;
    let prepared = prepare_output(
        &manual.engine,
        &manual.repository,
        &manual.record,
        &BTreeSet::new(),
        ResolutionAgentOutput {
            explanation: "Resolution content supplied explicitly by the user.".to_string(),
            referenced_context: Vec::new(),
            paths: supplied,
        },
        Vec::new(),
    )?;
    Ok(SuppliedResolutionPreviewReport {
        vault: manual.vault,
        repository_key: manual.repository_key,
        conflict_id: conflict_id.to_string(),
        dry_run: true,
        outcome: ApproveResolutionProposalOutcome::Planned,
        paths: prepared.paths,
        validation: vec![
            ResolutionProposalValidationCheck::ConflictInputsPreserved,
            ResolutionProposalValidationCheck::PermissionProfileNamed,
            ResolutionProposalValidationCheck::OutputPathsExact,
            ResolutionProposalValidationCheck::OutputBytesBounded,
            ResolutionProposalValidationCheck::NoFileDeletion,
            ResolutionProposalValidationCheck::WorktreeUnchanged,
            ResolutionProposalValidationCheck::RefsUnchanged,
        ],
    })
}

pub fn preview_patch_resolution(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    patch: &[u8],
) -> Result<PatchResolutionPreviewReport, AppError> {
    if !approval_options.dry_run {
        return Err(AppError::operation(
            "patch-resolution preview requires dry-run mode",
        ));
    }
    let state_store = SyncStateStore::user_default()?;
    let manual = prepare_manual_resolution_scope(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        &state_store,
    )?;
    let local = GitOid::parse(&manual.record.local_revision).map_err(AppError::operation)?;
    let patch_paths = manual
        .engine
        .check_patch(&manual.repository, &local, patch)
        .map_err(AppError::operation)?;
    require_exact_conflict_paths(&manual.record, &patch_paths)?;
    Ok(PatchResolutionPreviewReport {
        vault: manual.vault,
        repository_key: manual.repository_key,
        conflict_id: conflict_id.to_string(),
        dry_run: true,
        outcome: ApproveResolutionProposalOutcome::Planned,
        paths: patch_paths,
        validation: vec![
            ResolutionProposalValidationCheck::ConflictInputsPreserved,
            ResolutionProposalValidationCheck::PermissionProfileNamed,
            ResolutionProposalValidationCheck::OutputPathsExact,
            ResolutionProposalValidationCheck::WorktreeUnchanged,
            ResolutionProposalValidationCheck::RefsUnchanged,
        ],
    })
}

pub fn resolution_paths_from_patch(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    patch: &[u8],
) -> Result<Vec<ResolutionAgentPathOutput>, AppError> {
    if approval_options.dry_run {
        return Err(AppError::operation(
            "patch resolution paths require mutating mode",
        ));
    }
    let state_store = SyncStateStore::user_default()?;
    let manual = prepare_manual_resolution_scope(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        &state_store,
    )?;
    let local = GitOid::parse(&manual.record.local_revision).map_err(AppError::operation)?;
    let patch_paths = manual
        .engine
        .check_patch(&manual.repository, &local, patch)
        .map_err(AppError::operation)?;
    require_exact_conflict_paths(&manual.record, &patch_paths)?;
    let tree = manual
        .engine
        .apply_patch_to_tree(&manual.repository, &local, patch)
        .map_err(AppError::operation)?;
    patch_paths
        .into_iter()
        .map(|path| {
            let object = manual
                .engine
                .path_object(&manual.repository, &tree, &path)
                .map_err(AppError::operation)?
                .ok_or_else(|| {
                    AppError::operation(format!("supplied patch deleted conflict path `{path}`"))
                })?;
            let data = object.data.ok_or_else(|| {
                AppError::operation(format!("supplied patch path `{path}` is not a blob"))
            })?;
            Ok(ResolutionAgentPathOutput {
                path,
                content: data,
            })
        })
        .collect()
}

pub fn prepare_editor_resolution(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
) -> Result<EditorResolutionPlan, AppError> {
    let state_store = SyncStateStore::user_default()?;
    let manual = prepare_manual_resolution_scope(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        &state_store,
    )?;
    let base = GitOid::parse(
        manual
            .record
            .base_revision
            .as_deref()
            .ok_or_else(|| AppError::operation("editor resolution requires one merge base"))?,
    )
    .map_err(AppError::operation)?;
    let local = GitOid::parse(&manual.record.local_revision).map_err(AppError::operation)?;
    let remote = GitOid::parse(&manual.record.remote_revision).map_err(AppError::operation)?;
    let mut total = 0_usize;
    let mut files = Vec::with_capacity(manual.record.paths.len());
    for conflict_path in &manual.record.paths {
        let base_content = editor_side_content(
            &manual.engine,
            &manual.repository,
            &base,
            &conflict_path.path,
        )?;
        let local_content = editor_side_content(
            &manual.engine,
            &manual.repository,
            &local,
            &conflict_path.path,
        )?;
        let remote_content = editor_side_content(
            &manual.engine,
            &manual.repository,
            &remote,
            &conflict_path.path,
        )?;
        let marker_token = format!("VULCAN-CONFLICT-{}", manual.record.id);
        let initial_content = render_editor_conflict(
            &marker_token,
            &base_content,
            &local_content,
            &remote_content,
        );
        if initial_content.len() > MAX_AGENT_FILE_BYTES {
            return Err(AppError::operation(format!(
                "editor resolution `{}` exceeds the per-file byte limit",
                conflict_path.path
            )));
        }
        total = total.saturating_add(initial_content.len());
        files.push(EditorResolutionFile {
            path: conflict_path.path.clone(),
            initial_hash: blake3::hash(&initial_content).to_hex().to_string(),
            initial_content,
            marker_token,
        });
    }
    if total > MAX_AGENT_TOTAL_BYTES {
        return Err(AppError::operation(
            "editor resolution files exceed the total byte limit",
        ));
    }
    Ok(EditorResolutionPlan {
        vault: manual.vault,
        repository_key: manual.repository_key,
        conflict_id: conflict_id.to_string(),
        files,
    })
}

impl EditorResolutionPlan {
    #[must_use]
    pub fn preview_report(&self) -> EditorResolutionPreviewReport {
        EditorResolutionPreviewReport {
            vault: self.vault.clone(),
            repository_key: self.repository_key.clone(),
            conflict_id: self.conflict_id.clone(),
            dry_run: true,
            outcome: ApproveResolutionProposalOutcome::Planned,
            paths: self.files.iter().map(|file| file.path.clone()).collect(),
            validation: vec![
                ResolutionProposalValidationCheck::ConflictInputsPreserved,
                ResolutionProposalValidationCheck::PermissionProfileNamed,
                ResolutionProposalValidationCheck::OutputPathsExact,
                ResolutionProposalValidationCheck::OutputBytesBounded,
                ResolutionProposalValidationCheck::WorktreeUnchanged,
                ResolutionProposalValidationCheck::RefsUnchanged,
            ],
        }
    }
}

fn editor_side_content(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    revision: &GitOid,
    path: &str,
) -> Result<String, AppError> {
    let object = engine
        .path_object(repository, revision, path)
        .map_err(AppError::operation)?
        .ok_or_else(|| {
            AppError::operation(format!(
                "editor resolution requires `{path}` to exist on every preserved side"
            ))
        })?;
    let data = object.data.ok_or_else(|| {
        AppError::operation(format!(
            "editor resolution requires `{path}` to be a regular blob"
        ))
    })?;
    String::from_utf8(data).map_err(|_| {
        AppError::operation(format!(
            "editor resolution requires `{path}` to be valid UTF-8"
        ))
    })
}

fn render_editor_conflict(marker: &str, base: &str, local: &str, remote: &str) -> Vec<u8> {
    let mut output = String::new();
    output.push_str("<<<<<<< ");
    output.push_str(marker);
    output.push_str(" LOCAL\n");
    append_editor_side(&mut output, local);
    output.push_str("||||||| ");
    output.push_str(marker);
    output.push_str(" BASE\n");
    append_editor_side(&mut output, base);
    output.push_str("======= ");
    output.push_str(marker);
    output.push('\n');
    append_editor_side(&mut output, remote);
    output.push_str(">>>>>>> ");
    output.push_str(marker);
    output.push_str(" REMOTE\n");
    output.into_bytes()
}

fn append_editor_side(output: &mut String, content: &str) {
    output.push_str(content);
    if !content.ends_with('\n') {
        output.push('\n');
    }
}

struct ManualResolutionScope {
    vault: PathBuf,
    repository_key: String,
    record: SyncConflictRecord,
    engine: vulcan_sync::GitCliEngine,
    repository: vulcan_sync::GitRepository,
}

fn prepare_manual_resolution_scope(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    state_store: &SyncStateStore,
) -> Result<ManualResolutionScope, AppError> {
    let AgentScope {
        vault,
        repository_key,
        record,
        ..
    } = prepare_agent_scope(paths, conflict_id, proposal_options, state_store)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let conflict_store = SyncConflictStore::from_state_store(state_store);
    if conflict_store
        .get_resolution(&repository_key, conflict_id)?
        .is_some_and(|resolution| !resolution.is_abandoned())
    {
        return Err(AppError::operation(
            "the conflict already has a resolution in progress or applied",
        ));
    }
    ensure_no_existing_proposal(state_store, &repository_key, conflict_id)?;
    verify_preserved_conflict_refs(&engine, &repository, &record)?;
    let safety = engine
        .safety_state(&repository)
        .map_err(AppError::operation)?;
    if safety.staged_changes || safety.operation.is_some() {
        return Err(AppError::operation(
            "supplied resolution requires a clean normal index and no Git operation in progress",
        ));
    }
    let local = conflict_worktree_revision(&record)?;
    let expected_tree = conflict_worktree_tree(&engine, &repository, &record)?;
    if engine
        .snapshot_worktree_tree(&repository, Some(&local))
        .map_err(AppError::operation)?
        != expected_tree
    {
        return Err(AppError::operation(
            "the worktree no longer matches the preserved local conflict input",
        ));
    }
    if engine
        .remote_ref(
            &repository,
            &approval_options.remote,
            &approval_options.live_ref,
        )
        .map_err(AppError::operation)?
        .as_ref()
        .map(GitOid::as_str)
        != Some(conflict_live_input(&record)?)
    {
        return Err(AppError::operation(
            "the remote live ref moved after the conflict inputs were preserved",
        ));
    }
    Ok(ManualResolutionScope {
        vault,
        repository_key,
        record,
        engine,
        repository,
    })
}

fn require_exact_conflict_paths(
    record: &SyncConflictRecord,
    actual: &[String],
) -> Result<(), AppError> {
    let mut expected = record
        .paths
        .iter()
        .map(|path| path.path.clone())
        .collect::<Vec<_>>();
    expected.sort();
    expected.dedup();
    if actual == expected {
        Ok(())
    } else {
        Err(AppError::operation(format!(
            "supplied patch paths do not exactly match the conflict: expected {expected:?}, got {actual:?}"
        )))
    }
}

pub fn create_resolution_proposal_with_provider(
    paths: &VaultPaths,
    conflict_id: &str,
    options: &ResolutionProposalOptions,
    provider: &dyn ResolutionAgentProvider,
    cancellation: &SyncCancellationToken,
    state_store: &SyncStateStore,
) -> Result<ResolutionProposal, AppError> {
    cancellation_check(cancellation)?;
    let AgentScope {
        vault,
        repository_key,
        record,
        permission_guard,
    } = prepare_agent_scope(paths, conflict_id, options, state_store)?;
    let base_revision = record
        .base_revision
        .as_deref()
        .ok_or_else(|| AppError::operation("agent resolution requires one merge base"))?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    // Serialize the pre-generation checks, then release the repository
    // lock across the unbounded provider network call: holding it would
    // stall every other sync transaction for minutes. The post-generation
    // checks below re-run under a fresh lock, so a concurrent generation
    // or worktree edit fails cleanly instead of corrupting state.
    let inputs = locked_generation_inputs(
        &repository,
        paths,
        &engine,
        &record,
        options,
        state_store,
        conflict_id,
        &repository_key,
    )?;
    let ProviderRun {
        identity,
        output,
        tool_calls,
        supplied_context,
    } = run_provider_with_tools(
        paths,
        permission_guard,
        options,
        &inputs.request,
        provider,
        cancellation,
    )?;
    let _lock = acquire_proposal_lock(&repository)?;
    cancellation_check(cancellation)?;
    ensure_no_existing_proposal(state_store, &repository_key, conflict_id)?;
    persist_generated_proposal(
        paths,
        &engine,
        &repository,
        &record,
        &repository_key,
        options,
        state_store,
        base_revision,
        &inputs,
        ProviderRun {
            identity,
            output,
            tool_calls,
            supplied_context,
        },
    )
}

/// Acquires the shared repository mutation lock, preserving this
/// workflow's historical contention message.
fn acquire_proposal_lock(
    repository: &vulcan_sync::GitRepository,
) -> Result<vulcan_sync::RepositoryLock, AppError> {
    vulcan_sync::RepositoryLock::acquire(&repository.git_dir).map_err(|error| {
        if matches!(error, vulcan_sync::RepositoryLockError::Locked) {
            AppError::operation("another repository mutation is in progress")
        } else {
            AppError::from(error)
        }
    })
}

struct GenerationInputs {
    refs_before: Vec<(String, Option<String>)>,
    worktree_before: vulcan_sync::GitOid,
    request: ResolutionAgentRequest,
}

/// Persists a generated proposal after re-running every post-generation
/// check under the repository lock.
#[allow(clippy::too_many_arguments)]
fn persist_generated_proposal(
    paths: &VaultPaths,
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
    repository_key: &str,
    options: &ResolutionProposalOptions,
    state_store: &SyncStateStore,
    base_revision: &str,
    inputs: &GenerationInputs,
    run: ProviderRun,
) -> Result<ResolutionProposal, AppError> {
    let prepared = prepare_output(
        engine,
        repository,
        record,
        &run.supplied_context,
        run.output,
        run.tool_calls,
    )?;
    let proposal_tree = engine
        .resolve_merge_tree_with_paths(
            repository,
            &GitContentMergeResolutionRequest {
                base: GitOid::parse(base_revision).map_err(AppError::operation)?,
                accepted_remote: GitOid::parse(&record.remote_revision)
                    .map_err(AppError::operation)?,
                local_candidate: GitOid::parse(&record.local_revision)
                    .map_err(AppError::operation)?,
                paths: prepared.git_paths.clone(),
            },
        )
        .map_err(AppError::operation)?;
    verify_tree_objects(engine, repository, &proposal_tree, &prepared.git_paths)?;
    let conflict_paths = conflict_path_names(record);
    validate_proposal_whole_tree_inputs(
        paths,
        engine,
        repository,
        base_revision,
        &record.local_revision,
        &record.remote_revision,
        &proposal_tree,
        &conflict_paths,
    )?;
    verify_no_external_mutation(
        engine,
        repository,
        record,
        &inputs.worktree_before,
        &inputs.refs_before,
    )?;
    let patch = engine
        .diff_patch(
            repository,
            &GitOid::parse(&record.remote_revision).map_err(AppError::operation)?,
            &proposal_tree,
            &conflict_paths,
        )
        .map_err(AppError::operation)?;
    let proposal = assemble_proposal(
        record,
        repository_key.to_string(),
        run.identity,
        options,
        &inputs.request.focused_context,
        prepared,
        ProposalTree {
            oid: proposal_tree,
            patch,
        },
    )?;
    save_proposal(state_store, &proposal)?;
    Ok(proposal)
}

struct AgentScope {
    vault: PathBuf,
    repository_key: String,
    record: SyncConflictRecord,
    permission_guard: ProfilePermissionGuard,
}

/// Captures the pre-generation inputs under the repository lock. The caller
/// releases the lock across the provider call and re-validates afterwards.
#[allow(clippy::too_many_arguments)]
fn locked_generation_inputs(
    repository: &vulcan_sync::GitRepository,
    paths: &VaultPaths,
    engine: &dyn GitEngine,
    record: &SyncConflictRecord,
    options: &ResolutionProposalOptions,
    state_store: &SyncStateStore,
    conflict_id: &str,
    repository_key: &str,
) -> Result<GenerationInputs, AppError> {
    let _pre_lock = acquire_proposal_lock(repository)?;
    ensure_no_existing_proposal(state_store, repository_key, conflict_id)?;
    verify_preserved_conflict_refs(engine, repository, record)?;
    let refs_before = preserved_ref_snapshot(engine, repository, record)?;
    let local_revision = conflict_worktree_revision(record)?;
    let worktree_before = engine
        .snapshot_worktree_tree(repository, Some(&local_revision))
        .map_err(AppError::operation)?;
    let request = build_agent_request(paths, engine, repository, record, options)?;
    Ok(GenerationInputs {
        refs_before,
        worktree_before,
        request,
    })
}

fn prepare_agent_scope(
    paths: &VaultPaths,
    conflict_id: &str,
    options: &ResolutionProposalOptions,
    state_store: &SyncStateStore,
) -> Result<AgentScope, AppError> {
    validate_options(options)?;
    let selection = resolve_permission_profile(paths, Some(&options.permission_profile))
        .map_err(AppError::operation)?;
    let permission_guard = ProfilePermissionGuard::new(paths, selection);
    permission_guard.check_git().map_err(AppError::operation)?;
    for path in &options.focused_context {
        permission_guard
            .check_read_path(path)
            .map_err(AppError::operation)?;
    }
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let repository_key = repository_state_key(&vault);
    let record =
        SyncConflictStore::from_state_store(state_store).get(&repository_key, conflict_id)?;
    validate_agent_conflict_scope(&record)?;
    for path in &record.paths {
        permission_guard
            .check_read_path(&path.path)
            .map_err(AppError::operation)?;
    }
    Ok(AgentScope {
        vault,
        repository_key,
        record,
        permission_guard,
    })
}

struct ProviderRun {
    identity: ResolutionAgentIdentity,
    output: ResolutionAgentOutput,
    tool_calls: Vec<ResolutionProposalToolCall>,
    supplied_context: BTreeSet<String>,
}

fn run_provider_with_tools(
    paths: &VaultPaths,
    guard: ProfilePermissionGuard,
    options: &ResolutionProposalOptions,
    request: &ResolutionAgentRequest,
    provider: &dyn ResolutionAgentProvider,
    cancellation: &SyncCancellationToken,
) -> Result<ProviderRun, AppError> {
    let explicit_paths = request
        .focused_context
        .iter()
        .map(|context| context.path.clone())
        .collect::<Vec<_>>();
    let mut tools = VaultResolutionAgentTools::new(
        paths,
        guard,
        options.allow_broad_context,
        explicit_paths.iter().cloned(),
    );
    let (identity, output) = invoke_provider(provider, request, &mut tools, cancellation)?;
    let mut supplied_context = tools.referenced_paths;
    supplied_context.extend(explicit_paths);
    Ok(ProviderRun {
        identity,
        output,
        tool_calls: tools.calls,
        supplied_context,
    })
}

fn invoke_provider(
    provider: &dyn ResolutionAgentProvider,
    request: &ResolutionAgentRequest,
    tools: &mut dyn ResolutionAgentTools,
    cancellation: &SyncCancellationToken,
) -> Result<(ResolutionAgentIdentity, ResolutionAgentOutput), AppError> {
    cancellation_check(cancellation)?;
    let identity = provider.identity();
    validate_identity(&identity)?;
    let output = provider.propose(request, tools, cancellation)?;
    cancellation_check(cancellation)?;
    Ok((identity, output))
}

fn conflict_path_names(record: &SyncConflictRecord) -> Vec<String> {
    record.paths.iter().map(|path| path.path.clone()).collect()
}

pub fn create_resolution_proposal(
    paths: &VaultPaths,
    conflict_id: &str,
    options: &ResolutionProposalOptions,
    provider: &dyn ResolutionAgentProvider,
    cancellation: &SyncCancellationToken,
) -> Result<ResolutionProposal, AppError> {
    let state_store = SyncStateStore::user_default()?;
    create_resolution_proposal_with_provider(
        paths,
        conflict_id,
        options,
        provider,
        cancellation,
        &state_store,
    )
}

pub fn create_and_auto_accept_resolution_proposal(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    provider: &dyn ResolutionAgentProvider,
    cancellation: &SyncCancellationToken,
) -> Result<AutoAcceptResolutionProposalReport, AppError> {
    let state_store = SyncStateStore::user_default()?;
    create_and_auto_accept_resolution_proposal_with_state_store(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        provider,
        cancellation,
        &state_store,
    )
}

pub fn create_and_auto_accept_resolution_proposal_with_state_store(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    provider: &dyn ResolutionAgentProvider,
    cancellation: &SyncCancellationToken,
    state_store: &SyncStateStore,
) -> Result<AutoAcceptResolutionProposalReport, AppError> {
    if approval_options.dry_run || !approval_options.automatic {
        return Err(AppError::operation(
            "agent auto-accept requires a mutating automatic approval request",
        ));
    }
    let loaded = vulcan_core::load_vault_config(paths);
    if !loaded.config.sync.agent_auto_accept {
        return Err(AppError::operation(
            "agent auto-accept is disabled; set sync.agent_auto_accept=true in device-local config and request it explicitly",
        ));
    }
    let proposal = create_resolution_proposal_with_provider(
        paths,
        conflict_id,
        proposal_options,
        provider,
        cancellation,
        state_store,
    )?;
    let approval = approve_resolution_proposal_with_state_store(
        paths,
        conflict_id,
        &proposal.proposal_id,
        approval_options,
        cancellation,
        state_store,
    )
    .map_err(|error| {
        AppError::operation(format!(
            "auto-accept failed after retaining proposal {}; it remains ready for explicit review: {error}",
            proposal.proposal_id
        ))
    })?;
    Ok(AutoAcceptResolutionProposalReport { proposal, approval })
}

pub fn load_resolution_proposal(
    state_store: &SyncStateStore,
    repository_key: &str,
    conflict_id: &str,
    proposal_id: &str,
) -> Result<ResolutionProposal, AppError> {
    for (label, value) in [
        ("repository key", repository_key),
        ("conflict ID", conflict_id),
        ("proposal ID", proposal_id),
    ] {
        validate_hex_id(label, value)?;
    }
    let path = proposal_path(state_store, repository_key, conflict_id, proposal_id);
    let metadata = fs::metadata(&path).map_err(AppError::operation)?;
    if metadata.len() > MAX_PROPOSAL_RECORD_BYTES as u64 {
        return Err(AppError::operation(
            "resolution proposal exceeds its byte limit",
        ));
    }
    let proposal: ResolutionProposal =
        serde_json::from_slice(&fs::read(&path).map_err(AppError::operation)?)
            .map_err(AppError::operation)?;
    if !(1..=RESOLUTION_PROPOSAL_VERSION).contains(&proposal.version)
        || proposal.repository_key != repository_key
        || proposal.conflict_id != conflict_id
        || proposal.proposal_id != proposal_id
    {
        return Err(AppError::operation(
            "resolution proposal identity or version mismatch",
        ));
    }
    if proposal.version == RESOLUTION_PROPOSAL_VERSION
        && proposal.proposal_id != recompute_current_proposal_id(&proposal)?
    {
        return Err(AppError::operation(
            "resolution proposal content does not match its immutable ID",
        ));
    }
    Ok(proposal)
}

pub fn reject_resolution_proposal_with_state_store(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_id: &str,
    dry_run: bool,
    state_store: &SyncStateStore,
) -> Result<RejectResolutionProposalReport, AppError> {
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let repository_key = repository_state_key(&vault);
    let conflict_store = SyncConflictStore::from_state_store(state_store);
    let conflict = conflict_store.get(&repository_key, conflict_id)?;
    if !crate::sync_state::same_work_tree(&conflict.work_tree, &vault) {
        return Err(AppError::operation(
            "sync conflict record does not belong to the selected worktree",
        ));
    }
    let proposal =
        load_resolution_proposal(state_store, &repository_key, conflict_id, proposal_id)?;
    validate_proposal_inputs(&conflict, &proposal)?;
    let rejection = proposal_rejection_record(&proposal);
    let existing_rejection = load_proposal_audit(state_store, &rejection)?;
    ensure_proposal_has_no_resolution(&conflict_store, &repository_key, &proposal)?;
    if existing_rejection.is_some() {
        return Ok(rejection_report(
            &vault,
            &proposal,
            dry_run,
            RejectResolutionProposalOutcome::AlreadyRejected,
            &rejection.event_id,
        ));
    }
    if dry_run {
        return Ok(rejection_report(
            &vault,
            &proposal,
            true,
            RejectResolutionProposalOutcome::Planned,
            &rejection.event_id,
        ));
    }

    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let _lock = acquire_proposal_lock(&repository)?;
    ensure_proposal_has_no_resolution(&conflict_store, &repository_key, &proposal)?;
    if load_proposal_audit(state_store, &rejection)?.is_some() {
        return Ok(rejection_report(
            &vault,
            &proposal,
            false,
            RejectResolutionProposalOutcome::AlreadyRejected,
            &rejection.event_id,
        ));
    }
    save_proposal_audit(state_store, &rejection)?;
    Ok(rejection_report(
        &vault,
        &proposal,
        false,
        RejectResolutionProposalOutcome::Rejected,
        &rejection.event_id,
    ))
}

pub fn reject_resolution_proposal(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_id: &str,
    dry_run: bool,
) -> Result<RejectResolutionProposalReport, AppError> {
    let state_store = SyncStateStore::user_default()?;
    reject_resolution_proposal_with_state_store(
        paths,
        conflict_id,
        proposal_id,
        dry_run,
        &state_store,
    )
}

pub fn approve_resolution_proposal_with_state_store(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_id: &str,
    options: &ApproveResolutionProposalOptions,
    cancellation: &SyncCancellationToken,
    state_store: &SyncStateStore,
) -> Result<ApproveResolutionProposalReport, AppError> {
    cancellation_check(cancellation)?;
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let repository_key = repository_state_key(&vault);
    let store = SyncConflictStore::from_state_store(state_store);
    let record = store.get(&repository_key, conflict_id)?;
    if !crate::sync_state::same_work_tree(&record.work_tree, &vault) {
        return Err(AppError::operation(
            "sync conflict record does not belong to the selected worktree",
        ));
    }
    let proposal =
        load_resolution_proposal(state_store, &repository_key, conflict_id, proposal_id)?;
    validate_proposal_inputs(&record, &proposal)?;
    ensure_proposal_not_rejected(state_store, &proposal)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    verify_preserved_conflict_refs(&engine, &repository, &record)?;
    revalidate_proposal_tree(&engine, &repository, &record, &proposal, false)?;
    revalidate_proposal_whole_tree(paths, &engine, &repository, &proposal)?;
    let existing = store
        .get_resolution(&repository_key, conflict_id)?
        .filter(|resolution| !resolution.is_abandoned());
    validate_existing_proposal_resolution(existing.as_ref(), &record, &proposal)?;
    if existing
        .as_ref()
        .is_some_and(|resolution| resolution.applied)
    {
        return Ok(proposal_report(
            &vault,
            &proposal,
            options,
            ApproveResolutionProposalOutcome::AlreadyApplied,
            existing.as_ref(),
            None,
        ));
    }
    if options.dry_run {
        verify_approval_preconditions(
            &engine,
            &repository,
            &record,
            &proposal,
            options,
            existing.as_ref(),
        )?;
        return Ok(proposal_report(
            &vault,
            &proposal,
            options,
            ApproveResolutionProposalOutcome::Planned,
            None,
            None,
        ));
    }

    apply_approved_proposal(
        &ApprovalExecution {
            paths,
            vault: &vault,
            repository_key: &repository_key,
            record: &record,
            proposal: &proposal,
            options,
            state_store,
            store: &store,
        },
        &engine,
        &repository,
        cancellation,
    )
}

pub fn approve_resolution_proposal(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_id: &str,
    options: &ApproveResolutionProposalOptions,
    cancellation: &SyncCancellationToken,
) -> Result<ApproveResolutionProposalReport, AppError> {
    let state_store = SyncStateStore::user_default()?;
    approve_resolution_proposal_with_state_store(
        paths,
        conflict_id,
        proposal_id,
        options,
        cancellation,
        &state_store,
    )
}

struct ApprovalExecution<'a> {
    paths: &'a VaultPaths,
    vault: &'a Path,
    repository_key: &'a str,
    record: &'a SyncConflictRecord,
    proposal: &'a ResolutionProposal,
    options: &'a ApproveResolutionProposalOptions,
    state_store: &'a SyncStateStore,
    store: &'a SyncConflictStore,
}

fn apply_approved_proposal(
    context: &ApprovalExecution<'_>,
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    cancellation: &SyncCancellationToken,
) -> Result<ApproveResolutionProposalReport, AppError> {
    let _lock = acquire_proposal_lock(repository)?;
    cancellation_check(cancellation)?;
    ensure_proposal_not_rejected(context.state_store, context.proposal)?;
    verify_preserved_conflict_refs(engine, repository, context.record)?;
    revalidate_proposal_tree(engine, repository, context.record, context.proposal, true)?;
    revalidate_proposal_whole_tree(context.paths, engine, repository, context.proposal)?;
    let local = conflict_worktree_revision(context.record)?;
    let recovery_ref =
        conflict_recovery_ref(&context.record.id, "current").map_err(AppError::operation)?;
    let device_id = context
        .state_store
        .load_or_create_device_id(true)?
        .expect("mutating device identity creation returns an identity");
    let capture = engine
        .capture_worktree(
            repository,
            &GitCaptureRequest {
                base: Some(local.clone()),
                target_ref: recovery_ref,
                target_before: None,
                message: format!(
                    "vulcan proposal recovery snapshot\n\nVulcan-Conflict: {}\nVulcan-Proposal: {}\nVulcan-Sync-Version: 1\nVulcan-Sync-Device: {}\nVulcan-Sync-Source: {local}\nVulcan-Sync-Semantic: false\n",
                    context.record.id,
                    context.proposal.proposal_id,
                    device_id.as_str(),
                ),
            },
        )
        .map_err(AppError::operation)?;
    let immutable_recovery_ref = conflict_recovery_ref(&context.record.id, capture.commit.as_str())
        .map_err(AppError::operation)?;
    engine
        .update_ref(repository, &immutable_recovery_ref, &capture.commit)
        .map_err(AppError::operation)?;
    cancellation_check(cancellation)?;
    let existing = context
        .store
        .get_resolution(context.repository_key, &context.record.id)?
        .filter(|resolution| !resolution.is_abandoned());
    validate_existing_proposal_resolution(existing.as_ref(), context.record, context.proposal)?;
    verify_approval_preconditions(
        engine,
        repository,
        context.record,
        context.proposal,
        context.options,
        existing.as_ref(),
    )?;
    let mut resolution = resume_or_prepare_proposal(
        engine,
        repository,
        context.record,
        context.proposal,
        &capture,
        device_id.as_str(),
        existing,
    )?;
    context
        .store
        .save_resolution(context.repository_key, &resolution)?;
    cancellation_check(cancellation)?;
    publish_proposal_resolution(engine, repository, context.options, &mut resolution)?;
    context
        .store
        .save_resolution(context.repository_key, &resolution)?;
    cancellation_check(cancellation)?;
    let proposal_tree =
        GitOid::parse(&context.proposal.proposal_tree).map_err(AppError::operation)?;
    if capture.tree != proposal_tree {
        engine
            .apply_tree(
                repository,
                &capture.commit,
                &GitOid::parse(&resolution.resolution_commit).map_err(AppError::operation)?,
            )
            .map_err(AppError::operation)?;
    }
    update_sync_refs(engine, repository, context.options, &resolution)?;
    let cache_refresh = if context.paths.cache_db().is_file() {
        Some(refresh_cache_incrementally(context.paths)?)
    } else {
        None
    };
    save_approval_execution_audit(context, &resolution)?;
    resolution.applied = true;
    context
        .store
        .save_resolution(context.repository_key, &resolution)?;
    Ok(proposal_report(
        context.vault,
        context.proposal,
        context.options,
        ApproveResolutionProposalOutcome::Applied,
        Some(&resolution),
        cache_refresh,
    ))
}

fn save_approval_execution_audit(
    context: &ApprovalExecution<'_>,
    resolution: &SyncConflictResolutionRecord,
) -> Result<(), AppError> {
    save_approval_audit(
        context.state_store,
        context.proposal,
        resolution,
        context.options,
    )
}

fn validate_proposal_inputs(
    record: &SyncConflictRecord,
    proposal: &ResolutionProposal,
) -> Result<(), AppError> {
    if proposal.status != ResolutionProposalStatus::Ready
        || proposal.conflict_id != record.id
        || proposal.repository_key != record.repository_key
        || proposal.base_revision != record.base_revision.as_deref().unwrap_or_default()
        || proposal.local_revision != record.local_revision
        || proposal.remote_revision != record.remote_revision
        || proposal.policy_version != record.policy_version
        || proposal.policy_hash != record.policy_hash
    {
        return Err(AppError::operation(
            "resolution proposal no longer matches its immutable conflict inputs",
        ));
    }
    Ok(())
}

fn validate_existing_proposal_resolution(
    existing: Option<&SyncConflictResolutionRecord>,
    record: &SyncConflictRecord,
    proposal: &ResolutionProposal,
) -> Result<(), AppError> {
    if let Some(existing) = existing {
        if existing.side.is_some()
            || existing.proposal_id.as_deref() != Some(proposal.proposal_id.as_str())
            || existing.base_revision != proposal.base_revision
            || existing.local_revision != proposal.local_revision
            || existing.remote_revision != proposal.remote_revision
            || existing
                .live_input_revision
                .as_deref()
                .unwrap_or(&existing.remote_revision)
                != conflict_live_input(record)?
            || existing.resolved_tree != proposal.proposal_tree
        {
            return Err(AppError::operation(
                "another conflict resolution is already in progress",
            ));
        }
    }
    Ok(())
}

fn revalidate_proposal_tree(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
    proposal: &ResolutionProposal,
    reconstruct: bool,
) -> Result<(), AppError> {
    if proposal.paths.len() != record.paths.len() {
        return Err(AppError::operation(
            "resolution proposal path set no longer matches the conflict",
        ));
    }
    let tree = GitOid::parse(&proposal.proposal_tree).map_err(AppError::operation)?;
    let mut resolved = Vec::with_capacity(proposal.paths.len());
    let mut seen = BTreeSet::new();
    for path in &proposal.paths {
        if !seen.insert(path.path.as_str())
            || !record.paths.iter().any(|record| record.path == path.path)
        {
            return Err(AppError::operation(
                "resolution proposal contains a duplicate or unrelated path",
            ));
        }
        let object = engine
            .path_object(repository, &tree, &path.path)
            .map_err(AppError::operation)?
            .ok_or_else(|| AppError::operation(format!("proposal tree omitted `{}`", path.path)))?;
        let data = object.data.ok_or_else(|| {
            AppError::operation(format!("proposal path `{}` is not a blob", path.path))
        })?;
        if object.kind != "blob"
            || object.mode != path.mode
            || data.len() as u64 != path.bytes
            || blake3::hash(&data).to_hex().as_str() != path.content_hash
        {
            return Err(AppError::operation(format!(
                "proposal path `{}` failed exact object revalidation",
                path.path
            )));
        }
        validate_proposal_content(record, &path.path, &data)?;
        resolved.push(GitResolvedPath {
            path: path.path.clone(),
            mode: Some(path.mode.clone()),
            data: Some(data),
        });
    }
    if reconstruct {
        let reconstructed = engine
            .resolve_merge_tree_with_paths(
                repository,
                &GitContentMergeResolutionRequest {
                    base: GitOid::parse(&proposal.base_revision).map_err(AppError::operation)?,
                    accepted_remote: GitOid::parse(&proposal.remote_revision)
                        .map_err(AppError::operation)?,
                    local_candidate: GitOid::parse(&proposal.local_revision)
                        .map_err(AppError::operation)?,
                    paths: resolved,
                },
            )
            .map_err(AppError::operation)?;
        if reconstructed != tree {
            return Err(AppError::operation(
                "resolution proposal tree does not reconstruct from its immutable inputs",
            ));
        }
    }
    let patch = engine
        .diff_patch(
            repository,
            &GitOid::parse(&proposal.remote_revision).map_err(AppError::operation)?,
            &tree,
            &proposal
                .paths
                .iter()
                .map(|path| path.path.clone())
                .collect::<Vec<_>>(),
        )
        .map_err(AppError::operation)?;
    if patch != proposal.patch {
        return Err(AppError::operation(
            "resolution proposal patch no longer matches its tree",
        ));
    }
    Ok(())
}

fn revalidate_proposal_whole_tree(
    paths: &VaultPaths,
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    proposal: &ResolutionProposal,
) -> Result<(), AppError> {
    let tree = GitOid::parse(&proposal.proposal_tree).map_err(AppError::operation)?;
    let resolved_paths = proposal
        .paths
        .iter()
        .map(|path| path.path.clone())
        .collect::<Vec<_>>();
    validate_proposal_whole_tree_inputs(
        paths,
        engine,
        repository,
        &proposal.base_revision,
        &proposal.local_revision,
        &proposal.remote_revision,
        &tree,
        &resolved_paths,
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_proposal_whole_tree_inputs(
    paths: &VaultPaths,
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    base_revision: &str,
    local_revision: &str,
    remote_revision: &str,
    tree: &GitOid,
    resolved_paths: &[String],
) -> Result<(), AppError> {
    let config = load_validated_sync_config(paths)?;
    let base = GitOid::parse(base_revision).map_err(AppError::operation)?;
    let local = GitOid::parse(local_revision).map_err(AppError::operation)?;
    let remote = GitOid::parse(remote_revision).map_err(AppError::operation)?;
    validate_git_merge_tree(
        &config,
        engine,
        &GitAutomaticMergeValidation {
            repository,
            base: &base,
            local_candidate: &local,
            accepted_remote: &remote,
            merged_tree: tree,
            resolved_paths,
        },
    )
}

fn validate_proposal_content(
    record: &SyncConflictRecord,
    path: &str,
    data: &[u8],
) -> Result<(), AppError> {
    let kind = record
        .paths
        .iter()
        .find(|entry| entry.path == path)
        .and_then(|entry| entry.classification.as_ref())
        .map(|classification| classification.file_kind)
        .ok_or_else(|| AppError::operation(format!("proposal path `{path}` has no file kind")))?;
    match kind {
        vulcan_sync::MergeFileKind::Markdown => {
            let source = std::str::from_utf8(data).map_err(AppError::operation)?;
            let parsed = vulcan_core::parse_document(source, &vulcan_core::VaultConfig::default());
            if !parsed.diagnostics.is_empty() {
                return Err(AppError::operation(format!(
                    "proposal Markdown `{path}` produced parser diagnostics"
                )));
            }
        }
        vulcan_sync::MergeFileKind::Json | vulcan_sync::MergeFileKind::Canvas => {
            serde_json::from_slice::<serde_json::Value>(data).map_err(AppError::operation)?;
        }
        vulcan_sync::MergeFileKind::Bases => {
            serde_yaml::from_slice::<serde_yaml::Value>(data).map_err(AppError::operation)?;
        }
        vulcan_sync::MergeFileKind::Text => {
            std::str::from_utf8(data).map_err(AppError::operation)?;
        }
        vulcan_sync::MergeFileKind::Binary
        | vulcan_sync::MergeFileKind::ObsidianState
        | vulcan_sync::MergeFileKind::Missing => {
            return Err(AppError::operation(format!(
                "proposal path `{path}` has an ineligible file kind"
            )));
        }
    }
    Ok(())
}

fn prepare_proposal_resolution(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
    proposal: &ResolutionProposal,
    capture: &vulcan_sync::GitCapture,
    device_id: &str,
) -> Result<SyncConflictResolutionRecord, AppError> {
    let local = GitOid::parse(&record.local_revision).map_err(AppError::operation)?;
    let local_tree = conflict_worktree_tree(engine, repository, record)?;
    if capture.tree != local_tree {
        return Err(AppError::operation(
            "the worktree changed after the proposal was created; its recovery snapshot was retained",
        ));
    }
    let tree = GitOid::parse(&proposal.proposal_tree).map_err(AppError::operation)?;
    let remote = GitOid::parse(&record.remote_revision).map_err(AppError::operation)?;
    let live_input = GitOid::parse(conflict_live_input(record)?).map_err(AppError::operation)?;
    let parents = if live_input == remote {
        vec![remote.clone(), local.clone()]
    } else {
        vec![live_input.clone()]
    };
    let commit = engine
        .create_commit(
            repository,
            &tree,
            &parents,
            &format!(
                "vulcan conflict proposal resolution\n\nVulcan-Conflict: {}\nVulcan-Proposal: {}\nVulcan-Resolution-Provider: {}\nVulcan-Resolution-Model: {}\nVulcan-Sync-Version: 1\nVulcan-Sync-Device: {device_id}\nVulcan-Sync-Policy: {}:{}\nVulcan-Sync-Source: {remote}+{local}\nVulcan-Sync-Semantic: false\n",
                record.id,
                proposal.proposal_id,
                proposal.provider,
                proposal.model,
                record.policy_version,
                record.policy_hash,
            ),
        )
        .map_err(AppError::operation)?;
    let resolved_ref = conflict_proposal_resolution_ref(&record.id, &proposal.proposal_id)
        .map_err(AppError::operation)?;
    engine
        .update_ref(repository, &resolved_ref, &commit)
        .map_err(AppError::operation)?;
    Ok(SyncConflictResolutionRecord {
        version: SYNC_CONFLICT_RESOLUTION_VERSION,
        conflict_id: record.id.clone(),
        side: None,
        proposal_id: Some(proposal.proposal_id.clone()),
        base_revision: proposal.base_revision.clone(),
        local_revision: proposal.local_revision.clone(),
        remote_revision: proposal.remote_revision.clone(),
        live_input_revision: Some(live_input.to_string()),
        recovery_revision: capture.commit.to_string(),
        resolved_tree: proposal.proposal_tree.clone(),
        resolution_commit: commit.to_string(),
        published: false,
        applied: false,
    })
}

fn resume_or_prepare_proposal(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
    proposal: &ResolutionProposal,
    capture: &vulcan_sync::GitCapture,
    device_id: &str,
    existing: Option<SyncConflictResolutionRecord>,
) -> Result<SyncConflictResolutionRecord, AppError> {
    let Some(mut resolution) = existing else {
        return prepare_proposal_resolution(
            engine, repository, record, proposal, capture, device_id,
        );
    };
    let local_tree = conflict_worktree_tree(engine, repository, record)?;
    let proposal_tree = GitOid::parse(&proposal.proposal_tree).map_err(AppError::operation)?;
    let resolution_tree = engine
        .tree_oid(
            repository,
            &GitOid::parse(&resolution.resolution_commit).map_err(AppError::operation)?,
        )
        .map_err(AppError::operation)?;
    if capture.tree != local_tree
        && capture.tree != proposal_tree
        && capture.tree != resolution_tree
    {
        return Err(AppError::operation(
            "the worktree changed while proposal approval was pending; its recovery snapshot was retained",
        ));
    }
    resolution.recovery_revision = capture.commit.to_string();
    Ok(resolution)
}

fn publish_proposal_resolution(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    options: &ApproveResolutionProposalOptions,
    resolution: &mut SyncConflictResolutionRecord,
) -> Result<(), AppError> {
    let commit = GitOid::parse(&resolution.resolution_commit).map_err(AppError::operation)?;
    let remote_before = GitOid::parse(
        resolution
            .live_input_revision
            .as_deref()
            .unwrap_or(&resolution.remote_revision),
    )
    .map_err(AppError::operation)?;
    match engine
        .remote_ref(repository, &options.remote, &options.live_ref)
        .map_err(AppError::operation)?
        .as_ref()
    {
        Some(current) if current == &commit => {}
        Some(current) if current == &remote_before => {
            if engine
                .push_ref(
                    repository,
                    &options.remote,
                    &commit,
                    &options.live_ref,
                    Some(&remote_before),
                )
                .map_err(AppError::operation)?
                == GitPushResult::Rejected
            {
                return Err(AppError::operation(
                    "the remote live ref changed while publishing the approved proposal",
                ));
            }
        }
        _ => {
            return Err(AppError::operation(
                "the remote live ref no longer matches the proposal inputs",
            ));
        }
    }
    resolution.published = true;
    Ok(())
}

fn update_sync_refs(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    options: &ApproveResolutionProposalOptions,
    resolution: &SyncConflictResolutionRecord,
) -> Result<(), AppError> {
    let refs = GitSyncRefs::for_options(&GitSyncOptions {
        remote: options.remote.clone(),
        live_ref: options.live_ref.clone(),
        ..GitSyncOptions::default()
    })
    .map_err(AppError::operation)?;
    let commit = GitOid::parse(&resolution.resolution_commit).map_err(AppError::operation)?;
    engine
        .update_refs(
            repository,
            &[
                (&refs.local, &commit),
                (&refs.fetched, &commit),
                (&refs.pending, &commit),
            ],
        )
        .map_err(AppError::operation)?;
    Ok(())
}

fn save_approval_audit(
    store: &SyncStateStore,
    proposal: &ResolutionProposal,
    resolution: &SyncConflictResolutionRecord,
    options: &ApproveResolutionProposalOptions,
) -> Result<(), AppError> {
    let automatic = options.automatic;
    let action = if automatic {
        ResolutionProposalAuditAction::AutoAccepted
    } else {
        ResolutionProposalAuditAction::Approved
    };
    let event_id = blake3::hash(
        format!(
            "{}\0{}\0{}\0{}",
            if automatic {
                "auto_accepted"
            } else {
                "approved"
            },
            proposal.conflict_id,
            proposal.proposal_id,
            resolution.resolution_commit
        )
        .as_bytes(),
    )
    .to_hex()[..32]
        .to_string();
    let record = ResolutionProposalAuditRecord {
        version: RESOLUTION_PROPOSAL_AUDIT_VERSION,
        event_id,
        repository_key: proposal.repository_key.clone(),
        conflict_id: proposal.conflict_id.clone(),
        proposal_id: proposal.proposal_id.clone(),
        action,
        provider: proposal.provider.clone(),
        model: proposal.model.clone(),
        prompt_contract_version: proposal.prompt_contract_version,
        tool_contract_version: proposal.tool_contract_version,
        proposal_tree: proposal.proposal_tree.clone(),
        resolution_commit: Some(resolution.resolution_commit.clone()),
        validation: proposal.validation.clone(),
    };
    save_proposal_audit(store, &record)
}

fn proposal_rejection_record(proposal: &ResolutionProposal) -> ResolutionProposalAuditRecord {
    let event_id = blake3::hash(
        format!(
            "rejected\0{}\0{}",
            proposal.conflict_id, proposal.proposal_id
        )
        .as_bytes(),
    )
    .to_hex()[..32]
        .to_string();
    ResolutionProposalAuditRecord {
        version: RESOLUTION_PROPOSAL_AUDIT_VERSION,
        event_id,
        repository_key: proposal.repository_key.clone(),
        conflict_id: proposal.conflict_id.clone(),
        proposal_id: proposal.proposal_id.clone(),
        action: ResolutionProposalAuditAction::Rejected,
        provider: proposal.provider.clone(),
        model: proposal.model.clone(),
        prompt_contract_version: proposal.prompt_contract_version,
        tool_contract_version: proposal.tool_contract_version,
        proposal_tree: proposal.proposal_tree.clone(),
        resolution_commit: None,
        validation: proposal.validation.clone(),
    }
}

fn proposal_audit_path(store: &SyncStateStore, record: &ResolutionProposalAuditRecord) -> PathBuf {
    store
        .root()
        .join(&record.repository_key)
        .join("conflicts")
        .join(&record.conflict_id)
        .join("audit")
        .join(format!("{}.json", record.event_id))
}

fn load_proposal_audit(
    store: &SyncStateStore,
    expected: &ResolutionProposalAuditRecord,
) -> Result<Option<ResolutionProposalAuditRecord>, AppError> {
    let path = proposal_audit_path(store, expected);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::operation(error)),
    };
    if bytes.len() > MAX_PROPOSAL_RECORD_BYTES {
        return Err(AppError::operation(
            "resolution proposal audit record exceeds its byte limit",
        ));
    }
    let record: ResolutionProposalAuditRecord =
        serde_json::from_slice(&bytes).map_err(AppError::operation)?;
    if &record != expected {
        return Err(AppError::operation(
            "resolution proposal audit record identity mismatch",
        ));
    }
    Ok(Some(record))
}

fn save_proposal_audit(
    store: &SyncStateStore,
    record: &ResolutionProposalAuditRecord,
) -> Result<(), AppError> {
    let directory = store
        .root()
        .join(&record.repository_key)
        .join("conflicts")
        .join(&record.conflict_id)
        .join("audit");
    fs::create_dir_all(&directory).map_err(AppError::operation)?;
    let path = directory.join(format!("{}.json", record.event_id));
    write_json_noclobber(&directory, &path, record)
}

fn ensure_proposal_not_rejected(
    store: &SyncStateStore,
    proposal: &ResolutionProposal,
) -> Result<(), AppError> {
    if load_proposal_audit(store, &proposal_rejection_record(proposal))?.is_some() {
        Err(AppError::operation(format!(
            "resolution proposal `{}` was explicitly rejected",
            proposal.proposal_id
        )))
    } else {
        Ok(())
    }
}

fn ensure_proposal_has_no_resolution(
    store: &SyncConflictStore,
    repository_key: &str,
    proposal: &ResolutionProposal,
) -> Result<(), AppError> {
    if store
        .get_resolution(repository_key, &proposal.conflict_id)?
        .is_some_and(|resolution| !resolution.is_abandoned())
    {
        Err(AppError::operation(
            "the conflict already has a resolution in progress or applied",
        ))
    } else {
        Ok(())
    }
}

fn rejection_report(
    vault: &Path,
    proposal: &ResolutionProposal,
    dry_run: bool,
    outcome: RejectResolutionProposalOutcome,
    event_id: &str,
) -> RejectResolutionProposalReport {
    RejectResolutionProposalReport {
        vault: vault.to_path_buf(),
        repository_key: proposal.repository_key.clone(),
        conflict_id: proposal.conflict_id.clone(),
        proposal_id: proposal.proposal_id.clone(),
        dry_run,
        outcome,
        event_id: event_id.to_string(),
    }
}

fn proposal_report(
    vault: &Path,
    proposal: &ResolutionProposal,
    options: &ApproveResolutionProposalOptions,
    outcome: ApproveResolutionProposalOutcome,
    resolution: Option<&SyncConflictResolutionRecord>,
    cache_refresh: Option<ScanSummary>,
) -> ApproveResolutionProposalReport {
    ApproveResolutionProposalReport {
        vault: vault.to_path_buf(),
        repository_key: proposal.repository_key.clone(),
        conflict_id: proposal.conflict_id.clone(),
        proposal_id: proposal.proposal_id.clone(),
        dry_run: options.dry_run,
        outcome,
        proposal_tree: proposal.proposal_tree.clone(),
        recovery_revision: resolution.map(|value| value.recovery_revision.clone()),
        resolution_commit: resolution.map(|value| value.resolution_commit.clone()),
        cache_refresh,
    }
}

fn verify_approval_preconditions(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
    proposal: &ResolutionProposal,
    options: &ApproveResolutionProposalOptions,
    existing: Option<&SyncConflictResolutionRecord>,
) -> Result<(), AppError> {
    let safety = engine
        .safety_state(repository)
        .map_err(AppError::operation)?;
    if safety.staged_changes || safety.operation.is_some() {
        return Err(AppError::operation(
            "proposal approval requires a clean normal index and no Git operation in progress",
        ));
    }
    let local = conflict_worktree_revision(record)?;
    let current_tree = engine
        .snapshot_worktree_tree(repository, Some(&local))
        .map_err(AppError::operation)?;
    let expected_tree = conflict_worktree_tree(engine, repository, record)?;
    let proposal_tree = GitOid::parse(&proposal.proposal_tree).map_err(AppError::operation)?;
    if current_tree != expected_tree && current_tree != proposal_tree {
        return Err(AppError::operation(
            "the worktree no longer matches the preserved local input or approved proposal",
        ));
    }
    let remote = engine
        .remote_ref(repository, &options.remote, &options.live_ref)
        .map_err(AppError::operation)?;
    let expected_resolution = existing.map(|resolution| resolution.resolution_commit.as_str());
    if remote.as_ref().map(GitOid::as_str) != Some(conflict_live_input(record)?)
        && remote.as_ref().map(GitOid::as_str) != expected_resolution
    {
        return Err(AppError::operation(
            "the remote live ref moved after the proposal inputs were preserved",
        ));
    }
    Ok(())
}

fn assemble_proposal(
    record: &SyncConflictRecord,
    repository_key: String,
    identity: ResolutionAgentIdentity,
    options: &ResolutionProposalOptions,
    focused_context: &[ResolutionAgentContextFile],
    prepared: PreparedOutput,
    tree: ProposalTree,
) -> Result<ResolutionProposal, AppError> {
    let proposal_context = focused_context
        .iter()
        .map(|context| ResolutionProposalContext {
            path: context.path.clone(),
            content_hash: context.content_hash.clone(),
            bytes: context.content.len() as u64,
        })
        .collect::<Vec<_>>();
    let proposal_id = proposal_id(
        &record.id,
        &identity,
        &proposal_context,
        &prepared.tool_calls,
        &prepared.paths,
        &tree.oid,
    )?;
    Ok(ResolutionProposal {
        version: RESOLUTION_PROPOSAL_VERSION,
        proposal_id,
        status: ResolutionProposalStatus::Ready,
        conflict_id: record.id.clone(),
        repository_key,
        base_revision: record
            .base_revision
            .clone()
            .expect("proposal creation validated the merge base"),
        local_revision: record.local_revision.clone(),
        remote_revision: record.remote_revision.clone(),
        policy_version: record.policy_version,
        policy_hash: record.policy_hash.clone(),
        provider: identity.provider,
        model: identity.model,
        prompt_contract_version: identity.prompt_contract_version,
        tool_contract_version: RESOLUTION_AGENT_TOOL_CONTRACT_VERSION,
        permission_profile: options.permission_profile.clone(),
        broad_context_allowed: options.allow_broad_context,
        focused_context: proposal_context,
        tool_calls: prepared.tool_calls,
        explanation: prepared.explanation,
        referenced_context: prepared.referenced_context,
        proposal_tree: tree.oid.to_string(),
        patch: tree.patch,
        paths: prepared.paths,
        validation: vec![
            ResolutionProposalValidationCheck::ConflictInputsPreserved,
            ResolutionProposalValidationCheck::PermissionProfileNamed,
            ResolutionProposalValidationCheck::FocusedContextBounded,
            ResolutionProposalValidationCheck::FocusedToolsBounded,
            ResolutionProposalValidationCheck::OutputPathsExact,
            ResolutionProposalValidationCheck::OutputBytesBounded,
            ResolutionProposalValidationCheck::NoFileDeletion,
            ResolutionProposalValidationCheck::ExactTreeObjects,
            ResolutionProposalValidationCheck::WorktreeUnchanged,
            ResolutionProposalValidationCheck::RefsUnchanged,
            ResolutionProposalValidationCheck::WholeTreeLinksValid,
            ResolutionProposalValidationCheck::MassDeletionPolicy,
        ],
    })
}

struct ProposalTree {
    oid: GitOid,
    patch: String,
}

struct PreparedOutput {
    explanation: String,
    referenced_context: Vec<String>,
    git_paths: Vec<GitResolvedPath>,
    paths: Vec<ResolutionProposalPath>,
    tool_calls: Vec<ResolutionProposalToolCall>,
}

fn build_agent_request(
    paths: &VaultPaths,
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
    options: &ResolutionProposalOptions,
) -> Result<ResolutionAgentRequest, AppError> {
    let base = record
        .base_revision
        .as_deref()
        .ok_or_else(|| AppError::operation("agent resolution requires one merge base"))?;
    let mut total = 0_usize;
    let mut files = Vec::with_capacity(record.paths.len());
    for path in &record.paths {
        let mut side = |revision: Option<&str>| -> Result<ResolutionAgentSide, AppError> {
            let Some(revision) = revision else {
                return Ok(ResolutionAgentSide {
                    revision: None,
                    mode: None,
                    content: None,
                });
            };
            let revision_oid = GitOid::parse(revision).map_err(AppError::operation)?;
            let object = engine
                .path_object(repository, &revision_oid, &path.path)
                .map_err(AppError::operation)?;
            let content = object.as_ref().and_then(|object| object.data.clone());
            if content
                .as_ref()
                .is_some_and(|data| data.len() > MAX_AGENT_FILE_BYTES)
            {
                return Err(AppError::operation(format!(
                    "conflict input `{}` exceeds the per-file agent limit",
                    path.path
                )));
            }
            total = total.saturating_add(content.as_ref().map_or(0, Vec::len));
            Ok(ResolutionAgentSide {
                revision: Some(revision.to_string()),
                mode: object.as_ref().map(|object| object.mode.clone()),
                content,
            })
        };
        files.push(ResolutionAgentFile {
            path: path.path.clone(),
            base: side(Some(base))?,
            local: side(Some(&record.local_revision))?,
            remote: side(Some(&record.remote_revision))?,
        });
    }
    if total > MAX_AGENT_TOTAL_BYTES {
        return Err(AppError::operation(
            "conflict inputs exceed the total agent byte limit",
        ));
    }
    let mut context_paths = options.focused_context.clone();
    context_paths.sort();
    let mut focused_context = Vec::with_capacity(context_paths.len());
    let mut seen_context = BTreeSet::new();
    for path in &context_paths {
        if !seen_context.insert(path.as_str()) {
            return Err(AppError::operation(format!(
                "focused context path `{path}` was supplied more than once"
            )));
        }
        let bytes = secure_read(paths.vault_root(), Path::new(path)).map_err(|error| {
            AppError::operation(format!("cannot read focused context `{path}`: {error}"))
        })?;
        if bytes.len() > MAX_AGENT_CONTEXT_FILE_BYTES {
            return Err(AppError::operation(format!(
                "focused context `{path}` exceeds the per-file agent limit"
            )));
        }
        total = total.saturating_add(bytes.len());
        let content = String::from_utf8(bytes).map_err(|_| {
            AppError::operation(format!("focused context `{path}` must be valid UTF-8"))
        })?;
        focused_context.push(ResolutionAgentContextFile {
            path: path.clone(),
            content_hash: blake3::hash(content.as_bytes()).to_hex().to_string(),
            content,
        });
    }
    if total > MAX_AGENT_TOTAL_BYTES {
        return Err(AppError::operation(
            "conflict inputs and focused context exceed the total agent byte limit",
        ));
    }
    Ok(ResolutionAgentRequest {
        conflict_id: record.id.clone(),
        policy_version: record.policy_version,
        policy_hash: record.policy_hash.clone(),
        files,
        focused_context,
        broad_context_allowed: options.allow_broad_context,
        tool_contract_version: RESOLUTION_AGENT_TOOL_CONTRACT_VERSION,
    })
}

fn prepare_output(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
    supplied_context: &BTreeSet<String>,
    output: ResolutionAgentOutput,
    tool_calls: Vec<ResolutionProposalToolCall>,
) -> Result<PreparedOutput, AppError> {
    validate_text("proposal explanation", &output.explanation)?;
    if output.referenced_context.len() > MAX_CONTEXT_PATHS {
        return Err(AppError::operation(
            "proposal referenced too many context paths",
        ));
    }
    if output
        .referenced_context
        .iter()
        .any(|path| !valid_relative_path(path))
    {
        return Err(AppError::operation(
            "proposal referenced an invalid context path",
        ));
    }
    validate_referenced_context(supplied_context, &output.referenced_context)?;
    let expected = record
        .paths
        .iter()
        .map(|path| path.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut supplied = BTreeMap::new();
    let mut total = 0_usize;
    for path in output.paths {
        if !expected.contains(path.path.as_str()) || supplied.contains_key(&path.path) {
            return Err(AppError::operation(format!(
                "agent output path `{}` is duplicate or outside the conflict",
                path.path
            )));
        }
        if path.content.len() > MAX_AGENT_FILE_BYTES {
            return Err(AppError::operation(format!(
                "agent output `{}` exceeds the per-file limit",
                path.path
            )));
        }
        total = total.saturating_add(path.content.len());
        supplied.insert(path.path, path.content);
    }
    if supplied.len() != expected.len() || total > MAX_AGENT_TOTAL_BYTES {
        return Err(AppError::operation(
            "agent output must resolve every conflict path within the total byte limit",
        ));
    }
    let mut git_paths = Vec::with_capacity(record.paths.len());
    let mut paths = Vec::with_capacity(record.paths.len());
    for conflict_path in &record.paths {
        let content = supplied
            .remove(&conflict_path.path)
            .expect("validated exact path set");
        validate_proposal_content(record, &conflict_path.path, &content)?;
        let mode = resolved_mode(conflict_path)?;
        let resolved = GitResolvedPath {
            path: conflict_path.path.clone(),
            mode: Some(mode.clone()),
            data: Some(content.clone()),
        };
        // Exercise the engine's path and blob validation before tree construction.
        engine
            .path_object(
                repository,
                &GitOid::parse(&record.local_revision).map_err(AppError::operation)?,
                &conflict_path.path,
            )
            .map_err(AppError::operation)?;
        paths.push(ResolutionProposalPath {
            path: conflict_path.path.clone(),
            mode,
            content_hash: blake3::hash(&content).to_hex().to_string(),
            bytes: content.len() as u64,
        });
        git_paths.push(resolved);
    }
    Ok(PreparedOutput {
        explanation: output.explanation,
        referenced_context: output.referenced_context,
        git_paths,
        paths,
        tool_calls,
    })
}

fn validate_referenced_context(
    supplied_context: &BTreeSet<String>,
    referenced_context: &[String],
) -> Result<(), AppError> {
    if referenced_context.iter().collect::<BTreeSet<_>>().len() != referenced_context.len() {
        return Err(AppError::operation(
            "proposal referenced the same context path more than once",
        ));
    }
    if referenced_context
        .iter()
        .any(|path| !supplied_context.contains(path))
    {
        return Err(AppError::operation(
            "proposal referenced context that was not supplied to the provider",
        ));
    }
    Ok(())
}

fn resolved_mode(path: &crate::sync_conflicts::SyncConflictPathRecord) -> Result<String, AppError> {
    let base = path.base.mode.as_deref();
    let local = path.local.mode.as_deref();
    let remote = path.remote.mode.as_deref();
    if local == remote {
        local
    } else if local == base {
        remote
    } else if remote == base {
        local
    } else {
        None
    }
    .filter(|mode| *mode == "100644" || *mode == "100755")
    .map(str::to_string)
    .ok_or_else(|| {
        AppError::operation(format!(
            "conflict path `{}` has an ambiguous mode",
            path.path
        ))
    })
}

fn verify_tree_objects(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    tree: &GitOid,
    paths: &[GitResolvedPath],
) -> Result<(), AppError> {
    for path in paths {
        let actual = engine
            .path_object(repository, tree, &path.path)
            .map_err(AppError::operation)?
            .ok_or_else(|| AppError::operation(format!("proposal tree omitted `{}`", path.path)))?;
        if actual.kind != "blob"
            || actual.mode != path.mode.as_deref().unwrap_or_default()
            || actual.data.as_ref() != path.data.as_ref()
        {
            return Err(AppError::operation(format!(
                "proposal tree object for `{}` differs from provider output",
                path.path
            )));
        }
    }
    Ok(())
}

fn preserved_ref_snapshot(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
) -> Result<Vec<(String, Option<String>)>, AppError> {
    [
        record.preserved_base_ref.as_deref(),
        Some(record.preserved_local_ref.as_str()),
        Some(record.preserved_remote_ref.as_str()),
        record.preserved_record_ref.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(|reference| {
        let parsed = vulcan_sync::GitRefName::parse(reference).map_err(AppError::operation)?;
        Ok((
            reference.to_string(),
            engine
                .read_ref(repository, &parsed)
                .map_err(AppError::operation)?
                .map(|oid| oid.to_string()),
        ))
    })
    .collect()
}

fn verify_no_external_mutation(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
    expected_tree: &GitOid,
    refs_before: &[(String, Option<String>)],
) -> Result<(), AppError> {
    let local_revision = conflict_worktree_revision(record)?;
    let current = engine
        .snapshot_worktree_tree(repository, Some(&local_revision))
        .map_err(AppError::operation)?;
    if &current != expected_tree {
        return Err(AppError::operation(
            "worktree changed while the resolution proposal was generated",
        ));
    }
    if preserved_ref_snapshot(engine, repository, record)? != refs_before {
        return Err(AppError::operation(
            "preserved conflict refs changed while the proposal was generated",
        ));
    }
    Ok(())
}

fn proposal_id(
    conflict_id: &str,
    identity: &ResolutionAgentIdentity,
    context: &[ResolutionProposalContext],
    tool_calls: &[ResolutionProposalToolCall],
    paths: &[ResolutionProposalPath],
    tree: &GitOid,
) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(&(
        conflict_id,
        identity,
        context,
        tool_calls,
        paths,
        tree.as_str(),
    ))
    .map_err(AppError::operation)?;
    Ok(blake3::hash(&bytes).to_hex()[..32].to_string())
}

fn recompute_current_proposal_id(proposal: &ResolutionProposal) -> Result<String, AppError> {
    proposal_id(
        &proposal.conflict_id,
        &ResolutionAgentIdentity {
            provider: proposal.provider.clone(),
            model: proposal.model.clone(),
            prompt_contract_version: proposal.prompt_contract_version,
        },
        &proposal.focused_context,
        &proposal.tool_calls,
        &proposal.paths,
        &GitOid::parse(&proposal.proposal_tree).map_err(AppError::operation)?,
    )
}

fn save_proposal(store: &SyncStateStore, proposal: &ResolutionProposal) -> Result<(), AppError> {
    let directory = store
        .root()
        .join(&proposal.repository_key)
        .join("conflicts")
        .join(&proposal.conflict_id)
        .join("proposals");
    fs::create_dir_all(&directory).map_err(AppError::operation)?;
    let path = directory.join(format!("{}.json", proposal.proposal_id));
    let bytes = serde_json::to_vec_pretty(proposal).map_err(AppError::operation)?;
    if bytes.len() > MAX_PROPOSAL_RECORD_BYTES {
        return Err(AppError::operation(
            "resolution proposal record exceeds its byte limit",
        ));
    }
    write_bytes_noclobber(&directory, &path, &bytes)
}

fn write_json_noclobber(
    directory: &Path,
    path: &Path,
    value: &impl Serialize,
) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(AppError::operation)?;
    if bytes.len() > MAX_PROPOSAL_RECORD_BYTES {
        return Err(AppError::operation(
            "resolution proposal state exceeds its byte limit",
        ));
    }
    write_bytes_noclobber(directory, path, &bytes)
}

fn write_bytes_noclobber(directory: &Path, path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let mut temporary = NamedTempFile::new_in(directory).map_err(AppError::operation)?;
    temporary.write_all(bytes).map_err(AppError::operation)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AppError::operation)?;
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read(path).map_err(AppError::operation)?;
            if existing == bytes {
                Ok(())
            } else {
                Err(AppError::operation("resolution proposal ID collision"))
            }
        }
        Err(error) => Err(AppError::operation(error.error)),
    }
}

fn ensure_no_existing_proposal(
    store: &SyncStateStore,
    repository_key: &str,
    conflict_id: &str,
) -> Result<(), AppError> {
    let directory = store
        .root()
        .join(repository_key)
        .join("conflicts")
        .join(conflict_id)
        .join("proposals");
    match fs::read_dir(directory) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                Err(AppError::operation(format!(
                    "conflict `{conflict_id}` already has a retained resolution proposal"
                )))
            } else {
                Ok(())
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::operation(error)),
    }
}

fn proposal_path(
    store: &SyncStateStore,
    repository_key: &str,
    conflict_id: &str,
    proposal_id: &str,
) -> PathBuf {
    store
        .root()
        .join(repository_key)
        .join("conflicts")
        .join(conflict_id)
        .join("proposals")
        .join(format!("{proposal_id}.json"))
}

fn validate_hex_id(label: &str, value: &str) -> Result<(), AppError> {
    if value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(AppError::operation(format!("invalid {label} `{value}`")))
    }
}

fn validate_options(options: &ResolutionProposalOptions) -> Result<(), AppError> {
    validate_text("permission profile", &options.permission_profile)?;
    if options.focused_context.len() > MAX_CONTEXT_PATHS
        || options
            .focused_context
            .iter()
            .any(|path| !valid_relative_path(path) || is_internal_context_path(path))
    {
        return Err(AppError::operation(
            "focused context paths are invalid or unbounded",
        ));
    }
    Ok(())
}

fn is_internal_context_path(path: &str) -> bool {
    path == ".obsidian"
        || path.starts_with(".obsidian/")
        || path == ".vulcan"
        || path.starts_with(".vulcan/")
}

fn validate_agent_conflict_scope(record: &SyncConflictRecord) -> Result<(), AppError> {
    for path in &record.paths {
        let internal = path.path == ".obsidian"
            || path.path.starts_with(".obsidian/")
            || path.path == ".vulcan"
            || path.path.starts_with(".vulcan/");
        let unsupported = path.classification.as_ref().is_none_or(|classification| {
            matches!(
                classification.file_kind,
                vulcan_sync::MergeFileKind::Binary
                    | vulcan_sync::MergeFileKind::ObsidianState
                    | vulcan_sync::MergeFileKind::Missing
            )
        });
        if internal || unsupported {
            return Err(AppError::operation(format!(
                "conflict path `{}` is not eligible for agent input",
                path.path
            )));
        }
    }
    Ok(())
}

fn valid_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.bytes().any(|byte| byte == 0)
        && Path::new(value)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn validate_identity(identity: &ResolutionAgentIdentity) -> Result<(), AppError> {
    validate_text("provider", &identity.provider)?;
    validate_text("model", &identity.model)?;
    if identity.prompt_contract_version == 0 {
        return Err(AppError::operation(
            "prompt contract version must be positive",
        ));
    }
    Ok(())
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

fn cancellation_check(cancellation: &SyncCancellationToken) -> Result<(), AppError> {
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
    use crate::sync::sync_git_vault_with_state_store;
    use std::process::Command;
    use tempfile::{tempdir, TempDir};
    use vulcan_core::{paths::initialize_vulcan_dir, scan_vault, ScanMode};
    use vulcan_sync::{GitCliEngine, GitSyncOptions};

    struct FakeProvider {
        cancel: bool,
    }

    struct NoopTools;

    #[cfg(feature = "web")]
    #[derive(Default)]
    struct RecordingTools {
        calls: Vec<(String, String)>,
    }

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

    struct AmbiguousLinkProvider;

    struct InventedContextProvider;

    struct ToolUsingProvider;

    #[test]
    fn editor_conflict_markers_are_unique_and_preserve_all_sides() {
        let rendered = String::from_utf8(render_editor_conflict(
            "VULCAN-CONFLICT-deadbeef",
            "base",
            "local\n",
            "remote",
        ))
        .expect("UTF-8 markers");
        assert!(rendered.contains("<<<<<<< VULCAN-CONFLICT-deadbeef LOCAL\nlocal\n"));
        assert!(rendered.contains("||||||| VULCAN-CONFLICT-deadbeef BASE\nbase\n"));
        assert!(rendered.contains("======= VULCAN-CONFLICT-deadbeef\nremote\n"));
        assert!(rendered.ends_with(">>>>>>> VULCAN-CONFLICT-deadbeef REMOTE\n"));
    }

    impl ResolutionAgentProvider for ToolUsingProvider {
        fn identity(&self) -> ResolutionAgentIdentity {
            ResolutionAgentIdentity {
                provider: "fake".to_string(),
                model: "tool-using-v1".to_string(),
                prompt_contract_version: 1,
            }
        }

        fn propose(
            &self,
            _request: &ResolutionAgentRequest,
            tools: &mut dyn ResolutionAgentTools,
            _cancellation: &SyncCancellationToken,
        ) -> Result<ResolutionAgentOutput, AppError> {
            let result = tools.call("vault_search", r#"{"query":"context marker"}"#)?;
            assert!(result.contains("Context.md"));
            Ok(ResolutionAgentOutput {
                explanation: "Use the indexed context.".to_string(),
                referenced_context: vec!["Context.md".to_string()],
                paths: vec![ResolutionAgentPathOutput {
                    path: "Home.md".to_string(),
                    content: b"agent resolution\n".to_vec(),
                }],
            })
        }
    }

    impl ResolutionAgentProvider for InventedContextProvider {
        fn identity(&self) -> ResolutionAgentIdentity {
            ResolutionAgentIdentity {
                provider: "fake".to_string(),
                model: "invented-context-v1".to_string(),
                prompt_contract_version: 1,
            }
        }

        fn propose(
            &self,
            _request: &ResolutionAgentRequest,
            _tools: &mut dyn ResolutionAgentTools,
            _cancellation: &SyncCancellationToken,
        ) -> Result<ResolutionAgentOutput, AppError> {
            Ok(ResolutionAgentOutput {
                explanation: "Claim context that was never supplied.".to_string(),
                referenced_context: vec!["Secret.md".to_string()],
                paths: vec![ResolutionAgentPathOutput {
                    path: "Home.md".to_string(),
                    content: b"agent resolution\n".to_vec(),
                }],
            })
        }
    }

    impl ResolutionAgentProvider for AmbiguousLinkProvider {
        fn identity(&self) -> ResolutionAgentIdentity {
            ResolutionAgentIdentity {
                provider: "fake".to_string(),
                model: "ambiguous-link-v1".to_string(),
                prompt_contract_version: 1,
            }
        }

        fn propose(
            &self,
            request: &ResolutionAgentRequest,
            _tools: &mut dyn ResolutionAgentTools,
            _cancellation: &SyncCancellationToken,
        ) -> Result<ResolutionAgentOutput, AppError> {
            Ok(ResolutionAgentOutput {
                explanation: "Link the merged note to Target.".to_string(),
                referenced_context: Vec::new(),
                paths: vec![ResolutionAgentPathOutput {
                    path: request.files[0].path.clone(),
                    content: b"[[Target]]\n".to_vec(),
                }],
            })
        }
    }

    impl ResolutionAgentProvider for FakeProvider {
        fn identity(&self) -> ResolutionAgentIdentity {
            ResolutionAgentIdentity {
                provider: "fake".to_string(),
                model: "fixture-v1".to_string(),
                prompt_contract_version: 1,
            }
        }

        fn propose(
            &self,
            request: &ResolutionAgentRequest,
            _tools: &mut dyn ResolutionAgentTools,
            cancellation: &SyncCancellationToken,
        ) -> Result<ResolutionAgentOutput, AppError> {
            assert_eq!(request.files.len(), 1);
            assert_eq!(request.files[0].path, "Home.md");
            assert_eq!(
                request.files[0].base.content.as_deref(),
                Some(b"base\n".as_slice())
            );
            if let Some(context) = request.focused_context.first() {
                assert_eq!(context.path, "Home.md");
                assert_eq!(context.content, "writer\n");
                assert_eq!(
                    context.content_hash,
                    blake3::hash(b"writer\n").to_hex().to_string()
                );
            }
            if self.cancel {
                cancellation.cancel();
            }
            Ok(ResolutionAgentOutput {
                explanation: "Combine the two intended edits.".to_string(),
                referenced_context: request
                    .focused_context
                    .iter()
                    .map(|context| context.path.clone())
                    .collect(),
                paths: vec![ResolutionAgentPathOutput {
                    path: "Home.md".to_string(),
                    content: b"agent resolution\n".to_vec(),
                }],
            })
        }
    }

    struct ConflictFixture {
        _temporary: TempDir,
        store: SyncStateStore,
        reader: PathBuf,
        record: SyncConflictRecord,
    }

    fn conflict_fixture() -> ConflictFixture {
        conflict_fixture_with_split_targets(false)
    }

    fn conflict_fixture_with_split_targets(split_targets: bool) -> ConflictFixture {
        let temporary = tempdir().expect("temporary directory");
        let remote = temporary.path().join("remote.git");
        git(
            temporary.path(),
            &["init", "--quiet", "--bare", path(&remote)],
        );
        let writer = temporary.path().join("writer");
        fs::create_dir(&writer).expect("writer directory");
        git(
            &writer,
            &["-c", "init.defaultBranch=main", "init", "--quiet"],
        );
        configure_git(&writer);
        git(&writer, &["remote", "add", "origin", path(&remote)]);
        fs::write(writer.join("Home.md"), "base\n").expect("base note");
        commit_all(&writer, "base");
        let store = SyncStateStore::at(temporary.path().join("state"));
        sync_git_vault_with_state_store(
            &VaultPaths::new(&writer),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("bootstrap sync");
        let reader = temporary.path().join("reader");
        git(
            temporary.path(),
            &[
                "-c",
                "core.autocrlf=false",
                "clone",
                "--quiet",
                path(&writer),
                path(&reader),
            ],
        );
        configure_git(&reader);
        git(&reader, &["remote", "set-url", "origin", path(&remote)]);
        sync_git_vault_with_state_store(
            &VaultPaths::new(&reader),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("reader baseline");
        fs::write(writer.join("Home.md"), "writer\n").expect("writer edit");
        fs::write(reader.join("Home.md"), "reader\n").expect("reader edit");
        if split_targets {
            fs::create_dir(writer.join("Writer")).expect("writer folder");
            fs::write(writer.join("Writer/Target.md"), "writer target\n").expect("writer target");
            fs::create_dir(reader.join("Reader")).expect("reader folder");
            fs::write(reader.join("Reader/Target.md"), "reader target\n").expect("reader target");
        }
        sync_git_vault_with_state_store(
            &VaultPaths::new(&writer),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("writer sync");
        let report = sync_git_vault_with_state_store(
            &VaultPaths::new(&reader),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("conflicted sync");
        ConflictFixture {
            _temporary: temporary,
            store,
            reader,
            record: report.conflict_record.expect("conflict record"),
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn provider_proposal_is_bounded_persisted_and_does_not_mutate_refs_or_worktree() {
        let fixture = conflict_fixture();
        let refs_before = git_stdout(
            &fixture.reader,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/vulcan",
            ],
        );
        let cancellation = SyncCancellationToken::default();
        let proposal = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: vec!["Home.md".to_string()],
                allow_broad_context: false,
            },
            &FakeProvider { cancel: false },
            &cancellation,
            &fixture.store,
        )
        .expect("proposal");
        assert_eq!(proposal.status, ResolutionProposalStatus::Ready);
        assert_eq!(proposal.provider, "fake");
        assert_eq!(
            proposal.paths[0].content_hash,
            blake3::hash(b"agent resolution\n").to_hex().to_string()
        );
        assert!(proposal.patch.contains("agent resolution"));
        assert_eq!(proposal.version, RESOLUTION_PROPOSAL_VERSION);
        assert_eq!(proposal.tool_contract_version, 3);
        assert_eq!(proposal.focused_context.len(), 1);
        assert_eq!(proposal.focused_context[0].path, "Home.md");
        assert_eq!(
            proposal.focused_context[0].content_hash,
            blake3::hash(b"writer\n").to_hex().to_string()
        );
        assert!(proposal
            .validation
            .contains(&ResolutionProposalValidationCheck::WholeTreeLinksValid));
        assert!(proposal
            .validation
            .contains(&ResolutionProposalValidationCheck::MassDeletionPolicy));
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Home.md")).expect("note"),
            "writer\n"
        );
        assert_eq!(
            git_stdout(
                &fixture.reader,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/vulcan"
                ],
            ),
            refs_before
        );
        let proposal_path = fixture
            .store
            .root()
            .join(&fixture.record.repository_key)
            .join("conflicts")
            .join(&fixture.record.id)
            .join("proposals")
            .join(format!("{}.json", proposal.proposal_id));
        assert!(proposal_path.is_file());
        assert_eq!(
            load_resolution_proposal(
                &fixture.store,
                &fixture.record.repository_key,
                &fixture.record.id,
                &proposal.proposal_id,
            )
            .expect("stored proposal"),
            proposal
        );
        let duplicate = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
            },
            &FakeProvider { cancel: false },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect_err("only one retained proposal job is allowed");
        assert!(duplicate.to_string().contains("already has"));
        let repository = GitCliEngine::default()
            .discover_repository(&fixture.reader)
            .expect("repository");
        let object = GitCliEngine::default()
            .path_object(
                &repository,
                &GitOid::parse(&proposal.proposal_tree).expect("proposal tree"),
                "Home.md",
            )
            .expect("tree lookup")
            .expect("proposal object");
        assert_eq!(
            object.data.as_deref(),
            Some(b"agent resolution\n".as_slice())
        );

        assert_approval_lifecycle(&fixture, &proposal, &refs_before);
    }

    #[test]
    fn proposal_generation_rejects_new_whole_tree_link_ambiguity() {
        let fixture = conflict_fixture_with_split_targets(true);
        let error = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
            },
            &AmbiguousLinkProvider,
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect_err("new ambiguity must reject the proposal");

        assert!(error
            .to_string()
            .contains("new ambiguous wikilink link-resolution problem"));
        assert!(load_resolution_proposal(
            &fixture.store,
            &fixture.record.repository_key,
            &fixture.record.id,
            "missing"
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Home.md")).expect("local note"),
            "writer\n"
        );
    }

    #[test]
    fn proposal_rejects_provider_references_to_unsupplied_context() {
        let fixture = conflict_fixture();
        let error = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
            },
            &InventedContextProvider,
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect_err("invented context must fail closed");
        assert!(error.to_string().contains("context that was not supplied"));
    }

    #[test]
    fn proposal_records_bounded_tool_evidence_and_dynamic_context() {
        let fixture = conflict_fixture();
        let paths = VaultPaths::new(&fixture.reader);
        initialize_vulcan_dir(&paths).expect("initialize cache");
        fs::write(fixture.reader.join("Context.md"), "context marker\n").expect("context note");
        scan_vault(&paths, ScanMode::Full).expect("scan context");
        let proposal = create_resolution_proposal_with_provider(
            &paths,
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
            },
            &ToolUsingProvider,
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("tool-assisted proposal");

        assert_eq!(proposal.referenced_context, ["Context.md"]);
        assert_eq!(proposal.tool_calls.len(), 1);
        assert_eq!(proposal.tool_calls[0].name, "vault_search");
        assert_eq!(proposal.tool_calls[0].referenced_paths, ["Context.md"]);
        assert!(proposal
            .validation
            .contains(&ResolutionProposalValidationCheck::FocusedToolsBounded));
        let path = proposal_path(
            &fixture.store,
            &proposal.repository_key,
            &proposal.conflict_id,
            &proposal.proposal_id,
        );
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("proposal record"))
                .expect("proposal JSON");
        json["tool_calls"][0]["result_hash"] = serde_json::json!("0".repeat(64));
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json).expect("tampered JSON"),
        )
        .expect("tampered proposal fixture");
        let error = load_resolution_proposal(
            &fixture.store,
            &proposal.repository_key,
            &proposal.conflict_id,
            &proposal.proposal_id,
        )
        .expect_err("tampered tool evidence must invalidate the proposal ID");
        assert!(error
            .to_string()
            .contains("does not match its immutable ID"));
    }

    #[test]
    fn proposal_loader_accepts_version_one_records_without_context_metadata() {
        let fixture = conflict_fixture();
        let proposal = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
            },
            &FakeProvider { cancel: false },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("proposal");
        let path = proposal_path(
            &fixture.store,
            &proposal.repository_key,
            &proposal.conflict_id,
            &proposal.proposal_id,
        );
        let mut json = serde_json::to_value(&proposal).expect("proposal JSON");
        json["version"] = serde_json::json!(1);
        json.as_object_mut()
            .expect("proposal object")
            .remove("focused_context");
        fs::write(&path, serde_json::to_vec_pretty(&json).expect("JSON bytes"))
            .expect("legacy proposal fixture");

        let loaded = load_resolution_proposal(
            &fixture.store,
            &proposal.repository_key,
            &proposal.conflict_id,
            &proposal.proposal_id,
        )
        .expect("version one proposal");
        assert_eq!(loaded.version, 1);
        assert!(loaded.focused_context.is_empty());
    }

    #[test]
    fn focused_context_rejects_internal_and_non_utf8_files() {
        let fixture = conflict_fixture();
        let options = |path: &str| ResolutionProposalOptions {
            permission_profile: "unrestricted".to_string(),
            focused_context: vec![path.to_string()],
            allow_broad_context: false,
        };
        let internal = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &options(".vulcan/config.toml"),
            &FakeProvider { cancel: false },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect_err("internal context must be rejected");
        assert!(internal.to_string().contains("invalid or unbounded"));

        fs::write(fixture.reader.join("Context.bin"), [0xff, 0xfe])
            .expect("binary context fixture");
        let binary = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &options("Context.bin"),
            &FakeProvider { cancel: false },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect_err("binary context must be rejected");
        assert!(binary.to_string().contains("must be valid UTF-8"));
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

    #[test]
    fn explicit_rejection_is_content_free_idempotent_and_blocks_approval() {
        let fixture = conflict_fixture();
        let proposal = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
            },
            &FakeProvider { cancel: false },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("proposal");
        let refs_before = git_stdout(
            &fixture.reader,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/vulcan",
            ],
        );
        let rejection = proposal_rejection_record(&proposal);
        let audit_path = proposal_audit_path(&fixture.store, &rejection);

        let preview = reject_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal.proposal_id,
            true,
            &fixture.store,
        )
        .expect("rejection preview");
        assert_eq!(preview.outcome, RejectResolutionProposalOutcome::Planned);
        assert!(!audit_path.exists());

        let rejected = reject_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal.proposal_id,
            false,
            &fixture.store,
        )
        .expect("rejection");
        assert_eq!(rejected.outcome, RejectResolutionProposalOutcome::Rejected);
        let audit = fs::read_to_string(&audit_path).expect("rejection audit");
        assert!(audit.contains("\"action\": \"rejected\""));
        assert!(!audit.contains(&proposal.explanation));
        assert!(!audit.contains("agent resolution"));

        let repeated = reject_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal.proposal_id,
            false,
            &fixture.store,
        )
        .expect("idempotent rejection");
        assert_eq!(
            repeated.outcome,
            RejectResolutionProposalOutcome::AlreadyRejected
        );
        let approval_error = approve_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal.proposal_id,
            &ApproveResolutionProposalOptions {
                remote: GitRemote::parse("origin").expect("remote"),
                live_ref: GitRefName::parse("refs/heads/__vulcan-sync/live").expect("live ref"),
                dry_run: true,
                automatic: false,
            },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect_err("rejected proposal cannot be approved");
        assert!(approval_error.to_string().contains("explicitly rejected"));
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Home.md")).expect("local note"),
            "writer\n"
        );
        assert_eq!(
            git_stdout(
                &fixture.reader,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/vulcan"
                ],
            ),
            refs_before
        );
    }

    #[test]
    fn abandoned_resolutions_do_not_block_rejection_or_side_switches() {
        use crate::sync_conflicts::{ResolveSyncConflictOptions, SyncConflictResolutionSide};
        let fixture = conflict_fixture();
        let proposal = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
            },
            &FakeProvider { cancel: false },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("proposal");
        // Simulate a failed approval whose push was rejected: the durable
        // record exists but never published and never applied.
        let store = SyncConflictStore::from_state_store(&fixture.store);
        store
            .save_resolution(
                &fixture.record.repository_key,
                &SyncConflictResolutionRecord {
                    version: SYNC_CONFLICT_RESOLUTION_VERSION,
                    conflict_id: fixture.record.id.clone(),
                    side: None,
                    proposal_id: Some(proposal.proposal_id.clone()),
                    base_revision: fixture.record.base_revision.clone().unwrap_or_default(),
                    local_revision: fixture.record.local_revision.clone(),
                    remote_revision: fixture.record.remote_revision.clone(),
                    live_input_revision: None,
                    recovery_revision: fixture.record.local_revision.clone(),
                    resolved_tree: proposal.proposal_tree.clone(),
                    resolution_commit: fixture.record.remote_revision.clone(),
                    published: false,
                    applied: false,
                },
            )
            .expect("abandoned resolution");

        let rejection = reject_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal.proposal_id,
            true,
            &fixture.store,
        )
        .expect("rejection must not be blocked by an abandoned resolution");
        assert_eq!(rejection.outcome, RejectResolutionProposalOutcome::Planned);

        let sync_options = GitSyncOptions::default();
        let planned = crate::sync_conflicts::resolve_sync_conflict_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolveSyncConflictOptions {
                side: SyncConflictResolutionSide::Local,
                remote: sync_options.remote.clone(),
                live_ref: sync_options.live_ref.clone(),
                dry_run: true,
            },
            &fixture.store,
        )
        .expect("side switch must not be blocked by an abandoned resolution");
        assert_eq!(
            planned.outcome,
            crate::sync_conflicts::ResolveSyncConflictOutcome::Planned
        );
    }

    #[test]
    fn agent_auto_accept_requires_local_policy_and_reuses_approval_validation() {
        let fixture = conflict_fixture();
        let paths = VaultPaths::new(&fixture.reader);
        let proposal_options = ResolutionProposalOptions {
            permission_profile: "unrestricted".to_string(),
            focused_context: Vec::new(),
            allow_broad_context: false,
        };
        let approval_options = ApproveResolutionProposalOptions {
            remote: GitRemote::parse("origin").expect("remote"),
            live_ref: GitRefName::parse("refs/heads/__vulcan-sync/live").expect("live ref"),
            dry_run: false,
            automatic: true,
        };
        let disabled = create_and_auto_accept_resolution_proposal_with_state_store(
            &paths,
            &fixture.record.id,
            &proposal_options,
            &approval_options,
            &FakeProvider { cancel: false },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect_err("auto-accept defaults off");
        assert!(disabled.to_string().contains("disabled"));

        fs::create_dir_all(paths.vulcan_dir()).expect("Vulcan directory");
        fs::write(
            paths.local_config_file(),
            "[sync]\nagent_auto_accept = true\n",
        )
        .expect("local auto-accept policy");
        let report = create_and_auto_accept_resolution_proposal_with_state_store(
            &paths,
            &fixture.record.id,
            &proposal_options,
            &approval_options,
            &FakeProvider { cancel: false },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("auto-accepted proposal");
        assert_eq!(
            report.approval.outcome,
            ApproveResolutionProposalOutcome::Applied
        );
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Home.md")).expect("resolved note"),
            "agent resolution\n"
        );
        let audit_directory = fixture
            .store
            .root()
            .join(&fixture.record.repository_key)
            .join("conflicts")
            .join(&fixture.record.id)
            .join("audit");
        let audit_path = fs::read_dir(audit_directory)
            .expect("audit directory")
            .next()
            .expect("audit event")
            .expect("audit entry")
            .path();
        let audit = fs::read_to_string(audit_path).expect("audit record");
        assert!(audit.contains("\"action\": \"auto_accepted\""));
        assert!(!audit.contains(&report.proposal.explanation));
    }

    fn assert_approval_lifecycle(
        fixture: &ConflictFixture,
        proposal: &ResolutionProposal,
        refs_before: &str,
    ) {
        let sync_options = GitSyncOptions::default();
        fs::write(fixture.reader.join("Home.md"), "stale local edit\n").expect("stale local edit");
        let stale = approve_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal.proposal_id,
            &ApproveResolutionProposalOptions {
                remote: sync_options.remote.clone(),
                live_ref: sync_options.live_ref.clone(),
                dry_run: true,
                automatic: false,
            },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect_err("stale worktree must reject approval");
        assert!(stale.to_string().contains("worktree no longer matches"));
        fs::write(fixture.reader.join("Home.md"), "writer\n").expect("restore accepted input");
        let dry_run = approve_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal.proposal_id,
            &ApproveResolutionProposalOptions {
                remote: sync_options.remote.clone(),
                live_ref: sync_options.live_ref.clone(),
                dry_run: true,
                automatic: false,
            },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("approval preview");
        assert_eq!(dry_run.outcome, ApproveResolutionProposalOutcome::Planned);
        assert!(SyncConflictStore::from_state_store(&fixture.store)
            .get_resolution(&fixture.record.repository_key, &fixture.record.id)
            .expect("resolution state")
            .is_none());
        assert_eq!(
            git_stdout(
                &fixture.reader,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/vulcan"
                ],
            ),
            refs_before
        );

        let applied = approve_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal.proposal_id,
            &ApproveResolutionProposalOptions {
                remote: sync_options.remote.clone(),
                live_ref: sync_options.live_ref.clone(),
                dry_run: false,
                automatic: false,
            },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("approved proposal");
        assert_eq!(applied.outcome, ApproveResolutionProposalOutcome::Applied);
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Home.md")).expect("approved note"),
            "agent resolution\n"
        );
        let resolution = SyncConflictStore::from_state_store(&fixture.store)
            .get_resolution(&fixture.record.repository_key, &fixture.record.id)
            .expect("resolution state")
            .expect("proposal resolution");
        assert_eq!(
            resolution.proposal_id.as_deref(),
            Some(proposal.proposal_id.as_str())
        );
        assert!(resolution.applied);
        assert_audit_and_idempotency(fixture, proposal, sync_options);
    }

    fn assert_audit_and_idempotency(
        fixture: &ConflictFixture,
        proposal: &ResolutionProposal,
        sync_options: GitSyncOptions,
    ) {
        let audit_directory = fixture
            .store
            .root()
            .join(&fixture.record.repository_key)
            .join("conflicts")
            .join(&fixture.record.id)
            .join("audit");
        let audit_path = fs::read_dir(audit_directory)
            .expect("audit directory")
            .next()
            .expect("audit event")
            .expect("audit entry")
            .path();
        let audit = fs::read_to_string(audit_path).expect("audit record");
        assert!(audit.contains(&proposal.proposal_id));
        assert!(!audit.contains(&proposal.explanation));
        assert!(!audit.contains("agent resolution"));

        let repeated = approve_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal.proposal_id,
            &ApproveResolutionProposalOptions {
                remote: sync_options.remote,
                live_ref: sync_options.live_ref,
                dry_run: false,
                automatic: false,
            },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("repeated approval");
        assert_eq!(
            repeated.outcome,
            ApproveResolutionProposalOutcome::AlreadyApplied
        );
    }

    #[test]
    fn cancellation_after_provider_output_preserves_originals_without_a_proposal_record() {
        let fixture = conflict_fixture();
        let cancellation = SyncCancellationToken::default();
        let error = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
            },
            &FakeProvider { cancel: true },
            &cancellation,
            &fixture.store,
        )
        .expect_err("cancelled proposal");
        assert!(error.to_string().contains("cancelled"));
        assert!(fixture.record.preserved_record_ref.is_some());
        assert!(!fixture
            .store
            .root()
            .join(&fixture.record.repository_key)
            .join("conflicts")
            .join(&fixture.record.id)
            .join("proposals")
            .exists());
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

    fn path(path: &Path) -> &str {
        path.to_str().expect("UTF-8 test path")
    }

    fn configure_git(repository: &Path) {
        git(repository, &["config", "user.name", "Vulcan Test"]);
        git(
            repository,
            &["config", "user.email", "vulcan@example.invalid"],
        );
        git(repository, &["config", "core.autocrlf", "false"]);
    }

    fn commit_all(repository: &Path, message: &str) {
        git(repository, &["add", "-A"]);
        git(repository, &["commit", "--quiet", "-m", message]);
    }

    fn git(repository: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .current_dir(repository)
            .args(arguments)
            .status()
            .expect("Git should launch");
        assert!(status.success(), "Git failed: {arguments:?}");
    }

    fn git_stdout(repository: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(repository)
            .args(arguments)
            .output()
            .expect("Git should launch");
        assert!(output.status.success(), "Git failed: {arguments:?}");
        String::from_utf8(output.stdout)
            .expect("UTF-8 Git output")
            .trim()
            .to_string()
    }
}
