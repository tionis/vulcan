# Design Document
## Headless Obsidian Vault CLI with SQLite Graph Cache and Native Vector Search

**Implementation brief for the engineering agent**  
Date: 19 March 2026

User-facing CLI usage, filter syntax, and examples are documented separately in `docs/cli.md`. This document focuses on architecture and design decisions.

> **Recommended direction:** Build a single-binary CLI in Rust, treat the vault as the source of truth, use SQLite as a rebuildable graph/content/property cache, and add vector search as a second derived index. Prioritize correctness of link semantics, incremental invalidation, and property normalization over feature breadth.

## Decision summary

- **Binary name:** `vulcan`
- **Primary language:** Rust (edition 2021, MSRV 1.77) — Best fit for a fast, portable, single-binary CLI with strong text processing and SQLite integration.
- **Workspace layout:** Cargo workspace with `vulcan-core` (parser, indexer, data model), `vulcan-embed` (embedding provider trait and implementations), and `vulcan-cli` (CLI binary and command handlers). Start with this structure from the beginning to keep module boundaries clean.
- **Internal identifiers:** ULIDs — sortable by creation time, compact, no hyphens. Use the `ulid` crate.
- **Local data directory:** `.vulcan/` in the vault root, containing `cache.db` (SQLite cache), `config.toml` (shared vault configuration), and optional `config.local.toml` (device-local overrides). All commands are vault-scoped; there is no cross-vault global configuration.
- **Core storage:** SQLite — Excellent embedded relational store for cache tables, FTS, metadata, and query planning.
- **Full-text search:** SQLite FTS5 — Good hybrid-search partner; external-content mode avoids duplicating large text bodies.
- **Vector search:** `sqlite-vec` behind an abstraction — Keeps the solution embedded and local while preserving the option to swap backends later.
- **Property model:** Hybrid JSON + relational projections — Preserves loose Obsidian semantics without making query performance or typing unmanageable.
- **Correctness model:** Watcher + periodic reconciliation — File watchers improve freshness but should not be treated as a sufficient source of truth.
- **Chunk sizing:** Use character count as a proxy for token count (default ~4000 characters ≈ 1024 tokens). A lightweight tokenizer may be added later for model-specific accuracy.
- **CI:** GitHub Actions (`cargo test` + `clippy` + `fmt --check`), structured for future migration to Forgejo CI.

## 1. Project context and problem statement

The goal is to build a headless CLI for Obsidian vaults that does not depend on a live desktop Obsidian instance. The tool should support graph-aware operations such as backlinks, link fixing during moves, graph walking, property and Bases-style querying, full-text search, and semantic retrieval over note content.

The official Obsidian CLI controls the desktop app and requires the Obsidian app to be running; Obsidian also documents a separate Headless client for independent operation.[1][2]

The architecture should therefore assume that all semantics must be derived from vault contents on disk rather than delegated to a running Obsidian process. This makes parsing fidelity, cache invalidation, and repairability core design concerns rather than implementation details.

## 2. Primary use cases

The first release should optimize for practical vault engineering tasks instead of perfect parity with all Obsidian behavior.

- Resolve and query the note graph: backlinks, outgoing links, embeds, orphan detection, ambiguous targets, alias resolution.
- Perform safe file moves and renames with link rewriting across body text and properties.
- Provide fast content lookup: exact search, hybrid search, semantic similarity, near-duplicate detection, clustering, topic exploration.
- Expose structured querying over note properties and file metadata.
- Parse and query a useful subset of Obsidian Bases files, which are stored as `.base` YAML files and define views, filters, and formulas.[3][4]
- Let users treat a vault as a dynamic, queryable document database with saved views, ad hoc queries, and safe edit workflows.
- Offer an implementation-friendly automation surface for scripts, agents, and shell workflows.

## 3. Non-goals for the initial implementation

Do not aim for full visual or behavioral parity with the Obsidian desktop app in version 1.

- Perfect rendering parity with every community plugin.
- Reproducing the entire interactive Bases UI in version 1.
- Supporting arbitrary plugin-defined syntax extensions during initial indexing.
- Using the cache as the authoritative source for note contents.
- Building a distributed service before the local architecture is stable.

Version 1 should be a local, rebuildable, correctness-oriented indexing and query tool.

## 4. Recommended architecture

Use a three-layer architecture.

### Layer 1: Vault source of truth
The filesystem is canonical. Markdown notes, attachments, and `.base` files remain authoritative. If an `.obsidian` directory is present, its configuration is read to improve link resolution and property typing fidelity; if absent, the tool operates with sensible defaults. This means Vulcan works on any directory of Markdown files, not only Obsidian vaults.

### Layer 2: SQLite cache
SQLite stores the derived graph, parsed metadata, chunks, search indexes, property projections, diagnostics, and operational state. This database must be fully rebuildable from disk.

### Layer 3: Search/index extensions
FTS and vector search sit on top of the cache as additional derived indexes, not as replacements for the relational model.

### Operational flow
1. Initial full scan.
2. Parse and index changed files.
3. Resolve links and graph edges.
4. Refresh full-text and vector indexes for changed chunks.
5. Start a watcher for freshness.
6. Periodically reconcile against a cheap recrawl to repair missed or reordered events.

This model keeps correctness and recovery simple: if the cache becomes stale or a parser changes, rebuild it.

Cache-backed CLI commands may also trigger an incremental scan before reading from the cache. One-shot commands should block until the scan finishes; long-lived TUIs such as `browse` may open immediately on the current cache and refresh in place when a background scan completes. This freshness policy should be configurable per vault and overridable per invocation.

### Concurrency and write serialization

SQLite in WAL mode supports concurrent readers alongside a single writer. The implementation should adopt an explicit write-serialization strategy rather than relying on SQLite's busy-wait behavior.

Recommended approach:

