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

### 4.5 Full Bases expression language

**Depends on:** Phase 4.4 complete.
**Refs:** `references/bases_syntax.md` (expression syntax, operators, date arithmetic), `references/bases_functions.md` (all global functions, type methods, file/link/date/string/number/list/object/regex APIs), `references/bases_formulats.md` (formula property creation, referencing, examples)

- [x] **Expression parser**: hand-rolled recursive descent tokenizer + parser for the full Obsidian expression syntax (arithmetic, comparison, boolean, string concatenation, unary operators, parentheses, array/object literals)
- [x] **Expression evaluator**: tree-walking evaluator with `serde_json::Value` runtime type supporting null, bool, number, string, list, object; date as ms timestamp
- [x] **Global functions**: `if()`, `now()`, `today()`, `date()`, `duration()`, `number()`, `max()`, `min()`, `list()`, `link()`, `file()`, `escapeHTML()`, `html()`, `image()`, `icon()`
- [x] **String methods**: `.length`, `.contains()`, `.containsAll()`, `.containsAny()`, `.startsWith()`, `.endsWith()`, `.isEmpty()`, `.lower()`, `.title()`, `.trim()`, `.replace()`, `.repeat()`, `.reverse()`, `.slice()`, `.split()`, `.matches()`
- [x] **Number methods**: `.abs()`, `.ceil()`, `.floor()`, `.round()`, `.toFixed()`, `.isEmpty()`
- [x] **List methods**: `.length`, `.contains()`, `.containsAll()`, `.containsAny()`, `.filter()`, `.map()`, `.reduce()`, `.flat()`, `.join()`, `.slice()`, `.sort()`, `.reverse()`, `.unique()`, `.isEmpty()`
- [x] **Date type**: field access (`.year`, `.month`, `.day`, `.hour`, `.minute`, `.second`), methods (`.format()`, `.date()`, `.time()`, `.relative()`, `.isEmpty()`), date arithmetic with durations (`now() - "7d"`)
- [x] **Any/Object methods**: `.isTruthy()`, `.isType()`, `.toString()`, `.isEmpty()`, `.keys()`, `.values()`
- [x] **NoteRecord expansion**: add `file_size`, `tags`, `links` fields; batch-load from DB
- [x] **File field access**: `file.name`, `file.basename`, `file.folder`, `file.size`, `file.ext`, `file.mtime`, `file.ctime`, `file.tags`, `file.links`, `file.properties`, `file.path`
- [x] **File methods**: `file.hasTag()`, `file.hasProperty()`, `file.inFolder()`, `file.hasLink()`, `file.asLink()`
- [x] **Formula references**: `formula.X` (forward references produce null; no cycle detection needed for sequential evaluation)
- [x] **Filter expression upgrade**: `!=` operator added; filter string parser handles `==` → `=` translation; `file.hasTag()` and `file.inFolder()` translated to SQL-pushable filters
- [x] **Regex support**: regex literals `/pattern/flags` in tokenizer/parser; `.matches()` method with case-insensitive flag support
- [x] **Link methods**: `.asFile()` (resolves wikilink string to file object via vault-wide index), `.linksTo()` (checks outbound links of the source note)

#### 4.5.1 Custom Bases source types (extension point for Phase 9.15+)

The built-in Bases evaluator queries vault files as its data source. Phases 9.15 (TaskNotes) and potentially other plugins require registering **custom source types** that provide non-file-based row sets to the Bases query engine.

- [ ] `BasesSource` trait: `fn rows(&self, filter: &Filter) -> Result<Vec<Row>>` — pluggable data source that can produce rows for Bases evaluation
- [ ] Built-in source: `FileSource` — queries the documents table (current behavior, extracted into the trait)
- [ ] Custom source registration: `BasesEvaluator::register_source(name, source)` — register a named source type
- [ ] Source type in `.base` files: `source.type` field selects the data source (default: `file`; custom sources like `tasknotes` are registered by their respective phases)
- [ ] Source config passthrough: `source.config` is forwarded to the source implementation (e.g., `config.type: tasknotesTaskList` for TaskNotes views)
- [ ] Custom sources participate in the same filter/sort/group/formula pipeline as file-based queries
- [ ] Custom sources can define additional computed columns (e.g., TaskNotes urgency score, days until due)

**Note:** The trait definition and `FileSource` extraction can be implemented as part of Phase 4.5. The actual custom source registrations happen in their respective phases (9.15.8 for TaskNotes).

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
- [ ] **Create note from Bases view** (matches Obsidian behavior):
  - [ ] Derive the target folder from the view's filter context — if the view has a `file.folder = "Projects"` or `file.inFolder("Projects")` filter, new notes are created in `Projects/`
  - [ ] Filter analysis: walk the filter tree to extract folder constraints; prefer the most specific folder if multiple constraints exist
  - [ ] Fallback: if no folder can be derived, use the vault root or a configurable default
  - [ ] Pre-populate frontmatter properties from the view's filter context — if the view filters on `status = "todo"`, new notes get `status: todo` in frontmatter
  - [ ] Property derivation rules: only derive from equality filters (`=`, `is`), not from range or contains filters
  - [ ] Template support: if the view has an associated template (configurable per `.base` file via `create_template` key), use it as the base
  - [ ] TUI: `n` hotkey in Bases TUI creates a new note with derived folder and properties, then opens in `$EDITOR`
  - [ ] CLI: `vulcan bases create <file.base> [--title <title>]` — create a note matching the view's context
  - [ ] `--dry-run` shows derived folder, properties, and template without creating

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
- [x] Add `aho-corasick` crate to `vulcan-core/Cargo.toml` (already a transitive dep via `regex`; making it direct)
- [x] In `suggest_mentions()`, build an `AhoCorasick` automaton from all candidate names (once, before iterating files)
- [x] Replace the inner `for candidate in candidates { source.match_indices(...) }` loop in `find_note_mentions()` with a single `automaton.find_overlapping_iter(source)` pass
- [x] Map each match back to its `MentionCandidate` via the pattern index returned by Aho-Corasick
- [x] Preserve existing filtering: `ranges_intersect(blocked, ...)`, `ranges_intersect(&occupied, ...)`, `is_word_boundary()` checks remain unchanged — they operate on match positions regardless of how matches were found
- [x] The `link_mentions` command uses the same `suggest_mentions` path, so it benefits automatically
- [x] Unit tests: existing `suggest_mentions` tests must produce identical results; add a benchmark test with 1000+ candidates

**Expected improvement:** O(C × N) → O(N) per file (Aho-Corasick is linear in input length regardless of pattern count). For 13k candidates this is potentially 1000x faster per file.

**Files:** `vulcan-core/src/suggestions.rs`, `vulcan-core/Cargo.toml`

### 8.2 Duplicate/merge candidate optimization

Reduce the O(N²) pairwise Levenshtein comparison in `suggest_duplicates`.

**Current bottleneck:** `merge_candidates()` in `vulcan-core/src/suggestions.rs` compares every pair of `NoteIdentity` filenames with a custom Levenshtein implementation (lines 857–875, Wagner-Fischer). For 13k notes this is ~90M comparisons, each involving string lowercasing and O(len₁ × len₂) dynamic programming.

**Implementation:**
- [x] Pre-compute lowercased filenames once, outside the comparison loop (currently re-lowercased per pair)
- [x] Filter candidate pairs by filename length: Levenshtein distance ≤ 1 requires `|len₁ - len₂| ≤ 1`, so skip pairs where lengths differ by more than the threshold
- [x] Group filenames by length into buckets; only compare within same-length and adjacent-length buckets
- [x] Consider a BK-tree or sorted-prefix approach for further pruning if length filtering alone is insufficient
- [x] The scoring thresholds (exact match = 1.0, alias collision = 0.95, similar title = 0.8) and distance threshold (> 1 = skip) remain unchanged
- [x] Unit tests: existing `suggest_duplicates` tests must produce identical results

**Expected improvement:** Length filtering alone reduces comparisons from O(N²) to roughly O(N × B) where B = average bucket size. For natural filename distributions this is typically 10–100x fewer comparisons.

**Files:** `vulcan-core/src/suggestions.rs`

### 8.3 Graph query caching

Eliminate redundant link scans across graph operations by caching the adjacency data.

**Current bottleneck:** `note_link_counts()` in `vulcan-core/src/graph.rs` runs a full `SELECT ... FROM links JOIN documents` to build a HashMap of (inbound, outbound) counts. This is called by `query_graph_analytics()`, `query_graph_hubs()`, `query_graph_dead_ends()`, and `query_graph_moc_candidates()` — each independently. When a user runs `graph analytics` the query is called once, but the same SQL pattern is repeated across commands with no shared cache.

**Implementation:**
- [x] Extract adjacency loading into a `GraphAdjacency` struct that holds both the `HashMap<String, (usize, usize)>` counts and the raw edge list
- [x] `GraphAdjacency::load(connection)` runs the link query once and provides methods: `inbound_count()`, `outbound_count()`, `is_orphan()`, `hubs(min_degree)`, etc.
- [x] Refactor `query_graph_analytics()`, `query_graph_hubs()`, `query_graph_dead_ends()`, `query_graph_moc_candidates()` to accept `&GraphAdjacency` instead of re-querying
- [x] For CLI dispatch: load `GraphAdjacency` once per command invocation and pass it through
- [x] Also refactor `load_indexed_notes()` to return a shared `IndexedNoteSet` that can be reused across graph operations in the same invocation
- [x] `resolve_note_identifier()` currently does a linear scan over `&[IndexedNote]` with sequential predicate matching (path → filename → alias). Build a HashMap index on first call, similar to the `ResolverIndex` pattern already used in `resolver.rs`

**Expected improvement:** Graph commands that internally compute multiple metrics go from N link-query round trips to 1. For `graph analytics` on a large vault this saves a full table scan.

**Files:** `vulcan-core/src/graph.rs`

### 8.4 Missing database indexes

Add indexes for columns that appear in WHERE/JOIN clauses across many queries but currently lack coverage.

**Current gap:** The schema in `vulcan-core/src/cache/schema.rs` has no index on `documents(extension)` despite nearly every graph, search, property, and doctor query filtering on `WHERE extension = 'md'`. Similarly, `tags(document_id)` has no index despite DELETE/JOIN operations keyed on it.

**Implementation:**
- [x] Add a new schema migration (`apply_schema_v9`) that creates:
  - `CREATE INDEX IF NOT EXISTS idx_documents_extension ON documents(extension)` — used by graph.rs, search.rs, doctor.rs, properties.rs, suggestions.rs
  - `CREATE INDEX IF NOT EXISTS idx_tags_document_id ON tags(document_id)` — used by scan.rs (DELETE), search.rs (filter), graph.rs (identity loading)
  - `CREATE INDEX IF NOT EXISTS idx_headings_document_id ON headings(document_id)` — used by scan.rs (DELETE), search.rs (heading path lookups)
  - `CREATE INDEX IF NOT EXISTS idx_block_refs_document_id ON block_refs(document_id)` — used by scan.rs (DELETE)
  - `CREATE INDEX IF NOT EXISTS idx_links_source_resolved ON links(source_document_id, resolved_target_id)` — compound index for backlink queries that JOIN on both columns
- [x] Register the migration in `MigrationRegistry`
- [x] Bump `SCHEMA_VERSION` to 9 in `vulcan-core/src/lib.rs`
- [x] Verify with `EXPLAIN QUERY PLAN` that the new indexes are used by the most common queries
- [x] Run the existing test suite to confirm no regressions

**Expected improvement:** WHERE clauses on `extension = 'md'` go from full table scan to index lookup. For 13k documents this turns many O(N) scans into O(log N) lookups. The compound link index accelerates backlink queries specifically.

**Files:** `vulcan-core/src/cache/schema.rs`, `vulcan-core/src/cache/migrations.rs`, `vulcan-core/src/lib.rs`

### 8.5 Hybrid search batch filtering

Replace per-hit filter queries in hybrid search with a single batch lookup.

**Current bottleneck:** `matches_filters()` in `vulcan-core/src/search.rs` is called once per vector hit from `hybrid_search_hits()`. Each call runs up to 3 SQL queries: one to look up document_id by path, one to check tag existence, one to check property existence. With a typical candidate_limit of 40 vector hits, this is up to 120 individual queries.

**Implementation:**
- [x] Before the vector hit filter loop, collect all vector hit paths into a `Vec<&str>`
- [x] Run a single batch query to load document_ids for all paths: `SELECT path, id FROM documents WHERE path IN (?, ?, ...)`
- [x] If tag filter is active, run a single batch query: `SELECT DISTINCT document_id FROM tags WHERE document_id IN (...) AND tag_text = ?`
- [x] If property filter is active, run a single batch query: `SELECT DISTINCT document_id FROM property_values WHERE document_id IN (...) AND key = ?`
- [x] Build a `HashSet<String>` of passing document_ids and filter vector hits against it
- [x] The existing `filtered_paths` (from keyword search pre-filtering) continues to work as a fast pre-check before the batch queries
- [x] Unit tests: existing hybrid search tests must produce identical results

**Expected improvement:** 3N individual queries → 3 batch queries. For 40 vector hits this is 120 queries → 3.

**Files:** `vulcan-core/src/search.rs`

### 8.6 Vector index hash comparison

Replace in-memory hash loading with a SQL-side comparison for identifying pending chunks.

**Current bottleneck:** `index_vectors_with_progress()` in `vulcan-core/src/vector.rs` calls `store.load_hashes()` which loads ALL chunk hashes from the vector table into a `HashMap<String, Vec<u8>>`. Then it iterates all current chunks in Rust to find mismatches. For 50k+ chunks this allocates a large HashMap and does O(N) Rust-side comparison.

**Implementation:**
- [x] Add a `pending_chunk_ids(current_chunks: &[(chunk_id, content_hash)])` method to `VectorStore` / `SqliteVecStore`
- [x] Implementation: create a temp table with current chunk_id + content_hash pairs, then `SELECT chunk_id FROM temp WHERE NOT EXISTS (SELECT 1 FROM vectors_table WHERE vectors_table.chunk_id = temp.chunk_id AND vectors_table.content_hash = temp.content_hash)`
- [x] Similarly for stale detection: `SELECT chunk_id FROM vectors_table WHERE chunk_id NOT IN (SELECT chunk_id FROM temp)`
- [x] This avoids loading all hashes into memory and lets SQLite use its indexes
- [x] Fall back to current approach if temp table creation fails (defensive)
- [x] The `delete_chunks` call for stale chunks remains unchanged
- [x] Unit tests: existing vector index tests must produce identical results

**Expected improvement:** Eliminates O(N) memory allocation for hash HashMap; comparison done in SQLite with index support. Most beneficial when the majority of chunks are already indexed (common case for incremental re-index).

**Files:** `vulcan-embed/src/sqlite_vec.rs`, `vulcan-core/src/vector.rs`

### 8.7 Scan phase: further SQLite write optimization

Investigate and apply remaining SQLite tuning for bulk insert workloads.

**Context:** The scan phase currently achieves ~1100 files/s on fresh index but degrades from ~1500 to ~1100 as the B-tree grows. Link resolution takes ~16s for ~13k files due to per-row FK-validated UPDATEs.

**Implementation:**
- [x] Profile the scan write phase using the 10K-note synthetic vault (frontmatter + links); bottleneck is B-tree growth under bulk inserts — no perf/flamegraph needed as benchmarking was sufficient
- [x] Test disabling FK checks during bulk scan (`PRAGMA foreign_keys = OFF` within the scan transaction, re-enable after) — FKs are validated on INSERT which adds overhead for every link/heading/tag row
- [x] Test increasing `page_size` from default 4096 to 8192 or 16384 — benchmarked: 4096→6.83s, 8192→6.53s (+26% peak throughput), 16384→6.56s (no further gain); adopted 8192
- [x] Test `PRAGMA locking_mode = EXCLUSIVE` during scan — **rejected**: holds the lock between transactions, blocking all concurrent reads (WAL normally allows these); would break concurrent commands, editor plugins, and the incremental scan's own inner connections
- [x] Benchmark each change independently; kept page_size=8192 (~4% wall-clock, ~26% peak files/s on 10K vault)
- [x] Document findings: page_size=8192 comment added to configure_connection; FK disable in scan.rs

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

- [x] **Keybinding fix:** change note picker quit from `Esc | q` to `Esc`-only, so `q` can be typed in search queries
- [x] `vulcan edit <note>`: resolve note by path/filename/alias, open in `$VISUAL`/`$EDITOR`/`vi`
- [x] If `<note>` is ambiguous or omitted: spawn the existing note picker TUI, Enter opens selected note in editor
- [x] `vulcan edit --new <path>`: create a new empty note (or from template if 9.4.3 is implemented), open in editor
- [x] After editor exits: run an incremental rescan of the edited file to update the cache
- [x] If auto-commit is enabled (8.3): commit the change after rescan
- [x] Reuse `open_in_editor()` and `with_terminal_suspended()` from `bases_tui.rs` — extract these into a shared `editor.rs` utility module in `vulcan-cli/src/`
- [x] Non-interactive fallback: if not a TTY, print an error rather than spawning a picker
- [x] Integration test: create a temp vault, run `edit --new`, verify file exists and cache is updated

### 9.2 `browse` command — persistent note browser TUI

A persistent TUI session that acts as a lightweight terminal Obsidian. The user searches, previews, edits, and navigates notes without leaving the TUI.

```
vulcan browse
```

**Core loop:**
- [x] Start in the note picker view (extend existing `NotePickerState` from `note_picker.rs`)
- [x] Enter opens selected note in `$EDITOR`; on editor exit, return to picker with previous query and selection preserved
- [x] After each editor exit: incremental rescan of the edited file, refresh the note list
- [x] If auto-commit is enabled (8.3): commit after each editor session

