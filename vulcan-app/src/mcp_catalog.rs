//! Shared MCP tool catalog and permission-filtered discovery.

#![allow(clippy::must_use_candidate)]

use serde::Serialize;
use serde_json::Value;
use vulcan_core::{ConfigPermissionMode, PermissionMode, PermissionProfile};

use crate::mcp_protocol::McpMethodError;
use crate::mcp_schemas::{
    changeset_begin_input_schema, changeset_id_input_schema, changeset_land_input_schema,
    changeset_list_input_schema, config_get_input_schema, config_get_output_schema,
    config_set_input_schema, config_set_output_schema, daily_input_schema, daily_output_schema,
    empty_object_schema, git_blame_input_schema, git_commit_input_schema, git_diff_input_schema,
    git_log_input_schema, git_status_input_schema, graph_communities_input_schema,
    index_scan_input_schema, index_scan_output_schema, link_suggestion_resolve_input_schema,
    link_suggestions_input_schema, note_append_input_schema, note_append_output_schema,
    note_create_input_schema, note_create_output_schema, note_delete_input_schema,
    note_delete_output_schema, note_get_input_schema, note_get_output_schema,
    note_info_input_schema, note_info_output_schema, note_move_input_schema,
    note_outline_input_schema, note_outline_output_schema, note_patch_input_schema,
    note_patch_output_schema, note_set_input_schema, note_set_output_schema, query_input_schema,
    search_input_schema, search_output_schema, status_output_schema, sync_conflicts_input_schema,
    sync_doctor_input_schema, sync_status_input_schema, task_complete_input_schema,
    task_create_input_schema, task_query_input_schema, task_reminder_add_input_schema,
    task_reminder_remove_input_schema, task_reschedule_input_schema, web_fetch_input_schema,
    web_fetch_output_schema, web_search_input_schema, web_search_output_schema,
};
use crate::mcp_schemas::{
    empty_input_schema, file_get_input_schema, file_put_input_schema, operation_get_input_schema,
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
pub enum McpToolId {
    NoteGet,
    NoteOutline,
    Search,
    Query,
    Status,
    Daily,
    TaskQuery,
    TaskCreate,
    TaskComplete,
    TaskReschedule,
    TaskReminderAdd,
    TaskReminderRemove,
    NoteCreate,
    NoteAppend,
    NotePatch,
    NoteInfo,
    NoteSet,
    NoteDelete,
    WebSearch,
    WebFetch,
    ConfigGet,
    ConfigSet,
    IndexScan,
    GraphCommunities,
    LinkSuggestions,
    LinkSuggestionResolve,
    SyncStatus,
    SyncDoctor,
    SyncConflicts,
    SyncRefresh,
    SyncResume,
    FileGet,
    FilePut,
    NoteMove,
    GitStatus,
    GitDiff,
    GitLog,
    GitBlame,
    GitCommit,
    ChangesetBegin,
    ChangesetList,
    ChangesetLand,
    ChangesetAbandon,
    OperationGet,
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
    /// Git access with full-vault read and write: a commit takes every change.
    GitCommit,
    Refactor,
}

#[derive(Debug, Clone, Copy)]
pub struct McpToolCatalogEntry {
    pub id: McpToolId,
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub visibility: McpVisibilityRequirement,
    /// Also needs the profile's `destructive` permission (overwrite or delete).
    pub destructive: bool,
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

pub const MCP_TOOL_CATALOG: &[McpToolCatalogEntry] = &[
    McpToolCatalogEntry {
        id: McpToolId::NoteGet,
        name: "note_get",
        title: "Read Note Content",
        description: "Read one note or markdown file, optionally selecting a section, heading, block, or line range.",
        visibility: McpVisibilityRequirement::Read,
        destructive: false,
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
        visibility: McpVisibilityRequirement::Read,
        destructive: false,
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
        visibility: McpVisibilityRequirement::Read,
        destructive: false,
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
        visibility: McpVisibilityRequirement::Read,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: query_input_schema,
        output_schema: None,
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
        visibility: McpVisibilityRequirement::Read,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: empty_object_schema,
        output_schema: Some(status_output_schema),
        examples: &["vulcan status --output json"],
    },
    McpToolCatalogEntry {
        id: McpToolId::Daily,
        name: "daily",
        title: "Read Daily Notes",
        description: "Read structurally known daily notes. Use operation=latest for the newest existing daily note, operation=today only for today's date, show for a known date, and list/range for windows. Prefer this over search or generic query for journal requests.",
        visibility: McpVisibilityRequirement::Read,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: daily_input_schema,
        output_schema: Some(daily_output_schema),
        examples: &["daily {\"operation\":\"latest\",\"include_content\":true}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::GraphCommunities,
        name: "graph_communities",
        title: "Inspect Graph Communities",
        description: "Compute note-graph communities, orphan placement hints, and bridge notes.",
        visibility: McpVisibilityRequirement::Read,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: graph_communities_input_schema,
        output_schema: None,
        examples: &["vulcan graph communities --output json"],
    },
    McpToolCatalogEntry {
        id: McpToolId::LinkSuggestions,
        name: "link_suggestions",
        title: "List Link Suggestions",
        description: "List ranked suggestions for links between notes that are not linked yet, optionally for one note or one feedback status.",
        visibility: McpVisibilityRequirement::Read,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: link_suggestions_input_schema,
        output_schema: None,
        examples: &["link_suggestions {\"note\":\"Projects/Alpha\",\"limit\":5}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::LinkSuggestionResolve,
        name: "link_suggestion_resolve",
        title: "Accept Or Reject A Link Suggestion",
        description: "Accept or reject one link suggestion by its id from `link_suggestions`.",
        visibility: McpVisibilityRequirement::Write,
        destructive: false,
        annotations: mcp_annotations(false, false, true, false),
        input_schema: link_suggestion_resolve_input_schema,
        output_schema: None,
        examples: &["link_suggestion_resolve {\"id\":\"…\",\"action\":\"accept\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::TaskQuery,
        name: "task_query",
        title: "Query Tasks",
        description: "Run a Tasks plugin query source and return shaped task results. Use this when the user gives task-query semantics directly.",
        visibility: McpVisibilityRequirement::Read,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: task_query_input_schema,
        output_schema: None,
        examples: &["task_query {\"query\":\"not done\\ndue before tomorrow\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::TaskCreate,
        name: "task_create",
        title: "Create Task",
        description: "Create a task using the configured task system. Prefer this over raw note edits for new tasks.",
        visibility: McpVisibilityRequirement::Write,
        destructive: false,
        annotations: mcp_annotations(false, false, false, false),
        input_schema: task_create_input_schema,
        output_schema: None,
        examples: &["task_create {\"text\":\"Call Alex\",\"due\":\"tomorrow\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::TaskComplete,
        name: "task_complete",
        title: "Complete Task",
        description: "Mark one resolved task complete using task-aware mutation rules. Prefer this over raw note patching for task completion.",
        visibility: McpVisibilityRequirement::Write,
        destructive: false,
        annotations: mcp_annotations(false, false, true, false),
        input_schema: task_complete_input_schema,
        output_schema: None,
        examples: &["task_complete {\"task\":\"Tasks/Call Alex\",\"date\":\"today\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::TaskReschedule,
        name: "task_reschedule",
        title: "Reschedule Task",
        description: "Update a task due date using task-aware mutation rules. Prefer this over raw note patching for due-date changes.",
        visibility: McpVisibilityRequirement::Write,
        destructive: false,
        annotations: mcp_annotations(false, false, true, false),
        input_schema: task_reschedule_input_schema,
        output_schema: None,
        examples: &["task_reschedule {\"task\":\"Tasks/Call Alex\",\"due\":\"2026-05-09\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::TaskReminderAdd,
        name: "task_reminder_add",
        title: "Add Task Reminder",
        description: "Add one reminder to a TaskNotes task. Relative reminders follow the task's due (else scheduled) date, so they move with a recurring task.",
        visibility: McpVisibilityRequirement::Write,
        destructive: false,
        annotations: mcp_annotations(false, false, false, false),
        input_schema: task_reminder_add_input_schema,
        output_schema: None,
        examples: &["task_reminder_add {\"task\":\"Dentist\",\"reminder\":\"-1h\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::TaskReminderRemove,
        name: "task_reminder_remove",
        title: "Remove Task Reminder",
        description: "Remove one reminder from a TaskNotes task by its id.",
        visibility: McpVisibilityRequirement::Write,
        destructive: true,
        annotations: mcp_annotations(false, true, true, false),
        input_schema: task_reminder_remove_input_schema,
        output_schema: None,
        examples: &["task_reminder_remove {\"task\":\"Dentist\",\"id\":\"rem_1793763587775_0\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::NoteCreate,
        name: "note_create",
        title: "Create Note",
        description: "Create a new note from explicit body text, optional template, and optional frontmatter properties.",
        visibility: McpVisibilityRequirement::Write,
        destructive: false,
        annotations: mcp_annotations(false, false, false, false),
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
        visibility: McpVisibilityRequirement::Write,
        destructive: false,
        annotations: mcp_annotations(false, false, false, false),
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
        visibility: McpVisibilityRequirement::Write,
        destructive: false,
        annotations: mcp_annotations(false, false, false, false),
        input_schema: note_patch_input_schema,
        output_schema: Some(note_patch_output_schema),
        examples: &["vulcan note patch Projects/Alpha --find TODO --replace DONE"],
    },
    McpToolCatalogEntry {
        id: McpToolId::NoteInfo,
        name: "note_info",
        title: "Read Note Metadata",
        description: "Return summary metadata and graph counts for one resolved note.",
        visibility: McpVisibilityRequirement::Read,
        destructive: false,
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
        visibility: McpVisibilityRequirement::Write,
        destructive: true,
        annotations: mcp_annotations(false, true, true, false),
        input_schema: note_set_input_schema,
        output_schema: Some(note_set_output_schema),
        examples: &["vulcan note set Projects/Alpha --no-frontmatter < body.md"],
    },
    McpToolCatalogEntry {
        id: McpToolId::NoteDelete,
        name: "note_delete",
        title: "Delete Note",
        description: "Delete one note and report the backlinks that would become unresolved.",
        visibility: McpVisibilityRequirement::Write,
        destructive: true,
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
        visibility: McpVisibilityRequirement::Network,
        destructive: false,
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
        visibility: McpVisibilityRequirement::Network,
        destructive: false,
        annotations: mcp_annotations(true, false, false, true),
        input_schema: web_fetch_input_schema,
        output_schema: Some(web_fetch_output_schema),
        examples: &["vulcan web fetch https://example.com/article --mode markdown"],
    },
    McpToolCatalogEntry {
        id: McpToolId::ConfigGet,
        name: "config_get",
        title: "Read Effective Config",
        description: "Read the effective Vulcan config, optionally narrowed to one section.",
        visibility: McpVisibilityRequirement::ConfigRead,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: config_get_input_schema,
        output_schema: Some(config_get_output_schema),
        examples: &["vulcan config show periodic.daily"],
    },
    McpToolCatalogEntry {
        id: McpToolId::ConfigSet,
        name: "config_set",
        title: "Set Config Value",
        description: "Write one shared config value to `.vulcan/config.toml` using the same parser and auto-commit behavior as the CLI.",
        visibility: McpVisibilityRequirement::ConfigWrite,
        destructive: true,
        annotations: mcp_annotations(false, true, true, false),
        input_schema: config_set_input_schema,
        output_schema: Some(config_set_output_schema),
        examples: &["vulcan config set periodic.daily.template Templates/Daily"],
    },
    McpToolCatalogEntry {
        id: McpToolId::IndexScan,
        name: "index_scan",
        title: "Scan Vault Index",
        description: "Run an incremental or full vault scan and return the resulting scan summary.",
        visibility: McpVisibilityRequirement::Index,
        destructive: false,
        annotations: mcp_annotations(false, false, true, false),
        input_schema: index_scan_input_schema,
        output_schema: Some(index_scan_output_schema),
        examples: &["vulcan index scan --full"],
    },
    McpToolCatalogEntry {
        id: McpToolId::SyncStatus,
        name: "sync_status",
        title: "Inspect Sync Status",
        description: "Inspect the selected vault's Git sync state and exact remote live ref without capturing, publishing, or applying files.",
        visibility: McpVisibilityRequirement::GitReadAll,
        destructive: false,
        annotations: mcp_annotations(true, false, true, true),
        input_schema: sync_status_input_schema,
        output_schema: None,
        examples: &["sync_status {}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::SyncDoctor,
        name: "sync_doctor",
        title: "Diagnose Git Sync",
        description: "Run read-only Git layout, ref, journal, filter, cache, and target-platform synchronization diagnostics.",
        visibility: McpVisibilityRequirement::GitReadAll,
        destructive: false,
        annotations: mcp_annotations(true, false, true, true),
        input_schema: sync_doctor_input_schema,
        output_schema: None,
        examples: &["sync_doctor {}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::SyncConflicts,
        name: "sync_conflicts",
        title: "Inspect Preserved Sync Conflicts",
        description: "List unresolved preserved conflicts or inspect one immutable conflict record; this tool never resolves a conflict.",
        visibility: McpVisibilityRequirement::GitReadAll,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: sync_conflicts_input_schema,
        output_schema: None,
        examples: &["sync_conflicts {}", "sync_conflicts {\"conflict_id\":\"01...\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::FileGet,
        name: "file_get",
        title: "Read Vault File",
        description: "Read a bounded slice of any vault file, such as an attachment, as UTF-8 text or base64, with the whole file's content hash.",
        visibility: McpVisibilityRequirement::Read,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: file_get_input_schema,
        output_schema: None,
        examples: &["file_get {\"path\":\"Attachments/diagram.svg\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::FilePut,
        name: "file_put",
        title: "Write Vault File",
        description: "Create or replace a non-Markdown vault file, such as an attachment, from text or base64 (at most 10 MiB).",
        visibility: McpVisibilityRequirement::Write,
        destructive: false,
        annotations: mcp_annotations(false, true, false, false),
        input_schema: file_put_input_schema,
        output_schema: None,
        examples: &["file_put {\"path\":\"Attachments/plan.txt\",\"content\":\"draft\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::SyncRefresh,
        name: "sync_refresh",
        title: "Synchronize Now",
        description: "Start a synchronization of this registered vault now: through the daemon when it runs, otherwise directly. Returns the queued job or the cycle's report.",
        visibility: McpVisibilityRequirement::GitCommit,
        destructive: false,
        annotations: mcp_annotations(false, false, false, true),
        input_schema: empty_input_schema,
        output_schema: None,
        examples: &["sync_refresh {}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::SyncResume,
        name: "sync_resume",
        title: "Resume Synchronization",
        description: "Clear a paused registration so automatic synchronization runs again, and start a synchronization.",
        visibility: McpVisibilityRequirement::GitCommit,
        destructive: false,
        annotations: mcp_annotations(false, false, true, true),
        input_schema: empty_input_schema,
        output_schema: None,
        examples: &["sync_resume {}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::NoteMove,
        name: "note_move",
        title: "Move Or Rename Note",
        description: "Move or rename one note and rewrite every link and embed that points at it.",
        visibility: McpVisibilityRequirement::Refactor,
        destructive: false,
        annotations: mcp_annotations(false, false, false, false),
        input_schema: note_move_input_schema,
        output_schema: None,
        examples: &["note_move {\"note\":\"Inbox/Idea\",\"destination\":\"Projects/Idea\",\"dry_run\":true}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::GitStatus,
        name: "git_status",
        title: "Show Uncommitted Changes",
        description: "List staged, unstaged, and untracked vault paths.",
        visibility: McpVisibilityRequirement::GitReadAll,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: git_status_input_schema,
        output_schema: None,
        examples: &["git_status {}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::GitDiff,
        name: "git_diff",
        title: "Show Uncommitted Diff",
        description: "Return the unified diff of uncommitted vault changes, optionally for one path.",
        visibility: McpVisibilityRequirement::GitReadAll,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: git_diff_input_schema,
        output_schema: None,
        examples: &["git_diff {\"path\":\"Projects/Alpha.md\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::GitLog,
        name: "git_log",
        title: "List Recent Commits",
        description: "List the most recent commits that touched the vault.",
        visibility: McpVisibilityRequirement::GitReadAll,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: git_log_input_schema,
        output_schema: None,
        examples: &["git_log {\"limit\":10}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::GitBlame,
        name: "git_blame",
        title: "Blame File Lines",
        description: "Show which commit last changed each line of one vault file.",
        visibility: McpVisibilityRequirement::GitReadAll,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: git_blame_input_schema,
        output_schema: None,
        examples: &["git_blame {\"path\":\"Home.md\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::GitCommit,
        name: "git_commit",
        title: "Commit Vault Changes",
        description: "Commit named `paths`, every vault change (`all: true`), or everything in a `changeset`, running pre-commit hooks; the commit names the client in Vulcan-MCP-* trailers.",
        visibility: McpVisibilityRequirement::GitCommit,
        destructive: false,
        annotations: mcp_annotations(false, false, false, false),
        input_schema: git_commit_input_schema,
        output_schema: None,
        examples: &["git_commit {\"message\":\"Plan the release\",\"paths\":[\"Plans/Release.md\"]}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::ChangesetBegin,
        name: "changeset_begin",
        title: "Begin A Changeset",
        description: "Start a changeset: a branch and worktree outside the vault, indexed. Pass its `id` as `changeset` to note, file, search, query, task, and git tools to work in it until it lands or is abandoned; other writers, sync, and auto-commit do not see it meanwhile.",
        visibility: McpVisibilityRequirement::GitCommit,
        destructive: false,
        annotations: mcp_annotations(false, false, false, false),
        input_schema: changeset_begin_input_schema,
        output_schema: None,
        examples: &["changeset_begin {\"title\":\"Restructure the guides\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::ChangesetList,
        name: "changeset_list",
        title: "List Changesets",
        description: "List the vault's changesets with their commits ahead and uncommitted paths.",
        visibility: McpVisibilityRequirement::GitReadAll,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: changeset_list_input_schema,
        output_schema: None,
        examples: &["changeset_list {}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::ChangesetLand,
        name: "changeset_land",
        title: "Land A Changeset",
        description: "Land a changeset on the vault's branch: commit leftovers with `message`, rebase if the branch moved, fast-forward, re-index, and remove the changeset. A rebase conflict keeps the changeset unchanged and names the paths.",
        visibility: McpVisibilityRequirement::GitCommit,
        destructive: false,
        annotations: mcp_annotations(false, false, false, false),
        input_schema: changeset_land_input_schema,
        output_schema: None,
        examples: &["changeset_land {\"id\":\"01J…\",\"message\":\"Restructure the guides\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::ChangesetAbandon,
        name: "changeset_abandon",
        title: "Abandon A Changeset",
        description: "Delete a changeset, its branch, and its worktree without landing anything.",
        visibility: McpVisibilityRequirement::GitCommit,
        destructive: true,
        annotations: mcp_annotations(false, true, false, false),
        input_schema: changeset_id_input_schema,
        output_schema: None,
        examples: &["changeset_abandon {\"id\":\"01J…\"}"],
    },
    McpToolCatalogEntry {
        id: McpToolId::OperationGet,
        name: "operation_get",
        title: "Get An Operation",
        description: "Report how a tool call that timed out with an `indeterminate` error ended: running, succeeded with its result, or failed. Check it before repeating the call.",
        visibility: McpVisibilityRequirement::None,
        destructive: false,
        annotations: mcp_annotations(true, false, true, false),
        input_schema: operation_get_input_schema,
        output_schema: None,
        examples: &["operation_get {\"id\":\"01J…\"}"],
    },
];

/// Tools that take an optional `changeset` argument and then run in that
/// changeset's worktree instead of the vault. Custom tools always run on the
/// vault: their arguments are their own.
pub fn accepts_changeset(id: McpToolId) -> bool {
    matches!(
        id,
        McpToolId::NoteGet
            | McpToolId::NoteOutline
            | McpToolId::NoteInfo
            | McpToolId::NoteCreate
            | McpToolId::NoteAppend
            | McpToolId::NotePatch
            | McpToolId::NoteSet
            | McpToolId::NoteDelete
            | McpToolId::NoteMove
            | McpToolId::FileGet
            | McpToolId::FilePut
            | McpToolId::Search
            | McpToolId::Query
            | McpToolId::Daily
            | McpToolId::TaskQuery
            | McpToolId::TaskCreate
            | McpToolId::TaskComplete
            | McpToolId::TaskReschedule
            | McpToolId::TaskReminderAdd
            | McpToolId::TaskReminderRemove
            | McpToolId::GitStatus
            | McpToolId::GitDiff
            | McpToolId::GitLog
            | McpToolId::GitBlame
            | McpToolId::GitCommit
    )
}

/// A built-in tool's input schema as `profile` sees it: with the `changeset`
/// argument where the tool takes one and the profile may use changesets.
pub fn tool_input_schema(tool: &McpToolCatalogEntry, profile: &PermissionProfile) -> Value {
    let schema = (tool.input_schema)();
    if accepts_changeset(tool.id)
        && requirement_met(McpVisibilityRequirement::GitCommit, false, profile)
    {
        crate::mcp_schemas::with_changeset_argument(schema)
    } else {
        schema
    }
}

/// Tools a client should load up front even when it defers the rest behind
/// tool search (`_meta["anthropic/alwaysLoad"]`): the core navigation reads
/// and the most common edit.
pub const ALWAYS_LOADED_TOOLS: &[&str] =
    &["search", "query", "note_get", "note_outline", "note_append"];

pub fn tool_by_name(name: &str) -> Option<&'static McpToolCatalogEntry> {
    MCP_TOOL_CATALOG.iter().find(|tool| tool.name == name)
}

/// Find a built-in tool and apply the profile gate used for discovery.
/// `None` means the name is not a built-in (it may be a custom tool).
pub fn authorize_builtin_tool_call(
    name: &str,
    profile: &PermissionProfile,
    profile_name: &str,
) -> Result<Option<&'static McpToolCatalogEntry>, McpMethodError> {
    let Some(tool) = tool_by_name(name) else {
        return Ok(None);
    };
    if !tool_visible(tool, profile) {
        let requirement = if tool.destructive
            && tool_visible(
                &McpToolCatalogEntry {
                    destructive: false,
                    ..*tool
                },
                profile,
            ) {
            "destructive access (overwrite or delete)"
        } else {
            visibility_requirement_name(tool.visibility)
        };
        return Err(McpMethodError::tool(format!(
            "permission denied: tool `{}` requires {requirement} under profile `{profile_name}`",
            tool.name,
        )));
    }
    Ok(Some(tool))
}

fn visibility_requirement_name(requirement: McpVisibilityRequirement) -> &'static str {
    match requirement {
        McpVisibilityRequirement::None => "no access",
        McpVisibilityRequirement::Read => "read access",
        McpVisibilityRequirement::Write => "write access",
        McpVisibilityRequirement::Network => "network access",
        McpVisibilityRequirement::Index => "index access",
        McpVisibilityRequirement::ConfigRead => "config read access",
        McpVisibilityRequirement::ConfigWrite => "config write access",
        McpVisibilityRequirement::GitReadAll => "Git access and full-vault read access",
        McpVisibilityRequirement::GitCommit => "Git access and full-vault read and write access",
        McpVisibilityRequirement::Refactor => "refactor access",
    }
}

/// The built-in tools a profile may see and call, in catalog order.
/// `operation_get` is not among them: a transport that keeps operation
/// records adds it (see [`transport_tools`]).
pub fn visible_tool_catalog(profile: &PermissionProfile) -> Vec<&'static McpToolCatalogEntry> {
    MCP_TOOL_CATALOG
        .iter()
        .filter(|tool| tool.id != McpToolId::OperationGet && tool_visible(tool, profile))
        .collect()
}

/// Tools that exist only where the transport provides them.
pub fn transport_tools(operations: bool) -> Vec<&'static McpToolCatalogEntry> {
    MCP_TOOL_CATALOG
        .iter()
        .filter(|tool| operations && tool.id == McpToolId::OperationGet)
        .collect()
}

pub fn active_tool_names(profile: &PermissionProfile) -> Vec<String> {
    visible_tool_catalog(profile)
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect()
}

/// Whether `profile` grants what `tool` needs; the only gate on listing and
/// calling a built-in tool.
pub fn tool_visible(tool: &McpToolCatalogEntry, profile: &PermissionProfile) -> bool {
    requirement_met(tool.visibility, tool.destructive, profile)
}

/// Whether `profile` grants `requirement` (and `destructive`, when set).
pub fn requirement_met(
    requirement: McpVisibilityRequirement,
    destructive: bool,
    profile: &PermissionProfile,
) -> bool {
    if destructive && !matches!(profile.destructive, PermissionMode::Allow) {
        return false;
    }
    match requirement {
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
        McpVisibilityRequirement::GitCommit => {
            matches!(profile.git, PermissionMode::Allow)
                && profile.read.is_all()
                && profile.write.is_all()
        }
        McpVisibilityRequirement::Refactor => !profile.refactor.is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_changeset_argument_is_offered_where_changesets_can_be_used() {
        let note_get = tool_by_name("note_get").unwrap();
        let status = tool_by_name("status").unwrap();
        let has = |tool, profile: &PermissionProfile| {
            tool_input_schema(tool, profile)["properties"]
                .get("changeset")
                .is_some()
        };
        let unrestricted = PermissionProfile::unrestricted();
        assert!(has(note_get, &unrestricted));
        assert!(!has(status, &unrestricted));
        // Without Git access there are no changesets to name.
        assert!(!has(note_get, &PermissionProfile::daily_wiki_agent()));
    }

    #[test]
    fn destructive_tools_need_the_destructive_permission() {
        let daily = PermissionProfile::daily_wiki_agent();
        let writer = PermissionProfile::writer();
        for name in ["note_set", "note_delete"] {
            let tool = tool_by_name(name).unwrap();
            assert!(tool.destructive, "{name}");
            assert!(!tool_visible(tool, &daily), "{name}");
            assert!(tool_visible(tool, &writer), "{name}");
        }
        assert!(tool_visible(tool_by_name("note_append").unwrap(), &daily));
        assert!(matches!(
            authorize_builtin_tool_call("note_delete", &daily, "daily-wiki-agent"),
            Err(McpMethodError::Tool { ref message, .. })
                if message.contains("destructive access")
        ));
    }

    #[test]
    fn the_profile_alone_decides_which_builtins_are_listed_and_callable() {
        let readonly = PermissionProfile::readonly();
        let names = active_tool_names(&readonly);
        assert!(names.iter().any(|name| name == "note_get"));
        assert!(names.iter().any(|name| name == "task_query"));
        assert!(!names.iter().any(|name| name == "note_create"));
        assert!(!names.iter().any(|name| name.starts_with("tool_pack")));
        assert!(matches!(
            authorize_builtin_tool_call("custom_name", &readonly, "readonly"),
            Ok(None)
        ));
        assert!(matches!(
            authorize_builtin_tool_call("tool_pack_list", &readonly, "readonly"),
            Ok(None)
        ));
        assert!(matches!(
            authorize_builtin_tool_call("note_create", &readonly, "readonly"),
            Err(McpMethodError::Tool { .. })
        ));
        let unrestricted = PermissionProfile::unrestricted();
        // Everything but the transport's own operation_get.
        assert_eq!(
            visible_tool_catalog(&unrestricted).len() + transport_tools(true).len(),
            MCP_TOOL_CATALOG.len()
        );
        assert!(transport_tools(false).is_empty());
    }
}