- Use a single writer connection for all cache mutations. Read-only queries (backlinks, search, property lookups) may use separate connections concurrently.
- CLI-initiated mutations such as `move` or `reindex` must acquire an application-level write lock before beginning their transaction. If the watcher is mid-update, the CLI command waits; if a CLI command is running, the watcher queues events and replays them after the lock is released.
- The watcher should batch file system events into coalesced changesets before acquiring the write lock, rather than taking a write lock per event.
- Vault-mutating operations (move, rename) must be atomic from the user's perspective: update the filesystem first, then update the cache within a single transaction. If the cache update fails, the filesystem change has already happened and the next reconciliation will repair the cache.
- Long-running operations such as full reindex or batch embedding should use chunked transactions (e.g., commit every N documents) to avoid holding the write lock for minutes. This means partial progress is visible and a crash mid-reindex leaves the cache in a consistent but incomplete state, which reconciliation can repair.
- Never assume two concurrent CLI invocations coordinate with each other. If a user runs `move` in one terminal while `scan` runs in another, both must serialize through the same write lock. Use SQLite's `busy_timeout` as a backstop, but prefer the application-level lock to give better error messages.

## 5. Data model overview

The implementation should use stable internal identifiers rather than paths as primary keys. Paths move; identities should survive moves.

Recommended logical entities:

- `documents`: one row per note or relevant vault file.
- `chunks`: semantic subdivisions of notes for search and embeddings.
- `links`: raw link occurrences plus resolved targets.
- `headings` and `block_refs`: explicit subtargets.
- `aliases` and `tags`: query- and resolution-friendly projections.
- `properties`: canonical property blobs plus relational projections.
- `bases`: parsed `.base` files and derived query plans or diagnostics.
- `diagnostics`: unresolved links, parse errors, type mismatches, unsupported syntax.
- `vectors`: chunk embeddings keyed by chunk identity and model identity.
- `canvas_nodes` and `canvas_edges`: structured data from `.canvas` files (JSON Canvas format). Canvas files are a distinct document type — they are JSON, not Markdown. Text node content is chunked and indexed in FTS5; file node references create link edges in the vault graph. See §5.1 and Roadmap Phase 18.

Every entity should carry enough provenance to support incremental repair: source document id, content hash, parser version, and extraction version where relevant.

### 5.1 Canvas file support (Roadmap Phase 18)

Obsidian's JSON Canvas format (`.canvas`) stores visual spatial layouts of text, file references, external links, and groups connected by edges. Vulcan treats canvas files as a distinct document type with dedicated cache tables (`canvas_nodes`, `canvas_edges`), rather than forcing them through the Markdown parsing pipeline.

Key design points:

- **Text nodes are searchable.** Each text node becomes a search chunk with `chunk_strategy = "canvas_text"`, heading path set to the canvas filename and optional group label. This means `vulcan search` finds content inside canvases.
- **File nodes create graph edges.** A canvas file node referencing a vault note is stored as a link (type `canvas_file_ref`) in the existing `links` table. This integrates canvases into backlinks, graph analytics, and doctor validation.
- **Canvas-internal edges are canvas-scoped.** Edges between canvas nodes are stored in `canvas_edges` but do not participate in the vault-level knowledge graph. They are a layout concern, not a semantic link.
- **Move/rename rewriting applies.** When a note referenced by a canvas file node is moved, the rewrite engine updates the canvas JSON `file` field, matching the existing wikilink rewrite mechanism.

See `references/obsidian-skills/skills/json-canvas/SKILL.md` for the JSON Canvas spec and `docs/ROADMAP.md` Phase 18 for the full implementation plan.

## 6. Incremental indexing and correctness

Do not rely on modification time alone. Use `mtime` and size as a cheap first-pass filter, but maintain content hashes for verification, periodic audits, and rebuild decisions.

Important rules:

- The watcher accelerates freshness but is not the sole correctness mechanism.
- Treat replace-on-save and write-in-place editors as normal; different tools save files differently.
- On startup, reconcile the watcher state against a real directory scan.
- Keep a `parser_version` and `schema_version` in the database so that code changes can trigger targeted rebuilds.
- Build idempotent indexing passes: reprocessing the same file should converge to the same state.

If a file changes:

1. Re-read the raw source.
2. Re-parse frontmatter and body.
3. Recompute headings, blocks, links, aliases, tags, and chunks.
4. Re-resolve graph edges that touch this document.
5. Refresh FTS and embeddings only for changed chunks.

## 7. Chunking strategy

Chunks are the unit of granularity for FTS indexing, embedding, and similarity search. The chunking strategy affects retrieval quality, embedding cost, and cache invalidation granularity, so it should be configurable per vault while shipping with sensible defaults.

### Default chunking rules

The default chunker should produce heading-aware, paragraph-respecting chunks:

1. Split at heading boundaries (any level). Each heading starts a new chunk. The heading text is included as the first line of its chunk.
2. Within a heading section, split at paragraph boundaries if the section exceeds a target size (default: ~1024 tokens, configurable).
3. Never split mid-paragraph, mid-list-item, mid-blockquote, or mid-code-block. If a single block exceeds the target size, keep it as one oversized chunk rather than breaking semantic units.
4. Frontmatter is not a chunk. Properties are indexed separately.
5. Each chunk carries contextual metadata: source document id, heading path (e.g., `["Section 1", "Subsection A"]`), chunk sequence index within the document, byte offset range in the source file, and content hash.

### Configurable chunking

Support vault-level chunking configuration (e.g., in a config file or CLI flags) with at least these knobs:

- **Target chunk size** (in tokens or characters). Default: 1024 tokens.
- **Overlap** (number of tokens/characters to repeat from the preceding chunk). Default: 0. Overlapping chunks can improve retrieval recall at the cost of increased embedding volume.
- **Strategy selector**: `heading` (default, heading-aware splitting), `fixed` (fixed-size window with optional overlap, ignoring structure), or `paragraph` (one chunk per paragraph, no merging).

Additional strategies can be added later without schema changes as long as the `chunks` table records which strategy and version produced each chunk.

### Stability and invalidation

Chunking must be deterministic: the same file content with the same chunking config must always produce the same chunks. This is required so that content-hash comparisons can skip re-embedding unchanged chunks. When the chunking strategy or its parameters change, all chunks for affected documents must be invalidated and regenerated.

