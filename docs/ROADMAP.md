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
- [x] Preserve roundtrip-safe formatting when rewriting frontmatter properties and note bodies
  Current gap: rewrites are semantically correct, but formatting can still be normalized in ways that users notice.
  Required scope: preserve unrelated frontmatter ordering, comments, quoting style, list indentation/flow style where possible, and avoid unnecessary body-text churn outside the targeted edit.
  Acceptance target: moving or renaming one property/link should produce a minimal diff that is stable across repeated runs.
  Suggested implementation direction: operate on parsed spans with surgical replacements rather than serializing whole frontmatter blocks whenever feasible.
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
- [x] Benchmarks for large-vault vector maintenance and migration flows

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
- [x] Broader export surfaces for graph data, reports, and static search indexes
- [x] CSV export support for more list/query commands beyond the initial report set
- [x] Scriptable automation hooks for saved reports, repairs, and CI runs
- [x] Non-interactive machine-oriented exit codes for automation workflows

### 7.12 Query ergonomics and interactive workflows
- [x] Define a canonical query AST shared by `notes`, `search`, `bases`, saved reports, and serve/API handlers
  Current gap: query semantics are still split across `NoteQuery`, `SearchQuery`, Bases evaluation, and serve handlers.
  Required scope: source selection, typed predicates, projection/field selection, sort, grouping, pagination, and mutation targets.
  Constraint: do not expose raw SQLite schema or SQL as the long-term public contract.
- [x] Add a compact human query DSL for ad hoc vault querying without exposing raw SQL
  Recommended first surface: `from notes where ... select ... order by ... limit ...`.
  Requirement: compile into the canonical AST rather than adding a parallel execution path.
- [x] Add stable JSON query payloads for agents and automation that map directly to the internal query model
  Requirement: machine input must round-trip cleanly with the AST and remain valid in non-interactive mode.
  Follow-up: extend `describe` or add `help --json` coverage for the JSON query model and supported operators.
- [x] Add query-driven mutation workflows on top of the same model instead of overloading `.base` files as the write API
  Recommended first commands: `update`, `unset`, and targeted list/tag edits.
  Constraint: always support `--dry-run`, acquire the write lock, reuse the existing refactor/mutation pipeline, and rescan incrementally after apply.
- [x] Add a TTY-only fuzzy selector and disambiguation UI for missing or ambiguous note arguments
  Current shipped baseline: picker exists for `links`, `backlinks`, `related`, `vectors related`, and note-backed `vectors neighbors`.
  Remaining scope: cover the remaining note-identifier workflows such as `graph path`, `rename-alias`, `rename-heading`, `rename-block-ref`, `suggest mentions`, and similar single-note commands where interactive selection is sensible.
  Constraint: keep the picker built-in; do not require an external `fzf` binary.
- [x] Never auto-prompt in non-interactive mode or when `--output json` is active
- [x] Expand `bases tui <file.base>` beyond read-only inspection into a richer interactive workflow
- [x] Hide the Bases TUI diagnostics panel by default and make it toggleable for debugging or view-authoring work
- [x] Extend the detail pane to show both structured row details and a file preview
- [x] Add a full-screen preview mode for the selected note
- [x] Add note/property editing in the TUI through the same validated mutation engine used by CLI commands
- [x] Add an optional external-editor handoff for note and `.base` editing from the TUI
- [x] Add future Bases view-management workflows: create, delete, rename, and edit views with validation and live result preview
  Requirement: operate on a parsed/validated view model and write back through a serializer; do not patch `.base` files with ad hoc string replacements.
  Recommended first scope: create/delete/rename view, edit columns, sort, filters, and group-by.
  Constraint: preview the resulting row set and diagnostics before save.

#### 7.12 Current implementation baseline
- All items in 7.12 are now complete.
- Canonical `QueryAst` is shared by the `vulcan query` command with DSL and JSON input modes.
- `vulcan update` and `vulcan unset` provide query-driven property mutations with `--dry-run` and JSON output.
- The interactive note picker covers all single-note commands: `graph path`, `rename-alias`, `rename-heading`, `rename-block-ref`, and `suggest mentions`.
- Bases view management: `bases view-add`, `view-delete`, `view-rename`, `view-edit` operate on a parsed/validated model and write back through a proper round-trip serializer.

#### 7.12 Recommended implementation order
1. Introduce the canonical query AST and adapter layer without changing user-facing behavior yet.
2. Port existing `notes`, Bases evaluation, saved reports, and serve/API handlers onto the AST and prove result equivalence with tests.
3. Add JSON query payload support and schema/describe output so agents have a stable contract.
4. Add the human DSL on top of the AST once the execution model is shared.
5. Add query-driven mutation commands that reuse the same AST plus the existing write-safe refactor pipeline.
6. Expand picker coverage across the remaining note-identifier commands.
7. Finish Bases view-management on top of the same parsed model and serializer.

#### 7.12 Suggested file ownership for the next agent
- Core query model: likely a new module in `vulcan-core/src/` plus adapters in `properties.rs`, `bases.rs`, `saved_queries.rs`, and CLI-side serve wiring in `vulcan-cli/src/serve.rs`.
- Interactive picker expansion: `vulcan-cli/src/note_picker.rs`, `vulcan-cli/src/cli.rs`, and `vulcan-cli/src/lib.rs`.
- Bases view editing: `vulcan-core/src/bases.rs` for parsed model + serializer support and `vulcan-cli/src/bases_tui.rs` for the interactive workflow.
- Query-driven mutation commands: `vulcan-core/src/refactor.rs` or a sibling mutation module, then CLI wiring in `vulcan-cli/src/cli.rs` and `vulcan-cli/src/lib.rs`.

#### 7.12 Acceptance expectations
- Existing `notes`, `search`, `bases eval`, saved reports, and serve/API behavior must remain stable while being ported to the shared AST.
- Interactive features must stay optional conveniences only; every command still needs a deterministic non-interactive path.
- New mutations must preserve current write-lock, dry-run, and incremental-rescan guarantees.
- Add unit tests for AST parsing/compilation and integration tests proving equivalent results across flags, DSL, JSON, and saved/Bases execution where applicable.
- Update CLI snapshots and roadmap status with each shipped sub-batch rather than waiting for the whole track to finish.

---

## Phase 8: Performance Optimization

**Goal:** Systematically address algorithmic and database bottlenecks across the application. Phase 6.3 tuned the scan/index hot path; this phase targets the remaining query, suggestion, graph, and search operations that degrade on large vaults (10k+ notes).

**Depends on:** Phase 7 complete. Independent of Phase 9 (CLI refinements) — can be developed in parallel.

