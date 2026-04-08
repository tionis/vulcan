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
./target/release/vulcan --vault ~/wikis/mimir index init
./target/release/vulcan --vault ~/wikis/mimir index scan
./target/release/vulcan --vault ~/wikis/mimir --limit 5 query --where 'status = open'
./target/release/vulcan --vault ~/wikis/mimir browse
```

## Self-discovery

The CLI is designed to be self-describing at runtime.

- `vulcan --help` shows the top-level command groups.
- `vulcan <command> --help` shows command-specific syntax, caveats, and examples.
- `vulcan help` shows the integrated topic index, and `vulcan help <topic>` expands one command or concept.
- Topics include command docs (`help note get`), concepts (`help filters`, `help sandbox`), and JS/runtime reference entries (`help js`, `help js.vault`, `help js.plugins`).
- `vulcan describe` prints a compact command inventory for humans.
- `vulcan --output json describe` prints the runtime command schema in machine-oriented JSON.
- `vulcan --output json describe --format openai-tools` exports OpenAI function-calling tool definitions.
- `vulcan --output json describe --format mcp` exports MCP-style tool definitions.
- `vulcan completions <shell>` generates shell completions.

Useful starting points:

- `vulcan note --help`
- `vulcan search --help`
- `vulcan help filters`
- `vulcan help query`
- `vulcan periodic --help`
- `vulcan vectors --help`
- `vulcan saved --help`
- `vulcan plugin --help`
- `vulcan edit --help`
- `vulcan browse --help`

## Global options and output behavior

These global flags are available on all commands:

- `--vault <PATH>`: vault root directory
- `--output <human|markdown|json>`: output format
- `--refresh <off|blocking|background>`: override automatic cache refresh for cache-backed commands
- `--fields <a,b,c>`: select columns on list-style output
- `--provider <NAME>`: override the embedding provider for vector commands
- `--limit <N>`: cap returned rows
- `--offset <N>`: paginate list output
- `--verbose`: enable extra diagnostic output
- `--quiet`: suppress non-essential stderr output
- `--no-header`: suppress table headers for tabular human output
- `--color <auto|always|never>`: control ANSI color output

Output rules:

- `--output json` is the stable scripting and agent surface.
- Row-oriented commands emit line-delimited JSON.
- Single-report commands emit one JSON object.
- Command failures in JSON mode emit `{"error":"...","code":"..."}` on stdout with a non-zero exit code.
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

`vulcan index init` creates `.vulcan/config.toml`, `cache.db`, and a default `.vulcan/.gitignore` that keeps `config.toml` tracked while ignoring `config.local.toml`. It also detects importable Obsidian settings and reports them; use `vulcan index init --import` to apply every detected importer immediately. Use `vulcan index init --agent-files` to write the bundled `AGENTS.md` template and default `AI/Skills/*.md` reference files into the vault.

Automatic cache refresh is configured under `[scan]`:

```toml
[scan]
default_mode = "blocking"
browse_mode = "background"
```

Meaning:

- `default_mode` controls one-shot cache-backed commands such as `note backlinks`, `note links`, `note diff`, `query`, `search`, `graph`, and refactor commands that depend on current cache state.
- `browse_mode` controls `vulcan browse`.
- `off` uses the current cache as-is.
- `blocking` runs an incremental scan before the command continues.
- `background` is intended for long-lived interactive surfaces; one-shot commands treat it the same as `blocking`.

Use `--refresh` to override the configured behavior for one invocation:

```bash
vulcan --refresh off note backlinks Projects/Alpha
vulcan --refresh background browse
```

Note resolution rules:

- Commands that take a note reference accept a vault-relative path, filename, or alias.
- Some single-note commands allow omitting the note in a TTY and will open the built-in picker instead.
- When `--output json` is active, or when stdin/stdout is not interactive, Vulcan will not auto-prompt.

### Command aliases

Vulcan expands command aliases before clap parses the final argv. Aliases live under `[aliases]` in `.vulcan/config.toml` and are merged with a small built-in default set.

Example:

```toml
[aliases]
q = "query"
t = "tasks list"
today = "daily today"
ship = "query --where 'status = shipped'"
```

Behavior:

- Built-in defaults currently include `q`, `t`, and `today`.
- Vault-local aliases override built-in aliases when they use the same name.
- Use `vulcan config show aliases` to inspect the effective alias map.
- Alias expansion happens before clap parsing, so aliases can target grouped commands such as `tasks list` or `query --where ...`.

## Command catalogue

### Indexing, cache, and local service commands

- `vulcan index init [--import|--no-import] [--agent-files]`: create `.vulcan/`, `cache.db`, `config.toml`, and the local ignore rules; optionally import all detected Obsidian settings immediately and optionally write the bundled AGENTS/skills files.
- `vulcan index scan [--full] [--no-commit]`: perform an incremental or full scan and refresh the cache.
- `vulcan index rebuild [--dry-run]`: rebuild the cache from disk.
- `vulcan index repair fts [--dry-run]`: rebuild the full-text search index from cached chunks.
- `vulcan index watch [--debounce-ms <MS>] [--no-commit]`: keep the cache fresh from filesystem events.
- `vulcan index serve [--bind <ADDR>] [--no-watch] [--debounce-ms <MS>] [--auth-token <TOKEN>]`: start the local HTTP API server backed by the cache.
- `vulcan cache inspect`: show cache sizes and row counts.
- `vulcan cache verify [--fail-on-errors]`: verify cache invariants.
- `vulcan cache vacuum [--dry-run]`: run SQLite `VACUUM` on the cache.
- `vulcan checkpoint create <name>`: capture the current cache state under a checkpoint name.
- `vulcan checkpoint list`: list saved scan and manual checkpoints.
- `vulcan export markdown|json|csv|epub|zip|sqlite <query> ...`: materialize matched notes as combined documents, datasets, books, or archives.
- `vulcan export search-index [--path <FILE>] [--pretty]`: write the cached search corpus as a static JSON index.
- `vulcan changes [--checkpoint <name>]`: report note, link, property, and embedding changes since the last scan or a named checkpoint.
- `vulcan note diff <note> [--since <checkpoint>]`: show one note's changes since git `HEAD`, the last scan, or a named checkpoint.

### Config and import commands

- `vulcan config show [section]`: show the effective merged config or one section such as `periodic` or `aliases`.
- `vulcan config get <key>`: read one config value.
- `vulcan config edit [--no-commit]`: open the interactive `ratatui` settings editor for `.vulcan/config.toml`.
- `vulcan config set <key> <value> [--dry-run] [--no-commit]`: validate and write one config value.
- `vulcan config import core [--preview|--dry-run|--apply] [--target <shared|local>] [--no-commit]`: import Obsidian core settings from `.obsidian/app.json`, `.obsidian/templates.json`, and `.obsidian/types.json`.
- `vulcan config import dataview [--preview|--dry-run|--apply] [--target <shared|local>] [--no-commit]`: import Obsidian Dataview plugin settings.
- `vulcan config import kanban [--preview|--dry-run|--apply] [--target <shared|local>] [--no-commit]`: import Obsidian Kanban plugin settings.
- `vulcan config import periodic-notes [--preview|--dry-run|--apply] [--target <shared|local>] [--no-commit]`: import Obsidian Daily Notes core plugin settings plus the community Periodic Notes plugin settings.
- `vulcan config import tasks [--preview|--dry-run|--apply] [--target <shared|local>] [--no-commit]`: import Obsidian Tasks plugin settings.
- `vulcan config import templater [--preview|--dry-run|--apply] [--target <shared|local>] [--no-commit]`: import Obsidian Templater plugin settings.
- `vulcan config import --all [--preview|--dry-run|--apply] [--target <shared|local>] [--no-commit]`: run every detected importer in registry order and aggregate the results.
- `vulcan config import --list`: show every registered importer together with detection status and source paths.

Shared behavior:

- `config edit` groups settings by category, shows the effective value alongside shared/local overrides, validates each edit, and only writes the shared config file when you save.
- `--preview` and `--dry-run` are equivalent: they print the mapping plus a diff of the target config file without writing either `.vulcan/config.toml` or `.vulcan/config.local.toml`.
- `--apply` is the explicit write path; omitting both `--preview` and `--apply` still applies the import for backwards compatibility.
- `--target local` writes to `.vulcan/config.local.toml`; the default target is the shared `.vulcan/config.toml`.
- `--output json` returns the full import report, including target file, whether the run was a dry run, the mappings applied, and any detected conflicts.
- When vault auto-commit is enabled for mutations, config imports participate unless `--no-commit` is passed.

### Query, graph, and reporting commands

- `vulcan note links [note]`: list outgoing links for a note.
- `vulcan note backlinks [note]`: list inbound links for a note.
- `vulcan graph path [from] [to]`: shortest resolved-link path between two notes.
- `vulcan graph hubs`: notes with the highest degree.
- `vulcan graph moc`: candidate map-of-content notes.
- `vulcan graph dead-ends`: notes without outbound resolved note links.
- `vulcan graph components`: weakly connected components.
- `vulcan graph stats`: note-graph and vault analytics summary.
- `vulcan graph trends [--limit <N>]`: trends across saved scan checkpoints.
- `vulcan search <query> [--regex <pattern>] ...`: full-text search with optional typed property filters, explicit result sorting, and explicit regex mode.
- `vulcan query [QUERY] [--where <filter>] [--sort <field>] [--desc] [--language <auto|vulcan|dql>] ...`: run the human DSL, Dataview DQL, or JSON query payload with `--format table|paths|detail|count` and optional `--glob`.
- `vulcan ls [--where <filter>] [--tag <tag>] [--glob <pattern>] [--format <paths|detail|count>]`: thin alias for `query 'from notes'`.
- `vulcan refactor suggest mentions [note]`: plain-text mentions that could become links.
- `vulcan refactor suggest duplicates`: duplicate titles, alias collisions, and merge candidates.
- `vulcan refactor ...`: grouped cross-vault mutation commands (`rename-*`, `merge-tags`, `rewrite`, `move`, `link-mentions`, `suggest`).
- `vulcan saved list`: list saved query/report definitions from `.vulcan/reports`.
- `vulcan saved show <name>`: show one saved report definition.
- `vulcan saved create search <name> ...`: save a search definition.
- `vulcan saved create notes <name> ...`: save a query shortcut built from `query --where/--sort`.
- `vulcan saved create bases <name> <file.base>`: save a `.base` evaluation definition.
- `vulcan saved delete <name>`: remove one saved report definition.
- `vulcan saved run <name>`: execute one saved report.
- `vulcan automation list`: list saved reports automation can run.
- `vulcan automation run [<name> ...] [--all] ...`: run saved reports plus optional scan, doctor, verify, or FTS repair steps for CI and scripts.

### Periodic note commands

- `vulcan daily today [--no-edit] [--no-commit]`: open or create today's daily note.
- `vulcan daily show [date]`: print one daily note's contents. Defaults to today.
- `vulcan daily list [--from <date>] [--to <date>] [--week] [--month]`: list daily notes and extracted schedule events across a date window.
- `vulcan daily export-ics [--from <date>] [--to <date>] [--week] [--month] [--path <file.ics>] [--calendar-name <name>]`: export extracted daily-note events as an ICS calendar. Without `--path`, the calendar is written to stdout.
- `vulcan daily append <text> [--heading <heading>] [--date <date>] [--no-commit]`: append text to one daily note, creating it first when needed.
- `vulcan periodic weekly [date] [--no-edit] [--no-commit]`: open or create the weekly note containing the given date.
- `vulcan periodic monthly [date] [--no-edit] [--no-commit]`: open or create the monthly note containing the given date.
- `vulcan periodic <type> [date] [--no-edit] [--no-commit]`: generic open-or-create command for any configured period type.
- `vulcan periodic list [--type <period>]`: list indexed periodic notes from the cache.
- `vulcan periodic gaps [--type <period>] [--from <date>] [--to <date>]`: show missing periodic notes by expected path.

Behavior:

- Periodic note defaults come from `[periodic.*]` in `.vulcan/config.toml`.
- Custom period types use the same config map: define `[periodic.<name>]` with `unit = "days|weeks|months|quarters|years"`, `interval = <n>`, and an optional `anchor_date = "YYYY-MM-DD"` to align the cycle.
- `daily list` uses the configured weekly start when `--week` is selected and includes parsed schedule events from the `events` cache table.
- `daily export-ics` uses the same cached events and emits a one-way RFC 5545 calendar export.
- Periodic note creation uses the configured periodic template name when it resolves successfully; otherwise Vulcan creates a blank note and reports the template warning.
- Hidden compatibility aliases `weekly` and `monthly` still work for existing scripts, but the preferred public forms are `periodic weekly` and `periodic monthly`.
- These commands participate in auto-commit when they mutate note files and vault git auto-commit is enabled.

### Git commands

- `vulcan git status`: show staged, unstaged, and untracked vault files while filtering out `.vulcan/` internals.
- `vulcan git log [--limit <n>]`: show recent commit history for the vault repository.
- `vulcan git diff [path]`: show the current diff for one vault-relative path or for all eligible changed files.
- `vulcan git commit -m <message>`: stage changed vault files and create a commit, skipping `.vulcan/`.
- `vulcan git blame <path>`: show per-line authorship for one tracked vault-relative file.

Behavior:

- All `git` subcommands support `--output json`.
- `git diff` and `git blame` take vault-relative file paths, not note aliases.
- `git commit` refuses to stage `.vulcan/` state even when it is dirty.

### Runtime commands

- `vulcan run <script.js>`: execute a JavaScript file through the Vulcan runtime, stripping a leading shebang line when present.
- `vulcan run <script-name>`: resolve `.vulcan/scripts/<name>` or `.vulcan/scripts/<name>.js`.
- `vulcan run --script <file>`: shebang-friendly entrypoint for `#!/usr/bin/env -S vulcan run --script`.
- `vulcan run`: open the interactive JS REPL with persistent context.

Behavior:

- `console.log(...)` emits paragraph output before the final return value.
- `--output json` returns both `outputs` and the final `value`.
- `--timeout <duration>` overrides the JS execution limit for one script run or REPL session.
- `--sandbox strict|fs|net|none` selects the runtime capability tier for one script run or REPL session.
- The REPL supports multiline input, tab completion, history in `.vulcan/repl_history`, pretty-printed objects, and preserved JS variables between prompts.
- The runtime exposes `vault.note()`, `vault.notes()`, `vault.query()`, `vault.search()`, `vault.graph.*`, `vault.daily.*`, `vault.events()`, `vault.set/create/append/patch/update/unset`, `vault.transaction()`, `vault.refactor.*`, `web.search()`, `web.fetch()`, and `help(obj)`.

### Plugin commands

- `vulcan plugin list`: list registered plugin definitions plus discovered `.vulcan/plugins/*.js` files.
- `vulcan plugin enable <name>`: create or update `[plugins.<name>]` in `.vulcan/config.toml` and mark it enabled.
- `vulcan plugin disable <name>`: keep the plugin registration but disable hook execution.
- `vulcan plugin run <name>`: execute one plugin's `main(event, ctx)` entrypoint manually.

Behavior:

- Plugin files default to `.vulcan/plugins/<name>.js` unless `[plugins.<name>].path` overrides the location.
- Plugin registrations live in `.vulcan/config.toml` and can declare `events`, `sandbox`, `permission_profile`, and `description`.
- `on_note_write` and `on_pre_commit` are blocking hooks; other lifecycle hooks are post-events and only log failures.
- Vault trust is required for plugin execution. Untrusted vaults skip hooks with a warning and reject manual `plugin run`.
- `vulcan help js.plugins` documents the handler names, event payloads, and execution context.

### Web commands

- `vulcan web search <query> [--backend <name>] [--limit <n>]`: query the configured web search backend and return title/url/snippet results.
- `vulcan web fetch <url> [--mode markdown|html|raw] [--extract-article] [--save <path>]`: fetch one URL and render or save the response body.
- `vulcan help [<topic>] [--search <keyword>]`: browse integrated command and concept docs, with `--output json` for structured help consumers.
- Built-in help topics include `getting-started`, `examples`, `filters`, `query-dsl`, `scripting`, `sandbox`, `js`, `js.vault`, `js.vault.graph`, and `js.vault.note`.
- `vulcan describe [--format json-schema|openai-tools|mcp]`: export the CLI surface for humans or tool integrations.

Behavior:

- `web search` reads `[web.search]` from `.vulcan/config.toml`. The default backend is `duckduckgo` (no API key required). Setting `backend = "auto"` auto-selects the first available keyed backend (Kagi → Exa → Tavily → Brave) and falls back to DuckDuckGo. Supported backends: `duckduckgo`, `kagi` (`KAGI_API_KEY`), `exa` (`EXA_API_KEY`), `tavily` (`TAVILY_API_KEY`), `brave` (`BRAVE_API_KEY`).
- `web fetch` uses the configured Vulcan user-agent and performs a best-effort `robots.txt` check before requesting the target URL.
- `web fetch --mode markdown` converts HTML into readable markdown-like text; `--extract-article` prefers `<article>` or `<main>` content when present.
- `web fetch --save` writes the fetched output to disk and still reports metadata in JSON mode.

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
- `vulcan vectors cluster [--clusters <N>] [--dry-run]`: group indexed vectors into topical clusters.
- `vulcan vectors duplicates [--threshold <F32>]`: highly similar chunk pairs.
- `vulcan vectors models`: list stored embedding models.
- `vulcan vectors drop-model <cache-key>`: drop one model and its vectors.

Compatibility note:

- Hidden top-level compatibility aliases `cluster` and `related` still exist, but the preferred public forms are `vectors cluster` and `vectors related`.

## Query and filter syntax

`vulcan query --where`, `vulcan search --where`, `vulcan refactor rewrite`, `vulcan note update`, `vulcan note unset`, and `bases view-*` filters all use the same typed filter grammar.

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
- `matches`
- `matches_i`

Operator notes:

- `starts_with` is for text fields.
- `contains` is for list-valued properties such as `tags`.
- `matches` applies a Rust `regex` to string-valued fields.
- `matches_i` is the case-insensitive `regex` variant.

Supported value types:

- Text: `done`, `"In Progress"`, `'Rule Index'`
- Boolean: `true`, `false`
- Null: `null`
- Number: `42`, `3.5`
- Date or datetime: `2026-03-01`, `2026-03-01T09:30:00Z`
- `file.mtime`: integer milliseconds since the Unix epoch

Examples:

```bash
vulcan query --where 'status = done'
vulcan query --where 'tags contains sprint' --sort due
vulcan query --where 'file.path starts_with "Projects/"'
vulcan query --where 'file.name matches "^2026-"'
vulcan query --where 'owner matches_i "alice"'
vulcan note update --where 'status = draft' --key status --value done --dry-run
vulcan refactor rewrite --where 'file.path starts_with "Archive/"' --find TODO --replace DONE
vulcan bases view-add release.base Inbox --filter 'status = idea'
```

## Searching with `search`

`vulcan search` searches indexed note content and can be combined with typed filters.

Examples:

```bash
vulcan search dashboard
vulcan search 'dashboard "release notes" -draft'
vulcan search dashboard --where 'reviewed = true'
vulcan search --regex 'release\\s+readiness'
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
vulcan query
vulcan query --where 'status = done' --sort due
vulcan query 'from notes where status = done order by file.mtime desc limit 10'
vulcan query 'from notes where tags contains sprint and reviewed = true'
vulcan query --format paths 'from notes where file.name matches "^2026-"'
vulcan query --language dql 'TABLE status FROM #project'
vulcan query --glob 'Projects/**' 'from notes'
vulcan query --json '{"source":"notes","predicates":[{"field":"status","operator":"eq","value":"done"}]}'
vulcan query --explain 'from notes where status = backlog'
```

Behavior:

- Bare `vulcan query` defaults to `from notes`.
- `query --where ... --sort ...` builds a note query without writing the DSL explicitly.
- `--language auto` detects Dataview DQL when the query starts with `TABLE`, `LIST`, `TASK`, or `CALENDAR`.
- Use `--language dql` or `--language vulcan` to force the parser when auto-detection is not what you want.

Use `vulcan describe` when you need the runtime schema for the JSON form rather than prose.

## Interactive workflows

### Picker-backed note selection

These commands can resolve a note interactively when you omit the note in a TTY session:

- `note links`
- `note backlinks`
- `graph path`
- `vectors related`
- `note diff`
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
- `o`: with an empty fuzzy/tag/property query, open the selected Kanban board in a side-by-side board view
- `Esc`: quit

Ctrl-F full-text extras:

- `Ctrl-S`: cycle search result ordering (`relevance`, `path-*`, `modified-*`, `created-*`)
- `Alt-C`: toggle global case-sensitive matching
- `Ctrl-E`: toggle the parsed-query explain pane
- `PageUp` / `PageDown`: scroll the explain pane

Additional browse notes:

- By default, `browse` opens immediately on current cache contents and runs a background incremental refresh. Use `[scan].browse_mode` or `--refresh` to change that behavior.
- Printable characters always extend the active query or prompt. Browse actions use `Enter` or `Ctrl-*` shortcuts.
- In fuzzy/tag/property modes, `o` is reserved as a board-view shortcut only when the current query is empty and the selected note is an indexed Kanban board.
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

- `note`
- `refactor move`
- `refactor link-mentions`
- `refactor rewrite`
- `refactor rename-property`
- `refactor merge-tags`
- `refactor rename-alias`
- `refactor rename-heading`
- `refactor rename-block-ref`
- `edit`
- `browse`
- `daily`
- `periodic`
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

### `note`

`vulcan note` groups selector-aware single-note CRUD commands for automation and agent workflows.

Examples:

```bash
vulcan note get Dashboard --heading Tasks --match TODO --context 1
vulcan note get Dashboard --block-ref done-item --raw
vulcan note set Dashboard --no-frontmatter < body.md
vulcan note create Inbox/Idea --template brief --frontmatter status=idea
vulcan note append Dashboard "Shipped" --heading "## Done"
vulcan note patch Dashboard --find "/TODO \\d+/" --replace DONE --all --dry-run
```

Behavior:

- `note get` reads one note from disk and can compose `--heading`, `--block-ref`, `--lines`, `--match`, `--context`, and `--no-frontmatter`.
- `note get --raw` prints only the selected content; without `--raw`, line-oriented selectors show line numbers in human output.
- `note get --output json` returns the selected `content`, parsed `frontmatter`, and selection `metadata`.
- `note set` reads replacement content from stdin by default; `--file <path>` switches the input source.
- `note set --no-frontmatter` preserves the leading YAML block byte-for-byte and replaces only the note body.
- `note create` creates an empty note when stdin is a TTY, or merges piped stdin content with an optional template body when provided.
- `note create --frontmatter key=value` adds or overrides top-level frontmatter keys after template rendering.
- `note append` accepts literal text or `-` to read appended content from stdin.
- `note patch` accepts literal strings or `/regex/` patterns. It fails when the pattern matches more than once unless `--all` is passed.
- `note patch --dry-run` reports the planned replacements without modifying the note.
- Mutating note commands rescan the vault incrementally after a successful write and participate in auto-commit unless `--no-commit` is passed.
- `--check` on `set`, `create`, `append`, and `patch` runs non-blocking doctor-like diagnostics for the resulting note and includes those diagnostics in JSON output.

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

`vulcan template` creates notes from templates, `vulcan template insert` inserts rendered templates into existing notes, and `vulcan template preview` renders without writing a note.
If `.obsidian/templates.json` or the Templater plugin configures a template folder, Vulcan also discovers templates there and reports the source in `--list`.

Examples:

```bash
vulcan template list
vulcan template daily --path Daily/2026-03-26 --engine auto
vulcan template meeting
vulcan template insert daily Projects/Alpha --engine templater --var project=Vulcan
vulcan template insert daily --prepend
vulcan template preview daily --path Journal/Today
```

Behavior:

- `NAME` can be the full template filename or the filename stem.
- `.vulcan/templates` takes precedence over the Obsidian template folder when the same template name exists in both places.
- If `--path` is omitted, Vulcan creates `<date>-<template-name>.md` in the vault root.
- In an interactive terminal, the rendered note is opened in the editor before the rescan.
- Default `{{date}}` and `{{time}}` formats come from `[templates]` in `.vulcan/config.toml`.
- `--engine auto|native|templater` selects the renderer. `auto` is the default and switches to the Templater-compatible engine when the template contains `<% ... %>` tags.
- Repeat `--var key=value` to satisfy `tp.system.prompt()` and `tp.system.suggester()` in non-interactive runs.
- `template insert` renders `{{title}}` and the other template variables against the target note.
- `template insert` appends by default; `--prepend` inserts after the target note's frontmatter.
- If the insert target note is omitted in an interactive terminal, Vulcan opens the note picker.
- `template preview` disables mutating `tp.file.*` helpers and reports them as diagnostics instead of writing files.
- Template frontmatter is merged on insert: existing scalar values are preserved, missing keys are added, and list properties are union-merged.
- After either template creation or template insertion, Vulcan runs an incremental scan and then auto-commits if enabled.

Templater compatibility:

- Templater tags `<% ... %>`, `<%* ... %>`, `<%+ ... %>`, `<%_`, `_%>`, `<%-`, and `-%>` are supported alongside the original `{{date}}` and `{{title}}` variables.
- Native `tp.date`, `tp.file`, `tp.frontmatter`, `tp.system`, and `tp.config` helpers work without any extra setup.
- JS-backed helpers such as `tp.web.*`, `tp.user.*`, `tp.hooks.on_all_templates_executed()`, and `tp.obsidian.requestUrl()` require the default `js_runtime` feature.
- `[templates].user_scripts_folder` controls `tp.user.<name>()` script discovery, and `[templates].web_allowlist` gates outbound hosts for `tp.web.request()`, `tp.web.daily_quote()`, and `tp.obsidian.requestUrl()`.

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

- `vulcan saved create search ...`
- `vulcan saved create notes ...`
- `vulcan saved create bases ...`
- `vulcan saved run <name>`
- `vulcan saved list`
- `vulcan saved show <name>`
- `vulcan saved delete <name>`
- `vulcan automation list`
- `vulcan automation run [<name> ...] [--all] ...`

Examples:

```bash
vulcan saved create search release-dashboard 'dashboard "release notes"' --description "Release dashboard hits"
vulcan saved create notes due-soon --where 'due <= 2026-04-01' --sort due
vulcan saved run release-dashboard --output json
vulcan automation list
vulcan export epub 'from notes where file.path matches "^(People|Projects)/"' -o exports/team.epub --title "Team Notes" --backlinks
vulcan export epub 'from notes where file.path starts_with "Projects/"' -o exports/projects.epub --toc flat
vulcan export epub 'from notes where file.path = "Home.md"' -o exports/home.epub --frontmatter
vulcan export profiles
vulcan export profile team-book
vulcan automation run release-dashboard due-soon --scan --doctor
vulcan automation run --all --verify-cache --fail-on-issues
```

Automation notes:

- `automation run --doctor-fix` applies deterministic doctor fixes before reporting status.
- `automation run --fail-on-issues` returns exit code `2` when checks complete but issues remain.
- `export profiles` lists named recipes from `[export.profiles.<name>]` in `.vulcan/config.toml`.
- `export profile <name>` runs a config-driven export and resolves relative profile paths from the vault root.
- `export epub` preserves the selected note tree in the table of contents by default and can be flattened with `--toc flat`.
- `export epub --frontmatter` includes each note's YAML metadata in a styled collapsible panel before the rendered note body.
- `saved create search` uses the same syntax as `search`.
- `saved create notes` uses the same `--where` and `--sort` shortcut shape as `query`.
- Hidden compatibility aliases such as `batch` and the legacy `saved search|notes|bases` forms still work, but the preferred public forms are `automation ...` and `saved create ...`.

### Query output modes

- `query --format table`: the default structured note rows.
- `query --format paths`: one document path per line.
- `query --format detail`: document path, property summary, and a short preview.
- `query --format count`: only the number of matched rows.
- `query --glob 'Projects/**'`: apply an extra path glob after query evaluation.
- `ls` shares the same implementation but defaults to `--format paths`.

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

## Later roadmap items

### Phase A: CLI for LLMs (Roadmap Wave 5)

The core Wave 5 CLI surface is implemented: grouped commands, note CRUD, structured `help`, `describe --format ...`, bundled AGENTS/skills files, web tools, and git tools are all available today.

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

### Runtime Status

```
vulcan run <script.js|script-name>       # current script execution surface
vulcan run --script <file>               # current shebang entry point
vulcan run --sandbox fs runtime.js       # enable vault writes
vulcan run --sandbox net fetch.js        # enable web helpers
vulcan run                               # interactive REPL
```

Current runtime capabilities:
- sandbox selection with `strict`, `fs`, `net`, and `none`
- multiline REPL editing, completion, history, and persistent context
- write-capable vault APIs such as `vault.transaction()` and `vault.refactor.*`
- network-gated helpers through `web.search()` and `web.fetch()`

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