## 8. Link semantics and move-safe rewrites

Link correctness is one of the highest-risk areas. Store both raw syntax and resolved meaning.

For every link occurrence, persist at least:

- source document id
- raw link text
- link kind (wikilink, Markdown link, embed)
- display text, if any
- target path candidate
- target heading or block subpath
- resolved target document id, if resolution succeeded
- origin context (body text, property value, frontmatter field, etc.)

This is necessary because safe rewrites during file moves depend on resolved target identity, not naive string substitution. The implementation must also account for aliases, headings, and block references. Obsidian’s internal link model supports alternative note names through aliases and subtargets such as headings and block references.[5]

### Markdown parsing and live patching strategy

For the Rust implementation, prefer `pulldown-cmark` as the canonical parser for indexing and rewrite operations. Current `pulldown-cmark` (0.13.1) explicitly supports Obsidian-style wikilinks, supports GFM blockquote tags such as `[!NOTE]`, and exposes `into_offset_iter()` so the parser can return source byte ranges together with events.[13]

Those source ranges are the key reason to prefer it as the core parser for a CLI that needs safe rewrites. The rewrite engine should operate on semantic token spans rather than regex replacement or full document re-rendering.

Enable the following `pulldown-cmark` options:

- `ENABLE_WIKILINKS` — wikilinks (`[[target]]`, `[[target|display]]`)
- `ENABLE_GFM` — tables, strikethrough, task lists
- `ENABLE_MATH` — inline (`$...$`) and display (`$$...$$`) math, preventing misparsing of `$` as regular text
- `ENABLE_FOOTNOTES` — footnotes can contain links that must be tracked in the graph
- `ENABLE_YAML_STYLE_METADATA_BLOCKS` — emits a `MetadataBlock` event for frontmatter, avoiding the need to pre-strip `---` delimiters before parsing

pulldown-cmark emits wikilinks as `Tag::Link` or `Tag::Image` events with `LinkType::WikiLink { has_pothole: bool }`, where `has_pothole` indicates whether pipe syntax was used. Heading subpaths (`#heading`), block references (`#^block-id`), and note-vs-image embed classification are not handled natively and must be addressed by a supplementary Obsidian semantic pass. See `docs/investigations/pulldown_cmark_wikilinks.md` for the full gap analysis.

### Parser pipeline architecture

The parser pipeline uses a two-stage design that preserves byte-accurate offsets for link rewriting while producing clean text for FTS indexing. The core tension is that rewriting needs offsets into the *original* source, while indexing needs text with comments stripped and markers removed. Pre-processing the source before parsing would invalidate offsets; tracking comment state through the event stream is fragile. The solution is a pre-scan.

#### Stage 0: Comment region pre-scan

Before invoking pulldown-cmark, scan the raw source bytes for `%%` pairs (Obsidian comment delimiters) and record their byte ranges as comment regions in a sorted `Vec<Range<usize>>`. This is a simple linear scan — `%%` is an unambiguous delimiter. Checking whether a byte offset falls inside a comment is then a binary search.

#### Stage 1: pulldown-cmark event stream

Parse the unmodified source with all options enabled and `into_offset_iter()` for byte ranges. Because the source is unmodified, all offsets are valid for rewriting.

#### Stage 2: Single-pass semantic processor

Walk the event stream once, maintaining a small state machine. For each event, the processor does three things simultaneously:

**a) Extract graph entities (using original byte offsets):**

- **Links:** For every `WikiLink` event, split `dest_url` on `#` to extract `(target_path, subpath)`. If the subpath starts with `^`, classify as block reference; otherwise heading reference. For `Tag::Image` with `WikiLink` link type, distinguish note embeds from image embeds by checking file extension. Classify `obsidian://` URIs as external links.
- **Block refs:** Track the preceding block-level element. When a standalone paragraph matches `^[a-zA-Z0-9-]+`, record the block ID and associate it with the preceding block's byte range. (Obsidian places block IDs as bare paragraphs *after* the block they label.)
- **Headings:** Record level, text, byte offset.
- **Tags:** Match `#[a-zA-Z0-9/_-]+` in `Text` events to support nested tag hierarchies (`#tag/subtag`).
- **HTML link detection:** Flag `<a href` and `<img src` patterns in `Html`/`InlineHtml` events for `doctor` diagnostic reporting.

**b) Build clean chunk text (with comments and markers stripped):**

- If the current event's byte range overlaps a comment region from Stage 0, suppress the text content. This prevents private `%%comment%%` content from leaking into chunks, FTS, and embeddings.
- Strip `==` highlight markers from text content (keep the highlighted text itself).
- Accumulate text into chunk buffers, splitting at heading boundaries per the chunking strategy.

**c) Extract frontmatter:**

- On `MetadataBlock` event, capture raw YAML text. Parse with `serde_yaml` for the canonical layer. Preserve the raw text for lossless roundtrip.

The public API is a single function returning a `ParsedDocument` struct that contains frontmatter, headings, block refs, links, tags, aliases, chunk texts, and diagnostics. The indexer never touches pulldown-cmark directly.

```
vulcan-core/src/parser/
    mod.rs              -- public parse_document() entry point
    options.rs          -- pulldown-cmark option configuration
    comment_scanner.rs  -- Stage 0: find %% comment regions
    semantic_pass.rs    -- Stage 2: event stream processor
    link_classifier.rs  -- dest_url splitting, subpath detection, obsidian:// handling
    tag_extractor.rs    -- inline tag regex matching
    block_ref.rs        -- block ID detection, preceding-block association
    types.rs            -- ParsedDocument, RawLink, RawHeading, RawBlockRef, RawTag, etc.
```

### Move-safe rewrite approach

- Persist both raw token text and resolved target identity.
- During a move or rename, query inbound references by resolved target document id, re-parse only the affected source files, and rewrite only the destination segment of each affected link.
- Preserve original style choices such as wikilink vs Markdown-link syntax, embed marker, display text/alias, and heading or block suffix.
- Apply edits from the end of the file toward the start so offsets remain valid while patching.