**Baseline:** On a 13,389-file vault, scan performance was improved from ~300s to ~30s (10x) in Phase 6.3 via parallel file preparation, prepared statement caching, FTS trigger deferral, SQLite pragmas, and indexed link resolution. The improvements below target other commands.

### 8.1 Aho-Corasick mention detection

Replace the per-candidate string search in `suggest_mentions` / `link-mentions` with a single-pass multi-pattern automaton.

**Current bottleneck:** `find_note_mentions()` in `vulcan-core/src/suggestions.rs` iterates every `MentionCandidate` and calls `source.match_indices(&candidate.name)` for each — O(C × N) where C = candidate count (note names + aliases, ~13k for a large vault) and N = file content length. This runs per file being analyzed.

**Implementation:**
- [ ] Add `aho-corasick` crate to `vulcan-core/Cargo.toml` (already a transitive dep via `regex`; making it direct)
- [ ] In `suggest_mentions()`, build an `AhoCorasick` automaton from all candidate names (once, before iterating files)
- [ ] Replace the inner `for candidate in candidates { source.match_indices(...) }` loop in `find_note_mentions()` with a single `automaton.find_overlapping_iter(source)` pass
- [ ] Map each match back to its `MentionCandidate` via the pattern index returned by Aho-Corasick
- [ ] Preserve existing filtering: `ranges_intersect(blocked, ...)`, `ranges_intersect(&occupied, ...)`, `is_word_boundary()` checks remain unchanged — they operate on match positions regardless of how matches were found
- [ ] The `link_mentions` command uses the same `suggest_mentions` path, so it benefits automatically
- [ ] Unit tests: existing `suggest_mentions` tests must produce identical results; add a benchmark test with 1000+ candidates

**Expected improvement:** O(C × N) → O(N) per file (Aho-Corasick is linear in input length regardless of pattern count). For 13k candidates this is potentially 1000x faster per file.

**Files:** `vulcan-core/src/suggestions.rs`, `vulcan-core/Cargo.toml`

### 8.2 Duplicate/merge candidate optimization

Reduce the O(N²) pairwise Levenshtein comparison in `suggest_duplicates`.

**Current bottleneck:** `merge_candidates()` in `vulcan-core/src/suggestions.rs` compares every pair of `NoteIdentity` filenames with a custom Levenshtein implementation (lines 857–875, Wagner-Fischer). For 13k notes this is ~90M comparisons, each involving string lowercasing and O(len₁ × len₂) dynamic programming.

**Implementation:**
- [ ] Pre-compute lowercased filenames once, outside the comparison loop (currently re-lowercased per pair)
- [ ] Filter candidate pairs by filename length: Levenshtein distance ≤ 1 requires `|len₁ - len₂| ≤ 1`, so skip pairs where lengths differ by more than the threshold
- [ ] Group filenames by length into buckets; only compare within same-length and adjacent-length buckets
- [ ] Consider a BK-tree or sorted-prefix approach for further pruning if length filtering alone is insufficient
- [ ] The scoring thresholds (exact match = 1.0, alias collision = 0.95, similar title = 0.8) and distance threshold (> 1 = skip) remain unchanged
- [ ] Unit tests: existing `suggest_duplicates` tests must produce identical results

**Expected improvement:** Length filtering alone reduces comparisons from O(N²) to roughly O(N × B) where B = average bucket size. For natural filename distributions this is typically 10–100x fewer comparisons.

**Files:** `vulcan-core/src/suggestions.rs`

### 8.3 Graph query caching

Eliminate redundant link scans across graph operations by caching the adjacency data.

**Current bottleneck:** `note_link_counts()` in `vulcan-core/src/graph.rs` runs a full `SELECT ... FROM links JOIN documents` to build a HashMap of (inbound, outbound) counts. This is called by `query_graph_analytics()`, `query_graph_hubs()`, `query_graph_dead_ends()`, and `query_graph_moc_candidates()` — each independently. When a user runs `graph analytics` the query is called once, but the same SQL pattern is repeated across commands with no shared cache.

**Implementation:**
- [ ] Extract adjacency loading into a `GraphAdjacency` struct that holds both the `HashMap<String, (usize, usize)>` counts and the raw edge list
- [ ] `GraphAdjacency::load(connection)` runs the link query once and provides methods: `inbound_count()`, `outbound_count()`, `is_orphan()`, `hubs(min_degree)`, etc.
- [ ] Refactor `query_graph_analytics()`, `query_graph_hubs()`, `query_graph_dead_ends()`, `query_graph_moc_candidates()` to accept `&GraphAdjacency` instead of re-querying
- [ ] For CLI dispatch: load `GraphAdjacency` once per command invocation and pass it through
- [ ] Also refactor `load_indexed_notes()` to return a shared `IndexedNoteSet` that can be reused across graph operations in the same invocation
- [ ] `resolve_note_identifier()` currently does a linear scan over `&[IndexedNote]` with sequential predicate matching (path → filename → alias). Build a HashMap index on first call, similar to the `ResolverIndex` pattern already used in `resolver.rs`

**Expected improvement:** Graph commands that internally compute multiple metrics go from N link-query round trips to 1. For `graph analytics` on a large vault this saves a full table scan.

**Files:** `vulcan-core/src/graph.rs`

### 8.4 Missing database indexes

Add indexes for columns that appear in WHERE/JOIN clauses across many queries but currently lack coverage.

**Current gap:** The schema in `vulcan-core/src/cache/schema.rs` has no index on `documents(extension)` despite nearly every graph, search, property, and doctor query filtering on `WHERE extension = 'md'`. Similarly, `tags(document_id)` has no index despite DELETE/JOIN operations keyed on it.

**Implementation:**
- [ ] Add a new schema migration (`apply_schema_v9`) that creates:
  - `CREATE INDEX IF NOT EXISTS idx_documents_extension ON documents(extension)` — used by graph.rs, search.rs, doctor.rs, properties.rs, suggestions.rs
  - `CREATE INDEX IF NOT EXISTS idx_tags_document_id ON tags(document_id)` — used by scan.rs (DELETE), search.rs (filter), graph.rs (identity loading)
  - `CREATE INDEX IF NOT EXISTS idx_headings_document_id ON headings(document_id)` — used by scan.rs (DELETE), search.rs (heading path lookups)
  - `CREATE INDEX IF NOT EXISTS idx_block_refs_document_id ON block_refs(document_id)` — used by scan.rs (DELETE)
  - `CREATE INDEX IF NOT EXISTS idx_links_source_document_id_resolved ON links(source_document_id, resolved_target_id)` — compound index for backlink queries that JOIN on both columns
- [ ] Register the migration in `MigrationRegistry`
- [ ] Bump `SCHEMA_VERSION` to 9 in `vulcan-core/src/lib.rs`
- [ ] Verify with `EXPLAIN QUERY PLAN` that the new indexes are used by the most common queries
- [ ] Run the existing test suite to confirm no regressions