**Search mode hotkeys** (toggled in the picker's input bar):
- [x] Default / `/`: fuzzy path/alias/filename filter (current behavior)
- [x] `Ctrl-F`: full-text search mode — query runs against FTS5, results replace the note list, preview pane shows matching snippets with highlighted terms instead of raw file content
- [x] `Ctrl-T`: tag filter mode — type a tag name, fuzzy-match against all indexed tags, show notes matching the selected tag
- [x] `Ctrl-P`: property filter mode — type a property predicate (reuse the existing `where` filter syntax from `NoteQuery`), filter notes by property values

**Action hotkeys on the selected note:**
- [x] `e` or `Enter`: edit in `$EDITOR` (as above)
- [x] `m`: move/rename — inline prompt for destination path, runs the move-rewrite engine, refreshes note list
- [x] `b`: switch to a backlinks view for the selected note (list of linking notes with context, navigable)
- [x] `l`: switch to an outgoing links view for the selected note
- [x] `d`: run doctor on this specific note, show diagnostics in a temporary pane
- [x] `n`: create new note — prompt for path, open in editor, return to picker
- [x] `g`: show git log for this file (if vault is a git repo), displayed in a scrollable pane
- [x] `o`: if the selected file is a `.base` file, open it in the bases TUI (`bases tui`)

**UI details:**
- [x] Status bar at bottom: vault name, total note count, filtered count, last scan timestamp, current search mode indicator
- [x] Footer keybinding hints update to reflect current mode
- [x] Resize-safe layout (reuse `ratatui` constraint-based layout)

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

- [x] Add `[git]` section to `VaultConfig` with `GitConfig` struct: `auto_commit: bool`, `trigger: GitTrigger`, `message: String`, `scope: GitScope`, `exclude: Vec<String>`
- [x] Add `[git]` section to `DEFAULT_CONFIG_TEMPLATE` (commented out, with defaults shown)
- [x] New module `vulcan-core/src/git.rs`:
  - `is_git_repo(vault_root) -> bool`: check for `.git` directory or `git rev-parse --git-dir`
  - `auto_commit(paths, config, action, changed_files) -> Result<AutoCommitReport>`: stage files, create commit
  - `git_log(vault_root, file_path, limit) -> Result<Vec<GitLogEntry>>`: file history for browse TUI
  - `git_status(vault_root) -> Result<GitStatusReport>`: uncommitted changes summary
  - Shell out to `git` CLI (not libgit2) to keep dependencies light
  - Exclude `.vulcan/` and configured exclude paths from staging
- [x] `AutoCommitReport` struct: `committed: bool`, `message: String`, `files: Vec<String>`, `sha: Option<String>`
- [x] Call `auto_commit()` after successful execution of mutating commands: `move`, `update`, `unset`, `rename-property`, `merge-tags`, `rename-alias`, `rename-heading`, `rename-block-ref`, `link-mentions`, `rewrite`, `edit`, and browse TUI edits
- [x] `--no-commit` flag on all mutating CLI commands to suppress auto-commit for one invocation
- [x] If `auto_commit = true` but vault is not a git repo: emit a warning diagnostic, do not error
- [x] If `trigger = "scan"`: also commit after `scan` and `watch` detect and process external changes
- [x] Integration test: enable auto-commit in config, run a mutation, verify git log shows the commit with expected message

### 9.4 Additional CLI commands

#### 9.4.1 `diff` — single-note change view

```
vulcan diff [note] [--since <checkpoint>]
```

- [x] Show what changed in a specific note since last scan, checkpoint, or git HEAD
- [x] If git is available: show `git diff` for the file, rendered with context
- [x] If no git: fall back to comparing current content against cached content hash (show "changed" / "unchanged" / "new")
- [x] `--output json` support
- [x] Builds on existing `changes` command but focused on a single note with richer output

#### 9.4.2 `inbox` — quick capture

```
vulcan inbox <text>
vulcan inbox --file <path>     # append file contents
echo "idea" | vulcan inbox -   # read from stdin
```

- [x] Append text to a configurable inbox note
- [x] Config in `.vulcan/config.toml`:
  ```toml
  [inbox]
  path = "Inbox.md"         # relative to vault root
  format = "- {text}"       # template for each entry; supports {text}, {date}, {time}, {datetime}
  timestamp = true           # prepend ISO timestamp to each entry
  heading = "## Inbox"       # optional: append under this heading (create if missing)
  ```
- [x] Create the inbox note if it doesn't exist
- [x] Incremental rescan after append
- [x] Auto-commit if enabled
- [x] `--output json` returns `{ "path": "Inbox.md", "appended": true }`

#### 9.4.3 `template` — create note from template

```
vulcan template [name] [--path <output-path>]
vulcan template --list
```

- [x] Templates stored in `.vulcan/templates/` as regular markdown files
- [x] Template variables: `{{title}}` (derived from output path), `{{date}}`, `{{time}}`, `{{datetime}}`, `{{uuid}}`
- [x] `--list` shows available templates
- [x] If `--path` is omitted, prompt for path (or use template's own filename with date prefix)
- [x] After creation: open in `$EDITOR` if TTY, then rescan
- [x] Auto-commit if enabled

#### 9.4.4 `open` — open note in Obsidian

```
vulcan open [note]
```

- [x] Open a note in the Obsidian desktop app via `obsidian://open?vault=<name>&file=<path>` URI
- [x] Vault name derived from folder name or `.obsidian/` config
- [x] Uses `xdg-open` (Linux), `open` (macOS), or `start` (Windows) to launch the URI
- [x] Note resolution follows the same path/filename/alias/picker logic as other commands
- [x] Useful for quickly jumping from CLI analysis to visual Obsidian editing

### 9.5 Refresh ergonomics and config layering

Keep the cache fresh automatically for day-to-day CLI use, and split shared versus device-local config cleanly.

- [x] Add `[scan]` section to `VaultConfig` with `ScanConfig { default_mode, browse_mode }`
- [x] Add optional `.vulcan/config.local.toml` loaded after `.vulcan/config.toml`
- [x] Precedence becomes: `.vulcan/config.local.toml` > `.vulcan/config.toml` > `.obsidian/app.json` > built-in defaults
- [x] Default `.vulcan/.gitignore` ignores `config.local.toml` while tracking `config.toml`
- [x] Add global CLI override `--refresh <off|blocking|background>`
- [x] Automatically run incremental scans before one-shot cache-backed commands by default
- [x] `browse` opens on current cache contents and, when configured for `background`, performs an incremental scan in the background and refreshes the TUI in place on completion
- [x] Update runtime help, roadmap, design doc, and CLI guide for the new refresh/config semantics

### 9.6 Advanced search engine — Obsidian-compatible operators and query syntax

Bring vulcan's search closer to Obsidian's hybrid search engine so users can transfer query habits between tools, and so `browse` Ctrl-F becomes a powerful vault-wide search.

**Reference:** `references/search.md` (Obsidian search documentation).

**Design decisions:**
- **Obsidian compatibility is a goal, not a constraint.** We adopt Obsidian's operator names and semantics where they make sense for a CLI tool, but don't need 1:1 parity. Operators that rely on Obsidian-specific concepts (canvas search, embedded query blocks) are out of scope.
- **Inline operators are parsed in `prepare_search_query()`.** This extends the existing inline filter extraction (`tag:`, `path:`, `has:`) with new operators. No changes to the FTS5 schema are needed for most operators — they translate to SQL filters alongside the FTS MATCH.
- **Scope operators (`line:`, `block:`, `section:`) require chunk-level awareness.** The current FTS5 index is chunked but chunks don't map 1:1 to lines/blocks/sections. These operators need post-match filtering or secondary queries against the chunk/heading structure.
- **All surfaces share a single query engine.** The query parsing and execution changes live in `vulcan-core/src/search.rs`. The CLI (`vulcan search`), browse TUI (Ctrl-F), and HTTP API (`/search`) all call `search_vault()` with a `SearchQuery` — so improvements land everywhere at once. Surface-specific work (TUI hotkeys, API query params, CLI flags) is called out in dedicated subsections.
- **Bracket property syntax `[prop:val]` uses the same filter engine as `--where`.** Parsed bracket expressions are lowered to the same `FilterExpression` structs that `build_note_filter_clause()` already handles. This keeps property filtering semantics identical whether the user writes `--where "status = done"` or `[status:done]` inline.

#### 9.6.1 Boolean expression improvements

- [x] **Parenthesized grouping:** Parse `(A OR B) C` as grouped boolean expressions. The lexer emits `OpenParen`/`CloseParen` tokens; `compose_fts_query()` maps them to FTS5 parentheses.
- [x] **Nested negation with parens:** `-(work meetup)` excludes files matching both terms (AND-negation). Maps to FTS5 `NOT ("work" "meetup")`.
- [x] Update `--explain` output to render the parsed boolean tree in plain text, similar to Obsidian's "Explain search term" toggle. The existing `SearchPlan` struct gains a `parsed_query_explanation: Vec<String>` field with one line per operator/group. This flows through CLI rendering (`render_search_hit_explain()`), JSON output, and the HTTP API response unchanged (already serialised via `SearchReport`).

#### 9.6.2 New search operators

Extend `prepare_search_query()` to recognise additional Obsidian-style inline operators. Each operator is extracted from the token stream before FTS composition and translated into SQL filters or modified FTS expressions.

| Operator | Semantics | Implementation |
|---|---|---|
| `file:` | Match against filename (not full path). `file:.md`, `file:2024-01` | SQL: `WHERE note_filename LIKE '%' \|\| ? \|\| '%'` |
| `content:` | Restrict FTS match to chunk body, excluding title/aliases/headings columns | FTS5 column filter: `{content} : "term"` |
| `match-case:` | Case-sensitive match for the given term | Post-FTS filter: re-check hit content with exact-case comparison |
| `ignore-case:` | Explicitly case-insensitive (default behavior, but useful to override a global match-case toggle) | No-op under current defaults; flag for future use |
| `section:` | All terms must appear within the same section (text between two headings) | Group chunks by heading path; require all terms present within the same heading group |
| `line:` | All terms must co-occur on a single line | Post-FTS filter: for each hit chunk, check that at least one line contains all specified terms |
| `block:` | All terms must co-occur in the same block (paragraph) | Post-FTS filter: split chunk on blank lines, require all terms in one block |

- [x] Implement `file:` operator (SQL filename filter)
- [x] Implement `content:` operator (FTS5 column filter syntax)
- [x] Implement `match-case:` operator (post-FTS case-sensitive filter)
- [x] Implement `section:` operator (heading-group co-occurrence). Requires joining FTS hits back to `chunks.heading_path` to group chunks that share a heading ancestor; then checking that all sub-query terms appear within the same group. May need a `heading_id` or `section_id` column in `search_chunk_content` if grouping by JSON heading_path is too slow.
- [x] Implement `line:` operator (single-line co-occurrence filter). Post-FTS: for each hit chunk, split `content` on newlines and check that at least one line contains all sub-query terms.
- [x] Implement `block:` operator (paragraph co-occurrence filter). Post-FTS: split chunk content on blank-line boundaries (`\n\n`), require all terms in one block. The existing `paragraph` chunk strategy already splits on these boundaries — when chunks use that strategy, block co-occurrence is chunk co-occurrence and no post-filtering is needed.
- [x] All operators support nested sub-queries: `section:(dog cat)`, `line:(mix flour)`

#### 9.6.3 Task search operators

Search within task list items, leveraging the existing task/checkbox detection in the indexer.

- [x] `task:` — match term within any task line (`- [ ] ...` or `- [x] ...`)
- [x] `task-todo:` — match within uncompleted tasks only (`- [ ] ...`)
- [x] `task-done:` — match within completed tasks only (`- [x] ...`)
- [x] Implementation: post-FTS filter on hit snippets, or a dedicated `tasks` content column in FTS if performance requires it

#### 9.6.4 Inline property search with bracket syntax

Allow Obsidian-style `[property]` and `[property:value]` syntax inline in search queries, complementing the existing `--where` flag.

- [x] `[aliases]` → files where property `aliases` exists (equivalent to `has:aliases`)
- [x] `[status:done]` → files where `status = done` (equivalent to `--where "status = done"`)
- [x] `[status:Draft OR Published]` → property value is one of the listed values
- [x] `[aliases:null]` → property exists but has no value
- [x] Parse bracket expressions in `lex_search_query()` as a new token type; extract into property filters during `prepare_search_query()`

#### 9.6.5 Inline regex support

Allow regular expressions delimited by `/` in search queries.

- [x] `/\d{4}-\d{2}-\d{2}/` matches content via regex instead of FTS keyword
- [x] Combinable with operators: `path:/\d{4}-\d{2}-\d{2}/` matches file paths by regex
- [x] Implementation: regex terms bypass FTS and run as post-scan filters (SQLite REGEXP or Rust-side filtering on content). For large vaults, FTS results can be narrowed first if mixed with keyword terms.
- [x] Use Rust `regex` crate (already a dependency) for JS-compatible regex flavour

#### 9.6.6 Search result sorting

Add `--sort` to `vulcan search` and sort controls to `browse` Ctrl-F mode.

- [x] `--sort <field>`: `relevance` (default, BM25), `path-asc`, `path-desc`, `modified-newest`, `modified-oldest`, `created-newest`, `created-oldest`
- [x] Browse TUI: cycle sort order with a hotkey (e.g., `Ctrl-S`) in full-text search mode
- [x] Sort by relevance remains default; other sorts disable BM25 ranking and use SQL ORDER BY

#### 9.6.7 Browse TUI search integration

Wire all new search capabilities into the browse TUI's Ctrl-F mode.

- [x] All inline operators (`file:`, `content:`, `section:`, `[prop:val]`, etc.) work in the TUI search input
- [x] Status bar shows the explained/parsed query (operator breakdown) when `--explain` equivalent is toggled
- [x] Add a `Ctrl-E` toggle in Ctrl-F mode to show/hide the query explanation pane
- [x] Add a case-sensitivity toggle (e.g., `Alt-C`) that toggles global match-case in Ctrl-F mode

#### 9.6.8 `SearchQuery` struct and HTTP API updates

The `SearchQuery` struct in `vulcan-core/src/search.rs` is the single input contract shared by the CLI, browse TUI, and HTTP `/search` endpoint. New capabilities must be reflected here so all surfaces stay in sync.

- [x] Add `sort: Option<SearchSort>` field to `SearchQuery`. Enum values: `Relevance` (default), `PathAsc`, `PathDesc`, `ModifiedNewest`, `ModifiedOldest`, `CreatedNewest`, `CreatedOldest`. Used by keyword/hybrid search to choose between BM25 ranking and SQL ORDER BY.
- [x] Add `match_case: Option<bool>` field to `SearchQuery`. When `Some(true)`, all terms are treated as case-sensitive (applies to the global toggle; individual `match-case:` / `ignore-case:` inline operators override per-term). Default `None` means case-insensitive.
- [x] Extend `SearchPlan` with `parsed_query_explanation: Vec<String>` — human-readable breakdown of the parsed query (boolean structure, operators, property filters). Populated when `explain = true`.
- [x] Extend `SearchHit` with `matched_line: Option<usize>` — the 1-based line number of the best match within the chunk, when available (useful for `line:` and `match-case:` post-filters that already inspect individual lines).
- [x] HTTP `/search` endpoint (`serve.rs`): add query parameters `sort`, `match_case` mapping to the new `SearchQuery` fields. All new fields serialise into the JSON response via the existing `SearchReport` derive.
- Phase 10 daemon/web note: the axum-based `/search` route is not separate feature work. It reuses this already-established `SearchQuery` contract directly, so daemon and web layers inherit the Phase 9 CLI/serve search surface without redefining query parameters.

#### 9.6.9 Explain and diagnostics

Richer search-plan explanation for debugging complex queries across all surfaces.

- [x] `vulcan search --explain` CLI output: after the existing score breakdown, print a "Query plan" section showing the boolean tree, active operators, property filters, sort order, and regex patterns — one line per component.
- [x] JSON output (`--output json` and HTTP API): the `SearchPlan.parsed_query_explanation` array provides the same information machine-readably.
- [x] Browse TUI `Ctrl-E` explain pane (from 9.6.7) renders `parsed_query_explanation` lines in a scrollable pane.
- [x] When no results are found: the explanation includes suggestions (e.g., "did you mean `content:` instead of `contents:`?", "no tasks found in matched files for `task-todo:`").

#### 9.6.10 Cross-cutting integration notes

These are not separate tasks but constraints that apply across all 9.6 subsections:

- **Property filter unification:** Inline bracket syntax `[prop:val]` (9.6.4) and `--where "prop = val"` (existing) both lower to the same `build_note_filter_clause()` SQL generation in `properties.rs`. The bracket parser must produce identical `FilterExpression` values. Add test cases that verify equivalent results for both syntaxes.
- **Chunker/indexer implications:** The `section:` operator (9.6.2) may need a `section_id` or `heading_id` column added to `search_chunk_content` to enable efficient grouping. If added, this is a cache schema migration (bump `SCHEMA_VERSION`, add migration in `schema.rs`). The `block:` operator benefits from the existing `paragraph` chunk strategy but must also work when chunks use the `heading` or `fixed` strategies.
- **Post-FTS filter pipeline:** Operators like `match-case:`, `line:`, `block:`, `section:`, `task:`, and regex all require post-FTS filtering. Introduce a `PostFilter` trait or enum in `search.rs` that `search_vault()` applies after FTS hits are collected but before ranking/truncation. This avoids scattering filter logic across multiple call sites. The filter runs on the content of each hit chunk (already available in `SearchHit.snippet` or re-fetched from `chunks.content`).
- **`--raw-query` bypass:** When `raw_query = true`, inline operators are not parsed (existing behavior). This remains unchanged — raw mode is an escape hatch for direct FTS5 syntax.
- **Query DSL (`vulcan query`) and bases:** These use property filters only (via `NoteQuery` / `build_note_filter_clause()`), not FTS. The bracket syntax `[prop:val]` is search-only. No changes needed to the query DSL, but the shared filter engine in `properties.rs` must remain compatible as bracket expressions are lowered into it.
- **Saved reports:** `SearchQuery` is serialised into saved report definitions. New fields (`sort`, `match_case`) must have `#[serde(default)]` attributes so that existing saved reports deserialise without error.

### 9.7 Enhanced templates — Obsidian-compatible template variables and insertion

Extend the existing `template` command (9.4.3) with Obsidian-compatible template variable syntax and template-into-note insertion, so users can share templates between Obsidian and vulcan.

**Reference:** `references/templates.md` (Obsidian template documentation).

**Design decisions:**
- **Backward-compatible extension.** The existing `{{date}}`, `{{time}}`, `{{title}}` variables continue to work. Obsidian-style format strings (`{{date:YYYY-MM-DD}}`, `{{time:HH:mm}}`) are added as an extension.
- **Obsidian's template folder convention is supported but optional.** If `.obsidian/` config specifies a template folder, vulcan recognizes it alongside `.vulcan/templates/`. The `.vulcan/templates/` location takes precedence on conflict.
- **Template insertion into existing notes** is a new capability. Obsidian lets you insert a template into the active note at cursor position; vulcan's CLI equivalent appends or prepends template content to a specified note.

#### 9.7.1 Obsidian-compatible template variables

- [x] Support Moment.js-style format strings on `{{date}}` and `{{time}}`: `{{date:YYYY-MM-DD}}`, `{{time:HH:mm:ss}}`, `{{date:dddd, MMMM Do YYYY}}`
- [x] `{{date}}` and `{{time}}` are interchangeable when a format string is provided (matching Obsidian behavior): `{{time:YYYY-MM-DD}}` produces a date
- [x] Implement a subset of Moment.js format tokens: `YYYY`, `YY`, `MM`, `M`, `DD`, `D`, `dd`, `ddd`, `dddd`, `HH`, `H`, `hh`, `h`, `mm`, `m`, `ss`, `s`, `A`, `a`, `Do` (ordinal day), `MMMM`, `MMM`.
- [x] Configurable default date/time formats in `.vulcan/config.toml`:
  ```toml
  [templates]
  date_format = "YYYY-MM-DD"       # default for {{date}} without format string
  time_format = "HH:mm"            # default for {{time}} without format string
  ```
- [x] Existing variables (`{{title}}`, `{{datetime}}`, `{{uuid}}`) remain unchanged

#### 9.7.2 Template property merging

- [x] When a template contains YAML frontmatter properties, merge them into the target note's frontmatter on insertion
- [x] Merge strategy: template properties are added; existing note properties are not overwritten; list properties (e.g., `tags`) are union-merged
- [x] Template variables within frontmatter values are expanded: `date: "{{date}}"` becomes `date: "2026-03-26"`

#### 9.7.3 Template insertion into existing notes

```
vulcan template insert <template> [note]      # insert template content into note
vulcan template insert <template> --prepend    # prepend after frontmatter
vulcan template insert <template> --append     # append to end (default)
```

- [x] `vulcan template insert <template> [note]`: insert template content into an existing note (append by default)
- [x] `--prepend`: insert after frontmatter but before body content
- [x] `--append`: insert at end of file (default)
- [x] If `[note]` is omitted: spawn the note picker to select target
- [x] Template variables are expanded during insertion
- [x] Property merging (9.7.2) is applied to the target note's frontmatter
- [x] Incremental rescan and auto-commit after insertion

#### 9.7.4 Obsidian template folder discovery

- [x] If `.obsidian/` config specifies a template folder location, vulcan discovers and uses it as an additional template source
- [x] Template list (`vulcan template --list`) shows templates from both `.vulcan/templates/` and the Obsidian template folder, with source indicated
- [x] On conflict (same template name in both locations): `.vulcan/templates/` takes precedence, with a warning

### 9.8 Dataview-compatible metadata and querying

**Goal:** Full Dataview compatibility — any DQL query that works in Obsidian's Dataview plugin should produce equivalent results in Vulcan. This includes inline fields, the complete `file.*` implicit metadata namespace, list item and task extraction, the full DQL query language with all data commands, the complete expression language with ~60 built-in functions, and inline expression evaluation.

**Builds on:** Phase 4 (properties and Bases expression language provide the filter/expression evaluation engine), Phase 1 (parser pipeline for inline field and task extraction), Phase 9.6 (search operators, task search).
**Design refs:** §12b (Dataview-compatible metadata and querying), §9 (property handling), §12 (query model beyond v1)
**Reference material:** `references/obsidian-dataview/docs/` (full Dataview documentation), `references/datacore/` (Datacore successor plugin)

#### 9.8.1 Inline field extraction

Extend the parser pipeline to extract Dataview-style inline fields from note body text.

- [x] Detect `key:: value` patterns in `Text` events during the semantic pass, excluding code blocks, math blocks, and comment regions
- [x] Support parenthesized `(key:: value)` and bracket `[key:: value]` variants
- [x] Normalize inline field keys to match frontmatter property key normalization (lowercase, trimmed)
- [x] Store inline fields in `property_values` with a new `origin` column (`frontmatter`, `inline`, `inline_paren`, `inline_bracket`)
- [x] Schema migration: add `origin` column to `property_values` (default `frontmatter` for existing rows)
- [x] Handle inline fields containing link syntax (`[[Target]]`) as link-valued properties
- [x] Update property catalog to track inline field usage alongside frontmatter usage
- [x] Precedence: frontmatter properties take precedence over inline fields for typed queries; both are stored and queryable
- [x] Unit tests: all inline field variants, mixed frontmatter + inline, link-valued inline fields, fields inside code blocks (should be ignored)
- [x] Integration test: vault with Dataview-style inline fields, verify property extraction and precedence

**Automatic type inference on inline field values:**
- [x] Apply type inference during extraction: Link (`[[...]]`), Boolean (`true`/`false`), Date (ISO 8601 patterns including `yyyy-mm` month-only), Duration (unit patterns like `3 hours`, `1d 3h`), Number (numeric literals), List (comma-separated quoted strings), Text (fallback)
- [x] Unquoted comma-separated values (`a, b, c`) remain Text; only quoted (`"a", "b", "c"`) become List
- [x] Duplicate keys across frontmatter and inline fields collected into List type
- [x] Store inferred `value_type` alongside `value_text` so typed comparisons work in WHERE clauses

**Inline field parsing edge cases:**
- [x] Strip Markdown formatting tokens from field keys (`**bold**` → `bold`, `_italic_` → `italic`)
- [x] Emoji keys require bracket syntax: `[🎅:: value]`
- [x] Multiline inline field values: value ends at line break (only YAML frontmatter supports multiline)
- [x] Unit tests: type inference for each type, formatting in keys, emoji keys, unquoted vs quoted lists

#### 9.8.2 List item and task extraction

Extract **all** list items (not just tasks) as structured data, matching Dataview's `file.lists` and `file.tasks` metadata. Tasks are a subset of list items.

**List item extraction:**
- [x] Detect all list items (`-`, `*`, `+`, and numbered `1.`) during the semantic pass, including non-task items
- [x] Schema: `list_items` table — `id`, `document_id`, `text` (full text including annotations), `line_number`, `line_count` (lines spanned), `byte_offset`, `section_heading`, `parent_item_id` (nullable, for nesting), `is_task` (boolean), `block_id` (nullable, `^blockId` syntax)
- [x] Extract tags and links within list item text and store as item-scoped metadata
- [x] Track `annotated` flag: true if item text contains inline field annotations
- [x] Index on `list_items(document_id)`, `list_items(is_task)`, `list_items(parent_item_id)`
- [x] Unit tests: plain list items, nested lists, mixed task and non-task items, numbered lists
- [x] Integration test: vault with varied list items, verify `file.lists` returns all items

**Task extraction (extends list items):**
- [x] Detect task list items (`- [ ]`, `- [x]`, `- [/]`, `- [-]`, custom status characters) during the semantic pass
- [x] Schema: `tasks` table — `id`, `document_id`, `list_item_id` (foreign key to `list_items`), `status_char`, `text`, `byte_offset`, `parent_task_id` (nullable, for nested tasks), `section_heading`, `line_number`
- [x] Extract inline fields within task text (e.g., `- [ ] Buy groceries [due:: 2026-04-01]`) and store as task-scoped properties
- [x] Schema: `task_properties` table — `task_id`, `key`, `value_text`, `value_type`
- [x] Index on `tasks(document_id)`, `tasks(status_char)`, `task_properties(task_id)`, `task_properties(key)`
- [x] Task completion state mapping: `x` = done, ` ` = todo, `/` = in-progress, `-` = cancelled; configurable custom status characters via `.vulcan/config.toml`
- [x] Synthesize Dataview task fields at query time: `status` (char in brackets), `checked` (status is non-empty), `completed` (status is `x`), `fullyCompleted` (recursive subtree check), `visual` (rendered display text, defaults to `text`)
- [ ] Nested task query semantics: when a TASK query matches a parent, include child tasks in results even if children don't independently match the WHERE clause. Task hierarchy is preserved in output.
- [x] Tasks inherit page-level fields (frontmatter, inline fields) from their containing note
- [x] Tasks plugin emoji shorthand: detect `🗓️` (due), `✅` (completion), `➕` (created), `🛫` (start), `⏳` (scheduled) date annotations in task text and store as task properties with auto-parsed Date type
- [x] Tasks plugin priority levels: detect `⏫` (highest), `🔺` (high), `🔼` (medium), `🔽` (low), `⏬` (lowest) and store as `priority` task property
- [x] Tasks plugin recurrence notation: detect `🔁 every <pattern>` in task text and store as `recurrence` task property (parsing the RRULE pattern is deferred to §9.10)
- [x] Tasks plugin dependency notation: detect `⛔ <id>` (blocked by) and `🆔 <id>` (task ID) and store as task properties (dependency resolution deferred to §9.10)
- [x] Unit tests: basic tasks, nested tasks, tasks with inline fields, custom status characters
- [ ] Unit tests: `fullyCompleted` recursive check, nested task inclusion semantics, emoji shorthand date parsing, priority levels
- [x] Integration test: vault with varied task items, verify task extraction and property association

**Note:** The Obsidian Tasks plugin has a richer feature set beyond what Dataview extracts (its own query DSL in `` ```tasks `` blocks, recurring task expansion, task dependencies, custom status types). Full Tasks plugin compatibility is tracked in §9.10.

#### 9.8.3 Implicit file metadata (`file.*` namespace)

Implement the full `file.*` implicit metadata namespace that Dataview exposes on every note. These fields are synthesized at query time from existing cache tables, not stored redundantly.

- [x] `FileMetadataResolver` module: given a `document_id`, lazily resolve any `file.*` field from cache tables
- [x] `file.name` — filename without extension (from `documents`)
- [x] `file.path` — full vault-relative path (from `documents`)
- [x] `file.folder` — parent directory path (derived from `file.path`)
- [x] `file.ext` — file extension (derived from `file.path`)
- [x] `file.link` — synthetic link to the file
- [x] `file.size` — file size in bytes (from `documents` or filesystem)
- [x] `file.ctime` / `file.cday` — creation timestamp / date (filesystem or `documents`)
- [x] `file.mtime` / `file.mday` — modification timestamp / date (from `documents.modified_at`)
- [x] `file.tags` — all tags broken down by level: `#A/B/C` → `[#A, #A/B, #A/B/C]` (subtag expansion, from `tags` table)
- [x] `file.etags` — explicit tags only, not broken down: `[#A/B/C]` (from `tags` table)
- [x] `file.inlinks` — files linking to this file (reverse `links` table query)
- [x] `file.outlinks` — links from this file (`links` table)
- [x] `file.aliases` — aliases from frontmatter (from `property_values`)
- [x] `file.tasks` — all task items in file (from `tasks` table, returns task objects with full metadata)
- [x] `file.lists` — all list items including tasks (from `list_items` table, returns list item objects)
- [x] `file.frontmatter` — raw frontmatter as object (from `property_values` where `origin = 'frontmatter'`)
- [x] `file.day` — date extracted from filename (`yyyy-mm-dd` or `yyyymmdd` patterns), null if no date pattern
- [x] `file.starred` — bookmarked status (from `.obsidian/` bookmarks data if available, false otherwise)
- [x] `file.day` resolution: populated from filename date pattern (`yyyy-mm-dd`, `yyyymmdd`) OR from a frontmatter `Date` field; null otherwise
- [ ] Subtag inheritance in FROM sources: `FROM #A` matches notes with `#A`, `#A/B`, `#A/B/C`, etc.
- [x] Unit tests: each `file.*` field resolves correctly from cache data
- [ ] Integration test: DQL queries using `file.*` fields produce expected results

#### 9.8.4 Data type system and expression evaluator

Extend the expression evaluator to support Dataview's full type system and expression language. This is the foundation for DQL evaluation and inline expressions.

**Type system:**
- [ ] Extend the value representation to support all 8 Dataview types: Text, Number, Boolean, Date, Duration, Link, List, Object
- [x] Date type with sub-field access: `.year`, `.month`, `.day`, `.hour`, `.minute`, `.second`, `.millisecond`, `.weekday`, `.week`, `.weekyear`
- [x] Date literal shortcuts: `date(today)`, `date(now)`, `date(tomorrow)`, `date(yesterday)`, `date(sow)`, `date(eow)`, `date(som)`, `date(eom)`, `date(soy)`, `date(eoy)`
- [x] Duration type with compound units: `dur(1d 3h 20m)`, individual unit abbreviations (`s`, `m`, `h`, `d`, `w`, `mo`, `yr`)
- [x] Link as first-class type with metadata access via `meta(link)`: `.path`, `.display`, `.embed`, `.type`, `.subpath`
- [x] Type coercion: Date - Date → Duration, Date ± Duration → Date, Duration + Duration → Duration, String + Number → String (concatenation), String * Number → String (repeat)
- [x] Null ordering: `null` is less than all non-null values; `null` first in ascending sort, last in descending; `null` propagates through most arithmetic/function calls
- [x] GROUP BY null handling: rows with `null` group key form a separate group with `key = null`
- [ ] Date timezone semantics: `date(today)`, `date(now)`, etc. use system local timezone; `localtime(date)` converts UTC to local; timezone override configurable via `.vulcan/config.toml`
- [x] `typeof(value)` introspection returning type name strings

**Expression language extensions:**
- [x] Arithmetic operators on numbers, dates, and durations: `+`, `-`, `*`, `/`, `%`
- [x] Dotted field access: `object.field`, `object["field"]`
- [x] Array indexing: `array[0]`, `array[expr]` (0-indexed)
- [x] Link indexing: `[[Note]].field` — cross-note field access (join against linked page's metadata); returns `null` if target note doesn't exist; follows Vulcan's link resolution (shortest-path, alias matching)
- [x] Array/DataArray swizzling: `array.field` auto-maps and flattens; chained swizzling (`array.field.subfield`) maps through nested objects; null propagation through swizzles
- [x] Lambda expressions: `(arg1, arg2) => expression` for use in higher-order functions
- [ ] Column aliases: `field AS "Display Name"` in TABLE/LIST projections
- [ ] `WITHOUT ID` modifier for TABLE and LIST queries
- [ ] Keyword-escaped field access: `row["where"]` for reserved word collision (all DQL keywords must be escapable)
- [x] Field name normalization: case-insensitive, spaces/punctuation → hyphens, Markdown formatting stripped
- [x] Unit tests: each operator, type coercion rule, field access pattern, lambda evaluation, swizzling, link indexing (including missing target)

**Built-in function library (~60 functions, all auto-vectorize over arrays):**

*Type constructors:*
- [x] `date(any)`, `date(text, format)`, `dur(any)`, `number(string)`, `string(any)`, `link(path, [display])`, `embed(link, [embed])`, `elink(url, [display])`, `typeof(any)`, `object(key, value, ...)`, `list(value1, value2, ...)`

*Numeric:*
- [x] `round(n, [digits])`, `trunc(n)`, `floor(n)`, `ceil(n)`, `min(a, b, ...)`, `max(a, b, ...)`, `sum(array)`, `product(array)`, `average(array)`, `reduce(array, operand)`, `minby(array, func)`, `maxby(array, func)`

*Array/list:*
- [x] `length(array|object)`, `sort(list)`, `reverse(list)`, `unique(array)`, `flat(array, [depth])`, `slice(array, [start, [end]])`, `nonnull(array)`, `firstvalue(array)`

*Predicate/iteration:*
- [x] `contains(obj|list|string, value)`, `icontains(...)`, `econtains(...)`, `containsword(string, value)`, `all(array, [predicate])`, `any(array, [predicate])`, `none(array, [predicate])`, `filter(array, predicate)`, `map(array, func)`, `join(array, [delimiter])`

*String:*
- [x] `lower(s)`, `upper(s)`, `startswith(s, prefix)`, `endswith(s, suffix)`, `substring(s, start, [end])`, `split(s, delimiter, [limit])`, `replace(s, pattern, replacement)`, `regextest(pattern, s)`, `regexmatch(pattern, s)`, `regexreplace(s, pattern, replacement)`, `truncate(s, length, [suffix])`, `padleft(s, length, [padding])`, `padright(s, length, [padding])`

*Object:*
- [x] `extract(object, key1, key2, ...)`

*Date/duration:*
- [x] `dateformat(date, string)`, `durationformat(duration, string)`, `striptime(date)`, `localtime(date)`

*Utility:*
- [x] `default(field, value)` (null coalescing, vectorizes), `ldefault(list, value)` (non-vectorizing), `choice(bool, left, right)` (ternary), `display(any)`, `hash(seed, [text], [variant])`, `currencyformat(number, [currency])`, `meta(link)`

- [x] Function vectorization: all functions auto-apply over array arguments (e.g., `lower(["A", "B"])` → `["a", "b"]`). Exception: `ldefault(list, value)` does NOT vectorize (treats list as single value). `default(field, value)` DOES vectorize (applies element-wise).
- [x] Regex functions usable in WHERE clauses: `regextest()`, `regexmatch()`, `regexreplace()` with capture group support (`$1`, etc.)
- [x] Integration test: expression evaluation against test vault covering type coercion, functions, `file.*` access, null handling, vectorization

#### 9.8.5 DQL parser

Implement a parser for Dataview Query Language (DQL) that compiles to Vulcan's internal query AST.

- [x] Detect `` ```dataview `` fenced code blocks during parsing; store raw DQL text as block metadata
- [x] DQL tokenizer: keywords (`TABLE`, `LIST`, `TASK`, `CALENDAR`, `FROM`, `WHERE`, `SORT`, `GROUP BY`, `FLATTEN`, `LIMIT`, `ASC`, `DESC`, `ASCENDING`, `DESCENDING`, `AND`, `OR`, `NOT`, `WITHOUT`, `ID`, `AS`), identifiers, string literals, numbers, date/duration literals, operators, parentheses, links (`[[...]]`)
- [ ] DQL parser: recursive descent parser producing the internal query AST
  - [x] Query type: `TABLE`, `LIST`, `TASK`, `CALENDAR`
  - [x] `WITHOUT ID` modifier for TABLE and LIST
  - [x] Column/display expressions with `AS "alias"` support
  - [x] FROM clause: tag sources (`#tag`, includes subtags), folder sources (`"folder"`, includes subfolders), single-file sources (`"folder/File"`), incoming link sources (`[[note]]`), outgoing link sources (`outgoing([[note]])`), self-reference (`[[]]`, `[[#]]`), boolean combinations (`AND`, `OR`, `-`/`!`), parenthesized grouping
  - [x] WHERE clause: full expression language — field access (dotted paths, array indexing, link indexing `[[Note]].field`), comparisons (`=`, `!=`, `<`, `>`, `<=`, `>=`), boolean logic (`AND`, `OR`, `!`), arithmetic (`+`, `-`, `*`, `/`, `%`), function calls with arbitrary arguments, lambda expressions
  - [x] SORT clause: field + direction (`ASC`/`DESC`/`ASCENDING`/`DESCENDING`), multiple sort keys with comma separation
  - [x] GROUP BY clause: field or `(expression) AS name`
  - [x] FLATTEN clause: field or `(expression) AS name`
  - [x] LIMIT clause: integer cap on result count
  - [x] TABLE column expressions: arbitrary expressions evaluated per note (reuse extended expression evaluator)
  - [x] LIST display expression: optional per-note expression
- [ ] Compile FROM clauses to source/filter primitives (tag → `tags` table filter, folder → `documents.path` prefix, links → `links` table join, outgoing → forward `links` join)
- [ ] Compile WHERE expressions to `FilterExpression` structs (shared with Bases and `--where` CLI flag)
- [x] Data commands executed in source order (except FROM which is always first); multiple WHERE, SORT, FLATTEN, GROUP BY clauses allowed and composed sequentially
- [x] Computed GROUP BY: `GROUP BY (expr) AS name` with arbitrary expression
- [x] Computed FLATTEN: `FLATTEN (expr) AS name` assigns flattened result to a new field; if expression returns non-array, treat as single-element array
- [x] Multiple blocks per note: a note can contain multiple `` ```dataview `` blocks; `--block <n>` selects by 0-based index, default evaluates all
- [ ] Error recovery: malformed DQL produces diagnostics, not panics
- [ ] Unit tests: parse each clause type, boolean FROM combinations, nested WHERE expressions, lambda expressions, link indexing, `WITHOUT ID`, `AS` aliases, computed GROUP BY/FLATTEN, multiple data commands, malformed input
- [x] Integration test: round-trip DQL parse → AST → evaluation against a test vault

#### 9.8.6 DQL evaluation and CLI surface

Execute parsed DQL queries against the cache and expose results via CLI.

- [x] `vulcan dataview eval <file> [--block <n>]` — evaluate a DQL code block from a specific note (by block index or the first/only block)
- [x] `vulcan dataview query <dql-string>` — evaluate a DQL query string directly from the command line
- [ ] TABLE output: columnar table in human mode, array-of-objects in `--output json`; `WITHOUT ID` suppresses file link column
- [ ] LIST output: note list with optional expression values; `WITHOUT ID` shows only the expression value
- [ ] TASK output: task items grouped by source note, with status, text, `visual`, and all task metadata fields (`checked`, `completed`, `fullyCompleted`); nested task inclusion semantics (children included when parent matches)
- [ ] CALENDAR output: JSON with date-keyed entries (human mode shows a flat date-grouped list; calendar rendering is a WebUI concern)
- [x] GROUP BY support: produces `{ key, rows }` objects; `rows.field` extracts list of values; aggregation functions (`sum(rows.field)`, `length(rows)`, etc.) work over grouped rows
- [x] FLATTEN support: list expansion into individual result rows; multiple FLATTEN clauses compose sequentially; `FLATTEN expr AS name` assigns to a new field
- [x] LIMIT support: cap result count (applied after all other data commands)
- [x] SORT with multi-key tiebreaking and correct type-aware ordering
- [x] `file.*` namespace fully accessible in all expressions (WHERE, TABLE columns, SORT, GROUP BY, FLATTEN)
- [x] Link indexing in expressions: `[[Note]].field` resolves field from the linked note's metadata
- [ ] Diagnostics for unsupported DQL features surfaced in output
- [x] `--output json` on all subcommands
- [x] Empty result handling: TABLE with 0 results shows headers + result count; LIST with 0 results shows empty; TASK with 0 results shows nothing
- [ ] Result count display: configurable via Dataview settings (`displayResultCount`); show count in TABLE/TASK headers by default
- [x] Configurable column names: `primaryColumnName` (default `"File"`), `groupColumnName` (default `"Group"`) from Dataview settings
- [ ] Integration tests: TABLE, LIST, TASK, CALENDAR queries; GROUP BY with aggregation and null keys; FLATTEN with nested arrays and non-array expressions; multi-clause queries; `WITHOUT ID`; link indexing; empty results; all against test vault with known results

#### 9.8.7 Inline expression evaluation

Support Dataview inline expressions (`` `= expr` ``) for note rendering and query contexts.

- [x] Detect inline expressions (backtick-delimited text starting with configurable prefix, default `=`) during the semantic pass; store as inline expression metadata
- [x] Configurable inline query prefix from Dataview settings (`inlineQueryPrefix`, default `"="`); also detect inline DataviewJS prefix (`inlineJsQueryPrefix`, default `"$="`) when `dataviewjs` feature is enabled
- [x] `this` binding: within an inline expression, `this` resolves to the current note's full metadata (frontmatter + inline fields + `file.*` implicit metadata)
- [x] Reuse the extended expression evaluator (9.8.4) with the `this` context binding and full function library
- [x] Known limitation: inline expressions store the expression text, not the evaluated result — other notes cannot query the result of an inline expression (this matches Dataview behavior)
- [x] `vulcan dataview inline <file>` — evaluate all inline expressions in a note, output results alongside source expressions
- [x] In `--output json` mode, include evaluated inline expression results in note metadata
- [x] Diagnostics for expressions that fail to evaluate (type errors, missing fields)
- [x] Unit tests: `this.property` access, `this.file.name`, nested field access, function calls, missing field handling
- [x] Integration test: note with inline expressions, verify evaluation results

#### 9.8.8 DataviewJS evaluation (compile-time feature flag)

Evaluate `` ```dataviewjs `` code blocks using an embedded, sandboxed JavaScript runtime. Gated behind a `dataviewjs` Cargo feature flag — default builds detect and diagnose these blocks without a JS dependency.

**Detection and fallback (always available):**
- [x] Detect `dataviewjs` code blocks during parsing
- [x] Store as block metadata with `language = "dataviewjs"`
- [x] When feature is not compiled in: emit diagnostic "DataviewJS blocks require the `dataviewjs` feature flag"
- [x] Exclude from FTS indexing (code, not content)
- [x] Unit test: dataviewjs block detected and diagnosed without feature flag

**JS runtime integration (behind `dataviewjs` feature):**
- [x] Add `dataviewjs` feature flag to `vulcan-core/Cargo.toml` and `vulcan-cli/Cargo.toml`
- [ ] Embed JS runtime: Boa (pure Rust, preferred for build simplicity) or rquickjs/QuickJS bindings (alternative if performance requires it)
- [ ] Sandbox constraints: no filesystem access, no network access, no `eval` of external scripts
- [ ] Execution timeout: configurable via `.vulcan/config.toml` (default 5 seconds per block)
- [ ] Memory limit: cap JS heap allocation to prevent runaway scripts

**`dv` API object — query methods:**
- [ ] `dv.pages(source?)` — return DataArray of page objects matching a DQL FROM source (or all pages)
- [ ] `dv.page(path)` — return a single page's metadata object
- [ ] `dv.current()` — return current note's metadata (`this` equivalent)
- [ ] `dv.query(dql, [file], [settings])` — evaluate DQL, return `{ successful: boolean, value: result }` or `{ successful: false, error: string }`
- [ ] `dv.tryQuery(dql, [file], [settings])` — like `dv.query()` but throws on failure
- [ ] `dv.queryMarkdown(dql, [file], [settings])` — evaluate DQL, return rendered Markdown string
- [ ] `dv.tryQueryMarkdown(dql, [file], [settings])` — like `dv.queryMarkdown()` but throws on failure
- [ ] `dv.execute(dql)` — shorthand: evaluate DQL and render results directly (reuses 9.8.6 evaluation engine)
- [ ] Page objects expose frontmatter, inline fields, and full `file.*` namespace — same fields as DQL queries

**`dv` API object — render methods:**
- [ ] `dv.table(headers, rows)` — render table output (CLI: columnar; JSON: array-of-objects)
- [ ] `dv.list(items)` — render list output
- [ ] `dv.taskList(tasks, groupByFile?)` — render task list output
- [ ] `dv.paragraph(text)`, `dv.header(level, text)`, `dv.el(element, text, [attrs])`, `dv.span(text)` — text/element output (map to plain text in CLI)
- [ ] `dv.container` — reference to output container (CLI: output buffer object; WebUI: DOM element; used for CSS class manipulation)

**`dv` API object — I/O and view methods:**
- [ ] `dv.io.load(path)` — read a note's content as string (read-only, within vault boundary only)
- [ ] `dv.io.csv(path, [originFile])` — load and parse a CSV file, return DataArray of row objects
- [ ] `dv.io.normalize(path, [originFile])` — resolve a vault-relative path
- [ ] `dv.view(path, [input])` — load and execute an external JS file from the vault; `path` relative to vault root; optional `input` object available to loaded script; associated CSS file (`<path>.css`) loaded if present; vault-boundary enforcement applies

**`dv` API object — utility methods:**
- [ ] `dv.date(input)`, `dv.duration(input)` — type constructors matching DQL semantics
- [ ] `dv.compare(a, b)`, `dv.equal(a, b)` — Dataview comparison/equality semantics
- [ ] `dv.clone(value)` — deep clone a value
- [ ] `dv.func.*` — namespace exposing all DQL built-in functions (e.g., `dv.func.contains()`)
- [ ] `dv.luxon` — expose date/time library API (Luxon-compatible or Vulcan equivalent)

**DataArray implementation:**
- [ ] DataArray wraps query results with chainable methods: `.where(pred)`, `.filter(pred)`, `.map(fn)`, `.flatMap(fn)`, `.sort(key, [dir])`, `.groupBy(key)`, `.unique()`, `.distinct()`, `.limit(n)`, `.slice(start, [end])`, `.concat(other)`, `.indexOf(value)`, `.find(pred)`, `.findIndex(pred)`, `.includes(value)`, `.join(sep)`, `.every(pred)`, `.some(pred)`, `.none(pred)`
- [ ] Dataview-specific methods: `.sortInPlace(key, [dir])`, `.groupIn(key)` (recursive top-down grouping), `.mutate(fn)` (in-place mutation), `.into(key)` (map without flattening), `.expand(fn)` (recursive expansion), `.forEach(fn)`, `.array()` (convert to plain array), `.values` (raw array access)
- [ ] Swizzling: `dataArray.field` auto-maps and flattens; chained swizzling works through nested objects

**CLI surface:**
- [ ] `vulcan dataview eval <file> [--block <n>]` evaluates DataviewJS blocks when feature is compiled in (same command as DQL, dispatches by block language)
- [ ] `vulcan dataview query-js <js-string>` — evaluate a JS snippet directly from the command line
- [ ] `--output json` on both subcommands
- [ ] Diagnostics for runtime errors, timeout, and sandbox violations

**Testing:**
- [ ] Unit tests: `dv.pages()`, `dv.page()`, `dv.current()`, `dv.table()`, `dv.list()`, `dv.taskList()`, `dv.execute()`
- [ ] Integration test: DataviewJS blocks in test vault produce expected output
- [ ] Sandbox test: verify filesystem/network access is blocked, timeout triggers correctly
- [ ] Feature flag test: build without `dataviewjs`, verify detection-only behavior

#### 9.8.9 Dataview plugin settings import

Read and respect Dataview's per-vault configuration from `.obsidian/plugins/dataview/data.json` for seamless migration.

- [x] Discover and parse `.obsidian/plugins/dataview/data.json` during vault initialization
- [x] Import settings: `inlineQueryPrefix` (default `"="`), `inlineJsQueryPrefix` (default `"$="`), `enableDataviewJs`, `enableInlineDataviewJs`, `taskCompletionTracking`, `taskCompletionUseEmojiShorthand`, `taskCompletionText`, `recursiveSubTaskCompletion`, `displayResultCount`, `defaultDateFormat`, `defaultDateTimeFormat`, `maxRecursiveRenderDepth`, `primaryColumnName`, `groupColumnName`
- [x] Merge into runtime config with `.vulcan/config.toml` overrides taking precedence
- [x] Settings not found in the Dataview config fall back to Vulcan defaults
- [x] Unit test: parse sample `data.json`, verify settings merge and precedence
- [x] Integration test: vault with custom Dataview settings, verify inline prefix and display settings are respected

#### 9.8.10 Cross-cutting integration

- [x] **Search:** DQL code blocks and inline expressions are stored as metadata but excluded from FTS content indexing (they are queries, not prose). Inline field *values* are included in FTS.
- [ ] **Doctor:** Report notes with DQL blocks that fail to parse. Report inline fields with type inconsistencies against the property catalog. Report DataviewJS blocks (diagnosed when feature not compiled in).
- [ ] **Browse TUI:** Notes with DQL blocks could show evaluated query results in a detail pane (future enhancement, not required for initial implementation).
- [ ] **HTTP API:** `GET /{id}/dataview/eval` endpoint accepts a DQL string and returns structured results. Inline expression evaluation available via note render endpoints.
- [x] **Property queries:** Inline fields and `file.*` fields are queryable via the existing `--where` filter surface. `vulcan notes --where "due < date(today)"` finds notes where the `due` inline field is in the past. `vulcan notes --where "file.size > 10000"` finds large notes.
- [ ] **Bases interop:** Bases views and DQL queries share the same expression evaluation engine and filter primitives. A Bases view and a DQL TABLE query with equivalent logic should produce identical results.
- [ ] **Dataview test vault:** `tests/fixtures/vaults/dataview/` must exercise all features: inline fields (all variants, type inference, formatting edge cases), list items (plain and task, nested), `file.*` metadata access (including `file.day`, `file.tags` subtag expansion), DQL queries (TABLE, LIST, TASK, CALENDAR), GROUP BY (with null keys, computed expressions), FLATTEN (with non-array expressions, sequential composition), inline expressions (with configurable prefix), function calls (including vectorization, regex functions in WHERE), link indexing (`[[Note]].field` including missing targets), date/duration arithmetic, null ordering, Tasks plugin emoji shorthand, and DataviewJS blocks (evaluated when feature is compiled in, diagnosed otherwise).

#### 9.8 Recommended implementation order

1. **Inline field type inference** (9.8.1 additions) — add automatic type detection for inline field values so typed comparisons work from the start.
2. **List item extraction** (9.8.2 list items) — extend the parser to capture all list items, not just tasks. Migrate `tasks` to reference `list_items`.
3. **Implicit file metadata** (9.8.3) — implement `FileMetadataResolver` so `file.*` fields are available to the expression evaluator.
4. **Type system and expression evaluator** (9.8.4) — extend value representation, add Date/Duration/Link types, implement the full function library with vectorization, add lambda support, link indexing, swizzling, and null ordering.
5. **DQL parser** (9.8.5) — tokenizer and recursive descent parser producing the internal query AST, including computed GROUP BY/FLATTEN.
6. **DQL evaluation and CLI** (9.8.6) — wire the parser to the evaluator, implement GROUP BY / FLATTEN / LIMIT semantics with null key handling, add CLI commands.
7. **Inline expressions** (9.8.7) — configurable prefix, `this` binding, and CLI evaluation command.
8. **Dataview settings import** (9.8.9) — read `.obsidian/plugins/dataview/data.json` so all configurable behavior respects per-vault settings.
9. **DataviewJS** (9.8.8) — detection always; sandboxed JS evaluation with full `dv` API, DataArray, and `dv.view()` behind `dataviewjs` feature flag.
10. **Cross-cutting integration** (9.8.10) — search exclusions, doctor checks, API endpoints, comprehensive test vault.

### 9.9 Templater-compatible template engine

**Goal:** Support Templater-style `<% %>` template syntax in Vulcan's template system, allowing users to share templates between Obsidian (with Templater) and Vulcan. The DataviewJS sandbox (9.8.8) provides the JS runtime foundation; Templater reuses it for `<%* %>` execution commands.

**Builds on:** Phase 9.7 (enhanced templates), Phase 9.8.8 (DataviewJS sandbox for JS execution).
**Design refs:** §12b (expression evaluator), existing `template` command (9.4.3/9.7)
**Reference material:** `references/Templater/` (Templater source and documentation)

#### 9.9.1 Template syntax parsing

- [ ] Parse Templater command tags: `<% expr %>` (interpolation), `<%* code %>` (JS execution), `<%+ expr %>` (dynamic/deferred)
- [ ] Whitespace control: `<%_`/`_%>` (trim all whitespace), `<%-`/`-%>` (trim one newline)
- [ ] Detect Templater syntax in `.vulcan/templates/` and Obsidian template folder
- [ ] Backward compatibility: existing `{{date}}`, `{{title}}` variables continue to work; Templater syntax is an extension
- [ ] Templater folder discovery: read Templater settings from `.obsidian/plugins/templater-obsidian/data.json` for template folder location and user script folder

#### 9.9.2 `tp` API object — native modules

Implement the `tp` namespace natively (no JS required) for the most common template functions:

**tp.date:**
- [ ] `tp.date.now(format?, offset?, reference?, reference_format?)` — current/relative date with Moment.js-compatible formatting (reuse 9.7.1 format engine)
- [ ] `tp.date.tomorrow(format?)`, `tp.date.yesterday(format?)` — convenience shortcuts
- [ ] `tp.date.weekday(format?, weekday_number?, reference?, reference_format?)` — specific weekday

**tp.file:**
- [ ] `tp.file.title` — filename without extension
- [ ] `tp.file.path(absolute?)` — file path (vault-relative or absolute)
- [ ] `tp.file.folder(absolute?)` — parent folder name or path
- [ ] `tp.file.creation_date(format?)`, `tp.file.last_modified_date(format?)` — file timestamps
- [ ] `tp.file.content` — full file content
- [ ] `tp.file.tags` — all tags in file
- [ ] `tp.file.exists(filepath)` — check if file exists in vault
- [ ] `tp.file.include(filepath)` — include another template (recursive, depth limit 10)
- [ ] `tp.file.create_new(template, filename, open_new?, folder?)` — create new note from template
- [ ] `tp.file.move(new_path)`, `tp.file.rename(new_name)` — file operations (reuse move-rewrite engine)
- [ ] `tp.file.cursor(order?)` — insert cursor position placeholder (meaningful in editor contexts; no-op in non-interactive CLI)

**tp.frontmatter:**
- [ ] `tp.frontmatter.<key>` — direct access to frontmatter properties (reuse property resolver)
- [ ] Bracket notation for keys with spaces: `tp.frontmatter["key name"]`

**tp.system (CLI-adapted):**
- [ ] `tp.system.prompt(text, default?, throw_on_cancel?, multi_line?)` — CLI: read from stdin or use `--var key=value` flag; TUI: show input dialog
- [ ] `tp.system.suggester(items, values, ...)` — CLI: use existing note picker or `--var` flag; TUI: show selection picker
- [ ] `tp.system.clipboard()` — read system clipboard (platform-dependent, best-effort)

#### 9.9.3 `tp` API object — JS-dependent modules (behind `dataviewjs` feature)

These require the sandboxed JS runtime and are only available when `--features dataviewjs` is compiled:

- [ ] `<%* %>` execution commands — arbitrary JS with `tR` output accumulator
- [ ] `tp.web.request(url, json_path?)` — sandboxed HTTP GET (allowlist-based, configurable)
- [ ] `tp.web.daily_quote()`, `tp.web.random_picture(size?, query?)` — convenience web functions
- [ ] User script functions: load `.js` files from configured scripts folder as `tp.user.<name>(args)`
- [ ] System command user functions: execute shell commands with template variable substitution (requires explicit opt-in via config, disabled by default for security)
- [ ] `tp.hooks.on_all_templates_executed(callback)` — post-processing hook

**tp.config:**
- [ ] `tp.config.template_file` — TFile object (or Vulcan equivalent) for the template being processed
- [ ] `tp.config.target_file` — TFile object for the note the template is being inserted into
- [ ] `tp.config.run_mode` — numeric run mode indicator (0=create, 1=append, 5=dynamic; map to Vulcan equivalents)
- [ ] `tp.config.active_file` — currently active file (alias for target in CLI context)

**tp.obsidian (Vulcan equivalents):**
- [ ] `tp.obsidian.normalizePath(path)` — normalize vault-relative path (reuse Vulcan's path normalization)
- [ ] `tp.obsidian.htmlToMarkdown(html)` — convert HTML string to Markdown (use existing or add lightweight converter)
- [ ] `tp.obsidian.requestUrl(url)` — sandboxed HTTP request (reuse `tp.web` infrastructure, same allowlist restrictions)
- [ ] Emit diagnostic for Obsidian-specific APIs under `tp.app` that have no CLI equivalent (e.g., `tp.app.workspace`, `tp.app.vault.adapter`)

#### 9.9.4 Settings import

- [ ] Read Templater settings from `.obsidian/plugins/templater-obsidian/data.json`:
  | Setting key | Vulcan mapping |
  |---|---|
  | `templates_folder` | Template discovery path |
  | `templates_pairs` | Folder-specific template assignments |
  | `user_scripts_folder` | User script discovery path for `tp.user.*` |
  | `enable_system_commands` | Enable/disable `tp.system` command execution |
  | `shell_path` | Shell path for system commands |
  | `folder_templates` | Auto-apply templates on folder-based note creation |
  | `startup_templates` | Templates to run on vault open (map to `vulcan template run-startup`) |
  | `trigger_on_file_creation` | Auto-template on new file creation |
  | `syntax_highlighting` | Informational only (no CLI equivalent) |
  | `auto_jump_to_cursor` | Informational only (no CLI equivalent) |
- [ ] `vulcan config import templater` — import Templater settings and report mapping

#### 9.9.5 CLI integration

- [ ] `vulcan template` command detects Templater syntax and processes it (existing command, extended)
- [ ] `vulcan template --engine native|templater|auto` — force template engine selection (default: auto-detect based on `<% %>` presence)
- [ ] `--var key=value` flag for non-interactive template variable binding (replaces `tp.system.prompt()` in CI/automation contexts)
- [ ] Template preview: `vulcan template preview <name>` — show expanded template without creating a file
- [ ] Error diagnostics for Templater syntax that requires unavailable features (e.g., `tp.web` without `dataviewjs` feature)
- [ ] Integration test: Templater-syntax templates produce expected output, including `tp.file`, `tp.date`, `tp.frontmatter` access

### 9.10 Tasks plugin compatibility

**Goal:** Full compatibility with the Obsidian Tasks plugin — parse `` ```tasks `` query blocks, support recurring task expansion, task dependencies, custom status types, and priority-based filtering. This extends the Dataview task extraction (9.8.2) with Tasks-plugin-specific features.

**Builds on:** Phase 9.8.2 (task extraction and storage), Phase 9.8.4 (expression evaluator).
**Reference material:** [Obsidian Tasks documentation](https://publish.obsidian.md/tasks/)

#### 9.10.1 Tasks query language parser

- [ ] Detect `` ```tasks `` fenced code blocks during parsing; store raw query text as block metadata
- [ ] Tasks DSL parser: line-based filter language (each line is a filter or instruction)
  - [ ] Status filters: `not done`, `done`, `status.name includes <text>`, `status.type is <type>`
  - [ ] Date filters: `due before <date>`, `due after <date>`, `due on <date>`, `has due date`, `no due date` — and same for `created`, `start`, `scheduled`, `done` dates
  - [ ] Property filters: `description includes <text>`, `path includes <text>`, `heading includes <text>`, `tag includes <tag>`, `priority is <level>`
  - [ ] Recurrence filters: `is recurring`, `is not recurring`
  - [ ] Dependency filters: `is blocked`, `is not blocked`, `has id`
  - [ ] Boolean composition: `(filter1) AND (filter2)`, `(filter1) OR (filter2)`, `NOT (filter)`
  - [ ] Sort instructions: `sort by <field> [reverse]`
  - [ ] Group instructions: `group by <field> [reverse]`
  - [ ] Limit: `limit <n>`, `limit groups <n>`
  - [ ] Display options: `hide <field>`, `show <field>`, `short mode`
  - [ ] Explain: `explain` — output the parsed query plan

#### 9.10.2 Recurring task support

- [ ] Parse recurrence patterns from task text: `🔁 every <pattern>` (Tasks emoji) and `[repeat:: <pattern>]` (Dataview inline field)
- [ ] Support recurrence patterns: `every day`, `every week`, `every month`, `every year`, `every <n> days/weeks/months/years`, `every weekday`, `every Monday`, `every month on the 15th`
- [ ] Optional RRULE support for complex recurrence (RFC 5545 subset)
- [ ] Recurrence expansion: given a recurring task, compute next occurrence dates for query purposes
- [ ] `vulcan tasks next <n>` — show next N upcoming task instances (expanding recurrence)
- [ ] Store recurrence metadata in `task_properties` for query access

#### 9.10.3 Task dependencies

- [ ] Parse dependency annotations: `🆔 <id>` (task identifier), `⛔ <id>` (blocked by)
- [ ] Build task dependency graph from `tasks` and `task_properties` tables
- [ ] `is blocked` / `is not blocked` filter: a task is blocked if any of its `⛔` dependencies are not completed
- [ ] `vulcan tasks blocked` — list all blocked tasks with their blocking dependencies
- [ ] `vulcan tasks graph` — show task dependency graph (reuse graph analysis infrastructure)

#### 9.10.4 Custom status types

- [x] Support Tasks plugin custom status configuration: `[x]` = DONE, `[ ]` = TODO, `[/]` = IN_PROGRESS, `[-]` = CANCELLED, `[!]` = IMPORTANT, etc.
- [x] Status type categories: `TODO`, `DONE`, `IN_PROGRESS`, `CANCELLED`, `NON_TASK` — configurable via `.vulcan/config.toml` or imported from Tasks plugin settings
- [x] Read Tasks plugin status configuration from `.obsidian/plugins/obsidian-tasks-plugin/data.json`
- [ ] `status.type` and `status.name` queryable in both DQL and Tasks DSL

#### 9.10.5 Settings import

- [ ] Read Tasks plugin settings from `.obsidian/plugins/obsidian-tasks-plugin/data.json`:
  | Setting key | Vulcan mapping |
  |---|---|
  | `statusSettings.coreStatuses` | Core status type definitions (`[ ]`, `[x]`) |
  | `statusSettings.customStatuses` | Custom status type definitions (symbol → name → type → next) |
  | `globalFilter` | Global filter tag — only tasks matching this tag are considered by Tasks queries |
  | `globalQuery` | Default query prepended to all Tasks query blocks |
  | `removeGlobalFilter` | Whether to hide the global filter tag in rendered output |
  | `setCreatedDate` | Auto-set `➕ created` date on new tasks |
  | `recurrenceOnCompletion` | How recurring tasks create next instance on completion |
- [ ] `vulcan config import tasks` — import Tasks settings and report mapping

#### 9.10.6 CLI surface and evaluation

- [ ] `vulcan tasks query <query-string>` — evaluate a Tasks DSL query from the command line
- [ ] `vulcan tasks eval <file> [--block <n>]` — evaluate a `` ```tasks `` block from a note
- [ ] `vulcan tasks list [--filter <expr>]` — list tasks with optional DQL or Tasks DSL filter
- [ ] `--output json` on all subcommands
- [ ] Integration tests: Tasks DSL queries against test vault with known results

### 9.11 Kanban board support

**Goal:** Parse and query Obsidian Kanban plugin boards (`.md` files with column-as-heading structure), expose board state via CLI, and support board manipulation.

**Builds on:** Phase 9.8.2 (list item extraction), Phase 7.1 (metadata refactors).
**Reference material:** `references/obsidian-kanban/` (Kanban plugin source)

#### 9.11.1 Kanban board parsing

- [ ] Detect Kanban board files: presence of `kanban-plugin` key in frontmatter or YAML config code block at top of file
- [ ] Parse board structure: headings → columns, list items under headings → cards
- [ ] Extract card metadata: checkbox status, inline dates, tags, links, inline fields
- [ ] Parse board configuration from YAML code block (if present): column settings, archive column, completed column
- [ ] Configurable date and time triggers: parse date/time from card text using configurable trigger tokens (not hardcoded emoji — Kanban plugin allows `{date-trigger}` and `{time-trigger}` config, defaults `📅` and `⏰` but can be any string)
- [ ] Linked page metadata: cards that are `[[wikilinks]]` inherit metadata from the linked note (frontmatter, tags, inline fields) — enables filtering/sorting cards by linked note properties
- [ ] Store board structure in cache: `kanban_boards` table (or extend existing tables with board context)
- [ ] Index on board → column → card hierarchy

#### 9.11.2 Archive support

- [ ] Parse archive column: Kanban plugin supports a dedicated archive section (heading `## Archive` or configured via `archive-with-date` setting)
- [ ] `vulcan kanban archive <board> <card>` — move a card to the archive column
- [ ] Archive-with-date: optionally prepend archive date to card text (configurable via `archive-with-date` setting)
- [ ] `vulcan kanban show <board> --include-archive` — include archived cards in output (excluded by default)

#### 9.11.3 CLI surface

- [ ] `vulcan kanban list` — list all Kanban boards in the vault
- [ ] `vulcan kanban show <board>` — display board state (columns and card counts; `--verbose` shows all cards)
- [ ] `vulcan kanban cards <board> [--column <name>] [--status <status>]` — list cards with optional filters
- [ ] `vulcan kanban move <board> <card> <target-column>` — move a card between columns (rewrite the `.md` file)
- [ ] `vulcan kanban add <board> <column> <text>` — add a new card to a column
- [ ] `--output json` on all subcommands

#### 9.11.4 Settings import

- [ ] Read Kanban settings from `.obsidian/plugins/obsidian-kanban/data.json` — 39+ config keys including:
  | Setting key | Vulcan mapping |
  |---|---|
  | `date-trigger` | Date trigger token for card date parsing (default: `📅`) |
  | `time-trigger` | Time trigger token for card time parsing (default: `⏰`) |
  | `date-format` | Date display format |
  | `time-format` | Time display format |
  | `link-date-to-daily-note` | Whether date triggers create links to daily notes |
  | `metadata-keys` | Custom metadata keys extracted from cards |
  | `archive-with-date` | Whether to prepend date when archiving |
  | `prepend-archive-date` | Archive date format |
  | `new-card-insertion-method` | Where new cards are inserted (top/bottom of column) |
  | `hide-card-count` | Display preference |
  | `hide-tags-in-title` | Display preference |
  | `hide-tags-display` | Display preference |
  | `lane-width` | TUI/WebUI layout hint |
  | `max-archive-size` | Archive size limit |
  | `show-checkboxes` | Whether to show checkboxes on cards |
- [ ] `vulcan config import kanban` — import Kanban settings and report mapping
- [ ] Per-board settings override: individual boards can override global settings via their YAML code block

#### 9.11.5 TUI and WebUI (future)

- [ ] Browse TUI: `o` hotkey on Kanban `.md` files opens a board view with columns displayed side-by-side
- [ ] WebUI: Kanban board rendered as interactive drag-and-drop columns (depends on Phase 13/14)

### 9.12 AI assistant

**Goal:** A vault-aware AI assistant that can search, read, write, and analyze notes using an LLM inference backend (e.g., OpenRouter, local models via OpenAI-compatible API). This is a CLI-first feature — the assistant runs in the terminal and has full access to the vault through Vulcan's existing query and mutation infrastructure.

**Builds on:** Phase 5 (vectors/embeddings for semantic search), Phase 7.12 (query model), Phase 9.6 (search).

#### 9.12.1 Inference backend

- [ ] `AssistantProvider` trait: `chat(messages, tools?) -> Response`, `stream_chat(messages, tools?) -> Stream<Token>`
- [ ] `OpenAICompatibleProvider`: HTTP client for `/v1/chat/completions` endpoint (covers OpenRouter, local Ollama/vLLM, OpenAI, Anthropic via proxy)
- [ ] Config in `.vulcan/config.toml`:
  ```toml
  [assistant]
  provider = "openrouter"  # or "openai", "local", custom URL
  base_url = "https://openrouter.ai/api/v1"
  api_key_env = "OPENROUTER_API_KEY"  # env var name, not the key itself
  model = "anthropic/claude-sonnet-4"
  max_tokens = 4096
  temperature = 0.7
  ```
- [ ] Model selection via `--model` flag override
- [ ] Streaming output for interactive use

#### 9.12.2 Tool interface

The assistant has access to Vulcan's query and mutation tools:

- [ ] `search(query)` — full-text, semantic, and hybrid search
- [ ] `read_note(path)` — read a note's content
- [ ] `list_notes(filter?)` — list notes with optional property filter
- [ ] `query(dql)` — execute a DQL query
- [ ] `note_metadata(path)` — get frontmatter, inline fields, tags, links, tasks
- [ ] `backlinks(path)` — get notes linking to a given note
- [ ] `similar(path)` — find semantically similar notes (requires vectors)
- [ ] `create_note(path, content)` — create a new note
- [ ] `update_note(path, content)` — update a note's content
- [ ] `update_property(path, key, value)` — set a frontmatter property
- [ ] `append_to_note(path, text)` — append text to a note
- [ ] Tool calls require `--allow-write` flag for mutation tools (read-only by default)

#### 9.12.3 CLI surface

- [ ] `vulcan assistant <prompt>` — one-shot prompt with vault context
- [ ] `vulcan assistant --chat` — interactive multi-turn conversation
- [ ] `vulcan assistant --file <note> <prompt>` — prompt about a specific note (note content injected as context)
- [ ] `vulcan assistant --summarize <note>` — summarize a note
- [ ] `vulcan assistant --ask <question>` — RAG-style question answering against the vault (semantic search → context → answer)
- [ ] `vulcan assistant --prompt <name>` — use a named prompt file (see 9.12.6)
- [ ] `vulcan assistant --skill <name>` — invoke a named skill (see 9.12.7)
- [ ] `--output json` for structured output
- [ ] `--dry-run` on write operations shows planned changes without applying
- [ ] `--require-confirmation` — require user confirmation before tool calls that mutate the vault (default: true for write operations)

#### 9.12.4 System prompt and vault awareness

- [ ] Auto-generated system prompt includes: vault name, note count, tag summary, property catalog summary
- [ ] Vault context injection: relevant notes retrieved via search and injected into context window
- [ ] Context window management: truncate long note contents, prioritize recent/relevant sections
- [ ] `vulcan assistant --context <dql>` — pre-filter vault context with a DQL query before prompting
- [ ] `AGENTS.md` context file: if present in vault root, include as additional system context (vault-specific metadata, conventions, instructions for the assistant)

#### 9.12.5 Conversation persistence (gemini-scribe format)

Conversations are saved as Markdown files in a configurable folder, using Obsidian callouts for message formatting. This makes sessions browseable and searchable as regular vault notes.

- [ ] Configurable session folder: `assistant.sessions_folder` in `.vulcan/config.toml` (default: `AI/Sessions/`)
- [ ] Session file format — YAML frontmatter:
  ```yaml
  ---
  session_id: <ULID>
  type: conversation
  title: <auto-generated or user-provided>
  created: <ISO 8601>
  last_active: <ISO 8601>
  model: <model name used>
  context_files:
    - <paths of notes used as context>
  enabled_tools:
    - search
    - read_note
    - query
  require_confirmation: true
  ---
  ```
- [ ] Message format using Obsidian callouts:
  - `> [!user]+ User` — user messages (collapsible, expanded by default)
  - `> [!assistant]+ Assistant` — assistant responses
  - `> [!metadata]- Metadata` — tool calls, context retrieval, timestamps (collapsed by default)
  - Each message block separated by `## User` / `## Assistant` headings for readability
- [ ] Auto-save after each exchange in `--chat` mode
- [ ] Resume sessions: `vulcan assistant --resume <session-id-or-title>` — load conversation history from file
- [ ] `vulcan assistant sessions` — list saved sessions (title, date, message count)
- [ ] Session auto-titling: use LLM to generate a short title after the first exchange if none provided

#### 9.12.6 Prompts system

Prompts are reusable Markdown files that define pre-configured assistant behaviors, stored in a configurable prompts folder.

- [ ] Configurable prompts folder: `assistant.prompts_folder` in `.vulcan/config.toml` (default: `AI/Prompts/`)
- [ ] Prompt file format — Markdown with YAML frontmatter:
  ```yaml
  ---
  name: summarize-meeting
  description: Summarize meeting notes into action items
  version: 1
  override_system_prompt: false  # true = replace default system prompt; false = append
  tags:
    - productivity
    - meetings
  ---

  You are a meeting notes assistant. Given a meeting note, extract:
  1. Key decisions made
  2. Action items with owners
  3. Follow-up questions
  ```
- [ ] `vulcan assistant --prompt <name>` — load prompt by name (filename without `.md`, or `name` frontmatter field)
- [ ] `vulcan assistant prompts` — list available prompts (name, description, tags)
- [ ] Prompt variables: `{{context_files}}`, `{{vault_name}}`, `{{current_date}}` expanded at runtime
- [ ] Prompts can specify `context_files` in frontmatter to auto-load specific notes as context

#### 9.12.7 Skills system

Skills are Markdown-defined capabilities that combine a prompt with specific tool permissions and workflows. Directory reserved for future expansion.

- [ ] Configurable skills folder: `assistant.skills_folder` in `.vulcan/config.toml` (default: `AI/Skills/`)
- [ ] Skill file format — Markdown with YAML frontmatter:
  ```yaml
  ---
  name: daily-review
  description: Review today's notes and create a daily summary
  enabled_tools:
    - search
    - read_note
    - create_note
    - query
  require_confirmation: false
  output_file: "Reviews/{{date}}-daily-review.md"
  ---

  Search for all notes modified today. Read each one and create a summary note...
  ```
- [ ] `vulcan assistant --skill <name>` — invoke a skill (loads prompt + tool config + runs to completion)
- [ ] `vulcan assistant skills` — list available skills (name, description, tools used)
- [ ] Skills can define `output_file` to automatically save results to a vault note
- [ ] Skills can be chained: a skill's output can reference another skill

#### 9.12.8 Settings import

- [ ] No direct plugin equivalent to import — this is a Vulcan-native feature
- [ ] Migration helper: if `AGENTS.md` or prompt/skill-like files are detected in common locations (e.g., gemini-scribe folders), offer to import/symlink them into Vulcan's configured folders

### 9.13 QuickAdd-compatible automation (investigation phase)

**Goal:** Investigate and design Vulcan's equivalent of QuickAdd's macro/capture/template automation workflows. QuickAdd chains multiple operations (template creation, content capture, Obsidian commands, user scripts) into single-trigger actions.

**Status:** Investigation phase — the scope depends on how much of QuickAdd's functionality is valuable in a CLI context vs. being inherently UI-driven.

**Reference material:** `references/quickadd/` (QuickAdd source), [QuickAdd documentation](https://quickadd.obsidian.guide/docs/)

#### 9.13.1 Investigation tasks

- [ ] Audit QuickAdd's choice types and their CLI relevance:
  - **Template**: Create note from template with format syntax — maps directly to `vulcan template insert`
  - **Capture**: Append/prepend text to a note — maps to `vulcan capture`
  - **Macro**: Chain of commands executed sequentially — maps to `vulcan macro`
  - **Multi**: Menu of choices presented to user — maps to interactive CLI picker
- [ ] Evaluate QuickAdd's format syntax: `{{DATE}}`, `{{VALUE}}`, `{{FILE_NAME}}`, `{{MACRO:<name>}}`, `{{VDATE:format, offset}}`, `{{SELECTED}}`, `{{LINKCURRENT}}` — overlap with Templater and Vulcan's existing template variables
- [ ] Map QuickAdd's `CommandType` enum to Vulcan equivalents:
  | QuickAdd CommandType | Vulcan equivalent |
  |---|---|
  | `Obsidian` | Vulcan CLI command invocation |
  | `UserScript` | User script execution (reuse DataviewJS/Templater sandbox) |
  | `Wait` | Delay/pause in macro chain |
  | `NestedChoice` | Recursive macro/choice invocation |
  | `EditorCommand` | Text manipulation command (subset applicable to CLI) |
  | `AIAssistant` | Vulcan assistant invocation (9.12) |
  | `InfiniteAIAssistant` | Multi-turn assistant session |
  | `OpenFile` | `vulcan edit` / `$EDITOR` invocation |
  | `Conditional` | Conditional execution based on variable/expression |
- [ ] Assess QuickAdd macro chains: are these valuable as CLI pipelines or better served by shell scripts?
- [ ] Design decision: should Vulcan implement QuickAdd compatibility, a Vulcan-native automation DSL, or both?

#### 9.13.2 Likely scope (pending investigation)

- [ ] `vulcan capture <target-note> <text>` — quick append to a configured note (similar to `inbox` but configurable per target)
  - [ ] Capture format syntax: support `{{DATE}}`, `{{VALUE}}`, `{{FILE_NAME}}` variable expansion
  - [ ] Capture position: `--prepend` / `--append` / `--after-heading <heading>` / `--cursor` (editor contexts)
  - [ ] Capture to daily/weekly/monthly note with auto-creation
- [ ] `vulcan macro <name>` — execute a named sequence of Vulcan commands defined in `.vulcan/macros.toml`
- [ ] Macro definition format: TOML file defining named command sequences with variable interpolation
- [ ] Variable prompt control: macros can define variables that prompt the user for input at execution time (`--var` flag for non-interactive use)
- [ ] Conditional execution: `if` / `else` blocks in macro definitions based on variable values or file existence
- [ ] UserScript API: user scripts (`.js` files) can access a `quickAddApi`-equivalent object:
  - [ ] `api.inputPrompt(header, placeholder, value?)` — prompt for text input
  - [ ] `api.suggester(items, values)` — selection picker
  - [ ] `api.checkboxPrompt(items, defaults?)` — multi-select
  - [ ] `api.executeChoice(name)` — invoke another macro/choice by name
  - [ ] `api.utility.getClipboard()` / `api.utility.setClipboard(text)` — clipboard access

#### 9.13.3 Settings import

- [ ] Read QuickAdd settings from `.obsidian/plugins/quickadd/data.json`:
  | Setting key | Vulcan mapping |
  |---|---|
  | `choices` | Array of choice definitions (Template/Capture/Macro/Multi) — convert to Vulcan macro definitions |
  | `templateFolderPath` | Template discovery path (cross-reference with Templater settings) |
  | `globalVariables` | Global variable definitions for format syntax expansion |
  | `ai` | AI provider config (model, API key env, system prompt) — cross-reference with 9.12 assistant config |
  | `migrations.migrateToMacroIDRecords` | Internal migration flag (informational only) |
- [ ] `vulcan config import quickadd` — import QuickAdd choices as Vulcan macros where feasible, report unmappable choices

### 9.14 Plugin compatibility notes

Notes on other common Obsidian plugins and their relationship to Vulcan:

**Excalidraw:** Drawings stored as `.excalidraw.md` (Markdown with LZ-String compressed JSON in code blocks) or `.excalidraw` (plain JSON). Parsing, indexing, and WebUI rendering/editing are covered in **Phase 18.8 (Excalidraw support)** as a sub-phase of Canvas support.

**Advanced Tables:** Primarily a UI feature for editing Markdown tables. No data format to parse — Vulcan's existing Markdown parser already handles standard Markdown tables. WebUI table editing (tab navigation, column add/remove, sorting, alignment, CSV paste) is covered in **Phase 14.1 (Note editor → Advanced table editing)**.

**Calendar:** The Calendar plugin provides a calendar view linked to daily notes. Vulcan's browse TUI (9.2) could add a calendar navigation mode, and the WebUI (Phase 13) could render a calendar view. The DQL CALENDAR query type (9.8.6) already provides the data foundation. **Roadmap note:** Calendar navigation is a TUI/WebUI presentation concern, not a data/query concern.

**Obsidian Git:** Git-based vault synchronization and versioning. Vulcan already has git integration (9.3 auto-commit, `diff` command, browse TUI git log). No additional compatibility needed.

### 9.15 TaskNotes compatibility

**Goal:** Full compatibility with the TaskNotes plugin — tasks stored as individual Markdown files with rich YAML frontmatter, powered by Obsidian Bases views. TaskNotes treats each task as a first-class vault note (unlike the Tasks plugin which operates on inline checkboxes). Vulcan should parse, index, query, create, and manage TaskNotes task files, register custom Bases view types, and support the full TaskNotes configuration surface.

**Builds on:** Phase 4 (properties/Bases), Phase 9.8 (Dataview metadata), Phase 9.10 (Tasks plugin — complementary, not overlapping).
**Reference material:** `references/tasknotes/` (TaskNotes source), requires Obsidian 1.10.1+ for public Bases API.

#### 9.15.1 Task file format and parsing

- [ ] Detect TaskNotes task files: configurable identification method — by tag (default: `task` tag in frontmatter) or by property presence (e.g., `status` + `priority` fields)
- [ ] Configurable tasks folder: default `TaskNotes/Tasks/`, configurable via settings import
- [ ] Parse task frontmatter properties:
  | Property | Type | Description |
  |---|---|---|
  | `title` | string | Task title |
  | `status` | string | Task status (maps to custom status config) |
  | `priority` | string | Priority level (maps to custom priority config) |
  | `due` | date | Due date |
  | `scheduled` | date | Scheduled date |
  | `completedDate` | date | Completion timestamp |
  | `dateCreated` | date | Creation timestamp |
  | `dateModified` | date | Last modification timestamp |
  | `contexts` | list | Context tags (e.g., `@office`, `@home`) |
  | `projects` | list | Wikilinks to project notes |
  | `tags` | list | Standard tags |
  | `timeEstimate` | number | Estimated duration in minutes |
  | `recurrence` | string | RFC 5545 RRULE recurrence pattern |
  | `complete_instances` | list | Completed recurrence instance dates |
  | `skipped_instances` | list | Skipped recurrence instance dates |
  | `archived` | boolean | Archive flag |
  | `blockedBy` | list | Task dependency objects (uid, reltype, gap) |
  | `reminders` | list | Reminder objects (id, type, relatedTo, offset, description) |
  | `timeEntries` | list | Time tracking session objects (startTime, endTime, description) |
- [ ] Field mapping support: TaskNotes allows users to remap internal field names to custom frontmatter keys — honor `fieldMapping` configuration
- [ ] Custom user-defined fields: arbitrary additional frontmatter properties with typed schemas (text, number, date, boolean, list)
- [ ] Store parsed task data in cache: extend `documents` metadata or add `tasknotes_tasks` table with indexed columns for status, priority, due, scheduled, project, context
- [ ] Excluded folders: respect `excludedFolders` setting to skip non-task files in task folders

#### 9.15.2 Custom statuses and priorities

- [ ] Custom status definitions: each status has `id`, `value` (frontmatter string), `label` (display name), `color`, `isCompleted` (boolean), `autoArchive` (delay config)
  - Default statuses: `todo`, `in-progress`, `done`, `cancelled`
  - Users can add unlimited custom statuses with configurable completion semantics
- [ ] Custom priority definitions: each priority has `id`, `value`, `label`, `color`, `weight` (numeric for sorting/scoring)
  - Default priorities: `highest`, `high`, `medium`, `low`, `lowest`
- [ ] Status and priority are first-class query dimensions: filterable, sortable, groupable in DQL, Tasks DSL, and Bases views
- [ ] Auto-archive: when a task enters a completed status, optionally archive after a configurable delay

#### 9.15.3 Natural language task creation

- [ ] NLP parser for task input strings: extract structured properties from natural language
  - Example: `"Buy groceries tomorrow at 3pm @home #errands high priority"` → `{ title: "Buy groceries", due: "2026-03-28T15:00", contexts: ["@home"], tags: ["errands"], priority: "high" }`
- [ ] Configurable NLP trigger characters:
  | Trigger | Default | Extracts |
  |---|---|---|
  | `@` | contexts | `@home`, `@office` |
  | `#` | tags | `#errands`, `#work` |
  | `+` | projects | `+[[Project Name]]` |
  | `*` | status | `*done`, `*in-progress` |
- [ ] Date extraction: "tomorrow", "next Monday", "in 3 days", "January 15th" — reuse chrono-like date parsing
- [ ] Priority extraction: "high priority", "urgent", "low priority" — configurable keyword mapping
- [ ] `vulcan tasknotes add "natural language input"` — create task file from NLP-parsed input
- [ ] `--no-nlp` flag to create task with raw title (skip NLP parsing)
- [ ] Configurable NLP language (default: English, supports multiple languages)

#### 9.15.4 Recurring tasks (RRULE)

- [ ] Parse `recurrence` field as RFC 5545 RRULE string (e.g., `FREQ=WEEKLY;BYDAY=MO,WE,FR`)
- [ ] Recurrence expansion: compute next N occurrences for query and calendar display
- [ ] Per-instance completion: `complete_instances` tracks which occurrences are done without completing the entire recurring task
- [ ] Per-instance skipping: `skipped_instances` marks occurrences as intentionally skipped
- [ ] Flexible vs fixed scheduling: next instance calculated from completion date (flexible) or from original schedule (fixed) — configurable via `recurrenceAnchor`
- [ ] `vulcan tasknotes next <n>` — show next N upcoming task instances across all recurring tasks
- [ ] Integrate with 9.10 recurring task infrastructure where overlapping

#### 9.15.5 Task dependencies

- [ ] Parse `blockedBy` array: each entry has `uid` (wikilink to blocking task), `reltype`, and optional `gap` (ISO 8601 duration)
- [ ] Dependency relation types (RFC 9253):
  | Type | Meaning |
  |---|---|
  | `FINISHTOSTART` | Blocked task can start after blocker finishes (default) |
  | `FINISHTOFINISH` | Blocked task can finish after blocker finishes |
  | `STARTTOSTART` | Blocked task can start after blocker starts |
  | `STARTTOFINISH` | Blocked task can finish after blocker starts |
- [ ] Duration gaps: `gap: P1D` means "1 day after the blocker completes"
- [ ] Build dependency graph from task files (reuse graph infrastructure from Phase 2)
- [ ] `vulcan tasknotes blocked` — list all blocked tasks with their blocking dependencies
- [ ] `vulcan tasknotes graph` — visualize task dependency graph
- [ ] Integrate with 9.10 task dependency infrastructure

#### 9.15.6 Time tracking and pomodoro

- [ ] Parse `timeEntries` array: each entry has `startTime`, `endTime`, `description`
- [ ] `vulcan tasknotes track start <task>` — start a time tracking session (append to `timeEntries` with open `endTime`)
- [ ] `vulcan tasknotes track stop [task]` — stop the active session (set `endTime`)
- [ ] `vulcan tasknotes track status` — show currently active tracking session
- [ ] `vulcan tasknotes track log <task>` — show time entries for a task
- [ ] `vulcan tasknotes track summary [--period day|week|month]` — aggregate time spent across tasks
- [ ] Pomodoro timer (CLI):
  - [ ] `vulcan tasknotes pomodoro start <task>` — start a pomodoro work session
  - [ ] Configurable durations: `pomodoro.work_duration` (default 25min), `pomodoro.short_break` (5min), `pomodoro.long_break` (15min), `pomodoro.long_break_interval` (every 4 pomodoros)
  - [ ] Desktop notification on session end (best-effort, platform-dependent)
  - [ ] Pomodoro session history stored in task frontmatter (`pomodoros` array) or daily note (configurable)
- [ ] `timeEstimate` field: compare estimated vs actual time in reports

#### 9.15.7 Reminders

- [ ] Parse `reminders` array: each reminder has `id`, `type` (relative/absolute), `relatedTo` (due/scheduled), `offset` (ISO 8601 duration, e.g., `-PT15M`), `description`
- [ ] `vulcan tasknotes reminders [--upcoming <duration>]` — list upcoming reminders within a time window
- [ ] Daemon mode (Phase 10+): trigger desktop notifications for due reminders
- [ ] CLI mode: `vulcan tasknotes due [--within <duration>]` — show tasks due within a time window (simpler alternative to daemon-based reminders)

#### 9.15.8 Bases view integration

TaskNotes v4+ is built entirely on Obsidian Bases. Vulcan should register equivalent custom Bases view types:

- [ ] Register custom Bases source type: `tasknotes` with config subtypes:
  | View type | Description |
  |---|---|
  | `tasknotesTaskList` | Filterable, sortable, groupable task table |
  | `tasknotesKanban` | Kanban board (columns = status or custom field) |
  | `tasknotesCalendar` | Full calendar view (month/week/day/year) |
  | `tasknotesMiniCalendar` | Compact month overview |
- [ ] Parse `.base` view files in `TaskNotes/Views/` (YAML format):
  - Filter conditions: grouped AND/OR tree of property-based conditions
  - Sort key and direction
  - Group key and optional sub-group key
  - Formula definitions for computed columns
- [ ] Built-in formula support for TaskNotes views:
  | Formula | Expression |
  |---|---|
  | `daysUntilDue` | `if(due, ((number(date(due)) - number(today())) / 86400000).floor(), null)` |
  | `isOverdue` | `due && date(due) < today() && status != "done"` |
  | `urgencyScore` | `formula.priorityWeight + max(0, 10 - formula.daysUntilDue)` |
  | `efficiencyRatio` | `if(timeEstimate > 0, totalTimeSpent / timeEstimate, null)` |
- [ ] `vulcan tasknotes view <name>` — evaluate a saved Bases view from the command line
- [ ] `vulcan tasknotes view list` — list available TaskNotes views
- [ ] `--output json|table` on view evaluation
- [ ] Saved filter views: support `savedViews` config (named filter+sort+group presets) as CLI aliases

#### 9.15.9 CLI surface

- [ ] `vulcan tasknotes add <title-or-nlp-string>` — create a new task file
  - [ ] `--status`, `--priority`, `--due`, `--scheduled`, `--context`, `--project`, `--tag` flags for explicit property setting
  - [ ] `--template <name>` — create from a task template
- [ ] `vulcan tasknotes list [--filter <expr>]` — list tasks with optional filter expression
  - [ ] `--status <s>`, `--priority <p>`, `--due-before <date>`, `--due-after <date>`, `--project <p>`, `--context <c>` — shorthand filters
  - [ ] `--group-by <field>`, `--sort-by <field>` — grouping and sorting
  - [ ] `--include-archived` — include archived tasks (excluded by default)
- [ ] `vulcan tasknotes show <task>` — display task details (all properties, time entries, dependencies)
- [ ] `vulcan tasknotes edit <task>` — open task file in `$EDITOR`
- [ ] `vulcan tasknotes set <task> <property> <value>` — update a task property
- [ ] `vulcan tasknotes complete <task>` — mark task as completed (set status to done, record `completedDate`)
- [ ] `vulcan tasknotes archive <task>` — archive a completed task
- [ ] `vulcan tasknotes convert <file> [--line <n>]` — convert a line, checkbox, or heading in an existing note into a TaskNotes task file (inline task conversion)
- [ ] `--output json` on all subcommands

#### 9.15.10 Calendar sync (future — behind feature flag)

Calendar sync requires OAuth2 flows and HTTP clients; gate behind a `calendar-sync` compile-time feature flag.

- [ ] Google Calendar sync: OAuth2 authentication, selective calendar selection, incremental sync via sync tokens
- [ ] Microsoft Calendar sync: OAuth2 authentication, delta link-based sync
- [ ] ICS integration: subscribe to remote ICS feeds (periodic fetch + cache), watch local `.ics` files
- [ ] Timeblocking: create time blocks in calendar from task schedules
- [ ] Auto-export: export tasks to `.ics` file on configurable interval
- [ ] `vulcan tasknotes calendar sync` — trigger a manual sync cycle
- [ ] `vulcan tasknotes calendar list` — list connected calendars and sync status

#### 9.15.11 Settings import

- [ ] Read TaskNotes settings from `.obsidian/plugins/tasknotes/data.json` — comprehensive import covering:
  | Setting category | Key settings |
  |---|---|
  | **Core** | `tasksFolder`, `taskTag`, `taskIdentificationMethod`, `excludedFolders`, `defaultTaskPriority`, `defaultTaskStatus` |
  | **Field mapping** | `fieldMapping` — full property name remapping table (22 fields) |
  | **Custom types** | `customStatuses` (id, value, label, color, isCompleted, autoArchive), `customPriorities` (id, value, label, color, weight) |
  | **User fields** | `userFields` — custom field definitions (id, displayName, key, type, defaultValue, autosuggestFilter) |
  | **NLP** | `enableNaturalLanguageInput`, `nlpLanguage`, `nlpDefaultToScheduled`, `nlpTriggers` (trigger chars → property mapping) |
  | **Pomodoro** | `pomodoroWorkDuration`, `pomodoroShortBreakDuration`, `pomodoroLongBreakDuration`, `pomodoroLongBreakInterval`, `pomodoroAutoStartBreaks`, `pomodoroAutoStartWork`, `pomodoroStorageLocation` |
  | **Calendar** | `calendarViewSettings` (defaultView, slotDuration, firstDay, timeFormat, visibility flags), `enableTimeblocking` |
  | **ICS** | `icsIntegration` (enableAutoExport, autoExportPath, autoExportInterval, useICSEndAsDue) |
  | **Google Calendar** | `enableGoogleCalendar`, `enabledGoogleCalendars`, `googleCalendarExport` settings |
  | **Microsoft Calendar** | `enableMicrosoftCalendar`, `enabledMicrosoftCalendars` |
  | **API** | `enableAPI`, `apiPort`, `apiAuthToken`, `enableMCP`, `webhooks` |
  | **Bases** | `enableBases`, `autoCreateDefaultBasesFiles`, `commandFileMapping` |
  | **Saved views** | `savedViews` — named filter/sort/group presets |
  | **Task defaults** | `taskCreationDefaults` (defaultContexts, defaultTags, defaultProjects, defaultDueDate, defaultTimeEstimate, defaultReminders) |
  | **UI/Editor** | `modalFieldsConfig`, `defaultVisibleProperties`, `singleClickAction` — informational for future TUI/WebUI |
- [ ] `vulcan config import tasknotes` — import TaskNotes settings, create Vulcan-native config, report mapping
- [ ] Migrate `.base` view files: copy TaskNotes view definitions and validate they work with Vulcan's Bases evaluator

#### 9.15.12 HTTP API compatibility (daemon phase)

When the daemon is running (Phase 10+), expose TaskNotes-compatible REST endpoints:

- [ ] `GET /tasks` — list tasks with filter/sort/group query params
- [ ] `POST /tasks` — create a task (accepts NLP string or structured JSON)
- [ ] `PATCH /tasks/{id}` — update task properties
- [ ] `DELETE /tasks/{id}` — delete or archive a task
- [ ] `POST /tasks/{id}/track/start` — start time tracking
- [ ] `POST /tasks/{id}/track/stop` — stop time tracking
- [ ] `POST /tasks/{id}/pomodoro/start` — start pomodoro session
- [ ] `POST /tasks/{id}/pomodoro/stop` — stop pomodoro session
- [ ] `GET /tasks/calendar` — calendar events (tasks + calendar sync)
- [ ] Webhook support: configurable webhooks for task events (create, update, complete, archive, time tracked)
- [ ] MCP (Model Context Protocol) support: expose task operations as MCP tools for AI integration (cross-reference with 9.12 AI assistant)

### 9.16 Periodic notes (daily, weekly, monthly)

**Goal:** First-class support for periodic notes — daily notes, weekly notes, monthly notes, and custom periodic patterns. Multiple Phase 9 plugins depend on periodic note discovery and creation (TaskNotes pomodoro storage in daily notes, Kanban date-to-daily-note linking, QuickAdd capture to daily note, Calendar plugin integration, Dataview `file.day` resolution). This phase provides the shared infrastructure.

**Builds on:** Phase 1 (document indexing), Phase 9.7 (template variables for date formatting).
**Reference material:** [Obsidian Daily Notes core plugin](https://help.obsidian.md/Plugins/Daily+notes), [Obsidian Periodic Notes community plugin](https://github.com/liamcain/obsidian-periodic-notes)

#### 9.16.1 Periodic note configuration

- [ ] Configuration in `.vulcan/config.toml`:
  ```toml
  [periodic.daily]
  enabled = true
  folder = "Journal/Daily"
  format = "YYYY-MM-DD"          # date format for filename (Moment.js-compatible)
  template = "daily"              # template name from 9.7/9.9 template system

  [periodic.weekly]
  enabled = true
  folder = "Journal/Weekly"
  format = "YYYY-[W]ww"
  template = "weekly"
  start_of_week = "monday"       # monday | sunday | saturday

  [periodic.monthly]
  enabled = true
  folder = "Journal/Monthly"
  format = "YYYY-MM"
  template = "monthly"

  [periodic.quarterly]
  enabled = false
  folder = "Journal/Quarterly"
  format = "YYYY-[Q]Q"
  template = "quarterly"

  [periodic.yearly]
  enabled = false
  folder = "Journal/Yearly"
  format = "YYYY"
  template = "yearly"
  ```
- [ ] Each period type is independently configurable: folder, filename format, template, enabled flag
- [ ] Custom period types: allow user-defined periods beyond the built-in five (e.g., `[periodic.sprint]` with custom interval)

#### 9.16.2 Periodic note discovery and resolution

- [ ] `resolve_periodic_note(period, date) -> Option<Path>`: given a period type and date, compute the expected filename and check if it exists
- [ ] `resolve_daily_note(date) -> Option<Path>`: convenience alias for daily resolution
- [ ] Reverse resolution: given a note path, determine if it's a periodic note and extract the date (parse filename against configured format)
- [ ] `file.day` integration (Dataview 9.8.3): use periodic note configuration to resolve `file.day` — a daily note for `2026-03-27` has `file.day = date("2026-03-27")`
- [ ] Date-to-note linking: provide a lookup function for other phases (Kanban `link-date-to-daily-note`, TaskNotes calendar integration)
- [ ] Index periodic notes in cache: add `periodic_type` and `periodic_date` columns to documents table (nullable, populated during scan for notes matching periodic patterns)

#### 9.16.3 CLI surface

- [ ] `vulcan daily [date]` — open or create today's daily note (or specified date)
  - [ ] If note exists: open in `$EDITOR`
  - [ ] If note doesn't exist: create from template, then open
  - [ ] `--no-edit` flag: create only, don't open
- [ ] `vulcan weekly [date]`, `vulcan monthly [date]` — same pattern for other periods
- [ ] `vulcan periodic <type> [date]` — generic command for any configured period type
- [ ] `vulcan periodic list [--type daily|weekly|monthly|...]` — list periodic notes, optionally filtered by type
- [ ] `vulcan periodic gaps [--type daily] [--from <date>] [--to <date>]` — show missing periodic notes in a date range (useful for identifying gaps in journaling)
- [ ] `--output json` on all subcommands
- [ ] Auto-commit if enabled

#### 9.16.4 Settings import

- [ ] Import from Obsidian Daily Notes core plugin: `.obsidian/daily-notes.json`
  | Setting key | Vulcan mapping |
  |---|---|
  | `folder` | `periodic.daily.folder` |
  | `format` | `periodic.daily.format` |
  | `template` | `periodic.daily.template` |
  | `autorun` | Informational (no CLI equivalent) |
- [ ] Import from Periodic Notes community plugin: `.obsidian/plugins/periodic-notes/data.json`
  | Setting key | Vulcan mapping |
  |---|---|
  | `daily.enabled`, `daily.folder`, `daily.format`, `daily.templatePath` | `periodic.daily.*` |
  | `weekly.enabled`, `weekly.folder`, `weekly.format`, `weekly.templatePath` | `periodic.weekly.*` |
  | `monthly.enabled`, `monthly.folder`, `monthly.format`, `monthly.templatePath` | `periodic.monthly.*` |
  | `quarterly.enabled`, `quarterly.folder`, `quarterly.format`, `quarterly.templatePath` | `periodic.quarterly.*` |
  | `yearly.enabled`, `yearly.folder`, `yearly.format`, `yearly.templatePath` | `periodic.yearly.*` |
- [ ] `vulcan config import periodic-notes` — import periodic notes settings

### 9.17 Unified plugin settings import

**Goal:** A single command to discover and import settings from all supported Obsidian plugins at once, plus a status overview of what's importable.

- [ ] `vulcan config import --all` — discover all supported plugin `data.json` files in `.obsidian/plugins/` and import each one, reporting results:
  ```
  Importing plugin settings...
    ✓ dataview: 14 settings imported
    ✓ templater-obsidian: 10 settings imported
    ✓ obsidian-tasks-plugin: 7 settings imported
    ✓ obsidian-kanban: 15 settings imported
    ✗ quickadd: not installed
    ✓ tasknotes: 50 settings imported
    ✓ periodic-notes: 5 settings imported
    ✓ daily-notes (core): 3 settings imported
  ```
- [ ] `vulcan config import --list` — show which plugins are detected and importable without actually importing
- [ ] `vulcan config import --dry-run` — show what would be imported without writing config
- [ ] Conflict resolution: when multiple plugins configure the same Vulcan setting (e.g., template folder from both Templater and QuickAdd), warn and prefer the more specific plugin
- [ ] Import idempotency: re-running import updates existing config without duplicating entries
- [ ] Individual plugin importers remain available: `vulcan config import dataview`, `vulcan config import templater`, etc.

### Phase 9 implementation order

The Phase 9 sub-phases have both sequential dependencies and parallelization opportunities. This section consolidates the dependency edges into a recommended implementation order.

**Dependency graph:**

```
9.1 (edit) ─────────────────────────────┐
9.2 (browse TUI) ← 9.1                  │
9.3 (auto-commit) ──────────────────────│── can proceed in parallel
9.4 (additional CLI) ───────────────────│
9.5 (config layering) ─────────────────│
                                        │
9.6 (advanced search) ─────────────────│── foundation for 9.8, 9.12
9.7 (enhanced templates) ──────────────│── foundation for 9.9
                                        │
9.8 (Dataview) ← 4 (Bases), 9.6        │
  9.8.1 (inline fields + type inference)│
  9.8.2 (list items + tasks)            │── sequential within 9.8
  9.8.3 (file.* metadata) ← 9.16       │
  9.8.4 (type system + expression eval) │
  9.8.5-9.8.7 (DQL + inline)           │
  9.8.8 (DataviewJS) ← sandbox          │── enables 9.9.3
  9.8.9 (settings import)              │
                                        │
9.9  (Templater)    ← 9.7, 9.8.8       │
9.10 (Tasks plugin) ← 9.8.2            │── can proceed in parallel
9.11 (Kanban)       ← 9.8.2, 7.1       │   (after their prerequisites)
9.16 (Periodic)     ← 1, 9.7           │
                                        │
9.12 (AI assistant) ← 5, 7.12, 9.6     │── independent of 9.9–9.11
9.15 (TaskNotes)    ← 4, 9.8, 4.5.1    │── independent of 9.9–9.12
                                        │
9.13 (QuickAdd)     ← investigation     │── can start anytime
9.14 (plugin notes) ← informational     │
9.17 (unified import) ← all importers   │── last
```

**Recommended implementation order:**

1. **Wave 1 (parallel):** 9.1–9.5 — CLI foundation. These are largely complete and independent.
2. **Wave 2 (parallel):** 9.6 (search), 9.7 (templates) — foundation for later waves.
3. **Wave 3 (sequential):** 9.8.1 → 9.8.2 → 9.8.3 → 9.8.4 → 9.8.5 → 9.8.6 → 9.8.7 → 9.8.8 → 9.8.9 — Dataview, the largest sub-phase. Internal ordering is sequential.
4. **Wave 4 (parallel):** 9.9 (Templater), 9.10 (Tasks), 9.11 (Kanban), 9.16 (Periodic notes) — all have their prerequisites met after Wave 3. Can proceed in parallel.
5. **Wave 5 (parallel):** 9.12 (AI assistant), 9.15 (TaskNotes) — independent of Wave 4. Can start as early as after Wave 3, or run in parallel with Wave 4.
6. **Wave 6:** 9.13 (QuickAdd) — investigation phase, can start anytime but benefits from seeing 9.9 and 9.12 patterns first.
7. **Wave 7:** 9.17 (unified import) — must come after all individual plugin importers are implemented.

**Critical path:** Phase 4 → 9.6 → 9.8.1 → ... → 9.8.8 → 9.9 (Templater). The Dataview sub-phases are the longest sequential chain and gate Templater's JS-dependent features.

**Note on 9.8.3 and 9.16:** The `file.day` metadata field in 9.8.3 depends on periodic note configuration from 9.16. However, `file.day` can be stubbed initially (return null when no periodic config exists) and filled in when 9.16 lands. This avoids blocking all of 9.8 on 9.16.

---

## Phase 10: Multi-Vault Daemon

**Goal:** A long-running process that serves multiple vaults over a proper REST API. The CLI can connect to it instead of opening the cache directly, eliminating per-command startup cost and enabling multi-vault workflows.

**Depends on:** Phase 7 complete. Phases 9.8–9.17 (Dataview, Templater, Tasks, Kanban, AI, QuickAdd, TaskNotes, Periodic Notes) are CLI-phase foundation work that should be complete or well-advanced before daemon work begins.
**Design refs:** Existing `serve.rs` (single-vault HTTP server, hand-rolled), `watch.rs` (file watcher).

Search API note: search request semantics are already defined earlier by the shared `SearchQuery` contract from Phase 9.6. Phase 10 daemon work reuses that surface; it does not introduce a second search-parameter design step.

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
- **Forward reference:** Phase 17 replaces the per-vault token model with multi-user accounts, groups, and per-vault roles. The token infrastructure here (argon2 hashing, Bearer auth middleware) is reused — Phase 17 extends it, not replaces it.

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
- [ ] Auth: multi-user login page (username/password or API key), browser sessions via cookie or localStorage token. Uses the user management and ACL system from Phase 17. All API calls and rendered views respect the authenticated user's permissions.

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
- [ ] **Advanced table editing** (inspired by Advanced Tables plugin):
  - [ ] Tab/Shift-Tab navigation between cells
  - [ ] Auto-formatting: column alignment, padding, separator row maintenance
  - [ ] Add/remove columns and rows via toolbar or keyboard shortcuts
  - [ ] Column sorting (click header to sort by column, reorder rows in Markdown)
  - [ ] Column alignment toggle (left/center/right via `:---`, `:---:`, `---:` syntax)
  - [ ] Formula support in tables: spreadsheet-like expressions in cells (e.g., `=sum(col)`) evaluated on save — maps to Bases-style expressions where applicable
  - [ ] CSV/TSV paste: pasting tabular data auto-converts to Markdown table
  - [ ] Table toolbar: contextual toolbar when cursor is inside a table

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

Uses the full ACL system from Phase 17. Wiki mode adds vault-level access presets that configure the underlying ACL rules:

- [ ] **Public read / authenticated write** (default): unauthenticated users get `viewer` access, authenticated users use their vault role
- [ ] **Fully public**: unauthenticated users get `viewer` access, no login required for any read operation
- [ ] **Fully private**: no unauthenticated access, all users must log in
- [ ] **Per-folder and per-tag visibility**: configured via ACL rules from Phase 17.2 — e.g., hide `GM-Only/` from the `players` group
- [ ] **Document-level secrets**: `[!secret]` callouts and restricted embeds from Phase 17.4 are enforced in wiki rendering
- [ ] **Share links**: external share tokens from Phase 17.5 provide read access to specific notes/folders without requiring an account

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

## Phase 17: User Management & Access Control

**Goal:** Multi-user identity, group-based permissions, fine-grained ACLs, document-level secrets, and external share links. Provides the authorization layer that all web-facing features depend on.

**Depends on:** Phase 10 (daemon). Sub-phases 17.1–17.3 must be complete before Phase 13 ships. Sub-phases 17.4–17.5 are needed by Phase 16.

**Design principle: ACLs are not a cache.** User accounts, group memberships, ACL rules, sessions, and share tokens are authoritative state — they must never be stored in a vault's cache DB (which can be deleted and regenerated at any time). User/group configuration lives in human-editable TOML files in the daemon config directory. High-churn transactional data (sessions, API keys, share tokens) lives in a small authoritative SQLite database alongside the config.

### 17.1 User & group storage

```
~/.config/vulcan/
├── daemon.toml          # daemon config (from Phase 10)
├── users.toml           # user accounts and group definitions
└── auth.db              # sessions, API keys, share tokens (SQLite)
```

**Users and groups** are defined in `users.toml` — low churn, human-editable, can be version-controlled:

```toml
[users.alice]
display_name = "Alice"
email = "alice@example.com"
password_hash = "$argon2id$v=19$..."
disabled = false

[users.bob]
display_name = "Bob"
password_hash = "$argon2id$v=19$..."

[groups.gm]
display_name = "Game Masters"
members = ["alice"]

[groups.players]
display_name = "Players"
members = ["bob", "charlie"]
```

**Transactional auth data** lives in `auth.db` (not a cache — back up with daemon config):

```sql
sessions:    id, user_id, token_hash, created_at, expires_at
api_keys:    id, user_id, key_hash, label, scopes (JSON), created_at, expires_at
share_tokens: id, vault_id, resource, permission, token_hash, created_by,
              password_hash (nullable), created_at, expires_at
```

**CLI management commands:**

- [ ] `vulcan auth user add <username>` — create user, prompt for password
- [ ] `vulcan auth user remove <username>` — remove user (with confirmation)
- [ ] `vulcan auth user list` — list users with status
- [ ] `vulcan auth user disable/enable <username>` — toggle without deleting
- [ ] `vulcan auth group add <group> [--members alice,bob]` — create group
- [ ] `vulcan auth group remove <group>` — remove group
- [ ] `vulcan auth group members <group> add/remove <username>` — manage membership
- [ ] `vulcan auth apikey create <username> [--scope vault:personal:editor] [--expires 90d]` — generate API key
- [ ] `vulcan auth apikey revoke <key-id>` — revoke key
- [ ] `vulcan auth apikey list [--user username]` — list active keys

**API endpoints for user management** (owner/admin only):

- [ ] `GET /auth/users` — list users
- [ ] `POST /auth/users` — create user
- [ ] `PATCH /auth/users/{username}` — update user
- [ ] `DELETE /auth/users/{username}` — remove user
- [ ] `GET /auth/groups` — list groups
- [ ] `POST /auth/groups` — create group
- [ ] `PATCH /auth/groups/{group}` — update membership
- [ ] `POST /auth/session` — login (returns session token)
- [ ] `DELETE /auth/session` — logout

### 17.2 Vault roles & ACL rules

**Vault roles** assign coarse permissions per user or group per vault. Configured in `daemon.toml` alongside vault registration:

```toml
[[vault]]
id = "campaign"
path = "/home/user/vaults/campaign"

# Default role for authenticated users not otherwise listed
default_role = "viewer"

[[vault.roles]]
principal = "user:alice"
role = "owner"

[[vault.roles]]
principal = "group:gm"
role = "owner"

[[vault.roles]]
principal = "group:players"
role = "editor"
```

**Role hierarchy:**

| Role | Read | Write | Manage ACLs | Vault config |
|------|------|-------|-------------|--------------|
| `owner` | yes | yes | yes | yes |
| `editor` | yes | yes | no | no |
| `viewer` | yes | no | no | no |
| `none` | no | no | no | no |

**Fine-grained ACL rules** override the vault role for specific resources. Stored in vault config (`.vulcan/config.toml`) so they travel with the vault:

```toml
# .vulcan/config.toml — ACL rules section

[[acl]]
principal = "group:players"
resource = "folder:GM-Only/"
permission = "none"          # players cannot see anything in GM-Only/

[[acl]]
principal = "user:bob"
resource = "folder:Characters/Bob/"
permission = "editor"        # bob can edit his own character folder

[[acl]]
principal = "*"
resource = "tag:secret"
permission = "none"          # notes tagged #secret are hidden from everyone except owners

[[acl]]
principal = "group:gm"
resource = "tag:secret"
permission = "owner"         # GMs can see and edit #secret notes
```

**Resource specifiers:**

- `folder:<path>` — applies to all notes under the folder (recursive)
- `tag:<tag>` — applies to notes carrying the tag
- `note:<path>` — applies to a single note

**Evaluation order:** explicit deny (`none`) > most-specific grant > less-specific grant > vault role > default_role > no access. `owner` vault role bypasses all ACL rules (always has full access).

**CLI commands:**

- [ ] `vulcan auth acl add <vault> --principal <p> --resource <r> --permission <perm>` — add ACL rule
- [ ] `vulcan auth acl remove <vault> <rule-id>` — remove rule
- [ ] `vulcan auth acl list <vault>` — show effective rules
- [ ] `vulcan auth acl check <vault> <username> <path>` — test effective permission for a user on a note (useful for debugging)

### 17.3 Permission-filtered queries

**Core abstraction:** A `PermissionFilter` that resolves a user's effective permissions for a vault and produces a set of allowed/denied document IDs (or a SQL subquery). Every query function in `vulcan-core` accepts an optional `PermissionFilter`. When `None` (CLI local mode, owner), no filtering. When `Some`, results are restricted.

**Enforcement strategy — filter at the query layer, not post-hoc:**

| Feature | Enforcement |
|---|---|
| **Search (FTS + hybrid)** | Allowed-document CTE joined into FTS query; denied docs never appear in results or hit counts |
| **Graph (stats, paths, hubs, components)** | Nodes filtered to allowed set; edges to/from denied notes appear as dangling links (no target name or content) |
| **Backlinks** | Only backlinks from readable notes are returned |
| **Vectors / similarity** | Candidate set filtered before ranking; denied notes excluded from neighbor results |
| **Properties / Bases queries** | `WHERE` clause includes permission predicate |
| **Note content (`GET /{id}/notes/{path}`)** | 403 if no read permission |
| **Transclusions / embeds** | Embed of a denied note renders as `[restricted content]` |
| **Activity feed / changes** | Events filtered to permitted documents only |
| **Git history / diffs** | File-level diffs filtered to readable paths |
| **Automerge collab (Phase 16)** | WebSocket handshake checks permission: `editor`+ can edit, `viewer` can observe (read-only cursor), `none` rejected |

**Implementation:**

- [ ] `PermissionFilter` struct in `vulcan-core`: takes user identity + vault ACL config, resolves effective permission per document
- [ ] Method to generate SQL CTE or `IN (...)` clause for allowed document IDs
- [ ] Integrate into all query functions: `search`, `backlinks`, `graph_*`, `vector_neighbors`, `property_query`, `bases_evaluate`
- [ ] Daemon middleware: extract authenticated user from request, build `PermissionFilter`, pass to handlers
- [ ] CLI local mode: no filter (local user has full access to their own vault)
- [ ] Integration tests: verify that denied documents are invisible across search, graph, backlinks, and vector queries
- [ ] Performance: cache the allowed-document set per request (resolve once, reuse across queries in the same request)

### 17.4 Document-level secrets

Two complementary mechanisms for embedding restricted content within otherwise-accessible notes.

**Mechanism A: Folder/tag ACLs + embeds (comes free from 17.2)**

Use the existing ACL system to restrict folders or tags, then embed restricted content into shared notes:

```markdown
# Lord Blackwood
Noble of the Eastern Provinces. Known for his generous charity work.

The townsfolk speak highly of Lord Blackwood's patronage of the arts.

![[GM-Only/NPCs/Blackwood Secrets]]
```

The embedded note `GM-Only/NPCs/Blackwood Secrets.md` is in a restricted folder. When rendered for a player, the embed shows `[restricted content]`. When rendered for a GM, the full content is inlined.

- [ ] Embed rendering respects ACLs: check reader's permission on the embedded target
- [ ] Restricted embeds render as a styled `[restricted content]` placeholder (not silently omitted — the reader knows something exists)
- [ ] Search does not leak restricted embed content in snippets

**Mechanism B: Secret callouts**

For inline secrets co-located with their context — avoids splitting content across files:

```markdown
# Lord Blackwood
Noble of the Eastern Provinces.

> [!secret gm]
> Actually a vampire. CR 15. Plans to betray the party in session 12.
> Weakness: silver weapons, holy water.

## Public Knowledge
The townsfolk speak highly of Lord Blackwood...
```

The `[!secret <role-or-group>]` callout type is stripped from rendered output for users who do not match the specified principal. The principal can be a role name (`owner`, `editor`), a group name, or a username.

- [ ] Parser recognizes `[!secret <principal>]` callout variant; extracts principal and content range
- [ ] `ParsedDocument` stores secret regions with their required principal
- [ ] Rendering pipeline strips secret callout body for unauthorized users
- [ ] Search: secret callout text is indexed but filtered from results/snippets for unauthorized users (uses the same `PermissionFilter` mechanism — secret regions map to a permission check on the principal)
- [ ] Editor UI: secret callouts visually distinguished (e.g., lock icon, colored border) so authors can see what's hidden
- [ ] Nesting: secret callouts inside regular callouts work; nested secret callouts use the most restrictive principal

**Design note:** Both mechanisms protect content at the web/API layer only. The raw `.md` files on disk contain all content in plaintext. Users with filesystem access (CLI, Obsidian, git) see everything. This is intentional — the ACL layer protects the web-facing collaborative interface, not the underlying files.

### 17.5 External share links

Share links allow unauthenticated access to specific content — useful for sharing with people who don't have accounts (e.g., guest players in a pen-and-paper session).

```
https://host/s/{share_token}
```

- [ ] `POST /{id}/shares` — create share: `{ "resource": "note:Handouts/Map.md", "permission": "view", "expires": "2026-04-30", "password": null }`
- [ ] `GET /{id}/shares` — list active shares (owner only)
- [ ] `DELETE /{id}/shares/{share_id}` — revoke share
- [ ] `GET /s/{token}` — resolve share, render content (no auth required)
- [ ] Share tokens stored in `auth.db`, hashed with argon2
- [ ] Resource types: `note:<path>` (single note), `folder:<path>` (folder and children), `tag:<tag>` (all notes with tag)
- [ ] Permission: `view` (read-only rendered content) or `view-raw` (download markdown source)
- [ ] Optional password protection: share link prompts for password before rendering
- [ ] Expiry: shares can have an expiration date or be permanent until revoked
- [ ] Share respects document-level secrets: a shared note still strips `[!secret]` callouts the share's effective role cannot see (shares have an effective role of `viewer` unless configured otherwise)
- [ ] Rate limiting on share endpoints to prevent enumeration
- [ ] CLI: `vulcan auth share create <vault> <resource> [--expires 30d] [--password]`

### 17.6 Future: OIDC / SSO integration

Planned but not in initial scope. Deferred until the local user/group system is stable.

- [ ] OIDC provider configuration in `daemon.toml`: issuer URL, client ID/secret, scopes
- [ ] Login flow: browser redirects to IdP, daemon handles callback, creates/updates local user from claims
- [ ] Group mapping: map OIDC claims/groups to local groups (e.g., IdP group `campaign-gm` → local group `gm`)
- [ ] Hybrid mode: local accounts and OIDC accounts coexist, OIDC users auto-provisioned on first login
- [ ] Token refresh and session management integrated with `auth.db`

---

## Phase 18: Canvas Support

**Goal:** First-class support for Obsidian's JSON Canvas format (`.canvas` files). Index canvas content for search, surface canvas data in the graph, provide CLI commands for inspection and manipulation, and eventually render an interactive canvas editor in the WebUI.

**Depends on:** Phase 7 (core indexing and parsing infrastructure). WebUI canvas editor (18.5) depends on Phase 14 (WebUI write). Canvas ACLs follow from Phase 17.

**Reference:** `references/obsidian-skills/skills/json-canvas/SKILL.md` (JSON Canvas spec and examples), [jsoncanvas.org/spec/1.0](https://jsoncanvas.org/spec/1.0/).

**Design decisions:**
- **Canvas files are a distinct document type, not notes.** They are JSON, not Markdown. The indexer detects `.canvas` files during scan, parses them, and stores structured data (nodes, edges) in dedicated cache tables rather than forcing them through the Markdown/FTS pipeline.
- **Text nodes and file node references are searchable.** Text node content is chunked and indexed in FTS5 so `vulcan search` finds content inside canvases. File nodes generate link references to the vault graph (a canvas linking to a note is a graph edge).
- **Canvas graph integration.** Canvas files participate in the vault graph: a canvas is a node, each file-node reference is an edge to the referenced document, and group membership is captured as metadata. This means backlinks, graph analytics, and doctor all account for canvas relationships.
- **Incremental approach.** Core parsing and indexing first, CLI inspection second, WebUI read-only rendering third, interactive editor last.

### 18.1 Canvas parsing and data model

New module `vulcan-core/src/canvas.rs`:

- [ ] `Canvas` struct: `nodes: Vec<CanvasNode>`, `edges: Vec<CanvasEdge>`
- [ ] `CanvasNode` enum variants: `Text { id, x, y, width, height, text, color }`, `File { id, x, y, width, height, file, subpath, color }`, `Link { id, x, y, width, height, url, color }`, `Group { id, x, y, width, height, label, background, background_style, color }`
- [ ] `CanvasEdge` struct: `id, from_node, from_side, from_end, to_node, to_side, to_end, color, label`
- [ ] `parse_canvas(content: &str) -> Result<Canvas>`: deserialize JSON, validate node types, validate edge references (all `from_node`/`to_node` resolve to existing node IDs)
- [ ] `CanvasColor` type: preset `"1"`–`"6"` or hex string
- [ ] Validation: unique IDs across nodes and edges, valid side/end enum values, required fields per node type
- [ ] Unit tests: parse all examples from `references/obsidian-skills/skills/json-canvas/references/EXAMPLES.md`

### 18.2 Indexing and cache schema

Extend the cache schema and scanner to handle `.canvas` files:

- [ ] New cache tables:
  ```sql
  CREATE TABLE canvas_nodes (
    id TEXT NOT NULL,
    canvas_document_id TEXT NOT NULL REFERENCES documents(id),
    node_type TEXT NOT NULL,  -- 'text', 'file', 'link', 'group'
    x INTEGER, y INTEGER, width INTEGER, height INTEGER,
    content TEXT,             -- text content (text nodes), file path (file nodes), URL (link nodes), label (group nodes)
    color TEXT,
    PRIMARY KEY (canvas_document_id, id)
  );

  CREATE TABLE canvas_edges (
    id TEXT NOT NULL,
    canvas_document_id TEXT NOT NULL REFERENCES documents(id),
    from_node TEXT NOT NULL,
    to_node TEXT NOT NULL,
    from_side TEXT, to_side TEXT,
    from_end TEXT, to_end TEXT,
    label TEXT, color TEXT,
    PRIMARY KEY (canvas_document_id, id)
  );
  ```
- [ ] Scanner: detect `.canvas` extension, parse with `parse_canvas()`, populate `canvas_nodes` and `canvas_edges` tables
- [ ] Text node content indexed in FTS5: each text node becomes a search chunk with `chunk_strategy = "canvas_text"`, heading_path set to `["<canvas filename>", "<group label if any>"]`
- [ ] File node references registered as links in the existing `links` table (link type: `canvas_file_ref`), so they appear in backlinks and graph queries
- [ ] Link nodes (external URLs) stored but not indexed as vault links
- [ ] Incremental rescan: canvas files are rescanned on change like any other document
- [ ] Schema migration: bump `SCHEMA_VERSION`, add migration for new tables

### 18.3 Graph integration

Canvas files participate in the vault knowledge graph:

- [ ] Canvas documents appear as nodes in graph queries (`query_graph_analytics`, `query_hub_notes`, etc.)
- [ ] File-node references create edges from the canvas to the referenced note (edge type: `canvas_ref`)
- [ ] `query_backlinks()` for a note returns canvas references alongside wikilink backlinks, with context showing the canvas name and any edge labels
- [ ] `doctor` validates canvas references: file nodes pointing to non-existent vault files are reported as broken links
- [ ] Canvas-internal edges (between canvas nodes) are stored but not mixed into the vault-level graph — they are a canvas-level concern

### 18.4 CLI commands

```
vulcan canvas [path]                  # show canvas summary (node/edge counts, referenced files)
vulcan canvas list                    # list all canvas files in the vault
vulcan canvas nodes <path>            # list all nodes with type, position, and content preview
vulcan canvas edges <path>            # list all edges with from/to labels
vulcan canvas validate <path>         # validate canvas structure, report errors
vulcan canvas refs <path>             # list all file references and their resolution status
```

- [ ] `vulcan canvas <path>`: summary view — node count by type, edge count, referenced files (resolved/broken), group structure
- [ ] `vulcan canvas list`: list all `.canvas` files with node/edge counts
- [ ] `vulcan canvas nodes <path>`: table of nodes with id, type, position, content preview (truncated text or file path)
- [ ] `vulcan canvas edges <path>`: table of edges with from→to labels and connection details
- [ ] `vulcan canvas validate <path>`: structural validation — ID uniqueness, edge reference integrity, required fields, overlapping nodes warning
- [ ] `vulcan canvas refs <path>`: file references with resolution status (found/missing), useful for vault maintenance
- [ ] `--output json` support on all subcommands
- [ ] Browse TUI: `.canvas` files appear in the note list; pressing Enter on a canvas shows a text summary (node list, edge list) rather than opening in `$EDITOR` (JSON editing is awkward). `o` opens in `$EDITOR` for raw editing.

### 18.5 WebUI canvas rendering (read-only)

Render canvas files as interactive diagrams in the web vault browser (Phase 13+).

- [ ] Canvas detail view: render nodes as positioned boxes on a pannable/zoomable 2D surface
- [ ] Text nodes render Markdown content (reuse existing Markdown renderer)
- [ ] File nodes show a preview of the referenced note (title + excerpt) with a clickable link
- [ ] Link nodes show URL with favicon and a clickable external link
- [ ] Group nodes render as labeled containers with their children inside
- [ ] Edges render as SVG lines/arrows between node connection points, with labels
- [ ] Color presets mapped to the application's theme palette
- [ ] API endpoint: `GET /{id}/canvas/{path}` returns parsed canvas data as JSON (nodes + edges + resolved file references)
- [ ] Canvas list in the vault browser sidebar alongside notes

### 18.6 WebUI canvas editor (interactive)

A visual canvas editor in the web interface, completing the Obsidian canvas experience in the browser.

**Depends on:** Phase 14 (WebUI write infrastructure), 18.5 (read-only rendering).

- [ ] Drag-and-drop node creation: text, file reference (with vault note picker), link, group
- [ ] Node repositioning via drag
- [ ] Node resizing via drag handles
- [ ] Edge creation by dragging between node connection points
- [ ] Text node editing: inline Markdown editor (reuse the note editor component from Phase 14)
- [ ] Group management: drag nodes into/out of groups, resize groups
- [ ] Canvas save: serialize to JSON Canvas format, write via `PATCH /{id}/canvas/{path}`, rescan, auto-commit
- [ ] Undo/redo stack for canvas operations
- [ ] Keyboard shortcuts: delete selected node/edge, copy/paste nodes, zoom controls
- [ ] Automerge integration (if Phase 16 is complete): collaborative canvas editing via the same CRDT sync layer used for notes
- [ ] ACL enforcement: canvas files respect the same folder/tag ACLs as notes (Phase 17)

### 18.7 Cross-cutting integration

- [ ] **Search:** `vulcan search` finds text inside canvas text nodes. The `file:` search operator (9.6.2) matches `.canvas` files. A `type:canvas` or `type:note` operator could filter by document type.
- [ ] **Doctor:** Canvas file references are validated alongside wikilinks. Broken canvas references reported in `vulcan doctor` output.
- [ ] **Move/rename:** When a note referenced by a canvas file node is moved/renamed, the canvas `file` field is updated by the rewrite engine (same mechanism as wikilink rewriting).
- [ ] **HTTP API:** All canvas data accessible via the daemon API. `GET /{id}/canvas/` lists canvases, `GET /{id}/canvas/{path}` returns parsed data. Search results include canvas hits with `document_type: "canvas"`.
- [ ] **Permission filtering (Phase 17):** Canvas files subject to the same ACL rules as notes. File nodes referencing restricted notes render as `[restricted]` for unauthorized users.
- [ ] **Export:** Canvas data included in vault export/backup operations.

### 18.8 Excalidraw support

**Goal:** Parse, index, and (in WebUI) render Excalidraw drawings stored in the vault. Excalidraw is a visual document type similar to JSON Canvas — both are JSON-based with spatial layout — making Phase 18 the natural home.

**Reference:** [Excalidraw plugin](https://github.com/zsviczian/obsidian-excalidraw-plugin)

#### 18.8.1 Parsing and indexing

- [ ] Detect Excalidraw files: `.excalidraw` (plain JSON) and `.excalidraw.md` (Markdown wrapper with LZ-String compressed JSON in a code block)
- [ ] `.excalidraw.md` format parsing: extract the LZ-String compressed JSON from the `excalidraw-json` or `drawing` code fence, decompress, parse as Excalidraw JSON
- [ ] `.excalidraw` format parsing: direct JSON parse
- [ ] Extract text content from Excalidraw elements (text elements, labels on shapes, embedded links) for FTS indexing
- [ ] Extract embedded file references: Excalidraw supports embedding vault images and notes — register these as links in the `links` table
- [ ] Extract frontmatter from `.excalidraw.md` files (Excalidraw plugin stores metadata like `excalidraw-plugin`, `excalidraw-link-prefix`, etc.)
- [ ] Store Excalidraw metadata in cache: reuse `canvas_nodes` pattern or add `excalidraw_elements` table
- [ ] Incremental rescan on file change

#### 18.8.2 CLI commands

- [ ] `vulcan canvas list` extended: include `.excalidraw` and `.excalidraw.md` files alongside `.canvas` files (with type indicator)
- [ ] `vulcan canvas show <path>` for Excalidraw files: element count by type (rectangle, ellipse, text, arrow, etc.), embedded file references, text content preview
- [ ] `vulcan canvas refs <path>` for Excalidraw files: list embedded vault references and their resolution status

#### 18.8.3 WebUI rendering (read-only)

- [ ] Integrate Excalidraw's open-source React component (or a lightweight SVG renderer) for read-only rendering in the vault browser
- [ ] Excalidraw detail view: render the drawing as an interactive pannable/zoomable SVG surface
- [ ] Embedded vault files render as clickable links to the referenced notes
- [ ] API endpoint: `GET /{id}/excalidraw/{path}` returns parsed Excalidraw JSON

#### 18.8.4 WebUI editing (interactive)

- [ ] Embed the full Excalidraw editor component in the WebUI (Excalidraw is open-source, MIT licensed)
- [ ] Save: serialize Excalidraw state back to `.excalidraw.md` or `.excalidraw` format, write via API, rescan, auto-commit
- [ ] Vault file embedding: picker to insert vault note/image references into the drawing
- [ ] ACL enforcement: Excalidraw files respect folder/tag ACLs (Phase 17)

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
                                                 9.3 ──────→ Phase 11 (Git versioning)  Phase 17 (User mgmt & ACL)
                                                                ↓                         ↓
                                                        Phase 12 (Sync)           Phase 13 (WebUI browse)
                                                                                    ↓
                                  Phase 18 (Canvas) ───→ 18.5 (Canvas WebUI read) ← Phase 13
                                    ↑                           ↓
                                  Phase 7              Phase 14 (WebUI write + Automerge)
                                                                ↓
                                                 18.6 (Canvas WebUI editor) ← Phase 14
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
Phase 17 requires 10 (daemon). Sub-phases 17.1–17.3 (users, groups, ACLs, permission-filtered queries) must complete before Phase 13.
Phase 13 requires 10 and 17.1–17.3. Phase 14 requires 13 and 10's write endpoints. Phase 14 introduces Automerge as the document model.
Phase 15 requires 10. Phase 16 requires 13, 14, and 17.4–17.5 (document secrets, share links). Phase 16 also uses the Automerge foundation from Phase 14.
Phase 17.6 (OIDC/SSO) is a future direction — deferred until the local auth system is stable.
Phase 16.6 (local-first/WASM) is a future direction beyond the current roadmap scope.
Phase 18 (Canvas) core parsing/indexing/CLI (18.1–18.4) depends on Phase 7. WebUI read-only rendering (18.5) depends on Phase 13. Interactive canvas editor (18.6) depends on Phase 14. Canvas ACLs follow from Phase 17.
Phase 9.8 (Dataview) builds on Phase 4 (properties and Bases expression language) and Phase 9.6 (search operators, task search). Sub-phase 9.8.1 (inline fields + type inference) and 9.8.2 (list items and tasks) extend the parser pipeline. Sub-phase 9.8.3 (file.* metadata) synthesizes implicit fields from existing cache tables. Sub-phase 9.8.4 (type system and expression evaluator) extends the value representation with Date, Duration, Link types, ~60 built-in functions with auto-vectorization, lambda expressions, link indexing, swizzling, and null ordering. Sub-phases 9.8.5–9.8.7 (DQL parser, evaluation, inline expressions) build the query surface on top. Sub-phase 9.8.8 (DataviewJS) adds sandboxed JS evaluation with full dv API and DataArray behind a `dataviewjs` compile-time feature flag. Sub-phase 9.8.9 imports Dataview plugin settings from `.obsidian/plugins/dataview/data.json`. Dataview metadata and queries are available to all later phases (daemon, web, wiki) as foundation infrastructure.
Phase 9.9 (Templater) builds on Phase 9.7 (enhanced templates) and Phase 9.8.8 (DataviewJS sandbox for JS execution commands). Native tp.date/tp.file/tp.frontmatter modules need no JS; tp.web, user scripts, and execution commands reuse the DataviewJS sandbox.
Phase 9.10 (Tasks plugin) builds on Phase 9.8.2 (task extraction) and adds the Tasks DSL parser, recurring task expansion, dependency graph, and custom status types. Independent of 9.9.
Phase 9.11 (Kanban) builds on Phase 9.8.2 (list item extraction) and Phase 7.1 (metadata refactors). TUI/WebUI rendering depends on Phase 9.2 (browse TUI) and Phase 13 (WebUI) respectively.
Phase 9.12 (AI assistant) builds on Phase 5 (vectors) and Phase 7.12 (query model). Independent of 9.9–9.11. Requires an external inference API.
Phase 9.13 (QuickAdd) is an investigation phase — scope depends on CLI applicability findings. May be deferred or replaced by a Vulcan-native automation DSL.
Phase 9.15 (TaskNotes) builds on Phase 4 (properties/Bases, including 4.5.1 custom source types) and Phase 9.8 (Dataview metadata). It complements Phase 9.10 (Tasks plugin) — TaskNotes uses task-as-note files with rich frontmatter and Bases views, while Tasks plugin operates on inline checkboxes. Calendar sync (9.15.10) is behind a `calendar-sync` feature flag. HTTP API compatibility (9.15.12) depends on Phase 10 (daemon).
Phase 9.16 (Periodic notes) builds on Phase 1 (document indexing) and Phase 9.7 (template variables). It provides shared infrastructure for `file.day` resolution (9.8.3), Kanban date linking (9.11), QuickAdd daily note capture (9.13), and TaskNotes pomodoro storage (9.15). Can start as early as Wave 2 but `file.day` can be stubbed pending its completion.
Phase 9.17 (Unified import) must come after all individual plugin settings importers (9.8.9, 9.9.4, 9.10.5, 9.11.4, 9.13.3, 9.15.11, 9.16.4) are implemented.
Phase 4.5.1 (Custom Bases source types) extends the Bases evaluator with pluggable data sources. The trait and `FileSource` extraction are part of Phase 4. The actual custom source registrations happen in Phase 9.15.8 (TaskNotes Bases views).
Phase 18.8 (Excalidraw) is part of Phase 18 (Canvas) — both are visual JSON-based document types. Parsing/indexing (18.8.1–18.8.2) depends on Phase 7. WebUI rendering (18.8.3) depends on Phase 13. WebUI editing (18.8.4) depends on Phase 14.
Phase 14.1 (Note editor) includes Advanced Tables-style table editing for the WebUI — tab navigation, column management, sorting, CSV paste, and formula support.
See "Phase 9 implementation order" section (after 9.17) for the consolidated critical path and parallelization guide within Phase 9.

---

## New crates (Phases 10+)

| Crate | Type | Purpose |
|-------|------|---------|
| `vulcan-daemon` | lib | axum router, middleware, vault registry, daemon lifecycle |
| `vulcan-auth` | lib | User/group management, ACL evaluation, permission filtering, session/token handling |
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
| `openidconnect` | OIDC client for SSO integration | 17.6 |
