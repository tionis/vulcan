# Vulcan Implementation Roadmap

Tracking document for the phased implementation of Vulcan, a headless CLI for Obsidian vaults and plain Markdown directories.
Derived from `docs/design_document.md`. Update task status as work progresses.

**Status legend:** `[ ]` not started | `[~]` in progress | `[x]` complete | `[-]` cut/deferred

---

## Phase 1: Core indexing

**Goal:** Build the foundational data pipeline — scan a vault, parse every note, populate the SQLite cache with documents, links, headings, blocks, aliases, tags, and chunks, and provide a `doctor` command for diagnostics. This phase must be solid before anything else begins.

**Design refs:** §4 (architecture), §5 (data model), §6 (incremental indexing), §7 (chunking), §14 (vault config), §15 (schema/migration)

### 1.1 Project scaffold
- [x] Initialize Cargo workspace with three crates:
  - `vulcan-core` (lib): parser, indexer, data model, SQLite cache, file scanning, config
  - `vulcan-embed` (lib): embedding provider trait and implementations, vector store abstraction
  - `vulcan-cli` (bin): CLI binary, command handlers, output formatting
- [x] Add core dependencies to `vulcan-core`: `rusqlite` (with `bundled`), `serde`, `serde_yaml`, `serde_json`, `toml`, `pulldown-cmark` (with wikilinks + GFM), `notify`, `blake3`, `ulid`
- [x] Add dependencies to `vulcan-cli`: `clap`, `vulcan-core`
- [x] Set up `clap` CLI skeleton with global flags: `--vault <path>`, `--output <human|json>`, `--verbose`
- [x] Create `tests/fixtures/vaults/basic/` test vault with a handful of interlinked notes
- [x] Set up GitHub Actions CI: `cargo test` + `cargo clippy` + `cargo fmt --check`

### 1.2 SQLite cache foundation
- [x] Database initialization: create or open `.vulcan/cache.db` in vault root
- [x] Set `PRAGMA journal_mode = WAL`, `PRAGMA foreign_keys = ON`, `PRAGMA busy_timeout`
- [x] Implement `user_version`-based migration framework (ordered migration list, apply sequentially in transaction, refuse on downgrade)
- [x] Schema v1: `documents` table — `id` (ULID), `path` (relative to vault root), `filename`, `extension`, `content_hash`, `raw_frontmatter`, `file_size`, `file_mtime`, `parser_version`, `indexed_at`
- [x] Schema v1: `headings` table — `id`, `document_id`, `level`, `text`, `byte_offset`
- [x] Schema v1: `block_refs` table — `id`, `document_id`, `block_id_text`, `block_id_byte_offset`, `target_block_byte_start`, `target_block_byte_end` (the block ID is a standalone paragraph *after* the block it labels; store offsets for both the ID and the referenced content block)
- [x] Schema v1: `links` table — `id`, `source_document_id`, `raw_text`, `link_kind` (wikilink/markdown/embed), `display_text`, `target_path_candidate`, `target_heading`, `target_block`, `resolved_target_id` (nullable FK), `origin_context` (body/property/frontmatter), `byte_offset`
- [x] Schema v1: `aliases` table — `id`, `document_id`, `alias_text`
- [x] Schema v1: `tags` table — `id`, `document_id`, `tag_text` (normalized, no `#` prefix)
- [x] Schema v1: `chunks` table — `id`, `document_id`, `sequence_index`, `heading_path` (JSON array), `byte_offset_start`, `byte_offset_end`, `content_hash`, `chunk_strategy`, `chunk_version`
- [x] Schema v1: `diagnostics` table — `id`, `document_id` (nullable), `kind` (unresolved_link/parse_error/type_mismatch/unsupported_syntax), `message`, `detail` (JSON), `created_at`
- [x] Schema v1: `meta` table — `key`, `value` (for `schema_version`, `parser_version`, etc.)
- [x] Create indexes on: `documents(path)`, `documents(content_hash)`, `links(source_document_id)`, `links(resolved_target_id)`, `aliases(document_id)`, `aliases(alias_text)`, `tags(tag_text)`, `chunks(document_id)`
- [x] Write rebuild command: drop all rows, rescan vault from scratch
- [x] Unit tests for migration framework (apply, skip already-applied, refuse downgrade)

