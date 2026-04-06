use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;

const ROOT_AFTER_HELP: &str = "\
Quick start:
  vulcan query 'from notes where status = \"open\"'    Query by property
  vulcan search \"meeting notes\"                       Full-text search
  vulcan today                                         Open today's daily note
  vulcan note create Projects/new-idea.md              Create a note
  vulcan run                                           Start the JS REPL

Command groups (run `vulcan help` for the full grouped reference):
  Notes:       note, inbox
  Query:       query, search, ls, notes, backlinks, links, changes
  Refactor:    refactor, move, rename-property
  Tasks:       tasks, kanban
  Periodic:    today, daily, weekly, monthly, periodic, template
  Plugins:     bases, dataview
  Analysis:    graph, suggest, cluster, related, doctor
  Index:       index, vectors, cache, repair
  Runtime:     run, web
  Git:         git
  Automation:  saved, automation, batch, export, checkpoint
  Setup:       init, config, trust

Reference:
  vulcan help <command>              Integrated help for any command
  vulcan help --search <keyword>     Search all help topics
  vulcan query --help                DQL syntax and field reference
  vulcan search --help               Search query syntax and filters
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
  Use --regex for an explicit regex search without /pattern/ delimiters.

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
  create       create a note that matches the first Bases view context
  view-add     add a validated view definition
  view-delete  remove a view definition
  view-rename  rename a view
  view-edit    adjust filters, columns, sort, and grouping

Notes:
  view-* commands rewrite the parsed .base model instead of patching YAML text blindly.
  Mutating bases commands support --dry-run and --no-commit.
  `create` derives folder and equality frontmatter from the first view; the TUI `n` hotkey uses the current view.

Examples:
  vulcan bases eval release.base
  vulcan bases create release.base --title \"Launch Plan\"
  vulcan bases tui release.base
  vulcan bases view-add release.base Inbox --filter 'status = idea' --column file.name";

const BROWSE_COMMAND_AFTER_HELP: &str = "\
Browse modes:
  default      fuzzy note picker with live preview
  Ctrl-F       indexed full-text search with snippet preview
  Ctrl-T       tag filter mode
  Ctrl-P       property filter mode
  Ctrl-Y       periodic daily-note calendar mode
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
  In Ctrl-Y mode, arrows move by day, PageUp/PageDown change month, and typing YYYY-MM or YYYY-MM-DD jumps directly.
  In fuzzy/tag/property modes, `o` opens a selected Kanban board when the query is empty.
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

const NOTE_COMMAND_AFTER_HELP: &str = "\
Subcommands:
  get         read one note, optionally with heading, block, line, or regex selectors
  set         replace one note's contents from stdin or --file
  create      create a new note, optionally from a template and extra frontmatter
  append      append text to the end of a note, at the top, or under a heading
  patch       perform a guarded single-note find/replace
  info        show one note's summary metadata and graph stats
  history     show git history scoped to one note

Notes:
  `--check` runs non-blocking doctor-like diagnostics for the resulting note.
  Mutating note commands support --no-commit; `patch` also supports --dry-run.
  `note get --output json` returns content, parsed frontmatter, and selection metadata.
  `note append --periodic <daily|weekly|monthly>` creates the target periodic note first when missing.
  Repeat `--var key=value` to satisfy QuickAdd-style `{{VALUE}}` / `{{VDATE:...}}` prompts in automation.

Examples:
  vulcan note get Projects/Alpha --heading Status
  vulcan note info Projects/Alpha
  vulcan note history Projects/Alpha --limit 5
  vulcan note set Projects/Alpha --no-frontmatter < body.md
  vulcan note create Inbox/Idea --template daily --frontmatter status=idea
  vulcan note append Projects/Alpha \"Shipped\" --after-heading \"## Log\"
  vulcan note append Projects/Alpha \"Pinned\" --prepend
  vulcan note append \"- {{VALUE}}\" --periodic daily --var value=\"Called Alice\"
  vulcan note patch Projects/Alpha --find TODO --replace DONE";

const QUERY_COMMAND_AFTER_HELP: &str = "\
Query DSL syntax:
  from notes
    [where <field> <op> <value> [and <field> <op> <value>...]]
    [select <field>[,<field>...]]
    [order by <field> [desc|asc]]
    [limit <n>]
    [offset <n>]

Operators:
  = | > | >= | < | <= | starts_with | contains | matches | matches_i

Formats:
  table   structured note rows (default)
  paths   one path per line
  detail  path, metadata summary, and content preview
  count   matched row count only

Examples:
  vulcan query --format paths 'from notes where status = done'
  vulcan query --glob 'Projects/**' 'from notes'
  vulcan query 'from notes where file.name matches \"^2026-\"'
  vulcan query 'from notes where owner matches_i \"alice\"'";

const NOTE_GET_COMMAND_AFTER_HELP: &str = "\
Selectors:
  --heading <name>        limit to one heading section, including nested subheadings
  --block-ref <id>        limit to the block tagged with ^<id>
  --lines <range>         limit to a 1-based line range within the current selection
  --match <regex>         keep only lines matching the regex
  --context <n>           include N surrounding lines around each --match hit
  --no-frontmatter        strip a leading YAML frontmatter block from the output
  --raw                   print only the selected content without line numbers

Line range syntax:
  1-10    first ten lines
  50-     line 50 through the end
  -5      last five lines
  7       only line 7

Examples:
  vulcan note get Dashboard
  vulcan note get Dashboard --heading Tasks --match TODO --context 1
  vulcan note get Dashboard --block-ref status-card
  vulcan note get Dashboard --lines 10-20 --raw";

const GIT_COMMAND_AFTER_HELP: &str = "\
Subcommands:
  status       show staged, unstaged, and untracked vault files
  log          show recent commits in the vault repository
  diff         show the current diff for one path or the whole vault
  commit       stage changed vault files and create a commit
  blame        show per-line authorship for one tracked file

Notes:
  `commit` stages vault files only and skips `.vulcan/`.
  `diff` accepts an optional vault-relative path; without one it prints the full working-tree diff for eligible files.
  `blame` and path-scoped `diff` operate on vault-relative file paths, not note aliases.

Examples:
  vulcan git status
  vulcan git log --limit 5
  vulcan git diff Home.md
  vulcan git commit -m \"Update daily notes\"
  vulcan git blame Projects/Alpha.md";

const WEB_COMMAND_AFTER_HELP: &str = "\
Subcommands:
  search      query the configured web search backend
  fetch       fetch one URL as markdown, html, or raw content

Notes:
  `web search` reads backend settings from `[web.search]` in `.vulcan/config.toml`.
  `web fetch --extract-article` prefers `<article>`/`<main>` content when present.
  `web fetch` uses a Vulcan user-agent and performs a best-effort robots.txt check before fetching.

