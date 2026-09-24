//! Transport-neutral MCP request types shared by CLI and daemon adapters.

#![allow(clippy::struct_excessive_bools)]

use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub const MCP_INLINE_TEXT_LIMIT: usize = 4_096;
pub const MCP_PAGE_SIZE: usize = 100;
pub const MCP_RESOURCE_NOT_FOUND: i64 = -32002;
pub const MCP_QUERY_DEFAULT_LIMIT: usize = 50;
const MCP_DAILY_LIST_DEFAULT_LIMIT: usize = 20;

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
}
