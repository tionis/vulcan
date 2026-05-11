use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub(super) const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub(super) const MCP_INLINE_TEXT_LIMIT: usize = 4_096;
pub(super) const MCP_PAGE_SIZE: usize = 100;
pub(super) const MCP_RESOURCE_NOT_FOUND: i64 = -32002;

#[derive(Debug)]
pub(super) enum McpMethodError {
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
    pub(super) fn invalid_params(message: impl Into<String>) -> Self {
        Self::JsonRpc {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }

    pub(super) fn internal(message: impl Into<String>) -> Self {
        Self::JsonRpc {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }

    pub(super) fn method_not_found(message: impl Into<String>) -> Self {
        Self::JsonRpc {
            code: -32601,
            message: message.into(),
            data: None,
        }
    }

    pub(super) fn tool(message: impl Into<String>) -> Self {
        Self::Tool {
            message: message.into(),
            structured: None,
        }
    }
}

#[derive(Debug)]
pub(super) struct McpMethodOutcome {
    pub(super) response: Option<Value>,
    pub(super) emit_list_notifications: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct McpListParams {
    pub(super) cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpToolCallParams {
    pub(super) name: String,
    #[serde(default)]
    pub(super) arguments: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpPromptGetParams {
    pub(super) name: String,
    #[serde(default)]
    pub(super) arguments: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpResourceReadParams {
    pub(super) uri: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpCompletionParams {
    #[serde(rename = "ref")]
    pub(super) reference: McpCompletionReference,
    pub(super) argument: McpCompletionArgument,
    #[serde(default)]
    pub(super) context: McpCompletionContext,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(super) enum McpCompletionReference {
    #[serde(rename = "ref/prompt")]
    Prompt { name: String },
    #[serde(rename = "ref/resource")]
    Resource { uri: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpCompletionArgument {
    pub(super) name: String,
    #[serde(default)]
    pub(super) value: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct McpCompletionContext {
    #[serde(default)]
    pub(super) arguments: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpNoteGetArgs {
    pub(super) note: String,
    #[serde(default)]
    pub(super) mode: Option<String>,
    #[serde(default)]
    pub(super) section_id: Option<String>,
    #[serde(default)]
    pub(super) heading: Option<String>,
    #[serde(default)]
    pub(super) block_ref: Option<String>,
    #[serde(default)]
    pub(super) lines: Option<String>,
    #[serde(rename = "match", default)]
    pub(super) match_pattern: Option<String>,
    #[serde(default)]
    pub(super) context: usize,
    #[serde(default)]
    pub(super) no_frontmatter: bool,
    #[serde(default)]
    pub(super) raw: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpNoteOutlineArgs {
    pub(super) note: String,
    #[serde(default)]
    pub(super) section_id: Option<String>,
    #[serde(default)]
    pub(super) depth: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpSearchArgs {
    pub(super) query: String,
    #[serde(default)]
    pub(super) tag: Option<String>,
    #[serde(default)]
    pub(super) path_prefix: Option<String>,
    #[serde(default)]
    pub(super) has_property: Option<String>,
    #[serde(default)]
    pub(super) filters: Vec<String>,
    #[serde(default)]
    pub(super) mode: Option<String>,
    #[serde(default)]
    pub(super) sort: Option<String>,
    #[serde(default)]
    pub(super) match_case: bool,
    #[serde(default = "crate::mcp::default_search_limit")]
    pub(super) limit: usize,
    #[serde(default = "crate::mcp::default_search_context_size")]
    pub(super) context_size: usize,
    #[serde(default)]
    pub(super) raw_query: bool,
    #[serde(default)]
    pub(super) fuzzy: bool,
    #[serde(default)]
    pub(super) explain: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpQueryArgs {
    #[serde(default)]
    pub(super) query: Option<String>,
    #[serde(default)]
    pub(super) json: Option<String>,
    #[serde(default)]
    pub(super) filters: Vec<String>,
    #[serde(default)]
    pub(super) sort: Option<String>,
    #[serde(default)]
    pub(super) desc: bool,
    #[serde(default)]
    pub(super) engine: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpDailyShowArgs {
    #[serde(default)]
    pub(super) date: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpDailyListArgs {
    #[serde(default)]
    pub(super) from: Option<String>,
    #[serde(default)]
    pub(super) to: Option<String>,
    #[serde(default)]
    pub(super) week: bool,
    #[serde(default)]
    pub(super) month: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpTaskListArgs {
    #[serde(default)]
    pub(super) filter: Option<String>,
    #[serde(default)]
    pub(super) source: Option<String>,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) priority: Option<String>,
    #[serde(default)]
    pub(super) due_before: Option<String>,
    #[serde(default)]
    pub(super) due_after: Option<String>,
    #[serde(default)]
    pub(super) project: Option<String>,
    #[serde(default)]
    pub(super) context: Option<String>,
    #[serde(default)]
    pub(super) group_by: Option<String>,
    #[serde(default)]
    pub(super) sort_by: Option<String>,
    #[serde(default)]
    pub(super) include_archived: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpTaskQueryArgs {
    pub(super) query: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpTaskCreateArgs {
    pub(super) text: String,
    #[serde(default)]
    pub(super) note: Option<String>,
    #[serde(default)]
    pub(super) due: Option<String>,
    #[serde(default)]
    pub(super) priority: Option<String>,
    #[serde(default)]
    pub(super) dry_run: bool,
    #[serde(default)]
    pub(super) no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpTaskCompleteArgs {
    pub(super) task: String,
    #[serde(default)]
    pub(super) date: Option<String>,
    #[serde(default)]
    pub(super) dry_run: bool,
    #[serde(default)]
    pub(super) no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpTaskRescheduleArgs {
    pub(super) task: String,
    pub(super) due: String,
    #[serde(default)]
    pub(super) dry_run: bool,
    #[serde(default)]
    pub(super) no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpGraphCommunitiesArgs {
    #[serde(default)]
    pub(super) community: Option<usize>,
    #[serde(default)]
    pub(super) orphans: bool,
    #[serde(default)]
    pub(super) bridges: bool,
    #[serde(default)]
    pub(super) dry_run: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpSuggestLinksArgs {
    #[serde(default)]
    pub(super) note: Option<String>,
    #[serde(default = "crate::mcp::default_suggest_min_score")]
    pub(super) min_score: f64,
    #[serde(default)]
    pub(super) limit: Option<usize>,
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) accept: Option<String>,
    #[serde(default)]
    pub(super) reject: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpNoteCreateArgs {
    pub(super) path: String,
    #[serde(default)]
    pub(super) body: String,
    #[serde(default)]
    pub(super) template: Option<String>,
    #[serde(default)]
    pub(super) frontmatter: BTreeMap<String, Value>,
    #[serde(default)]
    pub(super) check: bool,
    #[serde(default)]
    pub(super) no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpNoteAppendArgs {
    #[serde(default)]
    pub(super) note: Option<String>,
    pub(super) text: String,
    #[serde(default)]
    pub(super) mode: Option<String>,
    #[serde(default)]
    pub(super) heading: Option<String>,
    #[serde(default)]
    pub(super) periodic: Option<String>,
    #[serde(default)]
    pub(super) date: Option<String>,
    #[serde(default)]
    pub(super) vars: BTreeMap<String, String>,
    #[serde(default)]
    pub(super) check: bool,
    #[serde(default)]
    pub(super) no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpNotePatchArgs {
    pub(super) note: String,
    #[serde(default)]
    pub(super) section_id: Option<String>,
    #[serde(default)]
    pub(super) heading: Option<String>,
    #[serde(default)]
    pub(super) block_ref: Option<String>,
    #[serde(default)]
    pub(super) lines: Option<String>,
    pub(super) find: String,
    pub(super) replace: String,
    #[serde(default)]
    pub(super) all: bool,
    #[serde(default)]
    pub(super) check: bool,
    #[serde(default)]
    pub(super) dry_run: bool,
    #[serde(default)]
    pub(super) no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpNoteInfoArgs {
    pub(super) note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpNoteSetArgs {
    pub(super) note: String,
    pub(super) content: String,
    #[serde(default)]
    pub(super) confirm: bool,
    #[serde(default)]
    pub(super) preserve_frontmatter: bool,
    #[serde(default)]
    pub(super) check: bool,
    #[serde(default)]
    pub(super) no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpNoteDeleteArgs {
    pub(super) note: String,
    #[serde(default)]
    pub(super) dry_run: bool,
    #[serde(default)]
    pub(super) confirm: bool,
    #[serde(default)]
    pub(super) no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpWebSearchArgs {
    pub(super) query: String,
    #[serde(default)]
    pub(super) backend: Option<String>,
    #[serde(default = "crate::mcp::default_web_limit")]
    pub(super) limit: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpWebFetchArgs {
    pub(super) url: String,
    #[serde(default)]
    pub(super) mode: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpConfigShowArgs {
    #[serde(default)]
    pub(super) section: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpConfigSetArgs {
    pub(super) key: String,
    pub(super) value: String,
    #[serde(default)]
    pub(super) dry_run: bool,
    #[serde(default)]
    pub(super) no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpIndexScanArgs {
    #[serde(default)]
    pub(super) full: bool,
    #[serde(default)]
    pub(super) no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct McpToolPackMutationArgs {
    pub(super) packs: Vec<String>,
}
