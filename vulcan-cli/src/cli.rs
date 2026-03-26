use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;

const ROOT_AFTER_HELP: &str = "\
Command Groups:
  Indexing: init, scan, rebuild, repair, watch, serve
  Graph and Query: links, backlinks, graph, search, notes, browse, query, bases, suggest, diff
  Semantic: vectors, cluster, related
  Reports and Automation: saved, checkpoint, changes, batch, export, automation
  Mutations: edit, update, unset, rename-property, merge-tags, rename-alias, rename-heading, rename-block-ref, inbox, template
  Maintenance: move, doctor, cache, link-mentions, rewrite, open, describe, completions

Docs:
  User guide: docs/cli.md
  Interactive help: vulcan edit --help and vulcan browse --help
  Query/filter reference: vulcan notes --help and vulcan search --help
  Machine-readable schema: vulcan describe

Freshness:
  Override automatic cache refresh with --refresh <off|blocking|background>";

const NOTES_COMMAND_AFTER_HELP: &str = "\
Sort keys:
  Any property key, or one of file.path, file.name, file.ext, file.mtime

\
Filter syntax:
  Repeat --where to combine filters with AND.
  Form: <field> <operator> <value>
  There is no OR or parenthesized filter syntax in --where today.

Fields:
  <property-key>
  file.path | file.name | file.ext | file.mtime

Operators:
  = | > | >= | < | <=
  starts_with   text fields only
  contains      list properties only

Values:
  text: done, \"In Progress\", 'Rule Index'
  booleans: true, false
  null: null
  numbers: 42, 3.5
  dates: 2026-03-01 or 2026-03-01T09:30:00Z
  file.mtime: integer milliseconds since the Unix epoch

Examples:
  vulcan notes --where 'status = done'
  vulcan notes --where 'tags contains sprint'
  vulcan notes --where 'file.path starts_with \"Projects/\"' --sort due";

const SEARCH_COMMAND_AFTER_HELP: &str = "\
Search query syntax:
  plain terms are ANDed: dashboard status
  group terms with parentheses: (dashboard or summary) release
  quoted phrases stay together: \"owned by\"
  use `or` between positive terms: dashboard or summary
  prefix a term or phrase with - to exclude it: dashboard -draft -\"old version\"
  negate grouped terms too: dashboard -(draft archived)
  scope terms to one line, block, or heading section:
    line:(mix flour), block:(release notes), section:(dog cat)
  inline filters on unquoted positive terms:
    tag:index
    path:People/
    has:status
    property:status
    [aliases]
    [status:done]
    /\\d{4}-\\d{2}-\\d{2}/
    path:/2026-03-\\d{2}/
    file:meeting
    content:release
    match-case:Bob
    ignore-case:Bob
    task:docs
    task-todo:followup
    task-done:ship

Notes:
  Use --where for typed property filters and list membership.
  --explain prints the parsed boolean tree plus active filters.
  Use --raw-query to pass SQLite FTS5 syntax through unchanged.

\
Filter syntax:
  Repeat --where to combine filters with AND.
  Form: <field> <operator> <value>
  There is no OR or parenthesized filter syntax in --where today.

Fields:
  <property-key>
  file.path | file.name | file.ext | file.mtime

Operators:
  = | > | >= | < | <=
  starts_with   text fields only
  contains      list properties only

Values:
  text: done, \"In Progress\", 'Rule Index'
  booleans: true, false
  null: null
  numbers: 42, 3.5
  dates: 2026-03-01 or 2026-03-01T09:30:00Z
  file.mtime: integer milliseconds since the Unix epoch

Examples:
  vulcan search 'dashboard \"release notes\" -draft'
  vulcan search 'tag:index path:People/ owned'
  vulcan search 'release [status:done]'
  vulcan search '/\\d{4}-\\d{2}-\\d{2}/'
  vulcan search 'file:meeting content:release'
  vulcan search 'section:(dog cat)'
  vulcan search 'task-todo:followup task-done:ship'
  vulcan search 'line:(mix flour) block:(oven timer)'
  vulcan search dashboard --where 'reviewed = true'
  vulcan search Bob --match-case
  vulcan search dashboard --sort path-desc
  vulcan search dashboard --sort modified-newest";

const REWRITE_COMMAND_AFTER_HELP: &str = "\
Scope selection:
  rewrite does a literal find/replace over notes selected by --where filters.
  Repeat --where to combine filters with AND.

\
Filter syntax:
  Repeat --where to combine filters with AND.
  Form: <field> <operator> <value>
  There is no OR or parenthesized filter syntax in --where today.

Fields:
  <property-key>
  file.path | file.name | file.ext | file.mtime

Operators:
  = | > | >= | < | <=
  starts_with   text fields only
  contains      list properties only

Values:
  text: done, \"In Progress\", 'Rule Index'
  booleans: true, false
  null: null
  numbers: 42, 3.5
  dates: 2026-03-01 or 2026-03-01T09:30:00Z
  file.mtime: integer milliseconds since the Unix epoch

