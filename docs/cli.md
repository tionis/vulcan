# CLI Guide

This guide documents the current user-facing Vulcan CLI. It matches the implemented command surface in this repository. For architecture and design decisions, see `docs/design_document.md`.

## Build and first scan

Build the release binary from the repo root:

```bash
cargo build --release -p vulcan-cli --bin vulcan
```

The binary is written to:

```text
./target/release/vulcan
```

Typical first run against a vault:

```bash
./target/release/vulcan --vault ~/wikis/mimir init
./target/release/vulcan --vault ~/wikis/mimir scan
./target/release/vulcan --vault ~/wikis/mimir notes --limit 5
./target/release/vulcan --vault ~/wikis/mimir browse
```

## Self-discovery

The CLI is designed to be self-describing at runtime.

- `vulcan --help` shows the top-level command groups.
- `vulcan <command> --help` shows command-specific syntax, caveats, and examples.
- `vulcan describe` prints the runtime command schema as JSON.
- `vulcan --output json describe` prints the same schema in compact machine-oriented JSON.
- `vulcan completions <shell>` generates shell completions.

Useful starting points:

- `vulcan notes --help`
- `vulcan search --help`
- `vulcan edit --help`
- `vulcan browse --help`
- `vulcan query --help`

## Global options and output behavior

These global flags are available on all commands:

- `--vault <PATH>`: vault root directory
- `--output <human|json>`: output format
- `--refresh <off|blocking|background>`: override automatic cache refresh for cache-backed commands
- `--fields <a,b,c>`: select columns on list-style output
- `--provider <NAME>`: override the embedding provider for vector commands
- `--limit <N>`: cap returned rows
- `--offset <N>`: paginate list output
- `--verbose`: enable extra diagnostic output

Output rules:

- `--output json` is the stable scripting and agent surface.
- Row-oriented commands emit line-delimited JSON.
- Single-report commands emit one JSON object.
- Many query commands also support `--export <csv|jsonl> --export-path <FILE>`.
- In non-interactive mode, Vulcan does not open fuzzy pickers or TTY prompts.

## Configuration layering and automatic refresh

Vulcan uses two vault-local config files:

- `.vulcan/config.toml`: shared vault config that is intended to be synced with the vault
- `.vulcan/config.local.toml`: optional device-local override loaded after `config.toml`

Precedence is:

1. `.vulcan/config.local.toml`
2. `.vulcan/config.toml`
3. `.obsidian/app.json`
4. Built-in defaults

`vulcan init` creates `.vulcan/config.toml`, `cache.db`, and a default `.vulcan/.gitignore` that keeps `config.toml` tracked while ignoring `config.local.toml`.

Automatic cache refresh is configured under `[scan]`:

```toml
[scan]
default_mode = "blocking"
browse_mode = "background"
```

Meaning:

- `default_mode` controls one-shot cache-backed commands such as `backlinks`, `links`, `notes`, `search`, `graph`, `diff`, `query`, and note-refactoring commands that depend on current cache state.
- `browse_mode` controls `vulcan browse`.
- `off` uses the current cache as-is.
- `blocking` runs an incremental scan before the command continues.
- `background` is intended for long-lived interactive surfaces; one-shot commands treat it the same as `blocking`.

Use `--refresh` to override the configured behavior for one invocation:

```bash
vulcan --refresh off backlinks Projects/Alpha
vulcan --refresh background browse
```

Note resolution rules:

- Commands that take a note reference accept a vault-relative path, filename, or alias.
- Some single-note commands allow omitting the note in a TTY and will open the built-in picker instead.
- When `--output json` is active, or when stdin/stdout is not interactive, Vulcan will not auto-prompt.

## Command catalogue

### Indexing, cache, and local service commands