For callouts specifically, treat them as blockquotes with Obsidian semantics rather than as a wholly separate document form. Obsidian defines a callout by placing `[!type]` on the first line of a blockquote, and callouts can contain ordinary Markdown and internal links.[14]

### Parser alternatives

`comrak` is the main alternative: it also supports source positions, front matter, GitHub-style alerts, and wikilinks.[15] However, `comrak` produces a full AST that must be materialized before patching, whereas `pulldown-cmark`'s event stream allows the rewrite engine to stream through a file, collect only the spans that need editing, and patch them in place without allocating a tree for the entire document. For rewrite-heavy operations across many files, this difference matters. `comrak` remains a reasonable choice if a richer AST is needed for other analysis passes, but it is secondary for the move-safe rewrite path.

Do not use `tree-sitter-markdown` as the canonical source of truth for vault correctness. Its own README states that it is not recommended where correctness is important and positions it primarily as a source of syntactical information for editors.[16]

## 9. Property handling strategy

Properties should be treated as soft-schema data with inconsistent types. A pure EAV model will become unpleasant to query, and a pure JSON blob will become unpleasant to index. Use a hybrid model.

### Recommended three-layer property model
1. Raw layer: preserve the exact frontmatter text or an equivalent lossless representation.
2. Canonical layer: parse into a normalized internal representation.
3. Query layer: project high-value fields into relational side tables.

### Recommended distinctions
- missing vs null vs empty string vs empty list vs invalid
- scalar vs multivalue
- note properties vs file properties vs computed properties
- raw type vs normalized type

### Special-case first-class semantics for at least
- `aliases`
- `tags`
- link-valued properties
- file metadata (for example `file.name`, `file.ext`, `file.mtime`)

Obsidian documents several default properties, including `tags`, `cssclasses`, and `aliases`.[6] Bases and formulas are property-type aware, so the query layer must support typed comparisons and list operations.[3][4]

### Recommendation
- Store canonical property payloads in JSON or JSONB.
- Materialize relational projections for scalar properties, multivalue properties, and link-valued properties.
- Keep a property catalog that records observed names, namespaces, dominant types, and usage counts.

This preserves vault fidelity while still enabling performant filtering and sorting.

## 10. Full-text search architecture

Use SQLite FTS5 as the exact-search backbone. Prefer an external-content table so the FTS index can reference content stored in ordinary cache tables rather than duplicating it. SQLite documents external-content tables explicitly, but also notes that they require the application to keep the index synchronized with the content table.[7]

Recommended indexed units:

- note title / filename
- aliases
- headings
- body chunks
- selected property text

The FTS query surface should remain simple and predictable. Do not attempt to encode every Obsidian search operator in version 1. Focus on reliable term lookup, phrase search, snippets, and ranking that can be composed with relational filters.

### Post-v1 search evolution (Roadmap 9.6)

After the core FTS infrastructure is stable, the search engine evolves toward Obsidian-compatible operators and richer query syntax. The key design constraints for this evolution:

- **Single query engine, multiple surfaces.** All search parsing and execution lives in `vulcan-core/src/search.rs`. The CLI (`vulcan search`), browse TUI (Ctrl-F), the current single-vault HTTP API (`/search`), and the later daemon/web layers all call `search_vault()` with a `SearchQuery` — improvements land everywhere at once. Daemon/web search should therefore be treated as reuse of an earlier foundation contract, not as a later-phase redesign.
- **Inline operators extend the existing token stream.** New operators (`file:`, `content:`, `section:`, `line:`, `block:`, `match-case:`, `task:`, `task-todo:`, `task-done:`) are extracted during query preparation alongside the existing `tag:`, `path:`, and `has:` filters. Most translate to SQL filters or FTS5 column specifiers; scope operators (`section:`, `line:`, `block:`) require post-FTS filtering against chunk structure.
- **Post-FTS filter pipeline.** Operators that cannot be expressed in FTS5 (case-sensitive match, line/block/section co-occurrence, task filtering, regex) run as a post-filter stage after FTS hits are collected but before ranking and truncation. This keeps the FTS index simple while supporting rich query semantics.
- **Bracket property syntax `[prop:val]` shares the filter engine.** Inline property expressions are lowered to the same `FilterExpression` structs used by `--where`, ensuring identical semantics.
- **Inline regex (`/pattern/`)** bypasses FTS and runs as a content scan, optionally narrowed by co-occurring keyword terms for performance.
- **Parenthesized boolean grouping** extends the lexer and `compose_fts_query()` to emit FTS5-native parentheses.

See `docs/ROADMAP.md` §9.6 for the full operator table, implementation plan, and cross-cutting integration notes (HTTP API, indexer, explain diagnostics).

## 11. Native vector search and clustering

Vector search should be implemented as a second derived index. Do not embed vectors directly into the graph tables.

### Recommended approach
- Embed chunks, not whole notes.
- Store vectors keyed by `chunk_id` and embedding model id.
- Keep model metadata: provider, model name, dimensions, normalization, chunking version.
- Re-embed only changed chunks.
- Use hybrid retrieval: FTS candidate generation, vector similarity, optional reranking.

`sqlite-vec` is a strong embedded-first fit for this project because it stores and queries float, int8, and binary vectors in `vec0` virtual tables and supports metadata columns. However, it is explicitly pre-v1, so it should be wrapped behind a small abstraction layer rather than imported deep into the architecture.[8]

Clustering should run in application code, not inside SQLite. Persist results back into the cache only as derived artifacts such as cluster ids, labels, centroids, or neighbor tables.

### Embedding provider architecture

The embedding pipeline must be pluggable. The primary target is any OpenAI-compatible embedding endpoint, which covers OpenRouter, Ollama, and custom backend proxies. Additional providers can be added later behind the same trait boundary.

Recommended design:

- Define an `EmbeddingProvider` trait with a minimal surface: accept a batch of text chunks, return a batch of vectors (or per-chunk errors). The trait should also expose model metadata (dimensions, normalization, max batch size, max input tokens).
- Ship a default `OpenAICompatibleProvider` that speaks the `/v1/embeddings` HTTP contract. Configuration requires a base URL, an optional API key, and a model name. This single implementation covers OpenAI, OpenRouter, Ollama (`http://localhost:11434/v1/embeddings`), and any proxy that implements the same contract.
- Batch requests according to the provider's advertised limits. Use async HTTP with concurrency control (e.g., a semaphore) to avoid overwhelming local or rate-limited remote endpoints.
- Handle transient failures with exponential backoff and per-chunk error recording. A failed chunk should not block the rest of the batch; record the failure in the diagnostics table and retry on the next indexing pass.
- Store the provider name, model name, and dimensions alongside each vector row so that vectors from different models are never mixed in similarity queries.
- Support a `--provider` or config-file field to select the active provider. Default to a no-op or explicit error if no provider is configured, rather than silently skipping embedding.
- Keep the provider abstraction in its own module so that adding a future native/ONNX-based local provider does not require touching the indexing pipeline.

## 12. Bases support

Treat Bases support as read-mostly and subset-first.

Facts to incorporate:

- Bases are saved as `.base` files.
- The syntax is YAML-based.
- Bases define views, filters, and formulas.[3][4]

Implementation guidance:

- Parse `.base` files into a validated internal model.
- Start with table-oriented querying and file/property formulas that map cleanly to the cache.
- Surface unsupported features as diagnostics rather than silent misbehavior.
- Separate parsing from evaluation so the parser can remain stable while the evaluator matures.

Do not hard-wire the evaluator directly to frontmatter blobs. Bases needs access to both note properties and computed file fields.

### Query model beyond version 1

Do not make `.base` syntax the canonical query language for the whole product. Bases should remain the saved view and report layer, while the core query engine should be defined independently.

Recommended direction:

- Define a canonical internal query model or AST that captures source, filters, projections, sort, grouping, pagination, and mutation targets.
- Compile multiple frontends into that same internal model: convenience CLI flags, a compact human query DSL, stable JSON payloads for agents and automation, and `.base` files.
- Keep Bases optimized for persisted, shareable views rather than as the only ad hoc query surface.
- Build future query-driven mutation commands on top of the same model so querying and editing use one semantic layer rather than parallel implementations.

This keeps the cache schema private, avoids exposing raw SQL as a long-term product contract, and lets both humans and agents work against one stable query abstraction.

### Interactive Bases workflows

Post-v1 interactive Bases support should be layered on top of the same query and mutation engine rather than inventing a separate TUI-only model.

Recommended direction:

- Keep diagnostics available, but hide the diagnostics panel by default and make it toggleable for debugging or view-authoring work.
- Expand the detail pane into a richer inspector that shows both structured row data and a file preview.
- Support a full-screen preview mode for the selected note so users can inspect more content without leaving the TUI.
- Add editing in stages: start with note/property edits and other safe structured mutations, then consider broader note editing.
- If full in-TUI text editing remains too limited for some workflows, allow an optional handoff to an external editor while preserving the same validation and rescan path on return.
- Treat future Bases-view editing as a higher-level workflow that edits validated view models and writes them back through a serializer, rather than patching `.base` files with ad hoc string edits.

The preferred sequence is to make note and property editing solid before attempting create/delete/rename/edit flows for Bases views themselves.

## 13. Language and framework recommendations

### Primary recommendation: Rust

Why Rust:

- Strong fit for a single-binary cross-platform CLI.
- Good control over parsing, indexing, and performance-sensitive text processing.
- Excellent ergonomics for embedding SQLite through `rusqlite`.
- Mature CLI ergonomics through `clap`.
- A reasonable path to file watching and efficient concurrency.

Suggested Rust stack:

- `clap` for CLI structure (`vulcan-cli`)
- `rusqlite` with `bundled` feature for SQLite access (`vulcan-core`)
- `notify` for file watching (`vulcan-core`)
- `pulldown-cmark` with `ENABLE_WIKILINKS` and `ENABLE_GFM`, plus a small Obsidian semantic pass for callouts, embeds, and rewrite-relevant token classification (`vulcan-core`)
- `serde` / `serde_yaml` / `toml` for structured parsing (`vulcan-core`)
- `ulid` for stable internal identifiers (`vulcan-core`)
- `reqwest` for HTTP embedding provider (`vulcan-embed`)
- `sqlite-vec` loaded as an extension behind a local trait or adapter (`vulcan-embed`)
- `sha2` or `blake3` for content hashing (`vulcan-core`)

### Secondary recommendation: Go
Choose Go only if delivery speed and implementation simplicity matter more than maximum parsing control.

Do not start with TypeScript unless sharing code with an Obsidian plugin ecosystem is a hard requirement; native packaging and high-volume text processing are generally better served by Rust or Go for this tool.

## 14. Vault configuration scope

Vulcan must work on any directory of Markdown files, whether or not it is an Obsidian vault. The `.obsidian` directory and its configuration files are entirely optional. When present, they improve link resolution fidelity and property typing; when absent, the tool operates with sensible defaults and vault-local configuration in `.vulcan/config.toml`, with optional device-local overrides in `.vulcan/config.local.toml`.

### `.vulcan/config.toml` (shared vault config)

This is Vulcan's primary configuration file, stored in the `.vulcan/` directory alongside the cache database. It is intended to travel with the vault and controls shared vault-scoped settings:

- Chunking strategy, target size, overlap
- Embedding provider (base URL, API key reference, model name)
- Link resolution defaults (shortest-path, relative, or absolute) when no `.obsidian/app.json` is present
- Whether to prefer wikilink or Markdown-link syntax for generated links
- Attachment folder path override
- Automatic cache refresh policy for cache-backed commands (`[scan]`)
- Template default date/time formats for `{{date}}` / `{{time}}` (`[templates]`)

### `.vulcan/config.local.toml` (optional device-local override)

This file is loaded after `.vulcan/config.toml` and may override any Vulcan setting for one device or machine. It is intended for device-local concerns such as endpoint URLs, API key environment variable names, auto-refresh preferences, or editor-adjacent workflow tuning that should not be synced back into the shared vault config.

