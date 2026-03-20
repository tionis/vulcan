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
- [~] Write rebuild command: drop all rows, rescan vault from scratch
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
- [ ] `--output json` global flag: all commands emit JSON when set
- [ ] Line-delimited JSON for streamed/list output
- [ ] `--fields` flag for field selection on list commands
- [ ] `--limit` and `--offset` for pagination
- [ ] Non-interactive detection: suppress spinners/prompts when stdout is not a TTY
- [ ] Snapshot tests for JSON output structure of `scan` and `doctor`

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
- [ ] `--fields` support
- [x] Integration tests against `basic/` vault

### 2.2 Move-safe rewrite engine
- [ ] `move <source> <dest>` command with `--dry-run` support
- [ ] Filesystem operation: rename/move the file first
- [ ] Identify all inbound links: query `links` table by `resolved_target_id`
- [ ] For each affected source file:
  - Re-parse to get fresh byte offsets
  - Locate the specific link span
  - Compute new link text respecting original style (wikilink vs markdown, display text, subpath)
  - Apply edits back-to-front to preserve offsets
- [ ] Update cache: re-index moved file + all rewritten source files
- [ ] Handle edge cases: links in frontmatter properties, links with display text, links with heading/block subpaths, embed links
- [ ] Respect `useMarkdownLinks` and `newLinkFormat` vault config for newly generated link text
- [ ] Input validation: reject path traversal, control characters, non-existent source
- [ ] Dry-run output: list all files that would be modified with before/after link text
- [ ] Unit tests for rewrite logic: style preservation, subpath handling, back-to-front editing
- [ ] Integration test: `move-rewrite/` vault — move a file, assert all links rewritten, run doctor to confirm zero broken links
- [ ] Roundtrip test: move a file, move it back, assert original link text restored

### 2.3 Write serialization
- [ ] Application-level write lock (file lock or advisory lock on the DB)
- [ ] CLI commands acquire write lock before mutating; watcher queues events during lock
- [ ] `busy_timeout` as backstop
- [ ] Test: concurrent scan + move produces correct final state

---

## Phase 3: Search

**Goal:** Full-text search over vault content using FTS5, with snippet extraction and ranking.

**Depends on:** Phase 1 complete. Independent of Phase 2.
**Design refs:** §10 (FTS architecture)

### 3.1 FTS5 setup
- [ ] Schema migration: add FTS5 virtual table in external-content mode, referencing `chunks` table
- [ ] Indexed fields: chunk text content, document title, aliases, headings
- [ ] Synchronization triggers or explicit rebuild to keep FTS in sync with chunks table
- [ ] Rebuild FTS command (for repair)

### 3.2 Search command
- [ ] `search <query>` command
- [ ] FTS5 query syntax: term search, phrase search, prefix search
- [ ] Snippet extraction with configurable context size
- [ ] Result ranking (BM25 via FTS5 rank)
- [ ] Compose with relational filters: `--tag`, `--path-prefix`, `--has-property`
- [ ] `--output json` with structured results (document path, chunk id, snippet, rank)
- [ ] `--limit` for result count
- [ ] Integration test: index `basic/` vault, search for known terms, assert results

### 3.3 Incremental FTS maintenance
- [ ] On re-index: delete FTS rows for changed chunks, insert new FTS rows
- [ ] Verify FTS stays in sync after incremental updates
- [ ] Idempotency test: reindex with no changes, assert FTS state unchanged

---

## Phase 4: Properties and Bases

**Goal:** Structured property querying with type awareness, and read-only evaluation of a subset of Bases files.

**Depends on:** Phase 1 complete. Independent of Phases 2 and 3.
**Design refs:** §9 (properties), §12 (Bases)