- `vulcan init`: create `.vulcan/`, `cache.db`, `config.toml`, and the local ignore rules.
- `vulcan scan [--full] [--no-commit]`: perform an incremental or full scan and refresh the cache.
- `vulcan rebuild [--dry-run]`: rebuild the cache from disk.
- `vulcan repair fts [--dry-run]`: rebuild the full-text search index from cached chunks.
- `vulcan watch [--debounce-ms <MS>] [--no-commit]`: keep the cache fresh from filesystem events.
- `vulcan serve [--bind <ADDR>] [--no-watch] [--debounce-ms <MS>] [--auth-token <TOKEN>]`: start the local HTTP API server backed by the cache.
- `vulcan cache inspect`: show cache sizes and row counts.
- `vulcan cache verify [--fail-on-errors]`: verify cache invariants.
- `vulcan cache vacuum [--dry-run]`: run SQLite `VACUUM` on the cache.
- `vulcan checkpoint create <name>`: capture the current cache state under a checkpoint name.
- `vulcan checkpoint list`: list saved scan and manual checkpoints.
- `vulcan export search-index [--path <FILE>] [--pretty]`: write the cached search corpus as a static JSON index.
- `vulcan changes [--checkpoint <name>]`: report note, link, property, and embedding changes since the last scan or a named checkpoint.
- `vulcan diff [note] [--since <checkpoint>]`: show one note's changes since git `HEAD`, the last scan, or a named checkpoint.

### Config import commands

- `vulcan config import core [--dry-run] [--target <shared|local>] [--no-commit]`: import Obsidian core settings from `.obsidian/app.json`, `.obsidian/templates.json`, and `.obsidian/types.json`.
- `vulcan config import kanban [--dry-run] [--target <shared|local>] [--no-commit]`: import Obsidian Kanban plugin settings.
- `vulcan config import tasks [--dry-run] [--target <shared|local>] [--no-commit]`: import Obsidian Tasks plugin settings.
- `vulcan config import templater [--dry-run] [--target <shared|local>] [--no-commit]`: import Obsidian Templater plugin settings.

Shared behavior:

- `--dry-run` prints the mapping and target file without writing either `.vulcan/config.toml` or `.vulcan/config.local.toml`.
- `--target local` writes to `.vulcan/config.local.toml`; the default target is the shared `.vulcan/config.toml`.
- `--output json` returns the full import report, including target file, whether the run was a dry run, the mappings applied, and any detected conflicts.
- When vault auto-commit is enabled for mutations, config imports participate unless `--no-commit` is passed.

### Query, graph, and reporting commands

- `vulcan links [note]`: list outgoing links for a note.
- `vulcan backlinks [note]`: list inbound links for a note.
- `vulcan graph path [from] [to]`: shortest resolved-link path between two notes.
- `vulcan graph hubs`: notes with the highest degree.
- `vulcan graph moc`: candidate map-of-content notes.
- `vulcan graph dead-ends`: notes without outbound resolved note links.
- `vulcan graph components`: weakly connected components.
- `vulcan graph stats`: note-graph and vault analytics summary.
- `vulcan graph trends [--limit <N>]`: trends across saved scan checkpoints.
- `vulcan notes ...`: property and file-metadata queries.
- `vulcan search ...`: full-text search with optional typed property filters and explicit result sorting.
- `vulcan query ...`: run the human DSL or JSON query payload.
- `vulcan suggest mentions [note]`: plain-text mentions that could become links.
- `vulcan suggest duplicates`: duplicate titles, alias collisions, and merge candidates.
- `vulcan saved list`: list saved query/report definitions from `.vulcan/reports`.
- `vulcan saved show <name>`: show one saved report definition.
- `vulcan saved search ...`: save a search definition.
- `vulcan saved notes ...`: save a property query definition.
- `vulcan saved bases <name> <file.base>`: save a `.base` evaluation definition.
- `vulcan saved run <name>`: execute one saved report.
- `vulcan batch [<name> ...] [--all]`: run several saved reports at once.
- `vulcan automation run ...`: run saved reports plus optional scan, doctor, verify, or FTS repair steps for CI and scripts.

### Bases commands

`vulcan bases` now covers both read and write workflows:

