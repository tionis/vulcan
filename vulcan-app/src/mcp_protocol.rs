//! Transport-neutral MCP request types shared by CLI and daemon adapters.

#![allow(clippy::struct_excessive_bools)]

use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The most note or file content a read returns inline; the rest is reached
/// through the note's or file's resource.
pub const MCP_CONTENT_INLINE_LIMIT: usize = 64 * 1024;
pub const MCP_PAGE_SIZE: usize = 100;
pub const MCP_RESOURCE_NOT_FOUND: i64 = -32002;
pub const MCP_QUERY_DEFAULT_LIMIT: usize = 50;
const MCP_DAILY_LIST_DEFAULT_LIMIT: usize = 20;

/// Coarse OAuth scope required for a protocol method. Fine-grained vault
/// permissions are checked separately by the selected application workflow.
#[must_use]
pub fn required_scope_for_request(payload: &Value) -> Option<&'static str> {
    let method = payload.get("method")?.as_str()?;
    if method.starts_with("tools/") {
        Some("mcp:tools")
    } else if method.starts_with("resources/") {
        Some("mcp:resources")
    } else if method.starts_with("prompts/") {
        Some("mcp:prompts")
    } else {
        None
    }
}

/// Tool families the instructions describe, each shown when the caller has
/// any of its tools.
const INSTRUCTION_FAMILIES: &[(&[&str], &str)] = &[
    (
        &["note_create", "note_append", "note_patch", "note_set", "note_delete", "note_move"],
        "Edit notes with note_append and note_patch (pass expected_hash from a read when editing existing text); note_create makes new notes; note_move renames and rewrites links.",
    ),
    (
        &["file_get", "file_put"],
        "Read or write non-Markdown files with file_get and file_put.",
    ),
    (
        &["task_query", "task_create", "task_complete", "task_reschedule"],
        "Tasks: task_query lists them (Tasks query source or filters); task_create, task_complete, task_reschedule, and the reminder tools change them.",
    ),
    (
        &["graph_communities", "link_suggestions"],
        "Graph: graph_communities clusters notes; link_suggestions proposes missing links.",
    ),
    (
        &["web_search", "web_fetch"],
        "Web: web_search and web_fetch reach outside the vault.",
    ),
    (
        &["git_status", "git_commit", "changeset_begin"],
        "History: git_status, git_diff, git_log, git_blame, and git_commit; changesets stage larger changes on a branch until changeset_land.",
    ),
    (
        &["sync_status", "sync_refresh"],
        "Sync: sync_status, sync_doctor, and sync_conflicts report on Git sync; sync_refresh and sync_resume start it.",
    ),
    (
        &["config_get", "config_set", "index_scan"],
        "Maintenance: config_get and config_set for settings, index_scan to refresh the index.",
    ),
];

/// What the server is and how to route a request, for the `instructions` of
/// `initialize` and `server/discover`. The first paragraph stands alone (some clients read only the
/// start); then one line per tool family the caller can use.
#[must_use]
pub fn server_instructions(active_tool_names: &[String]) -> String {
    let active = active_tool_names
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut text = String::from(
        "Vulcan serves one Markdown vault (an Obsidian vault or a plain folder of notes). \
         Read a known note with note_get, or note_outline first when it is long; select notes by \
         property, tag, or path with query; find them by content with search; use daily for \
         daily notes (\"latest\" means the newest existing note). The tools listed are exactly \
         what this connection's permission profile allows. Results are bounded: follow paging \
         instead of asking for everything.",
    );
    for (tools, line) in INSTRUCTION_FAMILIES {
        if tools.iter().any(|tool| active.contains(tool)) {
            text.push(' ');
            text.push_str(line);
        }
    }
    text.push_str(
        " Workflow guides for Vulcan are available as skill resources; read one before an unfamiliar multi-step task.",
    );
    text
}

/// Shape a built-in tool result: `structuredContent` always, plus the
/// compact JSON of it as text for clients that read only text content.
#[must_use]
pub fn tool_result(structured: Value) -> Value {
    let structured = wrap_tool_result(structured);
    let text = serde_json::to_string(&structured).unwrap_or_default();
    serde_json::json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": structured,
        "isError": false,
    })
}

