use serde_json::{Map, Value};

pub(super) fn schema_string(description: &str) -> Value {
    serde_json::json!({ "type": "string", "description": description })
}

pub(super) fn schema_boolean(description: &str) -> Value {
    serde_json::json!({ "type": "boolean", "description": description })
}

pub(super) fn schema_integer(description: &str) -> Value {
    serde_json::json!({ "type": "integer", "description": description })
}

pub(super) fn schema_string_enum(description: &str, values: &[&str]) -> Value {
    serde_json::json!({
        "type": "string",
        "description": description,
        "enum": values,
    })
}

pub(super) fn schema_array(items: Value, description: &str) -> Value {
    serde_json::json!({
        "type": "array",
        "description": description,
        "items": items,
    })
}

pub(super) fn schema_object(properties: Vec<(&str, Value)>, required: &[&str]) -> Value {
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

pub(super) fn empty_object_schema() -> Value {
    schema_object(Vec::new(), &[])
}

pub(super) fn generic_report_output_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": true,
    })
}

pub(super) fn note_get_input_schema() -> Value {
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

pub(super) fn note_outline_input_schema() -> Value {
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

pub(super) fn search_input_schema() -> Value {
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

pub(super) fn query_input_schema() -> Value {
    schema_object(
        vec![
            (
                "query",
                schema_string("Optional Vulcan query DSL or Dataview DQL source."),
            ),
            (
                "json",
                schema_string("Optional JSON-encoded Vulcan QueryAst payload."),
            ),
            (
                "filters",
                schema_array(
                    schema_string("Typed note filter."),
                    "Typed `--where` filters for note-property queries.",
                ),
            ),
            ("sort", schema_string("Optional note field to sort by.")),
            ("desc", schema_boolean("Sort descending.")),
            (
                "engine",
                schema_string_enum("Query parser.", &["auto", "dsl", "dql"]),
            ),
        ],
        &[],
    )
}

pub(super) fn daily_show_input_schema() -> Value {
    schema_object(
        vec![(
            "date",
            schema_string("Reference date such as today, tomorrow, or YYYY-MM-DD."),
        )],
        &[],
    )
}

pub(super) fn daily_list_input_schema() -> Value {
    schema_object(
        vec![
            (
                "from",
                schema_string("Start date for the daily-note window."),
            ),
            ("to", schema_string("End date for the daily-note window.")),
            (
                "week",
                schema_boolean("Use the current configured week window."),
            ),
            (
                "month",
                schema_boolean("Use the current configured month window."),
            ),
        ],
        &[],
    )
}

pub(super) fn task_list_input_schema() -> Value {
    schema_object(
        vec![
            (
                "filter",
                schema_string("Optional Tasks query or DQL fallback filter."),
            ),
            (
                "source",
                schema_string_enum(
                    "Task source model.",
                    &["all", "inline", "tasknotes", "file"],
                ),
            ),
            ("status", schema_string("Filter by status.")),
            ("priority", schema_string("Filter by priority.")),
            (
                "due_before",
                schema_string("Filter tasks due before this date."),
            ),
            (
                "due_after",
                schema_string("Filter tasks due after this date."),
            ),
            ("project", schema_string("Filter by project.")),
            ("context", schema_string("Filter by context.")),
            (
                "group_by",
                schema_string("Group result rows by task field."),
            ),
            ("sort_by", schema_string("Sort result rows by task field.")),
            (
                "include_archived",
                schema_boolean("Include archived TaskNotes tasks."),
            ),
        ],
        &[],
    )
}

pub(super) fn task_query_input_schema() -> Value {
    schema_object(
        vec![("query", schema_string("Tasks plugin query source."))],
        &["query"],
    )
}

pub(super) fn task_create_input_schema() -> Value {
    schema_object(
        vec![
            (
                "text",
                schema_string("Task text, optionally with NLP date/project/context hints."),
            ),
            (
                "note",
                schema_string("Optional note target for inline task creation."),
            ),
            ("due", schema_string("Optional due date.")),
            ("priority", schema_string("Optional priority.")),
            ("dry_run", schema_boolean("Preview without writing.")),
            (
                "no_commit",
                schema_boolean("Suppress auto-commit for this mutation."),
            ),
        ],
        &["text"],
    )
}

pub(super) fn task_complete_input_schema() -> Value {
    schema_object(
        vec![
            (
                "task",
                schema_string("Task path, title, or inline task selector."),
            ),
            ("date", schema_string("Completion date.")),
            ("dry_run", schema_boolean("Preview without writing.")),
            (
                "no_commit",
                schema_boolean("Suppress auto-commit for this mutation."),
            ),
        ],
        &["task"],
    )
}

pub(super) fn task_reschedule_input_schema() -> Value {
    schema_object(
        vec![
            (
                "task",
                schema_string("Task path, title, or inline task selector."),
            ),
            ("due", schema_string("New due date.")),
            ("dry_run", schema_boolean("Preview without writing.")),
            (
                "no_commit",
                schema_boolean("Suppress auto-commit for this mutation."),
            ),
        ],
        &["task", "due"],
    )
}

pub(super) fn note_create_input_schema() -> Value {
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

pub(super) fn note_append_input_schema() -> Value {
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

pub(super) fn note_patch_input_schema() -> Value {
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

pub(super) fn note_info_input_schema() -> Value {
    schema_object(
        vec![("note", schema_string("Note path or note identifier."))],
        &["note"],
    )
}

pub(super) fn note_set_input_schema() -> Value {
    schema_object(
        vec![
            ("note", schema_string("Note identifier to replace.")),
            ("content", schema_string("Replacement markdown content.")),
            (
                "confirm",
                schema_boolean("Must be true because this replaces the full note body."),
            ),
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

pub(super) fn note_delete_input_schema() -> Value {
    schema_object(
        vec![
            ("note", schema_string("Note identifier to delete.")),
            (
                "dry_run",
                schema_boolean("Preview deletion and backlinks without removing the file."),
            ),
            (
                "confirm",
                schema_boolean("Must be true unless dry_run is true."),
            ),
            (
                "no_commit",
                schema_boolean("Suppress auto-commit for this mutation."),
            ),
        ],
        &["note"],
    )
}

pub(super) fn web_search_input_schema() -> Value {
    schema_object(
        vec![
            ("query", schema_string("Web search query string.")),
            (
                "backend",
                schema_string_enum(
                    "Optional search backend override.",
                    &[
                        "disabled",
                        "auto",
                        "duckduckgo",
                        "kagi",
                        "exa",
                        "tavily",
                        "brave",
                        "ollama",
                    ],
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

pub(super) fn web_fetch_input_schema() -> Value {
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

pub(super) fn config_show_input_schema() -> Value {
    schema_object(
        vec![(
            "section",
            schema_string("Optional dotted config section to return."),
        )],
        &[],
    )
}

pub(super) fn config_set_input_schema() -> Value {
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

pub(super) fn index_scan_input_schema() -> Value {
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

pub(super) fn graph_communities_input_schema() -> Value {
    schema_object(
        vec![
            (
                "community",
                schema_integer("Optional community id to focus in client displays."),
            ),
            ("orphans", schema_boolean("Include orphan placement hints.")),
            ("bridges", schema_boolean("Include bridge-note rankings.")),
            (
                "dry_run",
                schema_boolean("Compute communities without persisting assignments."),
            ),
        ],
        &[],
    )
}

pub(super) fn suggest_links_input_schema() -> Value {
    schema_object(
        vec![
            (
                "note",
                schema_string("Optional note identifier to scope suggestions."),
            ),
            (
                "min_score",
                serde_json::json!({
                    "type": "number",
                    "description": "Minimum composite score to include."
                }),
            ),
            (
                "limit",
                schema_integer("Maximum number of suggestions to return."),
            ),
            (
                "status",
                schema_string_enum(
                    "Feedback status filter.",
                    &["pending", "accepted", "rejected"],
                ),
            ),
            ("accept", schema_string("Suggestion id to accept.")),
            ("reject", schema_string("Suggestion id to reject.")),
        ],
        &[],
    )
}

pub(super) fn tool_pack_mutation_input_schema() -> Value {
    schema_object(
        vec![(
            "packs",
            serde_json::json!({
                "type": "array",
                "description": "One or more tool-pack selectors to enable, disable, or set.",
                "items": {
                    "type": "string",
                    "enum": [
                        "notes-read",
                        "search",
                        "status",
                        "custom",
                        "daily",
                        "tasks",
                        "notes-write",
                        "notes-manage",
                        "web",
                        "config",
                        "index",
                    ],
                },
            }),
        )],
        &["packs"],
    )
}

pub(super) fn tool_pack_state_output_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "mode": {
                "type": "string",
                "enum": ["static", "adaptive"],
                "description": "Current MCP tool-pack session mode.",
            },
            "selectedToolPacks": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Canonical tool packs currently selected for this session.",
            },
            "availableToolPacks": {
                "type": "array",
                "description": "Available canonical packs with visibility information under the current permission profile.",
            },
        },
        "required": ["mode", "selectedToolPacks", "availableToolPacks"],
    })
}

pub(super) fn note_get_output_schema() -> Value {
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

pub(super) fn note_outline_output_schema() -> Value {
    generic_report_output_schema()
}

pub(super) fn search_output_schema() -> Value {
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

pub(super) fn status_output_schema() -> Value {
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
            ("graph_confidence", generic_report_output_schema()),
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

pub(super) fn note_create_output_schema() -> Value {
    generic_report_output_schema()
}

pub(super) fn note_append_output_schema() -> Value {
    generic_report_output_schema()
}

pub(super) fn note_patch_output_schema() -> Value {
    generic_report_output_schema()
}

pub(super) fn note_info_output_schema() -> Value {
    generic_report_output_schema()
}

pub(super) fn note_set_output_schema() -> Value {
    generic_report_output_schema()
}

pub(super) fn note_delete_output_schema() -> Value {
    generic_report_output_schema()
}

pub(super) fn web_search_output_schema() -> Value {
    generic_report_output_schema()
}

pub(super) fn web_fetch_output_schema() -> Value {
    generic_report_output_schema()
}

pub(super) fn config_show_output_schema() -> Value {
    generic_report_output_schema()
}

pub(super) fn config_set_output_schema() -> Value {
    generic_report_output_schema()
}

pub(super) fn index_scan_output_schema() -> Value {
    generic_report_output_schema()
}
