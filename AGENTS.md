# Vulcan

Headless CLI and multi-vault platform for Obsidian vaults and plain Markdown directories. Indexes notes into a SQLite cache for graph queries, full-text search, and vector search. Expanding into a daemon with REST API, sync integration, and web-based wiki.

## Key documents

- `docs/design_document.md` — Full architecture and design decisions. Read this first for any non-trivial work.
- `docs/ROADMAP.md` — Phased task breakdown with checkboxes. Update task status as you complete work.
- `docs/investigations/` — Dependency research (pulldown-cmark gaps, sqlite-vec build, parser comparison).

## Architecture

Three-layer model: vault (source of truth) → SQLite cache (rebuildable) → search indexes (derived).

Cargo workspace with crates:
- `vulcan-core` — Parser, indexer, data model, SQLite cache, file scanning, config, git integration
- `vulcan-embed` — Embedding provider trait, OpenAI-compatible provider, vector store abstraction
- `vulcan-cli` — CLI binary, command handlers, output formatting, TUI (note picker, bases TUI, browse TUI)
- `vulcan-daemon` (planned) — axum-based HTTP daemon, multi-vault registry, middleware
- `vulcan-sync` (planned) — Sync backend trait and implementations (obsidian-headless, git remote, passive)

## Critical constraints

- The vault is always the source of truth. The cache must be fully rebuildable from disk.
- Store both raw and resolved link representations. Never choose one or the other.
- `.obsidian/` is optional. The tool must work on any directory of Markdown files.
- Per-vault config lives in `.vulcan/` (cache.db + config.toml). Daemon config lives in `~/.config/vulcan/`.
- `sqlite-vec` is pre-v1 — always access through the `VectorStore` trait, never directly.
- Unsupported syntax surfaces as diagnostics, never silently ignored.
- Correctness and repairability over cleverness.
- `vulcan-core` stays synchronous. Async boundaries live in the daemon layer (`spawn_blocking`).
- Shell out to `git` CLI for git operations — avoid libgit2 to keep dependencies light.
- Every CLI command must work without the daemon running (direct SQLite access). The daemon is an optional acceleration/multi-vault layer.

## Tech stack

Core (existing):
- Rust edition 2021, MSRV 1.77
- ULIDs for all internal identifiers (`ulid` crate)
- `pulldown-cmark` 0.13+ with ENABLE_WIKILINKS, ENABLE_GFM, ENABLE_MATH, ENABLE_FOOTNOTES, ENABLE_YAML_STYLE_METADATA_BLOCKS
- `rusqlite` with `bundled` feature, WAL mode, `user_version` pragma for migrations
- `sqlite-vec` 0.1.x for vector search (statically compiled from bundled C source)
- `blake3` for content hashing
- `clap` for CLI
- `serde` / `serde_yaml` / `toml` for config and frontmatter
- `ratatui` + `crossterm` for TUI (note picker, bases TUI, browse TUI)

Daemon (planned):
- `axum` + `tokio` for async HTTP
- `tower-http` for middleware (CORS, compression, logging)
- `argon2` for token hashing

## Parser pipeline

The parser has a two-stage design (see design doc §8 for full detail):
1. **Pre-scan** raw source for `%%comment%%` regions (byte ranges)
2. **Single-pass semantic processor** over pulldown-cmark's event stream that simultaneously:
   - Extracts graph entities (links, headings, block refs, tags) with original byte offsets
   - Builds clean chunk text (comments stripped, `==highlight==` markers removed)
   - Extracts frontmatter from MetadataBlock events

pulldown-cmark does NOT handle: `#heading`/`#^block` subpath splitting, note-vs-image embed classification, `%%comments%%`, `==highlights==`, inline tags, `obsidian://` URIs, or HTML link detection. These are all handled in the semantic pass.

## Conventions

- `--output json` on all commands. Line-delimited JSON for streamed output.
- `--dry-run` on all mutating commands.
- All CLI commands must work in non-interactive mode (no TTY prompts).
- Tests go alongside the code they test. Integration tests use fixture vaults in `tests/fixtures/vaults/`.
- Schema migrations use `PRAGMA user_version`. Additive migrations preserve data; breaking migrations trigger rebuild.
- TUI utilities (`open_in_editor`, `with_terminal_suspended`) live in shared modules, not duplicated per TUI.
- Daemon REST API response format matches CLI `--output json` format — same report structs, same serialization.
- Auto-commit is always opt-in and suppressible with `--no-commit`.

## Key modules for new work

These existing modules are foundations that Phase 8+ builds on:

- `vulcan-cli/src/note_picker.rs` — Fuzzy note picker TUI with preview. Reuse for `edit` and extend for `browse`.
- `vulcan-cli/src/bases_tui.rs` — Full TUI with `open_in_editor()` and `with_terminal_suspended()`. Extract shared editor utilities from here.
- `vulcan-cli/src/serve.rs` — Single-vault HTTP server (hand-rolled). Will be superseded by the daemon but shows the existing API surface.
- `vulcan-core/src/vector.rs` — Multi-model vector storage with cache key namespacing. Recently refactored.
- `vulcan-core/src/config/mod.rs` — `VaultConfig` struct and `DEFAULT_CONFIG_TEMPLATE`. New config sections (`[git]`, `[inbox]`) go here.

## Testing

- Unit tests for every module.
- Integration tests against fixture vaults (basic, ambiguous-links, mixed-properties, broken-frontmatter, move-rewrite, bases).
- Reindex idempotency: index twice, assert identical state.
- Move roundtrip: move then move back, assert original link text restored.
- JSON output snapshot tests for CLI commands.