/// Shape a custom tool result: its own display text when it has one,
/// otherwise the compact JSON, and `structuredContent` always.
#[must_use]
pub fn custom_tool_result(structured: Value, text: Option<&str>) -> Value {
    let mut result = tool_result(structured);
    if let Some(text) = text {
        result["content"] = serde_json::json!([{"type": "text", "text": text}]);
    }
    result
}

fn wrap_tool_result(structured: Value) -> Value {
    if structured.is_object() {
        structured
    } else {
        serde_json::json!({"result": structured})
    }
}

/// Stable codes for tool errors, so an agent can branch on the kind of
/// failure rather than parse the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolErrorCode {
    InvalidArgument,
    NotFound,
    Ambiguous,
    Conflict,
    PermissionDenied,
    IndexMissing,
    Timeout,
    Indeterminate,
    Failed,
}

impl McpToolErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::NotFound => "not_found",
            Self::Ambiguous => "ambiguous",
            Self::Conflict => "conflict",
            Self::PermissionDenied => "permission_denied",
            Self::IndexMissing => "index_missing",
            Self::Timeout => "timeout",
            Self::Indeterminate => "indeterminate",
            Self::Failed => "failed",
        }
    }

    /// The code and a next step for an error raised from shared workflows,
    /// which report failures as messages.
    #[must_use]
    pub fn classify(message: &str) -> (Self, Option<&'static str>) {
        let lower = message.to_ascii_lowercase();
        if lower.starts_with("permission denied") || lower.contains("requires destructive access") {
            (Self::PermissionDenied, None)
        } else if lower.starts_with("stale_content") {
            (
                Self::Conflict,
                Some("Read it again and retry with the new content_hash."),
            )
        } else if lower.contains("cache is missing") || lower.contains("run `vulcan scan`") {
            (
                Self::IndexMissing,
                Some("The vault has no index yet: call index_scan, then retry."),
            )
        } else if lower.contains("ambiguous") {
            (Self::Ambiguous, None)
        } else if lower.contains("not found") || lower.contains("no longer exists") {
            (Self::NotFound, None)
        } else if lower.contains("timed out") {
            (Self::Timeout, None)
        } else {
            (Self::Failed, None)
        }
    }
}

/// The structured body of a tool error: `{"error": {"code", "message",
/// "parameter"?, "hint"?}}`, merged into any extra fields already present.
#[must_use]
pub fn tool_error_structured(message: &str, structured: Option<Value>) -> Value {
    let mut body = structured.unwrap_or_else(|| serde_json::json!({}));
    if !body.is_object() {
        body = serde_json::json!({ "detail": body });
    }
    let existing = body.get("error").cloned();
    if existing.as_ref().is_some_and(Value::is_object) {
        return body;
    }
    let message = existing
        .as_ref()
        .and_then(Value::as_str)
        .unwrap_or(message)
        .to_string();
    let (code, hint) = McpToolErrorCode::classify(&message);
    let mut error = serde_json::json!({ "code": code.as_str(), "message": message });
    if let Some(hint) = hint {
        error["hint"] = Value::String(hint.to_string());
    }
    body["error"] = error;
    body
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpListSnapshot {
    pub tools: String,
    pub prompts: String,
    pub resources: String,
}

impl McpListSnapshot {
    /// Return list-change notifications and advance this session's snapshot.
    pub fn changed_notifications(&mut self, current: Self) -> Vec<Value> {
        let mut notifications = Vec::new();
        for (changed, method) in [
            (
                self.tools != current.tools,
                "notifications/tools/list_changed",
            ),
            (
                self.prompts != current.prompts,
                "notifications/prompts/list_changed",
            ),
            (
                self.resources != current.resources,
                "notifications/resources/list_changed",
            ),
        ] {
            if changed {
                notifications.push(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": method,
                }));
            }
        }
        *self = current;
        notifications
    }
}

#[derive(Debug)]
pub enum McpMethodError {
    JsonRpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    Tool {
        message: String,
        structured: Option<Value>,
    },
}

