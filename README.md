# Vulcan

Headless CLI for [Obsidian](https://obsidian.md)-style vaults and plain Markdown directories. A single Rust binary that indexes your vault into a local SQLite cache and exposes graph queries, full-text search, semantic retrieval, scripting, and safe bulk mutations — no running Obsidian instance required.

## Features

- **Graph-aware indexing** — backlinks, outgoing links, embeds, orphan detection, alias resolution, and incremental cache refresh via file watcher or on-demand scan.
- **Full-text and semantic search** — SQLite FTS5 for keyword search; pluggable embedding providers and `sqlite-vec` for vector similarity, clustering, and related-note recommendations.
- **Structured queries** — filter notes by frontmatter properties, file metadata, tags, folders, and links. Supports Dataview Query Language (DQL), Dataview inline fields (`key:: value`), and Bases-style views.
- **Tasks and Kanban** — query and filter Tasks-plugin tasks and Kanban board cards from the index.
- **Journaling** — daily, weekly, monthly, and custom periodic notes with create/open/append/list workflows and ICS export.
- **Safe mutations** — rename/move notes with automatic link rewriting, bulk property updates, frontmatter unset, refactoring passes, and inbox quick-capture.
- **Templates** — create notes from Handlebars/Tera templates with property interpolation and date math.
- **JavaScript runtime** — embedded rquickjs sandbox for scripting (`vulcan run`), with a vault-aware JS API (`dv.*`, `web.search()`, `web.fetch()`), configurable sandbox tiers (strict / fs / net / none).
- **Web tools** — `vulcan web search` and `vulcan web fetch` for external lookups with configurable backends (Kagi, with more planned).
- **Reports and automation** — saved reports, batch execution, checkpoints, change detection, diff, and an automation command for CI/scheduled jobs.
- **Agent and tool integration** — `--output json` on every command, machine-readable schema export (`vulcan describe`), OpenAI function-calling and MCP tool definitions, shell completions.
- **Interactive TUI** — `vulcan browse` opens a persistent note browser in the terminal.

## Quick start

Build the binary:

```bash
cargo build --release -p vulcan-cli --bin vulcan
```

Initialize and scan a vault:

```bash
vulcan --vault ~/notes init
vulcan --vault ~/notes scan
```

Explore:

```bash
vulcan --vault ~/notes browse              # interactive TUI
vulcan --vault ~/notes search 'meeting notes'
vulcan --vault ~/notes notes --tag project --limit 10
vulcan --vault ~/notes daily today          # open today's daily note
vulcan --vault ~/notes query 'FROM "Projects" WHERE status = "active"'
vulcan --vault ~/notes describe             # print command inventory
```

## Configuration

Configuration lives in `.vulcan/` inside the vault root:

- `config.toml` — shared vault config (intended to be committed/synced)
- `config.local.toml` — device-local overrides (gitignored by default)

The local file merges on top of the shared file. See `vulcan help config` for details.

## Documentation

- [`docs/cli.md`](docs/cli.md) — CLI guide: command catalogue, query/filter syntax, search, interactive flows, config layering, auto-commit, templates, JSON/export
- [`docs/design_document.md`](docs/design_document.md) — Architecture and design decisions
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — Phased implementation status and remaining work
- [`docs/performance.md`](docs/performance.md) — Benchmarking and performance notes
- `vulcan help` — integrated topic index at runtime
- `vulcan help <topic>` — per-command and concept docs (e.g. `vulcan help filters`, `vulcan help sandbox`)

## Workspace

The repository is a Cargo workspace:

| Crate | Purpose |
|---|---|
| `vulcan-core` | Parsing, indexing, cache management, vault configuration, DQL, JS runtime |
| `vulcan-embed` | Embedding provider and vector store abstractions |
| `vulcan-cli` | The `vulcan` binary and command-line interface |