### 4.1 Property storage and projections
- [ ] Schema migration: `properties` table — `document_id`, `raw_yaml` (lossless), `canonical_json` (JSONB normalized)
- [ ] Schema migration: `property_values` table — `document_id`, `key`, `value_text`, `value_number`, `value_bool`, `value_date`, `value_type`, for relational projection of scalar properties
- [ ] Schema migration: `property_list_items` table — `document_id`, `key`, `index`, `value_text`, for multivalue properties
- [ ] Schema migration: `property_catalog` table — `key`, `observed_type`, `usage_count`, `namespace`
- [ ] Populate property tables during indexing pipeline (extend Phase 1 indexer)
- [ ] Type inference: use `.obsidian/types.json` when available, fall back to observed value heuristics
- [ ] Handle: missing vs null vs empty string vs empty list vs invalid
- [ ] Link-valued property detection and storage
- [ ] Unit tests: type coercion, multivalue, null/missing/empty distinctions
- [ ] Integration test: `mixed-properties/` vault, assert correct types and diagnostics for inconsistencies

### 4.2 Property query surface
- [ ] `query` or `notes` command with property filters: `--where "status = done"`, `--where "tags contains foo"`
- [ ] Typed comparisons: string, number, date, boolean, list membership
- [ ] Sort by property value
- [ ] `--output json` with property data in results
- [ ] Integration tests for filter/sort operations

### 4.3 Bases parser
- [ ] Parse `.base` YAML files into a validated internal model
- [ ] Extract: view type, filter definitions, sort definitions, formula definitions
- [ ] Separate parser from evaluator (parser is stable, evaluator matures over time)
- [ ] Emit diagnostics for unsupported constructs
- [ ] Unit tests with sample `.base` files

### 4.4 Bases evaluator (read-only subset)
- [ ] `bases eval <file.base>` command
- [ ] Evaluate supported filters against the property query layer
- [ ] Evaluate supported formulas (file properties, simple property access)
- [ ] Surface unsupported features as diagnostics in output, not silent omissions
- [ ] `--output json` for structured results
- [ ] Integration test: `bases/` vault with supported and unsupported constructs

---

## Phase 5: Vectors

**Goal:** Chunk-level embeddings via pluggable providers, nearest-neighbor search, duplicate detection, and clustering.

**Depends on:** Phase 1 (chunks table) and Phase 3 (hybrid retrieval).
**Design refs:** §7 (chunking), §11 (vectors + embedding providers)

### 5.1 Embedding provider trait
- [ ] Define `EmbeddingProvider` trait: `embed_batch(chunks) -> Vec<Result<Vec<f32>, Error>>`, `metadata() -> ModelMetadata`
- [ ] `ModelMetadata`: provider name, model name, dimensions, normalization, max batch size, max input tokens
- [ ] `OpenAICompatibleProvider`: HTTP client for `/v1/embeddings` endpoint
  - Config: base URL, API key (optional), model name
  - Batch according to provider limits
  - Async with concurrency semaphore
  - Exponential backoff on transient failures
- [ ] Provider selection via config file or `--provider` flag
- [ ] Error if no provider configured and embedding is requested
- [ ] Unit tests with mock HTTP server

### 5.2 Vector storage
- [ ] Schema migration: `vectors` table via `sqlite-vec` `vec0` virtual table — `chunk_id`, `provider_name`, `model_name`, `dimensions`, `embedding` (float vector)
- [ ] Abstract behind `VectorStore` trait so `sqlite-vec` can be swapped later
- [ ] Store provider/model metadata per row
- [ ] Never mix vectors from different models in the same query
- [ ] Unit tests for insert/query operations

### 5.3 Embedding pipeline
- [ ] `vectors index` command: embed all un-embedded chunks, or re-embed changed chunks
- [ ] Content-hash gating: skip chunks whose hash matches existing vector row
- [ ] Chunked transactions: commit every N embeddings to avoid long write locks
- [ ] Record failed chunks in diagnostics table; retry on next run
- [ ] Progress reporting (count, rate, errors)
- [ ] `--output json` for status reporting
- [ ] Integration test: embed chunks from `basic/` vault against a mock provider