Examples:
  vulcan rewrite --where 'status = draft' --find TODO --replace DONE --dry-run
  vulcan rewrite --where 'file.path starts_with \"Projects/\"' --find alpha --replace beta";

const BASES_COMMAND_AFTER_HELP: &str = "\
Subcommands:
  eval         evaluate a .base file and print its current rows
  tui          inspect a .base file interactively
  view-add     add a validated view definition
  view-delete  remove a view definition
  view-rename  rename a view
  view-edit    adjust filters, columns, sort, and grouping

Notes:
  view-* commands rewrite the parsed .base model instead of patching YAML text blindly.
  Mutating bases commands support --dry-run and --no-commit.

Examples:
  vulcan bases eval release.base
  vulcan bases tui release.base
  vulcan bases view-add release.base Inbox --filter 'status = idea' --column file.name";

const BROWSE_COMMAND_AFTER_HELP: &str = "\
Browse modes:
  default      fuzzy note picker with live preview
  Ctrl-F       indexed full-text search with snippet preview
  Ctrl-T       tag filter mode
  Ctrl-P       property filter mode
  /            return to fuzzy mode

Keys:
  Enter        edit the selected note
  Ctrl-N       create a new note
  Ctrl-R       move or rename the selected note
  Ctrl-B       open backlinks for the selected note
  Ctrl-O       open outgoing links for the selected note
  Ctrl-D       show doctor diagnostics for the selected note
  Ctrl-G       show git history for the selected file
  Esc          quit the browser

Notes:
  browse honors [scan].browse_mode in config; --refresh overrides it per invocation.
  `background` opens immediately on current cache contents, then reloads when the scan completes.
  Printable characters always extend the active query or prompt; browse actions use Enter or Ctrl shortcuts.
  Ctrl-E edits from fuzzy/tag/property modes; in Ctrl-F mode it toggles the explain pane.
  In Ctrl-F mode, Ctrl-S cycles result sort order and Alt-C toggles global match-case.
  After edits, creates, and moves, Vulcan rescans affected files and refreshes the browser.
  In backlinks/outgoing-link views, `o` opens the selected .base file in the Bases TUI.

Examples:
  vulcan browse
  vulcan --refresh background browse
  vulcan browse --no-commit";

const EDIT_COMMAND_AFTER_HELP: &str = "\
Behavior:
  If NOTE is omitted in an interactive terminal, Vulcan opens the note picker.
  With --new, Vulcan creates the target path first (appending .md when missing).

Notes:
  The editor is chosen from $VISUAL, then $EDITOR, then `vi`.
  After the editor exits, Vulcan runs an incremental scan of the edited file.

Examples:
  vulcan edit Projects/Alpha
  vulcan edit
  vulcan edit --new Inbox/Idea";

const DIFF_COMMAND_AFTER_HELP: &str = "\
Comparison source:
  --since <checkpoint> compares against a named checkpoint.
  Otherwise, git-backed vaults compare the note against git HEAD.
  Without git, Vulcan reports cache-level changes since the last scan.

Notes:
  Git-backed diffs include unified diff text in human and JSON output.
  Cache-backed diffs report which note, link, property, or embedding records changed.

Examples:
  vulcan diff Projects/Alpha
  vulcan diff --since weekly Projects/Alpha
  vulcan diff";

const INBOX_COMMAND_AFTER_HELP: &str = "\
Configuration:
  Inbox settings live under [inbox] in .vulcan/config.toml.
  path       relative note path to append to
  format     entry template; supports {text}, {date}, {time}, {datetime}
  timestamp  prepend the current datetime automatically
  heading    optional heading to append under (created if missing)

Input:
  Pass TEXT directly, use `-` to read from stdin, or use --file <PATH>.
  Vulcan creates the inbox note automatically when it does not exist.

Examples:
  vulcan inbox \"Call Alice about launch notes\"
  echo \"idea\" | vulcan inbox -
  vulcan inbox --file scratch.txt";

const TEMPLATE_COMMAND_AFTER_HELP: &str = "\
Template source:
  Templates live under .vulcan/templates as regular .md files.
  If .obsidian/templates.json configures a template folder, Vulcan lists that folder too.
  When the same template exists in both places, .vulcan/templates takes precedence.
  NAME can be the full filename or the filename stem.

Variables:
  {{title}} {{date}} {{time}} {{datetime}} {{uuid}}
  {{date:YYYY-MM-DD}} {{time:HH:mm}} {{date:dddd, MMMM Do YYYY}}

Configuration:
  Default template date/time formats live under [templates] in .vulcan/config.toml.
  date_format applies to {{date}} and time_format applies to {{time}}.