- `vulcan bases eval <file.base>`: evaluate a `.base` file against the indexed vault state.
- `vulcan bases create <file.base> [--title <title>] [--dry-run] [--no-commit]`: create a note matching the first view context.
- `vulcan bases tui <file.base>`: open the interactive Bases TUI.
- `vulcan bases view-add <file.base> <name> ...`: add a view definition.
- `vulcan bases view-delete <file.base> <name> [--dry-run] [--no-commit]`: delete a view.
- `vulcan bases view-rename <file.base> <old> <new> [--dry-run] [--no-commit]`: rename a view.
- `vulcan bases view-edit <file.base> <name> ...`: mutate filters, columns, sort, and grouping on an existing view.

Important behavior:

- View mutations operate on the parsed `.base` model and write it back through the serializer.
- Unsupported Bases constructs surface as diagnostics instead of being silently ignored.
- `bases create` derives the note folder from `file.folder = ...` and `file.inFolder(...)` filters, and pre-populates equality filters like `status = todo` into frontmatter.
- `view-*` commands support `--dry-run` where the result can be previewed, and `--no-commit` to suppress auto-commit for that invocation.

### Semantic and vector commands

- `vulcan vectors index [--dry-run]`: embed pending chunks.
- `vulcan vectors repair [--dry-run]`: repair stale or missing vector rows.
- `vulcan vectors rebuild [--dry-run]`: rebuild the vector index from scratch.
- `vulcan vectors queue status`: inspect pending vector indexing work.
- `vulcan vectors queue run [--dry-run]`: execute the explicit vector queue.
- `vulcan vectors neighbors [query] [--note <NOTE>]`: nearest indexed chunks for ad hoc text or an existing note.
- `vulcan vectors related [note]`: semantically related notes for one seed note.
- `vulcan vectors duplicates [--threshold <F32>]`: highly similar chunk pairs.
- `vulcan vectors models`: list stored embedding models.
- `vulcan vectors drop-model <cache-key>`: drop one model and its vectors.
- `vulcan cluster [--clusters <N>] [--dry-run]`: group indexed vectors into topical clusters.
- `vulcan related [note]`: top-level shortcut for note-to-note semantic recommendations.

## Query and filter syntax

`vulcan notes`, `vulcan search --where`, `vulcan rewrite`, `vulcan update`, `vulcan unset`, and `bases view-*` filters all use the same typed filter grammar.

Each filter uses this form:

```text
<field> <operator> <value>
```

Repeat `--where` to combine filters with logical `AND`.

Current limitation:

- `--where` does not support `OR`, parentheses, or nested expressions.

Supported fields:

- Any property key, for example `status`, `due`, `reviewed`, `tags`
- `file.path`
- `file.name`
- `file.ext`
- `file.mtime`

Supported operators:

- `=`
- `>`
- `>=`
- `<`
- `<=`
- `starts_with`
- `contains`

Operator notes:

- `starts_with` is for text fields.
- `contains` is for list-valued properties such as `tags`.

Supported value types:

- Text: `done`, `"In Progress"`, `'Rule Index'`
- Boolean: `true`, `false`
- Null: `null`
- Number: `42`, `3.5`
- Date or datetime: `2026-03-01`, `2026-03-01T09:30:00Z`
- `file.mtime`: integer milliseconds since the Unix epoch

Examples:

```bash
vulcan notes --where 'status = done'
vulcan notes --where 'tags contains sprint' --sort due
vulcan notes --where 'file.path starts_with "Projects/"'
vulcan update --where 'status = draft' --key status --value done --dry-run
vulcan rewrite --where 'file.path starts_with "Archive/"' --find TODO --replace DONE
vulcan bases view-add release.base Inbox --filter 'status = idea'
```

## Searching with `search`

`vulcan search` searches indexed note content and can be combined with typed filters.

Examples:

```bash
vulcan search dashboard
vulcan search 'dashboard "release notes" -draft'
vulcan search dashboard --where 'reviewed = true'
vulcan search 'tag:index path:People/ has:status owned'
vulcan search Bob --match-case
vulcan search dashboard --sort path-desc
vulcan search dashboard --sort modified-newest
```

Default query syntax:

- Plain terms are combined with logical `AND`.
- Parentheses group boolean expressions: `(dashboard or summary) release`.
- Quoted phrases stay together: `"owned by"`.
- Use `or` between positive terms: `dashboard or summary`.
- Prefix a term, quoted phrase, or parenthesized group with `-` to exclude it.
- Scoped operators can require terms to co-occur in one line, block, or heading section: `line:(mix flour)`, `block:(release notes)`, `section:(dog cat)`.

