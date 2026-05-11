#![allow(clippy::needless_pass_by_value, clippy::struct_excessive_bools)]

mod schemas;

use crate::app_config;
use crate::commands::periodic::{run_daily_list_command, run_daily_show_command};
use crate::commands::runtime::{run_web_fetch_command, run_web_search_command};
use crate::commands::tasks::{
    run_tasks_complete_command, run_tasks_create_command, run_tasks_list_command,
    run_tasks_query_command, run_tasks_reschedule_command, TasksCreateOptions, TasksListOptions,
};
use crate::commit::AutoCommitPolicy;
use crate::plugins;
use crate::{
    cli_command_tree, collect_complete_candidates, collect_help_command_topics,
    config_set_changed_files, custom_tool_registry_entry, normalize_note_path,
    permission_error_to_cli, resolve_existing_markdown_target, resolve_existing_note_path,
    resolve_help_topic, run_note_append_command, run_note_create_with_body,
    run_note_delete_command, run_note_get_command, run_note_info_command, run_note_outline_command,
    run_note_patch_command, run_note_set_with_content, run_status_command, CliError,
    McpToolAnnotations, McpToolPackArg, McpToolPackModeArg, McpToolsReport, McpTransportArg,
    NoteAppendMode, NoteAppendOptions, NoteAppendPeriodicArg, NoteGetMode, NoteGetOptions,
    NotePatchOptions, OutputFormat, SearchBackendArg, TasksListSourceArg, ToolRegistryEntry,
    WebFetchMode,
};
use schemas::{
    config_set_input_schema, config_set_output_schema, config_show_input_schema,
    config_show_output_schema, daily_list_input_schema, daily_show_input_schema,
    empty_object_schema, generic_report_output_schema, graph_communities_input_schema,
    index_scan_input_schema, index_scan_output_schema, note_append_input_schema,
    note_append_output_schema, note_create_input_schema, note_create_output_schema,
    note_delete_input_schema, note_delete_output_schema, note_get_input_schema,
    note_get_output_schema, note_info_input_schema, note_info_output_schema,
    note_outline_input_schema, note_outline_output_schema, note_patch_input_schema,
    note_patch_output_schema, note_set_input_schema, note_set_output_schema, query_input_schema,
    search_input_schema, search_output_schema, status_output_schema, suggest_links_input_schema,
    task_complete_input_schema, task_create_input_schema, task_list_input_schema,
    task_query_input_schema, task_reschedule_input_schema, tool_pack_mutation_input_schema,
    tool_pack_state_output_schema, web_fetch_input_schema, web_fetch_output_schema,
    web_search_input_schema, web_search_output_schema,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
#[cfg(feature = "oauth")]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;
use ulid::Ulid;
use vulcan_app::notes::resolve_periodic_target as app_resolve_periodic_target;
use vulcan_core::properties::load_note_index;
#[cfg(feature = "oauth")]
use vulcan_core::LocalOAuthUserConfig;
use vulcan_core::{
    accept_link_suggestion, assistant_config_summary, assistant_prompts_root,
    assistant_skills_root, evaluate_dql_with_filter, execute_query_report_with_filter,
    list_assistant_prompts, list_assistant_skills, load_assistant_prompt, load_assistant_skill,
    load_vault_config, query_graph_communities_with_filter, query_notes_with_filter,
    read_vault_agents_file, reject_link_suggestion, render_assistant_prompt,
    resolve_permission_profile, scan_vault_with_progress, search_vault_with_filter, suggest_links,
    watch_vault, ConfigPermissionMode, LinkSuggestionStatus, NoteQuery, PermissionGuard,
    PermissionMode, PermissionProfile, PluginEvent, ProfilePermissionGuard, QueryAst, QueryReport,
    ScanMode, ScanSummary, SearchQuery, SearchSort, VaultPaths, WatchOptions,
};
#[cfg(feature = "oauth")]
use vulcan_core::{
    discover_indieauth_endpoints, exchange_indieauth_code, pkce_s256_challenge, LocalOAuthIssuer,
    LocalOAuthIssuerConfig, OAuthResourceServer, OAuthResourceServerConfig,
};

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_INLINE_TEXT_LIMIT: usize = 4_096;
const MCP_PAGE_SIZE: usize = 100;
const MCP_RESOURCE_NOT_FOUND: i64 = -32002;
const MCP_HTTP_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const MCP_HTTP_POLL_INTERVAL: Duration = Duration::from_millis(250);
pub(crate) const DEFAULT_MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MCP_REQUEST_WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct McpHttpOptions {
    pub bind: String,
    pub endpoint: String,
    pub auth_token: Option<String>,
    #[cfg_attr(not(feature = "oauth"), allow(dead_code))]
    pub public_url: Option<String>,
    pub oauth_issuer: Option<String>,
    pub oauth_audience: Vec<String>,
    pub oauth_jwks_url: Option<String>,
    pub oauth_allowed_sub: Vec<String>,
    pub oauth_allowed_email: Vec<String>,
    pub oauth_local_client_id: Option<String>,
    pub oauth_local_client_secret: Option<String>,
    pub oauth_local_approval_token: Option<String>,
    pub oauth_local_subject: Option<String>,
    pub oauth_local_email: Option<String>,
    pub oauth_dcr: bool,
    pub oauth_dcr_allowed_redirect_host: Vec<String>,
    pub oauth_indieauth_authorization_endpoint: Option<String>,
    pub oauth_indieauth_token_endpoint: Option<String>,
    pub oauth_indieauth_client_id: Option<String>,
    pub oauth_indieauth_redirect_uri: Option<String>,
    pub oauth_indieauth_me: Option<String>,
    pub oauth_local_user: Vec<String>,
    pub request_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpToolPackMode {
    Static,
    Adaptive,
}

impl McpToolPackMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Adaptive => "adaptive",
        }
    }
}