Notes:
  If --path is omitted, Vulcan creates <date>-<template-name>.md in the vault root.
  In an interactive terminal, the new note is opened in $VISUAL/$EDITOR after rendering.
  `template insert` appends by default; use --prepend to insert after frontmatter instead.
  If the insert target note is omitted in an interactive terminal, Vulcan opens the note picker.

Examples:
  vulcan template --list
  vulcan template daily --path Daily/2026-03-26
  vulcan template meeting
  vulcan template insert daily Projects/Alpha
  vulcan template insert daily --prepend";

const OPEN_COMMAND_AFTER_HELP: &str = "\
Behavior:
  Resolves NOTE by path, filename, or alias.
  If NOTE is omitted in an interactive terminal, Vulcan opens the note picker.
  Launches obsidian://open?vault=<vault>&file=<path> through the platform opener.

Examples:
  vulcan open Projects/Alpha
  vulcan open Daily/2026-03-26
  vulcan open";

const DESCRIBE_COMMAND_AFTER_HELP: &str = "\
Output:
  describe prints the runtime CLI schema, including subcommands, options, defaults,
  possible values, and embedded after-help text.

Examples:
  vulcan describe
  vulcan --output json describe > vulcan-schema.json";

const COMPLETIONS_COMMAND_AFTER_HELP: &str = "\
Examples:
  vulcan completions bash > ~/.local/share/bash-completion/completions/vulcan
  vulcan completions fish > ~/.config/fish/completions/vulcan.fish";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormat {
    Csv,
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RefreshMode {
    Off,
    Blocking,
    Background,
}

