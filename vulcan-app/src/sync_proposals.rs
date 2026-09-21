//! Isolated, review-first agent resolution proposals for preserved Git conflicts.

use crate::durable_file::{self, DurableCreate};
use crate::scan::refresh_cache_incrementally_unlocked;
use crate::sync::{load_validated_sync_config, validate_git_merge_tree};
use crate::sync_conflicts::{
    conflict_group_selection_digest, conflict_groups, conflict_live_input,
    conflict_worktree_revision, conflict_worktree_tree,
    resolve_proposal_conflict_groups_with_state_store, verify_preserved_conflict_refs,
    ConflictGroupResolutionResult, ResolveProposalConflictGroupsOptions,
    ResolveSyncConflictOutcome, SyncConflictGroupKind, SyncConflictGroupState, SyncConflictRecord,
    SyncConflictResolutionRecord, SyncConflictStore, SYNC_CONFLICT_RESOLUTION_VERSION,
};
use crate::sync_state::{repository_state_key, SyncStateStore};
use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use vulcan_core::search::SearchMode;
use vulcan_core::{
    execute_query_report_with_filter, paths::secure_read, query_backlinks_with_filter,
    query_links_with_filter, resolve_permission_profile, search_vault_with_filter, PermissionGuard,
    ProfilePermissionGuard, QueryAst, ScanSummary, SearchQuery, VaultPaths,
};
use vulcan_sync::{
    conflict_proposal_resolution_ref, conflict_recovery_ref,
    remote_conflict_proposal_resolution_ref, GitAutomaticMergeValidation, GitCaptureRequest,
    GitContentMergeResolutionRequest, GitEngine, GitOid, GitPushResult, GitRefName, GitRemote,
    GitResolvedPath, GitSyncOptions, GitSyncRefs, SyncCancellationToken,
};

