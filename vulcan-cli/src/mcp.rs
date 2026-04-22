#![allow(clippy::needless_pass_by_value, clippy::struct_excessive_bools)]

use crate::app_config;
use crate::commit::AutoCommitPolicy;
use crate::plugins;
use crate::{
    cli_command_tree, collect_complete_candidates, collect_help_command_topics,
    config_set_changed_files, normalize_note_path, permission_error_to_cli,
    resolve_existing_markdown_target, resolve_existing_note_path, resolve_help_topic,
    run_note_append_command, run_note_create_with_body, run_note_delete_command,
    run_note_get_command, run_note_info_command, run_note_outline_command, run_note_patch_command,
    run_note_set_with_content, run_status_command, run_web_fetch_command, run_web_search_command,
    CliError, McpToolAnnotations, McpToolDefinition, McpToolPackArg, McpToolsReport,
    NoteAppendMode, NoteAppendOptions, NoteAppendPeriodicArg, NoteGetMode, NoteGetOptions,
    NotePatchOptions, OutputFormat, SearchBackendArg, WebFetchMode,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead};
use std::path::Path;
use vulcan_app::notes::resolve_periodic_target as app_resolve_periodic_target;
use vulcan_core::properties::load_note_index;
use vulcan_core::{
    assistant_config_summary, assistant_prompts_root, assistant_skills_root,
    list_assistant_prompts, list_assistant_skills, load_assistant_prompt, load_assistant_skill,
    load_vault_config, read_vault_agents_file, render_assistant_prompt, resolve_permission_profile,
    scan_vault_with_progress, search_vault_with_filter, ConfigPermissionMode, PermissionGuard,
    PermissionMode, PermissionProfile, PluginEvent, ProfilePermissionGuard, ScanMode, ScanSummary,
    SearchQuery, SearchSort, VaultPaths,
};

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_INLINE_TEXT_LIMIT: usize = 4_096;
const MCP_PAGE_SIZE: usize = 100;
const MCP_RESOURCE_NOT_FOUND: i64 = -32002;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum McpToolPack {
    Core,
    Extended,
    Admin,
}

impl McpToolPack {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Extended => "extended",
            Self::Admin => "admin",
        }
    }

    const fn includes(self, required: Self) -> bool {
        matches!(
            (self, required),
            (Self::Admin, _)
                | (Self::Extended, Self::Core | Self::Extended)
                | (Self::Core, Self::Core)
        )
    }
}