#[derive(Debug, Clone, PartialEq, Eq, Args, Default)]
pub struct ExportArgs {
    #[arg(
        long,
        value_enum,
        requires = "export_path",
        help = "Write query rows to a CSV or JSONL file"
    )]
    pub export: Option<ExportFormat>,

    #[arg(
        long = "export-path",
        requires = "export",
        help = "Destination file for CSV or JSONL exports"
    )]
    pub export_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum BasesCommand {
    #[command(about = "Evaluate a .base file against the indexed vault state")]
    Eval {
        #[arg(help = "Vault-relative path to the .base file to evaluate")]
        file: String,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Open an interactive TUI for a .base file")]
    Tui {
        #[arg(help = "Vault-relative path to the .base file to inspect")]
        file: String,
    },
    #[command(about = "Add a new view to a .base file")]
    ViewAdd {
        #[arg(help = "Vault-relative path to the .base file")]
        file: String,
        #[arg(help = "Name for the new view")]
        name: String,
        #[arg(long = "filter", help = "Filter expression; repeatable")]
        filters: Vec<String>,
        #[arg(long, help = "Column key to show; repeatable (sets column order)")]
        column: Vec<String>,
        #[arg(long, help = "Property key to sort by")]
        sort: Option<String>,
        #[arg(long, help = "Sort descending instead of ascending")]
        sort_desc: bool,
        #[arg(long, help = "Property key to group rows by")]
        group_by: Option<String>,
        #[arg(long, help = "Group descending instead of ascending")]
        group_desc: bool,
        #[arg(long, help = "Preview the view without writing changes")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Delete a view from a .base file")]
    ViewDelete {
        #[arg(help = "Vault-relative path to the .base file")]
        file: String,
        #[arg(help = "Name of the view to delete")]
        name: String,
        #[arg(long, help = "Preview without writing changes")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Rename a view in a .base file")]
    ViewRename {
        #[arg(help = "Vault-relative path to the .base file")]
        file: String,
        #[arg(help = "Current view name")]
        old_name: String,
        #[arg(help = "Replacement view name")]
        new_name: String,
        #[arg(long, help = "Preview without writing changes")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Edit a view in a .base file")]
    ViewEdit {
        #[arg(help = "Vault-relative path to the .base file")]
        file: String,
        #[arg(help = "Name of the view to edit")]
        name: String,
        #[arg(long = "add-filter", help = "Append a filter expression")]
        add_filters: Vec<String>,
        #[arg(long = "remove-filter", help = "Remove a filter expression")]
        remove_filters: Vec<String>,
        #[arg(long, help = "Set column order (repeatable)")]
        column: Vec<String>,
        #[arg(long, help = "Set the sort property (empty string to clear)")]
        sort: Option<String>,
        #[arg(long, help = "Set sort direction to descending")]
        sort_desc: bool,
        #[arg(long, help = "Set group-by property (empty string to clear)")]
        group_by: Option<String>,
        #[arg(long, help = "Set group-by direction to descending")]
        group_desc: bool,
        #[arg(long, help = "Preview changes without writing")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SearchMode {
    Keyword,
    Hybrid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SearchSortArg {
    Relevance,
    PathAsc,
    PathDesc,
    ModifiedNewest,
    ModifiedOldest,
    CreatedNewest,
    CreatedOldest,
}

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum VectorQueueCommand {
    #[command(about = "Report pending vector indexing work")]
    Status,
    #[command(about = "Run the pending vector indexing queue")]
    Run {
        #[arg(long, help = "Report the pending queue without embedding chunks")]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum VectorsCommand {
    #[command(about = "Embed pending chunks and update the vector index")]
    Index {
        #[arg(long, help = "Report pending vector work without writing embeddings")]
        dry_run: bool,
    },
    #[command(about = "Repair stale, missing, or mismatched vector rows")]
    Repair {
        #[arg(
            long,
            help = "Report the repair scope without mutating the vector index"
        )]
        dry_run: bool,
    },
    #[command(about = "Rebuild the vector index from scratch")]
    Rebuild {
        #[arg(
            long,
            help = "Report the rebuild scope without mutating the vector index"
        )]
        dry_run: bool,
    },
    #[command(about = "Inspect or run the explicit vector indexing queue")]
    Queue {
        #[command(subcommand)]
        command: VectorQueueCommand,
    },
    #[command(about = "Find nearest indexed chunks for text or a note")]
    Neighbors {
        #[arg(help = "Ad hoc text query to embed and search")]
        query: Option<String>,
        #[arg(long, help = "Existing note identifier to use as the similarity query")]
        note: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Recommend semantically related notes for one note")]
    Related {
        #[arg(
            help = "Note path, filename, or alias to use as the seed note; omit in a TTY session to pick interactively"
        )]
        note: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Report highly similar chunk pairs from the vector index")]
    Duplicates {
        #[arg(
            long,
            default_value_t = 0.95,
            help = "Minimum cosine similarity threshold for duplicate candidates"
        )]
        threshold: f32,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "List all stored embedding models in the vector index")]
    Models {
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Drop a stored embedding model and its vectors")]
    DropModel {
        #[arg(help = "Cache key of the model to drop (see `vectors models`)")]
        key: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum RepairCommand {
    #[command(about = "Rebuild the full-text search index from cached chunks")]
    Fts {
        #[arg(long, help = "Report the repair scope without mutating the cache")]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum GraphCommand {
    #[command(about = "Find the shortest resolved-link path between two notes")]
    Path {
        #[arg(
            help = "Starting note path, filename, or alias; omit in a TTY session to pick interactively"
        )]
        from: Option<String>,
        #[arg(
            help = "Destination note path, filename, or alias; omit in a TTY session to pick interactively"
        )]
        to: Option<String>,
    },
    #[command(about = "List notes with the highest combined link degree")]
    Hubs {
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Report candidate map-of-content style notes")]
    Moc {
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "List notes without outbound resolved note links")]
    DeadEnds {
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Report weakly connected components of the note graph")]
    Components {
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Summarize note-graph and vault analytics")]
    Stats {
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Show note-count, orphan, stale, and link trends over saved scans")]
    Trends {
        #[arg(
            long,
            default_value_t = 10,
            help = "Maximum number of checkpoints to include"
        )]
        limit: usize,
        #[command(flatten)]
        export: ExportArgs,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum CacheCommand {
    #[command(about = "Inspect cache sizes and row counts")]
    Inspect,
    #[command(about = "Verify cache invariants against derived indexes")]
    Verify {
        #[arg(long, help = "Return exit code 2 when one or more cache checks fail")]
        fail_on_errors: bool,
    },
    #[command(about = "Run SQLite VACUUM on the cache database")]
    Vacuum {
        #[arg(long, help = "Report the vacuum scope without mutating the cache")]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum SuggestCommand {
    #[command(about = "Report plain-text note mentions that could become links")]
    Mentions {
        #[arg(help = "Optional note path, filename, or alias to inspect")]
        note: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Report duplicate titles, alias collisions, and merge candidates")]
    Duplicates {
        #[command(flatten)]
        export: ExportArgs,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum SavedCommand {
    #[command(about = "List saved query and report definitions")]
    List,
    #[command(about = "Show one saved query or report definition")]
    Show {
        #[arg(help = "Saved report name")]
        name: String,
    },
    #[command(
        about = "Persist a saved search definition in .vulcan/reports",
        after_help = SEARCH_COMMAND_AFTER_HELP
    )]
    Search {
        #[arg(help = "Saved report name")]
        name: String,
        #[arg(
            help = "Full-text query string; supports phrases, `or`, `-term`, and inline tag:/path:/has: filters"
        )]
        query: String,
        #[arg(
            long = "where",
            help = "Typed property filter such as `status = done`; repeatable and combined with AND"
        )]
        filters: Vec<String>,
        #[arg(
            long,
            value_enum,
            default_value_t = SearchMode::Keyword,
            help = "Search strategy to store"
        )]
        mode: SearchMode,
        #[arg(long, help = "Restrict matches to notes carrying the given tag")]
        tag: Option<String>,
        #[arg(
            long = "path-prefix",
            help = "Restrict matches to paths under this prefix"
        )]
        path_prefix: Option<String>,
        #[arg(long = "has-property", help = "Require a property key to be present")]
        has_property: Option<String>,
        #[arg(long, value_enum, help = "Override result ordering")]
        sort: Option<SearchSortArg>,
        #[arg(long, help = "Require case-sensitive matching for unscoped terms")]
        match_case: bool,
        #[arg(
            long = "context-size",
            default_value_t = 18,
            help = "Approximate snippet context size for each search hit"
        )]
        context_size: usize,
        #[arg(long, help = "Persist the query string as raw FTS5 syntax")]
        raw_query: bool,
        #[arg(
            long,
            help = "Enable typo-tolerant fallback when the saved search runs"
        )]
        fuzzy: bool,
        #[arg(long, help = "Optional saved report description")]
        description: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(
        about = "Persist a saved property query definition in .vulcan/reports",
        after_help = NOTES_COMMAND_AFTER_HELP
    )]
    Notes {
        #[arg(help = "Saved report name")]
        name: String,
        #[arg(
            long = "where",
            help = "Filter expression such as `status = done`; repeatable and combined with AND"
        )]
        filters: Vec<String>,
        #[arg(
            long,
            help = "Property key or file field (`file.path`, `file.name`, `file.ext`, `file.mtime`) to sort by"
        )]
        sort: Option<String>,
        #[arg(long, help = "Sort descending instead of ascending")]
        desc: bool,
        #[arg(long, help = "Optional saved report description")]
        description: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Persist a saved Bases evaluation definition in .vulcan/reports")]
    Bases {
        #[arg(help = "Saved report name")]
        name: String,
        #[arg(help = "Vault-relative path to the .base file to evaluate")]
        file: String,
        #[arg(long, help = "Optional saved report description")]
        description: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Run one saved query or report definition")]
    Run {
        #[arg(help = "Saved report name")]
        name: String,
        #[command(flatten)]
        export: ExportArgs,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum CheckpointCommand {
    #[command(about = "Create or replace a named checkpoint from the current cache state")]
    Create {
        #[arg(help = "Checkpoint name")]
        name: String,
    },
    #[command(about = "List saved scan and manual checkpoints")]
    List {
        #[command(flatten)]
        export: ExportArgs,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum ExportCommand {
    #[command(about = "Write the cached search corpus as a static JSON index")]
    SearchIndex {
        #[arg(
            long,
            help = "Destination JSON file; omit to print the payload to stdout"
        )]
        path: Option<PathBuf>,
        #[arg(long, help = "Pretty-print the generated JSON payload")]
        pretty: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum AutomationCommand {
    #[command(about = "Run saved reports, checks, and repairs for non-interactive workflows")]
    Run {
        #[arg(help = "Saved report names to run")]
        reports: Vec<String>,
        #[arg(long, help = "Run every saved report definition in .vulcan/reports")]
        all_reports: bool,
        #[arg(
            long,
            help = "Run an incremental scan before checks and report execution"
        )]
        scan: bool,
        #[arg(long, conflicts_with = "doctor_fix", help = "Include doctor results")]
        doctor: bool,
        #[arg(
            long,
            conflicts_with = "doctor",
            help = "Apply deterministic doctor fixes before reporting status"
        )]
        doctor_fix: bool,
        #[arg(long, help = "Verify cache invariants")]
        verify_cache: bool,
        #[arg(long, help = "Repair the FTS index from cached chunks")]
        repair_fts: bool,
        #[arg(
            long,
            help = "Return exit code 2 when completed checks still report issues"
        )]
        fail_on_issues: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum TemplateSubcommand {
    #[command(about = "Insert a rendered template into an existing note")]
    Insert {
        #[arg(help = "Template name or filename stem")]
        template: String,
        #[arg(
            help = "Note path, filename, or alias to update; omit in a TTY session to pick interactively"
        )]
        note: Option<String>,
        #[arg(long, conflicts_with = "append", help = "Insert after frontmatter")]
        prepend: bool,
        #[arg(
            long,
            conflicts_with = "prepend",
            help = "Append to the end of the note"
        )]
        append: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum Command {
    #[command(about = "Initialize .vulcan/ state for a vault")]
    Init,
    #[command(about = "Rebuild the cache from disk")]
    Rebuild {
        #[arg(long, help = "Report rebuild scope without mutating the cache")]
        dry_run: bool,
    },
    #[command(about = "Repair derived indexes and cache structures")]
    Repair {
        #[command(subcommand)]
        command: RepairCommand,
    },
    #[command(about = "Watch the vault for filesystem changes and keep the cache fresh")]
    Watch {
        #[arg(
            long,
            default_value_t = 250,
            help = "Event coalescing window in milliseconds"
        )]
        debounce_ms: u64,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Serve local cache-backed HTTP APIs for repeated queries")]
    Serve {
        #[arg(
            long,
            default_value = "127.0.0.1:3210",
            help = "Bind address for the local HTTP server"
        )]
        bind: String,
        #[arg(long, help = "Disable the background watcher refresh loop")]
        no_watch: bool,
        #[arg(
            long,
            default_value_t = 250,
            help = "Watcher debounce window in milliseconds when serve watch mode is enabled"
        )]
        debounce_ms: u64,
        #[arg(
            long,
            help = "Optional shared secret required in the X-Vulcan-Token header"
        )]
        auth_token: Option<String>,
    },
    #[command(about = "Scan the vault and update the cache")]
    Scan {
        #[arg(long, help = "Force a full scan instead of incremental reconciliation")]
        full: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "List outgoing links for a note")]
    Links {
        #[arg(
            help = "Note path, filename, or alias to inspect; omit in a TTY session to pick interactively"
        )]
        note: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "List inbound links pointing at a note")]
    Backlinks {
        #[arg(
            help = "Note path, filename, or alias to inspect; omit in a TTY session to pick interactively"
        )]
        note: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Analyze the resolved note graph")]
    Graph {
        #[command(subcommand)]
        command: GraphCommand,
    },
    #[command(
        about = "Search indexed note content",
        after_help = SEARCH_COMMAND_AFTER_HELP
    )]
    Search {
        #[arg(
            help = "Full-text query string; supports phrases, `or`, `-term`, and inline tag:/path:/has: filters"
        )]
        query: String,
        #[arg(
            long = "where",
            help = "Typed property filter such as `status = done`; repeatable and combined with AND"
        )]
        filters: Vec<String>,
        #[arg(
            long,
            value_enum,
            default_value_t = SearchMode::Keyword,
            help = "Search strategy to use"
        )]
        mode: SearchMode,
        #[arg(long, help = "Restrict matches to notes carrying the given tag")]
        tag: Option<String>,
        #[arg(
            long = "path-prefix",
            help = "Restrict matches to paths under this prefix"
        )]
        path_prefix: Option<String>,
        #[arg(long = "has-property", help = "Require a property key to be present")]
        has_property: Option<String>,
        #[arg(long, value_enum, help = "Persist a non-default result ordering")]
        sort: Option<SearchSortArg>,
        #[arg(long, help = "Persist case-sensitive matching as the default")]
        match_case: bool,
        #[arg(
            long = "context-size",
            default_value_t = 18,
            help = "Approximate snippet context size for each search hit"
        )]
        context_size: usize,
        #[arg(long, help = "Treat the query string as raw FTS5 syntax")]
        raw_query: bool,
        #[arg(long, help = "Retry empty searches with typo-tolerant term expansion")]
        fuzzy: bool,
        #[arg(long, help = "Include the parsed search plan and score details")]
        explain: bool,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(
        about = "Query notes by typed properties",
        after_help = NOTES_COMMAND_AFTER_HELP
    )]
    Notes {
        #[arg(
            long = "where",
            help = "Filter expression such as `status = done`; repeatable and combined with AND"
        )]
        filters: Vec<String>,
        #[arg(
            long,
            help = "Property key or file field (`file.path`, `file.name`, `file.ext`, `file.mtime`) to sort by"
        )]
        sort: Option<String>,
        #[arg(long, help = "Sort descending instead of ascending")]
        desc: bool,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(
        about = "Evaluate and maintain Bases views",
        after_help = BASES_COMMAND_AFTER_HELP
    )]
    Bases {
        #[command(subcommand)]
        command: BasesCommand,
    },
    #[command(about = "Suggest link and merge opportunities from indexed notes")]
    Suggest {
        #[command(subcommand)]
        command: SuggestCommand,
    },
    #[command(about = "Persist and run saved reports from .vulcan/reports")]
    Saved {
        #[command(subcommand)]
        command: SavedCommand,
    },
    #[command(about = "Capture and inspect named cache-state checkpoints")]
    Checkpoint {
        #[command(subcommand)]
        command: CheckpointCommand,
    },
    #[command(about = "Write static export artifacts derived from the cache")]
    Export {
        #[command(subcommand)]
        command: ExportCommand,
    },
    #[command(about = "Report note, link, property, and embedding changes since a baseline")]
    Changes {
        #[arg(
            long,
            help = "Compare against a named checkpoint instead of the previous scan"
        )]
        checkpoint: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(
        about = "Show one note's changes since git HEAD, the last scan, or a checkpoint",
        after_help = DIFF_COMMAND_AFTER_HELP
    )]
    Diff {
        #[arg(
            help = "Note path, filename, or alias to inspect; omit in a TTY session to pick interactively"
        )]
        note: Option<String>,
        #[arg(long, help = "Named checkpoint to compare against instead of git HEAD")]
        since: Option<String>,
    },
    #[command(
        about = "Append a quick capture entry to the configured inbox note",
        after_help = INBOX_COMMAND_AFTER_HELP
    )]
    Inbox {
        #[arg(help = "Text to append, or `-` to read from stdin")]
        text: Option<String>,
        #[arg(long, help = "Read appended text from a file")]
        file: Option<PathBuf>,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(
        about = "Create notes from templates or insert templates into existing notes",
        after_help = TEMPLATE_COMMAND_AFTER_HELP
    )]
    Template {
        #[command(subcommand)]
        command: Option<TemplateSubcommand>,
        #[arg(help = "Template name or filename stem")]
        name: Option<String>,
        #[arg(long, help = "List available templates")]
        list: bool,
        #[arg(long, help = "Output path for the new note")]
        path: Option<String>,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Run multiple saved reports for automation and scheduled jobs")]
    Batch {
        #[arg(help = "Saved report names to run")]
        names: Vec<String>,
        #[arg(long, help = "Run every saved report definition in .vulcan/reports")]
        all: bool,
    },
    #[command(about = "Run checks, repairs, and saved reports for CI and scripts")]
    Automation {
        #[command(subcommand)]
        command: AutomationCommand,
    },
    #[command(about = "Cluster indexed vectors into topical groups")]
    Cluster {
        #[arg(long, default_value_t = 8, help = "Requested cluster count")]
        clusters: usize,
        #[arg(long, help = "Report cluster assignments without persisting them")]
        dry_run: bool,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Recommend semantically related notes for one note")]
    Related {
        #[arg(
            help = "Note path, filename, or alias to use as the seed note; omit in a TTY session to pick interactively"
        )]
        note: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(
        about = "Open a persistent note browser TUI",
        after_help = BROWSE_COMMAND_AFTER_HELP
    )]
    Browse {
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(
        about = "Open a note in $VISUAL/$EDITOR and refresh the cache afterwards",
        after_help = EDIT_COMMAND_AFTER_HELP
    )]
    Edit {
        #[arg(
            help = "Note path, filename, or alias to edit; with --new, the new note path to create"
        )]
        note: Option<String>,
        #[arg(long, help = "Create a new note instead of resolving an existing one")]
        new: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(
        about = "Open a note in the Obsidian desktop app",
        after_help = OPEN_COMMAND_AFTER_HELP
    )]
    Open {
        #[arg(
            help = "Note path, filename, or alias to open; omit in a TTY session to pick interactively"
        )]
        note: Option<String>,
    },
    #[command(about = "Run vector indexing and similarity commands")]
    Vectors {
        #[command(subcommand)]
        command: VectorsCommand,
    },
    #[command(about = "Move a note or attachment and safely rewrite inbound links")]
    Move {
        #[arg(help = "Existing source note or attachment path")]
        source: String,
        #[arg(help = "Destination note or attachment path")]
        dest: String,
        #[arg(long, help = "Report rewrite changes without moving files")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Convert unambiguous plain-text note mentions into links")]
    LinkMentions {
        #[arg(help = "Optional note path, filename, or alias to update")]
        note: Option<String>,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(
        about = "Apply a literal find/replace across notes selected by filters",
        after_help = REWRITE_COMMAND_AFTER_HELP
    )]
    Rewrite {
        #[arg(
            long = "where",
            help = "Typed property filter such as `status = done`; repeatable and combined with AND"
        )]
        filters: Vec<String>,
        #[arg(long, help = "Literal text to find")]
        find: String,
        #[arg(long, help = "Replacement text")]
        replace: String,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Inspect the vault for broken or suspicious state")]
    Doctor {
        #[arg(long, help = "Apply deterministic local repairs")]
        fix: bool,
        #[arg(
            long,
            help = "Report planned repairs without mutating the vault or cache"
        )]
        dry_run: bool,
        #[arg(
            long,
            help = "Return exit code 2 when the final doctor summary still reports issues"
        )]
        fail_on_issues: bool,
    },
    #[command(
        about = "Set a frontmatter property on notes selected by query filters",
        after_help = "\
Filter syntax:
  Repeat --where to combine filters with AND.
  Form: <field> <operator> <value>