Examples:
  vulcan web search \"release notes\" --limit 5
  vulcan web fetch https://example.com --mode markdown
  vulcan web fetch https://example.com --mode raw --save page.bin";

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
  If .obsidian/templates.json or the Templater plugin configures a template folder, Vulcan lists those folders too.
  When the same template exists in both places, .vulcan/templates takes precedence.
  NAME can be the full filename or the filename stem.

Variables:
  {{title}} {{date}} {{time}} {{datetime}} {{uuid}}
  {{date:YYYY-MM-DD}} {{time:HH:mm}} {{date:dddd, MMMM Do YYYY}}
  <% tp.file.title %> <%* tR += tp.date.now() %> <%+ tp.frontmatter.status %>

Configuration:
  Default template date/time formats live under [templates] in .vulcan/config.toml.
  date_format applies to {{date}} and time_format applies to {{time}}.
  web_allowlist gates tp.web.request()/tp.obsidian.requestUrl() hosts.

Notes:
  If --path is omitted, Vulcan creates <date>-<template-name>.md in the vault root.
  In an interactive terminal, the new note is opened in $VISUAL/$EDITOR after rendering.
  --engine auto detects <% ... %> tags and switches to the Templater-compatible renderer.
  Repeat --var key=value to satisfy tp.system.prompt()/suggester() in automation and CI.
  `template insert` appends by default; use --prepend to insert after frontmatter instead.
  If the insert target note is omitted in an interactive terminal, Vulcan opens the note picker.
  `template preview` renders without writing files and disables mutating tp.file.* helpers.

Examples:
  vulcan template --list
  vulcan template daily --path Daily/2026-03-26 --engine auto
  vulcan template meeting
  vulcan template insert daily Projects/Alpha --var project=Vulcan
  vulcan template insert daily --prepend
  vulcan template preview daily --path Journal/Today";

const OPEN_COMMAND_AFTER_HELP: &str = "\
Behavior:
  Resolves NOTE by path, filename, or alias.
  If NOTE is omitted in an interactive terminal, Vulcan opens the note picker.
  Launches obsidian://open?vault=<vault>&file=<path> through the platform opener.

Examples:
  vulcan open Projects/Alpha
  vulcan open Daily/2026-03-26
  vulcan open";

const RUN_COMMAND_AFTER_HELP: &str = "\
The JS runtime exposes the full vault API: query notes, search, read/write frontmatter,
call web APIs, and run DQL — all from a sandboxed rquickjs context.

Sandbox levels:
  strict   default — no filesystem or network access beyond vault APIs
  fs       allow filesystem access (read/write outside the vault)
  net      allow outbound HTTP requests
  none     no restrictions

Examples:
  vulcan run                                     Start interactive REPL
  vulcan run -e 'dql(\"from notes limit 5\")'     Evaluate a DQL snippet
  vulcan run -e 'notes({limit:3}).map(n=>n.path)' Inspect note paths
  vulcan run script.js                           Execute a script file
  vulcan run --eval-file prelude.js              Load a file then open REPL
  vulcan run --sandbox net -e 'web.search(\"rust async\")'  Web search
  vulcan run --no-startup                        Skip startup.js even if trusted
  cat query.js | vulcan run                      Pipe a script via stdin";

const DESCRIBE_COMMAND_AFTER_HELP: &str = "\
Output:
  json-schema    runtime CLI schema with commands, options, defaults, and after-help text
  openai-tools   OpenAI function-calling tool definitions
  mcp            MCP-style tool definitions

Examples:
  vulcan describe
  vulcan describe --format openai-tools
  vulcan describe --format mcp
  vulcan --output json describe > vulcan-schema.json";

const HELP_COMMAND_AFTER_HELP: &str = "\
Topics:
  Commands: help query, help note get, help refactor
  Concepts: help filters, help query-dsl, help getting-started, help examples

Examples:
  vulcan help
  vulcan help query
  vulcan help note get --output json
  vulcan help --search graph";

const COMPLETIONS_COMMAND_AFTER_HELP: &str = "\
Examples:
  vulcan completions bash > ~/.local/share/bash-completion/completions/vulcan
  vulcan completions fish > ~/.config/fish/completions/vulcan.fish";

const DATAVIEW_COMMAND_AFTER_HELP: &str = "\
Subcommands:
  inline      evaluate Dataview inline expressions from one note
  query       evaluate a DQL query string directly
  query-js    evaluate a DataviewJS snippet directly
  eval        evaluate indexed ```dataview``` blocks from one note

Examples:
  vulcan dataview inline Dashboard
  vulcan --output json dataview inline Projects/Alpha
  vulcan dataview query 'TABLE status FROM \"Projects\"'
  vulcan dataview query-js 'dv.current()' --file Dashboard
  vulcan dataview eval Dashboard --block 0";

const TASKS_COMMAND_AFTER_HELP: &str = "\
Subcommands:
  add         create one TaskNotes task file
  show        display one TaskNotes task with structured properties
  edit        open one TaskNotes task in $EDITOR
  set         update one TaskNotes task property
  complete    mark one task as completed
  archive     archive one completed TaskNotes task
  convert     convert a note, line, or heading into a TaskNotes task
  create      append one inline task to a note
  reschedule  change one task's due date
  query       evaluate a Tasks plugin query string directly
  eval        evaluate indexed ```tasks``` blocks from one note
  list        list indexed tasks, optionally filtered
  next        show upcoming recurring task instances
  blocked     list currently blocked tasks with their blockers
  graph       show the task dependency graph

Notes:
  `tasks query` uses the Tasks DSL.
  `tasks list --filter` accepts either the Tasks DSL or a Dataview expression.
  `tasks list` excludes archived TaskNotes by default; pass `--include-archived` to include them.
  Vault task defaults under [tasks] in .vulcan/config.toml apply to Tasks queries.

Examples:
  vulcan tasks add \"Buy groceries tomorrow @home\"
  vulcan tasks create \"Call Alice tomorrow\" --in Inbox
  vulcan tasks reschedule Write\\ Docs --due 2026-04-12
  vulcan tasks query 'not done'
  vulcan tasks eval Dashboard --block 0
  vulcan tasks list
  vulcan tasks list --source file --status in-progress --sort-by due
  vulcan tasks list --filter 'completed && file.name = \"Alpha\"'
  vulcan tasks next 5 --from 2026-03-29
  vulcan tasks blocked
  vulcan tasks graph";

const CONFIG_COMMAND_AFTER_HELP: &str = "\
Subcommands:
  import core    import Obsidian core settings into Vulcan config
  import dataview import Obsidian Dataview plugin settings into .vulcan/config.toml
  import kanban  import Obsidian Kanban plugin settings into .vulcan/config.toml
  import periodic-notes import Obsidian Daily Notes + Periodic Notes settings into .vulcan/config.toml
  import quickadd import Obsidian QuickAdd plugin settings into .vulcan/config.toml
  import tasknotes import Obsidian TaskNotes plugin settings into .vulcan/config.toml
  import templater import Obsidian Templater plugin settings into .vulcan/config.toml
  import tasks   import Obsidian Tasks plugin settings into .vulcan/config.toml