Supported inline filters inside the query text:

- `tag:<tag>`
- `path:<prefix>`
- `has:<property>`
- `property:<property>`
- `[property]`: require a frontmatter property to exist, for example `[aliases]`
- `[property:value]`: require a property to equal a value, for example `[status:done]`
- `[property:value OR other]`: require one of several values, for example `[status:Draft OR Published]`
- `/pattern/`: match note content with a Rust `regex` pattern
- `path:/pattern/`: match note paths with a Rust `regex` pattern
- `file:<filename-fragment>`: match the filename only, not the full path
- `content:<term>`: restrict the term to note body content, excluding title, aliases, and headings
- `match-case:<term>`: require an exact-case match after the normal FTS candidate search
- `ignore-case:<term>`: override a global case-sensitive search for one term
- `task:<term>`: require the term to appear on a Markdown task line
- `task-todo:<term>`: require the term to appear on an incomplete task (`- [ ]`)
- `task-done:<term>`: require the term to appear on a completed task (`- [x]`)

Useful search flags:

- `--where <FILTER>`: typed property filters using the grammar above
- `--tag <TAG>`: require a tag
- `--path-prefix <PREFIX>`: require a path prefix
- `--has-property <KEY>`: require the property to exist
- `--mode <keyword|hybrid>`: select keyword-only or keyword+vector search
- `--match-case`: require case-sensitive matching by default for unscoped terms
- `--sort <relevance|path-asc|path-desc|modified-newest|modified-oldest|created-newest|created-oldest>`: override result ordering
- `--context-size <N>`: snippet context size
- `--raw-query`: pass SQLite FTS5 syntax through unchanged
- `--fuzzy`: retry empty searches with typo-tolerant expansion
- `--explain`: include the parsed boolean tree, active filters, and scoring details

Additional explain behavior:

- When `--explain` finds no hits, Vulcan includes query suggestions such as likely operator typos and task-specific hints.
- JSON search output includes `matched_line` when Vulcan can identify the best line within the matched chunk.

Raw FTS5 example:

```bash
vulcan search --raw-query '"release" NEAR/5 "notes"'
```

**Planned enhancements (Roadmap 9.6):** Browse-TUI search controls will continue to grow with inline explanation panes and more live search toggles. See `docs/ROADMAP.md` §9.6.

## Query DSL and JSON payloads

`vulcan query` exposes the shared query layer directly.

DSL shape:

```text
from notes
  [where <field> <op> <value> [and <field> <op> <value>...]]
  [select <field>[,<field>...]]
  [order by <field> [desc|asc]]
  [limit <n>]
  [offset <n>]
```

JSON payload example:

```json
{"source":"notes","predicates":[{"field":"status","operator":"eq","value":"done"}],"sort":{"field":"file.mtime","descending":true},"limit":10}
```

Examples:

```bash
vulcan query 'from notes where status = done order by file.mtime desc limit 10'
vulcan query 'from notes where tags contains sprint and reviewed = true'
vulcan query --json '{"source":"notes","predicates":[{"field":"status","operator":"eq","value":"done"}]}'
vulcan query --explain 'from notes where status = backlog'
```

Use `vulcan describe` when you need the runtime schema for the JSON form rather than prose.

## Interactive workflows

### Picker-backed note selection

These commands can resolve a note interactively when you omit the note in a TTY session:

- `links`
- `backlinks`
- `graph path`
- `related`
- `vectors related`
- `diff`
- `edit`
- `open`

In non-interactive mode, or with JSON output redirected, Vulcan does not auto-prompt.

### `edit`

`vulcan edit` opens a note in `$VISUAL`, then `$EDITOR`, then `vi`.

Examples:

```bash
vulcan edit Projects/Alpha
vulcan edit
vulcan edit --new Inbox/Idea
```

Behavior:

- `--new` creates the path first, appending `.md` if needed.
- After the editor exits, Vulcan runs an incremental scan of the edited path.
- If auto-commit is enabled, the edit is committed unless `--no-commit` is passed.