Examples:
  vulcan update --where 'status = draft' --key status --value done --dry-run
  vulcan update --where 'tags contains wip' --key reviewed --value true
  vulcan update --where 'file.path starts_with \"Archive/\"' --key archived --value true"
    )]
    Update {
        #[arg(
            long = "where",
            help = "Filter expression such as `status = draft`; repeatable and combined with AND"
        )]
        filters: Vec<String>,
        #[arg(long, help = "Frontmatter property key to set")]
        key: String,
        #[arg(
            long,
            help = "New value for the property (YAML scalar or quoted string)"
        )]
        value: String,
        #[arg(long, help = "Report planned changes without modifying files")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(
        about = "Remove a frontmatter property from notes selected by query filters",
        after_help = "\
Filter syntax:
  Repeat --where to combine filters with AND.

Examples:
  vulcan unset --where 'status = draft' --key draft_notes --dry-run
  vulcan unset --where 'file.path starts_with \"Archive/\"' --key due"
    )]
    Unset {
        #[arg(
            long = "where",
            help = "Filter expression such as `status = draft`; repeatable and combined with AND"
        )]
        filters: Vec<String>,
        #[arg(long, help = "Frontmatter property key to remove")]
        key: String,
        #[arg(long, help = "Report planned removals without modifying files")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Rename a frontmatter property key across notes")]
    RenameProperty {
        #[arg(help = "Existing property key")]
        old: String,
        #[arg(help = "Replacement property key")]
        new: String,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Merge one tag into another across frontmatter and note bodies")]
    MergeTags {
        #[arg(help = "Source tag to replace")]
        source: String,
        #[arg(help = "Destination tag to write")]
        dest: String,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Rename an alias inside one note's frontmatter")]
    RenameAlias {
        #[arg(help = "Note path, filename, or alias to update")]
        note: String,
        #[arg(help = "Existing alias text")]
        old: String,
        #[arg(help = "Replacement alias text")]
        new: String,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Rename a heading and rewrite inbound heading links")]
    RenameHeading {
        #[arg(help = "Note path, filename, or alias containing the heading")]
        note: String,
        #[arg(help = "Existing heading text")]
        old: String,
        #[arg(help = "Replacement heading text")]
        new: String,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Rename a block reference and rewrite inbound block links")]
    RenameBlockRef {
        #[arg(help = "Note path, filename, or alias containing the block reference")]
        note: String,
        #[arg(help = "Existing block reference id without the ^ prefix")]
        old: String,
        #[arg(help = "Replacement block reference id without the ^ prefix")]
        new: String,
        #[arg(long, help = "Report planned rewrites without modifying files")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Inspect and maintain the SQLite cache")]
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    #[command(
        about = "Run a query using the human DSL or a JSON payload",
        after_help = "\
Query DSL syntax:
  from notes
    [where <field> <op> <value> [and <field> <op> <value>...]]
    [select <field>[,<field>...]]
    [order by <field> [desc|asc]]
    [limit <n>]
    [offset <n>]

JSON payload (--json flag):
  {\"source\":\"notes\",\"predicates\":[{\"field\":\"status\",\"operator\":\"eq\",\"value\":\"done\"}],
   \"sort\":{\"field\":\"file.mtime\",\"descending\":true},\"limit\":10}

Operators:  = | > | >= | < | <= | starts_with | contains
            (JSON: eq | gt | gte | lt | lte | starts_with | contains)

Examples:
  vulcan query 'from notes where status = done order by file.mtime desc limit 10'
  vulcan query 'from notes where tags contains sprint and reviewed = true'
  vulcan query --json '{\"source\":\"notes\",\"predicates\":[{\"field\":\"status\",\"operator\":\"eq\",\"value\":\"done\"}]}'
  vulcan query --explain 'from notes where status = backlog'"
    )]
    Query {
        #[arg(
            help = "DSL query string; e.g. 'from notes where status = done order by file.mtime desc'"
        )]
        dsl: Option<String>,
        #[arg(
            long,
            help = "JSON query payload; mutually exclusive with the positional DSL argument"
        )]
        json: Option<String>,
        #[arg(long, help = "Print the parsed query AST alongside the results")]
        explain: bool,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(
        about = "Describe the CLI schema and command surface",
        after_help = DESCRIBE_COMMAND_AFTER_HELP
    )]
    Describe,
    #[command(
        about = "Generate shell completion scripts",
        after_help = COMPLETIONS_COMMAND_AFTER_HELP
    )]
    Completions {
        #[arg(help = "Shell to generate completions for")]
        shell: Shell,
    },
}