Notes:
  Import commands preserve unrelated config sections and overwrite the mapped target keys.
  Shared flags: --dry-run, --target <shared|local>, --no-commit
  Use `config import --all` to apply every detected importer in registry order.
  Use `config import --list` to inspect detectable sources without writing.
  When git auto-commit is enabled for mutations, config imports participate like other mutating commands.

Examples:
  vulcan config import core
  vulcan config import dataview
  vulcan config import kanban
  vulcan config import --all --dry-run
  vulcan config import --list
  vulcan config import periodic-notes
  vulcan config import quickadd
  vulcan config import tasknotes --dry-run
  vulcan config import tasks --dry-run
  vulcan config import templater --target local
  vulcan --output json config import tasks";

const DAILY_COMMAND_AFTER_HELP: &str = "\
Subcommands:
  today       open or create today's daily note
  show        display one daily note's contents
  list        list daily notes and extracted schedule events
  export-ics  export extracted daily-note events as an ICS calendar
  append      append text to one daily note

Notes:
  `list --week` and `list --month` expand around the current date using the configured periodic week start.
  `show` defaults to today. `append` creates the daily note first when it does not exist.

Examples:
  vulcan daily today
  vulcan daily today --no-edit
  vulcan daily show 2026-04-03
  vulcan daily list --week
  vulcan daily export-ics --month --path Journal.ics
  vulcan daily append \"Called Alice\" --heading \"## Log\"";

const PERIODIC_COMMAND_AFTER_HELP: &str = "\
Behavior:
  `periodic <type> [date]` opens or creates the configured periodic note for that date.
  `weekly [date]` and `monthly [date]` are shorthand for `periodic weekly|monthly`.

Subcommands:
  list        list indexed periodic notes
  gaps        show missing periodic notes across a date range

Examples:
  vulcan weekly
  vulcan monthly 2026-04-03 --no-edit
  vulcan periodic yearly 2026-01-01
  vulcan periodic list --type daily
  vulcan periodic gaps --type daily --from 2026-04-01 --to 2026-04-07";

const TODAY_COMMAND_AFTER_HELP: &str = "\
Behavior:
  `today` is a top-level alias for `daily today`.
  It opens or creates the configured daily note for the current date.

Examples:
  vulcan today
  vulcan today --no-edit";

const KANBAN_COMMAND_AFTER_HELP: &str = "\
Subcommands:
  list        list indexed Kanban boards
  show        display one board by column
  cards       list cards from one board with optional filters
  archive     move one card into the archive column
  move        move one card between active columns
  add         add a new card to one active column

Notes:
  `kanban show` defaults to column counts; add `--verbose` to include cards.
  `kanban show --include-archive` adds the parsed archive section back into the output.
  `kanban cards --status` matches a task status character, status name, or status type.
  `kanban archive` rewrites the board note, supports `--dry-run`, and honors auto-commit unless `--no-commit` is set.
  `kanban move` rewrites the board note and respects `new_card_insertion_method` for the target column.
  `kanban add` inserts a new list item using the target column's configured insertion mode.

Examples:
  vulcan kanban list
  vulcan kanban show Board
  vulcan kanban show Board --verbose
  vulcan kanban show Board --include-archive
  vulcan kanban cards Board --column Todo
  vulcan kanban archive Board build-release
  vulcan kanban move Board build-release Done
  vulcan kanban add Board Todo \"Build release\"
  vulcan --output json kanban cards Board --status IN_PROGRESS";

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
    #[command(about = "Create a note from the first Bases view context")]
    Create {
        #[arg(help = "Vault-relative path to the .base file")]
        file: String,
        #[arg(long, help = "Optional note title; defaults to Untitled")]
        title: Option<String>,
        #[arg(long, help = "Preview the derived path, properties, and template")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
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
        #[arg(
            long,
            default_value_t = 50,
            help = "Maximum number of duplicate pairs to report"
        )]
        limit: usize,
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
pub enum IndexCommand {
    #[command(about = "Initialize .vulcan/ state for a vault")]
    Init(InitArgs),
    #[command(about = "Scan the vault and update the cache")]
    Scan {
        #[arg(long, help = "Force a full scan instead of incremental reconciliation")]
        full: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
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
pub enum ConfigImportCommand {
    #[command(about = "Import Obsidian core settings")]
    Core,
    #[command(about = "Import Obsidian Dataview plugin settings")]
    Dataview,
    #[command(about = "Import Obsidian Templater plugin settings")]
    Templater,
    #[command(about = "Import Obsidian QuickAdd plugin settings")]
    Quickadd,
    #[command(about = "Import Obsidian Kanban plugin settings")]
    Kanban,
    #[command(about = "Import Obsidian Daily Notes and Periodic Notes settings")]
    PeriodicNotes,
    #[command(
        name = "tasknotes",
        about = "Import Obsidian TaskNotes plugin settings"
    )]
    TaskNotes,
    #[command(about = "Import Obsidian Tasks plugin settings")]
    Tasks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConfigImportTargetArg {
    Shared,
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct ConfigImportArgs {
    #[arg(
        long,
        global = true,
        help = "Preview config changes without writing files"
    )]
    pub dry_run: bool,
    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t = ConfigImportTargetArg::Shared,
        help = "Select the target Vulcan config file"
    )]
    pub target: ConfigImportTargetArg,
    #[arg(long, global = true, help = "Suppress auto-commit for this invocation")]
    pub no_commit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct ConfigImportSelection {
    #[command(subcommand)]
    pub command: Option<ConfigImportCommand>,
    #[arg(
        long,
        conflicts_with = "list",
        help = "Import every detected Obsidian source in registry order"
    )]
    pub all: bool,
    #[arg(
        long,
        conflicts_with = "all",
        help = "List detectable import sources without writing files"
    )]
    pub list: bool,
    #[command(flatten)]
    pub args: ConfigImportArgs,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum ConfigCommand {
    #[command(about = "Import compatible Obsidian plugin settings")]
    Import(ConfigImportSelection),
}