**Expected improvement:** WHERE clauses on `extension = 'md'` go from full table scan to index lookup. For 13k documents this turns many O(N) scans into O(log N) lookups. The compound link index accelerates backlink queries specifically.

**Files:** `vulcan-core/src/cache/schema.rs`, `vulcan-core/src/cache/migrations.rs`, `vulcan-core/src/lib.rs`

### 8.5 Hybrid search batch filtering

Replace per-hit filter queries in hybrid search with a single batch lookup.

**Current bottleneck:** `matches_filters()` in `vulcan-core/src/search.rs` is called once per vector hit from `hybrid_search_hits()`. Each call runs up to 3 SQL queries: one to look up document_id by path, one to check tag existence, one to check property existence. With a typical candidate_limit of 40 vector hits, this is up to 120 individual queries.

**Implementation:**
- [ ] Before the vector hit filter loop, collect all vector hit paths into a `Vec<&str>`
- [ ] Run a single batch query to load document_ids for all paths: `SELECT path, id FROM documents WHERE path IN (?, ?, ...)`
- [ ] If tag filter is active, run a single batch query: `SELECT DISTINCT document_id FROM tags WHERE document_id IN (...) AND tag_text = ?`
- [ ] If property filter is active, run a single batch query: `SELECT DISTINCT document_id FROM property_values WHERE document_id IN (...) AND key = ?`
- [ ] Build a `HashSet<String>` of passing document_ids and filter vector hits against it
- [ ] The existing `filtered_paths` (from keyword search pre-filtering) continues to work as a fast pre-check before the batch queries
- [ ] Unit tests: existing hybrid search tests must produce identical results

**Expected improvement:** 3N individual queries → 3 batch queries. For 40 vector hits this is 120 queries → 3.

**Files:** `vulcan-core/src/search.rs`

### 8.6 Vector index hash comparison

Replace in-memory hash loading with a SQL-side comparison for identifying pending chunks.

**Current bottleneck:** `index_vectors_with_progress()` in `vulcan-core/src/vector.rs` calls `store.load_hashes()` which loads ALL chunk hashes from the vector table into a `HashMap<String, Vec<u8>>`. Then it iterates all current chunks in Rust to find mismatches. For 50k+ chunks this allocates a large HashMap and does O(N) Rust-side comparison.

**Implementation:**
- [ ] Add a `pending_chunk_ids(current_chunks: &[(chunk_id, content_hash)])` method to `VectorStore` / `SqliteVecStore`
- [ ] Implementation: create a temp table with current chunk_id + content_hash pairs, then `SELECT chunk_id FROM temp WHERE NOT EXISTS (SELECT 1 FROM vectors_table WHERE vectors_table.chunk_id = temp.chunk_id AND vectors_table.content_hash = temp.content_hash)`
- [ ] Similarly for stale detection: `SELECT chunk_id FROM vectors_table WHERE chunk_id NOT IN (SELECT chunk_id FROM temp)`
- [ ] This avoids loading all hashes into memory and lets SQLite use its indexes
- [ ] Fall back to current approach if temp table creation fails (defensive)
- [ ] The `delete_chunks` call for stale chunks remains unchanged
- [ ] Unit tests: existing vector index tests must produce identical results

**Expected improvement:** Eliminates O(N) memory allocation for hash HashMap; comparison done in SQLite with index support. Most beneficial when the majority of chunks are already indexed (common case for incremental re-index).

**Files:** `vulcan-embed/src/sqlite_vec.rs`, `vulcan-core/src/vector.rs`

### 8.7 Scan phase: further SQLite write optimization

Investigate and apply remaining SQLite tuning for bulk insert workloads.

**Context:** The scan phase currently achieves ~1100 files/s on fresh index but degrades from ~1500 to ~1100 as the B-tree grows. Link resolution takes ~16s for ~13k files due to per-row FK-validated UPDATEs.

**Implementation:**
- [ ] Profile the scan write phase with `perf` or `flamegraph` to identify the actual bottleneck (B-tree page splits vs. index maintenance vs. FK checks)
- [ ] Test disabling FK checks during bulk scan (`PRAGMA foreign_keys = OFF` within the scan transaction, re-enable after) — FKs are validated on INSERT which adds overhead for every link/heading/tag row
- [ ] Test increasing `page_size` from default 4096 to 8192 or 16384 for better B-tree fanout on large datasets
- [ ] Test `PRAGMA locking_mode = EXCLUSIVE` during scan (already single-writer, so no concurrency loss)
- [ ] Benchmark each change independently; only keep changes that show measurable improvement
- [ ] Document findings in code comments for future reference

**Expected improvement:** Incremental — possibly 10–30% reduction in scan write phase. The goal is to identify the remaining ceiling and document it, not necessarily to break through it.

**Files:** `vulcan-core/src/scan.rs`, `vulcan-core/src/cache/mod.rs`

### Implementation order

1. **8.4** (Missing indexes) — Quickest win, broad impact, no algorithm changes. ~30 minutes.
2. **8.1** (Aho-Corasick mentions) — Highest single-command impact. ~2 hours.
3. **8.5** (Hybrid search batch) — Straightforward query batching. ~1 hour.
4. **8.2** (Duplicate candidate optimization) — Algorithm improvement. ~1 hour.
5. **8.3** (Graph query caching) — Refactoring, medium scope. ~2 hours.
6. **8.6** (Vector hash comparison) — Store-layer change. ~2 hours.
7. **8.7** (Scan write profiling) — Investigative, results uncertain. ~2 hours.

---

## Phase 9: CLI Refinements

**Goal:** Improve the interactive CLI experience with direct note editing, a persistent browser TUI, auto-commit integration, and quality-of-life commands. These features make vulcan a practical daily-driver tool for vault maintenance, not just a query/analysis engine.

**Depends on:** Phase 7 complete.
**Design refs:** Existing `note_picker.rs` (fuzzy picker), `bases_tui.rs` (TUI infrastructure + `open_in_editor` + `with_terminal_suspended`), `serve.rs` (watcher integration).

**Design decisions:**
- **Keybinding: `q` no longer quits the picker.** The existing note picker uses both `Esc` and `q` to cancel. Since `edit` and `browse` require typing search queries, `q` must be a normal character. Change to `Esc`-only across all picker/TUI contexts (note picker, browse TUI). This is a minor breaking change.
- **Browse TUI ships incrementally in layers:** (1) edit loop only, (2) `Ctrl-F` full-text search, (3) action hotkeys, (4) remaining modes. Each layer is independently shippable.
- **TUI testing strategy:** Test state machine transitions on `BrowseState`/`NotePickerState` directly (no terminal). Use `ratatui::TestBackend` for render assertions on layout and content. Manual testing for interactive flows.