#[derive(Debug, Clone, Parser)]
#[command(
    author,
    version,
    about = "Headless CLI for Obsidian-style vaults and Markdown directories",
    long_about = None,
    after_help = ROOT_AFTER_HELP
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        default_value = ".",
        help = "Vault root directory"
    )]
    pub vault: PathBuf,

    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t = OutputFormat::Human,
        help = "Output format"
    )]
    pub output: OutputFormat,

    #[arg(
        long,
        global = true,
        value_enum,
        help = "Override automatic cache refresh mode for cache-backed commands"
    )]
    pub refresh: Option<RefreshMode>,

    #[arg(
        long,
        global = true,
        value_delimiter = ',',
        help = "Comma-separated field selection for list output"
    )]
    pub fields: Option<Vec<String>>,

    #[arg(
        long,
        global = true,
        help = "Embedding provider override for vector commands"
    )]
    pub provider: Option<String>,

    #[arg(long, global = true, help = "Maximum number of rows to return")]
    pub limit: Option<usize>,

    #[arg(
        long,
        global = true,
        default_value_t = 0,
        help = "Row offset for pagination"
    )]
    pub offset: usize,

    #[arg(
        long,
        global = true,
        action = ArgAction::SetTrue,
        help = "Enable extra diagnostic output"
    )]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}