pub const RESOLUTION_PROPOSAL_VERSION: u32 = 4;
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
const MAX_FORMATTER_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_FORMATTER_DIAGNOSTIC_BYTES: usize = 64 * 1024;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatterResolutionOptions {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub expected_version: String,
    pub config: Option<PathBuf>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum FormatterResolutionReport {
    Preview {
        report: SuppliedResolutionPreviewReport,
    },
    Proposed {
        proposal: Box<ResolutionProposal>,
    },
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionProposalOptions {
    pub permission_profile: String,
    pub focused_context: Vec<String>,
    pub allow_broad_context: bool,
    pub group_ids: Vec<String>,
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
pub struct ResolutionProposalSelection {
    pub group_ids: Vec<String>,
    pub selection_digest: String,
    pub accepted_revision: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ResolutionProposalSelection>,
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
    pub selection: Option<ResolutionProposalSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPatchResolution {
    pub paths: Vec<ResolutionAgentPathOutput>,
    pub selection: Option<ResolutionProposalSelection>,
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
        manual.selection.as_ref().map(|selection| &selection.paths),
        &BTreeSet::new(),
        ResolutionAgentOutput {
            explanation: "Resolution content supplied explicitly by the user.".to_string(),
            referenced_context: Vec::new(),
            paths: supplied,
        },
        Vec::new(),
        ResolutionContentSource::Reviewed,
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

pub fn create_supplied_resolution_proposal(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    supplied: Vec<ResolutionAgentPathOutput>,
    cancellation: &SyncCancellationToken,
) -> Result<ResolutionProposal, AppError> {
    let state_store = SyncStateStore::user_default()?;
    create_supplied_resolution_proposal_with_state_store(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        supplied,
        cancellation,
        &state_store,
    )
}

#[allow(clippy::too_many_lines)]
pub fn create_supplied_resolution_proposal_with_state_store(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    supplied: Vec<ResolutionAgentPathOutput>,
    cancellation: &SyncCancellationToken,
    state_store: &SyncStateStore,
) -> Result<ResolutionProposal, AppError> {
    create_supplied_resolution_proposal_with_expected_selection(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        supplied,
        None,
        None,
        None,
        cancellation,
        state_store,
    )
}

pub fn create_supplied_resolution_proposal_with_selection(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    supplied: Vec<ResolutionAgentPathOutput>,
    expected_selection: &ResolutionProposalSelection,
    cancellation: &SyncCancellationToken,
) -> Result<ResolutionProposal, AppError> {
    let state_store = SyncStateStore::user_default()?;
    create_supplied_resolution_proposal_with_expected_selection(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        supplied,
        Some(expected_selection),
        None,
        None,
        cancellation,
        &state_store,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn create_supplied_resolution_proposal_with_expected_selection(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    supplied: Vec<ResolutionAgentPathOutput>,
    expected_selection: Option<&ResolutionProposalSelection>,
    identity: Option<ResolutionAgentIdentity>,
    explanation: Option<String>,
    cancellation: &SyncCancellationToken,
    state_store: &SyncStateStore,
) -> Result<ResolutionProposal, AppError> {
    if approval_options.dry_run {
        return Err(AppError::operation(
            "supplied-resolution proposal requires mutating mode",
        ));
    }
    cancellation_check(cancellation)?;
    let manual = prepare_manual_resolution_scope(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        state_store,
    )?;
    if expected_selection.is_some()
        && manual
            .selection
            .as_ref()
            .map(|selection| &selection.persisted)
            != expected_selection
    {
        return Err(AppError::operation(
            "the accepted conflict frontier changed after the reviewed resolution was prepared; prepare and review it again",
        ));
    }
    let base_revision = manual
        .record
        .base_revision
        .as_deref()
        .ok_or_else(|| AppError::operation("supplied resolution requires one merge base"))?;
    let prepared = prepare_output(
        &manual.engine,
        &manual.repository,
        &manual.record,
        manual.selection.as_ref().map(|selection| &selection.paths),
        &BTreeSet::new(),
        ResolutionAgentOutput {
            explanation: explanation.unwrap_or_else(|| {
                "Resolution content supplied explicitly by the user.".to_string()
            }),
            referenced_context: Vec::new(),
            paths: supplied,
        },
        Vec::new(),
        ResolutionContentSource::Reviewed,
    )?;
    let (tree_base, tree_remote, tree_local) = if let Some(selection) = &manual.selection {
        let accepted =
            GitOid::parse(&selection.persisted.accepted_revision).map_err(AppError::operation)?;
        (accepted.clone(), accepted.clone(), accepted)
    } else {
        (
            GitOid::parse(base_revision).map_err(AppError::operation)?,
            GitOid::parse(&manual.record.remote_revision).map_err(AppError::operation)?,
            GitOid::parse(&manual.record.local_revision).map_err(AppError::operation)?,
        )
    };
    let proposal_tree = manual
        .engine
        .resolve_merge_tree_with_paths(
            &manual.repository,
            &GitContentMergeResolutionRequest {
                base: tree_base,
                accepted_remote: tree_remote,
                local_candidate: tree_local,
                paths: prepared.git_paths.clone(),
            },
        )
        .map_err(AppError::operation)?;
    verify_tree_objects(
        &manual.engine,
        &manual.repository,
        &proposal_tree,
        &prepared.git_paths,
    )?;
    let conflict_paths = manual.selection.as_ref().map_or_else(
        || conflict_path_names(&manual.record),
        |selection| selection.paths.iter().cloned().collect(),
    );
    validate_proposal_whole_tree_inputs(
        paths,
        &manual.engine,
        &manual.repository,
        base_revision,
        &manual.record.local_revision,
        manual
            .selection
            .as_ref()
            .map_or(manual.record.remote_revision.as_str(), |selection| {
                selection.persisted.accepted_revision.as_str()
            }),
        &proposal_tree,
        &conflict_paths,
    )?;
    cancellation_check(cancellation)?;
    let patch_base = manual
        .selection
        .as_ref()
        .map_or(manual.record.remote_revision.as_str(), |selection| {
            selection.persisted.accepted_revision.as_str()
        });
    let patch = manual
        .engine
        .diff_patch(
            &manual.repository,
            &GitOid::parse(patch_base).map_err(AppError::operation)?,
            &proposal_tree,
            &conflict_paths,
        )
        .map_err(AppError::operation)?;
    let proposal = assemble_proposal(
        &manual.record,
        manual.repository_key.clone(),
        identity.unwrap_or_else(|| SuppliedResolutionProvider::new(Vec::new()).identity()),
        proposal_options,
        &[],
        prepared,
        ProposalTree {
            oid: proposal_tree,
            patch,
        },
        manual
            .selection
            .as_ref()
            .map(|selection| selection.persisted.clone()),
    )?;
    save_proposal(state_store, &proposal)?;
    Ok(proposal)
}

/// Runs an explicitly selected formatter against private copies of every
/// preserved side, merges the normalized sides, and retains the result for
/// review. Formatter output is never accepted by this operation.
pub fn create_formatter_resolution_proposal(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    formatter: &FormatterResolutionOptions,
    cancellation: &SyncCancellationToken,
) -> Result<FormatterResolutionReport, AppError> {
    let state_store = SyncStateStore::user_default()?;
    create_formatter_resolution_proposal_with_state_store(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        formatter,
        cancellation,
        &state_store,
    )
}

#[allow(clippy::too_many_arguments)]
fn create_formatter_resolution_proposal_with_state_store(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    formatter: &FormatterResolutionOptions,
    cancellation: &SyncCancellationToken,
    state_store: &SyncStateStore,
) -> Result<FormatterResolutionReport, AppError> {
    validate_formatter_options(formatter)?;
    cancellation_check(cancellation)?;
    let scope = prepare_manual_resolution_scope(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        state_store,
    )?;
    scope
        .selection
        .as_ref()
        .ok_or_else(|| AppError::operation("formatter proposals require at least one --group"))?;
    let permission = resolve_permission_profile(paths, Some(&proposal_options.permission_profile))
        .map_err(AppError::operation)?;
    ProfilePermissionGuard::new(paths, permission)
        .check_execute()
        .map_err(AppError::operation)?;
    validate_formatter_conflicts(&scope.record, scope.selection.as_ref())?;
    let selection = scope
        .selection
        .as_ref()
        .expect("formatter selection was required")
        .persisted
        .clone();
    let files = formatter_inputs(&scope)?;
    drop(scope);

    let executable = fs::canonicalize(&formatter.executable).map_err(|error| {
        AppError::operation(format!(
            "cannot resolve formatter executable `{}`: {error}",
            formatter.executable.display()
        ))
    })?;
    let executable_hash = hash_bounded_file(&executable, MAX_AGENT_FILE_BYTES, "formatter")?;
    let version = run_formatter_version(&executable, formatter.timeout)?;
    if version != formatter.expected_version {
        return Err(AppError::operation(format!(
            "formatter version mismatch: expected `{}`, got `{version}`",
            formatter.expected_version
        )));
    }
    let temporary = tempfile::tempdir().map_err(AppError::operation)?;
    let config = materialize_formatter_config(temporary.path(), formatter.config.as_deref())?;
    let outputs = format_and_merge_inputs(
        temporary.path(),
        &executable,
        formatter,
        config.as_ref().map(|value| value.0.as_path()),
        &files,
        cancellation,
    )?;
    let config_hash = config.as_ref().map_or("none", |value| value.1.as_str());
    let identity_bytes = serde_json::to_vec(&(
        executable.to_string_lossy(),
        executable_hash.as_str(),
        version.as_str(),
        config_hash,
        &formatter.arguments,
    ))
    .map_err(AppError::operation)?;
    let identity = ResolutionAgentIdentity {
        provider: "vulcan-formatter".to_string(),
        model: blake3::hash(&identity_bytes).to_hex()[..32].to_string(),
        prompt_contract_version: 1,
    };
    let explanation = format!(
        "Generated by explicit formatter {} ({version}); executable={}, config={}.",
        executable.display(),
        executable_hash,
        config_hash
    );
    cancellation_check(cancellation)?;
    if approval_options.dry_run {
        let report = preview_supplied_resolution_with_state_store(
            paths,
            conflict_id,
            proposal_options,
            approval_options,
            outputs,
            state_store,
        )?;
        return Ok(FormatterResolutionReport::Preview { report });
    }
    let proposal = create_supplied_resolution_proposal_with_expected_selection(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        outputs,
        Some(&selection),
        Some(identity),
        Some(explanation),
        cancellation,
        state_store,
    )?;
    Ok(FormatterResolutionReport::Proposed {
        proposal: Box::new(proposal),
    })
}

#[derive(Debug)]
struct FormatterInput {
    path: String,
    base: Vec<u8>,
    local: Vec<u8>,
    remote: Vec<u8>,
}

fn validate_formatter_options(options: &FormatterResolutionOptions) -> Result<(), AppError> {
    if !options.executable.is_absolute() {
        return Err(AppError::operation(
            "formatter executable must be an absolute path",
        ));
    }
    if options.expected_version.trim().is_empty() || options.expected_version.len() > 1024 {
        return Err(AppError::operation(
            "formatter expected version must contain 1-1024 characters",
        ));
    }
    if options.timeout.is_zero() || options.timeout > Duration::from_secs(300) {
        return Err(AppError::operation(
            "formatter timeout must be between 1ms and 300s",
        ));
    }
    if options.arguments.len() > 64
        || options
            .arguments
            .iter()
            .any(|argument| argument.len() > 4096)
    {
        return Err(AppError::operation(
            "formatter arguments exceed their limit",
        ));
    }
    if options
        .arguments
        .iter()
        .filter(|argument| argument.matches("{file}").count() > 1)
        .count()
        > 0
    {
        return Err(AppError::operation(
            "each formatter argument may contain {file} at most once",
        ));
    }
    Ok(())
}

fn validate_formatter_conflicts(
    record: &SyncConflictRecord,
    selection: Option<&ResolvedProposalSelection>,
) -> Result<(), AppError> {
    for path in record.paths.iter().filter(|path| {
        selection.is_none_or(|selection| selection.paths.contains(path.path.as_str()))
    }) {
        if !path
            .classification
            .as_ref()
            .is_some_and(|classification| classification.formatting_candidate)
        {
            return Err(AppError::operation(format!(
                "conflict path `{}` is not a conservative formatting candidate",
                path.path
            )));
        }
        if path.base.object_id.is_none()
            || path.local.object_id.is_none()
            || path.remote.object_id.is_none()
        {
            return Err(AppError::operation(format!(
                "formatter proposal requires base, local, and remote blobs for `{}`",
                path.path
            )));
        }
    }
    Ok(())
}

fn formatter_inputs(scope: &ManualResolutionScope) -> Result<Vec<FormatterInput>, AppError> {
    let selected = &scope
        .selection
        .as_ref()
        .expect("formatter selection was required")
        .paths;
    let names = selected.iter().cloned().collect::<Vec<_>>();
    let base = GitOid::parse(
        scope
            .record
            .base_revision
            .as_deref()
            .ok_or_else(|| AppError::operation("formatter proposal requires one merge base"))?,
    )
    .map_err(AppError::operation)?;
    let local = GitOid::parse(&scope.record.local_revision).map_err(AppError::operation)?;
    let remote = GitOid::parse(&scope.record.remote_revision).map_err(AppError::operation)?;
    let base_objects = scope
        .engine
        .path_objects(&scope.repository, &base, &names)
        .map_err(AppError::operation)?;
    let local_objects = scope
        .engine
        .path_objects(&scope.repository, &local, &names)
        .map_err(AppError::operation)?;
    let remote_objects = scope
        .engine
        .path_objects(&scope.repository, &remote, &names)
        .map_err(AppError::operation)?;
    let mut total = 0_usize;
    names
        .into_iter()
        .map(|path| {
            let read = |objects: &BTreeMap<String, vulcan_sync::GitPathObject>, side: &str| {
                let content = objects
                    .get(&path)
                    .and_then(|object| object.data.clone())
                    .ok_or_else(|| {
                        AppError::operation(format!(
                            "formatter proposal requires a {side} blob for `{path}`"
                        ))
                    })?;
                if content.len() > MAX_AGENT_FILE_BYTES {
                    return Err(AppError::operation(format!(
                        "formatter input `{path}` exceeds the per-file byte limit"
                    )));
                }
                Ok(content)
            };
            let base = read(&base_objects, "base")?;
            let local = read(&local_objects, "local")?;
            let remote = read(&remote_objects, "remote")?;
            total = total.saturating_add(base.len() + local.len() + remote.len());
            if total > MAX_AGENT_TOTAL_BYTES {
                return Err(AppError::operation(
                    "formatter inputs exceed the aggregate byte limit",
                ));
            }
            Ok(FormatterInput {
                path,
                base,
                local,
                remote,
            })
        })
        .collect()
}

fn hash_bounded_file(path: &Path, limit: usize, label: &str) -> Result<String, AppError> {
    let metadata = fs::metadata(path).map_err(AppError::operation)?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(AppError::operation(format!(
            "{label} must be a regular file no larger than {limit} bytes"
        )));
    }
    let mut file = fs::File::open(path).map_err(AppError::operation)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let count = file.read(&mut buffer).map_err(AppError::operation)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn run_formatter_version(executable: &Path, timeout: Duration) -> Result<String, AppError> {
    let output = run_bounded_command(executable, &["--version".to_string()], None, timeout)?;
    if !output.status.success() {
        return Err(AppError::operation(format!(
            "formatter --version failed: {}",
            diagnostic(&output)
        )));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|_| AppError::operation("formatter --version returned non-UTF-8 output"))
}

fn materialize_formatter_config(
    root: &Path,
    config: Option<&Path>,
) -> Result<Option<(PathBuf, String)>, AppError> {
    let Some(config) = config else {
        return Ok(None);
    };
    let hash = hash_bounded_file(config, MAX_FORMATTER_CONFIG_BYTES, "formatter config")?;
    let extension = config
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("config");
    let target = root.join(format!("formatter-config.{extension}"));
    fs::copy(config, &target).map_err(AppError::operation)?;
    Ok(Some((target, hash)))
}

fn format_and_merge_inputs(
    root: &Path,
    executable: &Path,
    options: &FormatterResolutionOptions,
    config: Option<&Path>,
    inputs: &[FormatterInput],
    cancellation: &SyncCancellationToken,
) -> Result<Vec<ResolutionAgentPathOutput>, AppError> {
    let mut total = 0_usize;
    inputs
        .iter()
        .map(|input| {
            cancellation_check(cancellation)?;
            let paths = ["base", "local", "remote"]
                .map(|side| safe_formatter_path(root, side, &input.path));
            for (path, content) in paths.iter().zip([&input.base, &input.local, &input.remote]) {
                fs::create_dir_all(path.parent().expect("formatter path has a parent"))
                    .map_err(AppError::operation)?;
                fs::write(path, content).map_err(AppError::operation)?;
                run_formatter(executable, options, root, path, config)?;
            }
            let local = &paths[1];
            let merge_args = vec![
                "merge-file".to_string(),
                "--quiet".to_string(),
                local.to_string_lossy().into_owned(),
                paths[0].to_string_lossy().into_owned(),
                paths[2].to_string_lossy().into_owned(),
            ];
            let merged =
                run_bounded_command(Path::new("git"), &merge_args, Some(root), options.timeout)?;
            if !merged.status.success() {
                return Err(AppError::operation(format!(
                    "normalized sides for `{}` still conflict; review them manually ({})",
                    input.path,
                    diagnostic(&merged)
                )));
            }
            let content = fs::read(local).map_err(AppError::operation)?;
            if content.len() > MAX_AGENT_FILE_BYTES {
                return Err(AppError::operation(format!(
                    "formatter output `{}` exceeds the per-file byte limit",
                    input.path
                )));
            }
            total = total.saturating_add(content.len());
            if total > MAX_AGENT_TOTAL_BYTES {
                return Err(AppError::operation(
                    "formatter outputs exceed the aggregate byte limit",
                ));
            }
            Ok(ResolutionAgentPathOutput {
                path: input.path.clone(),
                content,
            })
        })
        .collect()
}

fn safe_formatter_path(root: &Path, side: &str, relative: &str) -> PathBuf {
    root.join(side).join(relative)
}

fn run_formatter(
    executable: &Path,
    options: &FormatterResolutionOptions,
    root: &Path,
    file: &Path,
    config: Option<&Path>,
) -> Result<(), AppError> {
    let mut saw_file = false;
    let mut arguments = Vec::with_capacity(options.arguments.len() + 1);
    for argument in &options.arguments {
        if argument.contains("{config}") && config.is_none() {
            return Err(AppError::operation(
                "formatter argument uses {config} without --formatter-config",
            ));
        }
        saw_file |= argument.contains("{file}");
        arguments.push(argument.replace("{file}", &file.to_string_lossy()).replace(
            "{config}",
            &config.map_or_else(String::new, |value| value.to_string_lossy().into_owned()),
        ));
    }
    if !saw_file {
        arguments.push(file.to_string_lossy().into_owned());
    }
    let output = run_bounded_command(executable, &arguments, Some(root), options.timeout)?;
    if !output.status.success() {
        return Err(AppError::operation(format!(
            "formatter failed for `{}`: {}",
            file.display(),
            diagnostic(&output)
        )));
    }
    Ok(())
}

struct BoundedCommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_bounded_command(
    executable: &Path,
    arguments: &[String],
    current_dir: Option<&Path>,
    timeout: Duration,
) -> Result<BoundedCommandOutput, AppError> {
    let stdout = tempfile::tempfile().map_err(AppError::operation)?;
    let stderr = tempfile::tempfile().map_err(AppError::operation)?;
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            stdout.try_clone().map_err(AppError::operation)?,
        ))
        .stderr(Stdio::from(
            stderr.try_clone().map_err(AppError::operation)?,
        ));
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }
    let mut child = command.spawn().map_err(|error| {
        AppError::operation(format!(
            "cannot execute `{}`: {error}",
            executable.display()
        ))
    })?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(AppError::operation)? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::operation(format!(
                "command `{}` exceeded its {:?} timeout",
                executable.display(),
                timeout
            )));
        }
        thread::sleep(Duration::from_millis(10));
    };
    Ok(BoundedCommandOutput {
        status,
        stdout: read_bounded_command_file(stdout, "stdout")?,
        stderr: read_bounded_command_file(stderr, "stderr")?,
    })
}

fn read_bounded_command_file(mut file: fs::File, stream: &str) -> Result<Vec<u8>, AppError> {
    let size = file.metadata().map_err(AppError::operation)?.len();
    if size > MAX_FORMATTER_DIAGNOSTIC_BYTES as u64 {
        return Err(AppError::operation(format!(
            "formatter {stream} exceeded its byte limit"
        )));
    }
    file.seek(SeekFrom::Start(0)).map_err(AppError::operation)?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(size).expect("bounded formatter diagnostic size fits usize"),
    );
    file.read_to_end(&mut bytes).map_err(AppError::operation)?;
    Ok(bytes)
}

