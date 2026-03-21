# CLI Guide

This document describes the current user-facing Vulcan CLI. It focuses on what is implemented today. For architecture and design decisions, see `docs/design_document.md`.

## Build and run

Build the release binary from the repo root:

```bash
cargo build --release -p vulcan-cli --bin vulcan
```

The binary will be written to:

```text
./target/release/vulcan
```

Typical first run against a vault:

```bash
./target/release/vulcan --vault ~/wikis/mimir init
./target/release/vulcan --vault ~/wikis/mimir scan
```

## Discoverability

Use these entry points first:

- `vulcan --help` for the top-level command list
- `vulcan <command> --help` for command-specific syntax and examples
- `vulcan describe` for a machine-readable command schema
- `vulcan completions bash` or `vulcan completions fish` for shell completions

Important naming note:

- The note-query command is `vulcan notes`, not `vulcan note`

## Global flags

These flags are available on all commands:

- `--vault <PATH>`: vault root directory
- `--output <human|json>`: human-readable or machine-readable output
- `--fields <a,b,c>`: limit list output to selected fields
- `--limit <N>`: cap returned rows
- `--offset <N>`: paginate list output
- `--provider <NAME>`: override the embedding provider for vector commands
- `--verbose`: enable extra diagnostic output

## Querying notes with `notes`

`vulcan notes` queries notes by typed frontmatter properties plus file metadata.

Examples:

```bash
vulcan --vault ~/wikis/mimir notes --where 'status = done'
vulcan --vault ~/wikis/mimir notes --where 'tags contains sprint' --sort due
vulcan --vault ~/wikis/mimir notes --where 'file.path starts_with "Projects/"'
```

### `--where` filter syntax

Each filter uses this form:

```text
<field> <operator> <value>
```

Repeat `--where` to combine filters with logical `AND`.

Current limitation:

- `--where` does not support `OR`, parentheses, or nested expressions

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

- `starts_with` is for text fields
- `contains` is for list-valued properties such as `tags`

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
vulcan notes --where 'reviewed = true'
vulcan notes --where 'estimate > 2'
vulcan notes --where 'due >= 2026-03-01'
vulcan notes --where 'tags contains sprint'
vulcan notes --where 'file.path starts_with "People/"'
vulcan notes --where 'file.ext = md'
```

### Sorting

Use `--sort <FIELD>` with:

- any property key
- `file.path`
- `file.name`
- `file.ext`
- `file.mtime`

Use `--desc` for descending order.

## Searching with `search`

`vulcan search` searches indexed note content and can be combined with property filters.

Examples:

```bash
vulcan --vault ~/wikis/mimir search dashboard
vulcan --vault ~/wikis/mimir search 'dashboard "release notes" -draft'
vulcan --vault ~/wikis/mimir search dashboard --where 'reviewed = true'
vulcan --vault ~/wikis/mimir search 'tag:index path:People/ owned'
```

### Default search query syntax

By default, the query string is parsed into a simple semantic search surface:

- Plain terms are combined with logical `AND`
- Quoted phrases stay together: `"owned by"`
- Use `or` between positive terms: `dashboard or summary`
- Prefix a term or quoted phrase with `-` to exclude it: `dashboard -draft -"old version"`

Supported inline filters inside the query text:

- `tag:<tag>`
- `path:<prefix>`
- `has:<property>`
- `property:<property>`

Examples:

```bash
vulcan search 'dashboard status'
vulcan search '"owned by" -robert'
vulcan search 'dashboard or summary'
vulcan search 'tag:index path:People/ has:status owned'
```

Notes:

- Inline filters are parsed only from unquoted positive tokens
- Use `--where` for typed property filters such as booleans, numbers, dates, or list membership

### `--raw-query`

Use `--raw-query` when you want to pass SQLite FTS5 syntax through unchanged:

```bash
vulcan search --raw-query '"release" NEAR/5 "notes"'
```

When `--raw-query` is set, Vulcan does not reinterpret the query text as phrases, `or`, `-term`, or inline `tag:` / `path:` filters.

### Additional search filters

These flags narrow the result set outside the text query:

- `--where <FILTER>`: typed property filters using the same grammar as `vulcan notes`
- `--tag <TAG>`: require a tag
- `--path-prefix <PREFIX>`: require a path prefix
- `--has-property <KEY>`: require the property to exist
- `--mode <keyword|hybrid>`: choose search strategy
- `--fuzzy`: retry empty searches with typo-tolerant expansion
- `--explain`: include search plan and score details

## Bases and saved reports

### `bases eval`

`vulcan bases eval <file.base>` evaluates a supported subset of `.base` files against the indexed vault.

Important current behavior:

- Vulcan does not treat `.base` syntax as the canonical query language for the whole product
- Supported Bases filters compile into the same typed property/file filter model used by `notes`
- Unsupported `.base` constructs surface as diagnostics instead of being silently ignored

### Saved reports

Saved reports live under `.vulcan/reports`.

Relevant commands:

- `vulcan saved search ...`
- `vulcan saved notes ...`
- `vulcan saved bases ...`
- `vulcan saved run <name>`
- `vulcan saved list`
- `vulcan saved show <name>`

`saved search` and `saved notes` use the same query and filter syntax as `search` and `notes`.

## Output shaping and exports

For scripting and agent use, prefer these controls:

- `--output json`
- `--fields`
- `--limit`
- `--offset`
- `--export <csv|jsonl> --export-path <FILE>`

Examples:

```bash
vulcan --output json search dashboard --limit 5
vulcan --output json --fields document_path,rank search dashboard
vulcan notes --where 'status = done' --export csv --export-path done.csv
```

Notes:

- Human output is optimized for terminal reading
- JSON output is the stable machine-oriented surface
- Commands that stream rows emit line-delimited JSON when appropriate

## Interactive behavior

Interactive features are optional conveniences. All commands still have deterministic non-interactive behavior.

Current TTY-only behavior:

- If you omit the note on some single-note commands, Vulcan can open a built-in fuzzy picker
- The picker is currently used for `links`, `backlinks`, `related`, `vectors related`, and note-backed `vectors neighbors`
- In non-interactive mode, or when `--output json` is active, Vulcan does not auto-prompt

Current Bases TUI behavior:

- `vulcan bases tui <file.base>` opens an interactive table view
- Diagnostics are hidden by default and toggleable
- The detail pane shows structured fields plus a file preview
- Full-screen preview is available from the TUI
- Property edits use the same validated mutation path as CLI refactors
- You can hand off to an external editor for notes or `.base` files

## Shell completions

Generate completions:

```bash
vulcan completions bash
vulcan completions fish
```

Typical install examples:

```bash
vulcan completions bash > ~/.local/share/bash-completion/completions/vulcan
vulcan completions fish > ~/.config/fish/completions/vulcan.fish
```

## Related docs

- `docs/design_document.md`
- `docs/ROADMAP.md`
- `docs/performance.md`