### 1.3 Vault discovery and file scanning
- [x] Recursive vault scan: walk directory, skip `.obsidian/`, `.vulcan/`, `.trash/`, hidden dirs, respect `.gitignore` if present
- [x] Detect file types: `.md` (notes), `.base` (Bases files), attachments (images, PDFs, etc.)
- [x] Compute content hash for each file
- [x] Incremental scan: compare `mtime` + `size` as cheap filter, verify with content hash, skip unchanged files
- [x] Insert/update `documents` rows; remove rows for deleted files
- [x] Reconciliation: on startup, diff cached document set against actual filesystem, surface deletions and additions
- [x] `scan` CLI command: trigger full or incremental scan, report counts
- [x] Unit tests for path normalization, hash computation
- [x] Integration test: scan `basic/` vault, verify document count and paths

### 1.4 Vault configuration parsing
- [x] Parse `.vulcan/config.toml`: chunking settings, link resolution defaults, link style preference, attachment folder override, embedding provider config
- [x] Create default `.vulcan/config.toml` on `vulcan init` with commented-out defaults
- [x] If `.obsidian/app.json` exists: read `useMarkdownLinks`, `newLinkFormat`, `attachmentFolderPath`, `strictLineBreaks` as fallback defaults
- [x] If `.obsidian/types.json` exists: read property type assignments to seed property catalog
- [x] Precedence: `.vulcan/config.toml` > `.obsidian/app.json` > built-in defaults
- [x] Fall back gracefully if neither `.vulcan/config.toml` nor `.obsidian/` exists (plain Markdown directory)
- [x] Emit diagnostic if a config file exists but is unparseable
- [x] Store merged config in an in-memory struct passed to parser and resolver
- [x] Unit tests for config merging, missing files, malformed files, precedence

### 1.5 Markdown parser pipeline

Module layout: `vulcan-core/src/parser/` with `mod.rs`, `options.rs`, `comment_scanner.rs`, `semantic_pass.rs`, `link_classifier.rs`, `tag_extractor.rs`, `block_ref.rs`, `types.rs`.

Public API: `parse_document(source: &str, config: &VaultConfig) -> ParsedDocument`

**Stage 0: Comment region pre-scan** (`comment_scanner.rs`)
- [x] Scan raw source bytes for `%%` pairs, record byte ranges as comment regions (`Vec<Range<usize>>`)
- [x] Handle both inline (`%%comment%%`) and multi-line (`%%\n...\n%%`) comments
- [x] Unit tests: paired comments, nested `%%`, unclosed `%%` (treat as literal), adjacent comments

**Stage 1: pulldown-cmark configuration** (`options.rs`)
- [x] Configure parser with `into_offset_iter()` and options: `ENABLE_WIKILINKS`, `ENABLE_GFM`, `ENABLE_MATH`, `ENABLE_FOOTNOTES`, `ENABLE_YAML_STYLE_METADATA_BLOCKS`

**Stage 2: Single-pass semantic processor** (`semantic_pass.rs`)

*a) Graph entity extraction (using original byte offsets):*
- [x] Link extraction: wikilinks (`[[target]]`, `[[target|display]]`), Markdown links (`[text](target)`), embeds (`![[target]]`)
- [x] For each link: capture raw text, kind, display text, target path candidate, heading/block subpath, byte offset, origin context
- [x] Link classifier (`link_classifier.rs`): split `dest_url` on `#` for heading/block subpath; detect `^` prefix for block refs; distinguish note embeds from image embeds by file extension; classify `obsidian://` URIs as external links
- [x] Heading extraction: level, text, byte offset
- [x] Block ref extraction (`block_ref.rs`): track preceding block-level element, detect standalone paragraphs matching `^[a-zA-Z0-9-]+`, associate with preceding block, record byte offsets for both the ID and the content block
- [x] Tag extraction (`tag_extractor.rs`): match `#[a-zA-Z0-9/_-]+` in `Text` events for inline tags including nested hierarchies (`#tag/subtag/deep`)
- [x] Callout classification: detect `[!type]` in blockquotes
- [x] HTML link detection: flag `<a href>` and `<img src>` in `Html`/`InlineHtml` events for `doctor` diagnostics

