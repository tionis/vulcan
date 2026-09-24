//! Shared MCP tool catalog and permission-filtered discovery.

#![allow(clippy::must_use_candidate)]

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use vulcan_core::{ConfigPermissionMode, PermissionMode, PermissionProfile};

use crate::mcp_schemas::{
    config_set_input_schema, config_set_output_schema, config_show_input_schema,
    config_show_output_schema, daily_input_schema, daily_list_input_schema, daily_output_schema,
    daily_show_input_schema, empty_object_schema, generic_report_output_schema,
    graph_communities_input_schema, index_scan_input_schema, index_scan_output_schema,
    note_append_input_schema, note_append_output_schema, note_create_input_schema,
    note_create_output_schema, note_delete_input_schema, note_delete_output_schema,
    note_get_input_schema, note_get_output_schema, note_info_input_schema, note_info_output_schema,
    note_outline_input_schema, note_outline_output_schema, note_patch_input_schema,
    note_patch_output_schema, note_set_input_schema, note_set_output_schema, query_input_schema,
    search_input_schema, search_output_schema, status_output_schema, suggest_links_input_schema,
    sync_conflicts_input_schema, sync_doctor_input_schema, sync_plan_input_schema,
    sync_status_input_schema, task_complete_input_schema, task_create_input_schema,
    task_list_input_schema, task_query_input_schema, task_reschedule_input_schema,
    tool_pack_mutation_input_schema, tool_pack_state_output_schema, web_fetch_input_schema,
    web_fetch_output_schema, web_search_input_schema, web_search_output_schema,
};

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct McpToolAnnotations {
    #[serde(rename = "readOnlyHint")]
    pub read_only_hint: bool,
    #[serde(rename = "destructiveHint")]
    pub destructive_hint: bool,
    #[serde(rename = "idempotentHint")]
    pub idempotent_hint: bool,
    #[serde(rename = "openWorldHint")]
    pub open_world_hint: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolPackMode {
    Static,
    Adaptive,
}

impl McpToolPackMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Adaptive => "adaptive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum McpToolPack {
    NotesRead,
    Search,
    Status,
    Graph,
    Custom,
    Daily,
    Tasks,
    NotesWrite,
    NotesManage,
    Web,
    Config,
    Index,
    ToolPacks,
    Sync,
}