fn diagnostic(output: &BoundedCommandOutput) -> String {
    let bytes = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    String::from_utf8_lossy(bytes).trim().to_string()
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
    require_exact_selected_paths(&manual.record, manual.selection.as_ref(), &patch_paths)?;
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
    Ok(prepare_patch_resolution(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        patch,
    )?
    .paths)
}

pub fn prepare_patch_resolution(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    patch: &[u8],
) -> Result<PreparedPatchResolution, AppError> {
    let state_store = SyncStateStore::user_default()?;
    prepare_patch_resolution_with_state_store(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        patch,
        &state_store,
    )
}

fn prepare_patch_resolution_with_state_store(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    patch: &[u8],
    state_store: &SyncStateStore,
) -> Result<PreparedPatchResolution, AppError> {
    if approval_options.dry_run {
        return Err(AppError::operation(
            "patch resolution paths require mutating mode",
        ));
    }
    let manual = prepare_manual_resolution_scope(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        state_store,
    )?;
    let local = GitOid::parse(&manual.record.local_revision).map_err(AppError::operation)?;
    let patch_paths = manual
        .engine
        .check_patch(&manual.repository, &local, patch)
        .map_err(AppError::operation)?;
    require_exact_selected_paths(&manual.record, manual.selection.as_ref(), &patch_paths)?;
    let tree = manual
        .engine
        .apply_patch_to_tree(&manual.repository, &local, patch)
        .map_err(AppError::operation)?;
    let paths = patch_paths
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
            Ok::<_, AppError>(ResolutionAgentPathOutput {
                path,
                content: data,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PreparedPatchResolution {
        paths,
        selection: manual
            .selection
            .as_ref()
            .map(|selection| selection.persisted.clone()),
    })
}

pub fn prepare_editor_resolution(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
) -> Result<EditorResolutionPlan, AppError> {
    let state_store = SyncStateStore::user_default()?;
    prepare_editor_resolution_with_state_store(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        &state_store,
    )
}

fn prepare_editor_resolution_with_state_store(
    paths: &VaultPaths,
    conflict_id: &str,
    proposal_options: &ResolutionProposalOptions,
    approval_options: &ApproveResolutionProposalOptions,
    state_store: &SyncStateStore,
) -> Result<EditorResolutionPlan, AppError> {
    let manual = prepare_manual_resolution_scope(
        paths,
        conflict_id,
        proposal_options,
        approval_options,
        state_store,
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
    for conflict_path in manual.record.paths.iter().filter(|path| {
        manual
            .selection
            .as_ref()
            .is_none_or(|selection| selection.paths.contains(&path.path))
    }) {
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
        selection: manual
            .selection
            .as_ref()
            .map(|selection| selection.persisted.clone()),
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
    selection: Option<ResolvedProposalSelection>,
    _vault_lock: Option<vulcan_core::write_lock::WriteLockGuard>,
    _lock: vulcan_sync::RepositoryLock,
}

#[derive(Debug, Clone)]
struct ResolvedProposalSelection {
    persisted: ResolutionProposalSelection,
    paths: BTreeSet<String>,
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
    } = prepare_resolution_scope(paths, conflict_id, proposal_options, state_store, false)?;
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let (vault_lock, lock) = acquire_proposal_apply_locks(paths, &repository)?;
    let conflict_store = SyncConflictStore::from_state_store(state_store);
    if conflict_store
        .get_effective_resolution(&repository_key, conflict_id)?
        .is_some()
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
    let accepted = engine
        .remote_ref(
            &repository,
            &approval_options.remote,
            &approval_options.live_ref,
        )
        .map_err(AppError::operation)?
        .ok_or_else(|| AppError::operation("the remote live ref is missing"))?;
    let selection = resolve_proposal_selection(
        &engine,
        &repository,
        &conflict_store,
        &repository_key,
        &record,
        &options_group_ids(proposal_options),
        &accepted,
    )?;
    let local = selection.as_ref().map_or_else(
        || conflict_worktree_revision(&record),
        |_| Ok(accepted.clone()),
    )?;
    let expected_tree = selection.as_ref().map_or_else(
        || conflict_worktree_tree(&engine, &repository, &record),
        |_| {
            engine
                .tree_oid(&repository, &accepted)
                .map_err(AppError::operation)
        },
    )?;
    if engine
        .snapshot_worktree_tree(&repository, Some(&local))
        .map_err(AppError::operation)?
        != expected_tree
    {
        return Err(AppError::operation(
            "the worktree no longer matches the preserved local conflict input",
        ));
    }
    if selection.is_none() && accepted.as_str() != conflict_live_input(&record)? {
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
        selection,
        _vault_lock: vault_lock,
        _lock: lock,
    })
}

fn options_group_ids(options: &ResolutionProposalOptions) -> Vec<String> {
    let mut group_ids = options.group_ids.clone();
    group_ids.sort();
    group_ids.dedup();
    group_ids
}

fn resolve_proposal_selection(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    store: &SyncConflictStore,
    repository_key: &str,
    record: &SyncConflictRecord,
    group_ids: &[String],
    accepted: &GitOid,
) -> Result<Option<ResolvedProposalSelection>, AppError> {
    if group_ids.is_empty() {
        return Ok(None);
    }
    if group_ids.len() > 128 {
        return Err(AppError::operation(
            "one resolution proposal may select at most 128 conflict groups",
        ));
    }
    let groups = conflict_groups(record);
    let by_id = groups
        .iter()
        .map(|group| (group.id.as_str(), group))
        .collect::<BTreeMap<_, _>>();
    let progress = store.group_progress(repository_key, record)?;
    let states = progress
        .groups
        .iter()
        .map(|group| (group.id.as_str(), group.state))
        .collect::<BTreeMap<_, _>>();
    let mut paths = BTreeSet::new();
    for group_id in group_ids {
        validate_hex_id("conflict group ID", group_id)?;
        let group = by_id
            .get(group_id.as_str())
            .ok_or_else(|| AppError::operation(format!("unknown conflict group `{group_id}`")))?;
        if group.kind == SyncConflictGroupKind::WholeTree {
            return Err(AppError::operation(
                "whole-tree validation conflicts cannot use a scoped proposal",
            ));
        }
        if !matches!(
            states.get(group_id.as_str()),
            Some(SyncConflictGroupState::Pending | SyncConflictGroupState::NeedsRebase)
        ) {
            return Err(AppError::operation(format!(
                "conflict group `{group_id}` already has an active or applied resolution batch"
            )));
        }
        paths.extend(group.paths.iter().cloned());
    }
    let original = GitOid::parse(conflict_live_input(record)?).map_err(AppError::operation)?;
    if original != *accepted {
        let selected = paths.iter().cloned().collect::<Vec<_>>();
        let original_objects = engine
            .path_objects(repository, &original, &selected)
            .map_err(AppError::operation)?;
        let accepted_objects = engine
            .path_objects(repository, accepted, &selected)
            .map_err(AppError::operation)?;
        if original_objects != accepted_objects {
            return Err(AppError::operation(
                "one or more selected conflict groups changed on the accepted live frontier and require a fresh reconciliation",
            ));
        }
    }
    Ok(Some(ResolvedProposalSelection {
        persisted: ResolutionProposalSelection {
            group_ids: group_ids.to_vec(),
            selection_digest: conflict_group_selection_digest(group_ids),
            accepted_revision: accepted.to_string(),
        },
        paths,
    }))
}

fn require_exact_selected_paths(
    record: &SyncConflictRecord,
    selection: Option<&ResolvedProposalSelection>,
    actual: &[String],
) -> Result<(), AppError> {
    let mut expected = record
        .paths
        .iter()
        .filter(|path| selection.is_none_or(|selection| selection.paths.contains(&path.path)))
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
    create_resolution_proposal_with_provider_for_target(
        paths,
        conflict_id,
        options,
        None,
        provider,
        cancellation,
        state_store,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn create_resolution_proposal_with_provider_for_target(
    paths: &VaultPaths,
    conflict_id: &str,
    options: &ResolutionProposalOptions,
    target: Option<(&GitRemote, &GitRefName)>,
    provider: &dyn ResolutionAgentProvider,
    cancellation: &SyncCancellationToken,
    state_store: &SyncStateStore,
) -> Result<ResolutionProposal, AppError> {
    if !options.group_ids.is_empty() && target.is_none() {
        return Err(AppError::operation(
            "selection-scoped agent proposals require an explicit remote live target",
        ));
    }
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
        target,
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
    let _locks = acquire_proposal_apply_locks(paths, &repository)?;
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
        target,
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

fn acquire_proposal_apply_locks(
    paths: &VaultPaths,
    repository: &vulcan_sync::GitRepository,
) -> Result<
    (
        Option<vulcan_core::write_lock::WriteLockGuard>,
        vulcan_sync::RepositoryLock,
    ),
    AppError,
> {
    let vault = paths
        .vulcan_dir()
        .is_dir()
        .then(|| vulcan_core::write_lock::acquire_write_lock(paths))
        .transpose()
        .map_err(AppError::operation)?;
    let repository = acquire_proposal_lock(repository)?;
    Ok((vault, repository))
}

struct GenerationInputs {
    refs_before: Vec<(String, Option<String>)>,
    worktree_before: vulcan_sync::GitOid,
    worktree_base: vulcan_sync::GitOid,
    selection: Option<ResolvedProposalSelection>,
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
    target: Option<(&GitRemote, &GitRefName)>,
    run: ProviderRun,
) -> Result<ResolutionProposal, AppError> {
    if let Some(selection) = &inputs.selection {
        let (remote, live_ref) = target.expect("selected proposal has explicit target");
        let current = engine
            .remote_ref(repository, remote, live_ref)
            .map_err(AppError::operation)?
            .ok_or_else(|| AppError::operation("the remote live ref is missing"))?;
        if current.as_str() != selection.persisted.accepted_revision {
            return Err(AppError::operation(
                "the accepted conflict frontier moved while the agent proposal was generated; generate a fresh proposal",
            ));
        }
    }
    let prepared = prepare_output(
        engine,
        repository,
        record,
        inputs.selection.as_ref().map(|selection| &selection.paths),
        &run.supplied_context,
        run.output,
        run.tool_calls,
        ResolutionContentSource::Agent,
    )?;
    let (tree_base, tree_remote, tree_local) = if let Some(selection) = &inputs.selection {
        let accepted =
            GitOid::parse(&selection.persisted.accepted_revision).map_err(AppError::operation)?;
        (accepted.clone(), accepted.clone(), accepted)
    } else {
        (
            GitOid::parse(base_revision).map_err(AppError::operation)?,
            GitOid::parse(&record.remote_revision).map_err(AppError::operation)?,
            GitOid::parse(&record.local_revision).map_err(AppError::operation)?,
        )
    };
    let proposal_tree = engine
        .resolve_merge_tree_with_paths(
            repository,
            &GitContentMergeResolutionRequest {
                base: tree_base,
                accepted_remote: tree_remote,
                local_candidate: tree_local,
                paths: prepared.git_paths.clone(),
            },
        )
        .map_err(AppError::operation)?;
    verify_tree_objects(engine, repository, &proposal_tree, &prepared.git_paths)?;
    let conflict_paths = inputs.selection.as_ref().map_or_else(
        || conflict_path_names(record),
        |selection| selection.paths.iter().cloned().collect(),
    );
    let accepted_revision = inputs
        .selection
        .as_ref()
        .map_or(record.remote_revision.as_str(), |selection| {
            selection.persisted.accepted_revision.as_str()
        });
    validate_proposal_whole_tree_inputs(
        paths,
        engine,
        repository,
        base_revision,
        &record.local_revision,
        accepted_revision,
        &proposal_tree,
        &conflict_paths,
    )?;
    verify_no_external_mutation(
        engine,
        repository,
        record,
        &inputs.worktree_before,
        &inputs.worktree_base,
        &inputs.refs_before,
    )?;
    let patch = engine
        .diff_patch(
            repository,
            &GitOid::parse(accepted_revision).map_err(AppError::operation)?,
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
        inputs
            .selection
            .as_ref()
            .map(|selection| selection.persisted.clone()),
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
    target: Option<(&GitRemote, &GitRefName)>,
) -> Result<GenerationInputs, AppError> {
    let _pre_locks = acquire_proposal_apply_locks(paths, repository)?;
    ensure_no_existing_proposal(state_store, repository_key, conflict_id)?;
    verify_preserved_conflict_refs(engine, repository, record)?;
    let refs_before = preserved_ref_snapshot(engine, repository, record)?;
    let store = SyncConflictStore::from_state_store(state_store);
    let accepted = target
        .map(|(remote, live_ref)| {
            engine
                .remote_ref(repository, remote, live_ref)
                .map_err(AppError::operation)?
                .ok_or_else(|| AppError::operation("the remote live ref is missing"))
        })
        .transpose()?;
    let selection = match accepted.as_ref() {
        Some(accepted) => resolve_proposal_selection(
            engine,
            repository,
            &store,
            repository_key,
            record,
            &options_group_ids(options),
            accepted,
        )?,
        None => None,
    };
    let local_revision = selection.as_ref().map_or_else(
        || conflict_worktree_revision(record),
        |_| {
            Ok(accepted
                .clone()
                .expect("selected proposal has accepted target"))
        },
    )?;
    let worktree_before = engine
        .snapshot_worktree_tree(repository, Some(&local_revision))
        .map_err(AppError::operation)?;
    if selection.is_some()
        && worktree_before
            != engine
                .tree_oid(repository, &local_revision)
                .map_err(AppError::operation)?
    {
        return Err(AppError::operation(
            "the worktree does not match the accepted proposal frontier",
        ));
    }
    let request = build_agent_request(
        paths,
        engine,
        repository,
        record,
        options,
        selection.as_ref(),
    )?;
    Ok(GenerationInputs {
        refs_before,
        worktree_before,
        worktree_base: local_revision,
        selection,
        request,
    })
}

fn prepare_agent_scope(
    paths: &VaultPaths,
    conflict_id: &str,
    options: &ResolutionProposalOptions,
    state_store: &SyncStateStore,
) -> Result<AgentScope, AppError> {
    prepare_resolution_scope(paths, conflict_id, options, state_store, true)
}

fn prepare_resolution_scope(
    paths: &VaultPaths,
    conflict_id: &str,
    options: &ResolutionProposalOptions,
    state_store: &SyncStateStore,
    require_agent_eligible: bool,
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
    let conflict_store = SyncConflictStore::from_state_store(state_store);
    let record = conflict_store.get(&repository_key, conflict_id)?;
    if conflict_store.resolution_state(&repository_key, conflict_id)?
        == crate::sync_conflicts::SyncConflictResolutionState::Superseded
    {
        return Err(AppError::operation(format!(
            "conflict `{conflict_id}` was superseded by later synchronization and is retained only as history; choose a currently unresolved record from `vulcan sync conflicts`"
        )));
    }
    let selected_paths = selected_paths_for_group_ids(&record, &options_group_ids(options))?;
    if require_agent_eligible {
        validate_agent_conflict_scope(&record, selected_paths.as_ref())?;
    }
    for path in record.paths.iter().filter(|path| {
        selected_paths
            .as_ref()
            .is_none_or(|selected| selected.contains(&path.path))
    }) {
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

fn selected_paths_for_group_ids(
    record: &SyncConflictRecord,
    group_ids: &[String],
) -> Result<Option<BTreeSet<String>>, AppError> {
    if group_ids.is_empty() {
        return Ok(None);
    }
    let groups = conflict_groups(record)
        .into_iter()
        .map(|group| (group.id.clone(), group))
        .collect::<BTreeMap<_, _>>();
    let mut paths = BTreeSet::new();
    for group_id in group_ids {
        validate_hex_id("conflict group ID", group_id)?;
        let group = groups
            .get(group_id)
            .ok_or_else(|| AppError::operation(format!("unknown conflict group `{group_id}`")))?;
        if group.kind == SyncConflictGroupKind::WholeTree {
            return Err(AppError::operation(
                "whole-tree validation conflicts cannot use a scoped proposal",
            ));
        }
        paths.extend(group.paths.iter().cloned());
    }
    Ok(Some(paths))
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

pub fn create_resolution_proposal_for_target(
    paths: &VaultPaths,
    conflict_id: &str,
    options: &ResolutionProposalOptions,
    remote: &GitRemote,
    live_ref: &GitRefName,
    provider: &dyn ResolutionAgentProvider,
    cancellation: &SyncCancellationToken,
) -> Result<ResolutionProposal, AppError> {
    let state_store = SyncStateStore::user_default()?;
    create_resolution_proposal_with_provider_for_target(
        paths,
        conflict_id,
        options,
        Some((remote, live_ref)),
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
    let proposal = create_resolution_proposal_with_provider_for_target(
        paths,
        conflict_id,
        proposal_options,
        Some((&approval_options.remote, &approval_options.live_ref)),
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
    let expected_id = match proposal.version {
        3 => Some(recompute_v3_proposal_id(&proposal)?),
        RESOLUTION_PROPOSAL_VERSION => Some(recompute_current_proposal_id(&proposal)?),
        _ => None,
    };
    if expected_id
        .as_deref()
        .is_some_and(|id| id != proposal.proposal_id)
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
    let resolved_paths = revalidate_proposal_tree(&engine, &repository, &record, &proposal, false)?;
    revalidate_proposal_whole_tree(paths, &engine, &repository, &proposal)?;
    if proposal.selection.is_some() {
        return approve_selected_proposal(
            paths,
            &vault,
            &record,
            &proposal,
            &resolved_paths,
            options,
            cancellation,
            state_store,
        );
    }
    let existing = store.get_effective_resolution(&repository_key, conflict_id)?;
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

#[allow(clippy::too_many_arguments)]
fn approve_selected_proposal(
    paths: &VaultPaths,
    vault: &Path,
    record: &SyncConflictRecord,
    proposal: &ResolutionProposal,
    resolved_paths: &[GitResolvedPath],
    options: &ApproveResolutionProposalOptions,
    cancellation: &SyncCancellationToken,
    state_store: &SyncStateStore,
) -> Result<ApproveResolutionProposalReport, AppError> {
    let selection = proposal
        .selection
        .as_ref()
        .expect("selected approval requires proposal selection");
    let expected_revision =
        GitOid::parse(&selection.accepted_revision).map_err(AppError::operation)?;
    let proposed_tree = GitOid::parse(&proposal.proposal_tree).map_err(AppError::operation)?;
    let result = resolve_proposal_conflict_groups_with_state_store(
        paths,
        &record.id,
        &ResolveProposalConflictGroupsOptions {
            group_ids: &selection.group_ids,
            proposal_id: &proposal.proposal_id,
            expected_revision: &expected_revision,
            proposed_tree: &proposed_tree,
            resolved_paths,
            remote: &options.remote,
            live_ref: &options.live_ref,
            dry_run: options.dry_run,
        },
        cancellation,
        state_store,
    )?;
    selected_proposal_report(vault, proposal, options, state_store, result)
}

fn selected_proposal_report(
    vault: &Path,
    proposal: &ResolutionProposal,
    options: &ApproveResolutionProposalOptions,
    state_store: &SyncStateStore,
    result: ConflictGroupResolutionResult,
) -> Result<ApproveResolutionProposalReport, AppError> {
    let outcome = match result.outcome {
        ResolveSyncConflictOutcome::Planned => ApproveResolutionProposalOutcome::Planned,
        ResolveSyncConflictOutcome::Resolved => ApproveResolutionProposalOutcome::Applied,
        ResolveSyncConflictOutcome::AlreadyResolved => {
            ApproveResolutionProposalOutcome::AlreadyApplied
        }
    };
    if !options.dry_run {
        if let Some(commit) = result.resolution_commit.as_deref() {
            save_approval_audit(state_store, proposal, commit, options)?;
        }
    }
    Ok(ApproveResolutionProposalReport {
        vault: vault.to_path_buf(),
        repository_key: proposal.repository_key.clone(),
        conflict_id: proposal.conflict_id.clone(),
        proposal_id: proposal.proposal_id.clone(),
        dry_run: options.dry_run,
        outcome,
        proposal_tree: proposal.proposal_tree.clone(),
        recovery_revision: result.recovery_revision,
        resolution_commit: result.resolution_commit,
        cache_refresh: result.cache_refresh,
    })
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
    let _locks = acquire_proposal_apply_locks(context.paths, repository)?;
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
                    "vulcan proposal recovery snapshot\n\nVulcan-Conflict: {}\nVulcan-Proposal: {}\nVulcan-Sync-Version: 2\nVulcan-Sync-Device: {}\nVulcan-Sync-Source: {local}\nVulcan-Sync-Semantic: false\n",
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
        .get_effective_resolution(context.repository_key, &context.record.id)?;
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
        Some(refresh_cache_incrementally_unlocked(context.paths)?)
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
        &resolution.resolution_commit,
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
    if let Some(selection) = &proposal.selection {
        let mut group_ids = selection.group_ids.clone();
        group_ids.sort();
        group_ids.dedup();
        if group_ids.is_empty()
            || group_ids != selection.group_ids
            || selection.selection_digest != conflict_group_selection_digest(&group_ids)
            || GitOid::parse(&selection.accepted_revision).is_err()
        {
            return Err(AppError::operation(
                "resolution proposal has an invalid conflict-group selection binding",
            ));
        }
        let groups = conflict_groups(record)
            .into_iter()
            .map(|group| (group.id.clone(), group))
            .collect::<BTreeMap<_, _>>();
        if group_ids.iter().any(|id| {
            groups
                .get(id)
                .is_none_or(|group| group.kind == SyncConflictGroupKind::WholeTree)
        }) {
            return Err(AppError::operation(
                "resolution proposal selects an unknown or whole-tree conflict group",
            ));
        }
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
) -> Result<Vec<GitResolvedPath>, AppError> {
    let expected = proposal_selected_paths(record, proposal)?;
    if proposal.paths.len() != expected.len() {
        return Err(AppError::operation(
            "resolution proposal path set no longer matches the conflict",
        ));
    }
    let tree = GitOid::parse(&proposal.proposal_tree).map_err(AppError::operation)?;
    let mut resolved = Vec::with_capacity(proposal.paths.len());
    let mut seen = BTreeSet::new();
    for path in &proposal.paths {
        if !seen.insert(path.path.as_str()) || !expected.contains(path.path.as_str()) {
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
        let source = if proposal.provider == "vulcan-manual" {
            ResolutionContentSource::Reviewed
        } else {
            ResolutionContentSource::Agent
        };
        validate_proposal_content(record, &path.path, &data, source)?;
        resolved.push(GitResolvedPath {
            path: path.path.clone(),
            mode: Some(path.mode.clone()),
            data: Some(data),
        });
    }
    if reconstruct {
        let (base, remote, local) = if let Some(selection) = &proposal.selection {
            let accepted =
                GitOid::parse(&selection.accepted_revision).map_err(AppError::operation)?;
            (accepted.clone(), accepted.clone(), accepted)
        } else {
            (
                GitOid::parse(&proposal.base_revision).map_err(AppError::operation)?,
                GitOid::parse(&proposal.remote_revision).map_err(AppError::operation)?,
                GitOid::parse(&proposal.local_revision).map_err(AppError::operation)?,
            )
        };
        let reconstructed = engine
            .resolve_merge_tree_with_paths(
                repository,
                &GitContentMergeResolutionRequest {
                    base,
                    accepted_remote: remote,
                    local_candidate: local,
                    paths: resolved.clone(),
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
            &GitOid::parse(proposal_patch_base(proposal)).map_err(AppError::operation)?,
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
    Ok(resolved)
}

fn proposal_selected_paths(
    record: &SyncConflictRecord,
    proposal: &ResolutionProposal,
) -> Result<BTreeSet<String>, AppError> {
    let Some(selection) = &proposal.selection else {
        return Ok(record.paths.iter().map(|path| path.path.clone()).collect());
    };
    let selected_groups = selection
        .group_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let paths = record
        .paths
        .iter()
        .filter(|path| selected_groups.contains(path.group_id.as_str()))
        .map(|path| path.path.clone())
        .collect::<BTreeSet<_>>();
    if paths.is_empty() {
        return Err(AppError::operation(
            "resolution proposal selection has no conflict paths",
        ));
    }
    Ok(paths)
}

fn proposal_patch_base(proposal: &ResolutionProposal) -> &str {
    proposal
        .selection
        .as_ref()
        .map_or(proposal.remote_revision.as_str(), |selection| {
            selection.accepted_revision.as_str()
        })
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
        proposal_patch_base(proposal),
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
    source: ResolutionContentSource,
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
            if source == ResolutionContentSource::Agent {
                return Err(AppError::operation(format!(
                    "proposal path `{path}` has an ineligible file kind"
                )));
            }
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
                "vulcan conflict proposal resolution\n\nVulcan-Conflict: {}\nVulcan-Proposal: {}\nVulcan-Resolution-Provider: {}\nVulcan-Resolution-Model: {}\nVulcan-Sync-Version: 2\nVulcan-Sync-Device: {device_id}\nVulcan-Sync-Policy: {}:{}\nVulcan-Sync-Source: {remote}+{local}\nVulcan-Sync-Semantic: false\n",
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
    let proposal_id = resolution.proposal_id.as_deref().ok_or_else(|| {
        AppError::operation("approved proposal resolution has no proposal identity")
    })?;
    let resolved_ref =
        remote_conflict_proposal_resolution_ref(&resolution.conflict_id, proposal_id)
            .map_err(AppError::operation)?;
    match engine
        .remote_ref(repository, &options.remote, &resolved_ref)
        .map_err(AppError::operation)?
    {
        Some(existing) if existing == commit => {}
        Some(_) => {
            return Err(AppError::operation(format!(
                "remote proposal resolution `{resolved_ref}` identifies a different commit"
            )));
        }
        None => {
            if engine
                .push_ref(repository, &options.remote, &commit, &resolved_ref, None)
                .map_err(AppError::operation)?
                == GitPushResult::Rejected
            {
                let existing = engine
                    .remote_ref(repository, &options.remote, &resolved_ref)
                    .map_err(AppError::operation)?;
                if existing.as_ref() != Some(&commit) {
                    return Err(AppError::operation(format!(
                        "remote proposal resolution `{resolved_ref}` was created concurrently with a different commit"
                    )));
                }
            }
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
    resolution_commit: &str,
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
            resolution_commit
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
        resolution_commit: Some(resolution_commit.to_string()),
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
    write_json_noclobber(&path, record)
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
    let has_complete_resolution = store
        .get_effective_resolution(repository_key, &proposal.conflict_id)?
        .is_some();
    let has_group_batch = store
        .list_batches(repository_key, &proposal.conflict_id)?
        .iter()
        .any(|batch| batch.proposal_id.as_deref() == Some(proposal.proposal_id.as_str()));
    if has_complete_resolution || has_group_batch {
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

#[allow(clippy::too_many_arguments)]
fn assemble_proposal(
    record: &SyncConflictRecord,
    repository_key: String,
    identity: ResolutionAgentIdentity,
    options: &ResolutionProposalOptions,
    focused_context: &[ResolutionAgentContextFile],
    prepared: PreparedOutput,
    tree: ProposalTree,
    selection: Option<ResolutionProposalSelection>,
) -> Result<ResolutionProposal, AppError> {
    let proposal_context = focused_context
        .iter()
        .map(|context| ResolutionProposalContext {
            path: context.path.clone(),
            content_hash: context.content_hash.clone(),
            bytes: context.content.len() as u64,
        })
        .collect::<Vec<_>>();
    let proposal_id = proposal_id_v4(
        &record.id,
        &identity,
        &proposal_context,
        &prepared.tool_calls,
        &prepared.paths,
        &tree.oid,
        selection.as_ref(),
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
        selection,
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

#[allow(clippy::too_many_lines)]
fn build_agent_request(
    paths: &VaultPaths,
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
    options: &ResolutionProposalOptions,
    selection: Option<&ResolvedProposalSelection>,
) -> Result<ResolutionAgentRequest, AppError> {
    let base = record
        .base_revision
        .as_deref()
        .ok_or_else(|| AppError::operation("agent resolution requires one merge base"))?;
    let conflict_paths = selection.map_or_else(
        || conflict_path_names(record),
        |selection| selection.paths.iter().cloned().collect(),
    );
    let base_oid = GitOid::parse(base).map_err(AppError::operation)?;
    let local_oid = GitOid::parse(&record.local_revision).map_err(AppError::operation)?;
    let remote_oid = GitOid::parse(&record.remote_revision).map_err(AppError::operation)?;
    let mut remaining = MAX_AGENT_TOTAL_BYTES;
    let base_objects = engine
        .path_objects_bounded(
            repository,
            &base_oid,
            &conflict_paths,
            MAX_AGENT_FILE_BYTES,
            remaining,
        )
        .map_err(AppError::operation)?;
    remaining = remaining.saturating_sub(path_object_bytes(&base_objects));
    let local_objects = engine
        .path_objects_bounded(
            repository,
            &local_oid,
            &conflict_paths,
            MAX_AGENT_FILE_BYTES,
            remaining,
        )
        .map_err(AppError::operation)?;
    remaining = remaining.saturating_sub(path_object_bytes(&local_objects));
    let remote_objects = engine
        .path_objects_bounded(
            repository,
            &remote_oid,
            &conflict_paths,
            MAX_AGENT_FILE_BYTES,
            remaining,
        )
        .map_err(AppError::operation)?;
    let mut total = 0_usize;
    let mut files = Vec::with_capacity(conflict_paths.len());
    for path in record
        .paths
        .iter()
        .filter(|path| selection.is_none_or(|selection| selection.paths.contains(&path.path)))
    {
        let mut side = |revision: Option<&str>,
                        objects: Option<&BTreeMap<String, vulcan_sync::GitPathObject>>|
         -> Result<ResolutionAgentSide, AppError> {
            let Some(revision) = revision else {
                return Ok(ResolutionAgentSide {
                    revision: None,
                    mode: None,
                    content: None,
                });
            };
            let object = objects.and_then(|objects| objects.get(&path.path));
            let content = object.and_then(|object| object.data.clone());
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
            if total > MAX_AGENT_TOTAL_BYTES {
                return Err(AppError::operation(
                    "conflict inputs exceed the total agent byte limit",
                ));
            }
            Ok(ResolutionAgentSide {
                revision: Some(revision.to_string()),
                mode: object.map(|object| object.mode.clone()),
                content,
            })
        };
        files.push(ResolutionAgentFile {
            path: path.path.clone(),
            base: side(Some(base), Some(&base_objects))?,
            local: side(Some(&record.local_revision), Some(&local_objects))?,
            remote: side(Some(&record.remote_revision), Some(&remote_objects))?,
        });
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
        selection: selection.map(|selection| selection.persisted.clone()),
        files,
        focused_context,
        broad_context_allowed: options.allow_broad_context,
        tool_contract_version: RESOLUTION_AGENT_TOOL_CONTRACT_VERSION,
    })
}

fn path_object_bytes(objects: &BTreeMap<String, vulcan_sync::GitPathObject>) -> usize {
    objects
        .values()
        .filter_map(|object| object.data.as_ref())
        .map(Vec::len)
        .sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolutionContentSource {
    Agent,
    Reviewed,
}

#[allow(clippy::too_many_arguments)]
fn prepare_output(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    record: &SyncConflictRecord,
    selected_paths: Option<&BTreeSet<String>>,
    supplied_context: &BTreeSet<String>,
    output: ResolutionAgentOutput,
    tool_calls: Vec<ResolutionProposalToolCall>,
    source: ResolutionContentSource,
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
        .filter(|path| selected_paths.is_none_or(|selected| selected.contains(&path.path)))
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
    let mut git_paths = Vec::with_capacity(expected.len());
    let mut paths = Vec::with_capacity(expected.len());
    for conflict_path in record
        .paths
        .iter()
        .filter(|path| selected_paths.is_none_or(|selected| selected.contains(&path.path)))
    {
        let content = supplied
            .remove(&conflict_path.path)
            .expect("validated exact path set");
        validate_proposal_content(record, &conflict_path.path, &content, source)?;
        let mode = resolved_mode(conflict_path, source == ResolutionContentSource::Reviewed)?;
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

fn resolved_mode(
    path: &crate::sync_conflicts::SyncConflictPathRecord,
    allow_new_file: bool,
) -> Result<String, AppError> {
    let base = path.base.mode.as_deref();
    let local = path.local.mode.as_deref();
    let remote = path.remote.mode.as_deref();
    if allow_new_file && base.is_none() && local.is_none() && remote.is_none() {
        Some("100644")
    } else if local == remote {
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
    worktree_base: &GitOid,
    refs_before: &[(String, Option<String>)],
) -> Result<(), AppError> {
    let current = engine
        .snapshot_worktree_tree(repository, Some(worktree_base))
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

fn proposal_id_v3(
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

fn proposal_id_v4(
    conflict_id: &str,
    identity: &ResolutionAgentIdentity,
    context: &[ResolutionProposalContext],
    tool_calls: &[ResolutionProposalToolCall],
    paths: &[ResolutionProposalPath],
    tree: &GitOid,
    selection: Option<&ResolutionProposalSelection>,
) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(&(
        conflict_id,
        identity,
        context,
        tool_calls,
        paths,
        tree.as_str(),
        selection,
    ))
    .map_err(AppError::operation)?;
    Ok(blake3::hash(&bytes).to_hex()[..32].to_string())
}

fn recompute_v3_proposal_id(proposal: &ResolutionProposal) -> Result<String, AppError> {
    proposal_id_v3(
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

fn recompute_current_proposal_id(proposal: &ResolutionProposal) -> Result<String, AppError> {
    proposal_id_v4(
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
        proposal.selection.as_ref(),
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
    write_bytes_noclobber(&path, &bytes)
}

fn write_json_noclobber(path: &Path, value: &impl Serialize) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(AppError::operation)?;
    if bytes.len() > MAX_PROPOSAL_RECORD_BYTES {
        return Err(AppError::operation(
            "resolution proposal state exceeds its byte limit",
        ));
    }
    write_bytes_noclobber(path, &bytes)
}

fn write_bytes_noclobber(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    match durable_file::create(path, bytes)? {
        DurableCreate::Created => Ok(()),
        DurableCreate::AlreadyExists => {
            let existing = fs::read(path).map_err(AppError::operation)?;
            if existing == bytes {
                Ok(())
            } else {
                Err(AppError::operation("resolution proposal ID collision"))
            }
        }
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
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(AppError::operation)?;
                let path = entry.path();
                let proposal_id = path
                    .file_stem()
                    .and_then(std::ffi::OsStr::to_str)
                    .ok_or_else(|| AppError::operation("invalid retained proposal filename"))?;
                let proposal =
                    load_resolution_proposal(store, repository_key, conflict_id, proposal_id)?;
                if !proposal_has_terminal_audit(store, &proposal)? {
                    return Err(AppError::operation(format!(
                        "conflict `{conflict_id}` already has a ready resolution proposal"
                    )));
                }
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::operation(error)),
    }
}

fn proposal_has_terminal_audit(
    store: &SyncStateStore,
    proposal: &ResolutionProposal,
) -> Result<bool, AppError> {
    let directory = store
        .root()
        .join(&proposal.repository_key)
        .join("conflicts")
        .join(&proposal.conflict_id)
        .join("audit");
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(AppError::operation(error)),
    };
    for entry in entries {
        let path = entry.map_err(AppError::operation)?.path();
        let metadata = fs::metadata(&path).map_err(AppError::operation)?;
        if metadata.len() > MAX_PROPOSAL_RECORD_BYTES as u64 {
            return Err(AppError::operation(
                "resolution proposal audit record exceeds its byte limit",
            ));
        }
        let audit: ResolutionProposalAuditRecord =
            serde_json::from_slice(&fs::read(path).map_err(AppError::operation)?)
                .map_err(AppError::operation)?;
        if audit.repository_key == proposal.repository_key
            && audit.conflict_id == proposal.conflict_id
            && audit.proposal_id == proposal.proposal_id
        {
            return Ok(true);
        }
    }
    Ok(false)
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

fn validate_agent_conflict_scope(
    record: &SyncConflictRecord,
    selected_paths: Option<&BTreeSet<String>>,
) -> Result<(), AppError> {
    for path in record
        .paths
        .iter()
        .filter(|path| selected_paths.is_none_or(|selected| selected.contains(&path.path)))
    {
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
    use crate::sync_conflicts::{SyncConflictPathRecord, SyncConflictSideRecord};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use tempfile::{tempdir, TempDir};
    use vulcan_core::{paths::initialize_vulcan_dir, scan_vault, ScanMode};
    use vulcan_sync::{GitCliEngine, GitSyncOptions};

    #[cfg(feature = "web")]
    #[test]
    fn resolution_provider_rejects_credentials_over_remote_http() {
        let error = match OpenAiCompatibleResolutionProvider::new(
            "http://api.example.com/v1",
            "fixture-model",
            Some("secret".to_string()),
        ) {
            Ok(_) => panic!("remote cleartext endpoint must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("credentials require HTTPS"));
    }

    #[test]
    fn reviewed_resolution_can_create_a_synthesized_conflict_destination() {
        let absent = |revision: &str| SyncConflictSideRecord {
            revision: revision.to_string(),
            object_id: None,
            mode: None,
            kind: None,
            artifact: None,
            content_hash: None,
            bytes: None,
        };
        let path = SyncConflictPathRecord {
            path: "Renamed/remote.base".to_string(),
            group_id: String::new(),
            group_kind: crate::sync_conflicts::SyncConflictGroupKind::Structural,
            classification: None,
            base: absent("base"),
            local: absent("local"),
            remote: absent("remote"),
        };

        assert_eq!(resolved_mode(&path, true).expect("reviewed mode"), "100644");
        assert!(resolved_mode(&path, false).is_err());
    }

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
            assert!(request.files[0]
                .base
                .content
                .as_deref()
                .is_some_and(|content| content.starts_with(b"base")));
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

    #[cfg(unix)]
    fn formatting_conflict_fixture() -> ConflictFixture {
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
        fs::write(writer.join("Home.md"), "alpha beta gamma\n").expect("base note");
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
        fs::write(writer.join("Home.md"), "alpha\nbeta gamma\n").expect("writer edit");
        fs::write(reader.join("Home.md"), "alpha  beta gamma\n").expect("reader edit");
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
        let record = report.conflict_record.expect("conflict record");
        assert!(
            record.paths[0]
                .classification
                .as_ref()
                .expect("classification")
                .formatting_candidate
        );
        ConflictFixture {
            _temporary: temporary,
            store,
            reader,
            record,
        }
    }

    #[cfg(unix)]
    #[test]
    fn formatter_normalizes_all_sides_into_a_review_only_scoped_proposal() {
        let fixture = formatting_conflict_fixture();
        let worktree_before = fs::read(fixture.reader.join("Home.md")).expect("worktree");
        let formatter = fixture
            .reader
            .parent()
            .expect("fixture root")
            .join("formatter.sh");
        fs::write(
            &formatter,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'fixture-fmt 1\\n'; exit 0; fi\ntr '\\n' ' ' < \"$1\" | tr -s ' ' > \"$1.tmp\"\nprintf '\\n' >> \"$1.tmp\"\nmv \"$1.tmp\" \"$1\"\n",
        )
        .expect("formatter script");
        let mut permissions = fs::metadata(&formatter).expect("metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&formatter, permissions).expect("executable formatter");
        let proposal_options = ResolutionProposalOptions {
            permission_profile: "unrestricted".to_string(),
            focused_context: Vec::new(),
            allow_broad_context: false,
            group_ids: vec![fixture.record.paths[0].group_id.clone()],
        };
        let approval_options = ApproveResolutionProposalOptions {
            remote: GitRemote::parse("origin").expect("remote"),
            live_ref: GitRefName::parse("refs/heads/__vulcan-sync/live").expect("live ref"),
            dry_run: false,
            automatic: false,
        };
        let report = create_formatter_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal_options,
            &approval_options,
            &FormatterResolutionOptions {
                executable: formatter,
                arguments: Vec::new(),
                expected_version: "fixture-fmt 1".to_string(),
                config: None,
                timeout: Duration::from_secs(5),
            },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("formatter proposal");
        let FormatterResolutionReport::Proposed { proposal } = report else {
            panic!("mutating mode should retain a proposal")
        };
        assert_eq!(proposal.provider, "vulcan-formatter");
        assert!(proposal.selection.is_some());
        assert_eq!(proposal.paths.len(), 1);
        assert_eq!(
            fs::read(fixture.reader.join("Home.md")).expect("unchanged worktree"),
            worktree_before
        );
        assert!(SyncConflictStore::from_state_store(&fixture.store)
            .get_effective_resolution(&fixture.record.repository_key, &fixture.record.id)
            .expect("resolution state")
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn agent_request_reads_conflict_sides_with_bounded_git_processes() {
        let fixture = conflict_fixture();
        let mut record = fixture.record.clone();
        let prototype = record.paths[0].clone();
        record.paths = (0..200)
            .map(|index| SyncConflictPathRecord {
                path: format!("missing-{index:03}.md"),
                ..prototype.clone()
            })
            .collect();
        let trace = fixture.reader.join("agent-git-invocations.log");
        let wrapper = fixture.reader.join("agent-git-wrapper");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec git \"$@\"\n",
                trace.display()
            ),
        )
        .expect("Git wrapper");
        let mut permissions = fs::metadata(&wrapper)
            .expect("wrapper metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&wrapper, permissions).expect("executable wrapper");
        let engine = GitCliEngine::new(&wrapper);
        let repository = engine
            .discover_repository(&fixture.reader)
            .expect("repository");
        fs::write(&trace, "").expect("reset trace");

        let request = build_agent_request(
            &VaultPaths::new(&fixture.reader),
            &engine,
            &repository,
            &record,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
                group_ids: Vec::new(),
            },
            None,
        )
        .expect("agent request");

        assert_eq!(request.files.len(), 200);
        assert_eq!(
            fs::read_to_string(&trace).expect("trace").lines().count(),
            3,
            "one tree inventory per immutable side, independent of path count"
        );
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

    fn two_path_conflict_fixture() -> ConflictFixture {
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
        fs::write(writer.join("Home.md"), "base home\n").expect("base home");
        fs::write(writer.join("Other.md"), "base other\n").expect("base other");
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
        fs::write(writer.join("Home.md"), "writer home\n").expect("writer home");
        fs::write(writer.join("Other.md"), "writer other\n").expect("writer other");
        fs::write(reader.join("Home.md"), "reader home\n").expect("reader home");
        fs::write(reader.join("Other.md"), "reader other\n").expect("reader other");
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
    fn selected_reviewed_proposal_overlays_and_applies_only_one_complete_group() {
        let fixture = two_path_conflict_fixture();
        let group_id = fixture
            .record
            .paths
            .iter()
            .find(|path| path.path == "Home.md")
            .expect("home conflict")
            .group_id
            .clone();
        let proposal_options = ResolutionProposalOptions {
            permission_profile: "unrestricted".to_string(),
            focused_context: Vec::new(),
            allow_broad_context: false,
            group_ids: vec![group_id.clone()],
        };
        let approval_options = ApproveResolutionProposalOptions {
            remote: GitRemote::parse("origin").expect("remote"),
            live_ref: GitRefName::parse("refs/heads/__vulcan-sync/live").expect("live ref"),
            dry_run: false,
            automatic: false,
        };
        let proposal = create_supplied_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal_options,
            &approval_options,
            vec![ResolutionAgentPathOutput {
                path: "Home.md".to_string(),
                content: b"reviewed home\n".to_vec(),
            }],
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("selected proposal");
        let selection = proposal.selection.as_ref().expect("selection binding");
        assert_eq!(selection.group_ids, [group_id]);
        assert_eq!(proposal.paths.len(), 1);

        let report = approve_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal.proposal_id,
            &approval_options,
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("selected approval");
        assert_eq!(report.outcome, ApproveResolutionProposalOutcome::Applied);
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Home.md")).expect("home"),
            "reviewed home\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Other.md")).expect("other"),
            "writer other\n"
        );
        let progress = SyncConflictStore::from_state_store(&fixture.store)
            .group_progress(&fixture.record.repository_key, &fixture.record)
            .expect("group progress");
        assert_eq!(progress.applied_groups, 1);
        assert_eq!(progress.pending_groups, 1);
    }

    #[test]
    fn selected_agent_proposal_discloses_and_applies_only_selected_groups() {
        let fixture = two_path_conflict_fixture();
        let home_group = fixture
            .record
            .paths
            .iter()
            .find(|path| path.path == "Home.md")
            .expect("home conflict")
            .group_id
            .clone();
        let remote = GitRemote::parse("origin").expect("remote");
        let live_ref = GitRefName::parse("refs/heads/__vulcan-sync/live").expect("live ref");
        let proposal = create_resolution_proposal_with_provider_for_target(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
                group_ids: vec![home_group.clone()],
            },
            Some((&remote, &live_ref)),
            &FakeProvider { cancel: false },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("selected agent proposal");
        assert_eq!(proposal.paths.len(), 1);
        assert_eq!(proposal.paths[0].path, "Home.md");
        assert_eq!(
            proposal.selection.as_ref().expect("selection").group_ids,
            [home_group]
        );

        let report = approve_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal.proposal_id,
            &ApproveResolutionProposalOptions {
                remote,
                live_ref,
                dry_run: false,
                automatic: false,
            },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("approve selected agent proposal");
        assert_eq!(report.outcome, ApproveResolutionProposalOutcome::Applied);
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Home.md")).expect("home"),
            "agent resolution\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.reader.join("Other.md")).expect("other"),
            "writer other\n"
        );
    }

    #[test]
    fn selected_agent_eligibility_ignores_unselected_ineligible_paths() {
        let fixture = two_path_conflict_fixture();
        let mut record = fixture.record;
        let home = record
            .paths
            .iter()
            .find(|path| path.path == "Home.md")
            .expect("home conflict");
        let selected = BTreeSet::from([home.path.clone()]);
        let other = record
            .paths
            .iter_mut()
            .find(|path| path.path == "Other.md")
            .expect("other conflict");
        other.path = ".obsidian/workspace.json".to_string();
        other
            .classification
            .as_mut()
            .expect("classification")
            .file_kind = vulcan_sync::MergeFileKind::ObsidianState;

        validate_agent_conflict_scope(&record, Some(&selected))
            .expect("unselected ineligible path is not disclosed");
        assert!(validate_agent_conflict_scope(&record, None).is_err());
    }

    #[test]
    fn selected_patch_and_editor_handoffs_stay_pinned_to_the_reviewed_frontier() {
        use crate::sync_conflicts::{
            resolve_sync_conflict_with_state_store, ResolveSyncConflictOptions,
            SyncConflictResolutionSide,
        };

        let fixture = two_path_conflict_fixture();
        let group_id = |name: &str| {
            fixture
                .record
                .paths
                .iter()
                .find(|path| path.path == name)
                .expect("conflict path")
                .group_id
                .clone()
        };
        let home_group = group_id("Home.md");
        let other_group = group_id("Other.md");
        let proposal_options = ResolutionProposalOptions {
            permission_profile: "unrestricted".to_string(),
            focused_context: Vec::new(),
            allow_broad_context: false,
            group_ids: vec![home_group],
        };
        let approval_options = ApproveResolutionProposalOptions {
            remote: GitRemote::parse("origin").expect("remote"),
            live_ref: GitRefName::parse("refs/heads/__vulcan-sync/live").expect("live ref"),
            dry_run: false,
            automatic: false,
        };
        let prepared = prepare_patch_resolution_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal_options,
            &approval_options,
            b"diff --git a/Home.md b/Home.md\n--- a/Home.md\n+++ b/Home.md\n@@ -1 +1 @@\n-reader home\n+patched home\n",
            &fixture.store,
        )
        .expect("selected patch");
        assert_eq!(prepared.paths.len(), 1);
        assert_eq!(prepared.paths[0].path, "Home.md");
        let selection = prepared.selection.clone().expect("patch selection");

        let editor = prepare_editor_resolution_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal_options,
            &approval_options,
            &fixture.store,
        )
        .expect("selected editor plan");
        assert_eq!(editor.files.len(), 1);
        assert_eq!(editor.files[0].path, "Home.md");
        assert_eq!(editor.selection.as_ref(), Some(&selection));

        resolve_sync_conflict_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolveSyncConflictOptions {
                side: SyncConflictResolutionSide::Remote,
                group_ids: vec![other_group],
                remote: approval_options.remote.clone(),
                live_ref: approval_options.live_ref.clone(),
                dry_run: false,
            },
            &fixture.store,
        )
        .expect("advance accepted frontier with sibling group");

        let error = create_supplied_resolution_proposal_with_expected_selection(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &proposal_options,
            &approval_options,
            prepared.paths,
            Some(&selection),
            None,
            None,
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect_err("reviewed patch must not rebind to a later accepted frontier");
        assert!(error
            .to_string()
            .contains("frontier changed after the reviewed resolution was prepared"));
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
                group_ids: Vec::new(),
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
                group_ids: Vec::new(),
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
                group_ids: Vec::new(),
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
                group_ids: Vec::new(),
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
                group_ids: Vec::new(),
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
    fn proposal_loader_keeps_version_three_integrity_checks_after_format_upgrade() {
        let fixture = conflict_fixture();
        let mut proposal = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
                group_ids: Vec::new(),
            },
            &FakeProvider { cancel: false },
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("proposal");
        proposal.version = 3;
        proposal.selection = None;
        proposal.proposal_id = recompute_v3_proposal_id(&proposal).expect("version 3 ID");
        save_proposal(&fixture.store, &proposal).expect("version 3 proposal");

        assert_eq!(
            load_resolution_proposal(
                &fixture.store,
                &proposal.repository_key,
                &proposal.conflict_id,
                &proposal.proposal_id,
            )
            .expect("valid version 3 proposal"),
            proposal
        );
        let path = proposal_path(
            &fixture.store,
            &proposal.repository_key,
            &proposal.conflict_id,
            &proposal.proposal_id,
        );
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("proposal record"))
                .expect("proposal JSON");
        json["paths"][0]["content_hash"] = serde_json::json!("0".repeat(64));
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
        .expect_err("tampered version 3 proposal must fail integrity validation");
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
                group_ids: Vec::new(),
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
            group_ids: Vec::new(),
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
    #[allow(clippy::too_many_lines)]
    fn explicit_rejection_is_content_free_idempotent_and_blocks_approval() {
        let fixture = conflict_fixture();
        let proposal = create_resolution_proposal_with_provider(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
                group_ids: Vec::new(),
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
        let replacement = create_supplied_resolution_proposal_with_state_store(
            &VaultPaths::new(&fixture.reader),
            &fixture.record.id,
            &ResolutionProposalOptions {
                permission_profile: "unrestricted".to_string(),
                focused_context: Vec::new(),
                allow_broad_context: false,
                group_ids: Vec::new(),
            },
            &ApproveResolutionProposalOptions {
                remote: GitRemote::parse("origin").expect("remote"),
                live_ref: GitRefName::parse("refs/heads/__vulcan-sync/live").expect("live ref"),
                dry_run: false,
                automatic: false,
            },
            vec![ResolutionAgentPathOutput {
                path: "Home.md".to_string(),
                content: b"replacement resolution\n".to_vec(),
            }],
            &SyncCancellationToken::default(),
            &fixture.store,
        )
        .expect("rejection permits a new proposal");
        assert_ne!(replacement.proposal_id, proposal.proposal_id);
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
                group_ids: Vec::new(),
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
                group_ids: Vec::new(),
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
            group_ids: Vec::new(),
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
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(&fixture.reader)
            .expect("proposal repository");
        let remote_resolution_ref =
            remote_conflict_proposal_resolution_ref(&fixture.record.id, &proposal.proposal_id)
                .expect("proposal resolution ref");
        assert_eq!(
            engine
                .remote_ref(&repository, &sync_options.remote, &remote_resolution_ref,)
                .expect("remote proposal resolution ref")
                .as_ref()
                .map(GitOid::as_str),
            Some(resolution.resolution_commit.as_str())
        );
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
                group_ids: Vec::new(),
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
