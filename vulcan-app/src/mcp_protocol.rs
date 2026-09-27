//! Transport-neutral MCP request types shared by CLI and daemon adapters.

#![allow(clippy::struct_excessive_bools)]

use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub const MCP_INLINE_TEXT_LIMIT: usize = 4_096;
pub const MCP_STRUCTURED_CONTENT_LIMIT: usize = 65_536;
pub const MCP_PAGE_SIZE: usize = 100;
pub const MCP_RESOURCE_NOT_FOUND: i64 = -32002;
pub const MCP_QUERY_DEFAULT_LIMIT: usize = 50;
const MCP_DAILY_LIST_DEFAULT_LIMIT: usize = 20;

#[derive(Debug, Clone)]
struct McpStoredResource {
    uri: String,
    mime_type: &'static str,
    text: String,
}

/// Ephemeral large tool results owned by a single MCP session.
#[derive(Debug, Clone)]
pub struct McpToolResourceStore {
    resources: BTreeMap<String, McpStoredResource>,
    next_id: u64,
}

impl Default for McpToolResourceStore {
    fn default() -> Self {
        Self {
            resources: BTreeMap::new(),
            next_id: 1,
        }
    }
}

impl McpToolResourceStore {
    /// Shape a built-in tool result, storing large JSON in this session.
    pub fn success_response(&mut self, tool_name: &str, structured: Value) -> Value {
        let structured = wrap_tool_result(structured);
        let serialized = serde_json::to_string_pretty(&structured).unwrap_or_default();
        let content = if serialized.len() <= MCP_INLINE_TEXT_LIMIT {
            vec![serde_json::json!({"type": "text", "text": serialized})]
        } else {
            vec![
                serde_json::json!({
                    "type": "text",
                    "text": tool_summary_text(tool_name, &structured),
                }),
                self.store_json(tool_name, &serialized),
            ]
        };
        tool_success_content(content, structured, serialized.len())
    }

    /// Shape a custom tool result, preserving its optional display text.
    pub fn custom_success_response(
        &mut self,
        tool_name: &str,
        structured: Value,
        text: Option<&str>,
    ) -> Value {
        let structured = wrap_tool_result(structured);
        let serialized = serde_json::to_string_pretty(&structured).unwrap_or_default();
        let mut content = Vec::new();
        if let Some(text) = text {
            if text.len() <= MCP_INLINE_TEXT_LIMIT {
                content.push(serde_json::json!({"type": "text", "text": text}));
            } else {
                content.push(serde_json::json!({
                    "type": "text",
                    "text": format!("`{tool_name}` returned text too large to inline; read the linked resource."),
                }));
                content.push(self.store_text(tool_name, text));
            }
        }
        if serialized.len() <= MCP_INLINE_TEXT_LIMIT {
            if text.is_none() {
                content.push(serde_json::json!({"type": "text", "text": serialized}));
            }
        } else {
            if text.is_none() {
                content.push(serde_json::json!({
                    "type": "text",
                    "text": tool_summary_text(tool_name, &structured),
                }));
            }
            content.push(self.store_json(tool_name, &serialized));
        }
        tool_success_content(content, structured, serialized.len())
    }

    #[must_use]
    pub fn read(&self, uri: &str) -> Option<Value> {
        let resource = self.resources.get(uri)?;
        Some(serde_json::json!({
            "contents": [{
                "uri": resource.uri,
                "mimeType": resource.mime_type,
                "text": resource.text,
            }]
        }))
    }

    pub fn store_json(&mut self, tool_name: &str, text: &str) -> Value {
        self.store(tool_name, text, "json", "application/json", "structured")
    }

    pub fn store_text(&mut self, tool_name: &str, text: &str) -> Value {
        self.store(tool_name, text, "txt", "text/plain", "text")
    }

    fn store(
        &mut self,
        tool_name: &str,
        text: &str,
        extension: &str,
        mime_type: &'static str,
        kind: &str,
    ) -> Value {
        let uri = format!("vulcan://tool-results/{}.{}", self.next_id, extension);
        self.next_id += 1;
        self.resources.insert(
            uri.clone(),
            McpStoredResource {
                uri: uri.clone(),
                mime_type,
                text: text.to_string(),
            },
        );
        serde_json::json!({
            "type": "resource_link",
            "uri": uri,
            "name": format!("{tool_name}-result.{extension}"),
            "description": format!("Full {kind} result for `{tool_name}`"),
            "mimeType": mime_type,
        })
    }
}

fn wrap_tool_result(structured: Value) -> Value {
    if structured.is_object() {
        structured
    } else {
        serde_json::json!({"result": structured})
    }
}

fn tool_success_content(content: Vec<Value>, structured: Value, serialized_len: usize) -> Value {
    let mut response = serde_json::json!({"isError": false});
    response
        .as_object_mut()
        .expect("tool response is an object")
        .insert("content".to_string(), Value::Array(content));
    if serialized_len <= MCP_STRUCTURED_CONTENT_LIMIT {
        response
            .as_object_mut()
            .expect("tool response is an object")
            .insert("structuredContent".to_string(), structured);
    }
    response
}

