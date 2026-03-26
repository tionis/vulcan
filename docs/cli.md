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
- `vulcan search ...`: full-text search with optional typed property filters.
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
- `vulcan bases tui <file.base>`: open the interactive Bases TUI.
- `vulcan bases view-add <file.base> <name> ...`: add a view definition.
- `vulcan bases view-delete <file.base> <name> [--dry-run] [--no-commit]`: delete a view.
- `vulcan bases view-rename <file.base> <old> <new> [--dry-run] [--no-commit]`: rename a view.
- `vulcan bases view-edit <file.base> <name> ...`: mutate filters, columns, sort, and grouping on an existing view.

Important behavior:

- View mutations operate on the parsed `.base` model and write it back through the serializer.
- Unsupported Bases constructs surface as diagnostics instead of being silently ignored.
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
```

Default query syntax:

- Plain terms are combined with logical `AND`.
- Quoted phrases stay together: `"owned by"`.
- Use `or` between positive terms: `dashboard or summary`.
- Prefix a term or quoted phrase with `-` to exclude it.

Supported inline filters inside the query text:

- `tag:<tag>`
- `path:<prefix>`
- `has:<property>`
- `property:<property>`

Useful search flags:

- `--where <FILTER>`: typed property filters using the grammar above
- `--tag <TAG>`: require a tag
- `--path-prefix <PREFIX>`: require a path prefix
- `--has-property <KEY>`: require the property to exist
- `--mode <keyword|hybrid>`: select keyword-only or keyword+vector search
- `--context-size <N>`: snippet context size
- `--raw-query`: pass SQLite FTS5 syntax through unchanged
- `--fuzzy`: retry empty searches with typo-tolerant expansion
- `--explain`: include parsed plan and scoring details

Raw FTS5 example:

```bash
vulcan search --raw-query '"release" NEAR/5 "notes"'
```

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

- `Enter` or `e`: edit the selected note
- `n`: create a new note
- `m`: move or rename the selected note
- `b`: backlinks view
- `l`: outgoing-links view
- `d`: note-scoped doctor view
- `g`: git history for the selected file
- `Esc`: quit

Additional browse notes:

- Single-letter actions only fire when the fuzzy query is empty.
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

Auto-commit is opt-in and configured per vault in `.vulcan/config.toml`:

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

Inbox configuration lives under `[inbox]` in `.vulcan/config.toml`:

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

`vulcan template` creates a note from `.vulcan/templates/*.md`.

Examples:

```bash
vulcan template --list
vulcan template daily --path Daily/2026-03-26
vulcan template meeting
```

Behavior:

- `NAME` can be the full template filename or the filename stem.
- If `--path` is omitted, Vulcan creates `<date>-<template-name>.md` in the vault root.
- In an interactive terminal, the rendered note is opened in the editor before the rescan.

Supported template variables:

- `{{title}}`
- `{{date}}`
- `{{time}}`
- `{{datetime}}`
- `{{uuid}}`

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

## Related docs

- `docs/design_document.md`
- `docs/ROADMAP.md`
- `docs/performance.md`