### 9.1 `edit` command — open note in `$EDITOR`

Open a note for editing directly from the CLI, with picker fallback for disambiguation.

```
vulcan edit [note]           # open specific note, or picker if omitted
vulcan edit --new [path]     # create new note, open in editor
```

- [ ] **Keybinding fix:** change note picker quit from `Esc | q` to `Esc`-only, so `q` can be typed in search queries
- [ ] `vulcan edit <note>`: resolve note by path/filename/alias, open in `$VISUAL`/`$EDITOR`/`vi`
- [ ] If `<note>` is ambiguous or omitted: spawn the existing note picker TUI, Enter opens selected note in editor
- [ ] `vulcan edit --new <path>`: create a new empty note (or from template if 9.4.3 is implemented), open in editor
- [ ] After editor exits: run an incremental rescan of the edited file to update the cache
- [ ] If auto-commit is enabled (8.3): commit the change after rescan
- [ ] Reuse `open_in_editor()` and `with_terminal_suspended()` from `bases_tui.rs` — extract these into a shared `editor.rs` utility module in `vulcan-cli/src/`
- [ ] Non-interactive fallback: if not a TTY, print an error rather than spawning a picker
- [ ] Integration test: create a temp vault, run `edit --new`, verify file exists and cache is updated

### 9.2 `browse` command — persistent note browser TUI

A persistent TUI session that acts as a lightweight terminal Obsidian. The user searches, previews, edits, and navigates notes without leaving the TUI.

```
vulcan browse
```

**Core loop:**
- [ ] Start in the note picker view (extend existing `NotePickerState` from `note_picker.rs`)
- [ ] Enter opens selected note in `$EDITOR`; on editor exit, return to picker with previous query and selection preserved
- [ ] After each editor exit: incremental rescan of the edited file, refresh the note list
- [ ] If auto-commit is enabled (8.3): commit after each editor session