The default `.vulcan/.gitignore` should ignore `config.local.toml` while still tracking `config.toml`.

### Configuration precedence

When multiple configuration sources exist, precedence is:

1. `.vulcan/config.local.toml`
2. `.vulcan/config.toml`
3. `.obsidian/app.json`
4. Built-in defaults

This allows users to keep a synced shared config while still overriding any setting locally without modifying the shared file or the Obsidian configuration.

### `.obsidian` configuration files (optional, read-only)

If an `.obsidian` directory is present, the following files are read to provide Obsidian-compatible defaults. All others are ignored in version 1.

**Recognized files:**

- **`.obsidian/app.json`** — Settings that affect link resolution and file handling:
  - `useMarkdownLinks`: whether the vault prefers Markdown-style links over wikilinks (affects rewrite style preservation).
  - `newLinkFormat`: `"shortest"`, `"relative"`, or `"absolute"` — determines how Obsidian resolves ambiguous link targets and how the tool should generate new link text during rewrites.
  - `attachmentFolderPath`: where attachments are stored, needed for resolving embed targets.
  - `strictLineBreaks`: affects Markdown rendering semantics, relevant if the tool ever produces rendered output.

- **`.obsidian/types.json`** — Property type assignments (text, number, date, checkbox, etc.). Used to seed the property catalog. Without this file, the tool infers types from observed values but may produce weaker type diagnostics.
- **`.obsidian/templates.json`** — Templates core-plugin settings. Vulcan may read `dateFormat` and `timeFormat` as defaults for Obsidian-compatible template rendering when `.vulcan` does not override them, and may discover the configured `folder` as an additional template source alongside `.vulcan/templates/`.

Template insertion into an existing note is a vault mutation, not a cache rewrite. The inserted template is first rendered against the target note context, then any template frontmatter is merged into the target note frontmatter by adding missing keys, preserving existing scalar values, and union-merging list properties such as `tags`.

**Low priority but useful:**

- **`.obsidian/bookmarks.json`** — Bookmarked notes, searches, and graphs. Useful for diagnostics and reporting.
- **`.obsidian/graph.json`** — Graph view display settings. Not needed for correctness.

**Explicitly ignored:**

- Community plugin configuration directories (`.obsidian/plugins/*/`). Parsing plugin-specific data is a non-goal for v1 (see §3).
- Theme and appearance files (`.obsidian/themes/`, `appearance.json`).
- Workspace and layout files (`workspace.json`, `workspaces.json`).
- Hotkey configuration (`hotkeys.json`).

### Default behavior without `.obsidian`

When no `.obsidian` directory exists and neither `.vulcan/config.toml` nor `.vulcan/config.local.toml` has explicit overrides:

- Link resolution: shortest-path matching (Obsidian's default)
- Link style: wikilinks
- Attachment folder: vault root
- Property types: inferred from observed values
- Strict line breaks: disabled
- Automatic cache refresh: blocking before one-shot cache-backed commands, background on `browse`

## 15. Operational and schema recommendations

### Schema guidance
- Use stable ids internally; never rely on path text as the sole join key.
- Record `schema_version`, `parser_version`, `embedding_version`, and `extraction_version`.
- Keep enough diagnostics to explain cache state to the user.
- Build explicit repair commands such as `scan`, `reindex`, `verify`, and `doctor`.
- Treat the database as disposable; never require the user to preserve it across incompatible versions.

### Schema migration strategy

Use SQLite's `user_version` pragma to track the database schema version. On startup, read `PRAGMA user_version` and compare it to the application's expected schema version.

- **Additive migrations** (new tables, new columns with defaults, new indexes): apply a forward migration function and increment `user_version`. These are lightweight and preserve existing data, including expensive artifacts like embeddings.
- **Breaking migrations** (column type changes, table restructuring, semantic changes to existing columns): drop and rebuild the cache from the vault. Since the cache is fully derived from disk, a rebuild is always correct and avoids the complexity of data-transforming migrations.
- Migration functions should be registered in an ordered list keyed by version number. On startup, apply all migrations between the current `user_version` and the target version sequentially within a transaction.
- If `user_version` is higher than the application expects (downgrade scenario), refuse to open the database and advise the user to rebuild.
- Always set `PRAGMA user_version = N` at the end of a successful migration transaction.

### CLI UX for humans and agents
The CLI should be designed for both direct human use and reliable agent invocation. These are related but not identical goals: human UX benefits from discoverability and convenience, while agent UX benefits from predictability, machine readability, and strong input validation. This section follows the same core framing argued by Justin Poehnelt: human DX and agent DX are orthogonal enough that the raw, predictable, machine-oriented path must be designed deliberately rather than treated as an afterthought.[12]

Recommended guidance:

- Keep a raw structured-input path as a first-class interface for every complex operation. Convenience flags are useful for humans, but nested actions such as Bases evaluation, structured moves, batch rewrites, or vector indexing options should also accept a single JSON payload that maps directly to the internal request model.
- Make machine-readable output a stable product surface, not a debug mode. Support `--output json` on all commands, and prefer line-delimited JSON for streamed or paginated output so agents can process results incrementally.
- Make the CLI self-describing at runtime. Add a command such as `describe`, `schema`, or `help --json` so an agent can inspect accepted parameters, payload shape, defaults, mutability, and output schema without relying on stale external documentation.
- Keep human-facing defaults pleasant, but make non-interactive behavior deterministic. Avoid prose-heavy output, spinner-only progress, or prompts that appear unexpectedly when stdout is not a TTY.
- Treat interactive pickers as TTY-only conveniences, not required control flow. Missing or ambiguous note-like arguments may trigger a built-in fuzzy selector with preview when the session is interactive, but the same command must fail deterministically in non-interactive mode and when `--output json` is active.
- Expose response-shaping controls for context discipline. Commands that list notes, links, diagnostics, or search hits should support field selection, limits, and streaming pagination so agents do not need to ingest large blobs of irrelevant text.
- Treat all agent-provided input as untrusted. Validate and reject path traversal, control characters, embedded query fragments in identifiers, malformed percent-encoding, and other inputs that indicate hallucinated or adversarial arguments.
- Add `--dry-run` anywhere a command mutates the vault or cache. Move, rename, rewrite, delete, repair, and migration-style operations should be previewable before they execute.
- Separate trusted metadata from untrusted note content in outputs. When returning note bodies, snippets, or search results to an agent, clearly distinguish raw vault content from CLI-generated diagnostics so prompt-like text inside notes is not confused for instructions from the tool itself.
- Ship agent-oriented context alongside normal `--help`. A `CONTEXT.md`, `AGENTS.md`, or skill files should document invariants such as “prefer field selection,” “use dry-run before mutations,” and “confirm destructive actions.”

### Recommended commands
- `scan`
- `search`
- `backlinks`
- `links`
- `move`
- `doctor`
- `bases eval`
- `vectors index`
- `vectors neighbors`
- `cluster`

The `doctor` command should be treated as a first-class product feature, not an afterthought. It should report unresolved links, ambiguous aliases, parse failures, stale index rows, type inconsistencies, and unsupported Bases constructs.

## 16. Performance considerations

Important performance principles:

- Parse only changed files, but make full rebuilds cheap and reliable.
- Chunk lazily but deterministically.
- Use batch transactions for indexing.
- Avoid over-normalizing cold data.
- Materialize only the projections that materially improve query plans.
- Keep link resolution and property coercion deterministic so cache churn stays low.

JSONB can be attractive for canonical property storage in SQLite because current SQLite versions support storing JSON in a binary form that avoids repeated parse/render overhead and usually consumes slightly less space.[9]

Be conservative with generated columns and expression indexes. They are useful for stable scalar derivations, but they are not a substitute for explicit side tables when dealing with array membership, multivalue properties, or other many-to-one indexing problems. SQLite’s JSON functions include table-valued functions such as `json_each` / `json_tree`, but these are not a good foundation for every hot-path property query.[9][10][11]

## 17. Known limitations and design constraints

The implementation agent should explicitly accept these constraints:

- Perfect parity with all Obsidian plugins is not feasible.
- Some vault semantics will remain app-specific or plugin-specific.
- Property types are inconsistent across real vaults and must be handled leniently.
- `sqlite-vec` may change before v1, so backend isolation is mandatory.
- FTS and vector search require careful synchronization; both are derived indexes, not source data.
- File system notifications are imperfect and platform-dependent, so reconciliation scans are not optional.

The product should fail loud, explain clearly, and repair cheaply.

## 18. Recommended phased delivery plan

Phases are listed in recommended order. Dependency edges are noted explicitly so that parallelizable work is visible.

### Phase 1: Core indexing
- File discovery, document table, frontmatter parsing, headings, block refs, links, aliases, tags.
- SQLite cache with `user_version`-based migration, rebuild path, diagnostics, and `doctor` command.
- `.obsidian/app.json` and `.obsidian/types.json` parsing.
- **No dependencies.** This is the foundation.

### Phase 2: Safe graph operations
- Backlinks, unresolved-link reporting, alias-aware resolution, move-safe rewrites.
- **Depends on:** Phase 1 (requires document table, link table, and alias resolution).

### Phase 3: Search
- FTS5 indexing, snippets, hybrid retrieval scaffolding.
- **Depends on:** Phase 1 (requires chunks and document table).
- **Independent of:** Phase 2. Can be developed in parallel with Phase 2.

### Phase 4: Properties and Bases
- Canonical property storage, relational projections, typed filtering, read-only subset of Bases evaluation.
- **Depends on:** Phase 1 (requires document table and frontmatter parsing).
- **Independent of:** Phases 2 and 3. Can be developed in parallel with either.

### Phase 5: Vectors
- Chunking pipeline, pluggable embedding providers, `sqlite-vec` integration, nearest-neighbor search, duplicate detection, clustering.
- **Depends on:** Phase 1 (requires chunks table) and Phase 3 (hybrid retrieval combines FTS and vector results).
- **Independent of:** Phases 2 and 4.

### Phase 6: Hardening
- Cross-platform watcher behavior, parser fuzzing, migration testing, performance tuning, CLI polish.
- **Depends on:** All prior phases. This is integration and stabilization work.

### Parallelism summary
After Phase 1 is complete, Phases 2, 3, and 4 can proceed in parallel. Phase 5 requires Phase 3. Phase 6 follows all others.

### Phases 7–18

Post-v1 phases are tracked in `docs/ROADMAP.md` and include:

- **Phase 7:** Post-v1 workflow features (move/rename variants, suggest, saved reports, link-mentions, automation)
- **Phase 8:** Performance optimizations
- **Phase 9:** CLI refinements (edit, browse TUI, auto-commit, additional commands, advanced search operators, enhanced templates)
- **Phase 10:** Multi-vault daemon with REST API
- **Phase 11:** Git auto-versioning at the daemon level
- **Phase 12:** Sync integration
- **Phase 13:** WebUI — admin panel and vault browser
- **Phase 14:** WebUI — note editor with Automerge CRDT sessions
- **Phase 15:** Extensibility and integrations (webhooks, Telegram, custom endpoints)
- **Phase 16:** Wiki mode with live collaborative editing
- **Phase 17:** User management, group-based ACLs, document-level secrets, share links
- **Phase 18:** Canvas support (parsing, indexing, CLI, WebUI rendering, interactive editor)

The design decisions in this document (three-layer architecture, cache as derived index, vault as source of truth, provider abstraction, parser pipeline) are load-bearing for all later phases. See the roadmap for dependency edges and implementation details.

## 19. Test strategy

Correctness is a core design goal, so the test strategy must be comprehensive and built alongside the implementation, not bolted on afterward.

### Unit tests

Every module should carry unit tests for its core logic:

- **Parser tests**: Frontmatter extraction, heading detection, block ref extraction, link parsing (wikilinks, Markdown links, embeds, aliased links, links with heading/block subpaths), callout recognition, and property type coercion. Test with well-formed input, malformed input, edge cases (empty frontmatter, unclosed wikilinks, nested blockquotes), and Unicode content.
- **Link resolution tests**: Shortest-path matching, absolute-path matching, ambiguous targets, alias-based resolution, resolution failures. These should test the resolver in isolation against a mock document index.
- **Chunking tests**: Verify that the same input always produces the same chunks (determinism). Test heading-boundary splitting, oversized blocks, empty documents, documents with only frontmatter, and configurable chunk size.
- **Property normalization tests**: Type inference, null/missing/empty distinctions, multivalue handling, link-valued property detection.

### Integration tests with test vaults

Maintain a set of test vaults in the repository (e.g., `tests/fixtures/vaults/`) that exercise specific scenarios:

- **`basic/`**: A small vault with a handful of interlinked notes, aliases, tags, and properties. Used as the baseline for graph correctness.
- **`ambiguous-links/`**: Notes with duplicate names in different folders, testing shortest-path resolution and ambiguity diagnostics.
- **`mixed-properties/`**: Notes with inconsistent property types for the same key (e.g., `status` as both a string and a list), testing lenient property handling.
- **`broken-frontmatter/`**: Notes with malformed YAML, unclosed delimiters, and mixed indentation, testing parser resilience and diagnostic output.
- **`move-rewrite/`**: A vault structured to test move-safe rewrites — after running a `move` operation, assert that all inbound links are rewritten correctly and no links are broken.
- **`bases/`**: A vault with `.base` files exercising supported filter and formula constructs, plus unsupported constructs that should produce diagnostics.

Integration tests should run the full indexing pipeline against these vaults and assert on the resulting database state: row counts, resolved link targets, diagnostic entries, property types, etc.

### Roundtrip and idempotency tests

- **Reindex idempotency**: Index a vault, then re-index it without changes. Assert that the database state is identical (no spurious updates, no changed content hashes).
- **Move roundtrip**: Move a file, then move it back. Assert that all links are restored to their original text.
- **Rebuild equivalence**: Build the cache from scratch, then build it incrementally from a partially-stale state. Assert that the final states are equivalent.

### Regression tests

When a bug is found, add a minimal test vault or test case that reproduces it before fixing. This prevents regressions and documents edge cases that the design did not anticipate.

### CLI output tests

For commands that support `--output json`, add snapshot or assertion-based tests that verify the JSON structure is stable. This is particularly important for agent consumers who depend on the output contract.

### Fuzz testing (Phase 6)

During hardening, apply fuzz testing to the Markdown parser and frontmatter extractor. The goal is to ensure that no input causes a panic, infinite loop, or memory safety violation. Use `cargo-fuzz` or `arbitrary`-based property testing.

## 20. Hard requirements for the implementation agent

The implementation agent should treat the following as mandatory:

- The vault remains the source of truth.
- The cache must be fully rebuildable.
- Raw and resolved link representations must both be stored.
- Properties must preserve raw fidelity and typed queryability.
- Vector search must remain backend-abstracted.
- The system must provide a clear repair and diagnostics path.
- The CLI must provide deterministic machine-readable input/output paths in addition to human-friendly ergonomics.
- Unsupported syntax must surface as diagnostics rather than being silently ignored.
- Correctness and repairability take priority over cleverness.

## Implementation checklist

- **Stable internal ids independent of paths** — **P0** — Required for rename safety and incremental repair.
- **Lossless raw frontmatter preservation** — **P0** — Needed for safe rewrites and diagnostic fidelity.
- **Raw + resolved link storage** — **P0** — Do not choose one or the other.
- **Periodic reconciliation scan** — **P0** — Watcher-only designs are brittle.
- **Stable `--output json` support across commands** — **P1** — Required for agent-safe automation and scripting.
- **Runtime schema/describe command** — **P1** — Lets agents inspect command contracts without external docs.
- **Dry-run for mutating operations** — **P1** — Preview before move, rewrite, delete, repair, or migration actions.
- **Input hardening for path/id/control-character failures** — **P1** — Assume hallucinated or adversarial arguments.
- **Field selection and streamed pagination** — **P1** — Protects context windows and improves composability.
- **Property catalog with observed types** — **P1** — Useful for query planning and diagnostics.
- **Chunk-level embeddings** — **P1** — Whole-note vectors are too coarse for many use cases.
- **Backend abstraction for vector store** — **P1** — Necessary because `sqlite-vec` is still pre-v1.
- **Doctor/verify command** — **P1** — Operational quality feature, not optional tooling.

## References

[1] Obsidian Help: CLI — official note that the Obsidian app must be running.  
[2] Obsidian Help: Headless / Headless Sync — official standalone and sync-focused headless documentation.  
[3] Obsidian Help: Introduction to Bases.  
[4] Obsidian Help: Bases syntax — `.base` files, YAML syntax, views, filters, formulas.  
[5] Obsidian Help: Aliases.  
[6] Obsidian Help: Properties — default properties such as `tags`, `cssclasses`, `aliases`.  
[7] SQLite Documentation: FTS5 external-content tables.  
[8] sqlite-vec documentation and repository — `vec0` tables, vector types, metadata columns, pre-v1 status.  
[9] SQLite Documentation: JSON functions and JSONB.  
[10] SQLite Documentation: generated columns.  
[11] SQLite Documentation / forum guidance relevant to many-to-one indexing with JSON arrays and table-valued functions.
[12] Justin Poehnelt: You Need to Rewrite Your CLI for AI Agents — https://justin.poehnelt.com/posts/rewrite-your-cli-for-ai-agents/
[13] pulldown-cmark documentation — `Options::ENABLE_WIKILINKS`, `Options::ENABLE_GFM`, and `Parser::into_offset_iter`.
[14] Obsidian Help: Callouts and Internal links.
[15] comrak documentation — extensions for source positions, front matter, alerts, and wikilinks.
[16] tree-sitter-markdown README — correctness limitations and editor-oriented positioning.

This document is intentionally opinionated. It is optimized to keep the first implementation correct, rebuildable, and extensible rather than feature-maximal.
