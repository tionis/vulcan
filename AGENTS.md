# Vulcan

Headless CLI and multi-vault platform for Obsidian vaults and plain Markdown directories. Indexes notes into a SQLite cache for graph queries, full-text search, and vector search. Expanding into a daemon with REST API, sync, web wiki, and broad Obsidian plugin compatibility.

## Key documents

- `docs/design_document.md` — Full architecture and design decisions. Read this first for any non-trivial work.
- `docs/ROADMAP.md` — Phased task breakdown with checkboxes. Update task status as you complete work.
- `docs/investigations/` — Dependency research (pulldown-cmark gaps, sqlite-vec build, parser comparison).
- `references/` — Plugin source repos and documentation (obsidian-dataview, Templater, obsidian-kanban, quickadd, tasknotes, obsidian-skills). Use these as authoritative references when implementing plugin compatibility.

## Architecture

Three-layer model: vault (source of truth) → SQLite cache (rebuildable) → search indexes (derived).

Cargo workspace with crates:
- `vulcan-core` — Parser, indexer, data model, SQLite cache, file scanning, config, git integration, expression evaluator, query AST
- `vulcan-embed` — Embedding provider trait, OpenAI-compatible provider, vector store abstraction
- `vulcan-cli` — CLI binary, command handlers, output formatting, TUI (note picker, bases TUI, browse TUI)
- `vulcan-daemon` (planned) — axum-based HTTP daemon, multi-vault registry, middleware
- `vulcan-sync` (planned) — Sync backend trait and implementations

## Critical constraints

- The vault is always the source of truth. The cache must be fully rebuildable from disk.
- Store both raw and resolved link representations. Never choose one or the other.
- `.obsidian/` is optional. The tool must work on any directory of Markdown files.
- Per-vault config lives in `.vulcan/` (cache.db + config.toml). Daemon config lives in `~/.config/vulcan/`.
- `sqlite-vec` is pre-v1 — always access through the `VectorStore` trait, never directly.
- Unsupported syntax surfaces as diagnostics, never silently ignored.
- Correctness and repairability over cleverness.
- `vulcan-core` stays synchronous. Async boundaries live in the daemon layer (`spawn_blocking`).
- Shell out to `git` CLI for git operations — avoid libgit2.
- Every CLI command must work without the daemon running (direct SQLite access).

## Tech stack

- Rust edition 2021, MSRV 1.77, ULIDs for all internal identifiers
- `pulldown-cmark` 0.13+ with ENABLE_WIKILINKS, ENABLE_GFM, ENABLE_MATH, ENABLE_FOOTNOTES, ENABLE_YAML_STYLE_METADATA_BLOCKS
- `rusqlite` with `bundled` feature, WAL mode, `user_version` pragma for migrations
- `sqlite-vec` 0.1.x for vector search (statically compiled from bundled C source)
- `blake3` for content hashing, `clap` for CLI, `ratatui` + `crossterm` for TUI
- Planned: `axum` + `tokio` for daemon, `automerge` for collaborative editing

## Current implementation status

Phases 1–8 and 9.1–9.7 are complete. The codebase has:
- Full vault indexing with incremental scan, link resolution, graph queries, FTS5 search, vector search
- Bases evaluator with full expression language, formulas, and interactive TUI
- Canonical query AST shared across CLI, Bases, and API surfaces
- Query-driven mutations (`update`, `unset`) with dry-run support
- Browse TUI, note picker, auto-commit, templates, inbox, diff
- Performance optimizations (Aho-Corasick, graph caching, batch filtering)

## Next implementation phases (Phase 9.8+)

See "Phase 9 implementation order" in `docs/ROADMAP.md` for the full dependency graph. Summary:

1. **9.8 Dataview** (largest) — Inline fields, type inference, `file.*` metadata, DQL parser/evaluator, ~60 built-in functions, DataviewJS sandbox (behind `dataviewjs` feature flag)
2. **9.9 Templater** — `<% %>` template syntax, `tp.*` API modules, reuses DataviewJS sandbox for JS
3. **9.10 Tasks plugin** — `tasks` code block DSL, recurring tasks (RRULE), dependencies, custom statuses
4. **9.11 Kanban** — Board parsing, configurable date/time triggers, archive, CLI commands
5. **9.12 AI assistant** — OpenAI-compatible inference, vault tool interface, conversation persistence (gemini-scribe callout format), prompts and skills as markdown files
6. **9.13 QuickAdd** — Investigation phase for macro/capture automation
7. **9.15 TaskNotes** — Task-as-note files, NLP creation, Bases view integration (requires 4.5.1 custom source types), time tracking, pomodoro
8. **9.16 Periodic notes** — Daily/weekly/monthly note infrastructure (shared dependency for many plugins)
9. **9.17 Unified import** — `vulcan config import --all` for all plugin settings

Each plugin phase includes a settings importer reading from `.obsidian/plugins/<plugin>/data.json`.

## Key modules for new work

- `vulcan-core/src/expression/` — Bases expression evaluator (tokenizer, parser, evaluator). Foundation for Dataview expressions.
- `vulcan-core/src/bases.rs` — Bases file parsing, evaluation, view management. Extend with custom source types (4.5.1).
- `vulcan-core/src/query.rs` — Canonical query AST. DQL compiles to this.
- `vulcan-core/src/parser/` — Markdown parser pipeline. Extend for inline fields, list item extraction.
- `vulcan-core/src/properties.rs` — Property storage and typed projections.
- `vulcan-core/src/config/mod.rs` — `VaultConfig` struct. New config sections go here.
- `vulcan-cli/src/bases_tui.rs` — Bases TUI with editor handoff. Shared TUI utilities extracted here.
- `vulcan-cli/src/browse_tui.rs` — File browser TUI.
- `vulcan-cli/src/serve.rs` — Single-vault HTTP server. Will be superseded by daemon.

## Conventions

- `--output json` on all commands. Line-delimited JSON for streamed output.
- `--dry-run` on all mutating commands.
- All CLI commands must work in non-interactive mode (no TTY prompts).
- Tests alongside code. Integration tests use fixture vaults in `tests/fixtures/vaults/`.
- Schema migrations use `PRAGMA user_version`. Additive migrations preserve data; breaking migrations trigger rebuild.
- Daemon REST API response format matches CLI `--output json` format.
- Auto-commit is always opt-in and suppressible with `--no-commit`.

## Testing

- Unit tests for every module.
- Integration tests against fixture vaults: `basic/`, `ambiguous-links/`, `mixed-properties/`, `broken-frontmatter/`, `move-rewrite/`, `bases/`, `dataview/`. Additional vaults planned for `templater/`, `tasks-plugin/`, `kanban/`, `tasknotes/`, `periodic/`, `ai-sessions/`.
- Reindex idempotency: index twice, assert identical state.
- Move roundtrip: move then move back, assert original link text restored.
- JSON output snapshot tests for CLI commands.