**Search mode hotkeys** (toggled in the picker's input bar):
- [ ] Default / `/`: fuzzy path/alias/filename filter (current behavior)
- [ ] `Ctrl-F`: full-text search mode — query runs against FTS5, results replace the note list, preview pane shows matching snippets with highlighted terms instead of raw file content
- [ ] `Ctrl-T`: tag filter mode — type a tag name, fuzzy-match against all indexed tags, show notes matching the selected tag
- [ ] `Ctrl-P`: property filter mode — type a property predicate (reuse the existing `where` filter syntax from `NoteQuery`), filter notes by property values

**Action hotkeys on the selected note:**
- [ ] `e` or `Enter`: edit in `$EDITOR` (as above)
- [ ] `m`: move/rename — inline prompt for destination path, runs the move-rewrite engine, refreshes note list
- [ ] `b`: switch to a backlinks view for the selected note (list of linking notes with context, navigable)
- [ ] `l`: switch to an outgoing links view for the selected note
- [ ] `d`: run doctor on this specific note, show diagnostics in a temporary pane
- [ ] `n`: create new note — prompt for path, open in editor, return to picker
- [ ] `g`: show git log for this file (if vault is a git repo), displayed in a scrollable pane
- [ ] `o`: if the selected file is a `.base` file, open it in the bases TUI (`bases tui`)

**UI details:**
- [ ] Status bar at bottom: vault name, total note count, filtered count, last scan timestamp, current search mode indicator
- [ ] Footer keybinding hints update to reflect current mode
- [ ] Resize-safe layout (reuse `ratatui` constraint-based layout)

**Incremental shipping layers:**
1. **Layer 1 — Edit loop:** Picker → editor → picker with rescan. Minimal viable `browse`.
2. **Layer 2 — Full-text search:** Add `Ctrl-F` mode with FTS5 results and snippet preview.
3. **Layer 3 — Action hotkeys:** `m` (move), `b` (backlinks), `l` (links), `n` (new note).
4. **Layer 4 — Remaining modes and actions:** `Ctrl-T` (tag filter), `Ctrl-P` (property filter), `d` (doctor), `g` (git log), `o` (open bases TUI).

Each layer is independently shippable and testable.

**Implementation notes:**
- Extend `NotePickerState` with a `mode: BrowseMode` enum (`Fuzzy`, `FullText`, `Tag`, `Property`) that controls filtering logic and preview rendering
- The browse TUI lives in a new `vulcan-cli/src/browse_tui.rs` module
- Reuse `note_picker.rs` types and fuzzy scoring; the browse TUI is a superset of the picker
- For FTS mode, call `search_vault()` from `vulcan-core` and map results to the same `(score, NoteIdentity)` display format
- For backlinks/links views, call `query_backlinks()`/`query_links()` and display as a navigable list that can be drilled into

**Testing strategy:**
- Unit tests for `BrowseState` transitions: mode switching, selection persistence across mode changes, query state reset behavior
- Unit tests for action dispatch: verify correct `vulcan-core` calls for move, backlinks, links, etc.
- `ratatui::TestBackend` render tests: verify layout adapts to terminal size, correct pane content for each mode, keybinding hints update per mode
- Integration tests: spin up a temp vault, exercise the edit loop programmatically (mock editor via `EDITOR=true`), verify cache is updated after edit
- Fuzzy scoring tests already exist in `note_picker.rs`; extend for new filter modes

### 9.3 Auto-commit

Automatically commit vault changes to git after vulcan-initiated mutations. Off by default.

**Config in `.vulcan/config.toml`:**

```toml
[git]
# Enable auto-commit after vault-mutating operations (default: false)
auto_commit = false

# What triggers a commit:
# - "mutation": commit after vulcan-initiated writes (move, update, unset,
#   rename-*, merge-tags, link-mentions, edit, browse edits)
# - "scan": also commit when scan detects external changes
trigger = "mutation"

# Commit message template. Variables: {action}, {files}, {count}
# {action} = the vulcan command name (e.g. "move", "update", "edit")
# {files} = comma-separated changed files (truncated to 5, with "+N more")
# {count} = total number of files changed
message = "vulcan {action}: {files}"

# Scope of files to commit:
# - "vulcan-only": only commit files that vulcan actually modified
# - "all": stage and commit ALL uncommitted changes in the vault
scope = "vulcan-only"

# Paths to always exclude from auto-commits (in addition to .vulcan/)
# exclude = [".obsidian/workspace.json", ".obsidian/workspace-mobile.json"]
```

- [ ] Add `[git]` section to `VaultConfig` with `GitConfig` struct: `auto_commit: bool`, `trigger: GitTrigger`, `message: String`, `scope: GitScope`, `exclude: Vec<String>`
- [ ] Add `[git]` section to `DEFAULT_CONFIG_TEMPLATE` (commented out, with defaults shown)
- [ ] New module `vulcan-core/src/git.rs`:
  - `is_git_repo(vault_root) -> bool`: check for `.git` directory or `git rev-parse --git-dir`
  - `auto_commit(paths, config, action, changed_files) -> Result<AutoCommitReport>`: stage files, create commit
  - `git_log(vault_root, file_path, limit) -> Result<Vec<GitLogEntry>>`: file history for browse TUI
  - `git_status(vault_root) -> Result<GitStatusReport>`: uncommitted changes summary
  - Shell out to `git` CLI (not libgit2) to keep dependencies light
  - Exclude `.vulcan/` and configured exclude paths from staging
- [ ] `AutoCommitReport` struct: `committed: bool`, `message: String`, `files: Vec<String>`, `sha: Option<String>`
- [ ] Call `auto_commit()` after successful execution of mutating commands: `move`, `update`, `unset`, `rename-property`, `merge-tags`, `rename-alias`, `rename-heading`, `rename-block-ref`, `link-mentions`, `rewrite`, `edit`, and browse TUI edits
- [ ] `--no-commit` flag on all mutating CLI commands to suppress auto-commit for one invocation
- [ ] If `auto_commit = true` but vault is not a git repo: emit a warning diagnostic, do not error
- [ ] If `trigger = "scan"`: also commit after `scan` and `watch` detect and process external changes
- [ ] Integration test: enable auto-commit in config, run a mutation, verify git log shows the commit with expected message

### 9.4 Additional CLI commands

#### 9.4.1 `diff` — single-note change view

```
vulcan diff [note] [--since <checkpoint>]
```

- [ ] Show what changed in a specific note since last scan, checkpoint, or git HEAD
- [ ] If git is available: show `git diff` for the file, rendered with context
- [ ] If no git: fall back to comparing current content against cached content hash (show "changed" / "unchanged" / "new")
- [ ] `--output json` support
- [ ] Builds on existing `changes` command but focused on a single note with richer output

#### 9.4.2 `inbox` — quick capture

```
vulcan inbox <text>
vulcan inbox --file <path>     # append file contents
echo "idea" | vulcan inbox -   # read from stdin
```

- [ ] Append text to a configurable inbox note
- [ ] Config in `.vulcan/config.toml`:
  ```toml
  [inbox]
  path = "Inbox.md"         # relative to vault root
  format = "- {text}"       # template for each entry; supports {text}, {date}, {time}, {datetime}
  timestamp = true           # prepend ISO timestamp to each entry
  heading = "## Inbox"       # optional: append under this heading (create if missing)
  ```
- [ ] Create the inbox note if it doesn't exist
- [ ] Incremental rescan after append
- [ ] Auto-commit if enabled
- [ ] `--output json` returns `{ "path": "Inbox.md", "appended": true }`

#### 9.4.3 `template` — create note from template

```
vulcan template [name] [--path <output-path>]
vulcan template --list
```

- [ ] Templates stored in `.vulcan/templates/` as regular markdown files
- [ ] Template variables: `{{title}}` (derived from output path), `{{date}}`, `{{time}}`, `{{datetime}}`, `{{uuid}}`
- [ ] `--list` shows available templates
- [ ] If `--path` is omitted, prompt for path (or use template's own filename with date prefix)
- [ ] After creation: open in `$EDITOR` if TTY, then rescan
- [ ] Auto-commit if enabled

#### 9.4.4 `open` — open note in Obsidian

```
vulcan open [note]
```

- [ ] Open a note in the Obsidian desktop app via `obsidian://open?vault=<name>&file=<path>` URI
- [ ] Vault name derived from folder name or `.obsidian/` config
- [ ] Uses `xdg-open` (Linux), `open` (macOS), or `start` (Windows) to launch the URI
- [ ] Note resolution follows the same path/filename/alias/picker logic as other commands
- [ ] Useful for quickly jumping from CLI analysis to visual Obsidian editing

---

## Phase 10: Multi-Vault Daemon

**Goal:** A long-running process that serves multiple vaults over a proper REST API. The CLI can connect to it instead of opening the cache directly, eliminating per-command startup cost and enabling multi-vault workflows.

**Depends on:** Phase 7 complete. Independent of Phase 9 (can be developed in parallel).
**Design refs:** Existing `serve.rs` (single-vault HTTP server, hand-rolled), `watch.rs` (file watcher).

### 10.1 Architecture decisions

The daemon extends the existing architecture rather than replacing it:

- **Same binary**: `vulcan daemon start/stop/status/config` — keeps deployment simple, shares all deps
- **HTTP framework**: `axum` replaces the hand-rolled `TcpListener` server. Provides async request handling, tower middleware (auth, CORS, logging, compression), and WebSocket support for live updates.
- **WebSocket-ready architecture**: Design the router module structure so that adding WebSocket upgrade endpoints (e.g., `/ws/{vault_id}/...`) is straightforward. Phase 16 will use WebSockets for real-time collaborative editing via Automerge sync protocol. No WebSocket code ships in Phase 10, but handlers should not assume pure request/response.
- **Async boundary**: `vulcan-core` stays synchronous (SQLite is inherently sync). The daemon wraps core calls in `tokio::task::spawn_blocking`. This avoids an async rewrite of the entire core.
- **New crate**: `vulcan-daemon` (lib) — contains the axum router, middleware, vault registry, and daemon lifecycle. `vulcan-cli` depends on it for the `daemon` subcommand.

### 10.2 Vault registry

```toml
# ~/.config/vulcan/daemon.toml
bind = "127.0.0.1:3210"

[[vault]]
id = "personal"
path = "/home/user/vaults/personal"
token = "$argon2id$v=19$..."  # hashed

[[vault]]
id = "work"
path = "/home/user/vaults/work"
token = "$argon2id$v=19$..."
read_only = true  # no mutation endpoints
```

- [ ] Vault registry config at `~/.config/vulcan/daemon.toml` (XDG_CONFIG_HOME respected)
- [ ] Each vault entry: `id` (short name, URL-safe), `path`, `token` (argon2 hashed), optional `read_only` flag
- [ ] `vulcan daemon config add <id> <path>` — register a vault, generate and display a token
- [ ] `vulcan daemon config remove <id>` — unregister a vault
- [ ] `vulcan daemon config list` — show registered vaults (paths, IDs, status)
- [ ] Auth tokens stored outside vault content — avoids coupling auth to the data it protects
- [ ] Vault auto-discovery: optionally scan a directory for vaults (e.g., `scan_dir = "/home/user/vaults"`)

### 10.3 REST API

All endpoints are namespaced by vault ID: `/{vault_id}/...`

**Read endpoints** (map 1:1 to existing CLI commands):
- [ ] `GET /{id}/search?q=...` — full-text and hybrid search
- [ ] `GET /{id}/notes?where=...&sort=...` — property query
- [ ] `GET /{id}/notes/{path}` — single note metadata + content
- [ ] `GET /{id}/links/{path}` — outgoing links
- [ ] `GET /{id}/backlinks/{path}` — inbound links
- [ ] `GET /{id}/graph/stats` — graph analytics
- [ ] `GET /{id}/graph/path?from=...&to=...` — shortest path
- [ ] `GET /{id}/graph/hubs`, `/dead-ends`, `/components` — graph analysis
- [ ] `GET /{id}/vectors/neighbors?q=...` — vector similarity
- [ ] `GET /{id}/vectors/related?note=...` — related notes
- [ ] `GET /{id}/vectors/models` — list embedding models
- [ ] `GET /{id}/bases/{file}` — evaluate a bases view
- [ ] `GET /{id}/doctor` — vault diagnostics
- [ ] `GET /{id}/query?dsl=...` or `POST /{id}/query` with JSON body — ad hoc query

**Write endpoints:**
- [ ] `POST /{id}/notes` — create a note (body: `{ "path": "...", "content": "..." }`)
- [ ] `PATCH /{id}/notes/{path}` — update properties or content
- [ ] `DELETE /{id}/notes/{path}` — delete a note
- [ ] `POST /{id}/move` — move/rename with link rewriting (`{ "source": "...", "destination": "..." }`)
- [ ] `POST /{id}/update` — bulk property update (`{ "where": [...], "set": { "key": "value" } }`)
- [ ] `POST /{id}/inbox` — quick capture (like `vulcan inbox`)
- [ ] `POST /{id}/scan` — trigger incremental rescan
- [ ] `POST /{id}/vectors/index` — trigger embedding indexing

**Daemon management:**
- [ ] `GET /health` — daemon health, vault statuses
- [ ] `GET /vaults` — list registered vaults with status
- [ ] Auth: per-vault `Authorization: Bearer <token>` header, validated against argon2 hash

### 10.4 Per-vault watcher

- [ ] Each registered vault gets its own file watcher thread (reuse `watch_vault_until`)
- [ ] Watcher keeps cache fresh automatically — API queries always return current data
- [ ] Watcher errors are surfaced via `/health` and `/{id}/health` endpoints
- [ ] Graceful shutdown: daemon stop signals all watchers to terminate

### 10.5 CLI daemon integration

- [ ] `vulcan daemon start` — start the daemon (foreground or `--detach` for background)
- [ ] `vulcan daemon stop` — send shutdown signal
- [ ] `vulcan daemon status` — show running state, registered vaults, uptime
- [ ] `vulcan --daemon` flag or `VULCAN_DAEMON_URL` env var on any CLI command: route the command through the daemon's REST API instead of direct SQLite access. Same UX, daemon does the work.
- [ ] Transparent fallback: if daemon is not running, fall back to direct mode with a warning

### 10.6 Implementation notes

- **`serve` becomes a lightweight shim over daemon internals.** The existing `vulcan serve` command is kept for single-vault convenience but refactored to use the same router and handler code as the daemon. Internally it registers the current vault as the sole vault and starts the daemon in single-vault mode. This ensures API consistency between `serve` and `daemon` without maintaining two codepaths.
- **Daemon dependencies (axum, tokio) are included unconditionally.** If compile time or binary size becomes a problem, they can be moved behind a `--features daemon` cargo feature flag later, but start without the complexity.
- Response format matches existing `--output json` format from CLI commands — the daemon serializes the same report structs
- Rate limiting and request logging via tower middleware
- CORS headers configurable for WebUI integration (Phase 13)

---

## Phase 11: Git Auto-Versioning (Daemon-Level)

**Goal:** Automatic version history for vault content managed by the daemon. Extends the per-vault auto-commit from Phase 9.3 to daemon-managed vaults with richer history APIs.

**Depends on:** Phase 9.3 (git module in vulcan-core), Phase 10 (daemon).

### 11.1 Daemon-level git integration

- [ ] On vault registration: detect if vault is a git repo, optionally `git init` if configured
- [ ] Configurable commit strategy per vault in `daemon.toml`:
  ```toml
  [[vault]]
  id = "personal"
  path = "/home/user/vaults/personal"
  [vault.git]
  auto_commit = true
  strategy = "batched"  # "per-write", "batched", or "manual"
  batch_interval_seconds = 300  # for "batched" strategy
  message = "vault: {files}"
  ```
- [ ] `per-write`: commit immediately after each mutation (same as Phase 9.3)
- [ ] `batched`: accumulate changes, commit every N seconds (daemon timer thread)
- [ ] `manual`: no auto-commit, but history endpoints still work if vault has git

### 11.2 History API endpoints

- [ ] `GET /{id}/history/{path}` — git log for a specific file (author, date, message, sha)
- [ ] `GET /{id}/history/{path}/{sha}` — file content at a specific commit
- [ ] `GET /{id}/diff/{path}?from={sha}&to={sha}` — diff between two versions
- [ ] `GET /{id}/diff` — uncommitted changes in the vault
- [ ] `GET /{id}/history` — recent commits across the whole vault

### 11.3 Branch management (optional)

- [ ] Daemon works on a configurable branch (default: current branch)
- [ ] `POST /{id}/git/snapshot` — create a named tag/branch for a point-in-time snapshot
- [ ] Integrate with existing `checkpoint` command for cache-level + git-level snapshots

---

## Phase 12: Sync Integration

**Goal:** Pluggable sync backends so vaults stay current across devices. The daemon orchestrates sync alongside watching and versioning.

**Depends on:** Phase 10 (daemon), Phase 11 (git versioning for conflict-aware sync).

### 12.1 Sync backend trait

```rust
trait SyncBackend: Send + Sync {
    fn start(&mut self, vault_path: &Path) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn status(&self) -> SyncStatus;  // Idle, Syncing, Error(String)
    fn trigger(&mut self) -> Result<()>;  // Force a sync cycle
}
```

- [ ] Define the trait in a new `vulcan-sync` crate (or module in `vulcan-daemon`)
- [ ] `SyncStatus` enum: `Idle`, `Syncing { progress: Option<f32> }`, `Error(String)`, `Disabled`

### 12.2 Obsidian headless sync backend

- [ ] Spawn and manage the `obsidian-headless` process as a subprocess
- [ ] Config in `daemon.toml`:
  ```toml
  [[vault]]
  id = "personal"
  [vault.sync]
  backend = "obsidian-headless"
  binary = "/usr/local/bin/obsidian-headless"  # path to binary
  # Additional obsidian-headless-specific config
  ```
- [ ] Monitor process health, restart on crash
- [ ] Forward sync status to daemon health endpoint

### 12.3 Git remote sync backend

- [ ] Pull/push on schedule or on trigger
- [ ] Config: `remote`, `branch`, `pull_interval_seconds`, `auto_push`
- [ ] Merge strategy: fast-forward only by default, configurable
- [ ] Conflict detection: if pull results in merge conflicts, surface as diagnostics (do not auto-resolve)

### 12.4 Passive sync backend

- [ ] For Syncthing, Dropbox, iCloud, etc. — the sync tool runs independently
- [ ] The daemon just watches for file changes (already handled by the watcher)
- [ ] Sync status is always "external" — daemon doesn't control it
- [ ] Useful for users who already have sync set up and just want the daemon's API layer

### 12.5 Sync API endpoints

- [ ] `GET /{id}/sync/status` — current sync state
- [ ] `POST /{id}/sync/trigger` — force a sync cycle
- [ ] `GET /{id}/sync/conflicts` — list files with unresolved conflicts (if applicable)

---

## Phase 13: WebUI — Admin and Browse

**Goal:** A web interface for managing the daemon, browsing vaults, and monitoring sync. Read-only initially, leveraging the existing JSON API.

**Depends on:** Phase 10 (daemon REST API).

### 13.1 Architecture

- [ ] Served by the daemon itself at a configurable path (e.g., `GET /ui/...`)
- [ ] Static SPA assets embedded in the binary at compile time (e.g., `rust-embed` or `include_dir`)
- [ ] Alternatively: separate frontend repo that builds to static files, daemon serves them
- [ ] Framework choice: lightweight (Svelte, Solid, or vanilla + htmx) — TBD at implementation time
- [ ] Auth: reuse daemon token auth, with a login page for browser sessions (cookie or localStorage token)

### 13.2 Admin panel

- [ ] Vault list with status indicators (online, syncing, error, indexing)
- [ ] Register/unregister vaults
- [ ] Per-vault config editing (sync settings, git settings, embedding config)
- [ ] Daemon health dashboard: uptime, memory, active watchers, recent errors
- [ ] Token management: generate, revoke, copy

### 13.3 Vault browser

- [ ] Note list with search (uses `/search` API)
- [ ] Note detail view: rendered markdown, frontmatter properties, backlinks, outgoing links
- [ ] Graph visualization: interactive node-link diagram (uses `/graph/*` APIs)
- [ ] Tag cloud / tag browser
- [ ] Property explorer: browse notes by property values
- [ ] Bases view rendering: display evaluated bases views as tables

---

## Phase 14: WebUI — Write and Collaborate

**Goal:** Turn the web browser into an editor for vault content.

**Depends on:** Phase 13 (read-only WebUI), Phase 10 (write API endpoints).

### 14.1 Note editor

**Automerge for live editing sessions.** Use `automerge` (Rust-native CRDT library) for real-time collaborative editing and ephemeral editing sessions. Automerge is scoped to the WebUI editing layer — it does **not** replace git as the versioning backend. The on-disk `.md` files remain the vault source of truth.

**Architecture:**
- The editor surface (CodeMirror or ProseMirror) binds to an Automerge text type for the duration of an editing session
- On save: Automerge doc state is materialized → `.md` file on disk → incremental rescan → git commit (if auto-commit enabled)
- On editor open: `.md` file content is loaded into a fresh Automerge doc (or resumed from a persisted session)
- Automerge docs are ephemeral by default — they exist while a note is being edited and are discarded after materialization. Optional session persistence in `.vulcan/` for crash recovery.
- Phase 16 live collaboration adds multi-peer sync on top of this same Automerge doc, without changing the materialization pipeline

**Design decision: git stays the versioning backend.** Automerge provides excellent real-time collaboration and offline merge, but the vault's canonical history remains in git. This avoids a dual source-of-truth problem — on-disk files are always authoritative for CLI, Obsidian, search, and indexing. Automerge is a transient editing layer, not a storage layer.

- [ ] Integrate `automerge` for ephemeral editing sessions (one Automerge doc per actively-edited note)
- [ ] Markdown editor component (CodeMirror or ProseMirror with Automerge text binding — TBD)
- [ ] Live preview (split-pane or toggle)
- [ ] Wikilink autocomplete (uses `/notes` API for suggestions)
- [ ] Tag autocomplete
- [ ] Frontmatter property editor (structured form UI, not raw YAML editing)
- [ ] Materialization pipeline: flush Automerge doc state to disk via `PATCH /{id}/notes/{path}`, which rescans and optionally commits
- [ ] Optional session persistence: store Automerge binary doc in `.vulcan/` for crash recovery, discard after successful materialization

### 14.2 Note management

- [ ] Create new notes (with optional template selection)
- [ ] Move/rename notes (with link rewriting preview)
- [ ] Delete notes (with broken-link impact preview)
- [ ] Inbox quick-capture widget

### 14.3 History and diff

- [ ] Git diff viewer for pending uncommitted changes
- [ ] File history timeline (uses `/history` API from Phase 11)
- [ ] Side-by-side diff between versions
- [ ] Restore previous version

### 14.4 Activity feed

- [ ] Recent changes across the vault (from `changes` API)
- [ ] Sync activity log
- [ ] Auto-commit log

---

## Phase 15: Extensibility and Integrations

**Goal:** Let vaults define custom behaviors and expose integration points for external tools.

**Depends on:** Phase 10 (daemon API).

### 15.1 Webhook system

- [ ] Vault config defines triggers and HTTP callbacks:
  ```toml
  [[webhooks]]
  event = "note.changed"      # note.changed, note.created, note.deleted, tag.added, scan.complete
  url = "https://example.com/hook"
  secret = "..."              # HMAC signing secret
  filter = "path:Projects/*"  # optional: only fire for matching notes
  ```
- [ ] Daemon fires webhooks asynchronously after events
- [ ] Retry with exponential backoff on failure
- [ ] Webhook delivery log accessible via API

### 15.2 Telegram bot integration

- [ ] Per-vault Telegram bot configuration:
  ```toml
  [vault.telegram]
  bot_token_env = "TELEGRAM_BOT_TOKEN"
  chat_id = "123456"
  commands = ["search", "inbox", "daily"]
  ```
- [ ] `/search <query>` — search the vault, return top results
- [ ] `/inbox <text>` — append to inbox note
- [ ] `/daily` — create or open today's daily note
- [ ] Implemented as a daemon plugin module

### 15.3 Custom API endpoints

- [ ] Vault config can define additional routes:
  ```toml
  [[endpoints]]
  path = "/inbox"
  method = "POST"
  action = "inbox"  # maps to built-in action

  [[endpoints]]
  path = "/daily"
  method = "POST"
  action = "template"
  template = "daily"
  ```
- [ ] Actions are a fixed set of built-in operations (inbox, template, update, etc.)
- [ ] This is intentionally not a plugin/scripting system — keeps the security surface small

### 15.4 Plugin trait (future)

- [ ] Rust trait for daemon plugins: `on_event`, `register_routes`, `on_startup`, `on_shutdown`
- [ ] Plugins compiled into the binary (feature flags) or loaded as dynamic libraries
- [ ] This is a future direction — start with the webhook and built-in endpoint system first

---

## Phase 16: Wiki Mode

**Goal:** A polished, public-facing wiki served from an Obsidian vault. Read-optimized with optional auth for editing. Supports real-time collaborative editing via Automerge CRDTs.

**Depends on:** Phase 13 (WebUI browse), Phase 14 (WebUI write, Automerge editing sessions).

**Automerge in Phase 16:** Phase 14 introduces Automerge for ephemeral single-user editing sessions. Phase 16 extends this to multi-user real-time collaboration by adding the Automerge sync protocol over WebSockets. The on-disk `.md` files and git remain the canonical store and versioning backend — Automerge is the live collaboration layer, not a replacement for git.

### 16.1 Public read mode

- [ ] Unauthenticated read access to rendered vault content
- [ ] Rendered Markdown with Obsidian-compatible features: callouts, embeds, math (KaTeX), wikilinks resolved to wiki URLs, mermaid diagrams, code highlighting
- [ ] Navigation: sidebar with folder tree, tag-based browsing, graph explorer
- [ ] Search: full FTS + vector hybrid search exposed in the UI
- [ ] Home page: configurable (default: note named `Home.md` or `index.md`)
- [ ] SEO: server-rendered HTML, meta tags, sitemap generation

### 16.2 Wiki-specific rendering

- [ ] Wikilinks rendered as clickable links to other wiki pages
- [ ] Embeds rendered inline (images, other notes, blocks)
- [ ] Backlinks section at the bottom of each page
- [ ] Table of contents generated from headings
- [ ] Breadcrumb navigation from folder path

### 16.3 Theming and branding

- [ ] Configurable per-vault theme (CSS custom properties)
- [ ] Custom header/footer HTML
- [ ] Logo and favicon configuration
- [ ] Light/dark mode toggle

### 16.4 Access control

- [ ] Public read / authenticated write (default)
- [ ] Fully public (no auth)
- [ ] Fully private (auth required for read and write)
- [ ] Per-folder or per-tag visibility rules (future)

### 16.5 Live collaborative editing

Real-time multi-user editing using Automerge CRDTs, building on the Automerge document model introduced in Phase 14.

- [ ] WebSocket endpoint `WS /{id}/collab/{path}` — joins an Automerge sync session for a note
- [ ] Server manages Automerge documents: one doc per actively-edited note, loaded from `.md` content on first open (or resumed from crash-recovery state)
- [ ] Automerge sync protocol over WebSocket: clients exchange sync messages to converge on shared state
- [ ] Presence awareness: cursor positions and user identifiers broadcast to all connected peers
- [ ] Materialization pipeline: periodically (and on last-editor-disconnect) flush Automerge doc state → `.md` file → incremental rescan → optional git commit
- [ ] Conflict-free by design: Automerge CRDTs guarantee convergence without manual conflict resolution
- [ ] Graceful degradation: if WebSocket disconnects, client continues editing locally; changes merge on reconnect
- [ ] Editor integration: the CodeMirror/ProseMirror binding from Phase 14 already uses Automerge — collaboration adds the sync layer on top

### 16.6 Local-first and WASM (future direction)

Automerge compiles to `wasm32`, enabling browser-side editing without a live server connection.

- [ ] Compile `automerge` to `wasm32` for browser-side document operations
- [ ] Client-side Automerge doc: browser owns the editing doc, syncs to server when online
- [ ] Offline support: edits persist in browser storage (IndexedDB/OPFS), merge on reconnect via Automerge sync protocol
- [ ] Potential: compile `vulcan-core` query engine to WASM for client-side search and graph queries (requires abstracting storage away from `rusqlite` — significant effort, evaluate when the use case is clear)

**Note:** Files on disk and git remain the canonical store even in a local-first model — the browser's Automerge doc is an ephemeral editing session that materializes back to the server. `vulcan-core` depends on `rusqlite(bundled)` and `sqlite-vec`, which do not compile to `wasm32`; a WASM query engine would need a different storage backend. This is a future direction — do not architect for it prematurely.

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
                                    ↓                    ↓                         ↓
                          Phase 8 (Performance)  Phase 9 (CLI refinements)  Phase 10 (Multi-vault daemon)
                                                   ↓                          ↓             ↓
                                                 9.3 ──────→ Phase 11 (Git versioning)  Phase 13 (WebUI browse)
                                                                ↓                         ↓
                                                        Phase 12 (Sync)           Phase 14 (WebUI write + Automerge)
                                                                                    ↓
                                                        Phase 15 (Extensibility) ← Phase 10
                                                                                    ↓
                                                                            Phase 16 (Wiki + live collab)
                                                                                    ↓
                                                                            16.6 (Local-first / WASM) [future]
```

Phase 8 (Performance) is independent and can proceed in parallel with Phases 9 and 10 after Phase 7.
Phases 9 and 10 can proceed in parallel after Phase 7.
Phase 11 requires 9.3 (git module) and 10 (daemon). Phase 12 requires 10 and 11.
Phase 13 requires 10. Phase 14 requires 13 and 10's write endpoints. Phase 14 introduces Automerge as the document model.
Phase 15 requires 10. Phase 16 requires 13 and 14 (including the Automerge foundation from 14 for live collaboration).
Phase 16.6 (local-first/WASM) is a future direction beyond the current roadmap scope.

---

## New crates (Phases 10+)

| Crate | Type | Purpose |
|-------|------|---------|
| `vulcan-daemon` | lib | axum router, middleware, vault registry, daemon lifecycle |
| `vulcan-sync` | lib | Sync backend trait and implementations (obsidian-headless, git remote, passive) |

The `vulcan-cli` binary gains the `daemon` subcommand group by depending on `vulcan-daemon`.
The `vulcan-daemon` crate depends on `vulcan-core` (for all vault operations) and `vulcan-sync` (for sync backends).

## Key dependencies to add (Phases 8+)

| Dependency | Purpose | Phase |
|------------|---------|-------|
| `aho-corasick` | Multi-pattern string matching for mention detection | 8 |
| `axum` | HTTP framework for daemon | 10 |
| `tokio` | Async runtime for axum | 10 |
| `tower-http` | CORS, compression, logging middleware | 10 |
| `argon2` | Token hashing | 10 |
| `automerge` | CRDT document model for collaborative editing | 14 |
| `rust-embed` or `include_dir` | Embed static WebUI assets | 13 |