### `browse`

`vulcan browse` is the persistent note browser TUI.

Modes:

- Default: fuzzy note picker with live preview
- `Ctrl-F`: full-text search with snippet preview
- `Ctrl-T`: tag filter mode
- `Ctrl-P`: property filter mode
- `/`: return to fuzzy mode

Primary keys:

- `Enter`: edit the selected note
- `Ctrl-V`: toggle the preview pane between the raw file/snippet view and a Dataview inspector for the selected note
- `Ctrl-N`: create a new note
- `Ctrl-R`: move or rename the selected note
- `Ctrl-B`: backlinks view
- `Ctrl-O`: outgoing-links view
- `Ctrl-D`: note-scoped doctor view
- `Ctrl-G`: git history for the selected file
- `Esc`: quit

Ctrl-F full-text extras:

- `Ctrl-S`: cycle search result ordering (`relevance`, `path-*`, `modified-*`, `created-*`)
- `Alt-C`: toggle global case-sensitive matching
- `Ctrl-E`: toggle the parsed-query explain pane
- `PageUp` / `PageDown`: scroll the explain pane

Additional browse notes:

- By default, `browse` opens immediately on current cache contents and runs a background incremental refresh. Use `[scan].browse_mode` or `--refresh` to change that behavior.
- Printable characters always extend the active query or prompt. Browse actions use `Enter` or `Ctrl-*` shortcuts.
- In full-text mode, the status line mirrors the parsed query/explain output when the explain pane is open.
- After edits, creates, and moves, Vulcan rescans the affected files and refreshes the list.
- In backlinks and outgoing-link views, `o` opens the selected `.base` file in the Bases TUI.
- `browse` accepts `--no-commit` to suppress auto-commit for that session.

### `open`

`vulcan open [note]` resolves a note and launches `obsidian://open?vault=<vault>&file=<path>` through the platform opener:

- Linux: `xdg-open`
- macOS: `open`
- Windows: `start`

This is for quickly handing off from CLI inspection to the Obsidian desktop app.

### `bases tui`

`vulcan bases tui <file.base>` opens the interactive Bases TUI. It is the main interactive surface for inspecting a `.base` result set and editing view definitions through validated mutation paths instead of manual YAML editing.

## Mutations, dry runs, and auto-commit

Most vault-mutating commands support `--no-commit`. Preview-capable mutations also expose `--dry-run`.

Common mutating commands:

- `move`
- `link-mentions`
- `rewrite`
- `update`
- `unset`
- `rename-property`
- `merge-tags`
- `rename-alias`
- `rename-heading`
- `rename-block-ref`
- `edit`
- `browse`
- `inbox`
- `template`
- `bases view-*`

Auto-commit is opt-in and configured in vault config, typically `.vulcan/config.toml` and optionally overridden in `.vulcan/config.local.toml`:

```toml
[git]
auto_commit = true
trigger = "mutation"
message = "vulcan {action}: {files}"
scope = "vulcan-only"
exclude = [".obsidian/workspace.json", ".obsidian/workspace-mobile.json"]
```

Behavior:

- `auto_commit = false` is the default.
- `trigger = "mutation"` commits successful mutating commands.
- `trigger = "scan"` also allows `scan` and `watch` to auto-commit external changes after they are indexed.
- `--no-commit` suppresses auto-commit for one invocation even when the vault config enables it.

## Inbox and templates

### `inbox`

`vulcan inbox` appends a quick capture entry to the configured inbox note.

Examples:

```bash
vulcan inbox "Call Alice"
echo "idea" | vulcan inbox -
vulcan inbox --file draft.txt
```

Inbox configuration lives under `[inbox]` in vault config (`.vulcan/config.toml` with optional local overrides in `.vulcan/config.local.toml`):

```toml
[inbox]
path = "Inbox.md"
format = "- {text}"
timestamp = true
heading = "## Inbox"
```

Supported inbox template variables:

- `{text}`
- `{date}`
- `{time}`
- `{datetime}`

Behavior:

- The inbox note is created automatically if it does not exist.
- If `heading` is set, Vulcan appends under that heading and creates the heading if needed.
- After writing, Vulcan runs an incremental scan and then auto-commits if enabled.

### `template`

`vulcan template` creates notes from templates, and `vulcan template insert` inserts rendered templates into existing notes.
If `.obsidian/templates.json` configures a template folder, Vulcan also discovers templates there and reports the source in `--list`.

Examples:

```bash
vulcan template --list
vulcan template daily --path Daily/2026-03-26
vulcan template meeting
vulcan template insert daily Projects/Alpha
vulcan template insert daily --prepend
```

Behavior:

- `NAME` can be the full template filename or the filename stem.
- `.vulcan/templates` takes precedence over the Obsidian template folder when the same template name exists in both places.
- If `--path` is omitted, Vulcan creates `<date>-<template-name>.md` in the vault root.
- In an interactive terminal, the rendered note is opened in the editor before the rescan.
- Default `{{date}}` and `{{time}}` formats come from `[templates]` in `.vulcan/config.toml`.
- `template insert` renders `{{title}}` and the other template variables against the target note.
- `template insert` appends by default; `--prepend` inserts after the target note's frontmatter.
- If the insert target note is omitted in an interactive terminal, Vulcan opens the note picker.
- Template frontmatter is merged on insert: existing scalar values are preserved, missing keys are added, and list properties are union-merged.
- After either template creation or template insertion, Vulcan runs an incremental scan and then auto-commits if enabled.

Supported template variables:

- `{{title}}`
- `{{date}}`
- `{{date:YYYY-MM-DD}}`
- `{{date:dddd, MMMM Do YYYY}}`
- `{{time}}`
- `{{time:HH:mm}}`
- `{{datetime}}`
- `{{uuid}}`

Supported Moment-style tokens for `{{date:...}}` and `{{time:...}}`:

- `YYYY`, `YY`
- `MM`, `M`
- `DD`, `D`, `Do`
- `dd`, `ddd`, `dddd`
- `HH`, `H`
- `hh`, `h`
- `mm`, `m`
- `ss`, `s`
- `A`, `a`
- `MMMM`, `MMM`

## Saved reports, exports, and automation

Saved report definitions live under `.vulcan/reports`.

Relevant commands:

- `vulcan saved search ...`
- `vulcan saved notes ...`
- `vulcan saved bases ...`
- `vulcan saved run <name>`
- `vulcan saved list`
- `vulcan saved show <name>`
- `vulcan batch <name>...`
- `vulcan batch --all`
- `vulcan automation run ...`

Examples:

```bash
vulcan saved search release-dashboard 'dashboard "release notes"' --description "Release dashboard hits"
vulcan saved notes due-soon --where 'due <= 2026-04-01' --sort due
vulcan saved run release-dashboard --output json
vulcan batch release-dashboard due-soon
vulcan automation run --scan --doctor --verify-cache --fail-on-issues
```

Automation notes:

- `automation run --doctor-fix` applies deterministic doctor fixes before reporting status.
- `automation run --fail-on-issues` returns exit code `2` when checks complete but issues remain.
- `saved search` and `saved notes` use the same syntax as `search` and `notes`.

## Shell completions

Generate completions:

```bash
vulcan completions bash
vulcan completions fish
vulcan completions zsh
```

Typical install examples:

```bash
vulcan completions bash > ~/.local/share/bash-completion/completions/vulcan
vulcan completions fish > ~/.config/fish/completions/vulcan.fish
```

Supported shells:

- `bash`
- `elvish`
- `fish`
- `powershell`
- `zsh`

## Planned commands

### Phase A: CLI for LLMs (Roadmap Wave 5)

The highest-priority CLI additions make Vulcan usable as a tool surface for any LLM harness (Claude Code, Codex, Gemini CLI, etc.) without the embedded agent. See `docs/ROADMAP.md` Phase 9.18 for full details.

**Note CRUD (9.18.2):**

```
vulcan note get <note> [--heading|--block-ref|--lines|--match|--context N|--no-frontmatter|--raw]
vulcan note set <note> [--file|--no-frontmatter|--check]
vulcan note create <path> [--template|--frontmatter k=v|--check]
vulcan note append <note> <text> [--heading|--check]
vulcan note patch <note> --find <str|regex> --replace <str> [--all|--check|--dry-run]
vulcan note doctor|links|backlinks|diff <note>
```