*b) Clean chunk text (comments and markers stripped):*
- [x] Suppress text content for events whose byte range overlaps a comment region from Stage 0
- [x] Strip `==` highlight markers from text (keep the highlighted text itself)
- [x] Accumulate clean text into chunk buffers (chunk splitting is handled by the chunking engine in §1.6)

*c) Frontmatter extraction:*
- [x] Capture raw YAML from `MetadataBlock` event, parse with `serde_yaml`, preserve raw text for lossless roundtrip
- [x] Alias extraction from frontmatter `aliases` field
- [x] Tag extraction from frontmatter `tags` field (merged with inline tags)

**ParsedDocument output type** (`types.rs`)
- [x] Define `ParsedDocument`: raw frontmatter, parsed frontmatter, headings, block refs, links, tags, aliases, chunk texts (clean), diagnostics
- [x] Define supporting types: `RawLink`, `RawHeading`, `RawBlockRef`, `RawTag`, `ChunkText`, `ParseDiagnostic`

**Unit tests**
- [x] Well-formed notes with all link variants (wikilinks, Markdown links, embeds, subpaths, display text)
- [x] Malformed frontmatter, empty files, frontmatter-only files
- [x] `%%comments%%` — verify stripped from chunk text, verify links inside comments are still extracted (with a diagnostic)
- [x] `==highlights==` — verify markers stripped, text preserved
- [x] Nested tags (`#tag/subtag/deep`)
- [x] `obsidian://` URIs classified as external
- [x] HTML links detected for diagnostics
- [x] Block refs: standalone `^id` after paragraph, list, blockquote, code block
- [x] Footnotes containing links
- [x] Callouts with internal links
- [x] Unicode content, unclosed wikilinks, edge cases

### 1.6 Chunking engine
- [x] Implement `heading` strategy (default): split at heading boundaries, sub-split at paragraph boundaries if section exceeds target size
- [x] Implement `fixed` strategy: fixed-size window with configurable overlap
- [x] Implement `paragraph` strategy: one chunk per paragraph
- [x] Respect semantic boundaries: never split mid-paragraph, mid-list-item, mid-blockquote, mid-code-block
- [x] Each chunk records: document_id, sequence index, heading path, byte offsets, content hash, strategy name, strategy version
- [x] Configuration: target chunk size (default ~4000 characters as proxy for ~1024 tokens), overlap (default 0), strategy selector
- [x] Determinism: same content + same config = same chunks (required for hash-based skip)
- [x] Unit tests: heading splits, oversized single blocks, empty docs, frontmatter-only docs, configurable size, determinism assertion

### 1.7 Indexing pipeline
- [x] Orchestrate: scan -> parse -> extract entities -> populate tables, all within batched transactions
- [x] For each changed document: re-parse, delete old derived rows (headings, blocks, links, aliases, tags, chunks), insert new rows
- [x] Content-hash gating: skip re-parse if hash unchanged
- [x] Record `parser_version` and `indexed_at` on each document row
- [x] Emit diagnostics for parse failures (malformed frontmatter, unrecognized syntax) rather than skipping silently
- [x] Integration test: index `basic/` vault, assert expected rows in all tables
- [x] Integration test: index `broken-frontmatter/` vault, assert diagnostics emitted

### 1.8 Link resolution
- [x] Implement Obsidian's link resolution algorithm:
  - Shortest-path matching (default): match by filename, prefer notes in same folder, then nearest
  - Absolute-path matching: match by full vault-relative path
  - Relative-path matching: resolve relative to source note
- [x] Respect `newLinkFormat` from vault config to select resolution strategy
- [x] Alias-aware resolution: if a link target matches an alias, resolve to that document
- [x] Populate `resolved_target_id` on `links` rows; leave null if resolution fails
- [x] Emit diagnostic for unresolved links and ambiguous targets (multiple candidates)
- [x] Unit tests: shortest-path, absolute, relative, alias, ambiguous, missing target
- [x] Integration test: `ambiguous-links/` vault, assert correct resolutions and diagnostics

### 1.9 Doctor command
- [x] `doctor` CLI command reporting:
  - Unresolved links (count + list)
  - Ambiguous link targets
  - Parse failures / malformed frontmatter
  - Stale index rows (documents in DB but not on disk)
  - Missing index rows (documents on disk but not in DB)
  - Orphan notes (no inbound or outbound links)
  - HTML links (`<a href>`, `<img src>`) in note content that are not tracked in the link graph
