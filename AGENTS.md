# Vulcan

Headless CLI and multi-vault platform for Obsidian vaults and plain Markdown directories. Indexes notes into a SQLite cache for graph queries, full-text search, and vector search. Expanding into a daemon with REST API, sync, web wiki, and broad Obsidian plugin compatibility.

## Key documents

- `docs/design_document.md` — Full architecture and design decisions. Read this first for any non-trivial work.
- `docs/ROADMAP.md` — Phased task breakdown with checkboxes. Update task status as you complete work.
- `docs/investigations/` — Dependency research (pulldown-cmark gaps, sqlite-vec build, parser comparison).
- `references/` — Plugin source repos and documentation (obsidian-dataview, Templater, obsidian-kanban, quickadd, tasknotes, obsidian-skills). Use these as authoritative references when implementing plugin compatibility.
- `docs/assistant/` — AGENTS.md template for user vaults and default skills shipped with Vulcan (relevant for 9.12 and 9.18.7 work).

## Repo dogfooding

When working in this repo, prefer using Vulcan itself to inspect and edit long Markdown docs such as the roadmap and design document instead of falling back immediately to ad hoc `grep`/`sed` flows.

- Use `vulcan note outline ./docs/ROADMAP.md` to discover semantic section ids and line spans.
- Use `vulcan note get ./docs/ROADMAP.md --section vulcan-implementation-roadmap/phase-9-cli-refinements/9-15-tasknotes-compatibility-primary-task-model@1866` to read one roadmap section without reopening the whole file.
- Use `vulcan note patch ./docs/ROADMAP.md --section <section-id-from-outline> --find 'old text' --replace 'new text' --dry-run` for surgical roadmap edits, then rerun without `--dry-run` once the patch is correct.
- Prefer `note get`/`note patch` with `--section`, `--heading`, `--block-ref`, or `--lines` for targeted work on large notes. This is good dogfooding and helps keep roadmap/design-document editing aligned with the intended agent workflow.

## Architecture

Three-layer model: vault (source of truth) → SQLite cache (rebuildable) → search indexes (derived).

Cargo workspace with crates:
- `vulcan-core` — Parser, indexer, data model, SQLite cache, file scanning, config, git integration, expression evaluator, query AST, DQL, DataviewJS (rquickjs), Kanban, TaskNotes, periodic notes, refactoring
- `vulcan-app` — Reusable synchronous application workflows that compose `vulcan-core` with filesystem mutation, plugin dispatch, config edits, scan refresh, and other non-UI orchestration
- `vulcan-embed` — Embedding provider trait, OpenAI-compatible provider, vector store abstraction
- `vulcan-cli` — CLI binary, command handlers, output formatting, TUI (note picker, bases TUI, browse TUI), JS REPL, and transport/presentation adapters over shared services
- `vulcan-daemon` (planned) — axum-based HTTP daemon, multi-vault registry, middleware
- `vulcan-sync` (planned) — Sync backend trait and implementations

Contributor boundary rule: new reusable business logic must not land in `vulcan-cli` unless it is clearly CLI/TUI-only. Prefer `vulcan-core` for reusable semantics and `vulcan-app` for reusable synchronous workflow orchestration.

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
- **JS sandbox tiers** (from most to least restrictive): `strict` (pure computation only, no I/O), `fs` (adds read-only vault file access), `net` (adds `web.search()` and `web.fetch()`), `none` (unrestricted). Default is `strict`. Scripts and DataviewJS blocks inherit the configured tier; web tools require `net` or higher.

## Tech stack

- Rust edition 2021, MSRV 1.77, ULIDs for all internal identifiers
- `pulldown-cmark` 0.13+ with ENABLE_WIKILINKS, ENABLE_GFM, ENABLE_MATH, ENABLE_FOOTNOTES, ENABLE_YAML_STYLE_METADATA_BLOCKS
- `rusqlite` with `bundled` feature, WAL mode, `user_version` pragma for migrations
- `sqlite-vec` 0.1.x for vector search (statically compiled from bundled C source)
- `blake3` for content hashing, `clap` for CLI, `ratatui` + `crossterm` for TUI
- `rquickjs` for DataviewJS/Templater JS sandbox (behind `js_runtime` feature flag)
- `reqwest` for web search/fetch (blocking client, gated on `net` sandbox tier)
- **Feature flag:** `js_runtime` (default on). Disables rquickjs, DataviewJS, Templater JS execution, and `web.*` JS APIs when off. Build without it via `cargo build --no-default-features`. All JS-dependent code must be gated with `#[cfg(feature = "js_runtime")]`.
- Planned: `axum` + `tokio` for daemon, `automerge` for collaborative editing

## Current implementation status

Phases 1–8, 9.1–9.11, 9.13, 9.15–9.18 are complete. Phase 9.12 (AI assistant) and 9.19 (CLI polish) are not yet started. The codebase has:
- Full vault indexing with incremental scan, link resolution, graph queries, FTS5 search, vector search
- Bases evaluator with full expression language, formulas, and interactive TUI
- Canonical query AST shared across CLI, Bases, and API surfaces
- Query-driven mutations (`update`, `unset`) with dry-run support
- Browse TUI, note picker, auto-commit, templates, inbox, diff
- Performance optimizations (Aho-Corasick, graph caching, batch filtering)
- Dataview: inline fields, DQL parser/evaluator, DataviewJS (rquickjs sandbox), `file.*` metadata, ~60 built-in functions
- Templater: `<% %>` syntax, full `tp.*` API (native + JS modules), settings import
- Tasks plugin: query DSL, recurring tasks (RRULE), dependencies, custom statuses
- Kanban: board parsing, archive, CLI surface, settings import
- TaskNotes: task-as-note files, NLP creation, time tracking, pomodoro, reminders, Bases integration
- Periodic notes: daily/weekly/monthly with create/open/append/list, structured events, ICS export
- Unified settings import: `vulcan config import --all` with conflict detection, batch import
- CLI redesign (9.18): note CRUD (`note get/set/create/append/patch`), refactor commands, JS runtime with REPL, web tools (`web search`/`web fetch`), git ops, `describe --format mcp|openai-tools`, integrated `help` system
- QuickAdd: capture format compatibility and settings import