impl McpToolPack {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotesRead => "notes-read",
            Self::Search => "search",
            Self::Status => "status",
            Self::Graph => "graph",
            Self::Custom => "custom",
            Self::Daily => "daily",
            Self::Tasks => "tasks",
            Self::NotesWrite => "notes-write",
            Self::NotesManage => "notes-manage",
            Self::Web => "web",
            Self::Config => "config",
            Self::Index => "index",
            Self::ToolPacks => "tool-packs",
            Self::Sync => "sync",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::NotesRead => "Read note content and outlines for scoped follow-up work.",
            Self::Search => "Search the vault with structured hits and snippets.",
            Self::Status => "Inspect vault status, cache metadata, and git summary.",
            Self::Graph => "Inspect graph communities and link suggestions.",
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
            Self::Sync => "Inspect and plan Git-backed vault synchronization without mutation.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolId {
    NoteGet,
    NoteOutline,
    Capabilities,
    Search,
    Query,
    Status,
    Daily,
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
    ToolPacks,
    SyncStatus,
    SyncPlan,
    SyncDoctor,
    SyncConflicts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpVisibilityRequirement {
    None,
    Read,
    Write,
    Network,
    Index,
    ConfigRead,
    ConfigWrite,
    GitReadAll,
}

#[derive(Debug, Clone, Copy)]
pub struct McpToolCatalogEntry {
    pub id: McpToolId,
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub packs: &'static [McpToolPack],
    pub visibility: McpVisibilityRequirement,
    pub annotations: McpToolAnnotations,
    pub input_schema: fn() -> Value,
    pub output_schema: Option<fn() -> Value>,
    pub examples: &'static [&'static str],
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

pub const PACK_NOTES_READ: &[McpToolPack] = &[McpToolPack::NotesRead];
pub const PACK_SEARCH: &[McpToolPack] = &[McpToolPack::Search];
pub const PACK_STATUS: &[McpToolPack] = &[McpToolPack::Status];
pub const PACK_GRAPH: &[McpToolPack] = &[McpToolPack::Graph];
pub const PACK_CUSTOM: &[McpToolPack] = &[McpToolPack::Custom];
pub const PACK_DAILY: &[McpToolPack] = &[McpToolPack::Daily];
const PACK_DAILY_READ: &[McpToolPack] = &[McpToolPack::NotesRead, McpToolPack::Daily];
pub const PACK_TASKS: &[McpToolPack] = &[McpToolPack::Tasks];
pub const PACK_NOTES_WRITE: &[McpToolPack] = &[McpToolPack::NotesWrite];
pub const PACK_NOTES_MANAGE: &[McpToolPack] = &[McpToolPack::NotesManage];
pub const PACK_WEB: &[McpToolPack] = &[McpToolPack::Web];
pub const PACK_CONFIG: &[McpToolPack] = &[McpToolPack::Config];
pub const PACK_INDEX: &[McpToolPack] = &[McpToolPack::Index];
const PACK_TOOL_PACKS: &[McpToolPack] = &[McpToolPack::ToolPacks];
pub const PACK_SYNC: &[McpToolPack] = &[McpToolPack::Sync];

pub const MCP_TOOL_CATALOG: &[McpToolCatalogEntry] = &[
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
        id: McpToolId::Capabilities,
        name: "capabilities",
        title: "Inspect MCP Capabilities",
        description: "Return compact routing guidance, active tools, startup-pinned and optional packs, and result-size limits for this MCP session.",
        packs: PACK_STATUS,
        visibility: McpVisibilityRequirement::None,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: empty_object_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["capabilities {}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::Daily,
        name: "daily",
        title: "Read Daily Notes",
        description: "Read structurally known daily notes. Use operation=latest for the newest existing daily note, operation=today only for today's date, show for a known date, and list/range for windows. Prefer this over search or generic query for journal requests.",
        packs: PACK_DAILY_READ,
        visibility: McpVisibilityRequirement::Read,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: daily_input_schema,
        output_schema: Some(daily_output_schema),
        examples: &["daily {\"operation\":\"latest\",\"include_content\":true}"],
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
        packs: PACK_GRAPH,
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
        packs: PACK_GRAPH,
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
        id: McpToolId::SyncStatus,
        name: "sync_status",
        title: "Inspect Sync Status",
        description: "Inspect the selected vault's Git sync state and exact remote live ref without capturing, publishing, or applying files.",
        packs: PACK_SYNC,
        visibility: McpVisibilityRequirement::GitReadAll,
        annotations: mcp_annotations(true, false, true, true),
        input_schema: sync_status_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["sync_status {}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::SyncPlan,
        name: "sync_plan",
        title: "Plan One Sync Cycle",
        description: "Build the same mutation-free finite-cycle preview as `vulcan sync run --dry-run` for the selected vault.",
        packs: PACK_SYNC,
        visibility: McpVisibilityRequirement::GitReadAll,
        annotations: mcp_annotations(true, false, true, true),
        input_schema: sync_plan_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["sync_plan {\"remote\":\"origin\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::SyncDoctor,
        name: "sync_doctor",
        title: "Diagnose Git Sync",
        description: "Run read-only Git layout, ref, journal, filter, cache, and target-platform synchronization diagnostics.",
        packs: PACK_SYNC,
        visibility: McpVisibilityRequirement::GitReadAll,
        annotations: mcp_annotations(true, false, true, true),
        input_schema: sync_doctor_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["sync_doctor {}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::SyncConflicts,
        name: "sync_conflicts",
        title: "Inspect Preserved Sync Conflicts",
        description: "List unresolved preserved conflicts or inspect one immutable conflict record; this tool never resolves a conflict.",
        packs: PACK_SYNC,
        visibility: McpVisibilityRequirement::GitReadAll,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: sync_conflicts_input_schema,
        output_schema: Some(generic_report_output_schema),
        examples: &["sync_conflicts {}", "sync_conflicts {\"conflict_id\":\"01...\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::ToolPacks,
        name: "tool_packs",
        title: "Manage MCP Tool Packs",
        description: "List or change optional packs for this session. Startup-selected packs are pinned; registry changes require the client to refresh tools/list.",
        packs: PACK_TOOL_PACKS,
        visibility: McpVisibilityRequirement::None,
        annotations: mcp_annotations(false, false, true, false),
        input_schema: tool_pack_mutation_input_schema,
        output_schema: Some(tool_pack_state_output_schema),
        examples: &["tool_packs {\"operation\":\"enable\",\"packs\":[\"web\"]}"],
    },
];

pub const ALL_MCP_TOOL_PACKS: &[McpToolPack] = &[
    McpToolPack::NotesRead,
    McpToolPack::Search,
    McpToolPack::Status,
    McpToolPack::Graph,
    McpToolPack::Custom,
    McpToolPack::Daily,
    McpToolPack::Tasks,
    McpToolPack::NotesWrite,
    McpToolPack::NotesManage,
    McpToolPack::Web,
    McpToolPack::Config,
    McpToolPack::Index,
    McpToolPack::ToolPacks,
    McpToolPack::Sync,
];

pub fn tool_by_name(name: &str) -> Option<&'static McpToolCatalogEntry> {
    MCP_TOOL_CATALOG.iter().find(|tool| tool.name == name)
}

pub fn resolve_selected_tool_packs(
    requested: &[McpToolPack],
    mode: McpToolPackMode,
) -> BTreeSet<McpToolPack> {
    let defaults = [
        McpToolPack::NotesRead,
        McpToolPack::Search,
        McpToolPack::Status,
    ];
    let source = if requested.is_empty() {
        defaults.as_slice()
    } else {
        requested
    };
    let mut selected: BTreeSet<McpToolPack> = source.iter().copied().collect();
    if matches!(mode, McpToolPackMode::Adaptive) {
        selected.insert(McpToolPack::ToolPacks);
    }
    selected
}

pub fn default_openai_tool_packs() -> BTreeSet<McpToolPack> {
    ALL_MCP_TOOL_PACKS
        .iter()
        .copied()
        .filter(|pack| !matches!(pack, McpToolPack::ToolPacks))
        .collect()
}

pub fn pack_name_list(selected_tool_packs: &BTreeSet<McpToolPack>) -> Vec<String> {
    ALL_MCP_TOOL_PACKS
        .iter()
        .copied()
        .filter(|pack| selected_tool_packs.contains(pack))
        .map(|pack| pack.as_str().to_string())
        .collect()
}

pub fn visible_tool_catalog(
    selected_tool_packs: &BTreeSet<McpToolPack>,
    profile: &PermissionProfile,
) -> Vec<&'static McpToolCatalogEntry> {
    MCP_TOOL_CATALOG
        .iter()
        .filter(|tool| tool_visible(tool, profile, selected_tool_packs))
        .collect()
}

pub fn tool_visible(
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

pub fn tool_allowed_by_profile(tool: &McpToolCatalogEntry, profile: &PermissionProfile) -> bool {
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
        McpVisibilityRequirement::GitReadAll => {
            matches!(profile.git, PermissionMode::Allow) && profile.read.is_all()
        }
    }
}

pub fn tool_names_for_pack(pack: McpToolPack, profile: &PermissionProfile) -> Vec<String> {
    MCP_TOOL_CATALOG
        .iter()
        .filter(|tool| tool.packs.contains(&pack))
        .filter(|tool| tool_allowed_by_profile(tool, profile))
        .map(|tool| tool.name.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_packs_and_permissions_are_shared_host_contracts() {
        let defaults = resolve_selected_tool_packs(&[], McpToolPackMode::Static);
        assert_eq!(
            pack_name_list(&defaults),
            ["notes-read", "search", "status"]
        );
        let adaptive = resolve_selected_tool_packs(&[McpToolPack::Web], McpToolPackMode::Adaptive);
        assert!(adaptive.contains(&McpToolPack::ToolPacks));
        let readonly = PermissionProfile::readonly();
        assert!(
            tool_by_name("note_get").is_some_and(|tool| tool_visible(tool, &readonly, &defaults))
        );
        assert!(!tool_by_name("note_create")
            .is_some_and(|tool| tool_allowed_by_profile(tool, &readonly)));
    }

    #[test]
    fn every_catalog_tool_has_a_unique_name_and_valid_schema_pair() {
        let mut names = BTreeSet::new();
        for tool in MCP_TOOL_CATALOG {
            assert!(names.insert(tool.name), "duplicate MCP tool {}", tool.name);
            let input = (tool.input_schema)();
            assert_eq!(input["type"], "object", "tool {}", tool.name);
            assert!(tool.output_schema.is_some(), "tool {}", tool.name);
        }
    }
}