- [x] Support `--output json` for machine-readable diagnostics
- [x] Integration test: run doctor against `basic/` and `broken-frontmatter/` vaults

### 1.10 CLI output infrastructure
- [x] `--output json` global flag: all commands emit JSON when set
- [x] Line-delimited JSON for streamed/list output
- [x] `--fields` flag for field selection on list commands
- [x] `--limit` and `--offset` for pagination
- [x] Non-interactive detection: suppress spinners/prompts when stdout is not a TTY
- [x] Snapshot tests for JSON output structure of `scan` and `doctor`

---

## Phase 2: Safe graph operations

**Goal:** Backlink queries, outgoing link queries, and move-safe file renames with automatic link rewriting. This is the core vault-engineering value proposition.

**Depends on:** Phase 1 complete.
**Design refs:** §8 (link semantics), §4 (concurrency)

### 2.1 Graph query commands
- [x] `backlinks <note>` command: list all documents linking to the target, with link context (line, kind, display text)
- [x] `links <note>` command: list all outgoing links from a note, with resolution status
- [x] Support note identification by path, filename, or alias
- [x] `--output json` support for both commands
- [x] `--fields` support
- [x] Integration tests against `basic/` vault

### 2.2 Move-safe rewrite engine
- [x] `move <source> <dest>` command with `--dry-run` support
- [x] Filesystem operation: rename/move the file first
- [x] Identify all inbound links: query `links` table by `resolved_target_id`
- [x] For each affected source file:
  - [x] Re-parse to get fresh byte offsets
  - [x] Locate the specific link span
  - [x] Compute new link text respecting original style (wikilink vs markdown, display text, subpath)
  - [x] Apply edits back-to-front to preserve offsets
- [x] Update cache: re-index moved file + all rewritten source files
- [x] Handle edge cases: links in frontmatter properties, links with display text, links with heading/block subpaths, embed links
- [x] Respect `useMarkdownLinks` and `newLinkFormat` vault config for newly generated link text
- [x] Input validation: reject path traversal, control characters, non-existent source
- [x] Dry-run output: list all files that would be modified with before/after link text
- [x] Unit tests for rewrite logic: style preservation, subpath handling, back-to-front editing
- [x] Integration test: `move-rewrite/` vault — move a file, assert all links rewritten, run doctor to confirm zero broken links
- [x] Roundtrip test: move a file, move it back, assert original link text restored

### 2.3 Write serialization
- [x] Application-level write lock (file lock or advisory lock on the DB)
- [x] CLI commands acquire write lock before mutating; watcher queues events during lock
- [x] `busy_timeout` as backstop
- [x] Test: concurrent scan + move produces correct final state

---

## Phase 3: Search

**Goal:** Full-text search over vault content using FTS5, with snippet extraction and ranking.

**Depends on:** Phase 1 complete. Independent of Phase 2.
**Design refs:** §10 (FTS architecture)

### 3.1 FTS5 setup
- [x] Schema migration: add FTS5 virtual table in external-content mode, referencing `chunks` table
- [x] Indexed fields: chunk text content, document title, aliases, headings
- [x] Synchronization triggers or explicit rebuild to keep FTS in sync with chunks table
- [x] Rebuild FTS command (for repair)

### 3.2 Search command
- [x] `search <query>` command
- [x] FTS5 query syntax: term search, phrase search, prefix search
- [x] Snippet extraction with configurable context size
- [x] Result ranking (BM25 via FTS5 rank)
- [x] Compose with relational filters: `--tag`, `--path-prefix`, `--has-property`
- [x] `--output json` with structured results (document path, chunk id, snippet, rank)
- [x] `--limit` for result count
- [x] Integration test: index `basic/` vault, search for known terms, assert results

### 3.3 Incremental FTS maintenance
- [x] On re-index: delete FTS rows for changed chunks, insert new FTS rows
- [x] Verify FTS stays in sync after incremental updates
- [x] Idempotency test: reindex with no changes, assert FTS state unchanged

---

## Phase 4: Properties and Bases

**Goal:** Structured property querying with type awareness, and read-only evaluation of a subset of Bases files.