fn tool_summary_text(tool_name: &str, structured: &Value) -> String {
    if let Some(path) = structured.get("path").and_then(Value::as_str) {
        return format!("Tool `{tool_name}` completed for `{path}`. Read the linked resource for the full JSON payload.");
    }
    if let Some(query) = structured.get("query").and_then(Value::as_str) {
        return format!("Tool `{tool_name}` completed for query `{query}`. Read the linked resource for the full JSON payload.");
    }
    format!("Tool `{tool_name}` completed. Read the linked resource for the full JSON payload.")
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
    pub live_ref: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct McpSyncDoctorArgs {
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub live_ref: Option<String>,
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
    pub offset: usize,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub include_properties: bool,
    #[serde(default)]
    pub allow_large_results: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpDailyShowArgs {
    #[serde(default)]
    pub date: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpDailyListArgs {
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
    pub offset: usize,
    #[serde(default)]
    pub order: Option<String>,
    #[serde(default)]
    pub include_events: bool,
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
    pub offset: usize,
    #[serde(default)]
    pub order: Option<String>,
    #[serde(default)]
    pub include_events: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTaskListArgs {
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
pub struct McpTaskQueryArgs {
    pub query: String,
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
pub struct McpGraphCommunitiesArgs {
    #[serde(default)]
    pub community: Option<usize>,
    #[serde(default)]
    pub orphans: bool,
    #[serde(default)]
    pub bridges: bool,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSuggestLinksArgs {
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default = "default_suggest_min_score")]
    pub min_score: f64,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub accept: Option<String>,
    #[serde(default)]
    pub reject: Option<String>,
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
    pub confirm: bool,
    #[serde(default)]
    pub preserve_frontmatter: bool,
    #[serde(default)]
    pub check: bool,
    #[serde(default)]
    pub no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpNoteDeleteArgs {
    pub note: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub confirm: bool,
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
pub struct McpConfigShowArgs {
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpToolPackMutationArgs {
    #[serde(default = "default_tool_pack_operation")]
    pub operation: String,
    #[serde(default)]
    pub packs: Vec<String>,
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
fn default_tool_pack_operation() -> String {
    "list".to_string()
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
    fn tool_resource_store_is_session_local_and_preserves_link_contract() {
        let mut first = McpToolResourceStore::default();
        let mut second = McpToolResourceStore::default();
        let json_link = first.store_json("note_get", "{\"content\":1}");
        assert_eq!(json_link["uri"], "vulcan://tool-results/1.json");
        assert_eq!(json_link["name"], "note_get-result.json");
        assert_eq!(
            json_link["description"],
            "Full structured result for `note_get`"
        );
        assert_eq!(json_link["mimeType"], "application/json");
        assert_eq!(
            first.read("vulcan://tool-results/1.json").unwrap()["contents"][0]["text"],
            "{\"content\":1}"
        );
        assert!(second.read("vulcan://tool-results/1.json").is_none());
        let text_link = first.store_text("custom", "large text");
        assert_eq!(text_link["uri"], "vulcan://tool-results/2.txt");
        assert_eq!(text_link["name"], "custom-result.txt");
        assert_eq!(text_link["description"], "Full text result for `custom`");
        assert_eq!(text_link["mimeType"], "text/plain");
        assert_eq!(
            first.read("vulcan://tool-results/2.txt").unwrap()["contents"][0]["text"],
            "large text"
        );
        assert_eq!(
            second.store_json("other", "{}")["uri"],
            "vulcan://tool-results/1.json"
        );
    }

    #[test]
    fn tool_success_responses_preserve_inline_and_resource_limits() {
        let mut store = McpToolResourceStore::default();
        let small = store.success_response("count", json!(3));
        assert_eq!(small["structuredContent"], json!({"result": 3}));
        assert_eq!(small["content"][0]["text"], "{\n  \"result\": 3\n}");

        let large = store.success_response(
            "note_get",
            json!({
                "path": "Large.md",
                "content": "x".repeat(MCP_STRUCTURED_CONTENT_LIMIT),
            }),
        );
        assert!(large.get("structuredContent").is_none());
        assert_eq!(large["content"][0]["text"], "Tool `note_get` completed for `Large.md`. Read the linked resource for the full JSON payload.");
        assert_eq!(large["content"][1]["uri"], "vulcan://tool-results/1.json");
        assert!(store.read("vulcan://tool-results/1.json").is_some());
    }

    #[test]
    fn custom_success_response_preserves_display_text_and_large_links() {
        let mut store = McpToolResourceStore::default();
        let response = store.custom_success_response(
            "custom",
            json!({"query": "large", "value": "x".repeat(MCP_STRUCTURED_CONTENT_LIMIT)}),
            Some(&"y".repeat(MCP_INLINE_TEXT_LIMIT + 1)),
        );
        assert!(response.get("structuredContent").is_none());
        assert_eq!(
            response["content"][0]["text"],
            "`custom` returned text too large to inline; read the linked resource."
        );
        assert_eq!(response["content"][1]["uri"], "vulcan://tool-results/1.txt");
        assert_eq!(
            response["content"][2]["uri"],
            "vulcan://tool-results/2.json"
        );
        assert!(store.read("vulcan://tool-results/1.txt").is_some());
        assert!(store.read("vulcan://tool-results/2.json").is_some());
    }
}
