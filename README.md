# Vulcan

> Pre-alpha: Vulcan is moving fast and still contains a large amount of LLM-written code. Treat it as experimental, keep backups, and use git or another versioning system before pointing it at important vaults.

Vulcan is a headless Rust toolkit for Obsidian-style vaults and plain Markdown directories. It indexes notes into a rebuildable local SQLite cache, then exposes search, graph queries, Dataview/Bases-style metadata, TaskNotes workflows, static publishing, scripting, MCP tools, and safe note mutations without requiring Obsidian to be running.

The long-term shape is a reusable Markdown/vault library stack plus a polished CLI. The current focus is a strong single-vault CLI and MCP server; the next major phase is a multi-vault daemon built on the shared `vulcan-core` and `vulcan-app` crates.

## What It Can Do

- **Index and query Markdown vaults**: incremental scanning, frontmatter, tags, wikilinks, embeds, aliases, block refs, attachments, and diagnostics.
- **Search and explore**: SQLite FTS5, Obsidian-like search operators, graph traversal, backlinks/outgoing links, communities, suggestions, and optional vector search via `sqlite-vec`.
- **Use structured knowledge models**: Dataview DQL, inline fields, inline expressions, `.base` views, task queries, TaskNotes, recurring tasks, dependencies, Kanban boards, and periodic notes.
- **Edit safely**: `note get/create/append/patch/set/delete/rename`, task create/complete/reschedule/archive, property updates, refactors, dry-run reports, link rewriting, and permission profiles.
- **Publish and export**: Markdown, JSON, CSV, Graph, EPUB, ZIP, SQLite, static search indexes, frontend bundles, and full static sites with profile-based transforms.
- **Automate locally**: JSON output on commands, saved reports, automation runs, checkpoints, shell completions, JavaScript scripting with sandbox tiers, custom skills, skill commands, and plugins.
- **Integrate with agents**: `vulcan describe`, OpenAI tool schemas, MCP stdio/HTTP, ChatGPT-compatible OAuth/IndieAuth, tool packs, resources, prompts, and Agent Skills-compatible vault guidance.

## Quick Start

Build the CLI:

```sh
cargo build --release -p vulcan-cli --bin vulcan
```

Initialize and scan a vault:

```sh
./target/release/vulcan --vault ~/notes index init
./target/release/vulcan --vault ~/notes index scan
```

Try common workflows:

```sh
vulcan --vault ~/notes browse
vulcan --vault ~/notes search 'meeting notes'
vulcan --vault ~/notes query 'FROM "Projects" WHERE status = "active"'
vulcan --vault ~/notes note get "Projects/Alpha.md" --output json
vulcan --vault ~/notes daily today
vulcan --vault ~/notes tasks list --output json
vulcan --vault ~/notes export markdown 'tag:publish' --path public.md
vulcan --vault ~/notes site build
vulcan --vault ~/notes doctor
```

For external agent runtimes and MCP clients:

```sh
vulcan --vault ~/notes agent install --overwrite
vulcan --vault ~/notes describe --format mcp
vulcan --vault ~/notes mcp --transport stdio --tool-pack notes-read,search,status
```

## Configuration

Vulcan stores vault-local state under `.vulcan/`:

- `.vulcan/config.toml`: shared vault configuration, usually committed with the vault
- `.vulcan/config.local.toml`: device-local overrides, ignored by default
- `.vulcan/cache.db`: rebuildable SQLite cache

Use `vulcan config ...` and `vulcan help config` for the editable config surface. Vulcan can import settings from supported Obsidian plugins with `vulcan index init --import` or `vulcan config import --all`.

## Automation And Agent Surfaces

Vulcan has several automation layers with different jobs:

| Surface | Use It For |
| --- | --- |
| CLI JSON | Shell scripts, CI, direct command automation |
| `vulcan run` | One-off JavaScript scripts against the vault API |
| Skills | Agent-readable workflow instructions and references in `.agents/skills/` |
| Skill commands | Typed callable tools inside skills, exposed to CLI, MCP, `describe`, and JS |
| Plugins | Event-driven lifecycle hooks such as note-write or pre-commit checks |
| MCP | ChatGPT/Claude/Codex-style tool clients with permission profiles and tool packs |

For a private ChatGPT connector, see [docs/guide/chatgpt-mcp.md](docs/guide/chatgpt-mcp.md). The recommended setup uses HTTPS, Vulcan's embedded OAuth issuer, IndieAuth for human login, Dynamic Client Registration when useful, and a narrow permission profile.

## Documentation

- [Getting started](docs/guide/getting-started.md): first commands and conventions
- [CLI guide](docs/cli.md): command catalogue and examples
- [Filters](docs/guide/filters.md) and [query DSL](docs/guide/query-dsl.md): selection syntax
- [Scripting](docs/guide/scripting.md), [sandboxing](docs/guide/sandbox.md), and [automation surfaces](docs/guide/automation-surfaces.md)
- [Skill commands](docs/assistant/skill_commands.md) and [custom tools](docs/assistant/custom_tools.md)
- [ChatGPT MCP setup](docs/guide/chatgpt-mcp.md)
- [Static sites](docs/guide/static-sites.md)
- [Design document](docs/design_document.md): architecture and crate boundaries
- [Roadmap](docs/ROADMAP.md): implementation status and planned phases
- [Hardening](docs/hardening.md): verification matrix and boundary checks

The integrated help system mirrors much of this documentation:

```sh
vulcan help
vulcan help filters
vulcan help assistant-integration
vulcan help custom-tools
```

## Workspace Layout

| Crate | Purpose |
| --- | --- |
| `vulcan-core` | Synchronous vault semantics: parser, indexer, cache, config model, query/search/graph/task logic, permissions, optional JS/web/OAuth/vector features |
| `vulcan-app` | Reusable synchronous workflows over `vulcan-core`: note/task/template/export/site/config/plugin/tool orchestration without terminal UI |
| `vulcan-embed` | Embedding provider trait and vector store implementations |
| `vulcan-cli` | The `vulcan` binary: `clap` surface, terminal output, TUI/editor integration, MCP stdio/HTTP server, completions |

Planned crates start with `vulcan-daemon`, which will own async HTTP/WebSocket transport, multi-vault registry state, background scheduling, and daemon lifecycle. Core remains synchronous; daemon code will wrap shared workflows at the async boundary.

## Development Checks

Run these before committing:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace --no-default-features
```

Feature and boundary expectations are documented in [docs/hardening.md](docs/hardening.md). The repository also has boundary tests to keep CLI, app, core, MCP, JS, web, OAuth, and vector responsibilities from drifting back together.