**Depends on:** Phase 1 complete. Independent of Phases 2 and 3.
**Design refs:** §9 (properties), §12 (Bases)

### 4.1 Property storage and projections
- [x] Schema migration: `properties` table — `document_id`, `raw_yaml` (lossless), `canonical_json` (JSONB normalized)
- [x] Schema migration: `property_values` table — `document_id`, `key`, `value_text`, `value_number`, `value_bool`, `value_date`, `value_type`, for relational projection of scalar properties
- [x] Schema migration: `property_list_items` table — `document_id`, `key`, `index`, `value_text`, for multivalue properties
- [x] Schema migration: `property_catalog` table — `key`, `observed_type`, `usage_count`, `namespace`
- [x] Populate property tables during indexing pipeline (extend Phase 1 indexer)
- [x] Type inference: use `.obsidian/types.json` when available, fall back to observed value heuristics
- [x] Handle: missing vs null vs empty string vs empty list vs invalid
- [x] Link-valued property detection and storage
- [x] Unit tests: type coercion, multivalue, null/missing/empty distinctions
- [x] Integration test: `mixed-properties/` vault, assert correct types and diagnostics for inconsistencies

### 4.2 Property query surface
- [x] `query` or `notes` command with property filters: `--where "status = done"`, `--where "tags contains foo"`
- [x] Typed comparisons: string, number, date, boolean, list membership
- [x] Sort by property value
- [x] `--output json` with property data in results
- [x] Integration tests for filter/sort operations

### 4.3 Bases parser
- [x] Parse `.base` YAML files into a validated internal model
- [x] Extract: view type, filter definitions, sort definitions, formula definitions
- [x] Separate parser from evaluator (parser is stable, evaluator matures over time)
- [x] Emit diagnostics for unsupported constructs
- [x] Unit tests with sample `.base` files

### 4.4 Bases evaluator (read-only subset)
- [x] `bases eval <file.base>` command
- [x] Evaluate supported filters against the property query layer
- [x] Evaluate supported formulas (file properties, simple property access)
- [x] Surface unsupported features as diagnostics in output, not silent omissions
- [x] `--output json` for structured results
- [x] Integration test: `bases/` vault with supported and unsupported constructs

---

## Phase 5: Vectors

**Goal:** Chunk-level embeddings via pluggable providers, nearest-neighbor search, duplicate detection, and clustering.

**Depends on:** Phase 1 (chunks table) and Phase 3 (hybrid retrieval).
**Design refs:** §7 (chunking), §11 (vectors + embedding providers)

### 5.1 Embedding provider trait
- [x] Define `EmbeddingProvider` trait: `embed_batch(chunks) -> Vec<Result<Vec<f32>, Error>>`, `metadata() -> ModelMetadata`
- [x] `ModelMetadata`: provider name, model name, dimensions, normalization, max batch size, max input tokens
- [x] `OpenAICompatibleProvider`: HTTP client for `/v1/embeddings` endpoint
  - Config: base URL, API key (optional), model name
  - Batch according to provider limits
  - Bounded concurrency across request batches
  - Exponential backoff on transient failures
- [x] Provider selection via config file or `--provider` flag
- [x] Error if no provider configured and embedding is requested
- [x] Unit tests with mock HTTP server

### 5.2 Vector storage
- [x] Schema migration: `vectors` table via `sqlite-vec` `vec0` virtual table — `chunk_id`, `provider_name`, `model_name`, `dimensions`, `embedding` (float vector)
- [x] Abstract behind `VectorStore` trait so `sqlite-vec` can be swapped later
- [x] Store provider/model metadata per row
- [x] Never mix vectors from different models in the same query
- [x] Unit tests for insert/query operations

### 5.3 Embedding pipeline
- [x] `vectors index` command: embed all un-embedded chunks, or re-embed changed chunks
- [x] Content-hash gating: skip chunks whose hash matches existing vector row
- [x] Chunked transactions: commit every N embeddings to avoid long write locks
- [x] Record failed chunks in diagnostics table; retry on next run
- [x] Progress reporting (count, rate, errors)
- [x] `--output json` for status reporting
- [x] Integration test: embed chunks from `basic/` vault against a mock provider