#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct InitArgs {
    #[arg(
        long,
        conflicts_with = "no_import",
        help = "Import all detected settings after initialization"
    )]
    pub import: bool,
    #[arg(
        long,
        conflicts_with = "import",
        help = "Suppress import detection suggestions after initialization"
    )]
    pub no_import: bool,
    #[arg(
        long,
        help = "Write AGENTS.md plus bundled AI skill markdown files into the vault"
    )]
    pub agent_files: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct PeriodicOpenArgs {
    #[arg(help = "Reference date for the period (defaults to today)")]
    pub date: Option<String>,
    #[arg(long, help = "Create the note without opening it in the editor")]
    pub no_edit: bool,
    #[arg(long, help = "Suppress auto-commit for this invocation")]
    pub no_commit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum DailyCommand {
    #[command(about = "Open or create today's daily note")]
    Today {
        #[arg(long, help = "Create the note without opening it in the editor")]
        no_edit: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Display one daily note's contents")]
    Show {
        #[arg(help = "Date to show (defaults to today)")]
        date: Option<String>,
    },
    #[command(about = "List daily notes and extracted schedule events")]
    List {
        #[arg(long, help = "Start date for the listing window")]
        from: Option<String>,
        #[arg(long, help = "End date for the listing window")]
        to: Option<String>,
        #[arg(long, conflicts_with = "month", help = "Use the current week")]
        week: bool,
        #[arg(long, conflicts_with = "week", help = "Use the current month")]
        month: bool,
    },
    #[command(about = "Export daily-note events as an ICS calendar")]
    ExportIcs {
        #[arg(long, help = "Start date for the export window")]
        from: Option<String>,
        #[arg(long, help = "End date for the export window")]
        to: Option<String>,
        #[arg(long, conflicts_with = "month", help = "Use the current week")]
        week: bool,
        #[arg(long, conflicts_with = "week", help = "Use the current month")]
        month: bool,
        #[arg(long, help = "Write the generated calendar to this .ics file")]
        path: Option<PathBuf>,
        #[arg(long, help = "Calendar name to embed in the ICS export")]
        calendar_name: Option<String>,
    },
    #[command(about = "Append text to one daily note")]
    Append {
        #[arg(help = "Text to append")]
        text: String,
        #[arg(long, help = "Optional heading to append under")]
        heading: Option<String>,
        #[arg(long, help = "Date to append to (defaults to today)")]
        date: Option<String>,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum GitCommand {
    #[command(about = "Show staged, unstaged, and untracked files")]
    Status,
    #[command(about = "Show recent commit history")]
    Log {
        #[arg(
            long,
            default_value_t = 10,
            help = "Maximum number of commits to return"
        )]
        limit: usize,
    },
    #[command(about = "Show the current diff for one path or the whole vault")]
    Diff {
        #[arg(help = "Optional vault-relative path to diff")]
        path: Option<String>,
    },
    #[command(about = "Stage changed vault files and create a commit")]
    Commit {
        #[arg(short = 'm', long, help = "Commit message to use")]
        message: String,
    },
    #[command(about = "Show per-line blame for one tracked file")]
    Blame {
        #[arg(help = "Vault-relative path to blame")]
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum WebCommand {
    #[command(about = "Query the configured web search backend")]
    Search {
        #[arg(help = "Search query to send to the backend")]
        query: String,
        #[arg(
            long,
            value_enum,
            help = "Override the configured search backend (kagi, exa, tavily, brave, auto)"
        )]
        backend: Option<SearchBackendArg>,
        #[arg(
            long,
            default_value_t = 10,
            help = "Maximum number of results to return"
        )]
        limit: usize,
    },
    #[command(about = "Fetch one URL as markdown, html, or raw content")]
    Fetch {
        #[arg(help = "URL to fetch")]
        url: String,
        #[arg(long, value_enum, default_value_t = WebFetchMode::Markdown)]
        mode: WebFetchMode,
        #[arg(long, help = "Write fetched output to this path")]
        save: Option<PathBuf>,
        #[arg(long, help = "Prefer article-like content when converting HTML")]
        extract_article: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum WebFetchMode {
    Markdown,
    Html,
    Raw,
}

/// CLI-level search backend selector (mirrors `SearchBackendKind` from config).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SearchBackendArg {
    /// Auto-detect: use the first backend whose API key env var is set.
    Auto,
    /// Kagi Search.
    Kagi,
    /// Exa (formerly Metaphor) neural search.
    Exa,
    /// Tavily Search.
    Tavily,
    /// Brave Search.
    Brave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DescribeFormatArg {
    JsonSchema,
    OpenaiTools,
    Mcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum QueryEngineArg {
    /// Auto-detect: DQL when input starts with TABLE/LIST/TASK/CALENDAR, native DSL otherwise
    Auto,
    /// Vulcan native query DSL (`from notes where …`)
    Dsl,
    /// Dataview Query Language (`TABLE … FROM … WHERE …`)
    Dql,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum QueryFormatArg {
    Table,
    Paths,
    Detail,
    Count,
    /// Tab-separated values (one row per note, easy for shell pipelines)
    Tsv,
    /// Comma-separated values (RFC 4180)
    Csv,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum PeriodicSubcommand {
    #[command(about = "List indexed periodic notes")]
    List {
        #[arg(long = "type", help = "Restrict results to one period type")]
        period_type: Option<String>,
    },
    #[command(about = "Show missing periodic notes in a date range")]
    Gaps {
        #[arg(long = "type", help = "Restrict gaps to one period type")]
        period_type: Option<String>,
        #[arg(long, help = "Start date for the gap window")]
        from: Option<String>,
        #[arg(long, help = "End date for the gap window")]
        to: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum DataviewCommand {
    #[command(about = "Evaluate Dataview inline expressions from one note")]
    Inline {
        #[arg(help = "Note path, filename, or alias containing inline expressions")]
        file: String,
    },
    #[command(about = "Evaluate a Dataview DQL query string")]
    Query {
        #[arg(help = "Quoted DQL query string")]
        dql: String,
    },
    #[command(about = "Evaluate a DataviewJS snippet directly")]
    QueryJs {
        #[arg(help = "Quoted DataviewJS snippet")]
        js: String,
        #[arg(
            long,
            help = "Optional current note path, filename, or alias for dv.current()/this"
        )]
        file: Option<String>,
    },
    #[command(about = "Evaluate indexed Dataview code blocks from one note")]
    Eval {
        #[arg(help = "Note path, filename, or alias containing Dataview blocks")]
        file: String,
        #[arg(long, help = "0-based Dataview block index to evaluate")]
        block: Option<usize>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum TasksCommand {
    #[command(about = "Create one TaskNotes task file")]
    Add {
        #[arg(help = "Task title or natural-language task input")]
        text: String,
        #[arg(
            long,
            help = "Skip natural-language parsing and use the raw text as the title"
        )]
        no_nlp: bool,
        #[arg(long, help = "Explicit task status")]
        status: Option<String>,
        #[arg(long, help = "Explicit task priority")]
        priority: Option<String>,
        #[arg(long, help = "Explicit due date or natural-language date phrase")]
        due: Option<String>,
        #[arg(long, help = "Explicit scheduled date or natural-language date phrase")]
        scheduled: Option<String>,
        #[arg(long = "context", help = "Context to add; repeat for multiple values")]
        contexts: Vec<String>,
        #[arg(
            long = "project",
            help = "Project note link or name; repeat for multiple values"
        )]
        projects: Vec<String>,
        #[arg(long = "tag", help = "Tag to add; repeat for multiple values")]
        tags: Vec<String>,
        #[arg(long, help = "Optional note template name")]
        template: Option<String>,
        #[arg(long, help = "Report the planned task file without writing it")]
        dry_run: bool,
        #[arg(long, help = "Skip auto-commit even when enabled in config")]
        no_commit: bool,
    },
    #[command(about = "Display one TaskNotes task with its structured properties")]
    Show {
        #[arg(help = "Task path, filename, alias, or title")]
        task: String,
    },
    #[command(about = "Open one TaskNotes task file in $EDITOR")]
    Edit {
        #[arg(help = "Task path, filename, alias, or title")]
        task: String,
        #[arg(long, help = "Skip auto-commit even when enabled in config")]
        no_commit: bool,
    },
    #[command(about = "Update one TaskNotes task property")]
    Set {
        #[arg(help = "Task path, filename, alias, or title")]
        task: String,
        #[arg(help = "Logical TaskNotes field name or raw frontmatter property")]
        property: String,
        #[arg(help = "New YAML value; use `null` to remove the property")]
        value: String,
        #[arg(long, help = "Report the planned change without writing the task file")]
        dry_run: bool,
        #[arg(long, help = "Skip auto-commit even when enabled in config")]
        no_commit: bool,
    },
    #[command(about = "Mark one task as completed")]
    Complete {
        #[arg(help = "Task path, filename, alias, or title")]
        task: String,
        #[arg(
            long,
            help = "Recurring instance date to complete (YYYY-MM-DD); defaults to the scheduled/due date or today"
        )]
        date: Option<String>,
        #[arg(long, help = "Report the planned change without writing the task file")]
        dry_run: bool,
        #[arg(long, help = "Skip auto-commit even when enabled in config")]
        no_commit: bool,
    },
    #[command(about = "Archive one completed TaskNotes task")]
    Archive {
        #[arg(help = "Task path, filename, alias, or title")]
        task: String,
        #[arg(long, help = "Report the planned change without writing the task file")]
        dry_run: bool,
        #[arg(long, help = "Skip auto-commit even when enabled in config")]
        no_commit: bool,
    },
    #[command(about = "Convert an existing note into a TaskNotes task")]
    Convert {
        #[arg(help = "Note path, filename, or alias to convert")]
        file: String,
        #[arg(
            long,
            help = "1-based source line to convert instead of converting the whole note"
        )]
        line: Option<i64>,
        #[arg(long, help = "Report the planned change without writing the task file")]
        dry_run: bool,
        #[arg(long, help = "Skip auto-commit even when enabled in config")]
        no_commit: bool,
    },
    #[command(about = "Append one inline task to a note")]
    Create {
        #[arg(help = "Task title or natural-language task input")]
        text: String,
        #[arg(
            long = "in",
            help = "Target note path, filename, or alias; defaults to the configured inbox note"
        )]
        note: Option<String>,
        #[arg(long, help = "Explicit due date or natural-language date phrase")]
        due: Option<String>,
        #[arg(long, help = "Explicit priority name")]
        priority: Option<String>,
        #[arg(long, help = "Report the planned change without writing the note")]
        dry_run: bool,
        #[arg(long, help = "Skip auto-commit even when enabled in config")]
        no_commit: bool,
    },
    #[command(about = "Change one task's due date")]
    Reschedule {
        #[arg(help = "Task path, filename, alias, title, or <note>:<line> for inline tasks")]
        task: String,
        #[arg(long, help = "New due date or natural-language date phrase")]
        due: String,
        #[arg(long, help = "Report the planned change without writing the task")]
        dry_run: bool,
        #[arg(long, help = "Skip auto-commit even when enabled in config")]
        no_commit: bool,
    },
    #[command(about = "Evaluate a Tasks plugin query string")]
    Query {
        #[arg(help = "Quoted Tasks query string")]
        query: String,
    },
    #[command(about = "Evaluate indexed Tasks code blocks from one note")]
    Eval {
        #[arg(help = "Note path, filename, or alias containing Tasks blocks")]
        file: String,
        #[arg(long, help = "0-based Tasks block index to evaluate")]
        block: Option<usize>,
    },
    #[command(about = "List indexed tasks with an optional filter")]
    List {
        #[arg(long, help = "Optional Tasks DSL query or Dataview expression filter")]
        filter: Option<String>,
        #[arg(
            long,
            value_enum,
            default_value_t = TasksListSourceArg::All,
            help = "Restrict results to TaskNotes file tasks, inline tasks, or both"
        )]
        source: TasksListSourceArg,
        #[arg(
            long,
            help = "Match a status symbol, name, type, or TaskNotes status string"
        )]
        status: Option<String>,
        #[arg(long, help = "Match one priority name")]
        priority: Option<String>,
        #[arg(long = "due-before", help = "Require due dates before this value")]
        due_before: Option<String>,
        #[arg(long = "due-after", help = "Require due dates after this value")]
        due_after: Option<String>,
        #[arg(long, help = "Require one matching TaskNotes project link")]
        project: Option<String>,
        #[arg(long, help = "Require one matching TaskNotes context value")]
        context: Option<String>,
        #[arg(long = "group-by", help = "Group the output by one task field")]
        group_by: Option<String>,
        #[arg(long = "sort-by", help = "Sort the output by one task field")]
        sort_by: Option<String>,
        #[arg(long, help = "Include archived TaskNotes in the result set")]
        include_archived: bool,
    },
    #[command(about = "Show upcoming recurring task instances")]
    Next {
        #[arg(help = "Maximum number of upcoming task instances to return")]
        count: usize,
        #[arg(
            long,
            help = "Reference date for recurrence expansion (defaults to today)"
        )]
        from: Option<String>,
    },
    #[command(about = "List currently blocked tasks with their blocking dependencies")]
    Blocked,
    #[command(about = "Show the task dependency graph")]
    Graph,
    #[command(about = "Manage TaskNotes time tracking sessions")]
    Track {
        #[command(subcommand)]
        command: TasksTrackCommand,
    },
    #[command(about = "Manage TaskNotes pomodoro sessions")]
    Pomodoro {
        #[command(subcommand)]
        command: TasksPomodoroCommand,
    },
    #[command(about = "List TaskNotes reminders due within a time window")]
    Reminders {
        #[arg(
            long,
            default_value = "1d",
            help = "Show reminders up to this duration ahead; overdue reminders are included"
        )]
        upcoming: String,
    },
    #[command(about = "List TaskNotes tasks due within a time window")]
    Due {
        #[arg(
            long,
            default_value = "7d",
            help = "Show due tasks up to this duration ahead; overdue tasks are included"
        )]
        within: String,
    },
    #[command(about = "Inspect TaskNotes Bases views")]
    View {
        #[command(subcommand)]
        command: TasksViewCommand,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum TasksTrackCommand {
    #[command(about = "Start time tracking for one TaskNotes task")]
    Start {
        #[arg(help = "Task path, filename, alias, or title")]
        task: String,
        #[arg(long, help = "Optional description for the started session")]
        description: Option<String>,
        #[arg(long, help = "Report the planned change without writing the task file")]
        dry_run: bool,
        #[arg(long, help = "Skip auto-commit even when enabled in config")]
        no_commit: bool,
    },
    #[command(about = "Stop the active time tracking session")]
    Stop {
        #[arg(help = "Optional task path, filename, alias, or title")]
        task: Option<String>,
        #[arg(long, help = "Report the planned change without writing the task file")]
        dry_run: bool,
        #[arg(long, help = "Skip auto-commit even when enabled in config")]
        no_commit: bool,
    },
    #[command(about = "Show the currently active time tracking session")]
    Status,
    #[command(about = "Show time entries for one TaskNotes task")]
    Log {
        #[arg(help = "Task path, filename, alias, or title")]
        task: String,
    },
    #[command(about = "Summarize tracked time across TaskNotes tasks")]
    Summary {
        #[arg(
            long,
            value_enum,
            default_value_t = TasksTrackSummaryPeriodArg::Week,
            help = "Summary window"
        )]
        period: TasksTrackSummaryPeriodArg,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum TasksPomodoroCommand {
    #[command(about = "Start a pomodoro work session for one TaskNotes task")]
    Start {
        #[arg(help = "Task path, filename, alias, or title")]
        task: String,
        #[arg(
            long,
            help = "Report the planned change without writing session history"
        )]
        dry_run: bool,
        #[arg(long, help = "Skip auto-commit even when enabled in config")]
        no_commit: bool,
    },
    #[command(about = "Stop the active pomodoro session")]
    Stop {
        #[arg(help = "Optional task path, filename, alias, or title")]
        task: Option<String>,
        #[arg(
            long,
            help = "Report the planned change without writing session history"
        )]
        dry_run: bool,
        #[arg(long, help = "Skip auto-commit even when enabled in config")]
        no_commit: bool,
    },
    #[command(about = "Show the currently active pomodoro session")]
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum TasksViewCommand {
    #[command(about = "Evaluate one TaskNotes Bases view or .base file")]
    Show {
        #[arg(help = "View name, file stem, or vault-relative .base path")]
        name: String,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "List available TaskNotes Bases views")]
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TasksTrackSummaryPeriodArg {
    Day,
    Week,
    Month,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TasksListSourceArg {
    File,
    Inline,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum KanbanCommand {
    #[command(about = "List indexed Kanban boards")]
    List,
    #[command(about = "Display one Kanban board by column")]
    Show {
        #[arg(help = "Board path, filename, or alias")]
        board: String,
        #[arg(long, help = "Include card details in the output")]
        verbose: bool,
        #[arg(long, help = "Include archived cards in the output")]
        include_archive: bool,
    },
    #[command(about = "List cards from one Kanban board")]
    Cards {
        #[arg(help = "Board path, filename, or alias")]
        board: String,
        #[arg(long, help = "Restrict cards to one column title")]
        column: Option<String>,
        #[arg(
            long,
            help = "Restrict cards to one task status character, name, or type"
        )]
        status: Option<String>,
    },
    #[command(about = "Move one Kanban card into the archive column")]
    Archive {
        #[arg(help = "Board path, filename, or alias")]
        board: String,
        #[arg(help = "Card id, block id, line number, or exact card text")]
        card: String,
        #[arg(long, help = "Preview the archive operation without writing the board")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Move one Kanban card between active columns")]
    Move {
        #[arg(help = "Board path, filename, or alias")]
        board: String,
        #[arg(help = "Card id, block id, line number, or exact card text")]
        card: String,
        #[arg(help = "Destination column title")]
        target_column: String,
        #[arg(long, help = "Preview the move without writing the board")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Add one Kanban card to an active column")]
    Add {
        #[arg(help = "Board path, filename, or alias")]
        board: String,
        #[arg(help = "Destination column title")]
        column: String,
        #[arg(help = "Card title text to add")]
        text: String,
        #[arg(long, help = "Preview the add without writing the board")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum RefactorCommand {
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
        about = "Suggest link and merge opportunities from indexed notes",
        hide = true
    )]
    Suggest {
        #[command(subcommand)]
        command: SuggestCommand,
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
        #[command(flatten)]
        render: TemplateRenderArgs,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Render a template without creating or editing files")]
    Preview {
        #[arg(help = "Template name or filename stem")]
        template: String,
        #[arg(
            long,
            help = "Target note path used for title/path/frontmatter context"
        )]
        path: Option<String>,
        #[command(flatten)]
        render: TemplateRenderArgs,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum NoteCommand {
    #[command(
        about = "Read one note, optionally narrowed by selectors",
        after_help = NOTE_GET_COMMAND_AFTER_HELP
    )]
    Get {
        #[arg(help = "Note path, filename, or alias to read")]
        note: String,
        #[arg(long, help = "Extract one heading section by exact heading text")]
        heading: Option<String>,
        #[arg(long = "block-ref", help = "Extract one block by block reference id")]
        block_ref: Option<String>,
        #[arg(long, help = "Select a 1-based line range such as 1-10, 50-, or -5")]
        lines: Option<String>,
        #[arg(long = "match", help = "Filter selected lines with a regex")]
        match_pattern: Option<String>,
        #[arg(long, default_value_t = 0, help = "Context lines around each match")]
        context: usize,
        #[arg(long, help = "Strip a leading YAML frontmatter block from the output")]
        no_frontmatter: bool,
        #[arg(long, help = "Print only the selected content without line numbers")]
        raw: bool,
    },
    #[command(about = "Replace one note's contents from stdin or a file")]
    Set {
        #[arg(help = "Note path, filename, or alias to overwrite")]
        note: String,
        #[arg(long, help = "Read replacement content from a file instead of stdin")]
        file: Option<PathBuf>,
        #[arg(
            long,
            help = "Preserve the existing YAML frontmatter and only replace the body"
        )]
        no_frontmatter: bool,
        #[arg(
            long,
            help = "Run non-blocking doctor-like diagnostics after the write"
        )]
        check: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Create a new note from optional stdin content and template context")]
    Create {
        #[arg(help = "New relative note path to create")]
        path: String,
        #[arg(long, help = "Render a template before creating the note")]
        template: Option<String>,
        #[arg(
            long = "frontmatter",
            action = ArgAction::Append,
            help = "Add or override a top-level frontmatter key using key=value syntax"
        )]
        frontmatter: Vec<String>,
        #[arg(long, help = "Run non-blocking doctor-like diagnostics after creation")]
        check: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Append text to a note or under a heading")]
    Append {
        #[arg(
            allow_hyphen_values = true,
            value_name = "NOTE_OR_TEXT",
            help = "Note path, filename, or alias to update; or the text itself when --periodic is set"
        )]
        note_or_text: String,
        #[arg(
            allow_hyphen_values = true,
            value_name = "TEXT",
            help = "Text to append, or `-` to read from stdin"
        )]
        text: Option<String>,
        #[arg(
            long = "after-heading",
            visible_alias = "heading",
            conflicts_with_all = ["prepend", "append"],
            help = "Append under this exact heading, creating it if needed"
        )]
        heading: Option<String>,
        #[arg(
            long,
            conflicts_with_all = ["heading", "append"],
            help = "Insert after frontmatter instead of appending at the end"
        )]
        prepend: bool,
        #[arg(
            long,
            conflicts_with_all = ["heading", "prepend"],
            help = "Append to the end of the note (default)"
        )]
        append: bool,
        #[arg(
            long,
            value_enum,
            help = "Target a periodic note type instead of a named note"
        )]
        periodic: Option<NoteAppendPeriodicArg>,
        #[arg(long, requires = "periodic", help = "Reference date for --periodic")]
        date: Option<String>,
        #[arg(
            long = "var",
            action = ArgAction::Append,
            help = "Bind QuickAdd-style prompt variables using key=value syntax"
        )]
        vars: Vec<String>,
        #[arg(long, help = "Run non-blocking doctor-like diagnostics after append")]
        check: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Find and replace inside a single note with guarded match counts")]
    Patch {
        #[arg(help = "Note path, filename, or alias to update")]
        note: String,
        #[arg(long, help = "Literal text or /regex/ pattern to find")]
        find: String,
        #[arg(long, help = "Replacement text")]
        replace: String,
        #[arg(long, help = "Allow replacing more than one match")]
        all: bool,
        #[arg(long, help = "Run non-blocking doctor-like diagnostics after patching")]
        check: bool,
        #[arg(long, help = "Report the planned patch without writing the file")]
        dry_run: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "Show summary metadata and graph stats for one note")]
    Info {
        #[arg(help = "Note path, filename, or alias to inspect")]
        note: String,
    },
    #[command(about = "Show git history scoped to one note")]
    History {
        #[arg(help = "Note path, filename, or alias to inspect")]
        note: String,
        #[arg(
            long,
            default_value_t = 10,
            help = "Maximum number of commits to return"
        )]
        limit: usize,
    },
    #[command(about = "List outgoing links for one note")]
    Links {
        #[arg(
            help = "Note path, filename, or alias to inspect; omit in a TTY session to pick interactively"
        )]
        note: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "List inbound links pointing at one note")]
    Backlinks {
        #[arg(
            help = "Note path, filename, or alias to inspect; omit in a TTY session to pick interactively"
        )]
        note: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Run doctor-style diagnostics against one note")]
    Doctor {
        #[arg(help = "Note path, filename, or alias to inspect")]
        note: String,
    },
    #[command(about = "Show one note's changes since git HEAD, the last scan, or a checkpoint")]
    Diff {
        #[arg(help = "Note path, filename, or alias to inspect")]
        note: String,
        #[arg(long, help = "Named checkpoint to compare against instead of git HEAD")]
        since: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TemplateEngineArg {
    Native,
    Templater,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NoteAppendPeriodicArg {
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Clone, PartialEq, Eq, Args)]
pub struct TemplateRenderArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = TemplateEngineArg::Auto,
        help = "Template engine to use: native, templater, or auto-detect"
    )]
    pub engine: TemplateEngineArg,
    #[arg(
        long = "var",
        action = ArgAction::Append,
        help = "Bind a template variable using key=value syntax"
    )]
    pub vars: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum Command {
    #[command(about = "Initialize, scan, rebuild, repair, watch, and serve index state")]
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    #[command(about = "Initialize .vulcan/ state for a vault", hide = true)]
    Init(InitArgs),
    #[command(about = "Rebuild the cache from disk", hide = true)]
    Rebuild {
        #[arg(long, help = "Report rebuild scope without mutating the cache")]
        dry_run: bool,
    },
    #[command(about = "Repair derived indexes and cache structures", hide = true)]
    Repair {
        #[command(subcommand)]
        command: RepairCommand,
    },
    #[command(
        about = "Watch the vault for filesystem changes and keep the cache fresh",
        hide = true
    )]
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
    #[command(
        about = "Serve local cache-backed HTTP APIs for repeated queries",
        hide = true
    )]
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
    #[command(about = "Scan the vault and update the cache", hide = true)]
    Scan {
        #[arg(long, help = "Force a full scan instead of incremental reconciliation")]
        full: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(about = "List outgoing links for a note", hide = true)]
    Links {
        #[arg(
            help = "Note path, filename, or alias to inspect; omit in a TTY session to pick interactively"
        )]
        note: Option<String>,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "List inbound links pointing at a note", hide = true)]
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
            required_unless_present = "regex",
            help = "Full-text query string; supports phrases, `or`, `-term`, and inline tag:/path:/has: filters"
        )]
        query: Option<String>,
        #[arg(
            long,
            conflicts_with = "query",
            help = "Run an explicit regex search without /pattern/ delimiters"
        )]
        regex: Option<String>,
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
        #[arg(
            long,
            help = "Exit with code 1 when zero results are returned (useful in shell conditionals)"
        )]
        exit_code: bool,
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
        about = "Evaluate Dataview-compatible metadata and inline expressions",
        after_help = DATAVIEW_COMMAND_AFTER_HELP
    )]
    Dataview {
        #[command(subcommand)]
        command: DataviewCommand,
    },
    #[command(
        about = "Evaluate and list Tasks plugin queries against indexed tasks",
        after_help = TASKS_COMMAND_AFTER_HELP
    )]
    Tasks {
        #[command(subcommand)]
        command: TasksCommand,
    },
    #[command(
        about = "Inspect indexed Kanban boards and cards",
        after_help = KANBAN_COMMAND_AFTER_HELP
    )]
    Kanban {
        #[command(subcommand)]
        command: KanbanCommand,
    },
    #[command(
        about = "Evaluate and maintain Bases views",
        after_help = BASES_COMMAND_AFTER_HELP
    )]
    Bases {
        #[command(subcommand)]
        command: BasesCommand,
    },
    #[command(
        about = "Suggest link and merge opportunities from indexed notes",
        hide = true
    )]
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
    #[command(
        about = "Import compatible plugin settings into .vulcan/config.toml",
        after_help = CONFIG_COMMAND_AFTER_HELP
    )]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    #[command(
        about = "Open, inspect, and append to daily notes",
        after_help = DAILY_COMMAND_AFTER_HELP
    )]
    Daily {
        #[command(subcommand)]
        command: DailyCommand,
    },
    #[command(
        about = "Open or create today's daily note",
        after_help = TODAY_COMMAND_AFTER_HELP
    )]
    Today {
        #[arg(long, help = "Create the note without opening it in the editor")]
        no_edit: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
    },
    #[command(
        about = "Inspect and mutate the vault git repository",
        after_help = GIT_COMMAND_AFTER_HELP
    )]
    Git {
        #[command(subcommand)]
        command: GitCommand,
    },
    #[command(
        about = "Execute JavaScript inside the Vulcan runtime sandbox",
        after_help = RUN_COMMAND_AFTER_HELP
    )]
    Run {
        #[arg(help = "Script path or named script from .vulcan/scripts")]
        script: Option<String>,
        #[arg(
            long = "script",
            help = "Treat the positional argument as a script file path for shebang use"
        )]
        script_mode: bool,
        #[arg(
            long,
            short = 'e',
            value_name = "CODE",
            action = clap::ArgAction::Append,
            help = "Evaluate a JS expression and print the result (may be repeated)"
        )]
        eval: Vec<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Load and evaluate a JS file, then drop into the REPL"
        )]
        eval_file: Option<String>,
        #[arg(
            long,
            value_name = "DURATION",
            help = "Override the JS execution timeout (for example 500ms, 30s, or 2m)"
        )]
        timeout: Option<String>,
        #[arg(
            long,
            value_name = "LEVEL",
            value_parser = ["strict", "fs", "net", "none"],
            help = "Select the JS sandbox level"
        )]
        sandbox: Option<String>,
        #[arg(
            long,
            help = "Skip auto-loading .vulcan/scripts/startup.js even in trusted vaults"
        )]
        no_startup: bool,
    },
    #[command(
        about = "Fetch and search external web content",
        after_help = WEB_COMMAND_AFTER_HELP
    )]
    Web {
        #[command(subcommand)]
        command: WebCommand,
    },
    #[command(
        about = "Open or create the weekly note for a date",
        after_help = PERIODIC_COMMAND_AFTER_HELP
    )]
    Weekly {
        #[command(flatten)]
        args: PeriodicOpenArgs,
    },
    #[command(
        about = "Open or create the monthly note for a date",
        after_help = PERIODIC_COMMAND_AFTER_HELP
    )]
    Monthly {
        #[command(flatten)]
        args: PeriodicOpenArgs,
    },
    #[command(
        about = "Open, list, and inspect periodic notes",
        after_help = PERIODIC_COMMAND_AFTER_HELP
    )]
    Periodic {
        #[command(subcommand)]
        command: Option<PeriodicSubcommand>,
        #[arg(help = "Configured period type to open when no subcommand is used")]
        period_type: Option<String>,
        #[arg(help = "Reference date for the period (defaults to today)")]
        date: Option<String>,
        #[arg(long, help = "Create the note without opening it in the editor")]
        no_edit: bool,
        #[arg(long, help = "Suppress auto-commit for this invocation")]
        no_commit: bool,
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
        #[command(flatten)]
        render: TemplateRenderArgs,
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
        about = "Read and mutate one note with selector-aware CRUD commands",
        after_help = NOTE_COMMAND_AFTER_HELP
    )]
    Note {
        #[command(subcommand)]
        command: NoteCommand,
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
    #[command(
        about = "Move a note or attachment and safely rewrite inbound links",
        hide = true
    )]
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
    #[command(
        about = "Convert unambiguous plain-text note mentions into links",
        hide = true
    )]
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
        after_help = REWRITE_COMMAND_AFTER_HELP,
        hide = true
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
    #[command(about = "Rename a frontmatter property key across notes", hide = true)]
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
    #[command(
        about = "Merge one tag into another across frontmatter and note bodies",
        hide = true
    )]
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
    #[command(about = "Rename an alias inside one note's frontmatter", hide = true)]
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
    #[command(
        about = "Rename a heading and rewrite inbound heading links",
        hide = true
    )]
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
    #[command(
        about = "Rename a block reference and rewrite inbound block links",
        hide = true
    )]
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
        after_help = QUERY_COMMAND_AFTER_HELP
    )]
    Query {
        #[arg(help = "Query string (DSL or DQL depending on --engine)")]
        dsl: Option<String>,
        #[arg(
            long,
            help = "JSON query payload; mutually exclusive with the positional DSL argument"
        )]
        json: Option<String>,
        #[arg(
            long,
            value_enum,
            default_value_t = QueryEngineArg::Auto,
            help = "Query engine: auto-detect (default), native dsl, or dql (Dataview)"
        )]
        engine: QueryEngineArg,
        #[arg(long, value_enum, default_value_t = QueryFormatArg::Table)]
        format: QueryFormatArg,
        #[arg(long, help = "Restrict result paths with a glob such as Projects/**")]
        glob: Option<String>,
        #[arg(long, help = "Print the parsed query AST alongside the results")]
        explain: bool,
        #[arg(
            long,
            help = "Exit with code 1 when zero results are returned (useful in shell conditionals)"
        )]
        exit_code: bool,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(
        about = "List note paths with query-style filters",
        after_help = "\
Thin alias for `vulcan query 'from notes' --format paths`.

Examples:
  vulcan ls
  vulcan ls --glob 'Projects/**'
  vulcan ls --where 'status = done'
  vulcan ls --tag project --format detail"
    )]
    Ls {
        #[arg(
            long = "where",
            help = "Typed property filter; repeatable and combined with AND"
        )]
        filters: Vec<String>,
        #[arg(long, help = "Restrict result paths with a glob such as Projects/**")]
        glob: Option<String>,
        #[arg(long, help = "Shorthand tag filter")]
        tag: Option<String>,
        #[arg(long, value_enum, default_value_t = QueryFormatArg::Paths)]
        format: QueryFormatArg,
        #[command(flatten)]
        export: ExportArgs,
    },
    #[command(about = "Apply vault-wide refactors and suggestion passes")]
    Refactor {
        #[command(subcommand)]
        command: RefactorCommand,
    },
    #[command(
        about = "Show integrated command and concept documentation",
        after_help = HELP_COMMAND_AFTER_HELP
    )]
    Help {
        #[arg(long, help = "Search help topics and command docs by keyword")]
        search: Option<String>,
        #[arg(help = "Optional topic such as `query` or `note get`")]
        topic: Vec<String>,
    },
    #[command(
        about = "Describe the CLI schema and command surface",
        after_help = DESCRIBE_COMMAND_AFTER_HELP
    )]
    Describe {
        #[arg(long, value_enum, default_value_t = DescribeFormatArg::JsonSchema)]
        format: DescribeFormatArg,
    },
    #[command(
        about = "Generate shell completion scripts",
        after_help = COMPLETIONS_COMMAND_AFTER_HELP
    )]
    Completions {
        #[arg(help = "Shell to generate completions for")]
        shell: Shell,
    },
    #[command(about = "Manage vault trust for startup scripts and plugin execution")]
    Trust {
        #[command(subcommand)]
        command: Option<TrustCommand>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum TrustCommand {
    #[command(about = "Mark the current vault as trusted")]
    Add,
    #[command(about = "Remove trust from the current vault")]
    Revoke,
    #[command(about = "List all trusted vault paths")]
    List,
}

#[derive(Debug, Clone, Parser)]
#[command(
    author,
    version,
    about = "Headless CLI for Obsidian-style vaults and Markdown directories",
    long_about = None,
    after_help = ROOT_AFTER_HELP,
    disable_help_subcommand = true
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

    #[arg(
        long,
        short = 'q',
        global = true,
        action = ArgAction::SetTrue,
        env = "VULCAN_QUIET",
        help = "Suppress scan progress, warnings, and non-essential stderr output"
    )]
    pub quiet: bool,

    #[arg(
        long,
        global = true,
        action = ArgAction::SetTrue,
        help = "Suppress column headers in table/TSV output (useful for piping)"
    )]
    pub no_header: bool,

    #[command(subcommand)]
    pub command: Command,
}
