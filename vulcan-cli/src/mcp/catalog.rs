use serde_json::Value;

use crate::{McpToolAnnotations, McpToolPackModeArg};

use super::schemas::{
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum McpToolPackMode {
    Static,
    Adaptive,
}

impl McpToolPackMode {
    pub(super) const fn as_str(self) -> &'static str {
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
pub(super) enum McpToolPack {
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
    pub(super) const fn as_str(self) -> &'static str {
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

    pub(super) const fn description(self) -> &'static str {
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
pub(super) enum McpToolId {
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
pub(super) enum McpVisibilityRequirement {
    None,
    Read,
    Write,
    Network,
    Index,
    ConfigRead,
    ConfigWrite,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct McpToolCatalogEntry {
    pub(super) id: McpToolId,
    pub(super) name: &'static str,
    pub(super) title: &'static str,
    pub(super) description: &'static str,
    pub(super) packs: &'static [McpToolPack],
    pub(super) visibility: McpVisibilityRequirement,
    pub(super) annotations: McpToolAnnotations,
    pub(super) input_schema: fn() -> Value,
    pub(super) output_schema: Option<fn() -> Value>,
    pub(super) examples: &'static [&'static str],
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

pub(super) const PACK_NOTES_READ: &[McpToolPack] = &[McpToolPack::NotesRead];
pub(super) const PACK_SEARCH: &[McpToolPack] = &[McpToolPack::Search];
pub(super) const PACK_STATUS: &[McpToolPack] = &[McpToolPack::Status];
pub(super) const PACK_CUSTOM: &[McpToolPack] = &[McpToolPack::Custom];
pub(super) const PACK_DAILY: &[McpToolPack] = &[McpToolPack::Daily];
pub(super) const PACK_TASKS: &[McpToolPack] = &[McpToolPack::Tasks];
pub(super) const PACK_NOTES_WRITE: &[McpToolPack] = &[McpToolPack::NotesWrite];
const PACK_NOTES_READ_WRITE: &[McpToolPack] = &[McpToolPack::NotesRead, McpToolPack::NotesWrite];
pub(super) const PACK_NOTES_MANAGE: &[McpToolPack] = &[McpToolPack::NotesManage];
pub(super) const PACK_WEB: &[McpToolPack] = &[McpToolPack::Web];
pub(super) const PACK_CONFIG: &[McpToolPack] = &[McpToolPack::Config];
pub(super) const PACK_INDEX: &[McpToolPack] = &[McpToolPack::Index];
const PACK_TOOL_PACKS: &[McpToolPack] = &[McpToolPack::ToolPacks];

pub(super) const MCP_TOOL_CATALOG: &[McpToolCatalogEntry] = &[
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