impl From<McpToolPackModeArg> for McpToolPackMode {
    fn from(value: McpToolPackModeArg) -> Self {
        match value {
            McpToolPackModeArg::Static => Self::Static,
            McpToolPackModeArg::Adaptive => Self::Adaptive,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum McpToolPack {
    NotesRead,
    Search,
    Status,
    Custom,
    Daily,
    Tasks,
    NotesWrite,
    NotesManage,
    Web,
    Config,
    Index,
    ToolPacks,
}

impl McpToolPack {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NotesRead => "notes-read",
            Self::Search => "search",
            Self::Status => "status",
            Self::Custom => "custom",
            Self::Daily => "daily",
            Self::Tasks => "tasks",
            Self::NotesWrite => "notes-write",
            Self::NotesManage => "notes-manage",
            Self::Web => "web",
            Self::Config => "config",
            Self::Index => "index",
            Self::ToolPacks => "tool-packs",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::NotesRead => "Read note content and outlines for scoped follow-up work.",
            Self::Search => "Search the vault with structured hits and snippets.",
            Self::Status => "Inspect vault status, cache metadata, and git summary.",
            Self::Custom => "Expose callable vault-defined skill command tools.",
            Self::Daily => {
                "Read daily notes and daily-note ranges with structured periodic metadata."
            }
            Self::Tasks => {
                "Query and mutate Tasks plugin and TaskNotes task workflows with typed operations."
            }
            Self::NotesWrite => "Create notes and apply targeted append/patch mutations.",
            Self::NotesManage => {
                "Read advanced note metadata and perform replace/delete mutations."
            }
            Self::Web => "Use the configured web search and fetch backends.",
            Self::Config => "Read and write effective Vulcan configuration.",
            Self::Index => "Run explicit vault index scans and maintenance refreshes.",
            Self::ToolPacks => {
                "Inspect and mutate the MCP tool-pack selection for the current session."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpToolId {
    NoteGet,
    NoteOutline,
    Search,
    Query,
    Status,
    DailyShow,
    DailyList,
    TaskList,
    TaskQuery,
    TaskCreate,
    TaskComplete,
    TaskReschedule,
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
    GraphCommunities,
    SuggestLinks,
    ToolPackList,
    ToolPackEnable,
    ToolPackDisable,
    ToolPackSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpVisibilityRequirement {
    None,
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
    packs: &'static [McpToolPack],
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

const PACK_NOTES_READ: &[McpToolPack] = &[McpToolPack::NotesRead];
const PACK_SEARCH: &[McpToolPack] = &[McpToolPack::Search];
const PACK_STATUS: &[McpToolPack] = &[McpToolPack::Status];
const PACK_CUSTOM: &[McpToolPack] = &[McpToolPack::Custom];
const PACK_DAILY: &[McpToolPack] = &[McpToolPack::Daily];
const PACK_TASKS: &[McpToolPack] = &[McpToolPack::Tasks];
const PACK_NOTES_WRITE: &[McpToolPack] = &[McpToolPack::NotesWrite];
const PACK_NOTES_READ_WRITE: &[McpToolPack] = &[McpToolPack::NotesRead, McpToolPack::NotesWrite];
const PACK_NOTES_MANAGE: &[McpToolPack] = &[McpToolPack::NotesManage];
const PACK_WEB: &[McpToolPack] = &[McpToolPack::Web];
const PACK_CONFIG: &[McpToolPack] = &[McpToolPack::Config];
const PACK_INDEX: &[McpToolPack] = &[McpToolPack::Index];
const PACK_TOOL_PACKS: &[McpToolPack] = &[McpToolPack::ToolPacks];

const MCP_TOOL_CATALOG: &[McpToolCatalogEntry] = &[
    McpToolCatalogEntry {
        id: McpToolId::NoteGet,
        name: "note_get",
        title: "Read Note Content",
        description: "Read one note or markdown file, optionally selecting a section, heading, block, or line range.",
        packs: PACK_NOTES_READ,
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
        packs: PACK_NOTES_READ,
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
        packs: PACK_SEARCH,
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
        id: McpToolId::Query,
        name: "query",
        title: "Query Vault",
        description: "Run the structured Vulcan query surface. Use this for property, tag, path, and DQL-style note queries instead of raw full-text search.",
        packs: PACK_SEARCH,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: query_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &[
            "query {\"filters\":[\"status = active\"],\"sort\":\"file.path\"}",
            "query {\"query\":\"TABLE file.link, status WHERE status = \\\"active\\\"\",\"engine\":\"dql\"}",
        ],
    },
    McpToolCatalogEntry {
        id: McpToolId::Status,
        name: "status",
        title: "Read Vault Status",
        description: "Return a vault overview with note counts, cache size, last scan time, and git status.",
        packs: PACK_STATUS,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: empty_object_schema,
        output_schema: Some(status_output_schema),
        examples: &["vulcan status --output json"],
    },
    McpToolCatalogEntry {
        id: McpToolId::DailyShow,
        name: "daily_show",
        title: "Show Daily Note",
        description: "Read one daily note with its resolved periodic metadata and structured schedule events. Use this before generic note reads for daily-routine questions.",
        packs: PACK_DAILY,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: daily_show_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["daily_show {\"date\":\"today\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::DailyList,
        name: "daily_list",
        title: "List Daily Notes",
        description: "List daily notes in a date window with event counts and extracted schedule events.",
        packs: PACK_DAILY,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: daily_list_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["daily_list {\"week\":true}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::GraphCommunities,
        name: "graph_communities",
        title: "Inspect Graph Communities",
        description: "Compute note-graph communities, orphan placement hints, and bridge notes.",
        packs: PACK_NOTES_READ,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(true, false, false, false),
        input_schema: graph_communities_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["vulcan graph communities --output json"],
    },
    McpToolCatalogEntry {
        id: McpToolId::SuggestLinks,
        name: "suggest_links",
        title: "Suggest Links",
        description: "Read ranked link suggestions, or accept/reject one suggestion when write permissions are available.",
        packs: PACK_NOTES_READ_WRITE,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(false, false, false, false),
        input_schema: suggest_links_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["vulcan suggest links --output json"],
    },
    McpToolCatalogEntry {
        id: McpToolId::TaskList,
        name: "task_list",
        title: "List Tasks",
        description: "List open or filtered tasks through Vulcan's task model. Use this for task summaries instead of raw note search.",
        packs: PACK_TASKS,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(true, false, false, false),
        input_schema: task_list_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["task_list {\"status\":\"open\",\"due_before\":\"2026-05-15\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::TaskQuery,
        name: "task_query",
        title: "Query Tasks",
        description: "Run a Tasks plugin query source and return shaped task results. Use this when the user gives task-query semantics directly.",
        packs: PACK_TASKS,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(true, false, false, false),
        input_schema: task_query_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["task_query {\"query\":\"not done\\ndue before tomorrow\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::TaskCreate,
        name: "task_create",
        title: "Create Task",
        description: "Create a task using the configured task system. Prefer this over raw note edits for new tasks.",
        packs: PACK_TASKS,
        visibility: McpVisibilityRequirement::Write,
        annotations: mcp_annotations(false, true, false, false),
        input_schema: task_create_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["task_create {\"text\":\"Call Alex\",\"due\":\"tomorrow\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::TaskComplete,
        name: "task_complete",
        title: "Complete Task",
        description: "Mark one resolved task complete using task-aware mutation rules. Prefer this over raw note patching for task completion.",
        packs: PACK_TASKS,
        visibility: McpVisibilityRequirement::Write,
        annotations: mcp_annotations(false, true, false, false),
        input_schema: task_complete_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["task_complete {\"task\":\"Tasks/Call Alex\",\"date\":\"today\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::TaskReschedule,
        name: "task_reschedule",
        title: "Reschedule Task",
        description: "Update a task due date using task-aware mutation rules. Prefer this over raw note patching for due-date changes.",
        packs: PACK_TASKS,
        visibility: McpVisibilityRequirement::Write,
        annotations: mcp_annotations(false, true, false, false),
        input_schema: task_reschedule_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["task_reschedule {\"task\":\"Tasks/Call Alex\",\"due\":\"2026-05-09\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::NoteCreate,
        name: "note_create",
        title: "Create Note",
        description: "Create a new note from explicit body text, optional template, and optional frontmatter properties.",
        packs: PACK_NOTES_WRITE,
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
        packs: PACK_NOTES_WRITE,
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
        packs: PACK_NOTES_WRITE,
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
        packs: PACK_NOTES_MANAGE,
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
        packs: PACK_NOTES_MANAGE,
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
        packs: PACK_NOTES_MANAGE,
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
        packs: PACK_WEB,
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
        packs: PACK_WEB,
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
        packs: PACK_CONFIG,
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
        packs: PACK_CONFIG,
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
        packs: PACK_INDEX,
        visibility: McpVisibilityRequirement::Index,
        annotations: mcp_annotations(false, false, false, false),
        input_schema: index_scan_input_schema,
        output_schema: Some(index_scan_output_schema),
        examples: &["vulcan index scan --full"],
    },
    McpToolCatalogEntry {
        id: McpToolId::ToolPackList,
        name: "tool_pack_list",
        title: "List MCP Tool Packs",
        description: "Inspect the available MCP tool packs and the current session's selected pack set.",
        packs: PACK_TOOL_PACKS,
        visibility: McpVisibilityRequirement::None,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: empty_object_schema,
        output_schema: Some(tool_pack_state_output_schema),
        examples: &["tool_pack_list"],
    },
    McpToolCatalogEntry {
        id: McpToolId::ToolPackEnable,
        name: "tool_pack_enable",
        title: "Enable MCP Tool Packs",
        description: "Enable one or more MCP tool packs for the current session and refresh the visible tool list.",
        packs: PACK_TOOL_PACKS,
        visibility: McpVisibilityRequirement::None,
        annotations: mcp_annotations(false, false, true, false),
        input_schema: tool_pack_mutation_input_schema,
        output_schema: Some(tool_pack_state_output_schema),
        examples: &["tool_pack_enable {\"packs\":[\"web\",\"notes-manage\"]}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::ToolPackDisable,
        name: "tool_pack_disable",
        title: "Disable MCP Tool Packs",
        description: "Disable one or more MCP tool packs for the current session and refresh the visible tool list.",
        packs: PACK_TOOL_PACKS,
        visibility: McpVisibilityRequirement::None,
        annotations: mcp_annotations(false, false, true, false),
        input_schema: tool_pack_mutation_input_schema,
        output_schema: Some(tool_pack_state_output_schema),
        examples: &["tool_pack_disable {\"packs\":[\"web\"]}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::ToolPackSet,
        name: "tool_pack_set",
        title: "Set MCP Tool Packs",
        description: "Replace the current session's selected MCP tool packs in one call and refresh the visible tool list.",
        packs: PACK_TOOL_PACKS,
        visibility: McpVisibilityRequirement::None,
        annotations: mcp_annotations(false, false, true, false),
        input_schema: tool_pack_mutation_input_schema,
        output_schema: Some(tool_pack_state_output_schema),
        examples: &["tool_pack_set {\"packs\":[\"notes-read\",\"search\"]}"],
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

#[derive(Debug, Clone)]
struct McpServerCore {
    paths: VaultPaths,
    selection: vulcan_core::ResolvedPermissionProfile,
    guard: ProfilePermissionGuard,
    tool_pack_mode: McpToolPackMode,
    selected_tool_packs: BTreeSet<McpToolPack>,
    stored_resources: BTreeMap<String, McpStoredResource>,
    next_resource_id: u64,
    snapshot: McpServerSnapshot,
}

#[derive(Debug)]
struct McpHttpSession {
    core: Mutex<McpServerCore>,
    subscribers: Mutex<Vec<mpsc::Sender<Value>>>,
    closed: AtomicBool,
}

impl McpHttpSession {
    fn new(core: McpServerCore) -> Self {
        Self {
            core: Mutex::new(core),
            subscribers: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        }
    }

    fn register_subscriber(&self) -> mpsc::Receiver<Value> {
        let (tx, rx) = mpsc::channel();
        self.subscribers
            .lock()
            .expect("mcp subscribers lock should not be poisoned")
            .push(tx);
        rx
    }

    fn broadcast(&self, messages: &[Value]) {
        if messages.is_empty() || self.closed.load(Ordering::SeqCst) {
            return;
        }
        let mut subscribers = self
            .subscribers
            .lock()
            .expect("mcp subscribers lock should not be poisoned");
        subscribers.retain(|sender| {
            messages
                .iter()
                .all(|message| sender.send(message.clone()).is_ok())
        });
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.subscribers
            .lock()
            .expect("mcp subscribers lock should not be poisoned")
            .clear();
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
struct McpHttpRequest {
    method: String,
    path: String,
    #[cfg_attr(not(feature = "oauth"), allow(dead_code))]
    query: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug)]
struct McpHttpResponse {
    status: u16,
    content_type: Option<&'static str>,
    body: Vec<u8>,
    extra_headers: Vec<(String, String)>,
}

#[cfg(feature = "oauth")]
#[derive(Debug, Clone)]
enum McpOAuthMode {
    External(Arc<OAuthResourceServer>),
    Local(Arc<LocalOAuthIssuer>),
}

#[cfg(feature = "oauth")]
#[derive(Debug, Clone)]
struct LocalOAuthCode {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    subject: String,
    email: Option<String>,
    permission_profile: Option<String>,
    expires_at: std::time::Instant,
}

#[cfg(feature = "oauth")]
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
struct LocalOAuthRegisteredClient {
    client_id: String,
    client_secret: String,
    redirect_uris: Vec<String>,
    client_name: Option<String>,
    token_endpoint_auth_method: String,
    client_id_issued_at: u64,
}

#[cfg(feature = "oauth")]
#[derive(Debug, Clone)]
struct LocalOAuthPendingIndieAuth {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    indieauth_code_verifier: String,
    state: Option<String>,
    expires_at: std::time::Instant,
}

#[cfg(feature = "oauth")]
#[derive(Debug, Clone)]
struct LocalOAuthIndieAuthConfig {
    authorization_endpoint: String,
    token_endpoint: String,
    client_id: String,
    redirect_uri: String,
    me: Option<String>,
}

#[derive(Debug)]
struct McpHttpProcessResult {
    response: Option<Value>,
    notifications: Vec<Value>,
    accepted_notification: bool,
}

#[derive(Debug, Clone)]
struct McpHttpServerContext {
    paths: VaultPaths,
    requested_profile: Option<String>,
    tool_pack_args: Vec<McpToolPackArg>,
    tool_pack_mode_arg: McpToolPackModeArg,
    endpoint: String,
    auth_token: Option<String>,
    #[cfg(feature = "oauth")]
    oauth: Option<McpOAuthMode>,
    bind_addr: SocketAddr,
    sessions: Arc<Mutex<BTreeMap<String, Arc<McpHttpSession>>>>,
    #[cfg(feature = "oauth")]
    oauth_codes: Arc<Mutex<BTreeMap<String, LocalOAuthCode>>>,
    #[cfg(feature = "oauth")]
    oauth_clients: Arc<Mutex<BTreeMap<String, LocalOAuthRegisteredClient>>>,
    #[cfg(feature = "oauth")]
    oauth_pending_indieauth: Arc<Mutex<BTreeMap<String, LocalOAuthPendingIndieAuth>>>,
    #[cfg(feature = "oauth")]
    oauth_dcr_enabled: bool,
    #[cfg(feature = "oauth")]
    oauth_dcr_allowed_redirect_hosts: Vec<String>,
    #[cfg(feature = "oauth")]
    oauth_indieauth: Option<LocalOAuthIndieAuthConfig>,
    #[cfg(feature = "oauth")]
    oauth_clients_path: Option<std::path::PathBuf>,
    request_timeout: Duration,
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
struct McpQueryArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    json: Option<String>,
    #[serde(default)]
    filters: Vec<String>,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    desc: bool,
    #[serde(default)]
    engine: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpDailyShowArgs {
    #[serde(default)]
    date: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpDailyListArgs {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    week: bool,
    #[serde(default)]
    month: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpTaskListArgs {
    #[serde(default)]
    filter: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    due_before: Option<String>,
    #[serde(default)]
    due_after: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    group_by: Option<String>,
    #[serde(default)]
    sort_by: Option<String>,
    #[serde(default)]
    include_archived: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpTaskQueryArgs {
    query: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpTaskCreateArgs {
    text: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    due: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpTaskCompleteArgs {
    task: String,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpTaskRescheduleArgs {
    task: String,
    due: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    no_commit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpGraphCommunitiesArgs {
    #[serde(default)]
    community: Option<usize>,
    #[serde(default)]
    orphans: bool,
    #[serde(default)]
    bridges: bool,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpSuggestLinksArgs {
    #[serde(default)]
    note: Option<String>,
    #[serde(default = "default_suggest_min_score")]
    min_score: f64,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    accept: Option<String>,
    #[serde(default)]
    reject: Option<String>,
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
    confirm: bool,
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
    confirm: bool,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpToolPackMutationArgs {
    packs: Vec<String>,
}

pub(crate) fn build_mcp_tool_definitions(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode_arg: McpToolPackModeArg,
) -> Result<McpToolsReport, CliError> {
    let tool_pack_mode = McpToolPackMode::from(tool_pack_mode_arg);
    let selected_tool_packs = resolve_selected_tool_packs(tool_pack_args, tool_pack_mode);
    let tools = build_mcp_tool_registry_entries(paths, requested_profile, &selected_tool_packs)?
        .into_iter()
        .map(|tool| tool.to_mcp_definition())
        .collect::<Vec<_>>();

    Ok(McpToolsReport {
        protocol_version: MCP_PROTOCOL_VERSION.to_string(),
        tool_pack_mode: tool_pack_mode.as_str().to_string(),
        selected_tool_packs: pack_name_list(&selected_tool_packs),
        tools,
    })
}

pub(crate) fn build_openai_tool_registry_entries(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode_arg: McpToolPackModeArg,
) -> Result<Vec<ToolRegistryEntry>, CliError> {
    let tool_pack_mode = McpToolPackMode::from(tool_pack_mode_arg);
    let selected_tool_packs =
        if tool_pack_args.is_empty() || is_default_tool_pack_args(tool_pack_args) {
            default_openai_tool_packs()
        } else {
            resolve_selected_tool_packs(tool_pack_args, tool_pack_mode)
        };
    build_mcp_tool_registry_entries(paths, requested_profile, &selected_tool_packs)
}

pub(crate) fn run_mcp(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode_arg: McpToolPackModeArg,
    transport_arg: McpTransportArg,
    http_options: &McpHttpOptions,
) -> Result<(), CliError> {
    match transport_arg {
        McpTransportArg::Stdio => run_mcp_stdio_server(
            paths,
            requested_profile,
            tool_pack_args,
            tool_pack_mode_arg,
            http_options.request_timeout,
        ),
        McpTransportArg::Http => run_mcp_http_server(
            paths,
            requested_profile,
            tool_pack_args,
            tool_pack_mode_arg,
            http_options,
        ),
    }
}

fn run_mcp_stdio_server(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode_arg: McpToolPackModeArg,
    request_timeout: Duration,
) -> Result<(), CliError> {
    let mut server =
        McpServerCore::new(paths, requested_profile, tool_pack_args, tool_pack_mode_arg)?;
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

        for message in server.process_request_with_timeout(request, request_timeout) {
            println!("{}", serde_json::to_string(&message).unwrap_or_default());
        }
    }

    Ok(())
}

fn run_mcp_http_server(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode_arg: McpToolPackModeArg,
    options: &McpHttpOptions,
) -> Result<(), CliError> {
    #[cfg(feature = "oauth")]
    let oauth = build_mcp_oauth_validator(paths, options)?;
    #[cfg(not(feature = "oauth"))]
    reject_mcp_oauth_options_when_disabled(options)?;
    #[cfg(feature = "oauth")]
    let auth_enabled = options.auth_token.is_some() || oauth.is_some();
    #[cfg(not(feature = "oauth"))]
    let auth_enabled = options.auth_token.is_some();
    let bind_addr = parse_mcp_http_bind_addr(&options.bind, auth_enabled)?;
    let endpoint = normalize_mcp_http_endpoint(&options.endpoint);
    let listener = TcpListener::bind(bind_addr).map_err(CliError::operation)?;
    listener
        .set_nonblocking(true)
        .map_err(CliError::operation)?;
    let addr = listener.local_addr().map_err(CliError::operation)?;
    eprintln!("MCP HTTP server listening on http://{addr}{endpoint}");
    spawn_mcp_index_watcher(paths.clone(), WatchOptions::default());
    let context = McpHttpServerContext {
        paths: paths.clone(),
        requested_profile: requested_profile.map(ToOwned::to_owned),
        tool_pack_args: tool_pack_args.to_vec(),
        tool_pack_mode_arg,
        endpoint,
        auth_token: options.auth_token.clone(),
        #[cfg(feature = "oauth")]
        oauth,
        bind_addr: addr,
        sessions: Arc::new(Mutex::new(BTreeMap::new())),
        #[cfg(feature = "oauth")]
        oauth_codes: Arc::new(Mutex::new(BTreeMap::new())),
        #[cfg(feature = "oauth")]
        oauth_clients: Arc::new(Mutex::new(load_oauth_registered_clients(paths)?)),
        #[cfg(feature = "oauth")]
        oauth_pending_indieauth: Arc::new(Mutex::new(BTreeMap::new())),
        #[cfg(feature = "oauth")]
        oauth_dcr_enabled: options.oauth_dcr,
        #[cfg(feature = "oauth")]
        oauth_dcr_allowed_redirect_hosts: if options.oauth_dcr_allowed_redirect_host.is_empty() {
            vec!["chatgpt.com".to_string()]
        } else {
            options.oauth_dcr_allowed_redirect_host.clone()
        },
        #[cfg(feature = "oauth")]
        oauth_indieauth: build_indieauth_config(options)?,
        #[cfg(feature = "oauth")]
        oauth_clients_path: Some(oauth_clients_path(paths)),
        request_timeout: options.request_timeout,
    };

    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let context = context.clone();
                thread::spawn(move || {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    if let Err(error) = handle_mcp_http_connection(&context, &mut stream) {
                        let response =
                            mcp_http_json_error_response(500, error.to_string(), Value::Null);
                        let _ = write_mcp_http_response(&mut stream, &response);
                    }
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(CliError::operation(error)),
        }
    }
}

fn spawn_mcp_index_watcher(paths: VaultPaths, options: WatchOptions) {
    thread::spawn(move || {
        if let Err(error) = watch_vault(&paths, &options, |report| -> Result<(), String> {
            if report.startup {
                eprintln!(
                    "MCP index watcher initialized: {} added, {} updated, {} unchanged, {} deleted",
                    report.summary.added,
                    report.summary.updated,
                    report.summary.unchanged,
                    report.summary.deleted
                );
            } else if report.summary.added + report.summary.updated + report.summary.deleted > 0 {
                eprintln!(
                    "MCP index watcher refreshed {} paths: {} added, {} updated, {} deleted",
                    report.paths.len(),
                    report.summary.added,
                    report.summary.updated,
                    report.summary.deleted
                );
            }
            Ok(())
        }) {
            eprintln!("MCP index watcher stopped: {error}");
        }
    });
}

fn handle_mcp_http_connection(
    context: &McpHttpServerContext,
    stream: &mut TcpStream,
) -> Result<(), CliError> {
    let request = match read_mcp_http_request(stream) {
        Ok(request) => request,
        Err(error) => {
            let response = mcp_http_json_error_response(400, error, Value::Null);
            write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
            return Ok(());
        }
    };

    #[cfg(feature = "oauth")]
    {
        if let Some(response) = handle_mcp_oauth_metadata(context, &request) {
            write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
            return Ok(());
        }
    }

    if request.path != context.endpoint {
        let response = mcp_http_json_error_response(404, "Not Found", Value::Null);
        write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        return Ok(());
    }
    if let Some(response) = validate_mcp_http_security(context, &request) {
        write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        return Ok(());
    }

    match request.method.as_str() {
        "POST" => {
            let response = handle_mcp_http_post(context, &request);
            write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        }
        "GET" => handle_mcp_http_sse(context, &request, stream)?,
        "DELETE" => {
            let response = handle_mcp_http_delete(context, &request);
            write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        }
        _ => {
            let response = mcp_http_json_error_response(405, "Method Not Allowed", Value::Null);
            write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        }
    }

    Ok(())
}

fn handle_mcp_http_post(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
) -> McpHttpResponse {
    if let Some(response) = validate_mcp_http_post_headers(request) {
        return response;
    }
    let payload = match parse_mcp_http_json_body(request) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    if let Some(response) = validate_mcp_protocol_version(request) {
        return response;
    }
    let (session_id, session, created_session) =
        match resolve_mcp_http_session(context, request, &payload) {
            Ok(session) => session,
            Err(response) => return response,
        };

    let result = {
        let mut core = session
            .core
            .lock()
            .expect("mcp core lock should not be poisoned");
        match core.process_http_request_with_timeout(payload.clone(), context.request_timeout) {
            Ok(result) => result,
            Err(error_response) => {
                if created_session {
                    context
                        .sessions
                        .lock()
                        .expect("mcp sessions lock should not be poisoned")
                        .remove(&session_id);
                }
                return McpHttpResponse {
                    status: 400,
                    content_type: Some("application/json"),
                    body: serde_json::to_vec(&error_response).expect("json should serialize"),
                    extra_headers: Vec::new(),
                };
            }
        }
    };

    session.broadcast(&result.notifications);

    if result.accepted_notification {
        return McpHttpResponse {
            status: 202,
            content_type: None,
            body: Vec::new(),
            extra_headers: Vec::new(),
        };
    }

    let response_body = result
        .response
        .expect("MCP HTTP requests should produce a JSON-RPC response");
    let mut extra_headers = Vec::new();
    if created_session {
        extra_headers.push(("Mcp-Session-Id".to_string(), session_id));
    }
    McpHttpResponse {
        status: 200,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&response_body).expect("json should serialize"),
        extra_headers,
    }
}

fn validate_mcp_http_post_headers(request: &McpHttpRequest) -> Option<McpHttpResponse> {
    if !request
        .headers
        .get("content-type")
        .is_some_and(|value| value.contains("application/json"))
    {
        return Some(mcp_http_json_error_response(
            400,
            "MCP POST requests require Content-Type: application/json",
            Value::Null,
        ));
    }
    if !request.headers.get("accept").is_some_and(|value| {
        value.contains("application/json") && value.contains("text/event-stream")
    }) {
        return Some(mcp_http_json_error_response(
            400,
            "MCP POST requests require Accept: application/json, text/event-stream",
            Value::Null,
        ));
    }
    None
}

fn parse_mcp_http_json_body(request: &McpHttpRequest) -> Result<Value, McpHttpResponse> {
    serde_json::from_slice(&request.body).map_err(|error| {
        mcp_http_json_error_response(400, format!("Parse error: {error}"), Value::Null)
    })
}

fn resolve_mcp_http_session(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
    payload: &Value,
) -> Result<(String, Arc<McpHttpSession>, bool), McpHttpResponse> {
    let is_initialize = payload
        .as_object()
        .and_then(|object| object.get("method"))
        .and_then(Value::as_str)
        == Some("initialize");

    if is_initialize {
        let session_id = Ulid::new().to_string();
        let bound_profile = request_bound_permission_profile(context, request);
        let requested_profile = context
            .requested_profile
            .as_deref()
            .or(bound_profile.as_deref());
        let core = McpServerCore::new(
            &context.paths,
            requested_profile,
            &context.tool_pack_args,
            context.tool_pack_mode_arg,
        )
        .map_err(|error| mcp_http_json_error_response(500, error.to_string(), Value::Null))?;
        let session = Arc::new(McpHttpSession::new(core));
        context
            .sessions
            .lock()
            .expect("mcp sessions lock should not be poisoned")
            .insert(session_id.clone(), Arc::clone(&session));
        return Ok((session_id, session, true));
    }

    let Some(session_id) = request.headers.get("mcp-session-id").cloned() else {
        return Err(mcp_http_json_error_response(
            400,
            "missing Mcp-Session-Id header",
            Value::Null,
        ));
    };
    let Some(session) = context
        .sessions
        .lock()
        .expect("mcp sessions lock should not be poisoned")
        .get(&session_id)
        .cloned()
    else {
        return Err(mcp_http_json_error_response(
            404,
            "unknown Mcp-Session-Id",
            Value::Null,
        ));
    };
    Ok((session_id, session, false))
}

fn handle_mcp_http_delete(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
) -> McpHttpResponse {
    let Some(session_id) = request.headers.get("mcp-session-id") else {
        return mcp_http_json_error_response(400, "missing Mcp-Session-Id header", Value::Null);
    };
    let session = context
        .sessions
        .lock()
        .expect("mcp sessions lock should not be poisoned")
        .remove(session_id);
    let Some(session) = session else {
        return mcp_http_json_error_response(404, "unknown Mcp-Session-Id", Value::Null);
    };
    session.close();
    McpHttpResponse {
        status: 204,
        content_type: None,
        body: Vec::new(),
        extra_headers: Vec::new(),
    }
}

fn handle_mcp_http_sse(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
    stream: &mut TcpStream,
) -> Result<(), CliError> {
    if !request
        .headers
        .get("accept")
        .is_some_and(|value| value.contains("text/event-stream"))
    {
        let response = mcp_http_json_error_response(
            405,
            "MCP GET requests require Accept: text/event-stream",
            Value::Null,
        );
        write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        return Ok(());
    }
    let Some(session_id) = request.headers.get("mcp-session-id") else {
        let response =
            mcp_http_json_error_response(400, "missing Mcp-Session-Id header", Value::Null);
        write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        return Ok(());
    };
    let Some(session) = context
        .sessions
        .lock()
        .expect("mcp sessions lock should not be poisoned")
        .get(session_id)
        .cloned()
    else {
        let response = mcp_http_json_error_response(404, "unknown Mcp-Session-Id", Value::Null);
        write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        return Ok(());
    };

    write_mcp_http_sse_headers(stream).map_err(CliError::operation)?;
    let receiver = session.register_subscriber();
    let mut keepalive_elapsed = Duration::ZERO;

    loop {
        if session.is_closed() {
            break;
        }
        match receiver.recv_timeout(MCP_HTTP_POLL_INTERVAL) {
            Ok(message) => {
                write_mcp_http_sse_event(stream, &message).map_err(CliError::operation)?;
                keepalive_elapsed = Duration::ZERO;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let notifications = {
                    let mut core = session
                        .core
                        .lock()
                        .expect("mcp core lock should not be poisoned");
                    core.list_changed_notifications()
                };
                for notification in notifications {
                    write_mcp_http_sse_event(stream, &notification).map_err(CliError::operation)?;
                }
                keepalive_elapsed += MCP_HTTP_POLL_INTERVAL;
                if keepalive_elapsed >= MCP_HTTP_KEEPALIVE_INTERVAL {
                    write_mcp_http_sse_keepalive(stream).map_err(CliError::operation)?;
                    keepalive_elapsed = Duration::ZERO;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
}

fn validate_mcp_http_security(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
) -> Option<McpHttpResponse> {
    #[cfg(feature = "oauth")]
    if let Some(oauth) = context.oauth.as_ref() {
        let Some(token) = bearer_token(&request.headers) else {
            return Some(oauth_error_response(
                oauth,
                "missing OAuth bearer token",
                "invalid_token",
            ));
        };
        if let Err(error) = validate_oauth_bearer_token(oauth, &token) {
            eprintln!("MCP OAuth bearer token rejected: {error}");
            return Some(oauth_error_response(
                oauth,
                error.to_string(),
                "invalid_token",
            ));
        }
    }
    if let Some(expected_token) = context.auth_token.as_deref() {
        let actual_token = bearer_or_shared_token(&request.headers);
        if actual_token.as_deref() != Some(expected_token) {
            return Some(mcp_http_json_error_response(
                401,
                "missing or invalid authentication token",
                Value::Null,
            ));
        }
    }
    if let Some(origin) = request.headers.get("origin") {
        if !origin_allowed(origin, context.bind_addr) {
            return Some(mcp_http_json_error_response(
                403,
                "invalid Origin header",
                Value::Null,
            ));
        }
    }
    None
}

#[cfg(feature = "oauth")]
fn validate_oauth_bearer_token(
    oauth: &McpOAuthMode,
    token: &str,
) -> Result<(), vulcan_core::OAuthError> {
    match oauth {
        McpOAuthMode::External(external) => external.validate_bearer_token(token),
        McpOAuthMode::Local(local) => local.validate_bearer_token(token).map(|_| ()),
    }
}

fn request_bound_permission_profile(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
) -> Option<String> {
    #[cfg(not(feature = "oauth"))]
    {
        let _ = (context, request);
        return None;
    }

    #[cfg(feature = "oauth")]
    {
        let McpOAuthMode::Local(local) = context.oauth.as_ref()? else {
            return None;
        };
        let token = bearer_token(&request.headers)?;
        let identity = local.validate_bearer_token(&token).ok()?;
        identity.permission_profile
    }
}

impl McpServerCore {
    fn new(
        paths: &VaultPaths,
        requested_profile: Option<&str>,
        tool_pack_args: &[McpToolPackArg],
        tool_pack_mode_arg: McpToolPackModeArg,
    ) -> Result<Self, CliError> {
        let selection = resolve_permission_profile(paths, requested_profile)
            .map_err(permission_error_to_cli)?;
        let tool_pack_mode = McpToolPackMode::from(tool_pack_mode_arg);
        let selected_tool_packs = resolve_selected_tool_packs(tool_pack_args, tool_pack_mode);
        let guard = ProfilePermissionGuard::new(paths, selection.clone());
        let snapshot = McpServerSnapshot {
            tools: tool_fingerprint(
                paths,
                Some(selection.name.as_str()),
                &selected_tool_packs,
                &selection.profile,
            ),
            prompts: prompt_files_fingerprint(paths),
            resources: resource_files_fingerprint(paths),
        };

        Ok(Self {
            paths: paths.clone(),
            selection,
            guard,
            tool_pack_mode,
            selected_tool_packs,
            stored_resources: BTreeMap::new(),
            next_resource_id: 1,
            snapshot,
        })
    }

    fn process_request_with_timeout(&mut self, request: Value, timeout: Duration) -> Vec<Value> {
        if timeout.is_zero() {
            return timeout_response_for_request(&request, timeout)
                .into_iter()
                .collect();
        }
        let timeout_request = request.clone();
        let mut worker = self.clone();
        let (sender, receiver) = mpsc::channel();
        if thread::Builder::new()
            .name("vulcan-mcp-request".to_string())
            .stack_size(MCP_REQUEST_WORKER_STACK_SIZE)
            .spawn(move || {
                let messages = worker.process_request(request);
                let _ = sender.send((worker, messages));
            })
            .is_err()
        {
            let id = request_id(&timeout_request).unwrap_or(Value::Null);
            return vec![jsonrpc_error(
                id,
                -32603,
                "MCP request worker could not be started".to_string(),
                None,
            )];
        }
        match receiver.recv_timeout(timeout) {
            Ok((next, messages)) => {
                *self = next;
                messages
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                timeout_response_for_request(&timeout_request, timeout)
                    .into_iter()
                    .collect()
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let id = request_id(&timeout_request).unwrap_or(Value::Null);
                vec![jsonrpc_error(
                    id,
                    -32603,
                    "MCP request worker stopped before producing a response".to_string(),
                    None,
                )]
            }
        }
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

    fn process_http_request(&mut self, request: &Value) -> Result<McpHttpProcessResult, Value> {
        let Some(request_object) = request.as_object() else {
            return Err(jsonrpc_error(
                Value::Null,
                -32600,
                "Invalid request".to_string(),
                None,
            ));
        };
        if request.is_array() {
            return Err(jsonrpc_error(
                Value::Null,
                -32600,
                "Batch requests are not supported by the 2025-06-18 MCP baseline".to_string(),
                None,
            ));
        }
        if request_object.contains_key("result") || request_object.contains_key("error") {
            return Ok(McpHttpProcessResult {
                response: None,
                notifications: Vec::new(),
                accepted_notification: true,
            });
        }

        let id = request_object.get("id").cloned().unwrap_or(Value::Null);
        let is_notification = !request_object.contains_key("id");
        let Some(method) = request_object.get("method").and_then(Value::as_str) else {
            return Err(jsonrpc_error(
                if is_notification { Value::Null } else { id },
                -32600,
                "Invalid request".to_string(),
                None,
            ));
        };

        let outcome = match self.handle_method(method, request_object.get("params")) {
            Ok(outcome) => outcome,
            Err(McpMethodError::JsonRpc {
                code,
                message,
                data,
            }) => {
                if is_notification {
                    return Err(jsonrpc_error(Value::Null, code, message, data));
                }
                return Ok(McpHttpProcessResult {
                    response: Some(jsonrpc_error(id, code, message, data)),
                    notifications: Vec::new(),
                    accepted_notification: false,
                });
            }
            Err(McpMethodError::Tool {
                message,
                structured,
            }) => {
                if is_notification {
                    return Err(jsonrpc_error(Value::Null, -32603, message, structured));
                }
                return Ok(McpHttpProcessResult {
                    response: Some(tool_error_response(id, message, structured)),
                    notifications: Vec::new(),
                    accepted_notification: false,
                });
            }
        };

        let notifications = if outcome.emit_list_notifications {
            self.list_changed_notifications()
        } else {
            Vec::new()
        };

        Ok(McpHttpProcessResult {
            response: if is_notification {
                None
            } else {
                outcome
                    .response
                    .map(|response| jsonrpc_result(id, response))
            },
            notifications,
            accepted_notification: is_notification,
        })
    }

    fn process_http_request_with_timeout(
        &mut self,
        request: Value,
        timeout: Duration,
    ) -> Result<McpHttpProcessResult, Value> {
        if timeout.is_zero() {
            return Ok(timeout_http_result(&request, timeout));
        }
        let timeout_request = request.clone();
        let mut worker = self.clone();
        let (sender, receiver) = mpsc::channel();
        if thread::Builder::new()
            .name("vulcan-mcp-http-request".to_string())
            .stack_size(MCP_REQUEST_WORKER_STACK_SIZE)
            .spawn(move || {
                let result = worker.process_http_request(&request);
                let _ = sender.send((worker, result));
            })
            .is_err()
        {
            return Err(jsonrpc_error(
                request_id(&timeout_request).unwrap_or(Value::Null),
                -32603,
                "MCP request worker could not be started".to_string(),
                None,
            ));
        }
        match receiver.recv_timeout(timeout) {
            Ok((next, result)) => {
                *self = next;
                result
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Ok(timeout_http_result(&timeout_request, timeout))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(jsonrpc_error(
                request_id(&timeout_request).unwrap_or(Value::Null),
                -32603,
                "MCP request worker stopped before producing a response".to_string(),
                None,
            )),
        }
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
                        self.visible_tool_items()?,
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
        visible_tool_catalog(&self.selected_tool_packs, &self.selection.profile)
    }

    fn visible_custom_tools(
        &self,
    ) -> Result<Vec<crate::tools::CustomToolDescriptor>, McpMethodError> {
        visible_custom_tools(
            &self.paths,
            Some(self.selection.name.as_str()),
            &self.selected_tool_packs,
        )
        .map_err(cli_tool_error)
    }

    fn visible_tool_items(&self) -> Result<Vec<Value>, McpMethodError> {
        let mut tools = self
            .visible_tools()
            .into_iter()
            .map(tool_list_item)
            .collect::<Vec<_>>();
        tools.extend(
            self.visible_custom_tools()?
                .iter()
                .map(custom_tool_list_item),
        );
        Ok(tools)
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
            if self
                .visible_custom_tools()?
                .iter()
                .any(|tool| tool.summary.name.starts_with("skill_"))
            {
                resources.push(serde_json::json!({
                    "uri": "vulcan://assistant/skill-commands/index",
                    "name": "Assistant Skill Command Index",
                    "title": "Vault Skill Command Index",
                    "description": "Visible Agent Skills-compatible command tools projected into the shared tool registry.",
                    "mimeType": "application/json",
                }));
            }
            if !self.visible_custom_tools()?.is_empty() {
                resources.push(serde_json::json!({
                    "uri": "vulcan://assistant/tools/index",
                    "name": "Assistant Tool Index",
                    "title": "Vault Custom Tool Index",
                    "description": "Visible callable skill command tools projected into the shared tool registry.",
                    "mimeType": "application/json",
                }));
            }
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
            templates.push(serde_json::json!({
                "uriTemplate": "vulcan://assistant/skill-commands/{name}",
                "name": "Assistant Skill Commands",
                "title": "Assistant Skill Command Resource",
                "description": "Read one visible projected skill command as structured JSON.",
                "mimeType": "application/json",
            }));
            if self.selected_tool_packs.contains(&McpToolPack::Custom) {
                templates.push(serde_json::json!({
                    "uriTemplate": "vulcan://assistant/tools/{name}",
                    "name": "Assistant Tools",
                    "title": "Assistant Tool Resource",
                    "description": "Read one visible callable skill command tool as structured JSON.",
                    "mimeType": "application/json",
                }));
            }
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

    #[allow(clippy::too_many_lines)]
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
            "vulcan://assistant/skill-commands/index" => {
                let commands = self
                    .visible_custom_tools()?
                    .into_iter()
                    .filter(|tool| tool.summary.name.starts_with("skill_"))
                    .collect::<Vec<_>>();
                if commands.is_empty() {
                    return Err(resource_not_found_error(
                        uri,
                        "Resource not found".to_string(),
                    ));
                }
                return Self::json_resource(uri, &commands);
            }
            "vulcan://assistant/tools/index" => {
                let tools = self.visible_custom_tools()?;
                if tools.is_empty() {
                    return Err(resource_not_found_error(
                        uri,
                        "Resource not found".to_string(),
                    ));
                }
                return Self::json_resource(uri, &tools);
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

        if let Some(name) = uri.strip_prefix("vulcan://assistant/skill-commands/") {
            let report = crate::tools::show_custom_tool(
                &self.paths,
                Some(self.selection.name.as_str()),
                name,
                &crate::custom_tool_registry_options(),
            )
            .map_err(|error| resource_not_found_error(uri, error.to_string()))?;
            if !report.callable || !report.tool.summary.name.starts_with("skill_") {
                return Err(resource_not_found_error(
                    uri,
                    format!(
                        "permission denied: resource `{uri}` is not available under profile `{}`",
                        self.selection.name
                    ),
                ));
            }
            return Self::json_resource(uri, &report);
        }

        if let Some(name) = uri.strip_prefix("vulcan://assistant/tools/") {
            let report = crate::tools::show_custom_tool(
                &self.paths,
                Some(self.selection.name.as_str()),
                name,
                &crate::custom_tool_registry_options(),
            )
            .map_err(|error| resource_not_found_error(uri, error.to_string()))?;
            let selected_pack_names = pack_name_list(&self.selected_tool_packs)
                .into_iter()
                .collect::<BTreeSet<_>>();
            if !report.callable
                || !custom_tool_matches_selected_packs(
                    &report.tool.summary.packs,
                    &selected_pack_names,
                )
            {
                return Err(resource_not_found_error(
                    uri,
                    format!(
                        "permission denied: resource `{uri}` is not available under profile `{}`",
                        self.selection.name
                    ),
                ));
            }
            return Self::json_resource(uri, &report);
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
            return self.call_custom_tool(name, arguments);
        };
        if !tool
            .packs
            .iter()
            .any(|pack| self.selected_tool_packs.contains(pack))
        {
            return Err(McpMethodError::invalid_params(format!(
                "Unknown tool: {name}"
            )));
        }
        if !tool_visible(tool, &self.selection.profile, &self.selected_tool_packs) {
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
            McpToolId::Query => {
                let args: McpQueryArgs = parse_tool_arguments(arguments)?;
                if args.query.is_some() && args.json.is_some() {
                    return Err(McpMethodError::invalid_params(
                        "`query` accepts either `query` or `json`, not both",
                    ));
                }
                let use_dql = match args.engine.as_deref().unwrap_or("auto") {
                    "dql" => true,
                    "dsl" => false,
                    "auto" => args.query.as_deref().is_some_and(|query| {
                        query.trim_start().to_ascii_uppercase().starts_with("TABLE")
                            || query.trim_start().to_ascii_uppercase().starts_with("LIST")
                            || query.trim_start().to_ascii_uppercase().starts_with("TASK")
                    }),
                    other => {
                        return Err(McpMethodError::invalid_params(format!(
                            "unsupported `query.engine`: {other}"
                        )));
                    }
                };
                if use_dql {
                    let dql = args.query.as_deref().ok_or_else(|| {
                        McpMethodError::invalid_params("DQL queries require `query`")
                    })?;
                    if !args.filters.is_empty() || args.sort.is_some() || args.desc {
                        return Err(McpMethodError::invalid_params(
                            "`query.filters`, `sort`, and `desc` cannot be combined with DQL",
                        ));
                    }
                    let result = evaluate_dql_with_filter(
                        &self.paths,
                        dql,
                        None,
                        Some(&self.guard.read_filter()),
                    )
                    .map_err(|error| McpMethodError::tool(error.to_string()))?;
                    return self.serialize_tool_report(tool.name, &result);
                }
                let report = match (args.query.as_deref(), args.json.as_deref()) {
                    (Some(dsl), None) => {
                        if !args.filters.is_empty() || args.sort.is_some() || args.desc {
                            return Err(McpMethodError::invalid_params(
                                "`query.filters`, `sort`, and `desc` cannot be combined with a DSL string or JSON query",
                            ));
                        }
                        let ast = QueryAst::from_dsl(dsl)
                            .map_err(|error| McpMethodError::tool(error.to_string()))?;
                        execute_query_report_with_filter(
                            &self.paths,
                            ast,
                            Some(&self.guard.read_filter()),
                        )
                        .map_err(|error| McpMethodError::tool(error.to_string()))?
                    }
                    (None, Some(json)) => {
                        if !args.filters.is_empty() || args.sort.is_some() || args.desc {
                            return Err(McpMethodError::invalid_params(
                                "`query.filters`, `sort`, and `desc` cannot be combined with a DSL string or JSON query",
                            ));
                        }
                        let ast = QueryAst::from_json(json)
                            .map_err(|error| McpMethodError::tool(error.to_string()))?;
                        execute_query_report_with_filter(
                            &self.paths,
                            ast,
                            Some(&self.guard.read_filter()),
                        )
                        .map_err(|error| McpMethodError::tool(error.to_string()))?
                    }
                    (None, None) => {
                        let note_query = NoteQuery {
                            filters: args.filters,
                            sort_by: args.sort,
                            sort_descending: args.desc,
                        };
                        let notes_report = query_notes_with_filter(
                            &self.paths,
                            &note_query,
                            Some(&self.guard.read_filter()),
                        )
                        .map_err(|error| McpMethodError::tool(error.to_string()))?;
                        let ast = QueryAst::from_note_query(&note_query)
                            .map_err(|error| McpMethodError::tool(error.to_string()))?;
                        QueryReport {
                            query: ast,
                            notes: notes_report.notes,
                        }
                    }
                    (Some(_), Some(_)) => unreachable!("checked above"),
                };
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::Status => {
                let report = run_status_command(&self.paths).map_err(cli_tool_error)?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::DailyShow => {
                let args: McpDailyShowArgs = parse_tool_arguments(arguments)?;
                let report = run_daily_show_command(&self.paths, args.date.as_deref(), "daily")
                    .map_err(cli_tool_error)?;
                self.check_read_note_access(&report.path)
                    .map_err(cli_tool_error)?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::DailyList => {
                let args: McpDailyListArgs = parse_tool_arguments(arguments)?;
                let report = run_daily_list_command(
                    &self.paths,
                    args.from.as_deref(),
                    args.to.as_deref(),
                    args.week,
                    args.month,
                )
                .map_err(cli_tool_error)?;
                let filtered = report
                    .into_iter()
                    .filter(|item| self.guard.check_read_path(&item.path).is_ok())
                    .collect::<Vec<_>>();
                self.serialize_tool_report(tool.name, &filtered)
            }
            McpToolId::GraphCommunities => {
                let args: McpGraphCommunitiesArgs = parse_tool_arguments(arguments)?;
                let report = query_graph_communities_with_filter(
                    &self.paths,
                    Some(&self.guard.read_filter()),
                    !args.dry_run,
                )
                .map_err(|error| McpMethodError::tool(error.to_string()))?;
                let mut value = serde_json::to_value(report)
                    .map_err(|error| McpMethodError::tool(error.to_string()))?;
                if let Value::Object(object) = &mut value {
                    object.insert("selected_community".to_string(), args.community.into());
                    object.insert("include_orphans".to_string(), args.orphans.into());
                    object.insert("include_bridges".to_string(), args.bridges.into());
                }
                Ok(self.tool_success_response(tool.name, value))
            }
            McpToolId::SuggestLinks => {
                let args: McpSuggestLinksArgs = parse_tool_arguments(arguments)?;
                if args.accept.is_some() && args.reject.is_some() {
                    return Err(McpMethodError::invalid_params(
                        "`suggest_links` accepts either `accept` or `reject`, not both",
                    ));
                }
                if let Some(id) = args.accept.as_deref() {
                    self.guard
                        .check_write_path(".vulcan/cache.db")
                        .map_err(|error| McpMethodError::tool(error.to_string()))?;
                    let suggestion = accept_link_suggestion(&self.paths, id)
                        .map_err(|error| McpMethodError::tool(error.to_string()))?;
                    return self.serialize_tool_report(
                        tool.name,
                        &vulcan_core::LinkSuggestionsReport {
                            suggestions: vec![suggestion],
                        },
                    );
                }
                if let Some(id) = args.reject.as_deref() {
                    self.guard
                        .check_write_path(".vulcan/cache.db")
                        .map_err(|error| McpMethodError::tool(error.to_string()))?;
                    let suggestion = reject_link_suggestion(&self.paths, id)
                        .map_err(|error| McpMethodError::tool(error.to_string()))?;
                    return self.serialize_tool_report(
                        tool.name,
                        &vulcan_core::LinkSuggestionsReport {
                            suggestions: vec![suggestion],
                        },
                    );
                }
                let status = args
                    .status
                    .as_deref()
                    .map(parse_link_suggestion_status)
                    .transpose()?;
                let report = suggest_links(
                    &self.paths,
                    args.note.as_deref(),
                    args.limit,
                    args.min_score,
                    status,
                )
                .map_err(|error| McpMethodError::tool(error.to_string()))?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::TaskList => {
                let args: McpTaskListArgs = parse_tool_arguments(arguments)?;
                let report = run_tasks_list_command(
                    &self.paths,
                    TasksListOptions {
                        filter: args.filter.as_deref(),
                        source: parse_tasks_default_source(args.source.as_deref())?,
                        status: args.status.as_deref(),
                        priority: args.priority.as_deref(),
                        due_before: args.due_before.as_deref(),
                        due_after: args.due_after.as_deref(),
                        project: args.project.as_deref(),
                        context: args.context.as_deref(),
                        group_by: args.group_by.as_deref(),
                        sort_by: args.sort_by.as_deref(),
                        include_archived: args.include_archived,
                    },
                )
                .map_err(cli_tool_error)?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::TaskQuery => {
                let args: McpTaskQueryArgs = parse_tool_arguments(arguments)?;
                let report =
                    run_tasks_query_command(&self.paths, &args.query).map_err(cli_tool_error)?;
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::TaskCreate => {
                let args: McpTaskCreateArgs = parse_tool_arguments(arguments)?;
                if !args.dry_run {
                    let planned = run_tasks_create_command(
                        &self.paths,
                        TasksCreateOptions {
                            text: &args.text,
                            note: args.note.as_deref(),
                            due: args.due.as_deref(),
                            priority: args.priority.as_deref(),
                            dry_run: true,
                        },
                        OutputFormat::Json,
                        false,
                        true,
                    )
                    .map_err(cli_tool_error)?;
                    for path in &planned.changed_paths {
                        self.check_write_path_access(path).map_err(cli_tool_error)?;
                    }
                }
                let report = run_tasks_create_command(
                    &self.paths,
                    TasksCreateOptions {
                        text: &args.text,
                        note: args.note.as_deref(),
                        due: args.due.as_deref(),
                        priority: args.priority.as_deref(),
                        dry_run: args.dry_run,
                    },
                    OutputFormat::Json,
                    false,
                    true,
                )
                .map_err(cli_tool_error)?;
                if !args.dry_run {
                    AutoCommitPolicy::for_mutation(&self.paths, args.no_commit)
                        .commit(
                            &self.paths,
                            "task-create",
                            &report.changed_paths,
                            Some(self.selection.name.as_str()),
                            true,
                        )
                        .map_err(|error| McpMethodError::tool(error.clone()))?;
                }
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::TaskComplete => {
                let args: McpTaskCompleteArgs = parse_tool_arguments(arguments)?;
                if !args.dry_run {
                    let planned = run_tasks_complete_command(
                        &self.paths,
                        &args.task,
                        args.date.as_deref(),
                        true,
                        OutputFormat::Json,
                        false,
                        true,
                    )
                    .map_err(cli_tool_error)?;
                    for path in &planned.changed_paths {
                        self.check_write_path_access(path).map_err(cli_tool_error)?;
                    }
                }
                let report = run_tasks_complete_command(
                    &self.paths,
                    &args.task,
                    args.date.as_deref(),
                    args.dry_run,
                    OutputFormat::Json,
                    false,
                    true,
                )
                .map_err(cli_tool_error)?;
                if !args.dry_run {
                    AutoCommitPolicy::for_mutation(&self.paths, args.no_commit)
                        .commit(
                            &self.paths,
                            "task-complete",
                            &report.changed_paths,
                            Some(self.selection.name.as_str()),
                            true,
                        )
                        .map_err(|error| McpMethodError::tool(error.clone()))?;
                }
                self.serialize_tool_report(tool.name, &report)
            }
            McpToolId::TaskReschedule => {
                let args: McpTaskRescheduleArgs = parse_tool_arguments(arguments)?;
                if !args.dry_run {
                    let planned = run_tasks_reschedule_command(
                        &self.paths,
                        &args.task,
                        &args.due,
                        true,
                        OutputFormat::Json,
                        false,
                        true,
                    )
                    .map_err(cli_tool_error)?;
                    for path in &planned.changed_paths {
                        self.check_write_path_access(path).map_err(cli_tool_error)?;
                    }
                }
                let report = run_tasks_reschedule_command(
                    &self.paths,
                    &args.task,
                    &args.due,
                    args.dry_run,
                    OutputFormat::Json,
                    false,
                    true,
                )
                .map_err(cli_tool_error)?;
                if !args.dry_run {
                    AutoCommitPolicy::for_mutation(&self.paths, args.no_commit)
                        .commit(
                            &self.paths,
                            "task-reschedule",
                            &report.changed_paths,
                            Some(self.selection.name.as_str()),
                            true,
                        )
                        .map_err(|error| McpMethodError::tool(error.clone()))?;
                }
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
                if !args.confirm {
                    return Err(McpMethodError::invalid_params(
                        "`note_set.confirm` must be true because this replaces the full note body",
                    ));
                }
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
                if !args.dry_run && !args.confirm {
                    return Err(McpMethodError::invalid_params(
                        "`note_delete.confirm` must be true unless `dry_run` is true",
                    ));
                }
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
            McpToolId::ToolPackList => {
                let structured = self.current_tool_pack_state();
                Ok(self.tool_success_response(tool.name, structured))
            }
            McpToolId::ToolPackEnable => {
                self.ensure_adaptive_tool_pack_mode()?;
                let args: McpToolPackMutationArgs = parse_tool_arguments(arguments)?;
                let requested = parse_tool_pack_selection_args(&args.packs)?;
                for pack in resolve_selected_tool_packs(&requested, McpToolPackMode::Static) {
                    self.selected_tool_packs.insert(pack);
                }
                let structured = self.current_tool_pack_state();
                Ok(self.tool_success_response(tool.name, structured))
            }
            McpToolId::ToolPackDisable => {
                self.ensure_adaptive_tool_pack_mode()?;
                let args: McpToolPackMutationArgs = parse_tool_arguments(arguments)?;
                let requested = parse_tool_pack_selection_args(&args.packs)?;
                for pack in resolve_selected_tool_packs(&requested, McpToolPackMode::Static) {
                    if pack != McpToolPack::ToolPacks {
                        self.selected_tool_packs.remove(&pack);
                    }
                }
                let structured = self.current_tool_pack_state();
                Ok(self.tool_success_response(tool.name, structured))
            }
            McpToolId::ToolPackSet => {
                self.ensure_adaptive_tool_pack_mode()?;
                let args: McpToolPackMutationArgs = parse_tool_arguments(arguments)?;
                let requested = parse_tool_pack_selection_args(&args.packs)?;
                self.selected_tool_packs =
                    resolve_selected_tool_packs(&requested, McpToolPackMode::Adaptive);
                let structured = self.current_tool_pack_state();
                Ok(self.tool_success_response(tool.name, structured))
            }
        }
    }

    fn call_custom_tool(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpMethodError> {
        if !self.selected_tool_packs.contains(&McpToolPack::Custom) {
            return Err(McpMethodError::invalid_params(format!(
                "Unknown tool: {name}"
            )));
        }
        let report = crate::tools::show_custom_tool(
            &self.paths,
            Some(self.selection.name.as_str()),
            name,
            &crate::custom_tool_registry_options(),
        )
        .map_err(|_| McpMethodError::invalid_params(format!("Unknown tool: {name}")))?;
        let selected_pack_names = pack_name_list(&self.selected_tool_packs)
            .into_iter()
            .collect::<BTreeSet<_>>();
        if !custom_tool_matches_selected_packs(&report.tool.summary.packs, &selected_pack_names) {
            return Err(McpMethodError::invalid_params(format!(
                "Unknown tool: {name}"
            )));
        }
        if !report.callable {
            return Err(McpMethodError::tool(format!(
                "permission denied: tool `{name}` is not available under profile `{}`",
                self.selection.name
            )));
        }
        let report = crate::tools::run_custom_tool(
            &self.paths,
            Some(self.selection.name.as_str()),
            name,
            &Value::Object(arguments.clone()),
            &crate::custom_tool_registry_options(),
            &crate::tools::CustomToolRunOptions {
                surface: "mcp".to_string(),
            },
        )
        .map_err(cli_tool_error)?;
        Ok(self.custom_tool_success_response(&report.name, report.result, report.text.as_deref()))
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

    fn ensure_adaptive_tool_pack_mode(&self) -> Result<(), McpMethodError> {
        if matches!(self.tool_pack_mode, McpToolPackMode::Adaptive) {
            Ok(())
        } else {
            Err(McpMethodError::tool(
                "tool-pack mutation requires `--tool-pack-mode adaptive` for this MCP session",
            ))
        }
    }

    fn current_tool_pack_state(&self) -> Value {
        let available = ALL_MCP_TOOL_PACKS
            .iter()
            .copied()
            .map(|pack| {
                let tools = tool_names_for_pack(pack, &self.selection.profile);
                serde_json::json!({
                    "name": pack.as_str(),
                    "description": pack.description(),
                    "selected": self.selected_tool_packs.contains(&pack),
                    "adaptiveOnly": pack == McpToolPack::ToolPacks,
                    "visibleTools": tools,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "mode": self.tool_pack_mode.as_str(),
            "selectedToolPacks": pack_name_list(&self.selected_tool_packs),
            "availableToolPacks": available,
        })
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

    fn custom_tool_success_response(
        &mut self,
        tool_name: &str,
        structured: Value,
        text: Option<&str>,
    ) -> Value {
        let structured = if structured.is_object() {
            structured
        } else {
            serde_json::json!({ "result": structured })
        };
        let serialized = serde_json::to_string_pretty(&structured).unwrap_or_default();
        let mut content = Vec::new();
        if let Some(text) = text {
            content.push(serde_json::json!({
                "type": "text",
                "text": text,
            }));
        }
        if serialized.len() <= MCP_INLINE_TEXT_LIMIT {
            if text.is_none() {
                content.push(serde_json::json!({
                    "type": "text",
                    "text": serialized,
                }));
            }
        } else {
            if text.is_none() {
                content.push(serde_json::json!({
                    "type": "text",
                    "text": tool_summary_text(tool_name, &structured),
                }));
            }
            content.push(self.store_tool_result_resource(tool_name, &serialized));
        }
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
            tools: tool_fingerprint(
                &self.paths,
                Some(self.selection.name.as_str()),
                &self.selected_tool_packs,
                &self.selection.profile,
            ),
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

fn parse_mcp_http_bind_addr(bind: &str, allow_remote: bool) -> Result<SocketAddr, CliError> {
    let addr = bind.parse::<SocketAddr>().map_err(|_| {
        CliError::operation("mcp bind address must be a socket address like 127.0.0.1:8765")
    })?;
    if !addr.ip().is_loopback() && !allow_remote {
        return Err(CliError::operation(
            "non-loopback MCP HTTP binds require --auth-token",
        ));
    }
    Ok(addr)
}

#[allow(clippy::too_many_lines)]
#[cfg(feature = "oauth")]
fn build_mcp_oauth_validator(
    paths: &VaultPaths,
    options: &McpHttpOptions,
) -> Result<Option<McpOAuthMode>, CliError> {
    let local_requested = options.oauth_local_client_id.is_some()
        || options.oauth_local_client_secret.is_some()
        || options.oauth_local_approval_token.is_some()
        || options.oauth_dcr
        || options.oauth_indieauth_authorization_endpoint.is_some()
        || options.oauth_indieauth_token_endpoint.is_some()
        || options.oauth_indieauth_me.is_some();
    if local_requested {
        if options.oauth_issuer.is_some() || options.auth_token.is_some() {
            return Err(CliError::operation(
                "local MCP OAuth issuer is mutually exclusive with --oauth-issuer and --auth-token",
            ));
        }
        let public_url = options.public_url.as_deref().ok_or_else(|| {
            CliError::operation("--public-url is required when using local MCP OAuth issuer")
        })?;
        let client_id = options
            .oauth_local_client_id
            .as_deref()
            .unwrap_or("vulcan-mcp");
        let client_secret = match options.oauth_local_client_secret.as_deref() {
            Some(secret) => secret.to_string(),
            None if options.oauth_dcr => load_or_create_local_oauth_issuer_secret(paths)?,
            None => {
                return Err(CliError::operation(
                    "--oauth-local-client-secret is required unless --oauth-dcr is enabled",
                ))
            }
        };
        let approval_token = options.oauth_local_approval_token.as_deref().unwrap_or("");
        if approval_token.is_empty()
            && options.oauth_indieauth_authorization_endpoint.is_none()
            && options.oauth_indieauth_me.is_none()
        {
            return Err(CliError::operation(
                "--oauth-local-approval-token is required unless IndieAuth is configured",
            ));
        }
        let subject = options
            .oauth_local_subject
            .as_deref()
            .unwrap_or("local-user");
        return LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: public_url.to_string(),
            client_id: client_id.to_string(),
            client_secret,
            approval_token: approval_token.to_string(),
            subject: subject.to_string(),
            email: options.oauth_local_email.clone(),
            users: parse_local_oauth_users(&options.oauth_local_user)?,
            dcr_enabled: options.oauth_dcr,
        })
        .map(Arc::new)
        .map(McpOAuthMode::Local)
        .map(Some)
        .map_err(CliError::operation);
    }

    let Some(issuer) = options.oauth_issuer.as_deref() else {
        if options.public_url.is_some()
            || !options.oauth_audience.is_empty()
            || options.oauth_jwks_url.is_some()
            || !options.oauth_allowed_sub.is_empty()
            || !options.oauth_allowed_email.is_empty()
        {
            return Err(CliError::operation(
                "--oauth-issuer is required when using MCP OAuth options",
            ));
        }
        return Ok(None);
    };
    if options.auth_token.is_some() {
        return Err(CliError::operation(
            "--auth-token and --oauth-issuer are mutually exclusive for direct MCP HTTP auth",
        ));
    }
    let public_url = options
        .public_url
        .as_deref()
        .ok_or_else(|| CliError::operation("--public-url is required when using --oauth-issuer"))?;
    if !public_url.starts_with("https://") {
        return Err(CliError::operation(
            "--public-url must be an HTTPS URL for MCP OAuth",
        ));
    }
    if options.oauth_audience.is_empty() {
        return Err(CliError::operation(
            "--oauth-audience is required when using --oauth-issuer",
        ));
    }
    if options.oauth_allowed_sub.is_empty() && options.oauth_allowed_email.is_empty() {
        return Err(CliError::operation(
            "at least one --oauth-allowed-sub or --oauth-allowed-email is required",
        ));
    }

    OAuthResourceServer::from_config(OAuthResourceServerConfig {
        issuer: issuer.to_string(),
        audiences: options.oauth_audience.clone(),
        jwks_url: options.oauth_jwks_url.clone(),
        allowed_subs: options.oauth_allowed_sub.clone(),
        allowed_emails: options.oauth_allowed_email.clone(),
        public_url: public_url.to_string(),
    })
    .map(Arc::new)
    .map(McpOAuthMode::External)
    .map(Some)
    .map_err(CliError::operation)
}

#[cfg(not(feature = "oauth"))]
fn reject_mcp_oauth_options_when_disabled(options: &McpHttpOptions) -> Result<(), CliError> {
    let oauth_requested = options.oauth_issuer.is_some()
        || !options.oauth_audience.is_empty()
        || options.oauth_jwks_url.is_some()
        || !options.oauth_allowed_sub.is_empty()
        || !options.oauth_allowed_email.is_empty()
        || options.oauth_local_client_id.is_some()
        || options.oauth_local_client_secret.is_some()
        || options.oauth_local_approval_token.is_some()
        || options.oauth_local_subject.is_some()
        || options.oauth_local_email.is_some()
        || options.oauth_dcr
        || !options.oauth_dcr_allowed_redirect_host.is_empty()
        || options.oauth_indieauth_authorization_endpoint.is_some()
        || options.oauth_indieauth_token_endpoint.is_some()
        || options.oauth_indieauth_client_id.is_some()
        || options.oauth_indieauth_redirect_uri.is_some()
        || options.oauth_indieauth_me.is_some()
        || !options.oauth_local_user.is_empty();
    if oauth_requested {
        return Err(CliError::operation(
            "MCP OAuth requires a build with the `oauth` feature enabled",
        ));
    }
    Ok(())
}

#[cfg(feature = "oauth")]
fn handle_mcp_oauth_metadata(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
) -> Option<McpHttpResponse> {
    let oauth = context.oauth.as_ref()?;
    if let McpOAuthMode::Local(local) = oauth {
        if request.path == "/oauth/register" {
            return Some(handle_local_oauth_register(context, request));
        }
        if request.path == "/oauth/authorize" {
            return Some(handle_local_oauth_authorize(context, local, request));
        }
        if request.path == "/oauth/token" {
            return Some(handle_local_oauth_token(context, local, request));
        }
        if request.path == "/oauth/indieauth/callback" {
            return Some(handle_local_oauth_indieauth_callback(
                context, local, request,
            ));
        }
    }
    if request.method != "GET" {
        return None;
    }
    if is_authorization_server_metadata_path(&request.path, &context.endpoint) {
        return Some(McpHttpResponse {
            status: 200,
            content_type: Some("application/json"),
            body: serde_json::to_vec(oauth_authorization_server_metadata(oauth))
                .expect("json should serialize"),
            extra_headers: Vec::new(),
        });
    }
    if is_protected_resource_metadata_path(&request.path, &context.endpoint) {
        return Some(oauth_protected_resource_response(oauth));
    }
    None
}

#[cfg(feature = "oauth")]
fn oauth_authorization_server_metadata(oauth: &McpOAuthMode) -> &Value {
    match oauth {
        McpOAuthMode::External(external) => external.authorization_server_metadata(),
        McpOAuthMode::Local(local) => local.authorization_server_metadata(),
    }
}

#[cfg(feature = "oauth")]
fn oauth_protected_resource_response(oauth: &McpOAuthMode) -> McpHttpResponse {
    let body = match oauth {
        McpOAuthMode::External(external) => serde_json::json!({
            "resource": external.public_url(),
            "authorization_servers": [external.authorization_server_issuer()],
            "bearer_methods_supported": ["header"],
        }),
        McpOAuthMode::Local(local) => serde_json::json!({
            "resource": local.public_url(),
            "authorization_servers": [local.public_url()],
            "bearer_methods_supported": ["header"],
        }),
    };
    McpHttpResponse {
        status: 200,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&body).expect("json should serialize"),
        extra_headers: Vec::new(),
    }
}

#[cfg(feature = "oauth")]
fn handle_local_oauth_authorize(
    context: &McpHttpServerContext,
    issuer: &LocalOAuthIssuer,
    request: &McpHttpRequest,
) -> McpHttpResponse {
    if request.method != "GET" {
        return oauth_plain_response(405, "method not allowed");
    }
    let params = parse_query_params(&request.query);
    let client_id = params.get("client_id").cloned().unwrap_or_default();
    let redirect_uri = params.get("redirect_uri").cloned().unwrap_or_default();
    let response_type = params.get("response_type").cloned().unwrap_or_default();
    let code_challenge = params.get("code_challenge").cloned().unwrap_or_default();
    let code_challenge_method = params
        .get("code_challenge_method")
        .cloned()
        .unwrap_or_default();
    if !local_oauth_client_redirect_allowed(context, issuer, &client_id, &redirect_uri)
        || response_type != "code"
        || code_challenge.is_empty()
        || code_challenge_method != "S256"
    {
        return oauth_plain_response(400, "invalid OAuth authorization request");
    }
    if let Some(indieauth) = context.oauth_indieauth.as_ref() {
        let state = Ulid::new().to_string();
        let indieauth_code_verifier = generate_pkce_verifier();
        let indieauth_code_challenge = pkce_s256_challenge(&indieauth_code_verifier);
        context
            .oauth_pending_indieauth
            .lock()
            .expect("oauth pending indieauth lock should not be poisoned")
            .insert(
                state.clone(),
                LocalOAuthPendingIndieAuth {
                    client_id,
                    redirect_uri,
                    code_challenge,
                    indieauth_code_verifier,
                    state: params.get("state").cloned(),
                    expires_at: std::time::Instant::now() + Duration::from_secs(600),
                },
            );
        return local_oauth_redirect_to_indieauth(indieauth, &state, &indieauth_code_challenge);
    }
    let approval_token = params.get("approval_token").cloned().unwrap_or_default();
    if !issuer.verify_approval_token(&approval_token) {
        return local_oauth_approval_form(&params);
    }
    let code = Ulid::new().to_string();
    let user = issuer.default_user();
    context
        .oauth_codes
        .lock()
        .expect("oauth code lock should not be poisoned")
        .insert(
            code.clone(),
            LocalOAuthCode {
                client_id,
                redirect_uri: redirect_uri.clone(),
                code_challenge,
                subject: user.subject,
                email: user.email,
                permission_profile: user.permission_profile,
                expires_at: std::time::Instant::now() + Duration::from_secs(300),
            },
        );
    let mut location = format!("{}?code={}", redirect_uri, percent_encode(&code));
    if let Some(state) = params.get("state") {
        location.push_str("&state=");
        location.push_str(&percent_encode(state));
    }
    McpHttpResponse {
        status: 302,
        content_type: None,
        body: Vec::new(),
        extra_headers: vec![("Location".to_string(), location)],
    }
}

#[cfg(feature = "oauth")]
fn handle_local_oauth_register(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
) -> McpHttpResponse {
    if !context.oauth_dcr_enabled {
        return oauth_json_error_response(404, "invalid_request", "DCR is not enabled");
    }
    if request.method != "POST" {
        return oauth_json_error_response(405, "invalid_request", "method not allowed");
    }
    let Ok(payload) = serde_json::from_slice::<Value>(&request.body) else {
        return oauth_json_error_response(400, "invalid_client_metadata", "invalid JSON");
    };
    let redirect_uris = payload
        .get("redirect_uris")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if redirect_uris.is_empty()
        || !redirect_uris
            .iter()
            .all(|uri| local_oauth_redirect_host_allowed(context, uri))
    {
        return oauth_json_error_response(
            400,
            "invalid_redirect_uri",
            "redirect URI is not allowed",
        );
    }
    let auth_method = payload
        .get("token_endpoint_auth_method")
        .and_then(Value::as_str)
        .unwrap_or("client_secret_basic");
    if !matches!(auth_method, "client_secret_basic" | "client_secret_post") {
        return oauth_json_error_response(
            400,
            "invalid_client_metadata",
            "unsupported token endpoint auth method",
        );
    }
    let client = LocalOAuthRegisteredClient {
        client_id: format!("vulcan-dcr-{}", Ulid::new()),
        client_secret: Ulid::new().to_string(),
        redirect_uris,
        client_name: payload
            .get("client_name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        token_endpoint_auth_method: auth_method.to_string(),
        client_id_issued_at: current_unix_timestamp(),
    };
    context
        .oauth_clients
        .lock()
        .expect("oauth clients lock should not be poisoned")
        .insert(client.client_id.clone(), client.clone());
    if let Err(error) = save_oauth_registered_clients(context) {
        return oauth_json_error_response(500, "server_error", error.to_string());
    }
    let body = serde_json::json!({
        "client_id": client.client_id,
        "client_secret": client.client_secret,
        "client_id_issued_at": client.client_id_issued_at,
        "client_secret_expires_at": 0,
        "redirect_uris": client.redirect_uris,
        "grant_types": ["authorization_code"],
        "response_types": ["code"],
        "token_endpoint_auth_method": client.token_endpoint_auth_method,
    });
    McpHttpResponse {
        status: 201,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&body).expect("json should serialize"),
        extra_headers: vec![("Cache-Control".to_string(), "no-store".to_string())],
    }
}

#[cfg(feature = "oauth")]
fn handle_local_oauth_token(
    context: &McpHttpServerContext,
    issuer: &LocalOAuthIssuer,
    request: &McpHttpRequest,
) -> McpHttpResponse {
    if request.method != "POST" {
        return oauth_json_error_response(405, "invalid_request", "method not allowed");
    }
    let params = parse_form_params(&request.body);
    let Some((client_id, client_secret)) = oauth_client_credentials(request, &params) else {
        return oauth_json_error_response(
            401,
            "invalid_client",
            "missing OAuth client credentials",
        );
    };
    if !issuer.verify_client(&client_id, &client_secret)
        && !local_oauth_registered_client_valid(context, &client_id, &client_secret)
    {
        return oauth_json_error_response(
            401,
            "invalid_client",
            "invalid OAuth client credentials",
        );
    }
    if params.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return oauth_json_error_response(400, "unsupported_grant_type", "unsupported grant type");
    }
    let Some(code) = params.get("code") else {
        return oauth_json_error_response(400, "invalid_request", "missing authorization code");
    };
    let Some(code_verifier) = params.get("code_verifier") else {
        return oauth_json_error_response(400, "invalid_request", "missing PKCE verifier");
    };
    let code_record = context
        .oauth_codes
        .lock()
        .expect("oauth code lock should not be poisoned")
        .remove(code);
    let Some(code_record) = code_record else {
        return oauth_json_error_response(400, "invalid_grant", "unknown authorization code");
    };
    if code_record.expires_at < std::time::Instant::now() {
        return oauth_json_error_response(400, "invalid_grant", "expired authorization code");
    }
    if code_record.client_id != client_id {
        return oauth_json_error_response(
            400,
            "invalid_grant",
            "authorization code client mismatch",
        );
    }
    if params.get("redirect_uri") != Some(&code_record.redirect_uri) {
        return oauth_json_error_response(
            400,
            "invalid_grant",
            "authorization code redirect mismatch",
        );
    }
    if !issuer.verify_pkce_s256(code_verifier, &code_record.code_challenge) {
        return oauth_json_error_response(400, "invalid_grant", "invalid PKCE verifier");
    }
    match issuer.issue_access_token_for(
        &code_record.subject,
        code_record.email,
        code_record.permission_profile,
    ) {
        Ok(access_token) => {
            let body = serde_json::json!({
                "access_token": access_token,
                "token_type": "Bearer",
                "expires_in": 3600,
                "scope": "openid email profile",
            });
            McpHttpResponse {
                status: 200,
                content_type: Some("application/json"),
                body: serde_json::to_vec(&body).expect("json should serialize"),
                extra_headers: vec![("Cache-Control".to_string(), "no-store".to_string())],
            }
        }
        Err(error) => oauth_json_error_response(500, "server_error", error.to_string()),
    }
}

#[cfg(feature = "oauth")]
fn handle_local_oauth_indieauth_callback(
    context: &McpHttpServerContext,
    issuer: &LocalOAuthIssuer,
    request: &McpHttpRequest,
) -> McpHttpResponse {
    if request.method != "GET" {
        return oauth_plain_response(405, "method not allowed");
    }
    let Some(indieauth) = context.oauth_indieauth.as_ref() else {
        return oauth_plain_response(404, "not found");
    };
    let params = parse_query_params(&request.query);
    if let Some(error) = params.get("error") {
        return oauth_plain_response(400, &format!("IndieAuth failed: {error}"));
    }
    let Some(state) = params.get("state") else {
        return oauth_plain_response(400, "missing IndieAuth state");
    };
    let pending = context
        .oauth_pending_indieauth
        .lock()
        .expect("oauth pending indieauth lock should not be poisoned")
        .remove(state);
    let Some(pending) = pending else {
        return oauth_plain_response(400, "unknown IndieAuth state");
    };
    if pending.expires_at < std::time::Instant::now() {
        return oauth_plain_response(400, "expired IndieAuth state");
    }
    let Some(code) = params.get("code") else {
        return oauth_plain_response(400, "missing IndieAuth code");
    };
    let subject = match exchange_indieauth_code(
        &indieauth.token_endpoint,
        code,
        &indieauth.redirect_uri,
        &indieauth.client_id,
        &pending.indieauth_code_verifier,
    ) {
        Ok(subject) => subject,
        Err(error) => return oauth_plain_response(400, &error.to_string()),
    };
    let Some(user) = issuer.user_for_subject(&subject) else {
        return oauth_plain_response(403, "IndieAuth subject is not allowed");
    };
    let code = Ulid::new().to_string();
    context
        .oauth_codes
        .lock()
        .expect("oauth code lock should not be poisoned")
        .insert(
            code.clone(),
            LocalOAuthCode {
                client_id: pending.client_id,
                redirect_uri: pending.redirect_uri.clone(),
                code_challenge: pending.code_challenge,
                subject: user.subject,
                email: user.email,
                permission_profile: user.permission_profile,
                expires_at: std::time::Instant::now() + Duration::from_secs(300),
            },
        );
    let mut location = format!("{}?code={}", pending.redirect_uri, percent_encode(&code));
    if let Some(state) = pending.state {
        location.push_str("&state=");
        location.push_str(&percent_encode(&state));
    }
    McpHttpResponse {
        status: 302,
        content_type: None,
        body: Vec::new(),
        extra_headers: vec![("Location".to_string(), location)],
    }
}

#[cfg(feature = "oauth")]
fn local_oauth_redirect_to_indieauth(
    indieauth: &LocalOAuthIndieAuthConfig,
    state: &str,
    code_challenge: &str,
) -> McpHttpResponse {
    let mut location = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256",
        indieauth.authorization_endpoint,
        percent_encode(&indieauth.client_id),
        percent_encode(&indieauth.redirect_uri),
        percent_encode(state),
        percent_encode(code_challenge)
    );
    if let Some(me) = indieauth.me.as_ref() {
        location.push_str("&me=");
        location.push_str(&percent_encode(me));
    }
    McpHttpResponse {
        status: 302,
        content_type: None,
        body: Vec::new(),
        extra_headers: vec![("Location".to_string(), location)],
    }
}

#[cfg(feature = "oauth")]
fn local_oauth_approval_form(params: &BTreeMap<String, String>) -> McpHttpResponse {
    let mut action = "/oauth/authorize?".to_string();
    let mut first = true;
    for (key, value) in params
        .iter()
        .filter(|(key, _)| key.as_str() != "approval_token")
    {
        if !first {
            action.push('&');
        }
        first = false;
        action.push_str(&percent_encode(key));
        action.push('=');
        action.push_str(&percent_encode(value));
    }
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Authorize Vulcan MCP</title></head>\
         <body><main><h1>Authorize Vulcan MCP</h1>\
         <form method=\"get\" action=\"{}\">\
         <label>Approval token <input name=\"approval_token\" type=\"password\" autocomplete=\"one-time-code\" autofocus></label>\
         <button type=\"submit\">Authorize</button>\
         </form></main></body></html>",
        html_escape(&action)
    );
    McpHttpResponse {
        status: 200,
        content_type: Some("text/html; charset=utf-8"),
        body: html.into_bytes(),
        extra_headers: Vec::new(),
    }
}

#[cfg(feature = "oauth")]
fn local_oauth_client_redirect_allowed(
    context: &McpHttpServerContext,
    issuer: &LocalOAuthIssuer,
    client_id: &str,
    redirect_uri: &str,
) -> bool {
    if client_id == issuer.client_id() && !redirect_uri.is_empty() {
        return true;
    }
    context
        .oauth_clients
        .lock()
        .expect("oauth clients lock should not be poisoned")
        .get(client_id)
        .is_some_and(|client| client.redirect_uris.iter().any(|uri| uri == redirect_uri))
}

#[cfg(feature = "oauth")]
fn local_oauth_registered_client_valid(
    context: &McpHttpServerContext,
    client_id: &str,
    client_secret: &str,
) -> bool {
    context
        .oauth_clients
        .lock()
        .expect("oauth clients lock should not be poisoned")
        .get(client_id)
        .is_some_and(|client| client.client_secret == client_secret)
}

#[cfg(feature = "oauth")]
fn local_oauth_redirect_host_allowed(context: &McpHttpServerContext, redirect_uri: &str) -> bool {
    let Some((scheme, rest)) = redirect_uri.split_once("://") else {
        return false;
    };
    if scheme != "https" {
        return false;
    }
    let host = rest
        .split('/')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default();
    context
        .oauth_dcr_allowed_redirect_hosts
        .iter()
        .any(|allowed| host == allowed || host.ends_with(&format!(".{allowed}")))
}

#[cfg(feature = "oauth")]
fn oauth_clients_path(paths: &VaultPaths) -> std::path::PathBuf {
    paths.vulcan_dir().join("mcp-oauth-clients.json")
}

#[cfg(feature = "oauth")]
fn oauth_issuer_secret_path(paths: &VaultPaths) -> PathBuf {
    paths.vulcan_dir().join("mcp-oauth-issuer-secret")
}

#[cfg(feature = "oauth")]
fn generate_pkce_verifier() -> String {
    format!(
        "{}{}{}{}",
        Ulid::new(),
        Ulid::new(),
        Ulid::new(),
        Ulid::new()
    )
}

#[cfg(feature = "oauth")]
fn load_or_create_local_oauth_issuer_secret(paths: &VaultPaths) -> Result<String, CliError> {
    let path = oauth_issuer_secret_path(paths);
    if path.exists() {
        let secret = fs::read_to_string(&path).map_err(CliError::operation)?;
        let secret = secret.trim().to_string();
        if secret.is_empty() {
            return Err(CliError::operation(format!(
                "{} is empty; remove it so Vulcan can regenerate the OAuth issuer secret",
                path.display()
            )));
        }
        return Ok(secret);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    let secret = generate_pkce_verifier();
    write_secret_file(&path, &secret)?;
    Ok(secret)
}

#[cfg(all(unix, feature = "oauth"))]
fn write_secret_file(path: &Path, secret: &str) -> Result<(), CliError> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(CliError::operation)?;
    file.write_all(secret.as_bytes())
        .map_err(CliError::operation)?;
    file.write_all(b"\n").map_err(CliError::operation)
}

#[cfg(all(not(unix), feature = "oauth"))]
fn write_secret_file(path: &Path, secret: &str) -> Result<(), CliError> {
    fs::write(path, format!("{secret}\n")).map_err(CliError::operation)
}

#[cfg(feature = "oauth")]
fn load_oauth_registered_clients(
    paths: &VaultPaths,
) -> Result<BTreeMap<String, LocalOAuthRegisteredClient>, CliError> {
    let path = oauth_clients_path(paths);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let content = fs::read_to_string(path).map_err(CliError::operation)?;
    let clients = serde_json::from_str::<Vec<LocalOAuthRegisteredClient>>(&content)
        .map_err(CliError::operation)?;
    Ok(clients
        .into_iter()
        .map(|client| (client.client_id.clone(), client))
        .collect())
}

#[cfg(feature = "oauth")]
fn save_oauth_registered_clients(context: &McpHttpServerContext) -> Result<(), CliError> {
    let Some(path) = context.oauth_clients_path.as_ref() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    let clients = context
        .oauth_clients
        .lock()
        .expect("oauth clients lock should not be poisoned")
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let serialized = serde_json::to_string_pretty(&clients).map_err(CliError::operation)?;
    fs::write(path, serialized).map_err(CliError::operation)
}

#[cfg(feature = "oauth")]
fn build_indieauth_config(
    options: &McpHttpOptions,
) -> Result<Option<LocalOAuthIndieAuthConfig>, CliError> {
    if options.oauth_indieauth_authorization_endpoint.is_none()
        && options.oauth_indieauth_token_endpoint.is_none()
        && options.oauth_indieauth_me.is_none()
    {
        return Ok(None);
    }
    let discovered = match (
        options.oauth_indieauth_authorization_endpoint.as_ref(),
        options.oauth_indieauth_token_endpoint.as_ref(),
        options.oauth_indieauth_me.as_ref(),
    ) {
        (Some(_), Some(_), _) => None,
        (_, _, Some(me)) => Some(discover_indieauth_endpoints(me).map_err(CliError::operation)?),
        _ => {
            return Err(CliError::operation(
                "--oauth-indieauth-me is required unless both IndieAuth endpoints are provided",
            ))
        }
    };
    let authorization_endpoint = options
        .oauth_indieauth_authorization_endpoint
        .clone()
        .or_else(|| {
            discovered
                .as_ref()
                .map(|endpoints| endpoints.authorization_endpoint.clone())
        })
        .ok_or_else(|| CliError::operation("IndieAuth authorization endpoint is required"))?;
    let token_endpoint = options
        .oauth_indieauth_token_endpoint
        .clone()
        .or_else(|| {
            discovered
                .as_ref()
                .map(|endpoints| endpoints.token_endpoint.clone())
        })
        .ok_or_else(|| CliError::operation("IndieAuth token endpoint is required"))?;
    let public_url = options
        .public_url
        .as_deref()
        .ok_or_else(|| CliError::operation("--public-url is required with IndieAuth options"))?;
    let origin = public_url_origin_for_cli(public_url)?;
    let client_id = options
        .oauth_indieauth_client_id
        .clone()
        .unwrap_or_else(|| origin.clone());
    let redirect_uri = options
        .oauth_indieauth_redirect_uri
        .clone()
        .unwrap_or_else(|| format!("{origin}/oauth/indieauth/callback"));
    Ok(Some(LocalOAuthIndieAuthConfig {
        authorization_endpoint,
        token_endpoint,
        client_id,
        redirect_uri,
        me: options.oauth_indieauth_me.clone(),
    }))
}

#[cfg(feature = "oauth")]
fn public_url_origin_for_cli(public_url: &str) -> Result<String, CliError> {
    let Some((scheme, rest)) = public_url.split_once("://") else {
        return Err(CliError::operation("--public-url must be absolute"));
    };
    let host = rest.split('/').next().unwrap_or(rest);
    Ok(format!("{scheme}://{host}"))
}

#[cfg(feature = "oauth")]
fn parse_local_oauth_users(users: &[String]) -> Result<Vec<LocalOAuthUserConfig>, CliError> {
    users
        .iter()
        .map(|entry| {
            let (subject, rest) = entry
                .split_once('=')
                .ok_or_else(|| CliError::operation("OAuth users must be subject=profile"))?;
            if subject.is_empty() || rest.is_empty() {
                return Err(CliError::operation("OAuth users must be subject=profile"));
            }
            let (profile, email) = rest
                .split_once(',')
                .map_or((rest, None), |(profile, email)| (profile, Some(email)));
            Ok(LocalOAuthUserConfig {
                subject: subject.to_string(),
                email: email.map(ToOwned::to_owned),
                permission_profile: Some(profile.to_string()),
            })
        })
        .collect()
}

#[cfg(feature = "oauth")]
fn current_unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(feature = "oauth")]
fn oauth_client_credentials(
    request: &McpHttpRequest,
    params: &BTreeMap<String, String>,
) -> Option<(String, String)> {
    if let Some(credentials) = request
        .headers
        .get("authorization")
        .and_then(|value| value.strip_prefix("Basic "))
        .and_then(decode_basic_credentials)
    {
        return Some(credentials);
    }
    let client_id = params.get("client_id")?.clone();
    let client_secret = params.get("client_secret")?.clone();
    Some((client_id, client_secret))
}

#[cfg(feature = "oauth")]
fn decode_basic_credentials(value: &str) -> Option<(String, String)> {
    use base64::prelude::{Engine, BASE64_STANDARD};

    let decoded = BASE64_STANDARD.decode(value).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (client_id, client_secret) = decoded.split_once(':')?;
    Some((client_id.to_string(), client_secret.to_string()))
}

#[cfg(feature = "oauth")]
fn oauth_plain_response(status: u16, message: &str) -> McpHttpResponse {
    McpHttpResponse {
        status,
        content_type: Some("text/plain; charset=utf-8"),
        body: message.as_bytes().to_vec(),
        extra_headers: Vec::new(),
    }
}

#[cfg(feature = "oauth")]
fn oauth_json_error_response(
    status: u16,
    error: &str,
    error_description: impl Into<String>,
) -> McpHttpResponse {
    let body = serde_json::json!({
        "error": error,
        "error_description": error_description.into(),
    });
    McpHttpResponse {
        status,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&body).expect("json should serialize"),
        extra_headers: vec![("Cache-Control".to_string(), "no-store".to_string())],
    }
}

#[cfg(feature = "oauth")]
fn is_protected_resource_metadata_path(path: &str, endpoint: &str) -> bool {
    path == "/.well-known/oauth-protected-resource"
        || path == format!("/.well-known/oauth-protected-resource{endpoint}")
}

#[cfg(feature = "oauth")]
fn is_authorization_server_metadata_path(path: &str, endpoint: &str) -> bool {
    path == "/.well-known/oauth-authorization-server"
        || path == format!("/.well-known/oauth-authorization-server{endpoint}")
        || path == "/.well-known/openid-configuration"
        || path == format!("/.well-known/openid-configuration{endpoint}")
}

#[cfg(feature = "oauth")]
fn oauth_error_response(
    oauth: &McpOAuthMode,
    message: impl Into<String>,
    error: &str,
) -> McpHttpResponse {
    let message = message.into();
    let error_description = escape_www_authenticate_value(&message);
    let mut response = mcp_http_json_error_response(401, message, Value::Null);
    response.extra_headers.push((
        "WWW-Authenticate".to_string(),
        format!(
            "Bearer error=\"{error}\", error_description=\"{}\", resource_metadata=\"{}\"",
            error_description,
            oauth_protected_resource_metadata_url(oauth),
        ),
    ));
    response
}

#[cfg(feature = "oauth")]
fn oauth_protected_resource_metadata_url(oauth: &McpOAuthMode) -> &str {
    match oauth {
        McpOAuthMode::External(external) => external.protected_resource_metadata_url(),
        McpOAuthMode::Local(local) => local.protected_resource_metadata_url(),
    }
}

#[cfg(feature = "oauth")]
fn escape_www_authenticate_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn normalize_mcp_http_endpoint(endpoint: &str) -> String {
    if endpoint.is_empty() || endpoint == "/" {
        "/mcp".to_string()
    } else if endpoint.starts_with('/') {
        endpoint.to_string()
    } else {
        format!("/{endpoint}")
    }
}

fn validate_mcp_protocol_version(request: &McpHttpRequest) -> Option<McpHttpResponse> {
    let version = request.headers.get("mcp-protocol-version")?;
    if version == MCP_PROTOCOL_VERSION {
        None
    } else {
        Some(mcp_http_json_error_response(
            400,
            format!("unsupported MCP-Protocol-Version `{version}`"),
            Value::Null,
        ))
    }
}

fn bearer_or_shared_token(headers: &BTreeMap<String, String>) -> Option<String> {
    if let Some(token) = bearer_token(headers) {
        return Some(token);
    }
    headers.get("x-vulcan-token").cloned()
}

fn bearer_token(headers: &BTreeMap<String, String>) -> Option<String> {
    if let Some(value) = headers.get("authorization") {
        if let Some(token) = value.strip_prefix("Bearer ") {
            return Some(token.to_string());
        }
    }
    None
}

fn origin_allowed(origin: &str, bind_addr: SocketAddr) -> bool {
    let origin = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .unwrap_or(origin);
    let host = origin.split('/').next().unwrap_or_default();
    let host = host.trim_matches(|ch| ch == '[' || ch == ']');
    let host = host.split(':').next().unwrap_or(host);
    if bind_addr.ip().is_loopback() {
        matches!(host, "127.0.0.1" | "localhost" | "::1")
    } else {
        host == bind_addr.ip().to_string()
    }
}

fn read_mcp_http_request(stream: &mut TcpStream) -> Result<McpHttpRequest, String> {
    let mut buffer = Vec::new();
    let mut header_end = None;

    loop {
        let mut chunk = [0_u8; 1024];
        let bytes_read = stream.read(&mut chunk).map_err(|error| error.to_string())?;
        if bytes_read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if let Some(position) = find_bytes(&buffer, b"\r\n\r\n") {
            header_end = Some(position + 4);
            break;
        }
        if buffer.len() > 64 * 1024 {
            return Err("request headers exceed 64 KiB".to_string());
        }
    }

    let header_end = header_end.ok_or_else(|| "incomplete HTTP request".to_string())?;
    let header_text = String::from_utf8(buffer[..header_end].to_vec())
        .map_err(|_| "request headers are not valid UTF-8".to_string())?;
    let mut lines = header_text.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| "missing HTTP request line".to_string())?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| "missing HTTP method".to_string())?
        .to_string();
    let target = request_parts
        .next()
        .ok_or_else(|| "missing HTTP request target".to_string())?;
    let (path, query) = target
        .split_once('?')
        .map_or((target, ""), |(path, query)| (path, query));
    let path = path.to_string();
    let query = query.to_string();

    let headers = lines
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect::<BTreeMap<_, _>>();
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);

    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_length {
        let mut chunk = vec![0_u8; content_length - body.len()];
        let bytes_read = stream
            .read(chunk.as_mut_slice())
            .map_err(|error| error.to_string())?;
        if bytes_read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..bytes_read]);
    }

    if body.len() < content_length {
        return Err("incomplete HTTP request body".to_string());
    }

    Ok(McpHttpRequest {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn write_mcp_http_response(
    stream: &mut TcpStream,
    response: &McpHttpResponse,
) -> Result<(), io::Error> {
    let status_text = match response.status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        302 => "Found",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    let mut headers = format!("HTTP/1.1 {} {}\r\n", response.status, status_text);
    if let Some(content_type) = response.content_type {
        headers.push_str("Content-Type: ");
        headers.push_str(content_type);
        headers.push_str("\r\n");
    }
    for (name, value) in &response.extra_headers {
        headers.push_str(name);
        headers.push_str(": ");
        headers.push_str(value);
        headers.push_str("\r\n");
    }
    headers.push_str("Content-Length: ");
    headers.push_str(&response.body.len().to_string());
    headers.push_str("\r\nConnection: close\r\n\r\n");
    stream.write_all(headers.as_bytes())?;
    if !response.body.is_empty() {
        stream.write_all(&response.body)?;
    }
    stream.flush()
}

fn write_mcp_http_sse_headers(stream: &mut TcpStream) -> Result<(), io::Error> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
    )?;
    stream.flush()
}

fn write_mcp_http_sse_event(stream: &mut TcpStream, message: &Value) -> Result<(), io::Error> {
    let payload = serde_json::to_string(message).expect("sse payload should serialize");
    let event_id = Ulid::new().to_string();
    let frame = format!("id: {event_id}\nevent: message\ndata: {payload}\n\n");
    stream.write_all(frame.as_bytes())?;
    stream.flush()
}

fn write_mcp_http_sse_keepalive(stream: &mut TcpStream) -> Result<(), io::Error> {
    stream.write_all(b": keepalive\n\n")?;
    stream.flush()
}

fn mcp_http_json_error_response(
    status: u16,
    message: impl Into<String>,
    id: Value,
) -> McpHttpResponse {
    let body = jsonrpc_error(id, -32600, message.into(), None);
    McpHttpResponse {
        status,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&body).expect("json should serialize"),
        extra_headers: Vec::new(),
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(feature = "oauth")]
fn parse_query_params(query: &str) -> BTreeMap<String, String> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .filter_map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            Some((percent_decode(key)?, percent_decode(value)?))
        })
        .collect()
}

#[cfg(feature = "oauth")]
fn parse_form_params(body: &[u8]) -> BTreeMap<String, String> {
    std::str::from_utf8(body).map_or_else(|_| BTreeMap::new(), parse_query_params)
}

#[cfg(feature = "oauth")]
fn percent_decode(value: &str) -> Option<String> {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let high = hex_value(bytes[index + 1])?;
                let low = hex_value(bytes[index + 2])?;
                output.push(high * 16 + low);
                index += 3;
            }
            b'%' => return None,
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(output).ok()
}

#[cfg(feature = "oauth")]
fn percent_encode(value: &str) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            output.push(char::from(byte));
        } else {
            write!(output, "%{byte:02X}").expect("writing to a String should not fail");
        }
    }
    output
}

#[cfg(feature = "oauth")]
const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(feature = "oauth")]
fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn tool_by_name(name: &str) -> Option<&'static McpToolCatalogEntry> {
    MCP_TOOL_CATALOG.iter().find(|tool| tool.name == name)
}

const ALL_MCP_TOOL_PACKS: &[McpToolPack] = &[
    McpToolPack::NotesRead,
    McpToolPack::Search,
    McpToolPack::Status,
    McpToolPack::Custom,
    McpToolPack::Daily,
    McpToolPack::Tasks,
    McpToolPack::NotesWrite,
    McpToolPack::NotesManage,
    McpToolPack::Web,
    McpToolPack::Config,
    McpToolPack::Index,
    McpToolPack::ToolPacks,
];

fn expand_tool_pack_arg(value: McpToolPackArg) -> &'static [McpToolPack] {
    match value {
        McpToolPackArg::NotesRead => PACK_NOTES_READ,
        McpToolPackArg::Search => PACK_SEARCH,
        McpToolPackArg::Status => PACK_STATUS,
        McpToolPackArg::Custom => PACK_CUSTOM,
        McpToolPackArg::Daily => PACK_DAILY,
        McpToolPackArg::Tasks => PACK_TASKS,
        McpToolPackArg::NotesWrite => PACK_NOTES_WRITE,
        McpToolPackArg::NotesManage => PACK_NOTES_MANAGE,
        McpToolPackArg::Web => PACK_WEB,
        McpToolPackArg::Config => PACK_CONFIG,
        McpToolPackArg::Index => PACK_INDEX,
    }
}

fn resolve_selected_tool_packs(
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode: McpToolPackMode,
) -> BTreeSet<McpToolPack> {
    let mut selected = BTreeSet::new();
    let default_selection = [
        McpToolPackArg::NotesRead,
        McpToolPackArg::Search,
        McpToolPackArg::Status,
    ];
    let source = if tool_pack_args.is_empty() {
        default_selection.as_slice()
    } else {
        tool_pack_args
    };
    for value in source {
        selected.extend(expand_tool_pack_arg(*value).iter().copied());
    }
    if matches!(tool_pack_mode, McpToolPackMode::Adaptive) {
        selected.insert(McpToolPack::ToolPacks);
    }
    selected
}

fn is_default_tool_pack_args(tool_pack_args: &[McpToolPackArg]) -> bool {
    tool_pack_args
        == [
            McpToolPackArg::NotesRead,
            McpToolPackArg::Search,
            McpToolPackArg::Status,
        ]
}

fn default_openai_tool_packs() -> BTreeSet<McpToolPack> {
    ALL_MCP_TOOL_PACKS
        .iter()
        .copied()
        .filter(|pack| !matches!(pack, McpToolPack::ToolPacks))
        .collect()
}

fn pack_name_list(selected_tool_packs: &BTreeSet<McpToolPack>) -> Vec<String> {
    ALL_MCP_TOOL_PACKS
        .iter()
        .copied()
        .filter(|pack| selected_tool_packs.contains(pack))
        .map(|pack| pack.as_str().to_string())
        .collect()
}

fn parse_tool_pack_selector(value: &str) -> Option<McpToolPackArg> {
    match value {
        "notes-read" => Some(McpToolPackArg::NotesRead),
        "search" => Some(McpToolPackArg::Search),
        "status" => Some(McpToolPackArg::Status),
        "custom" => Some(McpToolPackArg::Custom),
        "daily" => Some(McpToolPackArg::Daily),
        "tasks" => Some(McpToolPackArg::Tasks),
        "notes-write" => Some(McpToolPackArg::NotesWrite),
        "notes-manage" => Some(McpToolPackArg::NotesManage),
        "web" => Some(McpToolPackArg::Web),
        "config" => Some(McpToolPackArg::Config),
        "index" => Some(McpToolPackArg::Index),
        _ => None,
    }
}

fn parse_tool_pack_selection_args(names: &[String]) -> Result<Vec<McpToolPackArg>, McpMethodError> {
    if names.is_empty() {
        return Err(McpMethodError::invalid_params(
            "`packs` must include at least one tool-pack name",
        ));
    }
    names
        .iter()
        .map(|name| {
            parse_tool_pack_selector(name).ok_or_else(|| {
                McpMethodError::invalid_params(format!("unknown tool pack `{name}`"))
            })
        })
        .collect()
}

fn visible_tool_catalog(
    selected_tool_packs: &BTreeSet<McpToolPack>,
    profile: &PermissionProfile,
) -> Vec<&'static McpToolCatalogEntry> {
    MCP_TOOL_CATALOG
        .iter()
        .filter(|tool| tool_visible(tool, profile, selected_tool_packs))
        .collect()
}

fn build_mcp_tool_registry_entries(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    selected_tool_packs: &BTreeSet<McpToolPack>,
) -> Result<Vec<ToolRegistryEntry>, CliError> {
    let selection =
        resolve_permission_profile(paths, requested_profile).map_err(permission_error_to_cli)?;
    let mut tools = visible_tool_catalog(selected_tool_packs, &selection.profile)
        .into_iter()
        .map(mcp_tool_registry_entry)
        .collect::<Vec<_>>();
    tools.extend(
        visible_custom_tools(paths, requested_profile, selected_tool_packs)?
            .iter()
            .map(custom_tool_registry_entry),
    );
    Ok(tools)
}

fn visible_custom_tools(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    selected_tool_packs: &BTreeSet<McpToolPack>,
) -> Result<Vec<crate::tools::CustomToolDescriptor>, CliError> {
    if !selected_tool_packs.contains(&McpToolPack::Custom) {
        return Ok(Vec::new());
    }
    let selected_pack_names = pack_name_list(selected_tool_packs)
        .into_iter()
        .collect::<BTreeSet<_>>();
    Ok(crate::tools::list_custom_tools(
        paths,
        active_permission_profile,
        &crate::custom_tool_registry_options(),
    )?
    .into_iter()
    .filter(|tool| tool.callable)
    .filter(|tool| custom_tool_matches_selected_packs(&tool.summary.packs, &selected_pack_names))
    .collect())
}

fn custom_tool_matches_selected_packs(
    packs: &[String],
    selected_pack_names: &BTreeSet<String>,
) -> bool {
    if packs.is_empty() {
        return selected_pack_names.contains("custom");
    }
    packs.iter().any(|pack| selected_pack_names.contains(pack))
}

fn tool_visible(
    tool: &McpToolCatalogEntry,
    profile: &PermissionProfile,
    selected_tool_packs: &BTreeSet<McpToolPack>,
) -> bool {
    if !tool
        .packs
        .iter()
        .any(|pack| selected_tool_packs.contains(pack))
    {
        return false;
    }
    tool_allowed_by_profile(tool, profile)
}

fn tool_allowed_by_profile(tool: &McpToolCatalogEntry, profile: &PermissionProfile) -> bool {
    match tool.visibility {
        McpVisibilityRequirement::None => true,
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

fn tool_names_for_pack(pack: McpToolPack, profile: &PermissionProfile) -> Vec<String> {
    MCP_TOOL_CATALOG
        .iter()
        .filter(|tool| tool.packs.contains(&pack))
        .filter(|tool| tool_allowed_by_profile(tool, profile))
        .map(|tool| tool.name.to_string())
        .collect()
}

fn mcp_tool_registry_entry(tool: &McpToolCatalogEntry) -> ToolRegistryEntry {
    ToolRegistryEntry {
        name: tool.name.to_string(),
        title: tool.title.to_string(),
        description: tool.description.to_string(),
        input_schema: (tool.input_schema)(),
        output_schema: tool.output_schema.map(|schema| schema()),
        annotations: tool.annotations,
        tool_packs: tool
            .packs
            .iter()
            .map(|pack| pack.as_str().to_string())
            .collect(),
        examples: tool
            .examples
            .iter()
            .map(|item| (*item).to_string())
            .collect(),
    }
}

fn tool_list_item(tool: &McpToolCatalogEntry) -> Value {
    mcp_tool_registry_entry(tool).to_mcp_list_item()
}

fn custom_tool_list_item(tool: &crate::tools::CustomToolDescriptor) -> Value {
    custom_tool_registry_entry(tool).to_mcp_list_item()
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
    let mut params = params.cloned().unwrap_or_else(|| Value::Object(Map::new()));
    strip_reserved_method_params(&mut params);
    serde_json::from_value(params)
        .map_err(|error| McpMethodError::invalid_params(error.to_string()))
}

fn strip_reserved_method_params(params: &mut Value) {
    if let Value::Object(object) = params {
        object.remove("_meta");
    }
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

fn parse_link_suggestion_status(value: &str) -> Result<LinkSuggestionStatus, McpMethodError> {
    match value {
        "pending" => Ok(LinkSuggestionStatus::Pending),
        "accepted" => Ok(LinkSuggestionStatus::Accepted),
        "rejected" => Ok(LinkSuggestionStatus::Rejected),
        other => Err(McpMethodError::invalid_params(format!(
            "unsupported `suggest_links.status`: {other}"
        ))),
    }
}

fn parse_search_backend(
    backend: Option<String>,
) -> Result<Option<SearchBackendArg>, McpMethodError> {
    let Some(backend) = backend else {
        return Ok(None);
    };
    let parsed = match backend.as_str() {
        "disabled" => SearchBackendArg::Disabled,
        "auto" => SearchBackendArg::Auto,
        "duckduckgo" => SearchBackendArg::Duckduckgo,
        "kagi" => SearchBackendArg::Kagi,
        "exa" => SearchBackendArg::Exa,
        "tavily" => SearchBackendArg::Tavily,
        "brave" => SearchBackendArg::Brave,
        "ollama" => SearchBackendArg::Ollama,
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

fn parse_tasks_default_source(
    value: Option<&str>,
) -> Result<Option<TasksListSourceArg>, McpMethodError> {
    match value {
        None => Ok(None),
        Some("all") => Ok(Some(TasksListSourceArg::All)),
        Some("inline") => Ok(Some(TasksListSourceArg::Inline)),
        Some("tasknotes" | "file") => Ok(Some(TasksListSourceArg::Tasknotes)),
        Some(other) => Err(McpMethodError::invalid_params(format!(
            "unsupported `task_list.source`: {other}"
        ))),
    }
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

fn timeout_response_for_request(request: &Value, timeout: Duration) -> Option<Value> {
    let id = request_id(request)?;
    let message = format!(
        "MCP request timed out after {}ms",
        timeout.as_millis().max(1)
    );
    if request_method(request) == Some("tools/call") {
        Some(tool_error_response(
            id,
            message.clone(),
            Some(serde_json::json!({
                "error": message,
                "timed_out": true,
                "timeout_ms": timeout.as_millis().max(1),
            })),
        ))
    } else {
        Some(jsonrpc_error(
            id,
            -32000,
            message,
            Some(serde_json::json!({
                "timed_out": true,
                "timeout_ms": timeout.as_millis().max(1),
            })),
        ))
    }
}

fn timeout_http_result(request: &Value, timeout: Duration) -> McpHttpProcessResult {
    let response = timeout_response_for_request(request, timeout);
    McpHttpProcessResult {
        accepted_notification: response.is_none(),
        response,
        notifications: Vec::new(),
    }
}

fn request_id(request: &Value) -> Option<Value> {
    request
        .as_object()
        .and_then(|object| object.get("id"))
        .cloned()
}

fn request_method(request: &Value) -> Option<&str> {
    request
        .as_object()
        .and_then(|object| object.get("method"))
        .and_then(Value::as_str)
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

fn default_suggest_min_score() -> f64 {
    0.0
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
        McpVisibilityRequirement::None => "session access",
        McpVisibilityRequirement::Read => "read access",
        McpVisibilityRequirement::Write => "write access",
        McpVisibilityRequirement::Network => "network access",
        McpVisibilityRequirement::Index => "index access",
        McpVisibilityRequirement::ConfigRead => "config read access",
        McpVisibilityRequirement::ConfigWrite => "config write access",
    }
}

fn tool_fingerprint(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    selected_tool_packs: &BTreeSet<McpToolPack>,
    profile: &PermissionProfile,
) -> String {
    let mut parts = visible_tool_catalog(selected_tool_packs, profile)
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if let Ok(custom_tools) =
        visible_custom_tools(paths, active_permission_profile, selected_tool_packs)
    {
        if !custom_tools.is_empty() {
            parts.extend(
                custom_tools
                    .into_iter()
                    .map(|tool| format!("custom:{}", tool.summary.name)),
            );
            parts.push(path_tree_fingerprint(&assistant_skills_root(paths)));
        }
    }
    parts.join("\n")
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

#[cfg(test)]
mod tests;