### 5.4 Nearest-neighbor search
- [x] `vectors neighbors <query-text>` command: embed query, find nearest chunks
- [x] `vectors neighbors --note <path>` command: find notes similar to a given note (average or per-chunk)
- [x] Return: document path, chunk id, heading path, similarity score, snippet
- [x] `--limit`, `--output json`, `--fields`
- [x] Integration test with mock provider

### 5.5 Hybrid retrieval
- [x] Combine FTS results (Phase 3) with vector similarity results
- [x] `search` command gains `--mode hybrid` flag
- [x] Reciprocal rank fusion or simple score combination for ranking
- [x] Integration test: hybrid search returns results from both FTS and vector paths

### 5.6 Duplicate detection and clustering
- [x] `vectors duplicates` command: find chunk pairs above a similarity threshold
- [x] `cluster` command: run clustering in application code (k-means or HDBSCAN), persist cluster ids and labels back to cache
- [x] Clustering is a derived artifact, not a source of truth
- [x] `--output json` for both commands

---

## Phase 6: Hardening

**Goal:** Production readiness — cross-platform file watching, fuzz testing, performance tuning, migration testing, and CLI polish.

**Depends on:** All prior phases.
**Design refs:** §4 (concurrency/watcher), §16 (performance), §19 (test strategy)

### 6.1 File watcher
- [x] `watch` command or `--watch` flag: start `notify`-based file watcher
- [x] Batch and coalesce events before acquiring write lock
- [x] On startup: reconcile watcher state against directory scan
- [x] Cross-platform testing: Linux (inotify), macOS (FSEvents), Windows (ReadDirectoryChanges)
- [x] Handle edge cases: rapid-fire saves, file replacements (some editors), large batch changes

### 6.2 Fuzz testing
- [x] `cargo-fuzz` targets for: Markdown parser, frontmatter extractor, link parser, chunker
- [x] Goal: no panics, no infinite loops, no memory safety violations on arbitrary input
- [x] Add any crash-inducing inputs as regression test cases

### 6.3 Performance tuning
- [x] Benchmark full scan + index on a large vault (1000+ notes)
- [x] Profile and optimize hot paths: parsing, link resolution, FTS sync
- [x] Tune batch transaction sizes for indexing and embedding
- [x] Verify WAL mode performance under concurrent read/write
- [x] Benchmark search latency (FTS, vector, hybrid)

### 6.4 Migration testing
- [x] Test additive migration: add a column, verify existing data preserved
- [x] Test breaking migration: change schema version past threshold, verify clean rebuild
- [x] Test downgrade detection: newer DB + older binary = clear error message

### 6.5 CLI polish
- [x] `describe` or `help --json` command for runtime schema introspection
- [x] Consistent error messages with actionable guidance
- [x] Input hardening: validate paths, reject control characters, reject path traversal
- [x] `--dry-run` on all mutating commands (move, reindex, repair)
- [x] Agent-oriented documentation: ship `AGENTS.md` or similar with invariants for automated consumers
- [x] Shell completions via `clap_complete`

### 6.6 Comprehensive integration test suite
- [x] All test vaults produce expected results end-to-end
- [x] Reindex idempotency across all vaults
- [x] Rebuild equivalence: incremental vs. from-scratch produce identical cache state
- [x] CLI JSON output snapshot tests for every command
- [x] Doctor reports zero issues on clean, well-formed vaults

---

## Phase 7: Post-v1 workflow features

**Goal:** Extend Vulcan from a high-quality indexing/query engine into a stronger vault-maintenance and automation tool, while keeping the vault as source of truth and keeping expensive work explicit.

**Depends on:** Phase 6 complete. Individual tracks can ship independently once the cache, rewrite engine, and diagnostics surface are stable.

### 7.1 Metadata and taxonomy refactors
- [x] `rename-property <old> <new>` command with `--dry-run`
- [x] `merge-tags <source> <dest>` command with safe frontmatter and body rewrites
- [x] `rename-alias <note> <old> <new>` command or alias-normalization helper
- [x] `rename-heading <note> <old> <new>` with safe inbound `#heading` link rewrites
- [x] `rename-block-ref <note> <old> <new>` with safe inbound `#^block` link rewrites
- [~] Preserve roundtrip-safe formatting when rewriting frontmatter properties and note bodies
- [x] Integration tests for property, tag, and alias refactors