## Next implementation phases

See `docs/ROADMAP.md` for the full dependency graph and detailed task lists.

**9.12 Embedded AI assistant:** Full vault-native agent — OpenAI-compatible inference, tiered tool exposure (core tools always in prompt, rest via gradual discovery through `describe`/`help`), vault-aware system prompt, conversation persistence as vault notes (gemini-scribe callout format), context budgeting, prompts and skills as executable knowledge (markdown files that teach the LLM how to use Vulcan). Advanced skills can include executable JS scripts with `#!/usr/bin/env -S vulcan run --script` shebangs.

**9.12.8 Chat platform adapters:** Telegram first (internal, behind cargo feature flag), then Discord/Signal/Matrix. Chat platforms become mobile interfaces to the vault with per-user/per-platform sandboxed tool permissions.

**9.19 CLI polish:** Bug fixes, `vulcan run` improvements, shell completions, help polish, DQL completeness, missing commands, command reorg, scriptability, web search backend expansion (Exa/Tavily/Brave), settings TUI, event-driven plugin system, permission layer.

## Key modules for new work

- `vulcan-core/src/expression/` — Bases expression evaluator (tokenizer, parser, evaluator). Shared with Dataview expressions.
- `vulcan-core/src/bases.rs` — Bases file parsing, evaluation, view management.
- `vulcan-core/src/query.rs` — Canonical query AST. DQL compiles to this.
- `vulcan-core/src/dql/` — DQL parser and evaluator.
- `vulcan-core/src/parser/` — Markdown parser pipeline, inline field extraction, list item/task extraction.
- `vulcan-core/src/dataview_js.rs` — DataviewJS rquickjs sandbox, `dv.*` and `web.*` JS APIs.
- `vulcan-core/src/properties.rs` — Property storage and typed projections.
- `vulcan-core/src/config/mod.rs` — `VaultConfig` struct. New config sections go here.
- `vulcan-core/src/kanban.rs` — Kanban board parsing and evaluation.
- `vulcan-core/src/tasknotes.rs` — TaskNotes file format, NLP creation, time tracking.
- `vulcan-core/src/periodic.rs` — Periodic note discovery, resolution, and structured events.
- `vulcan-core/src/refactor.rs` — Vault-wide refactoring passes and suggestions.
- `vulcan-cli/src/commands/` — Command handler modules (one per command group).
- `vulcan-cli/src/bases_tui.rs` — Bases TUI with editor handoff.
- `vulcan-cli/src/browse_tui.rs` — File browser TUI.
- `vulcan-cli/src/js_repl.rs` — JavaScript REPL for `vulcan run --repl`.
- `vulcan-cli/src/template_engine.rs` — Templater-compatible template processing.
- `vulcan-cli/src/serve.rs` — Single-vault HTTP server. Will be superseded by daemon.

## Conventions

- `--output json` on all commands. Line-delimited JSON for streamed output.
- `--dry-run` on all mutating commands.
- All CLI commands must work in non-interactive mode (no TTY prompts).
- Tests alongside code. Integration tests use fixture vaults in `tests/fixtures/vaults/`.
- Schema migrations use `PRAGMA user_version`. Additive migrations preserve data; breaking migrations trigger rebuild.
- Daemon REST API response format matches CLI `--output json` format.
- Auto-commit is always opt-in and suppressible with `--no-commit`.

## Workflow

### Write tests for every change

Every new feature, bug fix, or behavioral change must include tests. Do not defer testing to a later commit.

- **New functions/modules:** add unit tests in the same file or a `tests` submodule.
- **New CLI commands or changed output:** add or update integration tests (often snapshot-based JSON output tests in `vulcan-cli/tests/`).
- **Parser or indexer changes:** add test cases to the relevant fixture vault or create a new fixture vault if needed.
- **Bug fixes:** add a regression test that would have caught the bug.
- **Config changes:** test both default values and explicit overrides.
- If a change cannot be meaningfully tested (e.g. pure formatting), note why in the commit message.

### Commit after each completed item

Commit each discrete feature, fix, or roadmap item as its own commit once it passes all checks. Do not batch unrelated changes into a single commit. Each commit should be a self-contained, working state.

### Before committing

**Always run before committing or submitting changes:**

```sh
cargo fmt --all                  # format all crates
cargo clippy --workspace --all-targets -- -D warnings   # lint all targets — treat warnings as errors
cargo test --workspace           # run all tests
```

If you changed a specific crate, at minimum run tests for that crate (`cargo test -p vulcan-core`). Run the full workspace test suite before committing.

Fix any formatting, lint, or test failures before committing. Do not skip these steps — CI will catch them and the resulting back-and-forth wastes time.

## Testing

- Unit tests for every module.
- Integration tests against fixture vaults: `basic/`, `ambiguous-links/`, `mixed-properties/`, `broken-frontmatter/`, `move-rewrite/`, `bases/`, `dataview/`, `attachments/`, `refactors/`, `suggestions/`, `tasknotes/`.
- Reindex idempotency: index twice, assert identical state.
- Move roundtrip: move then move back, assert original link text restored.
- JSON output snapshot tests for CLI commands.