### 5.4 Nearest-neighbor search
- [ ] `vectors neighbors <query-text>` command: embed query, find nearest chunks
- [ ] `vectors neighbors --note <path>` command: find notes similar to a given note (average or per-chunk)
- [ ] Return: document path, chunk id, heading path, similarity score, snippet
- [ ] `--limit`, `--output json`, `--fields`
- [ ] Integration test with mock provider

### 5.5 Hybrid retrieval
- [ ] Combine FTS results (Phase 3) with vector similarity results
- [ ] `search` command gains `--mode hybrid` flag
- [ ] Reciprocal rank fusion or simple score combination for ranking
- [ ] Integration test: hybrid search returns results from both FTS and vector paths

### 5.6 Duplicate detection and clustering
- [ ] `vectors duplicates` command: find chunk pairs above a similarity threshold
- [ ] `cluster` command: run clustering in application code (k-means or HDBSCAN), persist cluster ids and labels back to cache
- [ ] Clustering is a derived artifact, not a source of truth
- [ ] `--output json` for both commands

---

## Phase 6: Hardening

**Goal:** Production readiness — cross-platform file watching, fuzz testing, performance tuning, migration testing, and CLI polish.

**Depends on:** All prior phases.
**Design refs:** §4 (concurrency/watcher), §16 (performance), §19 (test strategy)

### 6.1 File watcher
- [ ] `watch` command or `--watch` flag: start `notify`-based file watcher
- [ ] Batch and coalesce events before acquiring write lock
- [ ] On startup: reconcile watcher state against directory scan
- [ ] Cross-platform testing: Linux (inotify), macOS (FSEvents), Windows (ReadDirectoryChanges)
- [ ] Handle edge cases: rapid-fire saves, file replacements (some editors), large batch changes

### 6.2 Fuzz testing
- [ ] `cargo-fuzz` targets for: Markdown parser, frontmatter extractor, link parser, chunker
- [ ] Goal: no panics, no infinite loops, no memory safety violations on arbitrary input
- [ ] Add any crash-inducing inputs as regression test cases

### 6.3 Performance tuning
- [ ] Benchmark full scan + index on a large vault (1000+ notes)
- [ ] Profile and optimize hot paths: parsing, link resolution, FTS sync
- [ ] Tune batch transaction sizes for indexing and embedding
- [ ] Verify WAL mode performance under concurrent read/write
- [ ] Benchmark search latency (FTS, vector, hybrid)

### 6.4 Migration testing
- [ ] Test additive migration: add a column, verify existing data preserved
- [ ] Test breaking migration: change schema version past threshold, verify clean rebuild
- [ ] Test downgrade detection: newer DB + older binary = clear error message

### 6.5 CLI polish
- [ ] `describe` or `help --json` command for runtime schema introspection
- [ ] Consistent error messages with actionable guidance
- [ ] Input hardening: validate paths, reject control characters, reject path traversal
- [ ] `--dry-run` on all mutating commands (move, reindex, repair)
- [ ] Agent-oriented documentation: ship `AGENTS.md` or similar with invariants for automated consumers
- [ ] Shell completions via `clap_complete`

### 6.6 Comprehensive integration test suite
- [ ] All test vaults produce expected results end-to-end
- [ ] Reindex idempotency across all vaults
- [ ] Rebuild equivalence: incremental vs. from-scratch produce identical cache state
- [ ] CLI JSON output snapshot tests for every command
- [ ] Doctor reports zero issues on clean, well-formed vaults

---

## Dependency graph

```
Phase 1 (Core indexing)
  ├── Phase 2 (Graph operations)
  ├── Phase 3 (Search) ──── Phase 5 (Vectors)
  └── Phase 4 (Properties/Bases)
                                    ↘
                               Phase 6 (Hardening) ← all phases
```

After Phase 1, Phases 2/3/4 can proceed in parallel.
Phase 5 requires Phase 3. Phase 6 follows all others.