impl McpMethodError {
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::JsonRpc {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::JsonRpc {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }

    pub fn method_not_found(message: impl Into<String>) -> Self {
        Self::JsonRpc {
            code: -32601,
            message: message.into(),
            data: None,
        }
    }

    pub fn tool(message: impl Into<String>) -> Self {
        Self::Tool {
            message: message.into(),
            structured: None,
        }
    }

    /// A tool error with a stable code, optionally naming the argument at fault.
    pub fn tool_coded(
        code: McpToolErrorCode,
        message: impl Into<String>,
        parameter: Option<String>,
    ) -> Self {
        let message = message.into();
        let mut error = serde_json::json!({ "code": code.as_str(), "message": message });
        if let Some(parameter) = parameter {
            error["parameter"] = Value::String(parameter);
        }
        Self::Tool {
            message,
            structured: Some(serde_json::json!({ "error": error })),
        }
    }

    /// The model sent arguments a tool cannot use: a tool error the model
    /// sees and can correct, never a protocol error.
    pub fn invalid_argument(message: impl Into<String>, parameter: Option<String>) -> Self {
        Self::tool_coded(McpToolErrorCode::InvalidArgument, message, parameter)
    }
}

#[derive(Debug)]
pub struct McpMethodOutcome {
    pub response: Option<Value>,
    pub emit_list_notifications: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct McpListParams {
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpToolCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct McpSyncTargetArgs {
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct McpSyncDoctorArgs {
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct McpSyncConflictsArgs {
    #[serde(default)]
    pub conflict_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpPromptGetParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpResourceReadParams {
    pub uri: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpCompletionParams {
    #[serde(rename = "ref")]
    pub reference: McpCompletionReference,
    pub argument: McpCompletionArgument,
    #[serde(default)]
    pub context: McpCompletionContext,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum McpCompletionReference {
    #[serde(rename = "ref/prompt")]
    Prompt { name: String },
    #[serde(rename = "ref/resource")]
    Resource { uri: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpCompletionArgument {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct McpCompletionContext {
    #[serde(default)]
    pub arguments: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpNoteGetArgs {
    pub note: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub section_id: Option<String>,
    #[serde(default)]
    pub heading: Option<String>,
    #[serde(default)]
    pub block_ref: Option<String>,
    #[serde(default)]
    pub lines: Option<String>,
    #[serde(rename = "match", default)]
    pub match_pattern: Option<String>,
    #[serde(default)]
    pub context: usize,
    #[serde(default)]
    pub no_frontmatter: bool,
    #[serde(default)]
    pub raw: bool,
    #[serde(default)]
    pub evaluate: bool,
    #[serde(default)]
    pub dataview_js: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpNoteOutlineArgs {
    pub note: String,
    #[serde(default)]
    pub section_id: Option<String>,
    #[serde(default)]
    pub depth: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSearchArgs {
    pub query: String,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub has_property: Option<String>,
    #[serde(default)]
    pub filters: Vec<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub match_case: bool,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default = "default_search_context_size")]
    pub context_size: usize,
    #[serde(default)]
    pub raw_query: bool,
    #[serde(default)]
    pub fuzzy: bool,
    #[serde(default)]
    pub explain: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpQueryArgs {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub json: Option<String>,
    #[serde(default)]
    pub filters: Vec<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub desc: bool,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub filename_pattern: Option<String>,
    #[serde(default = "default_query_limit")]
    pub limit: usize,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub include_properties: bool,
    #[serde(default)]
    pub allow_large_results: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpDailyArgs {
    pub operation: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default = "default_true")]
    pub include_content: bool,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub week: bool,
    #[serde(default)]
    pub month: bool,
    #[serde(default = "default_daily_list_limit")]
    pub limit: usize,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub order: Option<String>,
    #[serde(default)]
    pub include_events: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTaskQueryArgs {
    /// Tasks plugin query source; when set, the structured filters must be unset.
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default = "default_task_query_limit")]
    pub limit: usize,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub due_before: Option<String>,
    #[serde(default)]
    pub due_after: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub group_by: Option<String>,
    #[serde(default)]
    pub sort_by: Option<String>,
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTaskCreateArgs {
    pub text: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTaskCompleteArgs {
    pub task: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTaskRescheduleArgs {
    pub task: String,
    pub due: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTaskReminderAddArgs {
    pub task: String,
    pub reminder: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTaskReminderRemoveArgs {
    pub task: String,
    pub id: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpGraphCommunitiesArgs {
    #[serde(default)]
    pub community: Option<usize>,
    #[serde(default)]
    pub orphans: bool,
    #[serde(default)]
    pub bridges: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpLinkSuggestionsArgs {
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default = "default_suggest_min_score")]
    pub min_score: f64,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpLinkSuggestionResolveArgs {
    pub id: String,
    pub action: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpNoteCreateArgs {
    pub path: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub frontmatter: BTreeMap<String, Value>,
    #[serde(default)]
    pub check: bool,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpNoteAppendArgs {
    #[serde(default)]
    pub note: Option<String>,
    pub text: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub heading: Option<String>,
    #[serde(default)]
    pub periodic: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default)]
    pub check: bool,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub expected_hash: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpNotePatchArgs {
    pub note: String,
    #[serde(default)]
    pub section_id: Option<String>,
    #[serde(default)]
    pub heading: Option<String>,
    #[serde(default)]
    pub block_ref: Option<String>,
    #[serde(default)]
    pub lines: Option<String>,
    pub find: String,
    pub replace: String,
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub check: bool,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub expected_hash: Option<String>,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpNoteInfoArgs {
    pub note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpNoteSetArgs {
    pub note: String,
    pub content: String,
    #[serde(default)]
    pub preserve_frontmatter: bool,
    #[serde(default)]
    pub check: bool,
    #[serde(default)]
    pub expected_hash: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpNoteMoveArgs {
    pub note: String,
    pub destination: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpFileGetArgs {
    pub path: String,
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub limit_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpFilePutArgs {
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub encoding: Option<String>,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub expected_hash: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpGitDiffArgs {
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpGitLogArgs {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpGitBlameArgs {
    pub path: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpChangesetBeginArgs {
    #[serde(default)]
    pub title: Option<String>,
}

/// A changeset id, as `changeset_begin` returned it.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpChangesetIdArgs {
    pub id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpChangesetLandArgs {
    pub id: String,
    /// Commit message for uncommitted changeset changes before landing.
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpGitCommitArgs {
    pub message: String,
    /// The files to commit; in a changeset, everything when omitted.
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    /// Every uncommitted vault change.
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpNoteDeleteArgs {
    pub note: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub expected_hash: Option<String>,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpWebSearchArgs {
    pub query: String,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default = "default_web_limit")]
    pub limit: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpWebFetchArgs {
    pub url: String,
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfigGetArgs {
    #[serde(default)]
    pub section: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfigSetArgs {
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpIndexScanArgs {
    #[serde(default)]
    pub full: bool,
    #[serde(default)]
    pub no_commit: bool,
}

fn default_task_query_limit() -> usize {
    50
}
fn default_search_limit() -> usize {
    20
}
fn default_query_limit() -> usize {
    MCP_QUERY_DEFAULT_LIMIT
}
fn default_daily_list_limit() -> usize {
    MCP_DAILY_LIST_DEFAULT_LIMIT
}
fn default_true() -> bool {
    true
}
fn default_search_context_size() -> usize {
    18
}
fn default_suggest_min_score() -> f64 {
    0.0
}
fn default_web_limit() -> usize {
    10
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_scopes_cover_all_protocol_namespaces_without_granting_unknown_methods() {
        for (method, scope) in [
            ("tools/call", Some("mcp:tools")),
            ("tools/list", Some("mcp:tools")),
            ("resources/read", Some("mcp:resources")),
            ("resources/templates/list", Some("mcp:resources")),
            ("prompts/get", Some("mcp:prompts")),
            ("initialize", None),
            ("notifications/cancelled", None),
        ] {
            assert_eq!(
                required_scope_for_request(&json!({"method": method})),
                scope
            );
        }
        assert_eq!(required_scope_for_request(&json!({"method": 4})), None);
    }

    #[test]
    fn shared_request_defaults_preserve_mcp_limits() {
        let search: McpSearchArgs = serde_json::from_value(json!({"query": "rust"})).unwrap();
        let query: McpQueryArgs = serde_json::from_value(json!({})).unwrap();
        let daily: McpDailyArgs = serde_json::from_value(json!({"operation": "latest"})).unwrap();
        let web: McpWebSearchArgs = serde_json::from_value(json!({"query": "rust"})).unwrap();

        assert_eq!((search.limit, search.context_size), (20, 18));
        assert_eq!(query.limit, MCP_QUERY_DEFAULT_LIMIT);
        assert_eq!(daily.limit, MCP_DAILY_LIST_DEFAULT_LIMIT);
        assert!(daily.include_content);
        assert_eq!(web.limit, 10);
    }

    #[test]
    fn instructions_describe_only_the_tool_families_the_caller_has() {
        let readonly = vec![
            "note_get".to_string(),
            "search".to_string(),
            "task_query".to_string(),
        ];
        let instructions = server_instructions(&readonly);
        assert!(instructions.contains("Tasks: task_query"));
        assert!(!instructions.contains("Edit notes"));
        assert!(!instructions.contains("Web:"));

        let everything =
            crate::mcp_catalog::active_tool_names(&vulcan_core::PermissionProfile::unrestricted());
        let full = server_instructions(&everything);
        assert!(full.len() <= 2_000, "{} characters", full.len());
        // The opening stands alone for clients that read only the start.
        let opening = &full[..full.find(" Edit notes").unwrap()];
        assert!(opening.len() <= 512, "{} characters", opening.len());
        assert!(opening.contains("note_get") && opening.contains("search"));
    }

    #[test]
    fn shared_request_types_reject_unknown_fields() {
        assert!(serde_json::from_value::<McpToolCallParams>(json!({
            "name": "search",
            "unexpected": true
        }))
        .is_err());
    }

    #[test]
    fn list_snapshot_emits_only_changed_namespaces_and_advances_once() {
        let mut snapshot = McpListSnapshot {
            tools: "a".to_string(),
            prompts: "b".to_string(),
            resources: "c".to_string(),
        };
        let next = McpListSnapshot {
            tools: "new".to_string(),
            prompts: "b".to_string(),
            resources: "new".to_string(),
        };
        assert_eq!(
            snapshot.changed_notifications(next.clone()),
            vec![
                json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
                json!({"jsonrpc": "2.0", "method": "notifications/resources/list_changed"}),
            ]
        );
        assert!(snapshot.changed_notifications(next).is_empty());
    }

    #[test]
    fn tool_results_always_carry_structured_content_and_compact_text() {
        let small = tool_result(json!(3));
        assert_eq!(small["structuredContent"], json!({"result": 3}));
        assert_eq!(small["content"][0]["text"], "{\"result\":3}");
        assert_eq!(small["isError"], false);

        let large = tool_result(json!({"path": "Large.md", "content": "x".repeat(200_000)}));
        assert_eq!(large["structuredContent"]["path"], "Large.md");
        assert_eq!(large["content"].as_array().unwrap().len(), 1);

        let custom = custom_tool_result(json!({"ok": true}), Some("Done."));
        assert_eq!(custom["content"][0]["text"], "Done.");
        assert_eq!(custom["structuredContent"], json!({"ok": true}));
    }

    #[test]
    fn tool_errors_carry_stable_codes_and_hints() {
        let classified = tool_error_structured("note not found: Nope", None);
        assert_eq!(classified["error"]["code"], "not_found");
        let missing =
            tool_error_structured("cache is missing; run `vulcan scan` before searching", None);
        assert_eq!(missing["error"]["code"], "index_missing");
        assert!(missing["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("index_scan"));
        let stale = tool_error_structured(
            "stale_content: `A.md` changed since it was read",
            Some(json!({"timed_out": false})),
        );
        assert_eq!(stale["error"]["code"], "conflict");
        assert_eq!(stale["timed_out"], false);
        let McpMethodError::Tool { structured, .. } =
            McpMethodError::invalid_argument("bad", Some("limit".to_string()))
        else {
            panic!("tool error");
        };
        let coded = tool_error_structured("bad", structured);
        assert_eq!(coded["error"]["code"], "invalid_argument");
        assert_eq!(coded["error"]["parameter"], "limit");
    }
}