impl From<McpToolPackArg> for McpToolPack {
    fn from(value: McpToolPackArg) -> Self {
        match value {
            McpToolPackArg::Core => Self::Core,
            McpToolPackArg::Extended => Self::Extended,
            McpToolPackArg::Admin => Self::Admin,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpToolId {
    NoteGet,
    NoteOutline,
    Search,
    Status,
    NoteCreate,
    NoteAppend,
    NotePatch,
    NoteInfo,
    NoteSet,
    NoteDelete,
    WebSearch,
    WebFetch,
    ConfigShow,
    ConfigSet,
    IndexScan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpVisibilityRequirement {
    Read,
    Write,
    Network,
    Index,
    ConfigRead,
    ConfigWrite,
}

#[derive(Debug, Clone, Copy)]
struct McpToolCatalogEntry {
    id: McpToolId,
    name: &'static str,
    title: &'static str,
    description: &'static str,
    pack: McpToolPack,
    visibility: McpVisibilityRequirement,
    annotations: McpToolAnnotations,
    input_schema: fn() -> Value,
    output_schema: Option<fn() -> Value>,
    examples: &'static [&'static str],
}

#[allow(clippy::fn_params_excessive_bools)]
const fn mcp_annotations(
    read_only_hint: bool,
    destructive_hint: bool,
    idempotent_hint: bool,
    open_world_hint: bool,
) -> McpToolAnnotations {
    McpToolAnnotations {
        read_only_hint,
        destructive_hint,
        idempotent_hint,
        open_world_hint,
    }
}

const MCP_TOOL_CATALOG: &[McpToolCatalogEntry] = &[
    McpToolCatalogEntry {
        id: McpToolId::NoteGet,
        name: "note_get",
        title: "Read Note Content",
        description: "Read one note or markdown file, optionally selecting a section, heading, block, or line range.",
        pack: McpToolPack::Core,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: note_get_input_schema,
        output_schema: Some(note_get_output_schema),
        examples: &[
            "vulcan note get Projects/Alpha --section status@12",
            "vulcan note get Dashboard --mode html",
        ],
    },
    McpToolCatalogEntry {
        id: McpToolId::NoteOutline,
        name: "note_outline",
        title: "Inspect Note Outline",
        description: "Inspect a note's semantic sections and block references for scoped follow-up reads and patches.",
        pack: McpToolPack::Core,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: note_outline_input_schema,
        output_schema: Some(note_outline_output_schema),
        examples: &[
            "vulcan note outline Dashboard",
            "vulcan note outline Dashboard --section dashboard/tasks@9 --depth 1",
        ],
    },
    McpToolCatalogEntry {
        id: McpToolId::Search,
        name: "search",
        title: "Search Vault",
        description: "Run full-text or hybrid search across the vault and return structured hits with snippets and section metadata.",
        pack: McpToolPack::Core,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: search_input_schema,
        output_schema: Some(search_output_schema),
        examples: &[
            "vulcan search meeting",
            "vulcan search release --tag project --limit 5",
        ],
    },
    McpToolCatalogEntry {
        id: McpToolId::Status,
        name: "status",
        title: "Read Vault Status",
        description: "Return a vault overview with note counts, cache size, last scan time, and git status.",
        pack: McpToolPack::Core,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: empty_object_schema,
        output_schema: Some(status_output_schema),
        examples: &["vulcan status --output json"],
    },
    McpToolCatalogEntry {
        id: McpToolId::NoteCreate,
        name: "note_create",
        title: "Create Note",
        description: "Create a new note from explicit body text, optional template, and optional frontmatter properties.",
        pack: McpToolPack::Core,
        visibility: McpVisibilityRequirement::Write,
        annotations: mcp_annotations(false, true, false, false),
        input_schema: note_create_input_schema,
        output_schema: Some(note_create_output_schema),
        examples: &[
            "vulcan note create Inbox/Idea --template daily --frontmatter status=idea",
        ],
    },
    McpToolCatalogEntry {
        id: McpToolId::NoteAppend,
        name: "note_append",
        title: "Append To Note",
        description: "Append text to a note, prepend it, or insert it below a heading; periodic targets are also supported.",
        pack: McpToolPack::Core,
        visibility: McpVisibilityRequirement::Write,
        annotations: mcp_annotations(false, true, false, false),
        input_schema: note_append_input_schema,
        output_schema: Some(note_append_output_schema),
        examples: &[
            "vulcan note append Projects/Alpha \"Shipped\" --after-heading \"## Log\"",
            "vulcan note append \"- Called Alice\" --periodic daily",
        ],
    },
    McpToolCatalogEntry {
        id: McpToolId::NotePatch,
        name: "note_patch",
        title: "Patch Note Text",
        description: "Perform a guarded find-and-replace inside one note or one selected note scope.",
        pack: McpToolPack::Core,
        visibility: McpVisibilityRequirement::Write,
        annotations: mcp_annotations(false, true, false, false),
        input_schema: note_patch_input_schema,
        output_schema: Some(note_patch_output_schema),
        examples: &["vulcan note patch Projects/Alpha --find TODO --replace DONE"],
    },
    McpToolCatalogEntry {
        id: McpToolId::NoteInfo,
        name: "note_info",
        title: "Read Note Metadata",
        description: "Return summary metadata and graph counts for one resolved note.",
        pack: McpToolPack::Extended,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: note_info_input_schema,
        output_schema: Some(note_info_output_schema),
        examples: &["vulcan note info Projects/Alpha"],
    },
    McpToolCatalogEntry {
        id: McpToolId::NoteSet,
        name: "note_set",
        title: "Replace Note Content",
        description: "Replace one note's body content with supplied text, optionally preserving the existing frontmatter block.",
        pack: McpToolPack::Extended,
        visibility: McpVisibilityRequirement::Write,
        annotations: mcp_annotations(false, true, false, false),
        input_schema: note_set_input_schema,
        output_schema: Some(note_set_output_schema),
        examples: &["vulcan note set Projects/Alpha --no-frontmatter < body.md"],
    },
    McpToolCatalogEntry {
        id: McpToolId::NoteDelete,
        name: "note_delete",
        title: "Delete Note",
        description: "Delete one note and report the backlinks that would become unresolved.",
        pack: McpToolPack::Extended,
        visibility: McpVisibilityRequirement::Write,
        annotations: mcp_annotations(false, true, false, false),
        input_schema: note_delete_input_schema,
        output_schema: Some(note_delete_output_schema),
        examples: &["vulcan note delete Projects/Alpha --dry-run"],
    },
    McpToolCatalogEntry {
        id: McpToolId::WebSearch,
        name: "web_search",
        title: "Search The Web",
        description: "Query the configured web search backend and return structured result rows.",
        pack: McpToolPack::Extended,
        visibility: McpVisibilityRequirement::Network,
        annotations: mcp_annotations(true, false, false, true),
        input_schema: web_search_input_schema,
        output_schema: Some(web_search_output_schema),
        examples: &["vulcan web search \"rust async\" --limit 5"],
    },
    McpToolCatalogEntry {
        id: McpToolId::WebFetch,
        name: "web_fetch",
        title: "Fetch URL",
        description: "Fetch one URL as markdown, html, or raw content.",
        pack: McpToolPack::Extended,
        visibility: McpVisibilityRequirement::Network,
        annotations: mcp_annotations(true, false, false, true),
        input_schema: web_fetch_input_schema,
        output_schema: Some(web_fetch_output_schema),
        examples: &["vulcan web fetch https://example.com/article --mode markdown"],
    },
    McpToolCatalogEntry {
        id: McpToolId::ConfigShow,
        name: "config_show",
        title: "Read Effective Config",
        description: "Read the effective Vulcan config, optionally narrowed to one section.",
        pack: McpToolPack::Admin,
        visibility: McpVisibilityRequirement::ConfigRead,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: config_show_input_schema,
        output_schema: Some(config_show_output_schema),
        examples: &["vulcan config show periodic.daily"],
    },
    McpToolCatalogEntry {
        id: McpToolId::ConfigSet,
        name: "config_set",
        title: "Set Config Value",
        description: "Write one shared config value to `.vulcan/config.toml` using the same parser and auto-commit behavior as the CLI.",
        pack: McpToolPack::Admin,
        visibility: McpVisibilityRequirement::ConfigWrite,
        annotations: mcp_annotations(false, true, false, false),
        input_schema: config_set_input_schema,
        output_schema: Some(config_set_output_schema),
        examples: &["vulcan config set periodic.daily.template Templates/Daily"],
    },
    McpToolCatalogEntry {
        id: McpToolId::IndexScan,
        name: "index_scan",
        title: "Scan Vault Index",
        description: "Run an incremental or full vault scan and return the resulting scan summary.",
        pack: McpToolPack::Admin,
        visibility: McpVisibilityRequirement::Index,
        annotations: mcp_annotations(false, false, false, false),
        input_schema: index_scan_input_schema,
        output_schema: Some(index_scan_output_schema),
        examples: &["vulcan index scan --full"],
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct McpServerSnapshot {
    tools: String,
    prompts: String,
    resources: String,
}

#[derive(Debug, Clone)]
struct McpStoredResource {
    uri: String,
    mime_type: String,
    text: String,
}

#[derive(Debug)]
struct McpServerCore {
    paths: VaultPaths,
    selection: vulcan_core::ResolvedPermissionProfile,
    guard: ProfilePermissionGuard,
    tool_pack: McpToolPack,
    stored_resources: BTreeMap<String, McpStoredResource>,
    next_resource_id: u64,
    snapshot: McpServerSnapshot,
}

#[derive(Debug)]
enum McpMethodError {
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
    fn invalid_params(message: impl Into<String>) -> Self {
        Self::JsonRpc {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::JsonRpc {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }

    fn method_not_found(message: impl Into<String>) -> Self {
        Self::JsonRpc {
            code: -32601,
            message: message.into(),
            data: None,
        }
    }

    fn tool(message: impl Into<String>) -> Self {
        Self::Tool {
            message: message.into(),
            structured: None,
        }
    }
}

#[derive(Debug)]
struct McpMethodOutcome {
    response: Option<Value>,
    emit_list_notifications: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct McpListParams {
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpToolCallParams {
    name: String,
    #[serde(default)]
    arguments: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpPromptGetParams {
    name: String,
    #[serde(default)]
    arguments: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpResourceReadParams {
    uri: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpCompletionParams {
    #[serde(rename = "ref")]
    reference: McpCompletionReference,
    argument: McpCompletionArgument,
    #[serde(default)]
    context: McpCompletionContext,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum McpCompletionReference {
    #[serde(rename = "ref/prompt")]
    Prompt { name: String },
    #[serde(rename = "ref/resource")]
    Resource { uri: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpCompletionArgument {
    name: String,
    #[serde(default)]
    value: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct McpCompletionContext {
    #[serde(default)]
    arguments: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpNoteGetArgs {
    note: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    section_id: Option<String>,
    #[serde(default)]
    heading: Option<String>,
    #[serde(default)]
    block_ref: Option<String>,
    #[serde(default)]
    lines: Option<String>,
    #[serde(rename = "match", default)]
    match_pattern: Option<String>,
    #[serde(default)]
    context: usize,
    #[serde(default)]
    no_frontmatter: bool,
    #[serde(default)]
    raw: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpNoteOutlineArgs {
    note: String,
    #[serde(default)]
    section_id: Option<String>,
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpSearchArgs {
    query: String,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    path_prefix: Option<String>,
    #[serde(default)]
    has_property: Option<String>,
    #[serde(default)]
    filters: Vec<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    match_case: bool,
    #[serde(default = "default_search_limit")]
    limit: usize,
    #[serde(default = "default_search_context_size")]
    context_size: usize,
    #[serde(default)]
    raw_query: bool,
    #[serde(default)]
    fuzzy: bool,
    #[serde(default)]
    explain: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpNoteCreateArgs {
    path: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    frontmatter: BTreeMap<String, Value>,
    #[serde(default)]
    check: bool,
    #[serde(default)]
    no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpNoteAppendArgs {
    #[serde(default)]
    note: Option<String>,
    text: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    heading: Option<String>,
    #[serde(default)]
    periodic: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    vars: BTreeMap<String, String>,
    #[serde(default)]
    check: bool,
    #[serde(default)]
    no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpNotePatchArgs {
    note: String,
    #[serde(default)]
    section_id: Option<String>,
    #[serde(default)]
    heading: Option<String>,
    #[serde(default)]
    block_ref: Option<String>,
    #[serde(default)]
    lines: Option<String>,
    find: String,
    replace: String,
    #[serde(default)]
    all: bool,
    #[serde(default)]
    check: bool,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpNoteInfoArgs {
    note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpNoteSetArgs {
    note: String,
    content: String,
    #[serde(default)]
    preserve_frontmatter: bool,
    #[serde(default)]
    check: bool,
    #[serde(default)]
    no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpNoteDeleteArgs {
    note: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpWebSearchArgs {
    query: String,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default = "default_web_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpWebFetchArgs {
    url: String,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpConfigShowArgs {
    #[serde(default)]
    section: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpConfigSetArgs {
    key: String,
    value: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpIndexScanArgs {
    #[serde(default)]
    full: bool,
    #[serde(default)]
    no_commit: bool,
}

pub(crate) fn build_mcp_tool_definitions(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_arg: McpToolPackArg,
) -> Result<McpToolsReport, CliError> {
    let selection =
        resolve_permission_profile(paths, requested_profile).map_err(permission_error_to_cli)?;
    let tool_pack = McpToolPack::from(tool_pack_arg);
    let tools = visible_tool_catalog(tool_pack, &selection.profile)
        .into_iter()
        .map(|tool| McpToolDefinition {
            name: tool.name.to_string(),
            title: tool.title.to_string(),
            description: tool.description.to_string(),
            input_schema: (tool.input_schema)(),
            output_schema: tool.output_schema.map(|schema| schema()),
            annotations: tool.annotations,
            tool_pack: tool.pack.as_str().to_string(),
            examples: tool
                .examples
                .iter()
                .map(|item| (*item).to_string())
                .collect(),
        })
        .collect();

    Ok(McpToolsReport {
        protocol_version: MCP_PROTOCOL_VERSION.to_string(),
        tool_pack: tool_pack.as_str().to_string(),
        tools,
    })
}

pub(crate) fn run_mcp_server(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_arg: McpToolPackArg,
) -> Result<(), CliError> {
    let mut server = McpServerCore::new(paths, requested_profile, tool_pack_arg)?;
    let stdin = io::stdin();

    for line in stdin.lock().lines() {
        let line = line.map_err(CliError::operation)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request = match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => value,
            Err(error) => {
                let response =
                    jsonrpc_error(Value::Null, -32700, format!("Parse error: {error}"), None);
                println!("{}", serde_json::to_string(&response).unwrap_or_default());
                continue;
            }
        };

        for message in server.process_request(request) {
            println!("{}", serde_json::to_string(&message).unwrap_or_default());
        }
    }

    Ok(())
}

impl McpServerCore {
    fn new(
        paths: &VaultPaths,
        requested_profile: Option<&str>,
        tool_pack_arg: McpToolPackArg,
    ) -> Result<Self, CliError> {
        let selection = resolve_permission_profile(paths, requested_profile)
            .map_err(permission_error_to_cli)?;
        let tool_pack = McpToolPack::from(tool_pack_arg);
        let guard = ProfilePermissionGuard::new(paths, selection.clone());
        let snapshot = McpServerSnapshot {
            tools: tool_fingerprint(tool_pack, &selection.profile),
            prompts: prompt_files_fingerprint(paths),
            resources: resource_files_fingerprint(paths),
        };

        Ok(Self {
            paths: paths.clone(),
            selection,
            guard,
            tool_pack,
            stored_resources: BTreeMap::new(),
            next_resource_id: 1,
            snapshot,
        })
    }

    fn process_request(&mut self, request: Value) -> Vec<Value> {
        let Some(request_object) = request.as_object() else {
            return vec![jsonrpc_error(
                Value::Null,
                -32600,
                "Invalid request".to_string(),
                None,
            )];
        };
        if request_object.contains_key("result") || request_object.contains_key("error") {
            return vec![jsonrpc_error(
                request_object.get("id").cloned().unwrap_or(Value::Null),
                -32600,
                "Invalid request".to_string(),
                None,
            )];
        }
        if request.is_array() {
            return vec![jsonrpc_error(
                Value::Null,
                -32600,
                "Batch requests are not supported by the 2025-06-18 MCP baseline".to_string(),
                None,
            )];
        }

        let id = request_object.get("id").cloned().unwrap_or(Value::Null);
        let is_notification = !request_object.contains_key("id");
        let Some(method) = request_object.get("method").and_then(Value::as_str) else {
            if is_notification {
                return Vec::new();
            }
            return vec![jsonrpc_error(
                id,
                -32600,
                "Invalid request".to_string(),
                None,
            )];
        };

        let outcome = match self.handle_method(method, request_object.get("params")) {
            Ok(outcome) => outcome,
            Err(McpMethodError::JsonRpc {
                code,
                message,
                data,
            }) => {
                if is_notification {
                    return Vec::new();
                }
                return vec![jsonrpc_error(id, code, message, data)];
            }
            Err(McpMethodError::Tool {
                message,
                structured,
            }) => {
                if is_notification {
                    return Vec::new();
                }
                return vec![tool_error_response(id, message, structured)];
            }
        };

        let mut messages = Vec::new();
        if outcome.emit_list_notifications {
            messages.extend(self.list_changed_notifications());
        }
        if let Some(response) = outcome.response {
            messages.push(jsonrpc_result(id, response));
        }
        messages
    }

    fn handle_method(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> Result<McpMethodOutcome, McpMethodError> {
        match method {
            "initialize" => Ok(McpMethodOutcome {
                response: Some(Self::initialize_result()),
                emit_list_notifications: false,
            }),
            "ping" => Ok(McpMethodOutcome {
                response: Some(Value::Object(Map::new())),
                emit_list_notifications: true,
            }),
            "notifications/initialized" | "notifications/cancelled" => Ok(McpMethodOutcome {
                response: None,
                emit_list_notifications: false,
            }),
            "tools/list" => {
                let params: McpListParams = parse_method_params(params)?;
                Ok(McpMethodOutcome {
                    response: Some(paginated_result(
                        "tools",
                        self.visible_tools()
                            .into_iter()
                            .map(tool_list_item)
                            .collect::<Vec<_>>(),
                        params.cursor,
                    )?),
                    emit_list_notifications: true,
                })
            }
            "tools/call" => {
                let params: McpToolCallParams = parse_method_params(params)?;
                Ok(McpMethodOutcome {
                    response: Some(self.call_tool(&params.name, &params.arguments)?),
                    emit_list_notifications: true,
                })
            }
            "prompts/list" => {
                let params: McpListParams = parse_method_params(params)?;
                Ok(McpMethodOutcome {
                    response: Some(paginated_result(
                        "prompts",
                        self.visible_prompts()?
                            .into_iter()
                            .map(prompt_list_item)
                            .collect::<Vec<_>>(),
                        params.cursor,
                    )?),
                    emit_list_notifications: true,
                })
            }
            "prompts/get" => {
                let params: McpPromptGetParams = parse_method_params(params)?;
                Ok(McpMethodOutcome {
                    response: Some(self.get_prompt(&params.name, &params.arguments)?),
                    emit_list_notifications: true,
                })
            }
            "resources/list" => {
                let params: McpListParams = parse_method_params(params)?;
                Ok(McpMethodOutcome {
                    response: Some(paginated_result(
                        "resources",
                        self.visible_resources()?,
                        params.cursor,
                    )?),
                    emit_list_notifications: true,
                })
            }
            "resources/templates/list" => {
                let params: McpListParams = parse_method_params(params)?;
                Ok(McpMethodOutcome {
                    response: Some(paginated_result(
                        "resourceTemplates",
                        self.visible_resource_templates(),
                        params.cursor,
                    )?),
                    emit_list_notifications: true,
                })
            }
            "resources/read" => {
                let params: McpResourceReadParams = parse_method_params(params)?;
                Ok(McpMethodOutcome {
                    response: Some(self.read_resource(&params.uri)?),
                    emit_list_notifications: true,
                })
            }
            "completion/complete" => {
                let params: McpCompletionParams = parse_method_params(params)?;
                Ok(McpMethodOutcome {
                    response: Some(self.complete(&params)?),
                    emit_list_notifications: true,
                })
            }
            _ => Err(McpMethodError::method_not_found(format!(
                "Method not found: {method}"
            ))),
        }
    }

    fn initialize_result() -> Value {
        serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": true },
                "resources": { "listChanged": true },
                "prompts": { "listChanged": true },
                "completions": {},
            },
            "serverInfo": {
                "name": "vulcan",
                "version": env!("CARGO_PKG_VERSION"),
            }
        })
    }

    fn visible_tools(&self) -> Vec<&'static McpToolCatalogEntry> {
        visible_tool_catalog(self.tool_pack, &self.selection.profile)
    }

    fn visible_prompts(&self) -> Result<Vec<vulcan_core::AssistantPromptSummary>, McpMethodError> {
        if self.selection.profile.read.is_none() {
            return Ok(Vec::new());
        }
        let prompts = list_assistant_prompts(&self.paths)
            .map_err(|error| McpMethodError::internal(error.to_string()))?;
        Ok(prompts
            .into_iter()
            .filter(|prompt| self.prompt_visible(prompt))
            .collect())
    }

    fn visible_skills(&self) -> Result<Vec<vulcan_core::AssistantSkillSummary>, McpMethodError> {
        if self.selection.profile.read.is_none() {
            return Ok(Vec::new());
        }
        let skills = list_assistant_skills(&self.paths)
            .map_err(|error| McpMethodError::internal(error.to_string()))?;
        Ok(skills
            .into_iter()
            .filter(|skill| self.skill_visible(skill))
            .collect())
    }

    fn prompt_visible(&self, prompt: &vulcan_core::AssistantPromptSummary) -> bool {
        if self.guard.read_filter().path_permission().is_unrestricted()
            && !self.guard.has_policy_hook()
        {
            return true;
        }
        self.guard
            .check_read_path(&self.prompt_relative_path(prompt))
            .is_ok()
    }

    fn skill_visible(&self, skill: &vulcan_core::AssistantSkillSummary) -> bool {
        if self.guard.read_filter().path_permission().is_unrestricted()
            && !self.guard.has_policy_hook()
        {
            return true;
        }
        self.guard
            .check_read_path(&self.skill_relative_path(skill))
            .is_ok()
    }

    fn prompt_relative_path(&self, prompt: &vulcan_core::AssistantPromptSummary) -> String {
        load_vault_config(&self.paths)
            .config
            .assistant
            .prompts_folder
            .join(&prompt.path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn skill_relative_path(&self, skill: &vulcan_core::AssistantSkillSummary) -> String {
        load_vault_config(&self.paths)
            .config
            .assistant
            .skills_folder
            .join(&skill.path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn visible_resources(&self) -> Result<Vec<Value>, McpMethodError> {
        let mut resources = vec![serde_json::json!({
            "uri": "vulcan://help/overview",
            "name": "Help Overview",
            "title": "Vulcan Help Overview",
            "description": "Integrated overview of the Vulcan command surface and built-in help topics.",
            "mimeType": "application/json",
        })];

        if !self.selection.profile.read.is_none() {
            resources.push(serde_json::json!({
                "uri": "vulcan://assistant/prompts/index",
                "name": "Assistant Prompt Index",
                "title": "Vault Prompt Index",
                "description": "Visible prompts loaded from the configured assistant prompts folder.",
                "mimeType": "application/json",
            }));
            resources.push(serde_json::json!({
                "uri": "vulcan://assistant/skills/index",
                "name": "Assistant Skill Index",
                "title": "Vault Skill Index",
                "description": "Visible skills loaded from the configured assistant skills folder.",
                "mimeType": "application/json",
            }));
            if read_vault_agents_file(&self.paths)
                .map_err(|error| McpMethodError::internal(error.to_string()))?
                .is_some()
                && self.can_read_relative_path("AGENTS.md")
            {
                resources.push(serde_json::json!({
                    "uri": "vulcan://assistant/agents",
                    "name": "AGENTS.md",
                    "title": "Vault Agent Instructions",
                    "description": "The vault's root AGENTS.md instructions.",
                    "mimeType": "text/markdown",
                }));
            }
        }

        if self.guard.check_config_read().is_ok() {
            resources.push(serde_json::json!({
                "uri": "vulcan://assistant/config",
                "name": "Assistant Config Summary",
                "title": "Assistant Config Summary",
                "description": "Configured assistant prompt and skill folders for this vault.",
                "mimeType": "application/json",
            }));
        }

        Ok(resources)
    }

    fn visible_resource_templates(&self) -> Vec<Value> {
        let mut templates = vec![serde_json::json!({
            "uriTemplate": "vulcan://help/{topic}",
            "name": "Help Topics",
            "title": "Help Topic Resource",
            "description": "Read one built-in or command help topic as structured JSON.",
            "mimeType": "application/json",
        })];

        if !self.selection.profile.read.is_none() {
            templates.push(serde_json::json!({
                "uriTemplate": "vulcan://assistant/skills/{name}",
                "name": "Assistant Skills",
                "title": "Assistant Skill Resource",
                "description": "Read one visible assistant skill as structured JSON.",
                "mimeType": "application/json",
            }));
        }

        templates
    }

    fn get_prompt(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpMethodError> {
        let prompt = load_assistant_prompt(&self.paths, name)
            .map_err(|error| McpMethodError::invalid_params(error.to_string()))?;
        if !self.prompt_visible(&prompt.summary) {
            return Err(McpMethodError::invalid_params(format!(
                "prompt `{name}` is not available under profile `{}`",
                self.selection.name
            )));
        }
        let rendered = render_assistant_prompt(&prompt, &string_argument_map(arguments))
            .map_err(|error| McpMethodError::invalid_params(error.to_string()))?;
        Ok(serde_json::json!({
            "description": prompt.summary.description,
            "messages": [
                {
                    "role": prompt.summary.role,
                    "content": {
                        "type": "text",
                        "text": rendered,
                    }
                }
            ]
        }))
    }

    fn read_resource(&self, uri: &str) -> Result<Value, McpMethodError> {
        if let Some(stored) = self.stored_resources.get(uri) {
            return Ok(serde_json::json!({
                "contents": [{
                    "uri": stored.uri,
                    "mimeType": stored.mime_type,
                    "text": stored.text,
                }]
            }));
        }

        match uri {
            "vulcan://help/overview" => {
                let report = crate::help_overview();
                return Self::json_resource(uri, &report);
            }
            "vulcan://assistant/prompts/index" => {
                return Self::json_resource(uri, &self.visible_prompts()?);
            }
            "vulcan://assistant/skills/index" => {
                return Self::json_resource(uri, &self.visible_skills()?);
            }
            "vulcan://assistant/config" => {
                self.guard
                    .check_config_read()
                    .map_err(|error| McpMethodError::tool(error.to_string()))?;
                return Self::json_resource(uri, &assistant_config_summary(&self.paths));
            }
            "vulcan://assistant/agents" => {
                if !self.can_read_relative_path("AGENTS.md") {
                    return Err(resource_not_found_error(
                        uri,
                        format!(
                            "permission denied: resource `{uri}` is not available under profile `{}`",
                            self.selection.name
                        ),
                    ));
                }
                let contents = read_vault_agents_file(&self.paths)
                    .map_err(|error| McpMethodError::internal(error.to_string()))?
                    .ok_or_else(|| {
                        resource_not_found_error(uri, "Resource not found".to_string())
                    })?;
                return Ok(serde_json::json!({
                    "contents": [{
                        "uri": uri,
                        "mimeType": "text/markdown",
                        "text": contents,
                    }]
                }));
            }
            _ => {}
        }

        if let Some(topic) = uri.strip_prefix("vulcan://help/") {
            let report = if topic == "overview" {
                crate::help_overview()
            } else {
                let topic_path = topic.split('/').map(ToOwned::to_owned).collect::<Vec<_>>();
                resolve_help_topic(&topic_path)
                    .map_err(|error| resource_not_found_error(uri, error.message))?
            };
            return Self::json_resource(uri, &report);
        }

        if let Some(name) = uri.strip_prefix("vulcan://assistant/skills/") {
            let skill = load_assistant_skill(&self.paths, name)
                .map_err(|error| resource_not_found_error(uri, error.to_string()))?;
            if !self.skill_visible(&skill.summary) {
                return Err(resource_not_found_error(
                    uri,
                    format!(
                        "permission denied: resource `{uri}` is not available under profile `{}`",
                        self.selection.name
                    ),
                ));
            }
            return Self::json_resource(uri, &skill);
        }

        Err(resource_not_found_error(
            uri,
            "Resource not found".to_string(),
        ))
    }

    fn complete(&self, params: &McpCompletionParams) -> Result<Value, McpMethodError> {
        let values = match &params.reference {
            McpCompletionReference::Prompt { name } => {
                let prompt = load_assistant_prompt(&self.paths, name)
                    .map_err(|error| McpMethodError::invalid_params(error.to_string()))?;
                if !self.prompt_visible(&prompt.summary) {
                    return Err(McpMethodError::invalid_params(format!(
                        "prompt `{name}` is not available under profile `{}`",
                        self.selection.name
                    )));
                }
                let argument = prompt
                    .summary
                    .arguments
                    .iter()
                    .find(|argument| argument.name == params.argument.name)
                    .ok_or_else(|| {
                        McpMethodError::invalid_params(format!(
                            "prompt `{name}` does not define argument `{}`",
                            params.argument.name
                        ))
                    })?;
                self.complete_context(
                    argument.completion.as_deref().unwrap_or_default(),
                    &params.argument.value,
                    &params.context.arguments,
                )?
            }
            McpCompletionReference::Resource { uri } if uri == "vulcan://help/{topic}" => {
                if params.argument.name != "topic" {
                    return Err(McpMethodError::invalid_params(format!(
                        "resource template `{uri}` does not define argument `{}`",
                        params.argument.name
                    )));
                }
                help_topic_completion_candidates(&params.argument.value)
            }
            McpCompletionReference::Resource { uri }
                if uri == "vulcan://assistant/skills/{name}" =>
            {
                if params.argument.name != "name" {
                    return Err(McpMethodError::invalid_params(format!(
                        "resource template `{uri}` does not define argument `{}`",
                        params.argument.name
                    )));
                }
                self.visible_skills()?
                    .into_iter()
                    .map(|skill| skill.name)
                    .filter(|skill| skill.starts_with(&params.argument.value))
                    .collect()
            }
            McpCompletionReference::Resource { uri } => {
                return Err(McpMethodError::invalid_params(format!(
                    "unknown completion reference `{uri}`"
                )));
            }
        };

        Ok(serde_json::json!({
            "completion": {
                "values": values,
                "total": values.len(),
                "hasMore": false,
            }
        }))
    }

    fn complete_context(
        &self,
        context: &str,
        prefix: &str,
        _arguments: &BTreeMap<String, String>,
    ) -> Result<Vec<String>, McpMethodError> {
        match context {
            "" => Ok(Vec::new()),
            "note" => Ok(self.visible_note_completion_candidates(prefix)),
            "daily-date" => Ok(self.visible_daily_date_candidates(prefix)?),
            "prompt-name" => Ok(self
                .visible_prompts()?
                .into_iter()
                .map(|prompt| prompt.name)
                .filter(|name| name.starts_with(prefix))
                .collect()),
            "skill-name" => Ok(self
                .visible_skills()?
                .into_iter()
                .map(|skill| skill.name)
                .filter(|name| name.starts_with(prefix))
                .collect()),
            "help-topic" => Ok(help_topic_completion_candidates(prefix)),
            "bases-file" | "bases-view" | "kanban-board" | "vault-path" => Ok(self
                .filter_read_path_candidates(collect_complete_candidates(
                    &self.paths,
                    context,
                    Some(prefix),
                ))),
            "task-view" => Ok(
                self.filter_task_view_candidates(collect_complete_candidates(
                    &self.paths,
                    context,
                    Some(prefix),
                )),
            ),
            "script" => Ok(self.filter_script_candidates(collect_complete_candidates(
                &self.paths,
                context,
                Some(prefix),
            ))),
            other => Ok(collect_complete_candidates(
                &self.paths,
                other,
                Some(prefix),
            )),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn call_tool(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpMethodError> {
        let Some(tool) = tool_by_name(name) else {
            return Err(McpMethodError::invalid_params(format!(
                "Unknown tool: {name}"
            )));
        };
        if !self.tool_pack.includes(tool.pack) {
            return Err(McpMethodError::invalid_params(format!(
                "Unknown tool: {name}"
            )));
        }
        if !tool_visible(tool, &self.selection.profile, self.tool_pack) {
            return Err(McpMethodError::tool(format!(
                "permission denied: tool `{}` requires {} under profile `{}`",
                tool.name,
                visibility_requirement_name(tool.visibility),
                self.selection.name
            )));
        }

        match tool.id {
            McpToolId::NoteGet => {
                let args: McpNoteGetArgs = parse_tool_arguments(arguments)?;
                self.check_read_markdown_source_access(&args.note)
                    .map_err(cli_tool_error)?;
                let report = run_note_get_command(
                    &self.paths,
                    NoteGetOptions {
                        note: &args.note,
                        mode: parse_note_get_mode(args.mode)?,
                        section_id: args.section_id.as_deref(),
                        heading: args.heading.as_deref(),
                        block_ref: args.block_ref.as_deref(),
                        lines: args.lines.as_deref(),
                        match_pattern: args.match_pattern.as_deref(),
                        context: args.context,
                        no_frontmatter: args.no_frontmatter,
                        raw: args.raw,
                    },
                )
                .map_err(cli_tool_error)?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::NoteOutline => {
                let args: McpNoteOutlineArgs = parse_tool_arguments(arguments)?;
                self.check_read_markdown_source_access(&args.note)
                    .map_err(cli_tool_error)?;
                let report = run_note_outline_command(
                    &self.paths,
                    &args.note,
                    args.section_id.as_deref(),
                    args.depth,
                )
                .map_err(cli_tool_error)?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::Search => {
                let args: McpSearchArgs = parse_tool_arguments(arguments)?;
                if args.limit == 0 {
                    return Err(McpMethodError::invalid_params(
                        "`search.limit` must be at least 1",
                    ));
                }
                let report = search_vault_with_filter(
                    &self.paths,
                    &SearchQuery {
                        text: args.query,
                        tag: args.tag,
                        path_prefix: args.path_prefix,
                        has_property: args.has_property,
                        filters: args.filters,
                        provider: None,
                        mode: parse_search_mode(args.mode)?,
                        sort: parse_search_sort(args.sort)?,
                        match_case: args.match_case.then_some(true),
                        limit: Some(args.limit),
                        context_size: args.context_size,
                        raw_query: args.raw_query,
                        fuzzy: args.fuzzy,
                        explain: args.explain,
                    },
                    Some(&self.guard.read_filter()),
                )
                .map_err(|error| McpMethodError::tool(error.to_string()))?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::Status => {
                let report = run_status_command(&self.paths).map_err(cli_tool_error)?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::NoteCreate => {
                let args: McpNoteCreateArgs = parse_tool_arguments(arguments)?;
                let normalized_path = normalize_note_path(&args.path).map_err(cli_tool_error)?;
                self.check_write_path_access(&normalized_path)
                    .map_err(cli_tool_error)?;
                let report = run_note_create_with_body(
                    &self.paths,
                    &normalized_path,
                    args.template.as_deref(),
                    &frontmatter_bindings(&args.frontmatter),
                    &args.body,
                    args.check,
                    Some(self.selection.name.as_str()),
                    OutputFormat::Json,
                    false,
                    true,
                )
                .map_err(cli_tool_error)?;
                AutoCommitPolicy::for_mutation(&self.paths, args.no_commit)
                    .commit(
                        &self.paths,
                        "note-create",
                        &report.changed_paths,
                        Some(self.selection.name.as_str()),
                        true,
                    )
                    .map_err(|error| McpMethodError::tool(error.clone()))?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::NoteAppend => {
                let args: McpNoteAppendArgs = parse_tool_arguments(arguments)?;
                let periodic = parse_periodic_arg(args.periodic)?;
                if args.note.is_some() == periodic.is_some() {
                    return Err(McpMethodError::invalid_params(
                        "`note_append` requires exactly one of `note` or `periodic`",
                    ));
                }
                if let Some(note) = args.note.as_deref() {
                    self.check_write_note_access(note).map_err(cli_tool_error)?;
                } else if let Some(periodic) = periodic {
                    let config = load_vault_config(&self.paths).config;
                    let target = app_resolve_periodic_target(
                        &config.periodic,
                        note_append_periodic_type(periodic),
                        args.date.as_deref(),
                        true,
                    )
                    .map_err(|error| McpMethodError::tool(error.to_string()))?;
                    self.check_write_path_access(&target.path)
                        .map_err(cli_tool_error)?;
                }
                let report = run_note_append_command(
                    &self.paths,
                    NoteAppendOptions {
                        note: args.note.as_deref(),
                        text: &args.text,
                        mode: parse_note_append_mode(args.mode, args.heading.is_some())?,
                        heading: args.heading.as_deref(),
                        periodic,
                        date: args.date.as_deref(),
                        vars: &template_var_bindings(&args.vars),
                        check: args.check,
                    },
                    Some(self.selection.name.as_str()),
                    OutputFormat::Json,
                    false,
                    true,
                )
                .map_err(cli_tool_error)?;
                AutoCommitPolicy::for_mutation(&self.paths, args.no_commit)
                    .commit(
                        &self.paths,
                        "note-append",
                        std::slice::from_ref(&report.path),
                        Some(self.selection.name.as_str()),
                        true,
                    )
                    .map_err(|error| McpMethodError::tool(error.clone()))?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::NotePatch => {
                let args: McpNotePatchArgs = parse_tool_arguments(arguments)?;
                self.check_write_markdown_source_access(&args.note)
                    .map_err(cli_tool_error)?;
                let report = run_note_patch_command(
                    &self.paths,
                    NotePatchOptions {
                        note: &args.note,
                        section_id: args.section_id.as_deref(),
                        heading: args.heading.as_deref(),
                        block_ref: args.block_ref.as_deref(),
                        lines: args.lines.as_deref(),
                        find: &args.find,
                        replace: &args.replace,
                        replace_all: args.all,
                        check: args.check,
                        dry_run: args.dry_run,
                    },
                    Some(self.selection.name.as_str()),
                    OutputFormat::Json,
                    false,
                    true,
                )
                .map_err(cli_tool_error)?;
                if !args.dry_run {
                    AutoCommitPolicy::for_mutation(&self.paths, args.no_commit)
                        .commit(
                            &self.paths,
                            "note-patch",
                            std::slice::from_ref(&report.path),
                            Some(self.selection.name.as_str()),
                            true,
                        )
                        .map_err(|error| McpMethodError::tool(error.clone()))?;
                }
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::NoteInfo => {
                let args: McpNoteInfoArgs = parse_tool_arguments(arguments)?;
                self.check_read_note_access(&args.note)
                    .map_err(cli_tool_error)?;
                let report =
                    run_note_info_command(&self.paths, &args.note).map_err(cli_tool_error)?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::NoteSet => {
                let args: McpNoteSetArgs = parse_tool_arguments(arguments)?;
                self.check_write_note_access(&args.note)
                    .map_err(cli_tool_error)?;
                let report = run_note_set_with_content(
                    &self.paths,
                    &args.note,
                    &args.content,
                    args.preserve_frontmatter,
                    args.check,
                    Some(self.selection.name.as_str()),
                    OutputFormat::Json,
                    false,
                    true,
                )
                .map_err(cli_tool_error)?;
                AutoCommitPolicy::for_mutation(&self.paths, args.no_commit)
                    .commit(
                        &self.paths,
                        "note-set",
                        std::slice::from_ref(&report.path),
                        Some(self.selection.name.as_str()),
                        true,
                    )
                    .map_err(|error| McpMethodError::tool(error.clone()))?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::NoteDelete => {
                let args: McpNoteDeleteArgs = parse_tool_arguments(arguments)?;
                self.check_write_note_access(&args.note)
                    .map_err(cli_tool_error)?;
                let report = run_note_delete_command(
                    &self.paths,
                    &args.note,
                    args.dry_run,
                    Some(self.selection.name.as_str()),
                    OutputFormat::Json,
                    false,
                    true,
                )
                .map_err(cli_tool_error)?;
                if !args.dry_run {
                    AutoCommitPolicy::for_mutation(&self.paths, args.no_commit)
                        .commit(
                            &self.paths,
                            "note-delete",
                            std::slice::from_ref(&report.path),
                            Some(self.selection.name.as_str()),
                            true,
                        )
                        .map_err(|error| McpMethodError::tool(error.clone()))?;
                }
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::WebSearch => {
                let args: McpWebSearchArgs = parse_tool_arguments(arguments)?;
                if args.limit == 0 {
                    return Err(McpMethodError::invalid_params(
                        "`web_search.limit` must be at least 1",
                    ));
                }
                let report = run_web_search_command(
                    &self.paths,
                    &args.query,
                    parse_search_backend(args.backend)?,
                    args.limit,
                    Some(&self.guard),
                )
                .map_err(cli_tool_error)?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::WebFetch => {
                let args: McpWebFetchArgs = parse_tool_arguments(arguments)?;
                let report = run_web_fetch_command(
                    &self.paths,
                    &args.url,
                    parse_web_fetch_mode(args.mode)?,
                    None,
                    Some(&self.guard),
                )
                .map_err(cli_tool_error)?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::ConfigShow => {
                self.guard
                    .check_config_read()
                    .map_err(|error| McpMethodError::tool(error.to_string()))?;
                let args: McpConfigShowArgs = parse_tool_arguments(arguments)?;
                let report = app_config::build_config_show_report(
                    &self.paths,
                    args.section.as_deref(),
                    Some(self.selection.name.as_str()),
                )
                .map_err(cli_tool_error)?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::ConfigSet => {
                self.guard
                    .check_config_write()
                    .map_err(|error| McpMethodError::tool(error.to_string()))?;
                let args: McpConfigSetArgs = parse_tool_arguments(arguments)?;
                let had_gitignore = self.paths.gitignore_file().exists();
                let mut report = app_config::plan_config_set_report(
                    &self.paths,
                    &args.key,
                    &args.value,
                    args.dry_run,
                )
                .map_err(cli_tool_error)?;
                if !args.dry_run && report.updated {
                    report = app_config::apply_config_set_report(&self.paths, report)
                        .map_err(cli_tool_error)?;
                    AutoCommitPolicy::for_mutation(&self.paths, args.no_commit)
                        .commit(
                            &self.paths,
                            "config-set",
                            &config_set_changed_files(&self.paths, had_gitignore),
                            Some(self.selection.name.as_str()),
                            true,
                        )
                        .map_err(|error| McpMethodError::tool(error.clone()))?;
                }
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::IndexScan => {
                self.guard
                    .check_index()
                    .map_err(|error| McpMethodError::tool(error.to_string()))?;
                let args: McpIndexScanArgs = parse_tool_arguments(arguments)?;
                let summary = self.run_index_scan(args.full, args.no_commit)?;
                self.serialize_tool_report(tool.name, &summary)
            }
        }
    }

    fn run_index_scan(&self, full: bool, no_commit: bool) -> Result<ScanSummary, McpMethodError> {
        let auto_commit = AutoCommitPolicy::for_scan(&self.paths, no_commit);
        let summary = scan_vault_with_progress(
            &self.paths,
            if full {
                ScanMode::Full
            } else {
                ScanMode::Incremental
            },
            |_| {},
        )
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
        if summary.added + summary.updated + summary.deleted > 0 {
            auto_commit
                .commit(
                    &self.paths,
                    "scan",
                    &[],
                    Some(self.selection.name.as_str()),
                    true,
                )
                .map_err(|error| McpMethodError::tool(error.clone()))?;
        }
        let _ = plugins::dispatch_plugin_event(
            &self.paths,
            Some(self.selection.name.as_str()),
            PluginEvent::OnScanComplete,
            &serde_json::json!({
                "kind": PluginEvent::OnScanComplete,
                "mode": if full { "full" } else { "incremental" },
                "summary": &summary,
            }),
            true,
        );
        Ok(summary)
    }

    fn serialize_tool_report<T: serde::Serialize>(
        &mut self,
        tool_name: &str,
        report: &T,
    ) -> Result<Value, McpMethodError> {
        let structured = serde_json::to_value(report).map_err(|error| {
            McpMethodError::internal(format!("failed to serialize `{tool_name}` report: {error}"))
        })?;
        Ok(self.tool_success_response(tool_name, structured))
    }

    fn tool_success_response(&mut self, tool_name: &str, structured: Value) -> Value {
        let structured = if structured.is_object() {
            structured
        } else {
            serde_json::json!({ "result": structured })
        };
        let serialized = serde_json::to_string_pretty(&structured).unwrap_or_default();
        let content = if serialized.len() <= MCP_INLINE_TEXT_LIMIT {
            vec![serde_json::json!({
                "type": "text",
                "text": serialized,
            })]
        } else {
            let resource = self.store_tool_result_resource(tool_name, &serialized);
            vec![
                serde_json::json!({
                    "type": "text",
                    "text": tool_summary_text(tool_name, &structured),
                }),
                resource,
            ]
        };
        serde_json::json!({
            "content": content,
            "structuredContent": structured,
            "isError": false,
        })
    }

    fn store_tool_result_resource(&mut self, tool_name: &str, serialized: &str) -> Value {
        let uri = format!("vulcan://tool-results/{}.json", self.next_resource_id);
        self.next_resource_id += 1;
        let name = format!("{tool_name}-result.json");
        let description = format!("Full structured result for `{tool_name}`");
        self.stored_resources.insert(
            uri.clone(),
            McpStoredResource {
                uri: uri.clone(),
                mime_type: "application/json".to_string(),
                text: serialized.to_string(),
            },
        );
        serde_json::json!({
            "type": "resource_link",
            "uri": uri,
            "name": name,
            "description": description,
            "mimeType": "application/json",
        })
    }

    fn json_resource<T: serde::Serialize>(uri: &str, value: &T) -> Result<Value, McpMethodError> {
        let text = serde_json::to_string_pretty(value)
            .map_err(|error| McpMethodError::internal(error.to_string()))?;
        Ok(serde_json::json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": text,
            }]
        }))
    }

    fn list_changed_notifications(&mut self) -> Vec<Value> {
        let current = McpServerSnapshot {
            tools: tool_fingerprint(self.tool_pack, &self.selection.profile),
            prompts: prompt_files_fingerprint(&self.paths),
            resources: resource_files_fingerprint(&self.paths),
        };
        let mut notifications = Vec::new();
        if current.tools != self.snapshot.tools {
            notifications.push(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/tools/list_changed",
            }));
        }
        if current.prompts != self.snapshot.prompts {
            notifications.push(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/prompts/list_changed",
            }));
        }
        if current.resources != self.snapshot.resources {
            notifications.push(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/resources/list_changed",
            }));
        }
        self.snapshot = current;
        notifications
    }

    fn visible_note_completion_candidates(&self, prefix: &str) -> Vec<String> {
        let candidates = collect_complete_candidates(&self.paths, "note", Some(prefix));
        let mut seen = BTreeSet::new();
        candidates
            .into_iter()
            .filter(|candidate| {
                if self.guard.read_filter().path_permission().is_unrestricted()
                    && !self.guard.has_policy_hook()
                {
                    return true;
                }
                resolve_existing_note_path(&self.paths, candidate)
                    .ok()
                    .is_some_and(|path| self.guard.check_read_path(&path).is_ok())
            })
            .filter(|candidate| seen.insert(candidate.clone()))
            .collect()
    }

    fn visible_daily_date_candidates(&self, prefix: &str) -> Result<Vec<String>, McpMethodError> {
        if self.selection.profile.read.is_none() {
            return Ok(Vec::new());
        }
        let mut dates = load_note_index(&self.paths)
            .map_err(|error| McpMethodError::internal(error.to_string()))?
            .into_values()
            .filter(|note| note.periodic_type.as_deref() == Some("daily"))
            .filter(|note| {
                if self.guard.read_filter().path_permission().is_unrestricted()
                    && !self.guard.has_policy_hook()
                {
                    return true;
                }
                self.guard.check_read_path(&note.document_path).is_ok()
            })
            .filter_map(|note| note.periodic_date)
            .collect::<Vec<_>>();
        dates.sort_by(|left, right| right.cmp(left));
        dates.dedup();
        dates.retain(|date| date.starts_with(prefix));
        Ok(dates)
    }

    fn filter_read_path_candidates(&self, candidates: Vec<String>) -> Vec<String> {
        candidates
            .into_iter()
            .filter(|candidate| self.can_read_relative_path(candidate.trim_end_matches('/')))
            .collect()
    }

    fn filter_task_view_candidates(&self, candidates: Vec<String>) -> Vec<String> {
        let config_visible = self.guard.check_config_read().is_ok();
        candidates
            .into_iter()
            .filter(|candidate| {
                if Path::new(candidate)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("base"))
                {
                    return self.can_read_relative_path(candidate);
                }
                config_visible
            })
            .collect()
    }

    fn filter_script_candidates(&self, candidates: Vec<String>) -> Vec<String> {
        if !matches!(self.selection.profile.execute, PermissionMode::Allow) {
            return Vec::new();
        }
        candidates
            .into_iter()
            .filter(|candidate| {
                self.can_read_relative_path(&format!(".vulcan/scripts/{candidate}.js"))
            })
            .collect()
    }

    fn can_read_relative_path(&self, relative_path: &str) -> bool {
        if self.guard.read_filter().path_permission().is_unrestricted()
            && !self.guard.has_policy_hook()
        {
            return true;
        }
        self.guard.check_read_path(relative_path).is_ok()
    }

    fn check_read_note_access(&self, note: &str) -> Result<(), CliError> {
        if self.guard.read_filter().path_permission().is_unrestricted()
            && !self.guard.has_policy_hook()
        {
            return Ok(());
        }
        let resolved =
            vulcan_core::resolve_note_reference(&self.paths, note).map_err(CliError::operation)?;
        self.guard
            .check_read_path(&resolved.path)
            .map_err(CliError::operation)
    }

    fn check_write_note_access(&self, note: &str) -> Result<(), CliError> {
        if self
            .guard
            .write_filter()
            .path_permission()
            .is_unrestricted()
            && !self.guard.has_policy_hook()
        {
            return Ok(());
        }
        let resolved =
            vulcan_core::resolve_note_reference(&self.paths, note).map_err(CliError::operation)?;
        self.guard
            .check_write_path(&resolved.path)
            .map_err(CliError::operation)
    }

    fn check_write_path_access(&self, path: &str) -> Result<(), CliError> {
        if self
            .guard
            .write_filter()
            .path_permission()
            .is_unrestricted()
            && !self.guard.has_policy_hook()
        {
            return Ok(());
        }
        self.guard
            .check_write_path(path)
            .map_err(CliError::operation)
    }

    fn check_read_markdown_source_access(&self, note: &str) -> Result<(), CliError> {
        if self.guard.read_filter().path_permission().is_unrestricted()
            && !self.guard.has_policy_hook()
        {
            return Ok(());
        }
        let target = resolve_existing_markdown_target(&self.paths, note)?;
        let Some(relative_path) = target.vault_relative_path.as_deref() else {
            return Err(CliError::operation(format!(
                "permission profiles cannot read markdown files outside the selected vault root: {}",
                target.display_path
            )));
        };
        self.guard
            .check_read_path(relative_path)
            .map_err(CliError::operation)
    }

    fn check_write_markdown_source_access(&self, note: &str) -> Result<(), CliError> {
        if self
            .guard
            .write_filter()
            .path_permission()
            .is_unrestricted()
            && !self.guard.has_policy_hook()
        {
            return Ok(());
        }
        let target = resolve_existing_markdown_target(&self.paths, note)?;
        let Some(relative_path) = target.vault_relative_path.as_deref() else {
            return Err(CliError::operation(format!(
                "permission profiles cannot write markdown files outside the selected vault root: {}",
                target.display_path
            )));
        };
        self.guard
            .check_write_path(relative_path)
            .map_err(CliError::operation)
    }
}

fn tool_by_name(name: &str) -> Option<&'static McpToolCatalogEntry> {
    MCP_TOOL_CATALOG.iter().find(|tool| tool.name == name)
}

fn visible_tool_catalog(
    tool_pack: McpToolPack,
    profile: &PermissionProfile,
) -> Vec<&'static McpToolCatalogEntry> {
    MCP_TOOL_CATALOG
        .iter()
        .filter(|tool| tool_visible(tool, profile, tool_pack))
        .collect()
}

fn tool_visible(
    tool: &McpToolCatalogEntry,
    profile: &PermissionProfile,
    tool_pack: McpToolPack,
) -> bool {
    if !tool_pack.includes(tool.pack) {
        return false;
    }
    match tool.visibility {
        McpVisibilityRequirement::Read => !profile.read.is_none(),
        McpVisibilityRequirement::Write => !profile.write.is_none(),
        McpVisibilityRequirement::Network => profile.network.is_allowed(),
        McpVisibilityRequirement::Index => matches!(profile.index, PermissionMode::Allow),
        McpVisibilityRequirement::ConfigRead => {
            !matches!(profile.config, ConfigPermissionMode::None)
        }
        McpVisibilityRequirement::ConfigWrite => {
            matches!(profile.config, ConfigPermissionMode::Write)
        }
    }
}

fn tool_list_item(tool: &McpToolCatalogEntry) -> Value {
    serde_json::json!({
        "name": tool.name,
        "title": tool.title,
        "description": tool.description,
        "inputSchema": (tool.input_schema)(),
        "outputSchema": tool.output_schema.map(|schema| schema()),
        "annotations": tool.annotations,
    })
}

fn prompt_list_item(prompt: vulcan_core::AssistantPromptSummary) -> Value {
    serde_json::json!({
        "name": prompt.name,
        "title": prompt.title,
        "description": prompt.description,
        "arguments": prompt.arguments,
    })
}

fn parse_method_params<T: for<'de> Deserialize<'de>>(
    params: Option<&Value>,
) -> Result<T, McpMethodError> {
    serde_json::from_value(params.cloned().unwrap_or_else(|| Value::Object(Map::new())))
        .map_err(|error| McpMethodError::invalid_params(error.to_string()))
}

fn parse_tool_arguments<T: for<'de> Deserialize<'de>>(
    arguments: &Map<String, Value>,
) -> Result<T, McpMethodError> {
    serde_json::from_value(Value::Object(arguments.clone()))
        .map_err(|error| McpMethodError::invalid_params(error.to_string()))
}

fn parse_note_get_mode(mode: Option<String>) -> Result<NoteGetMode, McpMethodError> {
    match mode.as_deref().unwrap_or("markdown") {
        "markdown" => Ok(NoteGetMode::Markdown),
        "html" => Ok(NoteGetMode::Html),
        other => Err(McpMethodError::invalid_params(format!(
            "unsupported `note_get.mode`: {other}"
        ))),
    }
}

fn parse_search_mode(
    mode: Option<String>,
) -> Result<vulcan_core::search::SearchMode, McpMethodError> {
    match mode.as_deref().unwrap_or("keyword") {
        "keyword" => Ok(vulcan_core::search::SearchMode::Keyword),
        "hybrid" => Ok(vulcan_core::search::SearchMode::Hybrid),
        other => Err(McpMethodError::invalid_params(format!(
            "unsupported `search.mode`: {other}"
        ))),
    }
}

fn parse_search_sort(sort: Option<String>) -> Result<Option<SearchSort>, McpMethodError> {
    let value = match sort.as_deref() {
        None => return Ok(None),
        Some("relevance") => SearchSort::Relevance,
        Some("path_asc") => SearchSort::PathAsc,
        Some("path_desc") => SearchSort::PathDesc,
        Some("modified_newest") => SearchSort::ModifiedNewest,
        Some("modified_oldest") => SearchSort::ModifiedOldest,
        Some("created_newest") => SearchSort::CreatedNewest,
        Some("created_oldest") => SearchSort::CreatedOldest,
        Some(other) => {
            return Err(McpMethodError::invalid_params(format!(
                "unsupported `search.sort`: {other}"
            )));
        }
    };
    Ok(Some(value))
}

fn parse_search_backend(
    backend: Option<String>,
) -> Result<Option<SearchBackendArg>, McpMethodError> {
    let Some(backend) = backend else {
        return Ok(None);
    };
    let parsed = match backend.as_str() {
        "auto" => SearchBackendArg::Auto,
        "duckduckgo" => SearchBackendArg::Duckduckgo,
        "kagi" => SearchBackendArg::Kagi,
        "exa" => SearchBackendArg::Exa,
        "tavily" => SearchBackendArg::Tavily,
        "brave" => SearchBackendArg::Brave,
        other => {
            return Err(McpMethodError::invalid_params(format!(
                "unsupported `web_search.backend`: {other}"
            )));
        }
    };
    Ok(Some(parsed))
}

fn parse_web_fetch_mode(mode: Option<String>) -> Result<WebFetchMode, McpMethodError> {
    match mode.as_deref().unwrap_or("markdown") {
        "markdown" => Ok(WebFetchMode::Markdown),
        "html" => Ok(WebFetchMode::Html),
        "raw" => Ok(WebFetchMode::Raw),
        other => Err(McpMethodError::invalid_params(format!(
            "unsupported `web_fetch.mode`: {other}"
        ))),
    }
}

fn parse_note_append_mode(
    mode: Option<String>,
    has_heading: bool,
) -> Result<NoteAppendMode, McpMethodError> {
    match mode.as_deref() {
        None | Some("after_heading") if has_heading => Ok(NoteAppendMode::AfterHeading),
        None | Some("append") => Ok(NoteAppendMode::Append),
        Some("prepend") => Ok(NoteAppendMode::Prepend),
        Some("after_heading") => Err(McpMethodError::invalid_params(
            "`note_append.mode = after_heading` requires `heading`",
        )),
        Some(other) => Err(McpMethodError::invalid_params(format!(
            "unsupported `note_append.mode`: {other}"
        ))),
    }
}

fn parse_periodic_arg(
    value: Option<String>,
) -> Result<Option<NoteAppendPeriodicArg>, McpMethodError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed = match value.as_str() {
        "daily" => NoteAppendPeriodicArg::Daily,
        "weekly" => NoteAppendPeriodicArg::Weekly,
        "monthly" => NoteAppendPeriodicArg::Monthly,
        other => {
            return Err(McpMethodError::invalid_params(format!(
                "unsupported `note_append.periodic`: {other}"
            )));
        }
    };
    Ok(Some(parsed))
}

fn paginated_result(
    key: &str,
    items: Vec<Value>,
    cursor: Option<String>,
) -> Result<Value, McpMethodError> {
    let start = match cursor {
        Some(cursor) if !cursor.is_empty() => cursor.parse::<usize>().map_err(|_| {
            McpMethodError::invalid_params(format!("invalid pagination cursor `{cursor}`"))
        })?,
        _ => 0,
    };
    if start > items.len() {
        return Err(McpMethodError::invalid_params(format!(
            "pagination cursor `{start}` is out of range"
        )));
    }
    let end = usize::min(start + MCP_PAGE_SIZE, items.len());
    let mut result = Map::new();
    result.insert(key.to_string(), Value::Array(items[start..end].to_vec()));
    if end < items.len() {
        result.insert("nextCursor".to_string(), Value::String(end.to_string()));
    }
    Ok(Value::Object(result))
}

fn jsonrpc_result(id: Value, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn jsonrpc_error(id: Value, code: i64, message: String, data: Option<Value>) -> Value {
    let mut error = Map::new();
    error.insert("code".to_string(), Value::Number(code.into()));
    error.insert("message".to_string(), Value::String(message));
    if let Some(data) = data {
        error.insert("data".to_string(), data);
    }
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error,
    })
}

fn tool_error_response(id: Value, message: String, structured: Option<Value>) -> Value {
    let structured = structured.unwrap_or_else(|| serde_json::json!({ "error": message }));
    jsonrpc_result(
        id,
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": message,
            }],
            "structuredContent": structured,
            "isError": true,
        }),
    )
}

fn cli_tool_error(error: CliError) -> McpMethodError {
    McpMethodError::tool(error.message)
}

fn resource_not_found_error(uri: &str, message: String) -> McpMethodError {
    McpMethodError::JsonRpc {
        code: MCP_RESOURCE_NOT_FOUND,
        message,
        data: Some(serde_json::json!({ "uri": uri })),
    }
}

fn string_argument_map(arguments: &Map<String, Value>) -> BTreeMap<String, String> {
    arguments
        .iter()
        .map(|(key, value)| (key.clone(), json_value_to_string(value)))
        .collect()
}

fn json_value_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) => values
            .iter()
            .map(json_value_to_string)
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn frontmatter_bindings(frontmatter: &BTreeMap<String, Value>) -> Vec<String> {
    frontmatter
        .iter()
        .map(|(key, value)| format!("{key}={}", json_value_to_string(value)))
        .collect()
}

fn template_var_bindings(vars: &BTreeMap<String, String>) -> Vec<String> {
    vars.iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

fn default_search_limit() -> usize {
    20
}

fn default_search_context_size() -> usize {
    18
}

fn default_web_limit() -> usize {
    10
}

fn note_append_periodic_type(periodic: NoteAppendPeriodicArg) -> &'static str {
    match periodic {
        NoteAppendPeriodicArg::Daily => "daily",
        NoteAppendPeriodicArg::Weekly => "weekly",
        NoteAppendPeriodicArg::Monthly => "monthly",
    }
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

fn visibility_requirement_name(requirement: McpVisibilityRequirement) -> &'static str {
    match requirement {
        McpVisibilityRequirement::Read => "read access",
        McpVisibilityRequirement::Write => "write access",
        McpVisibilityRequirement::Network => "network access",
        McpVisibilityRequirement::Index => "index access",
        McpVisibilityRequirement::ConfigRead => "config read access",
        McpVisibilityRequirement::ConfigWrite => "config write access",
    }
}

fn tool_fingerprint(tool_pack: McpToolPack, profile: &PermissionProfile) -> String {
    visible_tool_catalog(tool_pack, profile)
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>()
        .join("\n")
}

fn prompt_files_fingerprint(paths: &VaultPaths) -> String {
    path_tree_fingerprint(&assistant_prompts_root(paths))
}

fn resource_files_fingerprint(paths: &VaultPaths) -> String {
    let mut parts = vec![
        path_tree_fingerprint(&assistant_prompts_root(paths)),
        path_tree_fingerprint(&assistant_skills_root(paths)),
        path_tree_fingerprint(&paths.vault_root().join("AGENTS.md")),
        path_tree_fingerprint(paths.config_file()),
        path_tree_fingerprint(&paths.vulcan_dir().join("config.local.toml")),
    ];
    parts.retain(|part| !part.is_empty());
    parts.join("\n--\n")
}

fn path_tree_fingerprint(path: &Path) -> String {
    let mut lines = Vec::new();
    collect_path_tree_fingerprint(path, &mut lines);
    lines.join("\u{1f}")
}

fn collect_path_tree_fingerprint(path: &Path, lines: &mut Vec<String>) {
    if !path.exists() {
        return;
    }
    if path.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        let mut child_paths = entries
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        child_paths.sort();
        for child_path in child_paths {
            collect_path_tree_fingerprint(&child_path, lines);
        }
        return;
    }

    lines.push(path.display().to_string());
    if let Ok(contents) = fs::read(path) {
        lines.push(contents.len().to_string());
        lines.push(String::from_utf8_lossy(&contents).into_owned());
    }
}

fn help_topic_completion_candidates(prefix: &str) -> Vec<String> {
    let mut values = vec!["overview".to_string()];
    values.extend(
        collect_help_command_topics(&cli_command_tree())
            .into_iter()
            .map(|topic| topic.name.replace(' ', "/")),
    );
    values.extend(
        [
            "getting-started",
            "examples",
            "filters",
            "query-dsl",
            "scripting",
            "sandbox",
            "js",
            "js.vault",
            "js.vault.graph",
            "js.vault.note",
            "js.plugins",
            "reports",
        ]
        .into_iter()
        .map(ToOwned::to_owned),
    );
    values.sort();
    values.dedup();
    values.retain(|value| value.starts_with(prefix));
    values
}

fn schema_string(description: &str) -> Value {
    serde_json::json!({ "type": "string", "description": description })
}

fn schema_boolean(description: &str) -> Value {
    serde_json::json!({ "type": "boolean", "description": description })
}

fn schema_integer(description: &str) -> Value {
    serde_json::json!({ "type": "integer", "description": description })
}

fn schema_string_enum(description: &str, values: &[&str]) -> Value {
    serde_json::json!({
        "type": "string",
        "description": description,
        "enum": values,
    })
}

fn schema_array(items: Value, description: &str) -> Value {
    serde_json::json!({
        "type": "array",
        "description": description,
        "items": items,
    })
}

fn schema_object(properties: Vec<(&str, Value)>, required: &[&str]) -> Value {
    let mut props = Map::new();
    for (key, value) in properties {
        props.insert(key.to_string(), value);
    }
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("object".to_string()));
    object.insert("properties".to_string(), Value::Object(props));
    object.insert("additionalProperties".to_string(), Value::Bool(false));
    if !required.is_empty() {
        object.insert(
            "required".to_string(),
            Value::Array(
                required
                    .iter()
                    .map(|item| Value::String((*item).to_string()))
                    .collect(),
            ),
        );
    }
    Value::Object(object)
}

fn empty_object_schema() -> Value {
    schema_object(Vec::new(), &[])
}

fn generic_report_output_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": true,
    })
}

fn note_get_input_schema() -> Value {
    schema_object(
        vec![
            (
                "note",
                schema_string("Note path or note identifier to read."),
            ),
            (
                "mode",
                schema_string_enum(
                    "Render selected content as markdown or HTML.",
                    &["markdown", "html"],
                ),
            ),
            (
                "section_id",
                schema_string("Semantic section id from `note_outline`."),
            ),
            ("heading", schema_string("Heading text to scope the read.")),
            (
                "block_ref",
                schema_string("Block reference id without the leading `^`."),
            ),
            (
                "lines",
                schema_string("Optional 1-based line range inside the current selection."),
            ),
            (
                "match",
                schema_string("Optional regex used to keep matching lines only."),
            ),
            (
                "context",
                schema_integer("Surrounding line count to include around each match."),
            ),
            (
                "no_frontmatter",
                schema_boolean("Strip leading YAML frontmatter from the output."),
            ),
            (
                "raw",
                schema_boolean(
                    "Return only the selected content without line numbers in human output.",
                ),
            ),
        ],
        &["note"],
    )
}

fn note_outline_input_schema() -> Value {
    schema_object(
        vec![
            (
                "note",
                schema_string("Note path or note identifier to outline."),
            ),
            (
                "section_id",
                schema_string("Optional semantic section id to focus the outline."),
            ),
            (
                "depth",
                schema_integer("Optional relative depth limit for descendant sections."),
            ),
        ],
        &["note"],
    )
}

fn search_input_schema() -> Value {
    schema_object(
        vec![
            (
                "query",
                schema_string("Search query string or regex literal."),
            ),
            ("tag", schema_string("Optional tag filter.")),
            (
                "path_prefix",
                schema_string("Optional vault-relative path prefix filter."),
            ),
            (
                "has_property",
                schema_string("Optional property-presence filter."),
            ),
            (
                "filters",
                schema_array(
                    schema_string("Typed property filter."),
                    "Additional typed `--where` filters.",
                ),
            ),
            (
                "mode",
                schema_string_enum("Search mode.", &["keyword", "hybrid"]),
            ),
            (
                "sort",
                schema_string_enum(
                    "Search result sort order.",
                    &[
                        "relevance",
                        "path_asc",
                        "path_desc",
                        "modified_newest",
                        "modified_oldest",
                        "created_newest",
                        "created_oldest",
                    ],
                ),
            ),
            (
                "match_case",
                schema_boolean("Treat the query as case-sensitive."),
            ),
            ("limit", schema_integer("Maximum number of hits to return.")),
            (
                "context_size",
                schema_integer("Context line count to include in snippets."),
            ),
            (
                "raw_query",
                schema_boolean("Disable search query rewriting."),
            ),
            ("fuzzy", schema_boolean("Allow fuzzy expansion fallback.")),
            (
                "explain",
                schema_boolean("Include search explanation details per hit."),
            ),
        ],
        &["query"],
    )
}

fn note_create_input_schema() -> Value {
    schema_object(
        vec![
            ("path", schema_string("Vault-relative note path to create.")),
            (
                "body",
                schema_string("Markdown body to write to the new note."),
            ),
            (
                "template",
                schema_string("Optional named template to render first."),
            ),
            (
                "frontmatter",
                serde_json::json!({
                    "type": "object",
                    "description": "Optional frontmatter properties to merge into the created note.",
                    "additionalProperties": true,
                }),
            ),
            (
                "check",
                schema_boolean("Run non-blocking diagnostics on the resulting note."),
            ),
            (
                "no_commit",
                schema_boolean("Suppress auto-commit for this mutation."),
            ),
        ],
        &["path"],
    )
}

fn note_append_input_schema() -> Value {
    schema_object(
        vec![
            (
                "note",
                schema_string("Target note identifier when appending to an existing note."),
            ),
            (
                "text",
                schema_string("Text to append, prepend, or insert under a heading."),
            ),
            (
                "mode",
                schema_string_enum("Append mode.", &["append", "prepend", "after_heading"]),
            ),
            (
                "heading",
                schema_string("Heading used when `mode = after_heading`."),
            ),
            (
                "periodic",
                schema_string_enum(
                    "Periodic note target to create or append to.",
                    &["daily", "weekly", "monthly"],
                ),
            ),
            (
                "date",
                schema_string("Reference date for periodic note resolution."),
            ),
            (
                "vars",
                serde_json::json!({
                    "type": "object",
                    "description": "QuickAdd-style template variables available inside the appended text.",
                    "additionalProperties": { "type": "string" }
                }),
            ),
            (
                "check",
                schema_boolean("Run non-blocking diagnostics on the resulting note."),
            ),
            (
                "no_commit",
                schema_boolean("Suppress auto-commit for this mutation."),
            ),
        ],
        &["text"],
    )
}

fn note_patch_input_schema() -> Value {
    schema_object(
        vec![
            (
                "note",
                schema_string("Note path or markdown file to patch."),
            ),
            (
                "section_id",
                schema_string("Semantic section id to patch inside."),
            ),
            ("heading", schema_string("Heading text to patch inside.")),
            (
                "block_ref",
                schema_string("Block reference id to patch inside."),
            ),
            (
                "lines",
                schema_string("Optional line range inside the selected scope."),
            ),
            ("find", schema_string("Literal text or regex to find.")),
            ("replace", schema_string("Replacement text.")),
            (
                "all",
                schema_boolean("Replace all matches instead of requiring a single match."),
            ),
            (
                "check",
                schema_boolean("Run non-blocking diagnostics on the resulting note."),
            ),
            (
                "dry_run",
                schema_boolean("Preview the patch without writing the file."),
            ),
            (
                "no_commit",
                schema_boolean("Suppress auto-commit for this mutation."),
            ),
        ],
        &["note", "find", "replace"],
    )
}

fn note_info_input_schema() -> Value {
    schema_object(
        vec![("note", schema_string("Note path or note identifier."))],
        &["note"],
    )
}

fn note_set_input_schema() -> Value {
    schema_object(
        vec![
            ("note", schema_string("Note identifier to replace.")),
            ("content", schema_string("Replacement markdown content.")),
            (
                "preserve_frontmatter",
                schema_boolean("Keep the existing frontmatter block and replace only the body."),
            ),
            (
                "check",
                schema_boolean("Run non-blocking diagnostics on the resulting note."),
            ),
            (
                "no_commit",
                schema_boolean("Suppress auto-commit for this mutation."),
            ),
        ],
        &["note", "content"],
    )
}

fn note_delete_input_schema() -> Value {
    schema_object(
        vec![
            ("note", schema_string("Note identifier to delete.")),
            (
                "dry_run",
                schema_boolean("Preview deletion and backlinks without removing the file."),
            ),
            (
                "no_commit",
                schema_boolean("Suppress auto-commit for this mutation."),
            ),
        ],
        &["note"],
    )
}

fn web_search_input_schema() -> Value {
    schema_object(
        vec![
            ("query", schema_string("Web search query string.")),
            (
                "backend",
                schema_string_enum(
                    "Optional search backend override.",
                    &["auto", "duckduckgo", "kagi", "exa", "tavily", "brave"],
                ),
            ),
            (
                "limit",
                schema_integer("Maximum number of results to return."),
            ),
        ],
        &["query"],
    )
}

fn web_fetch_input_schema() -> Value {
    schema_object(
        vec![
            ("url", schema_string("URL to fetch.")),
            (
                "mode",
                schema_string_enum("Fetch output mode.", &["markdown", "html", "raw"]),
            ),
        ],
        &["url"],
    )
}

fn config_show_input_schema() -> Value {
    schema_object(
        vec![(
            "section",
            schema_string("Optional dotted config section to return."),
        )],
        &[],
    )
}

fn config_set_input_schema() -> Value {
    schema_object(
        vec![
            ("key", schema_string("Dotted config key to write.")),
            ("value", schema_string("Raw CLI-style config value string.")),
            (
                "dry_run",
                schema_boolean("Preview the config change without writing it."),
            ),
            (
                "no_commit",
                schema_boolean("Suppress auto-commit for this mutation."),
            ),
        ],
        &["key", "value"],
    )
}

fn index_scan_input_schema() -> Value {
    schema_object(
        vec![
            (
                "full",
                schema_boolean("Force a full scan instead of incremental reconciliation."),
            ),
            (
                "no_commit",
                schema_boolean("Suppress scan auto-commit behavior."),
            ),
        ],
        &[],
    )
}

fn note_get_output_schema() -> Value {
    schema_object(
        vec![
            ("path", schema_string("Resolved path that was read.")),
            ("content", schema_string("Rendered selected content.")),
            (
                "frontmatter",
                serde_json::json!({ "description": "Parsed frontmatter object when present." }),
            ),
            ("metadata", generic_report_output_schema()),
        ],
        &["path", "content", "metadata"],
    )
}

fn note_outline_output_schema() -> Value {
    generic_report_output_schema()
}

fn search_output_schema() -> Value {
    schema_object(
        vec![
            ("query", schema_string("Effective query string.")),
            (
                "hits",
                schema_array(generic_report_output_schema(), "Structured search hits."),
            ),
        ],
        &["query", "hits"],
    )
}

fn status_output_schema() -> Value {
    schema_object(
        vec![
            ("vault_root", schema_string("Vault root path.")),
            ("note_count", schema_integer("Indexed note count.")),
            (
                "attachment_count",
                schema_integer("Indexed attachment count."),
            ),
            ("cache_bytes", schema_integer("Cache file size in bytes.")),
            (
                "git_dirty",
                schema_boolean("Whether the git working tree is dirty."),
            ),
        ],
        &[
            "vault_root",
            "note_count",
            "attachment_count",
            "cache_bytes",
            "git_dirty",
        ],
    )
}

fn note_create_output_schema() -> Value {
    generic_report_output_schema()
}

fn note_append_output_schema() -> Value {
    generic_report_output_schema()
}

fn note_patch_output_schema() -> Value {
    generic_report_output_schema()
}

fn note_info_output_schema() -> Value {
    generic_report_output_schema()
}

fn note_set_output_schema() -> Value {
    generic_report_output_schema()
}

fn note_delete_output_schema() -> Value {
    generic_report_output_schema()
}

fn web_search_output_schema() -> Value {
    generic_report_output_schema()
}

fn web_fetch_output_schema() -> Value {
    generic_report_output_schema()
}

fn config_show_output_schema() -> Value {
    generic_report_output_schema()
}

fn config_set_output_schema() -> Value {
    generic_report_output_schema()
}

fn index_scan_output_schema() -> Value {
    generic_report_output_schema()
}