### 7.2 Doctor auto-fix
- [x] `doctor --fix` mode for deterministic, safe repairs
- [x] Repair stale cache/index mismatches via targeted rebuild or repair flows
- [x] Repair missing `.vulcan/` scaffolding and other recoverable local state
- [x] Optional link-style normalization and safe unresolved-link remediation suggestions
- [x] `--dry-run` and `--output json` support for planned fixes

### 7.3 Attachment graph and asset maintenance
- [x] Index attachments as first-class assets in the cache
- [x] Track note-to-attachment embed references for images, PDFs, audio, and video
- [x] `doctor` checks for broken embeds and orphaned assets
- [x] Extend move-safe rewrites to attachment renames and moves
- [x] Optional text extraction / OCR pipeline for PDFs and images to feed search and vectors
- [x] Integration tests with attachment-heavy fixture vaults

### 7.4 Saved queries and exports
- [x] Persist saved query and report definitions in `.vulcan/`
- [x] Export `search`, `notes`, and `bases eval` results as CSV and JSONL
- [x] Non-interactive batch mode for scheduled reports and automation
- [x] Snapshot tests for saved-query and export output formats
- [x] Read-only `bases tui <file.base>` workflow for interactive inspection without sacrificing CLI parity

### 7.5 Local API and daemon mode
- [x] `serve` command exposing cache-backed local APIs (HTTP, JSON-RPC, or MCP)
- [x] Reuse the watcher and write-lock pipeline to keep served results fresh
- [x] Safe local-only defaults for bind address and authentication model
- [x] Integration tests for repeated query workloads without repeated CLI startup

### 7.6 Advanced vector operations
- [x] `vectors repair` / `vectors rebuild` commands with model migration support
- [x] Background-safe vector indexing queue with explicit operator control
- [x] Cluster labeling and summaries derived from representative chunks
- [x] Semantic recommendation surface such as `related <note>`
- [ ] Benchmarks for large-vault vector maintenance and migration flows

### 7.7 Graph analysis and reporting
- [x] `graph path <from> <to>` shortest-path query
- [x] `graph hubs`, `graph dead-ends`, `graph components`, and MOC-candidate reports
- [x] Orphan/staleness trend reporting over time
- [x] Vault analytics reports: note counts, link density, tag/property usage, stale-note summaries
- [x] `--output json` and integration tests for graph analysis commands

### 7.8 Search ergonomics
- [x] User-friendly phrase/operator query parsing on top of raw FTS syntax
- [x] `search --explain` for ranking/debug output
- [x] Fuzzy matching / typo tolerance
- [x] Richer property predicates and multi-filter composition

### 7.9 Link suggestions and bulk rewrites
- [x] Unlinked mention detection with candidate target suggestions
- [x] Optional mention-to-link conversion workflow with `--dry-run`
- [x] Bulk query-driven rewrite commands with previewable before/after output
- [x] Duplicate-title, alias, and merge-candidate suggestion reports

### 7.10 Cache maintenance and change reporting
- [x] `cache inspect`, `cache verify`, and `cache vacuum` commands
- [x] Performance and size diagnostics for cache, FTS, and vector indexes
- [x] Change reports since last scan or checkpoint for notes, links, properties, and embeddings
- [x] Integration tests for maintenance and reporting flows

### 7.11 Import, export, and automation
- [ ] Broader export surfaces for graph data, reports, and static search indexes
- [ ] CSV export support for more list/query commands beyond the initial report set
- [ ] Scriptable automation hooks for saved reports, repairs, and CI runs
- [ ] Non-interactive machine-oriented exit codes for automation workflows

---

## Dependency graph

```
Phase 1 (Core indexing)
  ├── Phase 2 (Graph operations)
  ├── Phase 3 (Search) ──── Phase 5 (Vectors)
  └── Phase 4 (Properties/Bases)
                                    ↘
                               Phase 6 (Hardening) ← all phases
                                                     ↓
                               Phase 7 (Post-v1 workflow features)
```

After Phase 1, Phases 2/3/4 can proceed in parallel.
Phase 5 requires Phase 3. Phase 6 follows all others. Phase 7 is a post-v1 backlog that can be split into independent tracks after Phase 6.