**Tool discovery for LLMs (9.18.7):**

```
vulcan describe                              # compact command listing with one-liners
vulcan describe --format openai-tools        # tool definitions for function calling
vulcan describe --format mcp                 # MCP tool definitions
vulcan help <topic> --output json            # structured help for machine consumption
```

**Other Wave 5 commands:**

```
vulcan query '...' [--format table|paths|detail|count] [--glob ...]
vulcan search '...' [--regex <pattern>]
vulcan web search|fetch
vulcan git status|log|diff|commit|blame
vulcan daily today|show|list|append
vulcan help [<topic>]
```

**External harness deliverables:** vault AGENTS.md template (written on `vulcan init`), default skills in `AI/Skills/` (bundled, written on `vulcan init` or `vulcan assistant init`), consistent JSON error output on all commands.

### Phase B: Embedded Agent (Roadmap Wave 6)

The full vault-native AI assistant with tiered tool exposure, vault-aware system prompt, conversation persistence, prompts, and skills.

```
vulcan assistant <prompt>                    # one-shot prompt
vulcan assistant --chat                      # multi-turn conversation
vulcan assistant --file <note> <prompt>      # prompt about a specific note
vulcan assistant --prompt <name>             # use a named prompt
vulcan assistant --skill <name>              # invoke a skill
vulcan assistant --resume <session>          # resume a conversation
vulcan assistant sessions                    # list saved sessions
vulcan assistant prompts                     # list available prompts
vulcan assistant skills                      # list available skills
vulcan assistant init                        # write default skills + vault AGENTS.md
```

### Phase C: Chat Platforms (Roadmap Wave 7)

```
vulcan assistant serve [--platform telegram|all]
vulcan assistant platforms                   # list configured platforms
vulcan assistant memory <platform> <user-id> # show user memory
```

### CLI Redesign — Command Reorganization (Roadmap 9.18.1, lands last)

The full two-level command hierarchy. This is a pre-alpha clean break that restructures the flat command layout.

```
vulcan refactor rename-alias|rename-heading|rename-block-ref|rename-property|merge-tags|rewrite|move|link-mentions
vulcan refactor suggest mentions|duplicates
vulcan ls [--glob ...] [--where ...] [--tag ...]   # alias for query with --format paths
vulcan run <script.js|script-name> [--sandbox strict|fs|net|none] [--timeout 30s]  # strips #! shebang if present
vulcan run --script <file>          # shebang entry point (for #!/usr/bin/env -S vulcan run --script)
vulcan run                          # REPL mode (no args)
vulcan tasks create|complete|reschedule  # new mutations
```

**Key changes from current layout:**
- `links`, `backlinks` → `note links`, `note backlinks`
- `rename-*`, `merge-tags`, `rewrite`, `move`, `link-mentions`, `suggest` → `refactor *`
- `init`, `scan`, `rebuild`, `repair`, `watch`, `serve` → `index *`
- New: `note get/set/create/append/patch`, `run` (JS runtime + REPL), `web search/fetch`, `git *`, `help`, `daily *`, `assistant *`

### `canvas` (Roadmap Phase 18)

Canvas support will add CLI commands for inspecting and validating Obsidian JSON Canvas files (`.canvas`):

- `vulcan canvas [path]`: summary view (node/edge counts, referenced files)
- `vulcan canvas list`: list all canvas files in the vault
- `vulcan canvas nodes <path>`: list all nodes with type, position, and content preview
- `vulcan canvas edges <path>`: list all edges with from/to labels
- `vulcan canvas validate <path>`: structural validation
- `vulcan canvas refs <path>`: file references with resolution status

Canvas text nodes will be indexed in FTS5 and searchable via `vulcan search`. File node references will participate in the vault graph (backlinks, doctor). See `docs/ROADMAP.md` Phase 18.

## Related docs

- `docs/design_document.md`
- `docs/ROADMAP.md`
- `docs/performance.md`
