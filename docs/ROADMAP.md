# Vulcan Implementation Roadmap

Tracking document for the phased implementation of Vulcan, a local-first Markdown information hub for Obsidian vaults and plain Markdown directories.
Derived from `docs/design_document.md`. Update task status as work progresses.

**Status legend:** `[ ]` not started | `[~]` in progress | `[x]` complete | `[-]` cut/deferred

## Delivery horizons and phase gates

The numbered phases describe dependency order, not a requirement to implement every documented idea before advancing. A later capability may be designed early and implemented opportunistically when its prerequisites are already available.

- **Committed delivery path:** Phase 9's pre-daemon gate ends at 9.29 and is complete. Phase 10 is therefore the next architectural milestone; unfinished candidate integrations do not block it.
- **Completed optional additions:** 9.30 (Outline publishing) and 9.31 (folder-note normalization) landed as independently useful work after the Phase 9 gate. Their numbering records implementation history rather than extending the daemon prerequisite chain.
- **Active optional addition:** 9.35 materializes large hierarchical Markdown documents as link-safe wiki trees. It builds on completed parser, refactor, attachment, and folder-note foundations without extending the Phase 10 gate.
- **Committed hub direction:** Phase 12 owns device/file-tree synchronization and Phase 15 owns external document bindings, content routes, and knowledge-system connectors. SilverBullet, Outline, HedgeDoc, and Git wiki work should extend those shared layers rather than become parallel product architectures.
- **Committed application-platform direction:** Phase 19 owns immutable `.vapp` packages, installation/instance lifecycle, sandboxed browser applications, typed app CLI commands, QuickJS host functions, server/browser WebAssembly components, and explicit canonical app data. It builds on the daemon, WebUI, and capability model without extending the Phase 10 gate.
- **Candidate capability tracks:** mdbase expansion and additional native vault workflows with compatibility adapters are maintained below as detailed design backlogs. They retain no implied promise of implementation order or completion before Phase 10.
- **Promotion gate:** move a candidate into the committed path only when there is a concrete use case, a capability-oriented domain and public surface, an identified dependency/ownership boundary, a sustainable adapter compatibility and testing strategy, and enough maintenance budget to support the advertised surface. Promote only the smallest independently useful native slice; importing one plugin's settings is not by itself a product boundary.
- **Placement rule:** durable Markdown semantics, parsing, diagnostics, and mutation-free exports may live in core/app tracks; daemon transports belong to Phase 10+, sync protocols to Phase 12, editor behavior to Phase 14, and supervised runtimes or first-party external integrations to Phase 15.

This keeps Vulcan's long-term interoperability ambition without treating every plausible adapter, plugin behavior, or external runtime as a prerequisite for the next platform layer.

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

- [x] `BasesSource` trait: `fn rows(&self, filter: &Filter) -> Result<Vec<Row>>` — pluggable data source that can produce rows for Bases evaluation
- [x] Built-in source: `FileSource` — queries the documents table (current behavior, extracted into the trait)
- [x] Custom source registration: `BasesEvaluator::register_source(name, source)` — register a named source type
- [x] Source type in `.base` files: `source.type` field selects the data source (default: `file`; custom sources like `tasknotes` are registered by their respective phases)
- [x] Source config passthrough: `source.config` is forwarded to the source implementation (e.g., `config.type: tasknotesTaskList` for TaskNotes views)
- [x] Custom sources participate in the same filter/sort/group/formula pipeline as file-based queries
- [x] Custom sources can define additional computed columns (e.g., TaskNotes urgency score, days until due)

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
- [x] Add shared additive publication selection plans with multiple query/graph clauses, per-clause seeds/direction/depth/result/traversal filters, authoritative exclusions, cycle/node bounds, and selection provenance
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
- [x] **Create note from Bases view** (matches Obsidian behavior):
  - [x] Derive the target folder from the view's filter context — if the view has a `file.folder = "Projects"` or `file.inFolder("Projects")` filter, new notes are created in `Projects/`
  - [x] Filter analysis: walk the filter tree to extract folder constraints; prefer the most specific folder if multiple constraints exist
  - [x] Fallback: if no folder can be derived, use the vault root or a configurable default
  - [x] Pre-populate frontmatter properties from the view's filter context — if the view filters on `status = "todo"`, new notes get `status: todo` in frontmatter
  - [x] Property derivation rules: only derive from equality filters (`=`, `is`), not from range or contains filters
  - [x] Template support: if the view has an associated template (configurable per `.base` file via `create_template` key), use it as the base
  - [x] TUI: `n` hotkey in Bases TUI creates a new note with derived folder and properties, then opens in `$EDITOR`
  - [x] CLI: `vulcan bases create <file.base> [--title <title>]` — create a note matching the view's context
  - [x] `--dry-run` shows derived folder, properties, and template without creating

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

**Goal:** Improve the interactive CLI experience with direct note editing, a persistent browser TUI, auto-commit integration, and quality-of-life commands. Later sub-phases (9.18) restructure the entire command surface into a two-level hierarchy, add single-note CRUD, a general-purpose JS runtime with REPL, web/git tools for agent use, and integrated documentation. These features make vulcan a practical daily-driver tool for vault maintenance and the foundation for AI integrations.

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
- [x] Nested task query semantics: when a TASK query matches a parent, include child tasks in results even if children don't independently match the WHERE clause. Task hierarchy is preserved in output.
- [x] Tasks inherit page-level fields (frontmatter, inline fields) from their containing note
- [x] Tasks plugin emoji shorthand: detect `🗓️` (due), `✅` (completion), `➕` (created), `🛫` (start), `⏳` (scheduled) date annotations in task text and store as task properties with auto-parsed Date type
- [x] Tasks plugin priority levels: detect `⏫` (highest), `🔺` (high), `🔼` (medium), `🔽` (low), `⏬` (lowest) and store as `priority` task property
- [x] Tasks plugin recurrence notation: detect `🔁 every <pattern>` in task text and store as `recurrence` task property (parsing the RRULE pattern is deferred to §9.10)
- [x] Tasks plugin dependency notation: detect `⛔ <id>` (blocked by) and `🆔 <id>` (task ID) and store as task properties (dependency resolution deferred to §9.10)
- [x] Unit tests: basic tasks, nested tasks, tasks with inline fields, custom status characters
- [x] Unit tests: `fullyCompleted` recursive check, nested task inclusion semantics, emoji shorthand date parsing, priority levels
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
- [x] Subtag inheritance in FROM sources: `FROM #A` matches notes with `#A`, `#A/B`, `#A/B/C`, etc.
- [x] Unit tests: each `file.*` field resolves correctly from cache data
- [x] Integration test: DQL queries using `file.*` fields produce expected results

#### 9.8.4 Data type system and expression evaluator

Extend the expression evaluator to support Dataview's full type system and expression language. This is the foundation for DQL evaluation and inline expressions.

**Type system:**
- [x] Extend the value representation to support all 8 Dataview types: Text, Number, Boolean, Date, Duration, Link, List, Object
- [x] Date type with sub-field access: `.year`, `.month`, `.day`, `.hour`, `.minute`, `.second`, `.millisecond`, `.weekday`, `.week`, `.weekyear`
- [x] Date literal shortcuts: `date(today)`, `date(now)`, `date(tomorrow)`, `date(yesterday)`, `date(sow)`, `date(eow)`, `date(som)`, `date(eom)`, `date(soy)`, `date(eoy)`
- [x] Duration type with compound units: `dur(1d 3h 20m)`, individual unit abbreviations (`s`, `m`, `h`, `d`, `w`, `mo`, `yr`)
- [x] Link as first-class type with metadata access via `meta(link)`: `.path`, `.display`, `.embed`, `.type`, `.subpath`
- [x] Type coercion: Date - Date → Duration, Date ± Duration → Date, Duration + Duration → Duration, String + Number → String (concatenation), String * Number → String (repeat)
- [x] Null ordering: `null` is less than all non-null values; `null` first in ascending sort, last in descending; `null` propagates through most arithmetic/function calls
- [x] GROUP BY null handling: rows with `null` group key form a separate group with `key = null`
- [x] Date timezone semantics: `date(today)`, `date(now)`, etc. use system local timezone; `localtime(date)` converts UTC to local; timezone override configurable via `.vulcan/config.toml`
- [x] `typeof(value)` introspection returning type name strings

**Expression language extensions:**
- [x] Arithmetic operators on numbers, dates, and durations: `+`, `-`, `*`, `/`, `%`
- [x] Dotted field access: `object.field`, `object["field"]`
- [x] Array indexing: `array[0]`, `array[expr]` (0-indexed)
- [x] Link indexing: `[[Note]].field` — cross-note field access (join against linked page's metadata); returns `null` if target note doesn't exist; follows Vulcan's link resolution (shortest-path, alias matching)
- [x] Array/DataArray swizzling: `array.field` auto-maps and flattens; chained swizzling (`array.field.subfield`) maps through nested objects; null propagation through swizzles
- [x] Lambda expressions: `(arg1, arg2) => expression` for use in higher-order functions
- [x] Column aliases: `field AS "Display Name"` in TABLE/LIST projections
- [x] `WITHOUT ID` modifier for TABLE and LIST queries
- [x] Keyword-escaped field access: `row["where"]` for reserved word collision (all DQL keywords must be escapable)
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
- [x] DQL parser: recursive descent parser producing the internal query AST
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
- [x] Compile FROM clauses to source/filter primitives (tag → `tags` table filter, folder → `documents.path` prefix, links → `links` table join, outgoing → forward `links` join)
- [x] Compile WHERE expressions to `FilterExpression` structs (shared with Bases and `--where` CLI flag)
- [x] Data commands executed in source order (except FROM which is always first); multiple WHERE, SORT, FLATTEN, GROUP BY clauses allowed and composed sequentially
- [x] Computed GROUP BY: `GROUP BY (expr) AS name` with arbitrary expression
- [x] Computed FLATTEN: `FLATTEN (expr) AS name` assigns flattened result to a new field; if expression returns non-array, treat as single-element array
- [x] Multiple blocks per note: a note can contain multiple `` ```dataview `` blocks; `--block <n>` selects by 0-based index, default evaluates all
- [x] Error recovery: malformed DQL produces diagnostics, not panics
- [x] Unit tests: parse each clause type, boolean FROM combinations, nested WHERE expressions, lambda expressions, link indexing, `WITHOUT ID`, `AS` aliases, computed GROUP BY/FLATTEN, multiple data commands, malformed input
- [x] Integration test: round-trip DQL parse → AST → evaluation against a test vault

#### 9.8.6 DQL evaluation and CLI surface

Execute parsed DQL queries against the cache and expose results via CLI.

- [x] `vulcan dataview eval <file> [--block <n>]` — evaluate a DQL code block from a specific note (by block index or the first/only block)
- [x] `vulcan dataview query <dql-string>` — evaluate a DQL query string directly from the command line
- [x] TABLE output: columnar table in human mode, array-of-objects in `--output json`; `WITHOUT ID` suppresses file link column
- [x] LIST output: note list with optional expression values; `WITHOUT ID` shows only the expression value
- [x] TASK output: task items grouped by source note, with status, text, `visual`, and all task metadata fields (`checked`, `completed`, `fullyCompleted`); nested task inclusion semantics (children included when parent matches)
- [x] CALENDAR output: JSON with date-keyed entries (human mode shows a flat date-grouped list; calendar rendering is a WebUI concern)
- [x] GROUP BY support: produces `{ key, rows }` objects; `rows.field` extracts list of values; aggregation functions (`sum(rows.field)`, `length(rows)`, etc.) work over grouped rows
- [x] FLATTEN support: list expansion into individual result rows; multiple FLATTEN clauses compose sequentially; `FLATTEN expr AS name` assigns to a new field
- [x] LIMIT support: cap result count (applied after all other data commands)
- [x] SORT with multi-key tiebreaking and correct type-aware ordering
- [x] `file.*` namespace fully accessible in all expressions (WHERE, TABLE columns, SORT, GROUP BY, FLATTEN)
- [x] Link indexing in expressions: `[[Note]].field` resolves field from the linked note's metadata
- [x] Diagnostics for unsupported DQL features surfaced in output
- [x] `--output json` on all subcommands
- [x] Empty result handling: TABLE with 0 results shows headers + result count; LIST with 0 results shows empty; TASK with 0 results shows nothing
- [x] Result count display: configurable via Dataview settings (`displayResultCount`); show count in TABLE/TASK headers by default
- [x] Configurable column names: `primaryColumnName` (default `"File"`), `groupColumnName` (default `"Group"`) from Dataview settings
- [x] Integration tests: TABLE, LIST, TASK, CALENDAR queries; GROUP BY with aggregation and null keys; FLATTEN with nested arrays and non-array expressions; multi-clause queries; `WITHOUT ID`; link indexing; empty results; all against test vault with known results

#### 9.8.7 Inline expression evaluation

Support Dataview inline expressions (`` `= expr` ``) for note rendering and query contexts.

- [x] Detect inline expressions (backtick-delimited text starting with configurable prefix, default `=`) during the semantic pass; store as inline expression metadata
- [x] Configurable inline query prefix from Dataview settings (`inlineQueryPrefix`, default `"="`); also detect inline DataviewJS prefix (`inlineJsQueryPrefix`, default `"$="`) when `js_runtime` feature is enabled
- [x] `this` binding: within an inline expression, `this` resolves to the current note's full metadata (frontmatter + inline fields + `file.*` implicit metadata)
- [x] Reuse the extended expression evaluator (9.8.4) with the `this` context binding and full function library
- [x] Known limitation: inline expressions store the expression text, not the evaluated result — other notes cannot query the result of an inline expression (this matches Dataview behavior)
- [x] `vulcan dataview inline <file>` — evaluate all inline expressions in a note, output results alongside source expressions
- [x] In `--output json` mode, include evaluated inline expression results in note metadata
- [x] Diagnostics for expressions that fail to evaluate (type errors, missing fields)
- [x] Unit tests: `this.property` access, `this.file.name`, nested field access, function calls, missing field handling
- [x] Integration test: note with inline expressions, verify evaluation results

#### 9.8.8 DataviewJS evaluation (compile-time feature flag)

Evaluate `` ```dataviewjs `` code blocks using an embedded, sandboxed JavaScript runtime. Gated behind the `js_runtime` Cargo feature flag (enabled by default).

**Detection and fallback (always available):**
- [x] Detect `dataviewjs` code blocks during parsing
- [x] Store as block metadata with `language = "dataviewjs"`
- [x] When feature is not compiled in: emit diagnostic "DataviewJS blocks require the `js_runtime` feature flag"
- [x] Exclude from FTS indexing (code, not content)
- [x] Unit test: `dataviewjs` block detected and diagnosed without feature flag

**JS runtime integration (behind `js_runtime` feature):**
- [x] Add `js_runtime` feature flag to `vulcan-core/Cargo.toml` and `vulcan-cli/Cargo.toml`
- [x] Embed JS runtime: rquickjs (QuickJS) — chosen for binary size (~300KB vs ~15MB Boa vs ~40MB V8), sub-millisecond startup, built-in sandboxing primitives (`set_memory_limit()`, `set_max_stack_size()`, `set_interrupt_handler()`), and ES2023 compliance. See 9.18.5 for the full JS runtime design including REPL, vault API, and sandbox levels.
- [x] Sandbox constraints: no filesystem access, no network access, no `eval` of external scripts
- [x] Execution timeout: configurable via `.vulcan/config.toml` (default 5 seconds per block)
- [x] Memory limit: cap JS heap allocation via `Runtime::set_memory_limit()` to prevent runaway scripts

**`dv` API object — query methods:**
- [x] `dv.pages(source?)` — return DataArray of page objects matching a DQL FROM source (or all pages)
- [x] `dv.page(path)` — return a single page's metadata object
- [x] `dv.current()` — return current note's metadata (`this` equivalent)
- [x] `dv.query(dql, [file], [settings])` — evaluate DQL, return `{ successful: boolean, value: result }` or `{ successful: false, error: string }`
- [x] `dv.tryQuery(dql, [file], [settings])` — like `dv.query()` but throws on failure
- [x] `dv.queryMarkdown(dql, [file], [settings])` — evaluate DQL, return rendered Markdown string
- [x] `dv.tryQueryMarkdown(dql, [file], [settings])` — like `dv.queryMarkdown()` but throws on failure
- [x] `dv.execute(dql)` — shorthand: evaluate DQL and render results directly (reuses 9.8.6 evaluation engine)
- [x] Page objects expose frontmatter, inline fields, and full `file.*` namespace — same fields as DQL queries

**`dv` API object — render methods:**
- [x] `dv.table(headers, rows)` — render table output (CLI: columnar; JSON: array-of-objects)
- [x] `dv.list(items)` — render list output
- [x] `dv.taskList(tasks, groupByFile?)` — render task list output
- [x] `dv.paragraph(text)`, `dv.header(level, text)`, `dv.el(element, text, [attrs])`, `dv.span(text)` — text/element output (map to plain text in CLI)
- [x] `dv.container` — reference to output container (CLI: output buffer object; WebUI: DOM element; used for CSS class manipulation)

**`dv` API object — I/O and view methods:**
- [x] `dv.io.load(path)` — read a note's content as string (read-only, within vault boundary only)
- [x] `dv.io.csv(path, [originFile])` — load and parse a CSV file, return DataArray of row objects
- [x] `dv.io.normalize(path, [originFile])` — resolve a vault-relative path
- [x] `dv.view(path, [input])` — load and execute an external JS file from the vault; `path` relative to vault root; optional `input` object available to loaded script; vault-boundary enforcement applies. Associated CSS file loading (`<path>.css`) deferred to WebUI phase.

**`dv` API object — utility methods:**
- [x] `dv.date(input)`, `dv.duration(input)` — type constructors matching DQL semantics
- [x] `dv.compare(a, b)`, `dv.equal(a, b)` — Dataview comparison/equality semantics
- [x] `dv.clone(value)` — deep clone a value
- [x] `dv.func.*` — namespace exposing all DQL built-in functions (e.g., `dv.func.contains()`)
- [x] `dv.luxon` — expose date/time library API (Luxon-compatible or Vulcan equivalent)

**DataArray implementation:**
- [x] DataArray wraps query results with chainable methods: `.where(pred)`, `.filter(pred)`, `.map(fn)`, `.flatMap(fn)`, `.sort(key, [dir])`, `.groupBy(key)`, `.unique()`, `.distinct()`, `.limit(n)`, `.slice(start, [end])`, `.concat(other)`, `.indexOf(value)`, `.find(pred)`, `.findIndex(pred)`, `.includes(value)`, `.join(sep)`, `.every(pred)`, `.some(pred)`, `.none(pred)`
- [x] Dataview-specific methods: `.sortInPlace(key, [dir])`, `.groupIn(key)` (recursive top-down grouping), `.mutate(fn)` (in-place mutation), `.into(key)` (map without flattening), `.expand(fn)` (recursive expansion), `.forEach(fn)`, `.array()` (convert to plain array), `.values` (raw array access)
- [x] Swizzling: `dataArray.field` auto-maps and flattens; chained swizzling works through nested objects

**CLI surface:**
- [x] `vulcan dataview eval <file> [--block <n>]` evaluates DataviewJS blocks when feature is compiled in (same command as DQL, dispatches by block language)
- [x] `vulcan dataview query-js <js-string>` — evaluate a JS snippet directly from the command line
- [x] `--output json` on both subcommands
- [x] Diagnostics for runtime errors, timeout, and sandbox violations

**Testing:**
- [x] Unit tests: `dv.pages()`, `dv.page()`, `dv.current()`, `dv.table()`, `dv.list()`, `dv.taskList()`, `dv.execute()`
- [x] Integration test: DataviewJS blocks in test vault produce expected output
- [x] Sandbox test: verify filesystem/network access is blocked, timeout triggers correctly
- [x] Feature flag test: build without `js_runtime`, verify detection-only behavior

#### 9.8.9 Dataview plugin settings import

Read and respect Dataview's per-vault configuration from `.obsidian/plugins/dataview/data.json` for seamless migration.

- [x] Discover and parse `.obsidian/plugins/dataview/data.json` during vault initialization
- [x] Import settings: `inlineQueryPrefix` (default `"="`), `inlineJsQueryPrefix` (default `"$="`), `enableDataviewJs`, `enableInlineDataviewJs`, `taskCompletionTracking`, `taskCompletionUseEmojiShorthand`, `taskCompletionText`, `recursiveSubTaskCompletion`, `displayResultCount`, `defaultDateFormat`, `defaultDateTimeFormat`, `maxRecursiveRenderDepth`, `primaryColumnName`, `groupColumnName`
- [x] Merge into runtime config with `.vulcan/config.toml` overrides taking precedence
- [x] Settings not found in the Dataview config fall back to Vulcan defaults
- [x] Unit test: parse sample `data.json`, verify settings merge and precedence
- [x] Integration test: vault with custom Dataview settings, verify inline prefix and display settings are respected
- Explicit `vulcan config import dataview` command is in 9.17.5; this section covers auto-load during vault initialization

#### 9.8.10 Cross-cutting integration

- [x] **Search:** DQL code blocks and inline expressions are stored as metadata but excluded from FTS content indexing (they are queries, not prose). Inline field *values* are included in FTS.
- [x] **Doctor:** Report notes with DQL blocks that fail to parse. Report inline fields with type inconsistencies against the property catalog. Report DataviewJS blocks (diagnosed when feature not compiled in).
- [x] **Browse TUI:** `Ctrl-V` toggles the detail pane between the raw file/snippet preview and a Dataview inspector showing evaluated inline expressions plus DQL/DataviewJS block results for the selected note.
- [x] **HTTP API:** Single-vault serve mode exposes structured Dataview endpoints: `GET /dataview/query`, `GET /dataview/query-js`, `GET /dataview/eval`, and `GET /dataview/inline`.
- [x] **Property queries:** Inline fields and `file.*` fields are queryable via the existing `--where` filter surface. `vulcan query --where "due < date(today)"` finds notes where the `due` inline field is in the past. `vulcan query --where "file.size > 10000"` finds large notes.
- [x] **Bases interop:** Bases views and DQL queries share the same expression evaluation engine and filter primitives. A Bases view and a DQL TABLE query with equivalent logic should produce identical results.
- [x] **Dataview test vault:** `tests/fixtures/vaults/dataview/` must exercise all features: inline fields (all variants, type inference, formatting edge cases), list items (plain and task, nested), `file.*` metadata access (including `file.day`, `file.tags` subtag expansion), DQL queries (TABLE, LIST, TASK, CALENDAR), GROUP BY (with null keys, computed expressions), FLATTEN (with non-array expressions, sequential composition), inline expressions (with configurable prefix), function calls (including vectorization, regex functions in WHERE), link indexing (`[[Note]].field` including missing targets), date/duration arithmetic, null ordering, Tasks plugin emoji shorthand, and DataviewJS blocks (evaluated when feature is compiled in, diagnosed otherwise).

#### 9.8 Recommended implementation order

1. **Inline field type inference** (9.8.1 additions) — add automatic type detection for inline field values so typed comparisons work from the start.
2. **List item extraction** (9.8.2 list items) — extend the parser to capture all list items, not just tasks. Migrate `tasks` to reference `list_items`.
3. **Implicit file metadata** (9.8.3) — implement `FileMetadataResolver` so `file.*` fields are available to the expression evaluator.
4. **Type system and expression evaluator** (9.8.4) — extend value representation, add Date/Duration/Link types, implement the full function library with vectorization, add lambda support, link indexing, swizzling, and null ordering.
5. **DQL parser** (9.8.5) — tokenizer and recursive descent parser producing the internal query AST, including computed GROUP BY/FLATTEN.
6. **DQL evaluation and CLI** (9.8.6) — wire the parser to the evaluator, implement GROUP BY / FLATTEN / LIMIT semantics with null key handling, add CLI commands.
7. **Inline expressions** (9.8.7) — configurable prefix, `this` binding, and CLI evaluation command.
8. **Dataview settings import** (9.8.9) — read `.obsidian/plugins/dataview/data.json` so all configurable behavior respects per-vault settings.
9. **DataviewJS** (9.8.8) — detection always; sandboxed JS evaluation with full `dv` API, DataArray, and `dv.view()` behind `js_runtime` feature flag.
10. **Cross-cutting integration** (9.8.10) — search exclusions, doctor checks, API endpoints, comprehensive test vault.

### 9.9 Templater-compatible template engine

**Goal:** Support Templater-style `<% %>` template syntax in Vulcan's template system, allowing users to share templates between Obsidian (with Templater) and Vulcan. The DataviewJS sandbox (9.8.8) provides the JS runtime foundation; Templater reuses it for `<%* %>` execution commands.

**Builds on:** Phase 9.7 (enhanced templates), Phase 9.8.8 (DataviewJS sandbox for JS execution).
**Design refs:** §12b (expression evaluator), existing `template` command (9.4.3/9.7)
**Reference material:** `references/Templater/` (Templater source and documentation)

#### 9.9.1 Template syntax parsing

- [x] Parse Templater command tags: `<% expr %>` (interpolation), `<%* code %>` (JS execution), `<%+ expr %>` (dynamic/deferred)
- [x] Whitespace control: `<%_`/`_%>` (trim all whitespace), `<%-`/`-%>` (trim one newline)
- [x] Detect Templater syntax in `.vulcan/templates/` and Obsidian template folder
- [x] Backward compatibility: existing `{{date}}`, `{{title}}` variables continue to work; Templater syntax is an extension
- [x] Templater folder discovery: read Templater settings from `.obsidian/plugins/templater-obsidian/data.json` for template folder location and user script folder

#### 9.9.2 `tp` API object — native modules

Implement the `tp` namespace natively (no JS required) for the most common template functions:

**tp.date:**
- [x] `tp.date.now(format?, offset?, reference?, reference_format?)` — current/relative date with Moment.js-compatible formatting (reuse 9.7.1 format engine)
- [x] `tp.date.tomorrow(format?)`, `tp.date.yesterday(format?)` — convenience shortcuts
- [x] `tp.date.weekday(format?, weekday_number?, reference?, reference_format?)` — specific weekday

**tp.file:**
- [x] `tp.file.title` — filename without extension
- [x] `tp.file.path(absolute?)` — file path (vault-relative or absolute)
- [x] `tp.file.folder(absolute?)` — parent folder name or path
- [x] `tp.file.creation_date(format?)`, `tp.file.last_modified_date(format?)` — file timestamps
- [x] `tp.file.content` — full file content
- [x] `tp.file.tags` — all tags in file
- [x] `tp.file.exists(filepath)` — check if file exists in vault
- [x] `tp.file.include(filepath)` — include another template (recursive, depth limit 10)
- [x] `tp.file.create_new(template, filename, open_new?, folder?)` — create new note from template
- [x] `tp.file.move(new_path)`, `tp.file.rename(new_name)` — file operations (reuse move-rewrite engine)
- [x] `tp.file.cursor(order?)` — insert cursor position placeholder (meaningful in editor contexts; no-op in non-interactive CLI)

**tp.frontmatter:**
- [x] `tp.frontmatter.<key>` — direct access to frontmatter properties (reuse property resolver)
- [x] Bracket notation for keys with spaces: `tp.frontmatter["key name"]`

**tp.system (CLI-adapted):**
- [x] `tp.system.prompt(text, default?, throw_on_cancel?, multi_line?)` — CLI: read from stdin or use `--var key=value` flag; TUI: show input dialog
- [x] `tp.system.suggester(items, values, ...)` — CLI: use existing note picker or `--var` flag; TUI: show selection picker
- [x] `tp.system.clipboard()` — read system clipboard (platform-dependent, best-effort)

#### 9.9.3 `tp` API object — JS-dependent modules (behind `js_runtime` feature)

These require the sandboxed JS runtime and are only available when `--features js_runtime` is compiled:

- [x] `<%* %>` execution commands — arbitrary JS with `tR` output accumulator
- [x] `tp.web.request(url, json_path?)` — sandboxed HTTP GET (allowlist-based, configurable)
- [x] `tp.web.daily_quote()`, `tp.web.random_picture(size?, query?)` — convenience web functions
- [x] User script functions: load `.js` files from configured scripts folder as `tp.user.<name>(args)`
- [x] System command user functions: execute shell commands with template variable substitution (requires explicit opt-in via config, disabled by default for security)
- [x] `tp.hooks.on_all_templates_executed(callback)` — post-processing hook

**tp.config:**
- [x] `tp.config.template_file` — TFile object (or Vulcan equivalent) for the template being processed
- [x] `tp.config.target_file` — TFile object for the note the template is being inserted into
- [x] `tp.config.run_mode` — numeric run mode indicator (0=create, 1=append, 5=dynamic; map to Vulcan equivalents)
- [x] `tp.config.active_file` — currently active file (alias for target in CLI context)

**tp.obsidian (Vulcan equivalents):**
- [x] `tp.obsidian.normalizePath(path)` — normalize vault-relative path (reuse Vulcan's path normalization)
- [x] `tp.obsidian.htmlToMarkdown(html)` — convert HTML string to Markdown (use existing or add lightweight converter)
- [x] `tp.obsidian.requestUrl(url)` — sandboxed HTTP request (reuse `tp.web` infrastructure, same allowlist restrictions)
- [x] Emit diagnostic for Obsidian-specific APIs under `tp.app` that have no CLI equivalent (e.g., `tp.app.workspace`, `tp.app.vault.adapter`)

#### 9.9.4 Settings import

- [x] Read Templater settings from `.obsidian/plugins/templater-obsidian/data.json`:
  | Setting key | Vulcan mapping |
  |---|---|
  | `templates_folder` | Template discovery path |
  | `templates_pairs` | User system command function assignments |
  | `user_scripts_folder` | User script discovery path for `tp.user.*` |
  | `enable_system_commands` | Enable/disable `tp.system` command execution |
  | `shell_path` | Shell path for system commands |
  | `folder_templates` | Auto-apply templates on folder-based note creation |
  | `trigger_on_file_creation_mode` | Select none, folder, or regex creation behavior |
  | `ignore_folders_on_creation` | Exclude configured folders from creation triggers |
  | `startup_templates` | Templates to run on vault open (map to `vulcan template run-startup`) |
  | `trigger_on_file_creation` | Auto-template on new file creation |
  | `syntax_highlighting` | Informational only (no CLI equivalent) |
  | `auto_jump_to_cursor` | Informational only (no CLI equivalent) |
- [x] `vulcan config import templater` — import Templater settings and report mapping
- [x] Accept both numeric and current string-encoded `intellisense_render` values during Templater import
- Refactor to implement `PluginImporter` trait when 9.17.1 lands

#### 9.9.5 CLI integration

- [x] `vulcan template` command detects Templater syntax and processes it (existing command, extended)
- [x] `vulcan template --engine native|templater|auto` — force template engine selection (default: auto-detect based on `<% %>` presence)
- [x] `--var key=value` flag for non-interactive template variable binding (replaces `tp.system.prompt()` in CI/automation contexts)
- [x] Template preview: `vulcan template preview <name>` — show expanded template without creating a file
- [x] Error diagnostics for Templater syntax that requires unavailable features (e.g., `tp.web` without `js_runtime` feature)
- [x] Integration test: Templater-syntax templates produce expected output, including `tp.file`, `tp.date`, `tp.frontmatter` access

#### 9.9.6 File-creation trigger execution

- [x] Apply configured folder mappings to notes created through the reusable `vulcan-app` note workflow; inherit the nearest ancestor mapping and let an explicit `--template` win
- [x] Apply ordered file-regex mappings and surface invalid patterns as diagnostics
- [x] Evaluate inline Templater/native commands on creation when no mapping replaces the note body
- [x] Track create paths separately from modifications in the file watcher and apply triggers from `vulcan watch` / `vulcan index watch` before refreshing the cache
- [x] Exclude configured template and ignored folders, preserve existing frontmatter, enforce watcher read/write permissions, and prevent modify-event trigger loops
- [x] Add config, note-creation, external-file, and watcher regression tests

#### 9.9.7 Startup-template execution follow-up

- [ ] Implement the documented `vulcan template run-startup` command as an explicit trusted workflow; startup templates execute for side effects without rewriting the template source
- [ ] Decide which long-lived daemon/serve entrypoints may invoke startup templates, keeping execution default-off and permission-gated

### 9.10 Tasks plugin compatibility (parsing and query layer)

**Goal:** Compatibility with the Obsidian Tasks plugin — parse `` ```tasks `` query blocks, support recurring task expansion, task dependencies, custom status types, and priority-based filtering. This extends the Dataview task extraction (9.8.2) with Tasks-plugin-specific features. Phase 9.10 provides the **parsing and query engine** for inline checkbox tasks; TaskNotes (9.15) is the primary task management model for Vulcan. Shared infrastructure (recurring tasks, dependencies, custom statuses) is implemented here and reused by 9.15.

**Builds on:** Phase 9.8.2 (task extraction and storage), Phase 9.8.4 (expression evaluator).
**Reference material:** [Obsidian Tasks documentation](https://publish.obsidian.md/tasks/)

#### 9.10.1 Tasks query language parser

- [x] Detect `` ```tasks `` fenced code blocks during parsing; store raw query text as block metadata
- [x] Tasks DSL parser: line-based filter language (each line is a filter or instruction)
  - [x] Status filters: `not done`, `done`, `status.name includes <text>`, `status.type is <type>`
  - [x] Date filters: `due before <date>`, `due after <date>`, `due on <date>`, `has due date`, `no due date` — and same for `created`, `start`, `scheduled`, `done` dates
  - [x] Property filters: `description includes <text>`, `path includes <text>`, `heading includes <text>`, `tag includes <tag>`, `priority is <level>`
  - [x] Recurrence filters: `is recurring`, `is not recurring`
  - [x] Dependency filters: `is blocked`, `is not blocked`, `has id`
  - [x] Boolean composition: `(filter1) AND (filter2)`, `(filter1) OR (filter2)`, `NOT (filter)`
  - [x] Sort instructions: `sort by <field> [reverse]`
  - [x] Group instructions: `group by <field> [reverse]`
  - [x] Limit: `limit <n>`, `limit groups <n>`
  - [x] Display options: `hide <field>`, `show <field>`, `short mode`
  - [x] Explain: `explain` — output the parsed query plan

#### 9.10.2 Recurring task support

- [x] Parse recurrence patterns from task text: `🔁 every <pattern>` (Tasks emoji) and `[repeat:: <pattern>]` (Dataview inline field)
- [x] Support recurrence patterns: `every day`, `every week`, `every month`, `every year`, `every <n> days/weeks/months/years`, `every weekday`, `every Monday`, `every month on the 15th`
- [x] Optional RRULE support for complex recurrence (RFC 5545 subset)
- [x] Recurrence expansion: given a recurring task, compute next occurrence dates for query purposes
- [x] `vulcan tasks next <n>` — show next N upcoming task instances (expanding recurrence)
- [x] Store recurrence metadata in `task_properties` for query access

#### 9.10.3 Task dependencies

- [x] Parse dependency annotations: `🆔 <id>` (task identifier), `⛔ <id>` (blocked by)
- [x] Build task dependency graph from `tasks` and `task_properties` tables
- [x] `is blocked` / `is not blocked` filter: a task is blocked if any of its `⛔` dependencies are not completed
- [x] `vulcan tasks blocked` — list all blocked tasks with their blocking dependencies
- [x] `vulcan tasks graph` — show task dependency graph (reuse graph analysis infrastructure)

#### 9.10.4 Custom status types

- [x] Support Tasks plugin custom status configuration: `[x]` = DONE, `[ ]` = TODO, `[/]` = IN_PROGRESS, `[-]` = CANCELLED, `[!]` = IMPORTANT, etc.
- [x] Status type categories: `TODO`, `DONE`, `IN_PROGRESS`, `CANCELLED`, `NON_TASK` — configurable via `.vulcan/config.toml` or imported from Tasks plugin settings
- [x] Read Tasks plugin status configuration from `.obsidian/plugins/obsidian-tasks-plugin/data.json`
- [x] `status.type` and `status.name` queryable in both DQL and Tasks DSL

#### 9.10.5 Settings import

- [x] Read Tasks plugin settings from `.obsidian/plugins/obsidian-tasks-plugin/data.json`:
  | Setting key | Vulcan mapping |
  |---|---|
  | `statusSettings.coreStatuses` | Core status type definitions (`[ ]`, `[x]`) |
  | `statusSettings.customStatuses` | Custom status type definitions (symbol → name → type → next) |
  | `globalFilter` | Global filter tag — only tasks matching this tag are considered by Tasks queries |
  | `globalQuery` | Default query prepended to all Tasks query blocks |
  | `removeGlobalFilter` | Whether to hide the global filter tag in rendered output |
  | `setCreatedDate` | Auto-set `➕ created` date on new tasks |
  | `recurrenceOnCompletion` | How recurring tasks create next instance on completion |
- [x] `vulcan config import tasks` — import Tasks settings and report mapping
- Refactor to implement `PluginImporter` trait when 9.17.1 lands

#### 9.10.6 CLI surface and evaluation

The Tasks plugin query commands are part of the unified `vulcan tasks` CLI (see 9.15.9). The Tasks DSL parser and evaluator are the implementation; the CLI surface is shared.

- [x] Tasks DSL query evaluation engine (called by `vulcan tasks query`)
- [x] Tasks block evaluation engine (called by `vulcan tasks eval`)
- [x] Inline task listing with filter support (called by `vulcan tasks list --source inline`)
- [x] `--output json` support
- [x] Integration tests: Tasks DSL queries against test vault with known results

### 9.11 Kanban board support

**Goal:** Parse and query Obsidian Kanban plugin boards (`.md` files with column-as-heading structure), expose board state via CLI, and support board manipulation.

**Builds on:** Phase 9.8.2 (list item extraction), Phase 7.1 (metadata refactors).
**Reference material:** `references/obsidian-kanban/` (Kanban plugin source)

#### 9.11.1 Kanban board parsing

- [x] Detect Kanban board files: presence of `kanban-plugin` key in frontmatter or footer settings code block/comment
- [x] Parse board structure: headings → columns, list items under headings → cards
- [x] Extract card metadata: checkbox status, inline dates, tags, links, inline fields
- [x] Parse board configuration from footer settings code block/comment (if present): column settings, archive column, completed column
- [x] Configurable date and time triggers: parse date/time from card text using configurable trigger tokens (not hardcoded emoji — Kanban plugin allows `{date-trigger}` and `{time-trigger}` config, defaults `📅` and `⏰` but can be any string)
- [x] Linked page metadata: cards that are `[[wikilinks]]` inherit metadata from the linked note (frontmatter, tags, inline fields) — enables filtering/sorting cards by linked note properties
- [x] Store board structure in cache: `kanban_boards` table (or extend existing tables with board context)
- [x] Index on board → column → card hierarchy

#### 9.11.2 Archive support

- [x] Parse archive column: Kanban plugin supports a dedicated archive section (heading `## Archive` or configured via `archive-with-date` setting)
- [x] `vulcan kanban archive <board> <card>` — move a card to the archive column
- [x] Archive-with-date: optionally prepend archive date to card text (configurable via `archive-with-date` setting)
- [x] `vulcan kanban show <board> --include-archive` — include archived cards in output (excluded by default)

#### 9.11.3 CLI surface

- [x] `vulcan kanban list` — list all Kanban boards in the vault
- [x] `vulcan kanban show <board>` — display board state (columns and card counts; `--verbose` shows all cards)
- [x] `vulcan kanban cards <board> [--column <name>] [--status <status>]` — list cards with optional filters
- [x] `vulcan kanban move <board> <card> <target-column>` — move a card between columns (rewrite the `.md` file)
- [x] `vulcan kanban add <board> <column> <text>` — add a new card to a column
- [x] `--output json` on all subcommands

#### 9.11.4 Settings import

- [x] Read Kanban settings from `.obsidian/plugins/obsidian-kanban/data.json` — 39+ config keys including:
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
- [x] `vulcan config import kanban` — import Kanban settings and report mapping
- Refactor to implement `PluginImporter` trait when 9.17.1 lands
- [x] Per-board settings override: individual boards can override global settings via their YAML code block

#### 9.11.5 TUI and WebUI (future)

- [x] Browse TUI: `o` hotkey on Kanban `.md` files opens a board view with columns displayed side-by-side
- [-] WebUI: Kanban board rendered as interactive drag-and-drop columns (moved to Phase 13.3 Vault browser / WebUI work)

### 9.12 External agent integration

**Status:** Completed as an external-runtime integration layer. Vulcan remains the source of truth for vault semantics, tools, prompts, and skills; an external runtime owns inference, session state, and chat UX.

**Goal:** Make Vulcan feel native inside external agent runtimes. The model should read vault `AGENTS.md`, discover commands through `describe` and `help`, load `.agents/skills/*/SKILL.md` on demand, and perform all vault reads and writes through Vulcan's JSON CLI instead of direct filesystem edits.

**Builds on:** Phase 5 (vectors/embeddings for semantic search), Phase 7.12 (query model), Phase 9.6 (search), Phase 9.18.2 (note CRUD), Phase 9.18.6 (web tools), Phase 9.18.7 (help/describe polish), Phase 9.18.8 (git ops).

#### 9.12.1 External runtime contract

- [x] Define the integration contract in `docs/assistant/pi_integration.md`
- [x] Document a reference runtime adapter that shells out to `vulcan` in `--output json` mode; no direct SQLite access, parser duplication, or note mutation outside Vulcan
- [x] Startup flow: locate vault root, load `AGENTS.md`, enumerate bundled/user skills, and call `vulcan describe --format openai-tools`
- [x] Tool registration modes:
  - static wrappers for the core note/search/query/property/inbox tools
  - dynamic discovery for the rest of the command surface via `help --output json`
- [x] Normalize stdout/stderr parsing, exit-code handling, and timeout errors so external runtimes see stable tool failures
- [x] Support both read-only and write-enabled profiles
- [x] External-runtime launch contract includes `--permissions <profile>` on every `vulcan` invocation, with `agent` as the default write-capable profile and `readonly` as the default browse-only profile

#### 9.12.2 Tool boundary and trust model

- [x] Default recommendation: run external runtimes without generic file-edit and shell-write tools for vault operations; all vault mutations should go through Vulcan commands
- [x] All note mutations flow through `vulcan note *`, `update`, `unset`, `inbox`, and `refactor *`
- [x] All vault reads flow through `note get`, `search`, `query`, graph tools, daily tools, git tools, and web tools as appropriate
- [x] Preserve CLI-to-tool 1:1 mapping; runtime adapters must not invent a second vault API
- [x] Document how `--dry-run`, `--check`, and git auto-commit fit into the agent workflow
- [x] Document a recommended least-privilege profile for read-only browsing, note editing, and high-trust refactoring
- [x] Tool wrappers and any future native assistant dispatch must treat Vulcan permission profiles as the authorization boundary: select a profile per session/tool call, pass it through unchanged, and rely on Vulcan-side denials instead of reimplementing policy in the runtime

#### 9.12.3 Prompts, skills, and vault context

- [x] Treat vault `AGENTS.md`, the configured prompts folder, and `.agents/skills/*/SKILL.md` as the primary durable prompt surface
- [x] Keep bundled default skills written by `vulcan init --agent-files` or `vulcan agent install`; user-defined skills remain plain vault files
- [x] Runtime integrations inject only a compact tool summary up front; detailed schemas and skill content stay on-demand through `describe`, `help`, and skill files
- [x] Publish a runtime-integration usage guide with recommended permission profiles and common pitfalls
- [x] Optional follow-up wrapper command: `vulcan agent print-config --runtime <name>` or similar to emit ready-to-paste setup snippets once the contract is stable

#### 9.12.4 Sessions and persistence boundary

- [x] The external runtime owns live chat/session state, compaction, and transcript storage by default
- [x] Vulcan does not initially implement gemini-scribe conversation files, assistant-specific memory notes, or a built-in `vulcan assistant --chat` runtime
- [x] Durable artifacts that matter to the user should be written as normal vault notes through the existing tool surface
- [x] Revisit session export/import only if external runtime session models prove insufficient for vault workflows

#### 9.12.5 Exit criteria and revisit triggers

- [x] Daily-driver workflows succeed in at least one external runtime without direct file editing: read note, patch note, search/query vault, run refactors, inspect git state, and consult skills
- [x] Reassess a native embedded runtime only if one of these remains unsolved:
  - vault-native session transcripts become essential
  - confirmation and permission UX must be enforced inside Vulcan itself
  - mobile/chat transports need tight in-process control
  - external runtimes cannot express the required tool discovery or sandboxing model

Preserved native-runtime steering lives in `docs/assistant/native_runtime_deferred.md`. That document is deferred reference material, not the current critical path.

#### 9.12.6 Prompts and skills remain vault-native

Prompts and skills stay as Markdown files in the vault. External runtimes consume them as reference material, and MCP exposes the same prompt files through protocol-native prompt discovery.

- [x] Configurable prompts folder: `assistant.prompts_folder` in `.vulcan/config.toml` (default: `AI/Prompts/`)
- [x] Configurable skills folder: `assistant.skills_folder` in `.vulcan/config.toml` (default: `.agents/skills/`)
- [x] Shared prompt loader/discovery API in Vulcan: enumerate prompt files from `assistant.prompts_folder`, parse metadata, and load/render prompt bodies for reuse by external-runtime helpers and MCP `prompts/*`
- [x] `vulcan init --agent-files` / `vulcan agent install` should be able to scaffold example prompt files into the configured prompts folder without making them special runtime-only assets
- [x] Prompt file format — Markdown with YAML frontmatter:
  ```yaml
  ---
  name: summarize-meeting
  description: Summarize meeting notes into action items
  version: 1
  tags:
    - productivity
    - meetings
  ---

  You are a meeting notes assistant. Given a meeting note, extract:
  1. Key decisions made
  2. Action items with owners
  3. Follow-up questions
  ```
- [x] Skill file format — one directory per skill under `.agents/skills/<name>/SKILL.md`, with Markdown plus YAML frontmatter:
  ```text
  .agents/skills/daily-review/SKILL.md
  ```
  ```yaml
  ---
  name: daily-review
  description: Review today's notes and create a daily summary
  tools:
    - search
    - note_get
    - note_create
    - query
    - daily_list
  output_file: "Reviews/{{date}}-daily-review.md"
  ---

  ## When to use
  Use this skill to review and summarize the day's work...
  ```
- [x] `skill_list()` and `skill_get(name)` remain part of the discoverable tool surface for external runtimes

**Default skills (shipped with Vulcan):**

Vulcan ships a standard library of skills that teach any external runtime how to use the tool surface effectively. These are bundled in the binary (via `include_str!`) and written to the vault on `vulcan init`.

- [x] **note-operations** — reading, creating, editing notes. Covers `note outline`, semantic `note get` selectors (section, heading, block-ref, lines, match), `note append` under headings, `note patch` find/replace safety (fails on multiple matches), frontmatter conventions. Common mistake: using `note set` when `note patch` or `note append` is safer.
- [x] **vault-query** — query DSL usage, filter expressions, property operators, sorting, `search` vs `query` guidance (search for content, query for metadata). Common mistake: using search when a property query is more precise.
- [x] **js-api-guide** — vault JS API patterns. `vault.note()`, `vault.notes().where().sortBy()`, `vault.query()`, `vault.graph`, `vault.transaction()` for atomic batch mutations. Examples for common operations: bulk property updates, cross-note analysis, generating summary tables.
- [x] **skill-creator** — creating and reviewing Agent Skills-compatible skills for Vulcan vaults, including `metadata.vulcan.commands`, direct script shebangs, `main(input, ctx)`, schemas, sandboxing, permission profiles, and `vulcan skill validate`.
- [x] **graph-exploration** — links, backlinks, shortest paths, hubs, dead ends, connected components. When to use graph traversal vs search. Common mistake: traversing large graphs without limiting depth.
- [x] **link-curation** — finding weak links, ambiguous links, orphan notes, missing backlinks, and notes that need aliases or tags. Common mistake: bulk rewriting links before reviewing suggested targets.
- [x] **daily-notes** — periodic note workflow: appending entries, reviewing date ranges, event syntax (`- [time] title [@key(value)] [#tag]`), querying events. Common mistake: creating duplicate daily notes instead of appending.
- [x] **properties-and-tags** — metadata management with `update_property`/`unset_property`. Property types, tag conventions, querying by metadata via `query where`. Common mistake: setting properties on the wrong note when names are ambiguous.
- [x] **refactoring** — rename aliases/headings/properties, merge tags, rewrite content, move notes. Always `--dry-run` first. Safety patterns for bulk operations. Common mistake: not checking backlinks before renaming.
- [x] **web-research** — `web search` for finding information, `web fetch` for extracting article content. Combining web content with vault notes. Output modes (markdown vs raw).
- [x] **git-workflow** — checking changes with `git status`/`git diff`, committing with descriptive messages, reviewing history with `git log`/`git blame`. Auto-commit behavior and `--no-commit` flag.
- [x] **task-management** — task syntax in notes, querying tasks by status/priority/due date, creating and completing tasks. Task dependencies and recurring tasks.
- [x] **configuration-and-permissions** — config inspection, profiles, access control, sandbox tiers, trust decisions, and denied-tool diagnosis.
- [x] **mcp-setup** — MCP stdio/HTTP setup, ChatGPT remote connector setup, OAuth/IndieAuth flow debugging, tool packs, permission profiles, and resource visibility.
- [x] **index-maintenance** — scan/reindex/cache maintenance, stale search repair, vector index checks, and derived-state diagnostics.
- [x] **dataview-and-bases** — Dataview DQL, inline fields, DataviewJS, `.base` files, formulas, saved views, and compatibility troubleshooting.
- [x] **templates-and-capture** — templates, inbox capture, Templater tags, QuickAdd-style capture flows, and note scaffolding.
- [x] **publishing-and-export** — static site builds, rendered exports, package formats, route/link policy, and publish diagnostics.
- [x] **plugin-authoring** — JavaScript lifecycle plugins, event hooks, trust and permissions, and deciding between plugins and skill commands.
- [x] **diagnostics-and-repair** — health checks, parser diagnostics, broken links, cache verification, and safe repair planning.
- [x] **conversation-export** — converting external assistant conversations into vault notes using a stable Markdown export format.

**User-defined skills:**

User skills live in the vault's skills folder (e.g., `.agents/skills/weekly-review/SKILL.md`, `.agents/skills/session-prep/SKILL.md`) and appear alongside defaults in `skill_list`. A GM might create a "session-prep" skill that pulls NPCs, locations, and plot threads for an RPG campaign. A researcher might create a "literature-review" skill that searches for related notes and generates a synthesis.

**Executable skill scripts:**

Advanced skills may include JavaScript scripts that expose functionality beyond what Markdown skill files can express — complex data transformations, API integrations, or multi-step vault operations. These scripts use the full vault JS API (see 9.18.5) and can be made directly executable with a shebang:

```bash
#!/usr/bin/env -S vulcan skill exec
// .agents/skills/session-prep/prepare.js
const npcs = vault.notes().where(n => n.tags.includes("npc") && n.frontmatter.campaign === "current");
const locations = vault.query("from notes where type = location and status = active");
console.log(JSON.stringify({ npcs: npcs.map(n => n.name), locations: locations.map(n => n.name) }));
```

This makes skill command scripts runnable by external agent harnesses (Claude Code, Codex, Gemini CLI) as plain executables — the harness does not need to know about Vulcan's JS runtime. Ad hoc user scripts in `.vulcan/scripts/` can still use `#!/usr/bin/env -S vulcan run --script`.

If a reusable script should become a typed direct-call tool across CLI, MCP, assistant integrations, and the internal JS API rather than remain a harness-local helper, declare it as an exposed skill command in `SKILL.md`.

#### 9.12.8 Deferred native chat integrations

**Status:** `[-]` Deferred under the `pi`-first strategy. Native Telegram/Signal/Matrix/Discord adapters are no longer on the immediate critical path.

**Rationale:** External runtimes already provide the terminal/chat loop, session management, and model integration. Vulcan should only absorb that complexity later if external runtimes cannot satisfy the workflow.

Detailed native assistant and chat-runtime ideas from the previous roadmap are preserved in `docs/assistant/native_runtime_deferred.md`.

- [-] Do not implement `vulcan assistant serve` or in-process chat adapters in the current Phase 9 plan
- [-] Revisit only after Phase 9.19.13 (permissions) and Phase 10 (daemon) are mature enough to support a safe long-lived service model
- [-] If revived, define a new post-MCP/daemon roadmap item rather than expanding Phase 9.12 itself
- [-] If revived later, keep the same rule: memory and durable artifacts live in the vault, and all mutations still go through the normal Vulcan command surface

#### 9.12.9 Agent asset import

- [x] No direct plugin equivalent to import — this is Vulcan-native scaffolding for external runtimes
- [x] Migration helper: if `AGENTS.md`, prompt files, or skill-like files are detected in common locations for external harnesses, offer to import or symlink them into Vulcan's configured folders
- [x] Do not import session histories by default; session storage belongs to the external runtime unless explicitly exported as vault notes

### 9.13 QuickAdd compatibility

**Goal:** Obsidian-compatible support for QuickAdd's capture and format syntax. QuickAdd chains multiple operations (template creation, content capture, Obsidian commands, user scripts) into single-trigger actions. Vulcan focuses on the data-format and settings-import side for vault compatibility; the macro/scripting side is handled by the JS runtime (9.18.5) and existing CLI commands.

**Status:** Scoped to capture format compatibility and settings import. QuickAdd's macro chains and user scripts map naturally to Vulcan's JS runtime (`vulcan run`) and shell scripts — no separate macro DSL needed.

**Reference material:** `references/quickadd/` (QuickAdd source), [QuickAdd documentation](https://quickadd.obsidian.guide/docs/)

#### 9.13.1 Capture format compatibility

QuickAdd's capture and template features use a format syntax for variable expansion. Support this syntax in `note append` and template contexts for vault compatibility:

- [x] QuickAdd format syntax support: `{{DATE}}`, `{{DATE:format}}`, `{{TIME}}`, `{{TIME:format}}`, `{{VDATE:format, offset}}` — reuse 9.7.1 Moment.js-compatible date formatting
- [x] `{{VALUE}}` — prompt for user input (CLI: read from stdin or `--var` flag; maps to existing `tp.system.prompt` infrastructure from 9.9.2)
- [x] `{{FILE_NAME}}`, `{{FILE_PATH}}`, `{{TITLE}}` — file context variables (already available in template engine)
- [x] `{{LINKCURRENT}}` — wikilink to the current file (when applicable)
- [x] Capture position support in `note append`: `--prepend` / `--append` / `--after-heading <heading>` (extends 9.18.2 `note append`)
- [x] Capture to daily/weekly/monthly note with auto-creation (delegates to 9.16 periodic note infrastructure)

**Not in scope:** `{{MACRO:<name>}}` (use JS runtime), `{{SELECTED}}` (editor-only), `EditorCommand` (UI-only), `Wait` (use shell), `NestedChoice` (use JS runtime). These QuickAdd features are inherently UI-driven or map directly to existing Vulcan infrastructure.

#### 9.13.2 Settings import

- [x] Read QuickAdd settings from `.obsidian/plugins/quickadd/data.json`:
  | Setting key | Vulcan mapping |
  |---|---|
  | `choices` | Array of choice definitions — import Template and Capture choices as note templates / capture configs; report Macro and Multi choices as requiring manual conversion to JS scripts |
  | `templateFolderPath` | Template discovery path (cross-reference with Templater settings) |
  | `globalVariables` | Global variable definitions for format syntax expansion |
  | `ai` | AI provider config (model, API key env, system prompt) — cross-reference with 9.12 external agent integration docs |
- [x] `vulcan config import quickadd` — import QuickAdd settings, convert capture/template choices, report unmappable choices with migration guidance (implement as `PluginImporter` per 9.17.1)

### 9.14 Native capability and plugin-adapter notes

Common Obsidian plugins are evidence for useful workflows and persisted formats, not a requirement to reproduce their product boundaries. Vulcan should expose native capability names and models, then add explicit settings, syntax, or conformance adapters where interoperability is valuable:

**Excalidraw:** Drawings stored as `.excalidraw.md` (Markdown with LZ-String compressed JSON in code blocks) or `.excalidraw` (plain JSON). Parsing, indexing, and WebUI rendering/editing are covered in **Phase 18.8 (Excalidraw support)** as a sub-phase of Canvas support.

**Advanced Tables:** Primarily a UI feature for editing Markdown tables. No data format needs a plugin-specific parser: Vulcan's existing Markdown model handles standard tables, while native WebUI table editing (tab navigation, column add/remove, sorting, alignment, CSV paste) is covered in **Phase 14.1 (Note editor → Advanced table editing)**.

**Calendar:** The plugin demonstrates a calendar view linked to daily notes. Vulcan's native periodic-note and event model supplies the data; the browse TUI (9.2) and WebUI (Phase 13) may present calendar navigation without depending on Calendar settings or terminology. The DQL CALENDAR query type (9.8.6) remains a compatibility view over that shared data.

**Obsidian Git:** Git-based vault synchronization and versioning. Vulcan already has git integration (9.3 auto-commit, `diff` command, browse TUI git log). No additional compatibility needed.

The same rule applies to completed work: canonical queries own the semantics underneath Dataview and Bases adapters; the unified task model owns recurrence and dependency semantics underneath Tasks and TaskNotes formats; native templates/capture/automation own workflows underneath Templates, Templater, and QuickAdd syntax; and the board model owns behavior underneath Kanban files. Preserve working compatibility contracts, but put new generally useful behavior into the native layer first.

### 9.15 TaskNotes compatibility (primary task model)

**Goal:** Full compatibility with the TaskNotes plugin — tasks stored as individual Markdown files with rich YAML frontmatter, powered by Obsidian Bases views. TaskNotes is Vulcan's **primary task management model**: tasks as first-class vault notes with structured metadata, rather than inline checkboxes scattered across files. Vulcan should parse, index, query, create, and manage TaskNotes task files, register custom Bases view types, and support the full TaskNotes configuration surface.

**Relationship to 9.10 (Tasks plugin):** The Tasks plugin (9.10) provides the parsing and query layer for inline checkbox tasks and `` ```tasks `` query blocks — important for vault compatibility. TaskNotes (9.15) is the recommended workflow for task management in Vulcan. Shared infrastructure (recurring tasks via RRULE, task dependencies, custom statuses) is implemented in 9.10 and reused here. The CLI surface is unified under `vulcan tasks` (see 9.15.9) — both inline and file-based tasks are queryable through the same commands.

**Builds on:** Phase 4 (properties/Bases), Phase 9.8 (Dataview metadata), Phase 9.10 (shared task infrastructure — recurring tasks, dependencies, custom statuses).
**Reference material:** `references/tasknotes/` (TaskNotes source), requires Obsidian 1.10.1+ for public Bases API.

#### 9.15.1 Task file format and parsing

- [x] Detect TaskNotes task files: configurable identification method — by tag (default: `task` tag in frontmatter) or by property presence (e.g., `status` + `priority` fields)
- [x] Configurable tasks folder: default `TaskNotes/Tasks/`, configurable via settings import
- [x] Parse task frontmatter properties:
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
- [x] Field mapping support: TaskNotes allows users to remap internal field names to custom frontmatter keys — honor `fieldMapping` configuration
- [x] Custom user-defined fields: arbitrary additional frontmatter properties with typed schemas (text, number, date, boolean, list)
- [x] Store parsed task data in cache: extend `documents` metadata or add `tasknotes_tasks` table with indexed columns for status, priority, due, scheduled, project, context
- [x] Excluded folders: respect `excludedFolders` setting to skip non-task files in task folders

#### 9.15.2 Custom statuses and priorities

Reuses the status type registry from 9.10.4 (which defines `TODO`, `DONE`, `IN_PROGRESS`, `CANCELLED`, `NON_TASK` categories for inline checkbox tasks). TaskNotes extends this with richer status metadata and adds priority definitions. Both Obsidian Tasks and TaskNotes status systems coexist — the status registry maps between checkbox characters (Tasks plugin) and frontmatter strings (TaskNotes) so queries work across both task types.

- [x] Custom status definitions: each status has `id`, `value` (frontmatter string), `label` (display name), `color`, `isCompleted` (boolean), `autoArchive` (delay config)
  - Default statuses: `todo`, `in-progress`, `done`, `cancelled`
  - Users can add unlimited custom statuses with configurable completion semantics
  - Map TaskNotes statuses to 9.10.4 status type categories (`isCompleted: true` → `DONE`, etc.) so unified queries work
- [x] Custom priority definitions: each priority has `id`, `value`, `label`, `color`, `weight` (numeric for sorting/scoring)
  - Default priorities: `highest`, `high`, `medium`, `low`, `lowest`
  - Map to Tasks plugin emoji priorities (⏫/🔺/🔼/🔽/⏬) for cross-format queries
- [x] Status and priority are first-class query dimensions: filterable, sortable, groupable in DQL, Tasks DSL, and Bases views
- [x] Auto-archive: when a task enters a completed status, optionally archive after a configurable delay

#### 9.15.3 Natural language task creation

- [x] NLP parser for task input strings: extract structured properties from natural language
  - Example: `"Buy groceries tomorrow at 3pm @home #errands high priority"` → `{ title: "Buy groceries", due: "2026-03-28T15:00", contexts: ["@home"], tags: ["errands"], priority: "high" }`
- [x] Configurable NLP trigger characters:
  | Trigger | Default | Extracts |
  |---|---|---|
  | `@` | contexts | `@home`, `@office` |
  | `#` | tags | `#errands`, `#work` |
  | `+` | projects | `+[[Project Name]]` |
  | `*` | status | `*done`, `*in-progress` |
- [x] Date extraction: "tomorrow", "next Monday", "in 3 days", "January 15th" — reuse chrono-like date parsing
- [x] Priority extraction: "high priority", "urgent", "low priority" — configurable keyword mapping
- [x] `vulcan tasks add "natural language input"` — create task file from NLP-parsed input
- [x] `--no-nlp` flag to create task with raw title (skip NLP parsing)
- [x] Configurable NLP language (default: English, supports multiple languages)

#### 9.15.4 Recurring tasks (RRULE)

Reuses the RRULE parsing and recurrence expansion infrastructure from 9.10.2. TaskNotes adds per-instance completion/skipping semantics on top.

- [x] Parse `recurrence` field as RFC 5545 RRULE string (e.g., `FREQ=WEEKLY;BYDAY=MO,WE,FR`) — reuse 9.10.2 RRULE parser
- [x] Recurrence expansion: compute next N occurrences for query and calendar display — reuse 9.10.2 expansion engine
- [x] Per-instance completion: `complete_instances` tracks which occurrences are done without completing the entire recurring task (TaskNotes-specific)
- [x] Per-instance skipping: `skipped_instances` marks occurrences as intentionally skipped (TaskNotes-specific)
- [x] Flexible vs fixed scheduling: next instance calculated from completion date (flexible) or from original schedule (fixed) — configurable via `recurrenceAnchor`

#### 9.15.5 Task dependencies

Reuses the dependency graph infrastructure from 9.10.3 (which handles inline emoji dependencies: `🆔`/`⛔`). TaskNotes extends the graph with richer RFC 9253 relation types and duration gaps. Both dependency formats feed into the same graph — `vulcan tasks blocked` and `vulcan tasks graph` show a unified view across inline and file-based tasks.

- [x] Parse `blockedBy` array: each entry has `uid` (wikilink to blocking task), `reltype`, and optional `gap` (ISO 8601 duration)
- [x] Dependency relation types (RFC 9253) — extends 9.10.3's simple blocked-by with:
  | Type | Meaning |
  |---|---|
  | `FINISHTOSTART` | Blocked task can start after blocker finishes (default, same as 9.10.3 `⛔`) |
  | `FINISHTOFINISH` | Blocked task can finish after blocker finishes |
  | `STARTTOSTART` | Blocked task can start after blocker starts |
  | `STARTTOFINISH` | Blocked task can finish after blocker starts |
- [x] Duration gaps: `gap: P1D` means "1 day after the blocker completes"
- [x] Feed TaskNotes dependencies into the shared dependency graph (9.10.3) so both emoji-based and frontmatter-based dependencies are queryable together

#### 9.15.6 Time tracking and pomodoro

Core time tracking and a simple CLI pomodoro timer. GUI (progress bars, visual timers, notifications) deferred to post-WebUI. See [Deferred enhancements — Time tracking GUI](#deferred-time-tracking-gui).

- [x] Parse `timeEntries` array: each entry has `startTime`, `endTime`, `description`
- [x] `vulcan tasks track start <task>` — start a time tracking session (append to `timeEntries` with open `endTime`)
- [x] `vulcan tasks track stop [task]` — stop the active session (set `endTime`)
- [x] `vulcan tasks track status` — show currently active tracking session
- [x] `vulcan tasks track log <task>` — show time entries for a task
- [x] `vulcan tasks track summary [--period day|week|month]` — aggregate time spent across tasks
- [x] Pomodoro timer (CLI):
  - [x] `vulcan tasks pomodoro start <task>` — start a pomodoro work session
  - [x] Configurable durations: `pomodoro.work_duration` (default 25min), `pomodoro.short_break` (5min), `pomodoro.long_break` (15min), `pomodoro.long_break_interval` (every 4 pomodoros)
  - [x] Pomodoro session history stored in task frontmatter (`pomodoros` array) or daily note (configurable)
- [x] `timeEstimate` field: compare estimated vs actual time in reports

#### 9.15.7 Reminders

Core reminder data model and query support. Reminder *delivery* (desktop notifications, Telegram messages, etc.) is deferred — see [Deferred enhancements — Reminder delivery channels](#deferred-reminder-delivery).

- [x] Parse `reminders` array: each reminder has `id`, `type` (relative/absolute), `relatedTo` (due/scheduled), `offset` (ISO 8601 duration, e.g., `-PT15M`), `description`
- [x] `vulcan tasks reminders [--upcoming <duration>]` — list upcoming reminders within a time window
- [x] `vulcan tasks due [--within <duration>]` — show tasks due within a time window
- [x] Reminder evaluation engine: given current time, resolve which reminders are active/overdue (reusable by future delivery integrations)

#### 9.15.8 Bases view integration

TaskNotes v4+ is built entirely on Obsidian Bases. Vulcan should register equivalent custom Bases view types:

- [x] Register custom Bases source type: `tasknotes` with config subtypes:
  | View type | Description |
  |---|---|
  | `tasknotesTaskList` | Filterable, sortable, groupable task table |
  | `tasknotesKanban` | Kanban board (columns = status or custom field) |
- [-] Calendar Bases views (`tasknotesCalendar`, `tasknotesMiniCalendar`) deferred to post-WebUI — calendar rendering is a visual concern. See [Deferred enhancements — Calendar Bases views](#deferred-calendar-bases-views).
- [x] Parse `.base` view files in `TaskNotes/Views/` (YAML format):
  - Filter conditions: grouped AND/OR tree of property-based conditions
  - Sort key and direction
  - Group key and optional sub-group key
  - Formula definitions for computed columns
- [x] Built-in formula support for TaskNotes views:
  | Formula | Expression |
  |---|---|
  | `daysUntilDue` | `if(due, ((number(date(due)) - number(today())) / 86400000).floor(), null)` |
  | `isOverdue` | `due && date(due) < today() && status != "done"` |
  | `urgencyScore` | `formula.priorityWeight + max(0, 10 - formula.daysUntilDue)` |
  | `efficiencyRatio` | `if(timeEstimate > 0, totalTimeSpent / timeEstimate, null)` |
- [x] `vulcan tasks view <name>` — evaluate a saved Bases view from the command line
- [x] `vulcan tasks view list` — list available TaskNotes views
- [x] `--output json|table` on view evaluation (structured JSON or default human table output)
- [x] Saved filter views: support `savedViews` config (named filter+sort+group presets) as CLI aliases

#### 9.15.9 Unified CLI surface (`vulcan tasks`)

The `vulcan tasks` command group is the unified interface for all task operations — both TaskNotes file-based tasks and inline checkbox tasks (9.10). TaskNotes operations are the default; inline task queries are available via `--source inline` or the Tasks DSL subcommands from 9.10.6.

**Task file management (TaskNotes):**

- [x] `vulcan tasks add <title-or-nlp-string>` — create a new TaskNotes task file
  - [x] `--status`, `--priority`, `--due`, `--scheduled`, `--context`, `--project`, `--tag` flags for explicit property setting
  - [x] `--template <name>` — create from a task template
- [x] `vulcan tasks show <task>` — display task details (all properties, time entries, dependencies)
- [x] `vulcan tasks edit <task>` — open task file in `$EDITOR`
- [x] `vulcan tasks set <task> <property> <value>` — update a task property
- [x] `vulcan tasks complete <task>` — mark task as completed (set status to done, record `completedDate`); works for both file-based and inline tasks
- [x] `vulcan tasks archive <task>` — archive a completed task (TaskNotes only)
- [x] `vulcan tasks convert <file> [--line <n>]` — convert a line, checkbox, or heading in an existing note into a TaskNotes task file

**Unified query (both task types):**

- [x] `vulcan tasks list [--filter <expr>]` — list tasks with optional filter expression; queries both TaskNotes files and inline tasks by default
  - [x] `--source file|inline|all` — filter by task type (default: `all`)
  - [x] `--status <s>`, `--priority <p>`, `--due-before <date>`, `--due-after <date>`, `--project <p>`, `--context <c>` — shorthand filters
  - [x] `--group-by <field>`, `--sort-by <field>` — grouping and sorting
  - [x] `--include-archived` — include archived tasks (excluded by default)
- [x] `vulcan tasks next <n>` — show next N upcoming task instances across all recurring tasks (both types)
- [x] `vulcan tasks blocked` — list all blocked tasks with their blocking dependencies (both types)
- [x] `vulcan tasks graph` — visualize task dependency graph (both types)

**Tasks plugin DSL (9.10 compatibility):**

- [x] `vulcan tasks query <query-string>` — evaluate a Tasks DSL query (from 9.10.1)
- [x] `vulcan tasks eval <file> [--block <n>]` — evaluate a `` ```tasks `` block from a note (from 9.10.6)

**Task mutations (from 9.18.9):**

- [x] `vulcan tasks create <text> [--in <note>] [--due <date>] [--priority <p>]` — create an inline task in a note (as opposed to `tasks add` which creates a TaskNotes file)
- [x] `vulcan tasks reschedule <task-id> --due <date>` — change task due date (both types)

**Shared:**

- [x] `--output json` on all subcommands

#### ~~9.15.10 Calendar sync~~ — deferred

**Status:** Closed for Phase 9. This struck section has no remaining Phase 9 implementation items; the calendar work is parked as post-Phase 9 research in [Deferred enhancements — Calendar integration](#deferred-calendar-integration).

Deferred to post-Phase 9 enhancements. Calendar integration needs deeper research into how the vault and assistant integrate with calendars holistically (not just TaskNotes). See [Deferred enhancements — Calendar integration](#deferred-calendar-integration).

#### 9.15.11 Settings import

- [x] Read TaskNotes settings from `.obsidian/plugins/tasknotes/data.json` — import settings for implemented features:
  | Setting category | Key settings |
  |---|---|
  | **Core** | `tasksFolder`, `archiveFolder`, `taskTag`, `taskIdentificationMethod`, `taskPropertyName`, `taskPropertyValue`, `excludedFolders`, `defaultTaskPriority`, `defaultTaskStatus` |
  | **Field mapping** | `fieldMapping` — implemented property remapping surface for indexed task fields |
  | **Custom types** | `customStatuses` (id, value, label, color, isCompleted, autoArchive), `customPriorities` (id, value, label, color, weight) |
  | **User fields** | `userFields` — custom field definitions (id, displayName, key, type) |
  | **NLP** | `enableNaturalLanguageInput`, `nlpLanguage`, `nlpDefaultToScheduled`, `nlpTriggers` (trigger chars → property mapping) |
  | **Pomodoro** | `pomodoroWorkDuration`, `pomodoroShortBreakDuration`, `pomodoroLongBreakDuration`, `pomodoroLongBreakInterval`, `pomodoroStorageLocation` |
  | **Bases** | `enableBases`, `autoCreateDefaultBasesFiles`, `commandFileMapping` |
  | **Saved views** | `savedViews` — named filter/sort/group presets |
  | **Task defaults** | `taskCreationDefaults` (defaultContexts, defaultTags, defaultProjects, defaultDueDate, defaultTimeEstimate, defaultReminders) |
- [x] Skipped during import (deferred features): Calendar view settings, ICS integration, Google Calendar, Microsoft Calendar, API/webhook settings, UI/editor settings. Report these as "skipped (feature not yet supported)" in the import summary.
- [x] `vulcan config import tasknotes` — import TaskNotes settings, create Vulcan-native config, report mapping (implement as `PluginImporter` per 9.17.1)
- [x] Migrate `.base` view files: copy TaskNotes view definitions and validate they work with Vulcan's Bases evaluator

#### ~~9.15.12 HTTP API compatibility~~ — deferred

**Status:** Closed for Phase 9. Do not implement TaskNotes plugin API compatibility as a Phase 9 item; task HTTP work belongs to the unified Phase 10 daemon API tracked in [Deferred enhancements — Task daemon API](#deferred-task-daemon-api).

Deferred. The Phase 10 daemon will expose task functionality through its own unified API design rather than replicating the TaskNotes plugin's REST endpoints. See [Deferred enhancements — Task daemon API](#deferred-task-daemon-api).

### 9.16 Periodic notes (daily, weekly, monthly)

**Goal:** First-class support for periodic notes — daily notes, weekly notes, monthly notes, and custom periodic patterns. Multiple Phase 9 plugins depend on periodic note discovery and creation (TaskNotes pomodoro storage in daily notes, Kanban date-to-daily-note linking, QuickAdd capture to daily note, Calendar plugin integration, Dataview `file.day` resolution). This phase provides the shared infrastructure.

**Builds on:** Phase 1 (document indexing), Phase 9.7 (template variables for date formatting).
**Reference material:** [Obsidian Daily Notes core plugin](https://help.obsidian.md/Plugins/Daily+notes), [Obsidian Periodic Notes community plugin](https://github.com/liamcain/obsidian-periodic-notes)

#### 9.16.1 Periodic note configuration

- [x] Configuration in `.vulcan/config.toml`:
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
- [x] Each period type is independently configurable: folder, filename format, template, enabled flag
- [x] Custom period types: allow user-defined periods beyond the built-in five via `[periodic.<name>]` with `unit = "days|weeks|months|quarters|years"`, `interval = <n>`, and optional `anchor_date = "YYYY-MM-DD"` for cycle alignment

#### 9.16.2 Periodic note discovery and resolution

- [x] `resolve_periodic_note(period, date) -> Option<Path>`: given a period type and date, compute the expected filename and check if it exists
- [x] `resolve_daily_note(date) -> Option<Path>`: convenience alias for daily resolution
- [x] Reverse resolution: given a note path, determine if it's a periodic note and extract the date (parse filename against configured format)
- [x] `file.day` integration (Dataview 9.8.3): use periodic note configuration to resolve `file.day` — a daily note for `2026-03-27` has `file.day = date("2026-03-27")`
- [x] Date-to-note linking: provide a lookup function for other phases (Kanban `link-date-to-daily-note`, TaskNotes calendar integration)
- [x] Index periodic notes in cache: add `periodic_type` and `periodic_date` columns to documents table (nullable, populated during scan for notes matching periodic patterns)

#### 9.16.3 Structured events in daily notes

Daily notes can contain structured event syntax under a configurable heading. Events are parsed during `scan` (similar to how tasks are extracted from markdown) and stored as structured `Event` records in the cache.

**Event syntax (list under configured heading):**

```markdown
## Schedule
- 09:00 Team standup
- 09:00-10:00 Team standup @location(Zoom)
- 14:00-15:30 Dentist #personal
- all-day Company offsite
```

Format: `- [time[-end]] title [@key(value)...] [#tag...]`
Lines under the schedule heading that don't match the time pattern are treated as regular list items (lenient parser).

**Configuration:**

```toml
[periodic.daily]
schedule_heading = "Schedule"   # heading to parse events from (optional)
```

**Cache schema:**

- [x] `events` table: `id`, `document_id`, `start_time` (TEXT, "HH:MM" or "all-day"), `end_time` (TEXT, nullable), `title`, `metadata` (JSON for @key(value) pairs), `tags` (JSON array), `byte_offset`
- [x] Index on `document_id` for per-note queries, index on `start_time` for range queries
- [x] Events extracted during scan via `extract_events(content, schedule_heading) -> Vec<Event>`

**Queryable via:**

- [x] `vulcan daily list` aggregates events across daily notes in a date range
- [x] JS runtime: `vault.daily.today().events`, `vault.events({ from, to })` (see 9.18.5)
- [x] One-way `.ics` export for daily-note events
- [-] Calendar UI rendering moved to 9.18.1 / Phase 13 as a presentation concern rather than periodic-note data infrastructure

#### 9.16.4 CLI surface

- [x] `vulcan daily today` — open or create today's daily note
  - [x] If note exists: open in `$EDITOR`
  - [x] If note doesn't exist: create from template, then open
  - [x] `--no-edit` flag: create only, don't open
- [x] `vulcan daily show [date]` — display a daily note's content (default: today)
- [x] `vulcan daily list [--from <date>] [--to <date>]` — list daily notes in range, with aggregated events (also `--week`, `--month` shorthand)
- [x] `vulcan daily export-ics [--from <date>] [--to <date>] [--week] [--month] [--path <file.ics>]` — export parsed daily-note events as an ICS calendar
- [x] `vulcan daily append <text> [--heading <name>] [--date <date>]` — append text to a daily note (default: today)
- [x] `vulcan weekly [date]`, `vulcan monthly [date]` — same pattern for other periods
- [x] `vulcan periodic <type> [date]` — generic command for any configured period type
- [x] `vulcan periodic list [--type daily|weekly|monthly|...]` — list periodic notes, optionally filtered by type
- [x] `vulcan periodic gaps [--type daily] [--from <date>] [--to <date>]` — show missing periodic notes in a date range (useful for identifying gaps in journaling)
- [x] `--output json` on all subcommands
- [x] Auto-commit if enabled

**Note:** The daily and periodic note commands already live under top-level `daily`, `weekly`, `monthly`, and `periodic` groups. Phase 9.18.1 extends the grouped command-tree cleanup across the broader CLI surface, including browse calendar mode and per-group dispatch modules.

#### 9.16.4 Settings import

- [x] Import from Obsidian Daily Notes core plugin: `.obsidian/daily-notes.json`
  | Setting key | Vulcan mapping |
  |---|---|
  | `folder` | `periodic.daily.folder` |
  | `format` | `periodic.daily.format` |
  | `template` | `periodic.daily.template` |
  | `autorun` | Informational (no CLI equivalent) |
- [x] Import from Periodic Notes community plugin: `.obsidian/plugins/periodic-notes/data.json`
  | Setting key | Vulcan mapping |
  |---|---|
  | `daily.enabled`, `daily.folder`, `daily.format`, `daily.templatePath` | `periodic.daily.*` |
  | `weekly.enabled`, `weekly.folder`, `weekly.format`, `weekly.templatePath` | `periodic.weekly.*` |
  | `monthly.enabled`, `monthly.folder`, `monthly.format`, `monthly.templatePath` | `periodic.monthly.*` |
  | `quarterly.enabled`, `quarterly.folder`, `quarterly.format`, `quarterly.templatePath` | `periodic.quarterly.*` |
  | `yearly.enabled`, `yearly.folder`, `yearly.format`, `yearly.templatePath` | `periodic.yearly.*` |
- [x] `vulcan config import periodic-notes` — import periodic notes settings (implement as `PluginImporter` per 9.17.1; covers both core Daily Notes and community Periodic Notes sources)

### 9.17 Unified settings import infrastructure

**Goal:** A shared migration framework that translates Obsidian core and supported plugin settings into native Vulcan capability configuration, with comprehensive CLI flags, mapping diagnostics, conflict detection, and batch import. Plugin names identify source adapters and provenance only; imported destination keys remain capability-oriented. The infrastructure (9.17.1–9.17.3) is implementable early — individual source importers in their respective phases plug into it.

**Depends on:** Phase 9.5 (config layering — already complete). Individual plugin importers depend on 9.17.1 for the shared trait.

#### 9.17.1 Importer trait and shared engine

Define a trait that all importers implement, replacing the current free-standing `import_*_plugin_config` functions:

- [x] `PluginImporter` trait in `vulcan-core::config`:
  - `fn name(&self) -> &str` — importer identifier (e.g., `"tasks"`, `"core"`, `"dataview"`)
  - `fn display_name(&self) -> &str` — human-readable name (e.g., `"Obsidian Tasks plugin"`)
  - `fn source_paths(&self, paths: &VaultPaths) -> Vec<PathBuf>` — files this importer reads from
  - `fn detect(&self, paths: &VaultPaths) -> bool` — whether the source is present and importable
  - `fn import(&self, paths: &VaultPaths, target: ImportTarget) -> Result<ConfigImportReport, ConfigImportError>` — perform the import
  - `fn dry_run(&self, paths: &VaultPaths) -> Result<ConfigImportReport, ConfigImportError>` — compute what would change without writing
- [x] `ImportTarget` enum: `Shared` (config.toml, default) | `Local` (config.local.toml)
- [x] Extend `ConfigImportReport` with `target_file: PathBuf` and `dry_run: bool`
- [x] Importer registry: `fn all_importers() -> Vec<Box<dyn PluginImporter>>` — returns all known importers in priority order
- [x] Extract shared TOML merge logic from the current duplicated `write_*_import()` functions into a single `merge_import_into_toml()` helper
- [x] Refactor existing `import_tasks_plugin_config`, `import_templater_plugin_config`, `import_kanban_plugin_config` to implement `PluginImporter`
- [x] Import idempotency: re-running any import updates existing config without duplicating entries (already the case — verify trait implementations preserve this)
- [x] Unit test: trait dispatch works for all existing importers
- [x] Unit test: `dry_run` returns accurate diff without writing files

#### 9.17.2 Shared CLI flags

Add shared flags to the import CLI surface, replacing the per-variant `no_commit` with shared arguments:

- [x] `--dry-run` flag on all import subcommands and on `vulcan config import` itself — calls `dry_run()` instead of `import()`, prints what would change without writing
- [x] `--target shared|local` flag (default: `shared`) — selects `config.toml` or `config.local.toml` as write target
- [x] `--no-commit` flag retained (suppress auto-commit for this invocation)
- [x] Global `--output json|human` already works — verify all import report rendering respects it
- [x] Extract shared CLI dispatch handler: the current three near-identical match arms in `lib.rs` become a single `run_import(importer, flags, paths)` function that handles auto-commit, dry-run gating, target selection, and report printing
- [x] CLI test: `--dry-run` does not write to disk
- [x] CLI test: `--target local` writes to `config.local.toml`
- [x] CLI test: flags compose correctly (`--dry-run --target local` previews what would go into `config.local.toml`)

#### 9.17.3 Conflict detection

- [x] When multiple importers set the same Vulcan config key (during `--all`), detect and warn
- [x] Resolution: last writer wins (importers run in a fixed order: core first, then plugins alphabetically). Emit a warning listing the key, both sources, and which value was kept.
- [x] `ConfigImportReport` gains `conflicts: Vec<ImportConflict>` with `key`, `sources`, `kept_value`
- [x] Human output shows conflicts as warnings; JSON output includes them in the report object
- [x] Unit test: two importers setting the same key produces a conflict warning

#### 9.17.4 Core settings importer (`vulcan config import core`)

Import Obsidian's core settings files, which are currently only used as runtime fallback defaults during `load_vault_config`. Explicit import makes the vault self-contained — removing `.obsidian/` does not change behavior.

- [x] `CoreImporter` implementing `PluginImporter`, reading from up to three source files:
  - `.obsidian/app.json` — link style, link resolution mode, attachment folder, strict line breaks
  - `.obsidian/templates.json` — date format, time format, template folder
  - `.obsidian/types.json` — property type definitions
- [x] Import mappings:
  | Source file | Source key | Vulcan config key |
  |---|---|---|
  | `app.json` | `useMarkdownLinks` | `links.style` |
  | `app.json` | `newLinkFormat` | `links.resolution` |
  | `app.json` | `attachmentFolderPath` | `links.attachment_folder` |
  | `app.json` | `strictLineBreaks` | `strict_line_breaks` |
  | `templates.json` | `dateFormat` | `templates.date_format` |
  | `templates.json` | `timeFormat` | `templates.time_format` |
  | `templates.json` | `folder` | `templates.obsidian_folder` |
  | `types.json` | (all entries) | `property_types.*` |
- [x] `vulcan config import core` CLI subcommand with all shared flags
- [x] Missing source files are individually skipped (not all-or-nothing) — report which were found
- [x] Unit test: import from all three source files, verify config output
- [x] Unit test: partial sources (e.g., only `app.json` present) import correctly

#### 9.17.5 Dataview settings importer (`vulcan config import dataview`)

Parity with the other plugin importers. Dataview settings are currently auto-loaded during config initialization but have no explicit import command to write them into `config.toml`.

- [x] `DataviewImporter` implementing `PluginImporter`, reading from `.obsidian/plugins/dataview/data.json`
- [x] Import mappings (same 14 settings already parsed in `load_obsidian_dataview_config`):
  | Setting key | Vulcan config key |
  |---|---|
  | `inlineQueryPrefix` | `dataview.inline_query_prefix` |
  | `inlineJsQueryPrefix` | `dataview.inline_js_query_prefix` |
  | `enableDataviewJs` | `dataview.enable_dataview_js` |
  | `enableInlineDataviewJs` | `dataview.enable_inline_dataview_js` |
  | `taskCompletionTracking` | `dataview.task_completion_tracking` |
  | `taskCompletionUseEmojiShorthand` | `dataview.task_completion_use_emoji_shorthand` |
  | `taskCompletionText` | `dataview.task_completion_text` |
  | `recursiveSubTaskCompletion` | `dataview.recursive_subtask_completion` |
  | `displayResultCount` | `dataview.display_result_count` |
  | `defaultDateFormat` | `dataview.default_date_format` |
  | `defaultDateTimeFormat` | `dataview.default_datetime_format` |
  | `maxRecursiveRenderDepth` | `dataview.max_recursive_render_depth` |
  | `primaryColumnName` | `dataview.primary_column_name` |
  | `groupColumnName` | `dataview.group_column_name` |
- [x] `vulcan config import dataview` CLI subcommand with all shared flags
- [x] Unit test: import and idempotency

#### 9.17.6 Batch import commands

- [x] `vulcan config import --all` — discover all importable sources via the importer registry, run each in priority order, aggregate reports:
  - Respects `--dry-run`, `--target`, `--no-commit`, `--output`
  - Single commit for the batch (not one commit per importer)
  - Reports per-importer results and any conflicts (9.17.3)
  - Human output format:
    ```
    Importing settings...
      + core: 7 settings from app.json, templates.json, types.json
      + dataview: 14 settings imported
      + templater: 10 settings imported
      + tasks: 7 settings imported
      + kanban: 39 settings imported
      - quickadd: not detected
      - tasknotes: not detected
      - periodic-notes: not detected
    ```
- [x] `vulcan config import --list` — show what is importable without importing; calls `detect()` on each importer
  - Human output: detected/not-detected with source file paths
  - JSON output: array of `{ name, detected, source_paths }`
- [x] `--all` and `--list` are flags on the `Import` variant of `ConfigCommand`, coexisting with the existing subcommand dispatch
- [x] Unit test: `--all` imports all detected sources
- [x] Unit test: `--list` does not write anything
- [x] Integration test: `--all --dry-run` shows batch preview

#### 9.17.7 `vulcan init` integration

- [x] After `vulcan init` creates `.vulcan/config.toml`, detect importable sources via the importer registry and print a summary:
  ```
  Detected importable Obsidian settings:
    core (app.json, templates.json, types.json)
    dataview (.obsidian/plugins/dataview/data.json)
    templater (.obsidian/plugins/templater-obsidian/data.json)
  Run `vulcan config import --all` to import them.
  ```
- [x] `vulcan init --import` — automatically run `--all` import after initialization
- [x] `vulcan init --no-import` — suppress the detection summary (for scripted use)
- [x] Default behavior (no flag): detect and print the suggestion, do not auto-import

#### 9.17.8 Schema-drift hardening

- [x] Make registered JSON settings importers presence-aware so missing source fields never reset
  existing Vulcan settings to defaults.
- [x] Decode supported top-level fields independently, import valid fields when a sibling has an
  unsupported value, and surface each rejected field through `ConfigImportReport.skipped`.
- [x] Track current Templater structured hotkeys, QuickAdd template-folder arrays and structured
  file-exists behavior, Kanban's `-1` unlimited archive size, and Periodic Notes calendar sets while
  retaining legacy source aliases.
- [x] Add regression coverage for schema drift, partial failure, lossy mappings, and preservation of
  existing settings.

### 9.18 CLI redesign — command reorganization, note CRUD, JS runtime, and agent tools

**Goal:** Restructure the CLI command surface into a clean two-level hierarchy, add single-note CRUD operations, extend the query system, implement a general-purpose JS runtime with REPL, add web/git tools for agent use, and embed integrated documentation. The public help/describe surface now follows the grouped hierarchy; hidden migration aliases may remain temporarily while the cutover finishes.

**Design principle:** The CLI is simultaneously a human-facing tool and the tool interface for AI integrations (9.12 external runtimes now, any future native runtime later). Every command should have clean `--output json` support, deterministic behavior, and stable output contracts. The reorganization groups related commands under two-level subcommand namespaces for discoverability without sacrificing ergonomics.

#### 9.18.1 Command tree reorganization

Restructure all existing commands into logical groups. The public command surface is the grouped hierarchy; temporary hidden aliases may remain during migration.

**Depends on:** Phase 7 (all commands that are being moved must exist first)

**New command groups:**

| Group | Purpose | Commands |
|-------|---------|----------|
| `note` | Single-note CRUD and inspection | `get`, `set`, `create`, `append`, `patch`, `doctor`, `links`, `backlinks`, `diff` |
| `query` | Multi-note structured queries | (existing, enhanced) |
| `search` | Full-text content search | (existing, enhanced) |
| `refactor` | Cross-vault mutations | `rename-alias`, `rename-heading`, `rename-block-ref`, `rename-property`, `merge-tags`, `rewrite`, `move`, `link-mentions`, `suggest` |
| `web` | External data fetching (agent tools) | `search`, `fetch` |
| `run` | JS runtime execution and REPL | (new) |
| `help` | Integrated documentation | (new) |
| `daily` | Daily note operations | `today`, `show`, `list`, `export-ics`, `append` (extends 9.16) |
| `git` | Sandboxed git operations | `status`, `log`, `diff`, `commit`, `blame` |
| `graph` | Graph analytics | (existing) |
| `vectors` | Vector/semantic operations | (existing) |
| `tasks` | Unified task management (TaskNotes + inline) | `add`, `list`, `show`, `edit`, `set`, `complete`, `archive`, `create`, `query`, `eval`, `next`, `blocked`, `graph`, `track`, `view` (see 9.15.9) |
| `kanban` | Kanban board operations | (existing) |
| `bases` | Bases view operations | (existing) |
| `dataview` | Dataview evaluation | (existing) |
| `index` | Indexing infrastructure | `init`, `scan`, `rebuild`, `repair`, `watch`, `serve` |
| `saved` | Saved reports | (existing) |
| `config` | Plugin settings import | (existing) |
| `cache` | Cache maintenance | (existing) |

**Top-level commands (not grouped):** `doctor` (vault-wide), `diff` (vault-wide), `inbox`, `ls`, `describe`, `completions`, `checkpoint`, `changes`, `automation`, `export`, `browse`

- [x] **Split `vulcan-cli/src/lib.rs` into per-group modules.** The current `lib.rs` is ~10,400 lines containing the dispatch match, ~95 `print_*`/`render_*` functions, and command-specific logic in a single file. As part of this reorganization, split into:
  - `commands/note.rs`, `commands/graph.rs`, `commands/tasks.rs`, `commands/refactor.rs`, etc. — each module owns its dispatch arm and print functions
  - `output.rs` — shared output utilities (color, pagination, JSON helpers, `ListOutputControls`)
  - `resolve.rs` — note resolution and interactive selection helpers
  - `lib.rs` retains only top-level dispatch routing and shared setup
- [x] Restructure `Command` enum in `cli.rs` to use nested subcommand enums for each group
- [x] Move existing commands into their new groups:
  - `links`, `backlinks` → `note links`, `note backlinks`
  - `rename-alias`, `rename-heading`, `rename-block-ref`, `rename-property`, `merge-tags`, `rewrite`, `move`, `link-mentions` → `refactor *`
  - `suggest` → `refactor suggest`
  - `init`, `scan`, `rebuild`, `repair`, `watch`, `serve` → `index *`
- [x] Alias commands that appear in both group and top-level: `note doctor` → `doctor <note>`, `note diff` → `diff <note>`
- [x] Add a calendar navigation/rendering mode for periodic notes in the browse TUI; Phase 13 WebUI can reuse the same periodic/event data foundation for a graphical calendar view
- [x] Update `describe` command output to reflect new hierarchy
- [x] Update shell completion generation
- [x] Update all integration tests
- [x] Update `docs/cli.md` with new command reference

#### 9.18.2 Note CRUD commands (`note` group)

**Depends on:** Phase 7 (mutation infrastructure), Phase 2 (links/backlinks)

**`note get` — Read note content with selectors**

- [x] `vulcan note outline <note>` — return the note's semantic section ids, heading paths, block refs, and line spans for low-bloat follow-up reads
- [x] `vulcan note get <note>` — print full note content
- [x] `--section <id>` — extract a section by semantic id from `note outline`
- [x] `--heading <name>` — extract section under heading (inclusive of subheadings until next heading at same or higher level)
- [x] `--block-ref <id>` — extract block by reference ID
- [x] `--lines <range>` — extract line range (syntax: `1-10`, `50-`, `-5` for last 5 lines)
- [x] `--match <regex>` — grep-like: return matching lines
- [x] `--context <n>` — lines of context around `--match` hits (default: 0)
- [x] `--no-frontmatter` — strip YAML header from output
- [x] `--raw` — no formatting, no line numbers, just content
- [x] `--output json` returns structured object with content, frontmatter, metadata
- [x] JSON metadata includes continuation hints (`total_lines`, `has_more_before`, `has_more_after`) plus `section_id` when a semantic section is selected
- [x] Selectors are composable: `--heading "Section" --match "TODO"` searches within the heading
- [x] Shared core note-outline/selection logic powers CLI, MCP, and JS runtime reads so semantic partial reads behave the same everywhere
- [x] Search JSON hits expose `section_id` and absolute `line_spans` so agents can pivot from discovery to a precise follow-up note read

**`note set` — Replace note content**

- [x] `vulcan note set <note>` — read new content from stdin
- [x] `--file <path>` — read content from a file
- [x] `--no-frontmatter` — preserve existing YAML header, only replace body
- [x] `--check` — run doctor-like diagnostics after write (broken links, syntax, frontmatter)
- [x] Auto-commit if enabled
- [x] Incremental rescan after write

**`note create` — Create a new note**

- [x] `vulcan note create <path>` — create with empty content or from stdin
- [x] `--template <name>` — use a template (from 9.7/9.9 template system)
- [x] `--frontmatter <key=value>` — set frontmatter properties (repeatable)
- [x] `--check` — run diagnostics after creation
- [x] Error if note already exists (no silent overwrite)
- [x] Auto-commit if enabled

**`note append` — Append text to a note**

- [x] `vulcan note append <note> <text>` — append text at end (or read from stdin with `-`)
- [x] `--heading <name>` — append under a specific heading
- [x] `--check` — run diagnostics after append
- [x] Auto-commit if enabled

**`note patch` — Find and replace in a single note**

- [x] `vulcan note patch <note> --find <pattern> --replace <text>`
- [x] `--find` accepts literal strings or regex (prefix with `/` for regex: `--find '/\d{4}-\d{2}-\d{2}/'`)
- [x] **Safety: fails if `--find` matches more than once** (prevents accidental bulk edits)
- [x] `--all` flag to allow multiple replacements
- [x] `--section`, `--heading`, `--block-ref`, and `--lines` narrow patching to one semantic region before matching
- [x] `--check` — run diagnostics after patch
- [x] `--dry-run` — show planned changes without writing
- [x] Reuses `bulk_replace` infrastructure from `vulcan-core::suggestions`
- [x] Auto-commit if enabled

**`--check` flag (shared across write commands)**

- [x] Runs the same diagnostic checks as `doctor` on the single modified file
- [x] Reports: broken links, broken block refs, malformed frontmatter, syntax issues
- [x] Non-blocking: writes succeed even if checks find issues, but warnings are printed to stderr
- [x] `--output json` includes diagnostics in the response object

#### 9.18.3 Query enhancements

**Depends on:** Phase 7.12 (query model)

**Output format modes**

- [x] `--format table` — current default: columnar table output
- [x] `--format paths` — one file path per line, suitable for piping (like `find` or `rg -l`)
- [x] `--format detail` — expanded per-note view: path, frontmatter summary, first N lines of content
- [x] `--format count` — just the match count (integer)
- [x] `--glob <pattern>` — filter results by file path glob (e.g. `--glob "Projects/**"`)

**`ls` alias**

- [x] `vulcan ls` — thin alias for `vulcan query 'from notes' --format paths`
- [x] `--glob <pattern>` — filter by file path glob
- [x] `--where <filter>` — property filters (repeatable, AND-combined)
- [x] `--tag <tag>` — shorthand tag filter
- [x] `--format paths|detail|count` — output format (default: `paths`, unlike `query` which defaults to `table`)
- [x] Same underlying implementation as `query` — no new query engine, just different defaults

**Regex operator in predicates**

- [x] New `QueryOperator::Matches` variant for regex matching in `where` clauses
- [x] DSL syntax: `from notes where file.name matches "^\d{4}-\d{2}-\d{2}"`
- [x] Uses the `regex` crate
- [x] Case-insensitive variant: `matches_i`
- [x] Applies to string-valued fields only (property values, `file.path`, `file.name`)

**Regex in search**

- [x] Extend `search` command with regex support alongside existing `/pattern/` inline syntax
- [x] `vulcan search --regex <pattern>` for explicit regex queries
- [x] Regex results include line numbers and context (consistent with `--match` in `note get`)

#### 9.18.4 Refactor command group

Move existing mutation commands under `refactor` namespace. No behavioral changes — only the command path changes.

- [x] `vulcan refactor rename-alias` (was `vulcan rename-alias`)
- [x] `vulcan refactor rename-heading` (was `vulcan rename-heading`)
- [x] `vulcan refactor rename-block-ref` (was `vulcan rename-block-ref`)
- [x] `vulcan refactor rename-property` (was `vulcan rename-property`)
- [x] `vulcan refactor merge-tags` (was `vulcan merge-tags`)
- [x] `vulcan refactor rewrite` (was `vulcan rewrite`)
- [x] `vulcan refactor move` (was `vulcan move`)
- [x] `vulcan refactor link-mentions` (was `vulcan link-mentions`)
- [x] `vulcan refactor suggest mentions|duplicates` (was `vulcan suggest`)

#### 9.18.5 JS runtime, REPL, and vault scripting

**Depends on:** Phase 9.8.8 (rquickjs integration, `dv` API). This phase extends the DataviewJS sandbox into a general-purpose scripting environment.

**Script execution**

- [x] `vulcan run <script.js>` — execute a JS file (strips `#!` shebang line if present)
- [x] `vulcan run <script-name>` — look up by name in `.vulcan/scripts/` directory (strips `#!` shebang line if present)
- [x] `vulcan run --script` — shebang entry point: identical to `vulcan run <script.js>` but designed for use in shebang lines (`#!/usr/bin/env -S vulcan run --script`). Makes JS scripts directly executable by the OS, external agent harnesses (Claude Code, Codex, Gemini CLI), and shell pipelines without knowing they are Vulcan JS.
- [x] `--sandbox strict|fs|net|none` — sandbox isolation level (default: `strict`)
  - `strict`: CPU/memory limits, no I/O beyond read-only vault API
  - `fs`: adds write access to vault (note CRUD, frontmatter mutations, refactors)
  - `net`: adds network access (`web.search()`, `web.fetch()`)
  - `none`: drops resource limits (CPU/memory), retains all API access
- [x] `--timeout <duration>` — execution timeout (default: 30s), enforced via `Runtime::set_interrupt_handler()`
- [x] `console.log()` output to stdout at all sandbox levels
- [x] Script exit code: 0 on success, non-zero on error
- [x] `--output json` wraps script output in structured JSON

**REPL**

- [x] `vulcan run` (no arguments) — drops into interactive JS REPL
- [x] Persistent `Context` across evaluations (variables survive between prompts)
- [x] Multi-line input: detect incomplete expressions (unmatched `{`, `(`, template literals)
- [x] Tab completion for `vault.`, `vault.graph.`, `note.` and other API namespaces
- [x] Pretty-printed results: colored JSON for objects, formatted tables for note collections
- [x] REPL history saved to `.vulcan/repl_history`
- [x] Sandbox level configurable: `vulcan run --sandbox fs` then REPL has write access

**Deep vault JS API**

The JS runtime exposes deep access to vault internals, not just CLI wrappers. The API binds directly to vulcan-core structs.

**Tier 1 — Read-only (available at `strict` sandbox level):**

```js
// Note objects with rich properties
const note = vault.note("MyNote");
note.content          // raw markdown
note.outline()        // semantic sections, block refs, line spans
note.read({ section: "tasks@22" }) // partial read using the same selectors as `note get`
note.frontmatter      // parsed YAML as JS object
note.tags             // parsed tags array
note.aliases          // aliases array
note.headings         // parsed heading tree
note.blocks           // block refs
note.tasks            // parsed task items
note.dataview_fields  // inline DV fields
note.links()          // outgoing links as objects
note.backlinks()      // incoming links
note.neighbors(2)     // 2-hop neighborhood

// Graph as a first-class object
const g = vault.graph;
g.shortestPath("NoteA", "NoteB")
g.components()
g.hubs({ limit: 10 })
g.deadEnds()
g.neighbors("NoteA", { depth: 3 })
g.subgraph(["NoteA", "NoteB", "NoteC"])  // induced subgraph
g.filter(n => n.tags.includes("project")) // filtered graph view

// Collection operations with chainable API
vault.notes()
  .where(n => n.frontmatter.status === "active")
  .sortBy(n => n.mtime)
  .limit(10)
  .forEach(n => { ... })

// Query/search
vault.query("from notes where status = done", { format: "paths" })
vault.search("search term", { limit: 10 })

// Vectors/semantic
vault.vectors.similar("MyNote", { limit: 5 })
vault.vectors.search("concept query", { limit: 10 })
vault.vectors.cluster({ k: 8 })

// Daily notes / events
vault.daily.today()
vault.daily.get("2026-03-31")
vault.daily.range("2026-03-01", "2026-03-31")
vault.daily.today().events  // parsed structured events from 9.16.3

// Aggregated events across daily notes
vault.events({ from: "2026-03-31", to: "2026-04-07" })
```

**Tier 2 — Write (requires `fs` sandbox or higher):**

```js
vault.set(path, content, opts)
vault.create(path, opts)
vault.append(path, text, opts)
vault.patch(path, find, replace, opts)
vault.update(path, key, value)      // set frontmatter property
vault.unset(path, key)              // remove frontmatter property
vault.refactor.*                    // rename, rewrite, move, merge-tags
vault.inbox(text)
vault.daily.append(text, { heading: "Schedule", date: "2026-04-01" })

// Batch mutations (transactional)
vault.transaction(tx => {
  const note = tx.create("NewNote", { frontmatter: { status: "draft" } });
  tx.append("Index", `- [[${note.name}]]`, { heading: "Recent" });
  tx.patch("OldNote", { find: "old link", replace: `[[${note.name}]]` });
}); // atomic commit, doctor check at end
```

**Tier 3 — External (requires `net` sandbox):**

```js
web.search(query, opts)   // web search via configured backend
web.fetch(url, opts)      // fetch URL, opts.mode: "markdown"|"html"|"raw"
```

**Tier 4 — Unrestricted (`none` sandbox):**

- Drops CPU/memory resource limits but does not add new APIs beyond tiers 1-3
- `console.log()` available at all tiers

**Sandbox resource limits (applied at `strict`, `fs`, and `net` levels):**
- `Runtime::set_memory_limit()` — hard memory cap (configurable, default 64MB)
- `Runtime::set_max_stack_size()` — stack limit (default 256KB)
- `Runtime::set_interrupt_handler()` — periodic check for CPU time limit and `--timeout`

**Configuration in `.vulcan/config.toml`:**

```toml
[js_runtime]
memory_limit_mb = 64        # max JS heap (default 64MB)
stack_limit_kb = 256         # max stack size (default 256KB)
default_timeout_seconds = 30 # default --timeout value
default_sandbox = "strict"   # default --sandbox level
scripts_folder = ".vulcan/scripts"  # lookup path for named scripts
```

- [x] Implement `vault` global object with note(), notes(), query(), search() methods
- [x] Implement `Note` JS class wrapping `NoteIndex`/`NoteRecord` core structs
- [x] Implement `vault.graph` object wrapping petgraph structure
- [x] Implement collection API with `.where()`, `.sortBy()`, `.limit()`, `.forEach()`
- [x] Implement `vault.daily` namespace (delegates to 9.16 infrastructure)
- [x] Implement `vault.events()` aggregation across daily notes
- [x] Implement write methods (Tier 2) with sandbox level checks
- [x] Implement `vault.transaction()` for atomic batch mutations
- [x] Implement `web.search()` and `web.fetch()` (Tier 3), gated on `net` sandbox
- [x] Implement `help(obj)` introspection function (see 9.18.7)
- [x] Unit tests: each API method, sandbox enforcement, timeout/memory limits
- [x] Integration tests: scripts against test vault, REPL session simulation

#### 9.18.6 Web tools (`web` group)

**Depends on:** None (standalone HTTP client functionality). Primarily consumed by 9.12 external agent integrations and the JS runtime (9.18.5).

**`web search`**

- [x] `vulcan web search <query>` — perform a web search
- [x] `--backend kagi|...` — search backend (default from config, Kagi first implementation)
- [x] `--limit <n>` — max results (default: 10)
- [x] `--output json` returns structured results: `[{ title, url, snippet }]`
- [x] Pluggable backend via `SearchBackend` trait: `fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>>`
- [x] Configuration in `.vulcan/config.toml`:
  ```toml
  [web.search]
  backend = "kagi"
  api_key_env = "KAGI_API_KEY"
  ```
- [x] Kagi backend implementation using their Search API

**`web fetch`**

- [x] `vulcan web fetch <url>` — fetch a URL and output content
- [x] `--mode markdown` — convert HTML to markdown with `rs-trafilatura` main-content extraction
- [x] `--mode html` — raw HTML
- [x] `--mode raw` — raw response body
- [x] `--save <path>` — save output to file (for images, PDFs, binary content)
- [x] Markdown extraction fails explicitly when no readable main content is found; `html`/`raw` remain the escape hatches
- [x] `--output json` returns `{ url, status, content_type, content }`
- [x] Respect `robots.txt` (best effort)
- [x] User-Agent header identifying Vulcan

#### 9.18.7 Integrated documentation, describe, and external harness support

**Depends on:** None (can be developed independently). Content grows as other 9.18 sub-phases land.

This sub-phase covers three related concerns: human-facing documentation (`help`), machine-facing tool schema export (`describe`), and external LLM harness integration (vault AGENTS.md, default skills, JSON errors).

**`help` command**

- [x] `vulcan help` — overview and topic index
- [x] `vulcan help <topic>` — display documentation for a topic
- [x] Topics cover commands, concepts, and API reference:
  - Commands: `help note get`, `help query`, `help refactor`, `help daily`, etc.
  - Concepts: `help filters`, `help query-dsl`, `help scripting`, `help sandbox`
  - JS API: `help js`, `help js.vault`, `help js.vault.graph`, `help js.vault.note`
  - Guides: `help getting-started`, `help examples`
- [x] `vulcan help --search <keyword>` — search across all documentation topics
- [x] `vulcan help --output json <topic>` — structured help output for machine consumption (parameter names, types, descriptions, defaults, examples as JSON)
- [x] Rendered markdown in terminal with colors/formatting (using `termimad` or similar)
- [x] Distinct from `--help` which remains terse and flag-focused

**`describe` command enhancements**

- [x] `vulcan describe` — compact listing of all commands with one-line descriptions (existing, polish for LLM consumption)
- [x] `vulcan describe --format json-schema` — export tool definitions as JSON Schema (default, current behavior)
- [x] `vulcan describe --format openai-tools` — export as OpenAI function-calling tool definitions (name, description, parameters as JSON Schema)
- [x] `vulcan describe --format mcp` — export as MCP tool definitions for direct integration with Claude Code, Cursor, etc.
- [x] Each format includes: command name, description, parameters with types/defaults/required flags, and examples
- [x] External harnesses can call `describe` to auto-generate tool configs. Runtime-integration work is tracked in Phase 9.12.

**External LLM harness support**

For LLM harnesses (Claude Code, Codex, Gemini CLI, `pi`, etc.) that use Vulcan as a tool provider:

- [x] **Vault AGENTS.md template** — shipped with Vulcan, optionally written on `vulcan init`. Contents:
  - Available Vulcan commands organized by category with brief descriptions
  - Key conventions: always use `--output json`, `--dry-run` before mutations, note names may be ambiguous
  - Pointers to the skills directory: "Read `.agents/skills/*/SKILL.md` for detailed usage patterns and examples"
  - Common pitfalls: `note patch` fails on multiple matches (safety), property types are lenient, etc.
- [x] **Default skills as files** — bundled in the binary (via `include_str!`), written to vault via `vulcan init --agent-files` or `vulcan agent install`. See 9.12.6 for the full skill list. These serve external harnesses identically: Claude Code, Codex, Gemini CLI, or a reference `pi` adapter reads `.agents/skills/js-api-guide/SKILL.md` and learns the vault JS API.
- [x] **Dedicated harness installer** — `vulcan agent install [--reset <skill>] [--overwrite]` scaffolds root `AGENTS.md` plus `.agents/skills/<name>/SKILL.md`, and `init --agent-files` reuses the same bundled payload for first-run setup. Bundled skills opt into automatic refresh with `metadata.vulcan.managed: true`; unmarked same-name packages and create-only scaffolds are preserved, while targeted reset handles explicit migration or recovery.
- [x] **Consistent JSON error output** — all commands in `--output json` mode return structured errors: `{"error": "<message>", "code": "<error_code>"}` rather than unstructured stderr text. Error codes are stable and documented.
- [x] **Non-interactive guarantee** — all commands detect non-TTY mode and never prompt. Ambiguous note matches return an error with candidates rather than opening a picker.

**Documentation source**

- [x] Docs stored as markdown files in `docs/` directory in the repo
- [x] Organization:
  ```
  docs/
    guide/
      getting-started.md
      query-dsl.md
      filters.md
      scripting.md
      sandbox.md
    reference/
      commands/
        note-get.md
        query.md
        refactor.md
        daily.md
        ...
      js-api/
        vault.md
        graph.md
        note-object.md
        collections.md
        ...
    examples/
      recipes.md
  ```
- [x] Compiled into binary via `include_str!` or build script generating a `HashMap<&str, &str>`
- [x] Docs are versioned with the code — never out of sync

**`help()` in JS REPL**

- [x] `help(obj)` function available in the JS runtime
- [x] Displays function signature, parameter descriptions, return type, examples, and cross-references
- [x] Each Rust function exposed to JS carries its docstring as metadata
Example:
  ```
  vulcan> help(vault.query)
  vault.query(dsl: string, opts?: QueryOpts): NoteResult[]

  Run a query DSL string against the vault.
    dsl   - Query in Vulcan DSL syntax
    opts  - { format: "table"|"paths"|"detail"|"count", limit: number }

  Example:
    vault.query("from notes where file.path starts_with Projects/", { limit: 5 })

  See also: vault.notes(), vault.search()
  ```

#### 9.18.8 Git operations (`git` group)

**Depends on:** Phase 9.3 (git module). Provides sandboxed git access for 9.12 external agent integrations without requiring full shell access.

- [x] `vulcan git status` — working tree status (staged, modified, untracked)
- [x] `vulcan git log [--limit <n>]` — recent commit history (default: 10)
- [x] `vulcan git diff [<path>]` — show diff (optionally scoped to a note)
- [x] `vulcan git commit -m <message>` — create a commit (stages vault files only, not `.vulcan/`)
- [x] `vulcan git blame <path>` — per-line blame for a note
- [x] `--output json` on all subcommands
- [x] Implementation: shell out to `git` binary with controlled arguments (no arbitrary command injection)
- [x] Validation: refuse dangerous operations (force push, reset --hard, etc.)

#### 9.18.9 Task mutations (integrated into unified `tasks` CLI)

**Depends on:** Phase 9.10 (Tasks plugin compatibility), Phase 9.15 (TaskNotes)

Task mutation commands (`tasks create`, `tasks complete`, `tasks reschedule`) are defined in the unified CLI surface (9.15.9). This sub-phase covers the implementation:

- [x] Inline task creation: modify note content using `note patch` infrastructure (9.18.2)
- [x] Task completion: update inline task checkbox or TaskNotes frontmatter status
- [x] Task rescheduling: update due date in inline task emoji/annotation or TaskNotes frontmatter
- [x] Auto-commit if enabled

### Phase 9 implementation order

The Phase 9 sub-phases have both sequential dependencies and parallelization opportunities. This section consolidates the dependency edges into a recommended implementation order.

**Dependency graph:**

```
9.1 (edit) ─────────────────────────────┐
9.2 (browse TUI) ← 9.1                  │
9.3 (auto-commit) ──────────────────────│── can proceed in parallel
9.4 (additional CLI) ───────────────────│
9.5 (config layering) ────────────────-─│
                                        │
9.6 (advanced search) ───────────────-──│── foundation for 9.8, 9.12
9.7 (enhanced templates) ────────────-──│── foundation for 9.9
                                        │
9.8 (Dataview) ← 4 (Bases), 9.6         │
  9.8.1 (inline fields + type inference)│
  9.8.2 (list items + tasks)            │── sequential within 9.8
  9.8.3 (file.* metadata) ← 9.16        │
  9.8.4 (type system + expression eval) │
  9.8.5-9.8.7 (DQL + inline)            │
  9.8.8 (DataviewJS) ← sandbox          │── enables 9.9.3
  9.8.9 (settings import)               │
                                        │
9.9  (Templater)    ← 9.7, 9.8.8        │
9.10 (Tasks plugin) ← 9.8.2             │── can proceed in parallel
9.11 (Kanban)       ← 9.8.2, 7.1        │   (after their prerequisites)
9.16 (Periodic)     ← 1, 9.7            │
                                        │
9.15 (TaskNotes)    ← 4, 9.8, 9.10, 4.5.1│── primary task model, unified CLI
                                        │
9.13 (QuickAdd)     ← 9.7, 9.16         │── capture format compat
9.14 (plugin notes) ← informational     │
                                        │
9.17.1-3 (import infra)  ← 9.5          │── early (Wave 2)
9.17.4 (core importer)   ← 9.17.1      │── Wave 2
9.17.5 (dataview import) ← 9.17.1,9.8.9│── Wave 3
9.17.6 (batch commands)  ← 9.17.1      │── Wave 3
9.17.7 (init integration)← 9.17.6      │── Wave 3+
                                        │
--- AI path (CLI first, then external runtimes and MCP) ---
9.18.2 (note CRUD)       ← 7, 2        │── Wave 5 (CLI for LLMs)
9.18.3 (query enhance)   ← 7.12        │── Wave 5
9.18.6 (web tools)       ← standalone  │── Wave 5
9.18.7 (help/docs/describe)← standalone│── Wave 5
9.18.8 (git ops)         ← 9.3         │── Wave 5
+ default skills, vault AGENTS.md      │── Wave 5 deliverables
                                        │
9.12.1-6 (pi integration) ← 9.18.2,6,7,8│── Wave 6 (after CLI tools)
                                        │
9.21 (embedded assistant host mode)← 9.12,9.19.13 │── retired after pilot; MCP/external runtime path kept
                                        │
9.18.4 (refactor group)  ← 7           │── Wave 6+ (with 9.18.1)
9.18.5 (JS runtime/REPL) ← 9.8.8       │── Wave 6+ (after DataviewJS)
9.18.9 (task mutations)  ← 9.10        │── Wave 6+ (after Tasks)
9.18.1 (cmd reorg)       ← 7           │── last (after commands exist)
                                        │
9.19.1 (bug fixes)       ← 9.16, 9.18  │── Wave 6+ (anytime after periodic+CLI)
9.19.2 (run improvements)← 9.18.5      │── Wave 6+ (after JS runtime)
9.19.3 (shell completions)← 9.18.1     │── Wave 8+ (after cmd reorg)
9.19.4 (help polish)     ← 9.18.7      │── Wave 6+ (after help system)
9.19.5 (DQL completeness)← 9.8         │── Wave 6+ (after Dataview)
9.19.6 (missing commands)← 9.18,9.16   │── Wave 6+ (after CLI redesign+periodic)
9.19.7 (cmd reorg v2)    ← 9.18.1,9.19.6│── Wave 8+ (after cmd reorg + new cmds)
9.19.8 (scriptability)   ← 9.18        │── Wave 6+ (after CLI redesign)
9.19.9 (cmd clarity)     ← 9.18.1      │── Wave 8+ (after cmd reorg)
9.19.10 (web backend)    ← 9.18.6      │── Wave 6+ (after web tools)
9.19.11 (settings TUI)   ← 9.17        │── Wave 8+ (after settings import)
9.19.12 (plugin system)  ← 9.19.1,9.19.2,9.19.6│── Wave 8+ (after fixes+trust+cmds)
9.19.13 (permissions)    ← 9.19.6,9.19.12│── Wave 9 (after full cmd surface+plugins)
9.19.14 (binary size)    ← standalone  │── anytime (research)
9.19.17 (config surface) ← 9.17,9.19.11,9.19.12,9.19.13│── Wave 9+ (after import infra + initial settings TUI + plugins + permissions)
9.19.15 (MCP rework)     ← 9.12.6,9.18.7,9.19.6,9.19.13│── Wave 9 (after vault-native prompts + permissions + basic MCP)
9.23 (adaptive MCP packs)← 9.19.15,9.19.13,9.18.7     │── Wave 9+ (after protocol-native MCP, before broader MCP clients depend on it)
9.24 (skill command tools)← 9.18.5,9.19.12,9.19.13,9.19.15,9.23│── Wave 9+ (after JS runtime + plugins + permissions + protocol-native tool registry)
9.19.16 (test hardening) ← 9.19.6,9.19.7,9.19.13,9.19.12│── final hardening wave before 9.20/10
                                         │
9.25 (graph communities) ← 9.19.13      │── Wave 9+ (after permissions, existing graph adjacency)
9.26 (suggest links)     ← 9.25         │── Wave 9+ (after communities, existing mentions+vectors)
9.27 (confidence tags)   ← 9.26         │── Wave 9+ (after link suggestions, existing links schema)
```

**Recommended implementation order:**

The key sequencing principle for AI-related work: **CLI tool surface first** (usable by external harnesses immediately), **then external-runtime integration** (`pi` first), **then native runtimes or chat adapters only if they solve a proven gap**. Each phase is independently valuable.

1. **Wave 1 (parallel):** 9.1–9.5 — CLI foundation. These are largely complete and independent.
2. **Wave 2 (parallel):** 9.6 (search), 9.7 (templates), **9.17.1–9.17.4 (import infrastructure + core importer)** — the import infrastructure only depends on 9.5 (already complete). Core importer depends only on 9.17.1.
3. **Wave 3 (sequential + parallel):** 9.8.1 → 9.8.2 → 9.8.3 → 9.8.4 → 9.8.5 → 9.8.6 → 9.8.7 → 9.8.8 → 9.8.9 — Dataview, the largest sub-phase. Internal ordering is sequential. **9.17.5 (dataview importer) slots in after 9.8.9. 9.17.6 (batch commands) can proceed as soon as 9.17.4 + any existing importer are on the trait.** Refactor existing importers (9.9.4, 9.10.5, 9.11.4) to `PluginImporter` trait.
4. **Wave 4 (parallel):** 9.9 (Templater), 9.10 (Tasks), 9.11 (Kanban), 9.16 (Periodic notes) — all have their prerequisites met after Wave 3. Can proceed in parallel. Each plugin's settings import uses `PluginImporter` from the start.
5. **Wave 5 — CLI for LLMs (parallel):** **9.18.2 (note CRUD)**, **9.18.3 (query enhancements)**, **9.18.6 (web tools)**, **9.18.7 (help/describe polish)**, **9.18.8 (git ops)**, 9.15 (TaskNotes). This wave makes the CLI usable as a tool surface by any LLM harness (Claude Code, Codex, Gemini CLI, `pi`, etc.) without a Vulcan-native runtime. Deliverables include: note CRUD commands, `describe --format` for tool schema export, `help --output json` for structured command docs, default skills (bundled), vault AGENTS.md template, and consistent JSON error output. Can proceed in parallel with Wave 4.
6. **Wave 6 — External agent integration (sequential):** **9.12.1–9.12.6 as one coherent deliverable.** `pi` package/extension contract → tool boundary and trust model → AGENTS/skills-driven prompting → session/persistence boundary → rollout guidance and revisit criteria. Depends on Wave 5 for the tool surface.
7. **Wave 6+ (sequential after prerequisites):** **9.18.5 (JS runtime/REPL)** ← requires 9.8.8; **9.18.9 (task mutations)** ← requires 9.10; **9.18.4 (refactor group)** ← with 9.18.1.
8. **Wave 7 — Embedded host evaluation:** **9.21** shipped as a CLI-hosted managed-engine pilot and was then retired. Use MCP, `describe`, agent skills, and external runtimes instead.
9. **Wave 8:** 9.13 (QuickAdd) — capture format compatibility and settings import. Benefits from 9.7 (template variables) and 9.16 (periodic notes) being in place. QuickAdd importer (9.13.2) uses `PluginImporter`.
10. **Wave 9+:** 9.19.15 → 9.23 → 9.24 for the protocol-native programmable tool surface. Build the MCP-native registry first, then pack negotiation, then vault-defined skill command tools on top of that shared registry.
11. **Wave 9+ — Link graph intelligence (after Wave 9+ MCP foundation):** **9.25 → 9.26 → 9.27** sequenced because each builds on the output of the prior phase. Community detection (9.25) on the existing link graph enables cross-community scoring in link suggestions (9.26), which in turn feeds INFERRED edges into the confidence-tagged graph (9.27). 9.25 and 9.26 are additive (new features); 9.27 is a structural schema change that wires through all existing graph surfaces.
12. **9.17.7 (init integration)** can land anytime after 9.17.6.
13. **9.18.1 (command tree reorg)** should land last within 9.18 — it renames everything, so it's easier to build the new commands first (9.18.2–9.18.9) under the old structure, then reorganize in one pass.

**Critical path:** Phase 4 → 9.6 → 9.8.1 → ... → 9.8.8 → 9.9 (Templater). The Dataview sub-phases are the longest sequential chain and gate Templater's JS-dependent features. For the subprocess-runtime AI path, the critical chain is: 9.18.2/9.18.7/9.18.8 → 9.12.1–9.12.6 (`pi` integration). For the MCP-native AI path, the follow-on chain is: 9.12.6 (vault-native prompts/skills) → 9.19.6 (basic MCP server) → 9.19.13 (permissions) → 9.19.15 (protocol-native MCP rework) → 9.23 (adaptive MCP tool packs) → 9.24 (vault-native skill command tools). Native chat adapters are explicitly off the current critical path. For JS/runtime-backed programmability: 9.8.8 → 9.18.5 → 9.19.12/9.19.13 → 9.24.

**Note on 9.8.3 and 9.16:** The `file.day` metadata field in 9.8.3 depends on periodic note configuration from 9.16. However, `file.day` can be stubbed initially (return null when no periodic config exists) and filled in when 9.16 lands. This avoids blocking all of 9.8 on 9.16.

### 9.19 CLI polish, bug fixes, and UX improvements

**Goal:** Address usability issues, bugs, and missing features discovered during real-world CLI usage. This sub-phase focuses on polish rather than new capabilities — fixing broken flows, improving error messages, adding missing flags, and refining the interactive experience.

**Depends on:** 9.18 (CLI redesign), 9.8 (Dataview/DQL), 9.16 (Periodic notes)

**Test fixtures:** Several items below require test vaults that exercise features like periodic templates, DQL queries with complex expressions, and task management. A synthetic test vault should be created under `tests/fixtures/polish-vault/` with:
- A template directory (`00-09 Management & Meta/05 Templates/`) containing `daily.md`, `weekly.md`
- Notes with inline dataview fields, `triage_status` frontmatter, and `choice()`/`dateformat()` expressions
- A clippings folder with mixed `triage_status` values for DQL `WHERE` clause testing
- Kanban boards and bases for context-aware completion testing
- TaskNotes and inline tasks for task source toggling

**Reference data sources:** The issues below were discovered using a private Obsidian vault (`~/wikis/mimir`). Relevant query examples from that vault are captured in the DQL test cases rather than referenced directly.

**Recommended priority order** (within the Wave 6+ window where most 9.19 items land):

1. [x] **9.19.1** (bug fixes) — broken things first
2. [x] **9.19.5** (DQL completeness) — core functionality gap blocking real queries
3. [x] **9.19.4** (help polish) — first impression for new users
4. [x] **9.19.2** (run improvements) — developer experience, `--eval` is quick win
5. [x] **9.19.8** (scriptability) — CI/automation users, `--quiet` and `--output json` audit
6. [x] **9.19.6** (missing commands) — filling gaps, MCP server
7. [x] **9.19.3** (shell completions) — nice-to-have, depends on command surface being stable
8. [x] **9.19.9** (command clarity) — docs and naming, low effort
9. [x] **9.19.10** (web search backends) — explicit `SearchBackend` enum, Exa/Tavily/Brave
10. [x] **9.19.7** (reorg) — after everything above is built, reorganize in one pass
11. [x] **9.19.13** (permissions) — groundwork for Phase 17, can proceed in parallel with earlier items
12. [x] **9.19.12** (plugins) — after permissions design is clear
13. [x] **9.19.11** (settings TUI) — nice-to-have, depends on config surface being stable
14. [x] **9.19.17** (config surface completion) — close the remaining gap between the config model and the CLI/TUI/docs so users can manage aliases, permission profiles, plugin registrations, local overrides, and optional sections without manual TOML surgery
15. [x] **9.19.14** (binary size) — informational, anytime
16. [x] **9.19.15** (MCP protocol-native rework) — promote MCP from "CLI-over-JSON-RPC" to a protocol-native surface with curated tools, vault-native prompts, resources, completion, and structured results
17. [x] **9.23** (adaptive MCP tool packs) — replace the fixed exposure ladder with composable tool packs plus optional session-local pack negotiation for clients that can refresh tools on demand
18. [x] **9.19.16** (integration hardening) — thorough end-to-end coverage and fuzz/property testing before later platform work

#### 9.19.1 Bug fixes

**Periodic template resolution is broken**

The `resolve_template_file()` function (`vulcan-cli/src/lib.rs:8985`) matches the template name from periodic config against `template.name`, which is just the filename (e.g., `daily.md`). When the periodic config specifies a path like `00-09 Management & Meta/05 Templates/daily`, the match fails because the config value includes directory components but `template.name` does not. The template file exists and manual loading works — only periodic note auto-loading is broken.

- [x] Fix `resolve_template_file()` to also match against `template.display_path` (which includes the directory prefix) and strip `.md` from both sides of the comparison
- [x] Normalize path separators and handle both `foo/bar/daily` and `foo/bar/daily.md` forms
- [x] Add test: periodic note creation with a template path containing directory components
- [x] Add test: template resolution by bare name (`daily`) still works

**`vulcan vectors duplicates` hangs or is extremely slow**

The command appears to hang on non-trivial vaults. Needs profiling to determine whether it's a quadratic similarity comparison, unbounded result set, or blocking I/O issue.

- [x] Profile `vector_duplicates()` in `vulcan-core` on a vault with >1000 embedded notes
- [x] Tighten top-N pruning so the similarity cutoff tracks the current worst retained pair, not the just-evicted pair
- [x] Add progress reporting (incremental output or progress bar) for long-running similarity scans
- [x] Add `--limit` flag to cap result count and short-circuit early
- [x] Consider approximate nearest-neighbor indexing (e.g., HNSW) if brute-force is the bottleneck — deferred for now; the 1.2k-note benchmark keeps the duplicate phase under 1s and the current `VectorStore`/`sqlite-vec` stack does not yet expose ANN-specific capabilities. See `docs/investigations/vector_duplicates_ann.md`.

#### 9.19.2 `vulcan run` improvements

**`--eval` flag for one-liners**

Currently running a JS one-liner requires `echo "code" | vulcan run`. Add a direct flag.

- [x] `vulcan run --eval '<code>'` / `vulcan run -e '<code>'` — evaluate a JS expression and print the result
- [x] `vulcan run --eval-file <path>` — evaluate a JS file then drop into the REPL (preload mode)
- [x] Support multiple `-e` flags chained sequentially

**Trusted vault startup scripts (stretch goal)**

Auto-load `.vulcan/scripts/startup.js` before entering the REPL, but only if the vault is marked trusted.

- [x] Add vault trust model: `vulcan trust` marks current vault as trusted, stored in `~/.config/vulcan/trusted_vaults.json` (list of canonical vault root paths)
- [x] `vulcan trust --revoke` removes trust
- [x] `vulcan trust --list` shows trusted vaults
- [x] When trusted, `vulcan run` (REPL mode) auto-evaluates `.vulcan/scripts/startup.js` if it exists
- [x] Print a notice when startup script is loaded (`Loading .vulcan/scripts/startup.js...`)
- [x] `--no-startup` flag to skip auto-loading even in trusted vaults

**JS REPL QoL improvements**

The REPL currently uses rustyline but lacks several ergonomic features expected from a modern REPL.

- [x] **Tab completion:** Extend the existing completer to cover `dv.*`, `web.*`, `console.*`, `app.*`, and dot-commands.
- [x] **Special variables:** `_` = last successful result, `_error` = last error object.
- [x] **Multi-line editing:** Allow users to navigate within multi-line expressions using arrow keys (rustyline supports this with proper configuration). Show a visual continuation indicator.
- [x] **Reverse history search:** Enable rustyline's `Ctrl+R` reverse search mode (via `EditMode::Emacs`)
- [x] **Persistent history:** History persists at `.vulcan/repl_history`, max size raised to 10,000 entries
- [x] **Syntax highlighting:** Use rustyline's `Highlighter` trait to colorize JS keywords, strings, numbers, and comments in the input line
- [x] **Colorized result pretty-printing:** JSON output with colored keys (cyan), bracket dimming; JSON array colorized in `print_pretty_json`.
- [x] **REPL dot-commands:** `.type <expr>`, `.keys <expr>`, `.inspect <expr>`, `.time <expr>`, `.bench <expr> [n]`, `.source <fn>`

**`help()` improvements in JS runtime**

- [x] `help()` with no arguments — print a welcome message listing available globals (`vault`, `dv`, `web`, `console`, `app`)
- [x] `help(vault)` — print an overview of the vault API with all available namespaces
- [x] `help(dv)` — print an overview of the Dataview JS API
- [x] Register help metadata for all top-level objects (`vault`, `dv`, `web`, `console`, `app`)
- [x] Bare `help` (without parens) — now serializes as `"[function help]"` and REPL shows friendly tip for unknown type conversion errors

**`dv` global completeness**

The `dv` global is registered (`globalThis.dv = dv` in `dataview_js.rs:2394`) and has functions, but users report it appears empty. Investigate and fix:

- [x] Verify `dv` is accessible in the REPL context (confirmed via `globalThis.dv = dv` in prelude)
- [x] Add help metadata for `dv` (registered via `__vulcanRegisterHelp(dv, ...)`)
- [x] `Object.keys(dv)` returns method names (dv is a plain JS object)

**Obsidian API compatibility objects**

The JS runtime does not expose Obsidian-compatible objects (`app`, `tp`, etc.) that users expect for cross-compatibility with Obsidian scripts.

- [x] Add a stub `app` global: `app.vault.getName()`, `app.vault.getAbstractFileByPath()`, `app.vault.getMarkdownFiles()`, `app.vault.read()`, `app.vault.modify()`
- [x] Map Obsidian API calls to Vulcan equivalents (`app.vault.read(file)` → `vault.note(path).content`)
- [x] For unsupported Obsidian APIs (`app.workspace`, `app.metadataCache`), throw descriptive errors via Proxy
- [x] `help(app)` documents compatibility coverage

**Error handling for bare identifiers**

`help` (without parens) produces `error: Error converting from js 'undefined' into type 'string'`. This is a rquickjs type coercion error that leaks to the user.

- [x] Catch type conversion errors in the REPL eval loop and produce friendlier messages (`friendly_repl_error`)
- [x] `__vulcanPlain` now serializes functions as `"[function name]"` instead of causing type conversion errors

**Raw markdown / HTML access**

- These tasks are foundational for **Phase 9.20 (Static site builder)**. Land them on the shared renderer used by `site build`, later WebUI note pages, and any future wiki mode rather than as isolated one-off HTML conversions.
- [x] Ensure `vault.note(path).content` returns raw markdown (verify this works and document it)
- [x] Add `vault.note(path).html` — render the note's markdown to HTML using the existing markdown pipeline
- [x] `--mode html` flag on `vulcan note get` for CLI access to rendered HTML

#### 9.19.3 Shell completion improvements

The current completions are generated by clap and are not context-aware — they complete command names and flags but not dynamic arguments.

- [x] **`vulcan bases eval <tab>`** — complete with available bases view names (requires querying the vault index at completion time)
- [x] **`vulcan kanban <tab>`** — complete with kanban board note names
- [x] **`vulcan note get <tab>`** — complete with note names/paths from the index
- [x] **`vulcan daily show <tab>`** — complete with available date patterns (`today`, `yesterday`, `tomorrow`, ISO dates) and dates of existing daily notes in the vault
- [x] **`vulcan run <tab>`** — complete with script names from `.vulcan/scripts/`
- [x] **`vulcan tasks view <tab>`** — complete with saved task view names
- [x] Implementation: hidden `vulcan complete <context>` subcommand returns newline-separated candidates; `vulcan completions <shell>` appends dynamic hook lines after the static clap script
- [x] Support Fish, Bash, and Zsh dynamic completions
- [x] Complete reusable configured identifiers consistently across positional and option-value arguments: export, Outline, and site profiles; integration routes; permission profiles; aliases; plugins; saved reports; templates; and skills.
- [x] Keep generated Bash completion helpers compatible with macOS Bash 3.2, including empty-array expansion under `set -u`.

Skill-impact review: no bundled skill workflow changes are required. This is a human shell-discovery improvement over commands and safeguards already documented by the existing skills.

#### 9.19.4 Help system and CLI formatting polish

**Root help page redesign**

The current `vulcan help` is a flat list of ~130 subcommand paths without descriptions — effectively unusable. Both `vulcan help` and `vulcan --help` need to be overhauled.

- [x] Group commands by category (Note operations, Query & Search, Refactor, Tasks, etc.) with one-line descriptions
- [x] Show the command tree hierarchy with indentation for subcommands
- [x] Add a brief intro paragraph explaining what Vulcan is and common workflows
- [x] Colorize group headers, command names, and descriptions differently (headings bold-cyan, inline code bold-cyan in help; help now passes `--color` setting through instead of `is_terminal()`)
- [x] `vulcan help <group>` (e.g., `vulcan help note`) shows all subcommands in that group with descriptions and usage examples
- [x] `vulcan --help` (clap) should match the grouped layout — currently lists 37 commands in alphabetical order with no visual hierarchy
- [x] Keep `vulcan help` overview grouped; avoid repeating the same raw command tree below the curated category index

**Examples in `--help`**

Most commands show only flags in their `--help` output. Add 2–3 concrete usage examples to every command's help text.

- [x] Audit all commands for missing `Examples:` sections in their clap after-help — added `after_help` for `graph`, `checkpoint`, `export`, `cache`, `doctor`, `vectors`, `changes`, `cluster`, `related`, `trust`, `refactor`
- [x] Add examples that show common workflows, not just flag combinations
- [x] Prioritize commands users encounter first: `note get`, `query`, `search`, `daily`, `tasks list`, `run`
- [x] For the `saved` workflow, add an end-to-end example: create → list → run → use in automation

**Color and formatting**

- [x] Use consistent color scheme across all output: `AnsiPalette` (bold, yellow, red, cyan, dim) used throughout; help now routes through `--color` flag
- [x] Add `--color auto|always|never` global flag (respect `NO_COLOR` env var); also reads `VULCAN_COLOR` env var
- [x] Format table output with aligned columns when stdout is a TTY and `--fields` is specified with `--format table`
- [x] Add an integrated terminal markdown renderer, expose raw `--output markdown`, and add `vulcan render [file]` so markdown-first outputs auto-render in interactive terminals while staying pipe-friendly
- [-] Progress bars for long-running operations (scan, vectors, batch) using `indicatif` or similar — deferred; current eprintln progress reporting is sufficient for the common case

**`describe` command assessment**

Evaluate whether `describe` is still needed as a user-facing command or should be hidden/internal-only, given the improved `help` system.

- [x] If `describe` is only useful for LLM harness integration, move it to `vulcan describe` (keep it but remove from the main help listing, mark as `hide = true` in clap)
- [x] Ensure `help` covers all use cases that a human user would have used `describe` for
- [x] Make bare `vulcan describe` explicitly point to machine-readable export modes instead of dumping another human command list

#### 9.19.5 DQL completeness

The DQL engine is missing several Dataview features needed for real-world queries. Example query that fails (from a clippings management view):

```dql
TABLE
    file.link AS Clipping,
    choice(triage_status, triage_status, "new") AS Triage,
    dateformat(file.ctime, "yyyy-MM-dd") AS Added,
    dateformat(file.mtime, "yyyy-MM-dd") AS Updated
  FROM "00-09 Management & Meta/00 Inbox/00.12 Clippings"
  WHERE file.name != this.file.name
    AND (
      !triage_status
      OR triage_status = "new"
      OR triage_status = "split"
    )
  SORT file.ctime ASC
  LIMIT 100
```

Additional examples to test against (create synthetic equivalents in test fixtures):
- Project management views with `GROUP BY` and nested property access
- Master/admin dashboards with `FLATTEN` and multi-condition `WHERE` clauses
- Task triage views with `this.file.name` self-referencing and negation checks on frontmatter fields

Missing DQL features:

- [x] **`this.file.name` / `this.file.*` self-reference** — the current note's metadata in `WHERE` clauses. Requires passing the "source file" context to DQL evaluation.
- [x] **`file.link`** — render file path as a wikilink `[[name]]` in TABLE output
- [x] **`file.ctime` / `file.mtime`** — file creation and modification timestamps. Verify these are exposed in the DQL evaluation context; if not, add them from filesystem metadata or the scan cache.
- [x] **Falsy checks on frontmatter fields** — `!triage_status` should evaluate to true when the field is missing, null, empty string, or false. Currently may not handle missing-field-as-falsy correctly.
- [x] **`choice()` function** — already parsed (confirmed in `dql/compile.rs` tests) but verify end-to-end evaluation works with truthy frontmatter values as the condition
- [x] **`dateformat()` function** — already parsed (confirmed in `dql/eval.rs` tests) but verify it handles `file.ctime`/`file.mtime` date values correctly
- [x] **`FLATTEN` operator** — expand array-valued fields into separate rows
- [x] **`GROUP BY` with expressions** — group results by computed expressions, not just field names
- [x] Add integration tests for each of the above using the synthetic test vault

#### 9.19.6 Missing commands

Commands that are absent from the CLI but expected based on the existing surface area and common workflows.

**`note` group additions**

- [x] **`note delete <note> [--dry-run] [--no-commit]`** — delete a note and optionally report dangling inbound links. Currently notes can be created but not deleted from the CLI.
- [x] **`note rename <note> <new-name> [--dry-run] [--no-commit]`** — rename a note in-place, rewriting inbound links. Thin wrapper around `refactor move` with a friendlier interface (no target directory required when staying in the same folder).
- [x] **`note info <note>`** — summary view: path, word count, heading count, link/backlink counts, tag list, frontmatter keys, created/modified dates. Quick overview without reading content. `--output json` for scripting.
- [x] **`note history <note> [--limit <n>]`** — git log scoped to a single note file. Shortcut for `git log -- <path>` with formatted output showing commit message, date, and diff stats.

**Vault discovery commands**

- [x] **`vulcan status`** — vault overview: root path, note count, last scan time, cache size, config summary (enabled features, template/periodic settings), git branch and dirty status. The "dashboard" a user checks first.
- [x] **`vulcan tags [--count] [--sort count|name] [--where <filter>]`** — list all tags in the vault with occurrence counts. Filterable to a path prefix or property condition. `--output json` returns `[{ tag, count }]`.
- [x] **`vulcan properties [--count] [--sort count|name] [--type]`** — list all frontmatter property keys used across the vault with occurrence counts and inferred types. Essential for discovering available `--where` filter fields.

**Graph export**

- [x] **`graph export --format dot|json|graphml`** — export the resolved link graph for visualization in external tools (Gephi, Graphviz, d3, etc.). `dot` produces Graphviz DOT format, `json` produces `{ nodes: [...], edges: [...] }`, `graphml` produces GraphML XML.

**Config management**

The `config` group currently only has `import`. Users need to inspect and modify config without editing TOML by hand.

- [x] **`config show [section]`** — print current effective config (merged `.vulcan/config.toml` + defaults). Optional section filter (`config show periodic`, `config show js_runtime`).
- [x] **`config set <key> <value>`** — set a single config value. Dot-notation keys: `config set periodic.daily.template "Templates/daily"`. Validates the value type before writing.
- [x] **`config get <key>`** — read a single config value. Useful in scripts.

**Periodic note subcommand parity**

`daily` has 5 subcommands (`today`, `show`, `list`, `append`, `export-ics`) but `weekly` and `monthly` are bare commands with no subcommands. When nesting under `periodic` (9.19.7), all period types should share the same surface.

- [x] **`periodic show [date] --type daily|weekly|monthly`** — display a periodic note (generalized from `daily show`)
- [x] **`periodic list --type <type> [--from] [--to]`** — list periodic notes of a type (generalized from `daily list`)
- [x] **`periodic append <text> --type <type> [--heading] [--date]`** — append to a periodic note (generalized from `daily append`)
- [x] **`periodic export-ics --type <type> [--from] [--to]`** — export events from periodic notes
- [x] `daily today`, `daily show`, etc. remain as convenience aliases that set `--type daily` implicitly

**Template subcommand consistency**

`template --list` is a flag, unlike every other group which uses `<group> list`. There's also no way to view a template's contents without opening the file.

- [x] **`template list`** — proper subcommand replacing `template --list`
- [x] **`template show <name>`** — display a template's raw contents and metadata (source, engine, variables)
- [x] Keep `template --list` as a hidden backward-compat alias

**Convenience aliases**

- [x] **`vulcan today`** — top-level alias for `vulcan daily today`. This is the single most common command; saving keystrokes matters.

**MCP server mode**

`index serve` exposes HTTP but only 8 read-only endpoints. Full HTTP API expansion to cover the broader CLI surface (note CRUD, tasks, refactor, git, etc.) is still deferred to **Phase 10.3** where it ships properly with axum, middleware, and multi-vault support. MCP now has its own dedicated transport path via `vulcan mcp`, so LLM harness integration no longer depends on `index serve`.

- [x] **`vulcan mcp`** — start an MCP server over stdio or Streamable HTTP. Exposes the full tool surface from `describe --format mcp` as a live server.
- [x] Reuse the `describe --format mcp` tool definitions as the MCP tool manifest
- [x] Support MCP tool calls by dispatching to the same command handlers used by the CLI
- [x] MCP server should respect the permission layer (9.19.13) via `--permissions <profile>`
- [x] Add `vulcan mcp` to the top-level command list (it's an integration point, not a subcommand of `index`)

Full MCP follow-on work is tracked in **9.19.15 MCP protocol-native rework** below.

#### 9.19.7 Command reorganization

**Depends on:** 9.18.1 (command tree reorg) — this sub-phase is a second pass after the initial reorganization, informed by real usage.

**Reduce top-level command count**

The current CLI has 37 top-level commands. This is too many for discoverability. Target: ~25 top-level commands by nesting or merging.

- [x] **Nest `cluster` and `related` under `vectors`** — use `vectors cluster` and `vectors related`; the old top-level entries were removed.
- [x] **Nest `weekly` and `monthly` under `periodic`** — use `periodic weekly` and `periodic monthly`; the old top-level entries were removed.
- [x] **Merge `batch` into `automation`** — use `automation run`; the old top-level `batch` entry was removed.
- [x] **Hide top-level `diff`** — `note diff` already exists for single-note diffs. If the top-level `diff` does something different (vault-wide), clarify; if identical, remove.
- [x] **Absorb `notes` into `query`** — `notes --where` is a subset of `query`. Use `vulcan query`; `vulcan notes` is not a command.

**Group reassignment**

The current group labels don't match user mental models.

- [x] Move `browse`, `edit`, `open` into an **Interactive** group
- [x] Move `run`, `web` into a **Scripting** group (or keep under a renamed "Tools" group)
- [x] Move `diff` out of "Graph and Query" — it's about change history, not querying
- [x] Reconsider `template` under "Journaling" — templates are used for more than journals; maybe "Content Creation" or just "Notes"

**`automation` expansion**

Currently has only one subcommand (`run`). Either flatten to a top-level command or add subcommands to justify the group.

- [x] If merging `batch` into `automation`, the group gains purpose: `automation run [reports...]`, `automation run --all`, `automation list` (show what would run)
- [-] Otherwise, flatten `automation run` to a top-level `automation` command with run semantics

**`saved` creation UX**

The current `saved search <name> <query>` reads like "search within saved reports" rather than "create a saved search". Improve discoverability.

- [x] Rename creation subcommands: `saved create search <name>`, `saved create notes <name>`, `saved create bases <name>` — or use a flag: `saved create <name> --type search`
- [x] Add `saved delete <name>` if not already present

**`update`/`unset` placement**

These top-level bulk mutation commands aren't in any group and feel orphaned. They operate on filtered note sets (like `query`) but mutate frontmatter (like `note`).

- [x] Move to `note update --where <filter> --key <key> --value <value>` and `note unset --where <filter> --key <key>`. The `--where` flag distinguishes bulk mode from single-note mode.
- [-] Alternatively, place under `refactor update`/`refactor unset` since they're cross-vault mutations.
- [x] Keep hidden top-level aliases for backward compatibility.

**Query language auto-detection**

Users who know Dataview will type DQL into `vulcan query` and get errors. Users who know Vulcan DSL will be confused by `dataview query`.

- [x] Auto-detect query language in `vulcan query`: if input starts with `TABLE`, `LIST`, `TASK`, or `CALENDAR` (case-insensitive), route to DQL evaluation
- [x] Print a note when auto-detection triggers: `(detected as Dataview query)`
- [x] `--language dql|vulcan` flag to force language when auto-detection is wrong

**User-defined command aliases**

Power users want shortcuts like `vulcan t` → `vulcan tasks list` or `vulcan q` → `vulcan query`.

- [x] Add `[aliases]` section in `.vulcan/config.toml`:
  ```toml
  [aliases]
  t = "tasks list"
  q = "query"
  tl = "tasks list --source tasknotes --sort-by due"
  inbox = "inbox"
  ```
- [x] Alias expansion happens before clap parsing — simple string prefix replacement
- [x] `vulcan aliases` or `config show aliases` to list active aliases
- [x] Built-in aliases ship as defaults (e.g., `today` → `daily today`) and can be overridden


#### 9.19.8 Scriptability improvements

**Depends on:** 9.18 (CLI redesign)

**Goal:** Make every command composable in shell pipelines, CI scripts, and LLM tool chains. The CLI should follow the Unix philosophy: each command does one thing, outputs are predictable and parseable, and commands compose via pipes.

**Quiet mode**

- [x] Add `--quiet` / `-q` global flag — suppress scan progress, warnings, and non-essential stderr output. Only errors and primary output remain.
- [x] Respect `VULCAN_QUIET=1` environment variable as equivalent to `--quiet`

**Table output control**

- [x] Add `--no-header` flag for table/TSV output — suppress column headers for piping to `cut`/`awk`
- [x] Add `--format tsv` output mode — tab-separated values, easy for shell pipelines. Complement to `--output json` which is heavy for simple field extraction.
- [x] `--format csv` as a direct output mode (not just `--export csv` which writes to a file)

**Field discovery**

- [x] `vulcan query --list-fields` — print available field names for `--where`, `--sort`, `--fields` based on the current vault's frontmatter keys and `file.*` builtins
- [x] Include field types and example values where available

**Exit code conventions**

- [x] Standardize exit codes across all commands: 0 = success, 1 = error, 2 = issues found (doctor, automation)
- [x] Add `--exit-code` flag on query/search commands: return exit code 1 if zero results. Useful in conditionals: `if vulcan query --where 'status = blocked' --exit-code; then ...`

**Stdin-based batch operations**

- [x] `vulcan note update --stdin` — read note paths from stdin (one per line) instead of using `--where` filters. Enables: `vulcan ls --tag todo --format paths | vulcan note update --stdin --key status --value done`
- [x] Same for `vulcan note unset --stdin`, `vulcan refactor rewrite --stdin`
- [x] This complements `--where` filters — filters for ad-hoc, stdin for composed pipelines

**`--output json` audit**

- [x] Verify every command supports `--output json` and produces valid JSON
- [x] Commands that currently only support `--output human` should gain JSON support
- [x] JSON output should never include ANSI escape codes or progress output

**Structured error output**

- [x] All commands in `--output json` mode should return errors as `{"error": "<message>", "code": "<error_code>"}` on stdout with appropriate exit code — not unstructured stderr text (partially implemented in 9.18.7, verify completeness)
- [x] Handle closed stdout pipes gracefully so commands used with `head`, `sed`, or similar shell filters exit quietly instead of panicking on `Broken pipe`

#### 9.19.9 Command clarity and discoverability

**Status:** Complete. Remaining `[-]` entries in this section are intentional product decisions: use `automation run` as the single batch-report entrypoint and use Phase 9.20 `site build` as the canonical HTML publication path instead of adding a second renderer under `export html`.

**`vulcan automation run` / `vulcan saved` — report system is opaque**

The relationship between saved reports, automation run, and the `saved` command is unclear to users. It's not obvious what a "report" even is, how to create one, or when to use which command.

- [x] Write a clear conceptual overview for `vulcan help reports` explaining: what a saved report is (a persisted query/check in `.vulcan/reports/`), how to create one, the report file format, and how they relate to automation and `saved`
- [x] Clarify the command roles and either merge or clearly differentiate:
  - `vulcan saved` — CRUD for saved reports (list, show, create, delete)
  - `vulcan automation run` — execute reports with optional scan/doctor/repair checks and CI exit codes
- [x] If the distinction between `automation run` and `batch` doesn't justify two commands, merge them — `batch` was removed; `automation run` is the single entrypoint
- [x] Make `--all` behavior consistent across commands — `automation run --all-reports` is the only batch-report all switch
- [x] Add usage examples showing the full workflow: create a report → run it → use in CI

**`vulcan changes` purpose**

The command reports note/link/property/embedding changes since the last scan or checkpoint. Clarify when users should use it.

- [x] Add a clear description in `--help` and `vulcan help changes` explaining use cases: post-sync review, changelog generation, CI diff checks
- [x] Add usage examples in the help text

**`vulcan export` expansion**

The export surface now covers documents, datasets, archives, and static search indexes. EPUB was the remaining book-friendly gap.

- [x] `vulcan export markdown <query>` — export matched notes as a combined markdown document
- [x] `vulcan export json <query>` — export note metadata and content as JSON
- [x] `vulcan export csv <query>` — export query results as CSV
- [-] `vulcan export html <query>` — superseded by **Phase 9.20** `site build`; if retained, implement as a thin one-shot wrapper around a transient site profile rather than a separate rendering path
- [x] `vulcan export graph --format dot|json` — export the link graph in DOT or JSON format
- [x] `vulcan export zip <query> -o vault.zip` — export matched notes with content, metadata, and attachments as a structured ZIP archive (preserves directory layout)
- [x] `vulcan export sqlite <query> -o vault.db` — export to a self-contained SQLite database with tables for notes (path, content, frontmatter JSON), links, tags, and tasks
- [x] `vulcan export epub <query> -o book.epub` — render matched notes to an EPUB document optionally enriched with backlinks, with table of contents derived from note structure, tags and link ordering
- [x] EPUB export bundles referenced local assets into the book archive and rewrites chapter links/embed sources to packaged media paths
- [x] Add reusable export `content_transforms` (initially `exclude_callouts`) so publication-oriented exports can strip callout blocks before packaging Markdown/JSON/EPUB/ZIP output
- [x] Generalize profile `content_transforms` to ordered rule tables with optional rule queries; the export profile query still defines the exported note set, while each rule query only targets which exported notes receive that rule's transforms
- [x] Rework `vulcan export profile` around explicit `rule` management so profile creation/settings stay focused on profile-wide fields while ordered publication rules are added, updated, listed, deleted, and moved directly
- [x] Extend `content_transforms` with section filtering (`exclude_headings`) so publication exports can drop whole heading sections and their subsections
- [x] Extend `content_transforms` with metadata filtering (`exclude_frontmatter_keys`, `exclude_inline_fields`) so exported/public notes can remove sensitive structured data without hand-editing sources; transformed export metadata, links, and inline-expression evaluation must all be rebuilt from the rewritten note content
- [x] Extend `content_transforms` with literal/regex replacement rules for publication redaction and normalization workflows; replacement order must be preserved and transformed metadata/attachment references must be rebuilt from rewritten content
- [x] Add publication link policy controls for transformed exports and future site builds (`error`, `warn`, `drop-link`, `render-plain-text`) when content transforms remove the target or anchor context
- [x] Add publication asset policy controls so transformed exports and future site builds can exclude/rewrite attachments based on path, extension, or whether they are only referenced from stripped content

**`vulcan tasks` source selection**

The command may not correctly toggle between TaskNotes-only and all-tasks (including inline embedded tasks) modes.

- [x] Add `--source tasknotes|inline|all` flag (default from config)
- [x] Add config option `[tasks] default_source = "all"` in `.vulcan/config.toml`
- [x] Verify filtering logic works correctly for each source mode
- [x] Document the distinction in `vulcan help tasks`

**`notes` vs `note` confusion**

`vulcan notes` (property query) and `vulcan note` (single-note CRUD) differ by one character. Users will constantly type the wrong one.

- [x] At minimum, add a clear error message when `vulcan notes get` or `vulcan note --where` is attempted: suggest the correct command
- [x] Long-term: absorb `notes` into `query` (see 9.19.7) to eliminate the confusion entirely; `vulcan notes` was removed and `vulcan query` is the supported command

**`vulcan note outline` on large docs**

Large markdown documents are hard to navigate when the outline repeats full heading paths on every line, and agents often need to inspect/patch standalone `.md` files outside the current vault root.

- [x] Add `vulcan note outline --section <id>` to focus on one outline subtree
- [x] Add `vulcan note outline --depth <n>` to limit descendants relative to the current scope
- [x] Render human `vulcan note outline` output as a clearer tree with separate scope metadata instead of repeating full heading paths on every line
- [x] Allow `vulcan note outline`, `vulcan note get`, and `vulcan note patch` to operate on explicit markdown file paths outside the current vault root
- [x] Add `vulcan note checkbox` for direct checkbox toggles by absolute line or scoped checkbox index, including standalone markdown files outside the vault root
- [x] Render human `vulcan note outline` output with heading markers (`#`, `##`, ...) plus nested line/id metadata and ANSI color when enabled

#### 9.19.10 Web search backend expansion

Make the search backend an explicit enum (`SearchBackend`) and add support for additional providers beyond the current Kagi implementation.

- [x] Replace free-form `backend: String` with a `SearchBackend` enum (`Kagi`, `Exa`, `Tavily`, `Brave`) in config
- [x] Each variant defines its own `default_api_key_env()` and `default_base_url()` so users only need to set `backend = "exa"` and the rest auto-derives
- [x] Implement Exa search backend: `api_key_env = "EXA_API_KEY"`, `base_url = "https://api.exa.ai/search"`, auth via `x-api-key` header, JSON POST body
- [x] Implement Tavily search backend: `api_key_env = "TAVILY_API_KEY"`
- [x] Implement Brave Search backend: `api_key_env = "BRAVE_API_KEY"`
- [x] Change default backend order: Kagi (if key present) → Exa (if key present) → Tavily (if key present) → Brave (if key present) → error with setup instructions
- [x] For `web fetch`, evaluate Tavily Extract and Firecrawl as alternatives to the current built-in extraction path
  See `docs/investigations/web_fetch_extract_backends.md`.
- [x] Add duckduckgo backend
- [x] Use duckduckgo backend as default backend as it can work without an api key
- [x] Update config documentation with backend options and per-provider examples

#### 9.19.11 Settings TUI

**Goal:** A terminal UI for viewing and editing `.vulcan/config.toml` with import-from-Obsidian support.

- [x] `vulcan config edit` — open a TUI (using `ratatui`) for browsing and editing settings
- [x] Organize settings by category with descriptions for each option
- [x] `vulcan config import --preview` — show a diff of what would change before applying imported settings
- [x] `vulcan config import --apply` — apply the diff
- [x] Validate settings on save (reject invalid values with inline error messages)
- [x] TaskNotes settings import may need fixes — verify and fix edge cases

#### 9.19.12 Event-driven plugin system (research + design)

**Goal:** Allow users to write JS plugins that hook into Vulcan lifecycle events (file write, pre-commit, post-scan, note create, etc.), similar to Git hooks or IDE extensions but running inside the rquickjs sandbox with the vault trust and permission model from 9.19.2.

**Depends on:** 9.19.1 (bug fixes), 9.19.2 (trusted vaults, JS runtime improvements), 9.19.6 (missing commands — plugin hooks need the full mutation surface)

**Design considerations:**

- Plugins are JS files in `.vulcan/plugins/` registered via `.vulcan/config.toml`
- Event model: `on_note_write`, `on_note_create`, `on_note_delete`, `on_pre_commit`, `on_post_commit`, `on_scan_complete`, `on_refactor`, etc.
- Plugins declare which events they subscribe to and what permissions they need (read-only, fs, net)
- Vault must be trusted (9.19.2 trust model) for plugins to run; untrusted vaults skip plugin execution with a warning
- Plugins can block events (e.g., a linter returning errors on `on_note_write` prevents the write) or run as post-hooks (fire-and-forget)
- Sandbox level per plugin, capped at the vault's trust level
- Plugin execution timeout inherited from JS runtime config
- `vulcan plugin list`, `vulcan plugin enable/disable`, `vulcan plugin run <name>` for manual invocation

**Tasks:**

- [x] Design the event lifecycle and hook points — enumerate all mutation paths in vulcan-core that should emit events
- [x] Design the plugin manifest format (event subscriptions, permissions, metadata)
- [x] Design blocking vs non-blocking hook semantics (pre-hooks can abort, post-hooks cannot)
- [x] Prototype: single `on_note_write` hook running a JS linter function in rquickjs
- [x] Implement plugin discovery and registration from `.vulcan/plugins/`
- [x] Implement event dispatch at each hook point in vulcan-core
- [x] Implement `vulcan plugin` CLI commands
- [x] Document the plugin API in `help js.plugins`

#### 9.19.13 Permission layer

**Goal:** Add a unified permission model in `vulcan-core` that applies across the entire application — CLI commands, HTTP serve API, MCP server, JS runtime, and plugins. The CLI defaults to full permissions (optimized for human use); restrictions are for specific contexts: semi-trusted script input, sandboxed agents, serve API consumers, and plugins. This is also the **foundational layer** that Phase 17's delegable capability system builds on: identity-aware grant resolution must produce the same `PermissionGrant` and use the same `PermissionFilter`, not build a parallel enforcement path.

**Depends on:** 9.19.6 (missing commands — the full command surface should exist before gating it), 9.19.12 (plugin system — key consumer)

**Motivation:** Currently, the only permission controls are: (1) the JS runtime sandbox levels (`strict`/`fs`/`net`/`none`), which only gate JS API calls; (2) the `--auth-token` flag on `index serve`, which is all-or-nothing. There is no way to grant an MCP client read-only access, restrict a plugin to a specific folder, cap JS resource usage per profile, or prevent a serve API consumer from running refactors. The JS sandbox should be subsumed by this model rather than remaining a parallel system.

**Relationship to Phase 17 (multi-user delegable capabilities):**

Phase 17 introduces users, groups, explicit root grants, attenuable child grants, limited credentials for agents and automation, document-level secrets, share links, and identity-aware query filtering. Roles such as `owner`, `editor`, and `viewer` remain ergonomic grant templates rather than a separate RBAC enforcement system. The design here must ensure Phase 17 is a natural extension, not a rewrite:

- Phase 9.19.13 defines the **`PermissionGrant` type** in `vulcan-core` — the resolved unit of "what is allowed". Phase 17 defines how one or more rooted, attenuated grants resolve for a user, group, agent, automation process, service, or share credential; the resolved type itself is shared.
- Phase 9.19.13 defines the **`PermissionGuard` trait** — the interface every command handler calls to check permissions. Phase 17 implements it with a capability-aware resolver (authenticated subject → applicable rooted grants and constraints → effective grant), while 9.19.13's implementation resolves from a static profile.
- Phase 17's `PermissionFilter` (which generates SQL CTEs for permission-filtered queries) takes a resolved `PermissionGuard` as input — not a user identity or grant graph. This means the same filter works for static-profile restrictions (9.19.13) and capability-derived restrictions (17.3).
- **Resource specifiers** (`folder:<path>`, `tag:<tag>`, `note:<path>`) are defined here and reused verbatim in Phase 17 capability grants. Phase 17 adds subjects, issuers, parent lineage, delegation constraints, expiry, and revocation; 9.19.13 does not need identity or provenance because a profile is already resolved.

```
  9.19.13                          Phase 17
  ┌─────────────────┐             ┌──────────────────────────────┐
  │ PermissionGrant  │◄────────────│ Root + delegated grants      │
  │ (path rules,     │             │ for user/group/credential    │
  │  capability flags,│             │ → resolves to PermissionGrant│
  │  resource limits) │             └──────────────────────────────┘
  └────────┬─────────┘
           │
  ┌────────▼─────────┐
  │ PermissionGuard   │ ← trait, called by every command handler
  │  check_read(path) │
  │  check_write(path)│
  │  check_network()  │
  │  resource_limits()│
  └────────┬─────────┘
           │
  ┌────────▼─────────┐
  │ PermissionFilter  │ ← generates SQL CTEs for filtered queries
  │  (17.3 extends)   │    works with both profile-based and user-based guards
  └──────────────────┘
```

**Permission dimensions:**

| Dimension | Scope | Description |
|-----------|-------|-------------|
| **Read** | resource specifiers (`folder:`, `tag:`, `note:`) or `all`/`none` | Which notes/files can be read |
| **Write** | resource specifiers or `all`/`none` | Which notes/files can be created/modified/deleted |
| **Refactor** | resource specifiers or `all`/`none` | Cross-vault mutations (rename, rewrite, merge-tags, move). Scope restricts which files a refactor may touch. |
| **Git** | `allow`/`deny` | Git operations (commit, blame, log, diff) |
| **Network** | `allow`/`deny`, optional domain allowlist | Web search and fetch. Domain allowlist restricts which hosts can be contacted. |
| **Index** | `allow`/`deny` | Scan, rebuild, repair, watch, serve |
| **Config** | `read`/`write`/`none` | Config inspection vs. modification |
| **Execute** | `allow`/`deny` | JS runtime and plugin execution |
| **Shell** | `allow`/`deny` | Dangerous: ability to run arbitrary CLI commands from JS (future API, default `deny`) |
| **CPU limit** | integer ms or `unlimited` | Per-evaluation CPU time cap for JS execution |
| **Memory limit** | integer MB or `unlimited` | JS heap size cap |
| **Stack limit** | integer KB or `unlimited` | JS stack size cap |

**Resource specifiers** (shared with Phase 17):

- `folder:<glob>` — applies to all notes under matching folders (recursive). Glob syntax: `Projects/**`, `Journal/2026-*/**`.
- `tag:<tag>` — applies to notes carrying the tag. Useful for restricting access to `#secret` or `#draft` content.
- `note:<path>` — applies to a single note.
- `*` (wildcard) — matches all resources (equivalent to `all`).

The same specifier syntax is used in Phase 17 grants, ensuring consistency. Existing static profiles continue to accept `allow` and `deny` lists, with deny taking precedence. Phase 17 expresses ordinary user authority as positive, default-deny capabilities; non-delegable canonical policy ceilings may still narrow the resolved grant.

```toml
[permissions.profiles.agent]
read = "all"
write = { allow = ["folder:Projects/**", "folder:Journal/**", "folder:AI/**"], deny = ["folder:Archive/**"] }
refactor = { allow = ["folder:Projects/**"] }
git = "allow"
network = { allow = true, domains = ["api.tavily.com", "*.wikipedia.org"] }
index = "deny"
config = "read"
execute = "allow"
shell = "deny"
cpu_limit_ms = 5000
memory_limit_mb = 64
stack_limit_kb = 256

[permissions.profiles.readonly]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
execute = "deny"

[permissions.profiles.plugin-lint]
read = "all"
write = "none"
network = "deny"
shell = "deny"
cpu_limit_ms = 2000
memory_limit_mb = 32

[permissions.profiles.player]
# Preview of what Phase 17 can resolve from rooted, attenuated grants.
# 9.19.13 can express this statically; 17 generates it dynamically per subject.
read = { allow = ["*"], deny = ["folder:GM-Only/**", "tag:secret"] }
write = { allow = ["folder:Characters/Bob/**"], deny = ["*"] }
refactor = "none"
git = "deny"
```

**Default behavior:** The CLI with no `--permissions` flag uses a built-in `unrestricted` profile where everything is `all`/`allow`/`unlimited`. This preserves the current behavior — no permission checks on normal human use. The permission guard is always present in the call chain but the unrestricted implementation is a no-op (zero overhead on the hot path).

**Backward compatibility with JS sandbox:**

The `--sandbox` flag remains as a convenience alias that maps to permission profiles:
- `strict` → `{ read: "all", write: "none", refactor: "none", network: "deny", shell: "deny", cpu_limit_ms: 30000, memory_limit_mb: 64 }`
- `fs` → strict + `write: "all"`, `refactor: "all"`
- `net` → fs + `network: "allow"`
- `none` → `unrestricted`

If both `--sandbox` and `--permissions` are specified, the more restrictive of the two wins per dimension.

**Application points:**

- [x] **CLI global flag:** `--permissions <profile>` — activate a named profile for this invocation
- [x] **`index serve` / MCP server:** `--permissions <profile>` — all requests gated by this profile. Auth-token remains for authentication; permissions for authorization.
- [x] **JS runtime:** permission guard is threaded into the runtime context. All vault API calls (`vault.note()`, `vault.set()`, `web.fetch()`, etc.) check the active guard before executing. Resource limits (CPU, memory, stack) are applied via rquickjs `Runtime::set_memory_limit()`, `set_max_stack_size()`, and `set_interrupt_handler()`.
- [x] **Plugin system (9.19.12):** each plugin declares required permissions in its manifest. Execution is denied if requirements exceed the active profile. Plugins can request a *subset* of the active profile's permissions.
- [-] **AI integrations (9.12):** moved to 9.12.1 and 9.12.2 now that the assistant path is an external-runtime/tool-contract layer rather than an in-process permission consumer.
- [x] **Phase 17 integration point:** Phase 17 implements `PermissionGuard` with a capability-aware guard that resolves authenticated subject → applicable rooted and attenuated grants → `PermissionGrant`. The guard/filter boundary and resource specifiers from 9.19.13 are reused; Phase 17 may add finer create/delete and authority-administration dimensions while preserving existing profile semantics.

**Implementation:**

Core types in `vulcan-core`:

- [x] **`ResourceSpecifier`** enum: `Folder(GlobPattern)`, `Tag(String)`, `Note(String)`, `All`. Shared by 9.19.13 profiles and Phase 17 capability grants.
- [x] **`PathPermission`** struct: `allow: Vec<ResourceSpecifier>`, `deny: Vec<ResourceSpecifier>`. Deny-wins-on-conflict semantics.
- [x] **`PermissionGrant`** struct: all dimensions (read, write, refactor as `PathPermission`; git, network, index, config, execute, shell as capability flags; network domain allowlist; CPU/memory/stack limits). This is the **resolved** permission set — it has no concept of users or roles.
- [x] **`PermissionGuard`** trait: `check_read(path) -> Result<()>`, `check_write(path) -> Result<()>`, `check_refactor(path) -> Result<()>`, `check_network(domain) -> Result<()>`, `check_git() -> Result<()>`, `check_shell() -> Result<()>`, `resource_limits() -> ResourceLimits`. Two implementations:
  - `ProfilePermissionGuard` (9.19.13): resolves from a static `PermissionGrant` loaded from config
  - `CapabilityPermissionGuard` (Phase 17): resolves applicable rooted grants, delegation constraints, and canonical policy ceilings for an authenticated subject → `PermissionGrant`
- [x] **`PermissionFilter`** struct: takes a `&dyn PermissionGuard`, generates a set of allowed/denied paths. Provides `fn sql_cte() -> String` for filtered queries and `fn is_allowed(path) -> bool` for single-path checks. Phase 17.3 reuses this identity-neutral filter; it does not need to know whether the guard is profile- or capability-derived.

Integration:

- [x] Thread `&dyn PermissionGuard` through the command dispatch layer — every command handler receives a reference
- [x] Gate all file read operations in vulcan-core through `check_read()`
- [x] Gate all file write/create/delete operations through `check_write()`
- [x] Gate refactor operations through `check_refactor()` (checks all affected paths)
- [x] Gate JS runtime API functions through the guard (replace current `sandbox_allows_fs`/`sandbox_allows_network` boolean checks)
- [x] Apply resource limits from the guard to rquickjs runtime configuration
- [x] Map `JsRuntimeSandbox` enum to `PermissionGrant`; keep `--sandbox` as convenience alias
- [x] Build `PermissionFilter` from guard for query functions (search, notes, graph, vectors) — returns all results when unrestricted, filters when restricted. Phase 17 reuses this without changes.
- [x] Add `[permissions]` section to `.vulcan/config.toml` with named profiles
- [x] `vulcan config show permissions` to inspect active and available profiles
- [x] Clear error messages on denial: `"permission denied: write to 'Archive/old-note.md' not allowed by profile 'agent' (write allows: folder:Projects/**, folder:Journal/**, folder:AI/**)"`
- [x] Unit tests: each dimension, resource specifier matching (folder globs, tags, note paths), deny-overrides-allow, domain allowlist, resource limit application
- [x] Integration tests: CLI with `--permissions readonly` rejects writes, serve API respects profile, JS runtime obeys resource limits from profile, `PermissionFilter` correctly restricts query results

**Design note: why not Cedar/Casbin/JS?**

A general-purpose policy engine (Cedar, Casbin) or the JS VM was considered and rejected. The critical requirement is **SQL CTE generation** for filtered queries (`PermissionFilter` in Phase 17.3) — every search, graph query, and vector search must restrict results via SQL predicates, not post-hoc filtering. Cedar/Casbin evaluate point checks opaquely (`is_authorized() → bool`); translating arbitrary policies to SQL `WHERE` clauses is fragile or impossible. With our custom `PermissionGrant` struct, both point checks and SQL generation derive trivially from the same data: `ResourceSpecifier::Folder(glob)` maps to `glob_match()` in Rust and `path GLOB ?` in SQL. One data structure, two derivations, consistency by construction. JS was rejected because: policy eval in the sandboxed VM is architecturally circular, JS functions can't generate SQL, and per-check JS eval is too slow for the hot path.

**Policy hooks (advanced escape hatch)**

For the rare case where static config rules are insufficient, a policy hook provides custom authorization logic. This is separate from the event plugin hooks (9.19.12) — it runs at a lower level during permission evaluation, not as a side effect of vault operations.

- [x] Optional `policy_hook` field in permission profiles pointing to a JS file in `.vulcan/plugins/`
- [x] Hook receives `{ principal, action, resource, profile_decision }` — the profile's allow/deny decision is already resolved before the hook runs
- [x] Hook can return `"deny"` (with reason) or `"pass"` (accept the profile's decision). **Cannot return `"allow"`** — hooks can only narrow permissions, never widen them. This prevents a compromised hook from bypassing restrictions.
- [x] Hook runs in a restricted JS context: read-only vault access, no network, no recursion into permission checks, short timeout (100ms). Uses its own `PermissionGrant` that is hardcoded to read-only + no-network + no-shell.
- [x] Only executes in trusted vaults (9.19.2 trust model)
- [x] Hook failures (timeout, error) are treated as `"deny"` — fail-closed
- [x] Example use case: a GM's campaign wiki hook that denies access to notes containing `[!secret gm]` callouts for non-GM users, before Phase 17's document-level secrets (17.4) is implemented
- [x] Performance: hooks are opt-in per profile. The default `unrestricted` profile has no hook. When present, the hook is called only after the static rules produce `"allow"` — denied requests never reach the hook.

#### 9.19.14 Binary size analysis

The current Linux x86\_64 release binary is about 31.3MB unstripped and 26.0MB stripped. This is acceptable given the portability goal, but worth understanding and trimming where the wins are low-risk.

- [x] Inspect the release binary using `cargo tree`, `size`, `strip`, and built archive sizes (local environment did not have `cargo-bloat`)
- [x] Fix `js_runtime` feature propagation so `cargo build --release -p vulcan-cli --no-default-features` actually removes QuickJS from the CLI binary
- [x] Narrow `zip` features to `deflate` now that exports only use stored and deflated entries
- [x] Document findings in `docs/performance.md`

#### 9.19.15 MCP protocol-native rework

**Goal:** Turn the initial `vulcan mcp` stdio wrapper into a protocol-native MCP server that works well for generic MCP clients, not only subprocess-style harnesses that already know Vulcan's tool and skill layout.

**Depends on:** 9.12.6 (vault-native prompts and skills), 9.18.7 (stable `describe`/`help` docs), 9.19.6 (basic stdio MCP server), and 9.19.13 (permission layer). The Streamable HTTP transport should share contracts with Phase 10's eventual axum daemon/router work rather than forcing a second MCP redesign later.

**Design principle:** MCP is not just "the CLI schema over JSON-RPC". In the subprocess/CLI case the host can preload `AGENTS.md`, prompt files, and skill summaries. Generic MCP clients usually cannot. Vulcan therefore needs its own protocol-native discovery and progressive-disclosure surface.

**Status:** Complete for Phase 9. Implemented in `vulcan-cli/src/mcp.rs` with a native `2025-06-18` MCP server over stdio and Streamable HTTP, curated headless tool packs, structured tool results, request timeout handling, vault-native prompts/resources/completions, change notifications, first-class task tools, and shared registry/export plumbing via `describe --format mcp`. Remaining `[-]` entries are intentional follow-ons: preserve the contract when Phase 10 daemon routing arrives, and defer MCP Apps until there is a concrete host/UI flow to target.

**Protocol baseline**

- [x] Upgrade the MCP server baseline to **protocolVersion `2025-06-18`** rather than staying on `2024-11-05`
- [x] Advertise protocol-native capabilities instead of only `{ tools: {} }`: `tools`, `resources`, `prompts`, and `completions`, with `listChanged` where supported
- [x] Keep **stdio** as the local-process/default transport for Phase 9
- [x] Define an internal transport-agnostic MCP server core (registry + dispatcher + serializers) so stdio and later HTTP transports share the same behavior
- [x] Treat `2025-11-25` `tasks` as explicitly out of scope for this sub-phase; do not upgrade solely to pick up experimental task primitives

**Tool surface curation**

- [x] Replace "all visible CLI leaf commands become MCP tools" with an explicit MCP tool registry
- [x] Curate a **headless-only** default MCP tool surface: hide TUI/editor/desktop-launch/server-management commands such as `browse`, `edit`, `open`, `bases_tui`, `config edit`, nested `mcp`, and similar interactive affordances
- [x] Define server-side MCP tool packs or exposure modes such as `core`, `extended`, and `admin`, with `core` as the default for generic clients
- [x] Ensure MCP tool exposure composes with permission profiles rather than bypassing them
- [x] Decide how `vulcan describe --format mcp` maps to the curated surface so CLI schema export and live MCP exposure do not drift silently

**Tool metadata and structured results**

- [x] Add MCP tool metadata beyond `name`/`description`/`inputSchema`: `title` plus `annotations` such as `readOnlyHint`, `destructiveHint`, `idempotentHint`, and `openWorldHint`
- [x] Add `outputSchema` for tools whose JSON result shape is stable enough to declare
- [x] Return `structuredContent` plus a text fallback for tool calls instead of wrapping raw CLI JSON stdout as opaque text
- [x] Distinguish JSON-RPC protocol errors (unknown tool, invalid params, server error) from tool execution failures returned with `isError: true`
- [x] For very large results, support summarized text plus embedded resource references rather than forcing the entire payload into one text blob

**Vault-native prompts**

- [x] Implement a shared prompt loader over `assistant.prompts_folder` from `.vulcan/config.toml`
- [x] Expose prompt files from that configured vault folder through MCP `prompts/list` + `prompts/get`
- [x] Support a Markdown + frontmatter prompt format that can declare prompt `name`, `description`, `arguments`, `version`, and tags without introducing a second prompt store
- [x] Reuse the same prompt loader for subprocess-runtime helpers and MCP prompt exposure
- [x] Allow `vulcan init --agent-files` / `vulcan agent install` to scaffold example prompt files into the configured prompts folder
- [x] Emit `notifications/prompts/list_changed` when the available vault prompt set changes

**Resources and reference material**

- [x] Expose protocol-native reference material over `resources/list` + `resources/read`: command docs/help, vault `AGENTS.md`, assistant config summaries, skill indexes, and skill content
- [x] Use MCP resource templates where appropriate instead of trying to enumerate every high-cardinality item eagerly
- [x] Add stable resource URIs for machine-readable command docs and assistant material, e.g. command help by command path and skill content by skill name
- [x] Emit `notifications/resources/list_changed` when relevant vault assistant files or config-backed docs change
- [x] Treat resources as the MCP replacement for out-of-band injected skill/reference text in generic clients

**Completion and progressive disclosure**

- [x] Implement MCP `completion/complete` for prompt arguments and resource-template arguments
- [x] Reuse the existing dynamic completion engine where practical: note names, prompt names, skill names, command paths, bases views, task views, and periodic dates
- [x] Use completions plus prompt/resource discovery to keep the default MCP surface compact while still making advanced capability discoverable on demand
- [x] Ensure completion responses respect permission profiles and avoid leaking hidden or unauthorized names

**Dispatch and performance**

- [x] Replace per-tool subprocess respawning with direct in-process dispatch to the same command handlers/serializers used by the CLI
- [x] Preserve CLI/MCP parity for refresh behavior, permission checks, and JSON report structs while removing process spawn overhead
- [x] Keep a single source of truth for MCP permission requirements, annotations, and output schemas so the registry does not drift from command behavior
- [x] Add cancellation/timeout handling where practical so long-running MCP calls fail predictably rather than hanging the client

**HTTP transport follow-through**

- [x] Prepare the MCP core so that Phase 10's axum daemon can expose the same registry over **Streamable HTTP** without redesigning the feature surface
- [x] Implement Streamable HTTP transport on `vulcan mcp --transport http` with MCP sessions, SSE notifications, and the same protocol-native registry exposed over stdio
- [x] Keep HTTP transport bound to loopback by default; require `--auth-token` for non-loopback binds while still enforcing the selected Vulcan permission profile on every request
- [x] Reuse the shared note outline/read/patch/search-hit contracts for MCP HTTP responses instead of inventing a separate paging or chunk-follow-up model
- [-] When Phase 10 replaces the current single-vault listener with daemon routing/middleware, preserve this MCP contract instead of redefining transport semantics again

**Explicit deferrals**

- [x] Expose MCP `tasks` through the curated tasks tool pack now that daily-driver MCP workflows need first-class task semantics
- [-] Defer MCP Apps integration until there is a concrete host/UI flow to target; do not add app-specific surface area speculatively

**Testing**

- [x] Add end-to-end MCP integration tests for initialize/capability negotiation, `tools/*`, `resources/*`, `prompts/*`, and `completion/complete`
- [x] Add regression tests for permission-filtered tool/resource/prompt visibility and denial paths
- [x] Add fixture-driven tests for vault prompt loading from `assistant.prompts_folder` and prompt change notifications
- [x] Add parity tests asserting that structured MCP outputs match the corresponding CLI `--output json` report shapes
- [x] Add tests covering curated tool exposure so interactive-only commands cannot accidentally leak back into the MCP surface
- [x] Add end-to-end Streamable HTTP tests for session bootstrap, SSE list-change notifications, and auth-token enforcement

#### 9.19.16 Integration hardening and fuzzing

**Goal:** Make the application difficult to break by expanding automated coverage far beyond per-command happy paths. This phase focuses on end-to-end integration flows, edge cases, regression harnesses, and parser/query fuzzing so later phases inherit a stable base rather than stacking on brittle behavior.

**Depends on:** 9.19.6 (full CLI surface), 9.19.7 (command reorg stabilized), 9.19.13 (permission layer), and ideally 9.19.12 once plugin flows exist. Work can start earlier, but this phase should be treated as the final hardening pass before Phase 9.20 and Phase 10 become the main focus. If 9.19.15 lands before later platform work, extend this harness to cover the protocol-native MCP surface as part of normal regression expansion.

**Principle:** Prefer tests that reflect how real users and external runtimes actually drive Vulcan:

- Full command sequences, not isolated function calls
- Cross-feature flows, not single-module correctness only
- Failure-mode assertions, not just success snapshots
- Deterministic fixtures first, then synthetic stress/property/fuzz coverage where parser surfaces justify it

**Coverage targets**

- [x] Add a dedicated `tests/fixtures/vaults/polish/` or `hardening/` vault that combines the major feature families in one place: Dataview, Tasks, Kanban, TaskNotes, Periodic notes, templates, saved reports, permissions, aliases, ambiguous links, attachments, extracted text, and malformed edge cases
- [x] Add end-to-end CLI flow tests that cover realistic workflows: `init -> scan -> query/search -> note mutate -> refactor -> export`, including rescans and idempotent reruns
- [x] Add regression suites for uninitialized and partially initialized vaults: missing cache, missing `.vulcan/`, malformed config, stale derived indexes, and mixed tracked/untracked git state
- [x] Expand permission-profile integration coverage across CLI, serve, MCP, JS runtime, and future plugin boundaries: read/write/refactor/git/network/config/execute denials plus filtered-result assertions
- [x] Add cross-feature integration tests for the main combinations users actually rely on:
  - Dataview and DQL over Periodic notes, TaskNotes, Tasks, and Bases
  - refactors followed by search/query/graph validation
  - template rendering combined with QuickAdd variables, daily note creation, and note mutations
  - vector indexing/search with permission filters and cache rebuild/repair paths
  - exports after refactors, permissions, and filtered queries
- [x] Add refresh/watch/serve stability tests: repeated requests, background refresh interactions, cache rebuild after file churn, and request behavior while scans are in flight
- [x] Add broader output-contract tests for `--output json`, line-delimited JSON, markdown, CSV/TSV, and human output where users depend on exact machine-readable fields or stable semantics
- [x] Add migration and repair hardening tests: open old schemas, run migrations, rebuild derived state, verify no data leaks or orphaned rows, and assert rebuild-idempotency across all major cache-backed tables
- [x] Add synthetic large-vault integration tests or stress harnesses for performance-sensitive paths that have already regressed once: graph queries, search, vectors, note loading, and multi-feature scans

**Property-based and fuzz testing**

- [x] Introduce property-based tests where invariants are clear and deterministic:
  - path normalization and round-tripping
  - move/rewrite round trips
  - query AST parse/serialize/parse stability
  - config merge precedence invariants
  - permission filter allow/deny precedence
- [x] Add parser/query fuzz targets using `cargo fuzz` or an equivalent harness for the most exposed text surfaces:
  - Markdown/document parser
  - DQL tokenizer/parser
  - expression parser
  - Tasks query parser
  - config/TOML ingestion where malformed input should never panic
- [x] Make fuzzing outputs actionable: minimized crashing inputs should be checked into regression fixtures or unit tests immediately after triage
- [x] Document how to run the fuzz/property suites locally and in CI, including which jobs are required on every PR vs. nightly/periodic hardening runs

**Exit criteria**

- [x] Every critical command family has at least one realistic multi-step integration flow test, not just isolated output snapshots
- [x] Every parser or text-ingestion surface that handles untrusted or user-authored input has either dedicated fuzz/property coverage or a documented reason it does not
- [x] Previously fixed regressions are captured as permanent regression tests before the phase is considered complete
- [x] CI coverage is intentionally layered: fast required tests on every change, heavier integration/fuzz/stress suites on scheduled or opt-in jobs

#### 9.19.17 Config surface completion and schema-driven settings UX

**Goal:** Close the remaining gap between the rich `VaultConfig` / permission-profile model and the config-management UX. Every supported setting should be manageable through stable non-interactive CLI commands, while the TUI becomes a discoverable layer over the same schema-driven mutation engine. Manual TOML editing remains supported for power users, but it should no longer be the only practical way to create new aliases, permission profiles, plugin registrations, local overrides, or optional config sections.

**Depends on:** 9.17 (import infrastructure and merged config handling), 9.19.11 (initial settings TUI), 9.19.12 (plugin registration model), and 9.19.13 (permission profiles and guard model). This work should also preserve the dedicated export-profile commands rather than collapsing them back into raw dot-path mutation.

**Why this follow-on exists:** The current surface is good at reading config and editing already-discovered leaves, but still has several structural gaps:

- `config show` / `config get` expose the effective merged config broadly, but `config set` still relies on paths that already exist in the effective config tree
- New named map entries such as `aliases.<name>`, `permissions.profiles.<name>`, and most plugin-registration fields still require manual TOML edits or one-off helper commands
- `config edit` builds its entry list from discovered leaf paths, so empty optional sections and add/remove flows are not discoverable
- Shared vs. local overrides are visible, but generic mutation commands do not yet give full control over which file is being edited
- The default config template is currently the closest thing to a full reference; help/docs are still partly hand-maintained instead of derived from one canonical config schema

**Implementation plan**

**1. Shared config schema / descriptor layer**

- [x] Add a reusable config-descriptor registry in `vulcan-app` that is the single source of truth for editable config surface area. It should not live in `vulcan-cli`.
- [x] Each descriptor should define at least: logical path, storage path, section/category, value kind (scalar / enum / array / object / named map entry), target-file support (`shared`, `local`, or both), description/help text, examples, default behavior, validation hook, and whether the path is creatable when absent.
- [x] Cover every stable `VaultConfig` leaf plus dynamic config families that are user-facing and intended to be managed without raw TOML editing:
  - `aliases.*`
  - `plugins.*`
  - `permissions.profiles.*`
  - `export.profiles.*` for discovery/docs/TUI parity, while keeping the dedicated export-profile commands as the preferred mutation surface
- [x] Add a descriptor-completeness test so new config fields cannot land without metadata for CLI/TUI/docs.

**2. Generic non-interactive config CRUD**

- [x] Extend `vulcan config set` to support `--target shared|local` so generic mutations can write either `.vulcan/config.toml` or `.vulcan/config.local.toml`, matching import behavior.
- [x] Add `vulcan config unset <key> [--target ...]` to remove one override and prune empty tables safely.
- [x] Make `config set` capable of creating absent-but-supported optional sections and leaves when the descriptor marks them as creatable, for example:
  - `embedding.*`
  - `extraction.*`
  - `quickadd.template_folder`
  - `web.search.*`
- [x] Keep rejecting unknown or schema-less keys so the command remains a supported config contract rather than a raw TOML writer with dotted paths.
- [x] Add a discovery command such as `vulcan config list [section]` that shows known keys with type, description, mutability, default, and whether the effective value comes from defaults, Obsidian import, shared config, or local config.

**3. Dedicated commands for named config collections**

- [x] Add first-class CLI commands for aliases rather than requiring users to know raw map keys:
  - `vulcan config alias list`
  - `vulcan config alias set <name> <expansion> [--target ...]`
  - `vulcan config alias delete <name> [--target ...]`
- [x] Add first-class CLI commands for permission profiles rather than requiring manual creation of `[permissions.profiles.<name>]`:
  - `vulcan config permissions profile list`
  - `vulcan config permissions profile show <name>`
  - `vulcan config permissions profile create <name> [--clone <base>] [--target ...]`
  - `vulcan config permissions profile set <name> <dimension> <value> [--target ...]`
  - `vulcan config permissions profile delete <name> [--target ...]`
- [x] Expand the plugin config surface so users can manage the full registration without hand-editing TOML:
  - register/set path
  - enable/disable
  - add/remove subscribed events
  - set sandbox
  - set permission profile
  - set description
- [x] Route these dedicated commands through the same shared config-mutation layer as generic `config set/unset` so behavior, validation, target handling, and docs stay consistent.

**4. Settings TUI v2 on top of the same schema**

- [x] Rework `vulcan config edit` so it is schema-driven rather than leaf-discovery-driven. Every supported category should appear even when unset.
- [x] Add an explicit target toggle (`shared` vs `local`) in the TUI and make the precedence model visible in the detail pane.
- [x] Add create/remove flows for dynamic collections:
  - new alias
  - new permission profile
  - new plugin registration
  - existing export profile discovery/edit handoff
- [x] Add richer editors for arrays/maps/enum values so users do not need to type raw TOML literals for common operations such as adding an event hook or appending an allowlist domain.
- [x] Add structured row editors for Templater folder, regex, and ignored-folder creation rules, with bounded existing-vault folder suggestions that never prevent manual path entry.
- [x] Show value provenance in the UI (`default`, `Obsidian import`, `shared override`, `local override`) so users understand why an effective value looks the way it does.
- [x] Reuse the same descriptors, validators, and examples as the non-interactive CLI and help system. The TUI must not grow its own private config schema.

**5. Documentation and help as a first-class config surface**

- [x] Generate or derive config reference docs from the descriptor registry so `help config` and the docs stop drifting from the actual supported mutation surface.
- [x] Expand `vulcan help config` and related docs to include:
  - key path
  - type / enum values
  - default
  - shared vs local guidance
  - examples
  - whether the setting is better handled by a dedicated command
- [x] Add explicit manual-editing guidance for power users: precedence rules, when to prefer `.vulcan/config.local.toml`, and example TOML blocks for aliases, permission profiles, plugin registrations, and imported plugin sections.
- [x] Keep the default config template, but treat it as a convenience sample rather than the sole exhaustive reference.

**6. Tests**

- [x] Unit tests for descriptor parsing, validation hooks, default rendering, and target-file eligibility.
- [x] Integration tests for generic `config set/unset` covering creation of previously-absent optional sections and leaves.
- [x] Integration tests for named collection workflows:
  - creating `aliases.ship`
  - creating and mutating `permissions.profiles.agent`
  - registering/configuring `plugins.lint`
  - editing shared vs local overrides and verifying precedence
- [x] TUI state-machine tests for creation, deletion, and editing of schema-defined entries that were previously absent from the config file.
- [x] Snapshot tests for generated config help/reference output so docs and CLI descriptors stay aligned.
- [x] Regression tests asserting that dedicated commands (`plugin ...`, permission-profile commands, export profile commands) round-trip through the same config mutation layer instead of reimplementing divergent TOML edits.

**Exit criteria**

- [x] Every supported user-facing config family is manageable via either generic `config` CRUD or a dedicated command that uses the same underlying descriptor/mutation layer.
- [x] Creating a new alias, permission profile, plugin registration, local override, or optional config section no longer requires manual TOML editing.
- [x] The TUI can discover and create supported settings even when the relevant section is absent from both shared and local config files.
- [x] `help config` and the docs reflect the actual supported config surface from generated metadata, not hand-maintained lists.

---

## Phase 9.20: Static Site Builder

**Goal:** Generate a polished, Obsidian-native static website directly from a vault, with profile-scoped publication rules, a shared HTML renderer, and a fast local preview loop. This phase is intentionally scheduled **before** daemon/WebUI work in the roadmap priority order so later phases reuse a proven rendering layer instead of inventing a second one.

**Depends on:** Phase 7 complete. Best started after Phase 9 is functionally complete, especially 9.8 (Dataview), 9.10 (Tasks), 9.11 (Kanban), 9.15 (TaskNotes), 9.16 (Periodic notes), 9.18.2/9.18.7 (note HTML/docs surface), and 9.19.6 (missing commands / CLI surface). DataviewJS static execution in 9.20.7 depends on 9.8.8 and 9.18.5.

**Design refs:** `docs/design_document.md` §10 (single search engine reused across CLI/HTTP/web), §12b inline expressions and DataviewJS rendering concerns, 9.19.2 (raw markdown / HTML access), 9.19.9 (`export html`), Phase 13 (WebUI browse), and Phase 16 (Wiki mode).

**Why this phase exists:** Quartz, Obsidian Publish, and similar tools prove there is demand for vault-native publishing, but Vulcan has an unusual advantage: the parser, graph cache, query AST, Dataview/Bases evaluator, task model, and link resolver already exist locally in Rust. A static builder exercises exactly the parts later WebUI/wiki phases need most — rendering, routing, page indexes, search/graph assets, and publish filtering — without first taking on daemon auth, multi-vault orchestration, or collaborative editing.

**Core rules**

- **CLI-first, daemon-independent.** `vulcan site build` and `vulcan site serve` must work against a local vault and SQLite cache without Phase 10 running.
- **One renderer, many surfaces.** Note HTML for `note get --mode html`, static site pages, and later WebUI/wiki pages must come from the same render pipeline and data structures.
- **Vault remains the source of truth.** The output directory is disposable. Rebuilds must be deterministic from vault contents + config.
- **Privacy by omission, never by hiding.** In static output, "private pages" means excluded at build time. Never emit hidden HTML/JSON and rely on client-side checks.
- **Subset publishing uses existing query/filter concepts.** Site profiles should reuse canonical query/filter machinery instead of inventing a separate publish DSL.
- **No required Node toolchain.** The default theme, renderer, and preview server should ship in the Rust binary. Optional downstream theming pipelines can exist later.
- **Separate frontend pipelines must consume Vulcan contracts, not replace Vulcan semantics.** If Astro/Next/Vite or other tools are used later, Vulcan should still own vault-aware parsing, querying, filtering, and rendered publication fragments instead of forcing downstream tools to reimplement Obsidian behavior.
- **Custom CSS and light/dark mode are baseline features, not post-launch polish.**
- **Static assets must respect publish filters.** Search indexes, graph JSON, hover-preview manifests, RSS feeds, and copied attachments must never leak excluded notes.

**Configuration sketch**

```toml
[site.profiles.public]
title = "My Notes"
base_url = "https://notes.example.com"
output_dir = ".vulcan/site/public"
home = "Home"
language = "en"
theme = "default"
include_query = 'from "Garden"'
exclude_folders = ["Templates/**", "Archive/**"]
exclude_tags = ["private", "draft"]
search = true
graph = true
backlinks = true
rss = true
extra_css = ["site/public.css"]
extra_js = ["site/public.js"]
favicon = "site/favicon.png"

[site.profiles.docs]
title = "Project Docs"
base_url = "https://docs.example.com"
output_dir = ".vulcan/site/docs"
include_query = 'from "Docs"'
theme = "default"
search = true
graph = false
```

#### Current implementation status (2026-04-30)

- `site build|serve|profiles|doctor` are in-tree with JSON output, deterministic route planning, folder/tag/recent/home/search/graph pages, route/search/graph/hover/recent/related manifests, RSS/sitemap emission, and publish-filter diagnostics.
- The builder reuses the same shared HTML renderer already used by `note get --mode html` and `render --mode html`; this currently covers inline Dataview expressions, `dataview` query blocks, `tasks` query blocks, `.base` embeds, note embeds, callouts, attachment rewriting, and DataviewJS off/static fallback behavior.
- The preview loop now exposes both JSON polling and SSE live-reload endpoints, surfaces publish diagnostics to the terminal/browser overlay, and tracks changed/deleted outputs so watch rebuilds only rewrite files whose bytes actually changed.
- The original Phase 9.20 baseline is complete: the built-in static site path and the external frontend-bundle path now share the same publication selection, transforms, route planning, manifests, asset copying, deterministic HTML fragments, and local live-reload/watch loop. Follow-up expansion can still broaden fixture libraries over time, but the shared publication contract intended for later WebUI/wiki reuse is now in place.
- Follow-up optimization (2026-05-06): site builds now persist per-profile note-render state under `.vulcan/site-state/` and reuse unchanged note pages when the profile config, published set, and planned routes remain stable. Watch/small-update rebuilds invalidate backlink/tag/folder/embed/query dependents conservatively, while aggregate pages and manifests are still regenerated from the current rendered-note set.
- Follow-up shell/theme overhaul (2026-05-06): the built-in site shell now uses explicit left/main/right regions, emits a published `assets/navigation-tree.json` explorer manifest, supports folder-note-aware navigation, `system` / `light` / `dark` palette selection, reader mode, persisted rail/module/folder state, and mirrored shell/navigation/module settings in the frontend-bundle contract.
- Follow-up shell/performance pass (2026-05-07): the default explorer now collapses folders while still exposing folder-note landing links, the built-in shell moved closer to Quartz's flatter rail/content proportions, published search indexes now reuse source-derived note text instead of reparsing rendered HTML, client prefix lookup is bounded and binary-search-based, and graph rendering uses a deterministic ring layout instead of the older quadratic force loop.
- Follow-up asset-copy pass (2026-05-07): copied site assets now persist per-profile source/output metadata under `.vulcan/site-assets/`, letting incremental rebuilds skip rereading unchanged large assets while still repairing drifted output copies when the published file no longer matches the last successful build.
- Follow-up page-output reuse pass (2026-05-07): no-op incremental site builds now persist per-note emitted page signatures in `.vulcan/site-state/` and skip regenerating unchanged note pages when both the cached render state and the on-disk published output still match, while continuing to repair drifted/missing page files automatically.
- Follow-up shell/theme expansion is tracked in 9.20.10. It builds on the completed 9.20 foundation rather than reopening the original renderer/publication exit criteria.

### 9.20.1 Shared render contract and CLI surface

This is the foundation. Do this before building site chrome, templates, or preview tooling.

- [x] Add `vulcan site build [--profile <name>] [--output-dir <path>] [--clean] [--dry-run] [--watch]`
- [x] Add `vulcan site serve [--profile <name>] [--output-dir <path>] [--port <n>] [--watch]`
- [x] Add `vulcan site profiles` (list available site profiles with effective settings)
- [x] Add `vulcan site doctor [--profile <name>]` for publish-specific diagnostics: unpublished link targets, slug collisions, unsupported embeds, missing assets, SEO metadata gaps
- [x] Land `vault.note(path).html` and `vulcan note get --mode html` on the same renderer used by site generation
- [x] Add HTML output to `vulcan render` so users can quickly convert markdown from stdin or files using the same shared render pipeline rather than a separate converter
- [x] Define shared render structs in Rust (`RenderContext`, `RenderedNote`, `RenderedEmbed`, `SiteRoute`, etc.) so CLI/site/WebUI reuse the same contracts
- [x] Define deterministic route/slug planning with diagnostics on collisions and stable defaults derived from note path/frontmatter
- [x] Add JSON output for `site build`, `site profiles`, and `site doctor` for automation/LLM use
- [x] Snapshot tests for single-note HTML rendering and route manifests

### 9.20.2 Site profiles and publication selection

Publishing a subset of the vault is a first-class requirement. Profiles are the mechanism.

- [x] Add `[site.profiles.<name>]` config section to `.vulcan/config.toml`
- [x] Support profile fields: `title`, `base_url`, `output_dir`, `home`, `language`, `theme`, `search`, `graph`, `backlinks`, `rss`, `favicon`, `logo`, `extra_css`, `extra_js`
- [x] Add a profile-scoped deploy path / site prefix setting distinct from `base_url` so generated sites can be hosted under subpaths such as `/wiki/` as well as at the domain root
- [x] Support inclusion/exclusion by canonical query AST, folder glob, explicit note path, tag, and frontmatter predicates
- [x] Support multiple profiles per vault so one vault can publish a public garden, project docs, and private local preview separately
- [x] Reuse export/publication `content_transforms`, link policy, and asset policy in site profiles so export, static site, and future web wiki publication all share the same audience-filtering model
- [x] Use the same rule semantics in site profiles: profile selection defines the published note set, while per-rule queries only target which published notes get transformed
- [x] Add per-profile slug/frontmatter overrides: title, description, canonical URL, summary image, custom slug
- [x] Add link policy for references that point outside the published subset: `error`, `warn`, `drop-link`, or `render-plain-text`
- [x] Add attachment policy: copy only referenced assets, copy whole folders, or error on missing references
- [x] "Private pages" in static mode are implemented as exclusion rules only; document this constraint explicitly in help and config docs
- [x] Config tests for precedence, profile parsing, subset selection, and publish-leak prevention

### 9.20.3 Obsidian-native HTML renderer

The renderer should understand vault semantics, not just CommonMark.

- [x] Render Markdown to HTML with Obsidian-compatible support for wikilinks, heading/block refs, note embeds, image/audio/video/PDF embeds, footnotes, callouts, task lists, tables, and syntax highlighting
- [x] Render math and mermaid with clear server/client responsibilities; the shared renderer now emits stable math/mermaid markers, the built-in shell auto-enhances them when KaTeX/Mermaid runtimes are present, and the static-site docs spell out the server/runtime contract
- [x] Generate stable heading IDs and block anchors for deep links and embeds
- [x] Render note/block embeds recursively with loop detection and depth limits
- [x] Copy referenced attachments into the output with deterministic paths; optionally content-hash emitted asset filenames
- [x] Add configurable raw HTML policy: passthrough, sanitize, or strip with diagnostics
- [x] Generate per-page metadata from existing indexes: title, excerpt/summary, tags, aliases, outgoing links, backlinks, breadcrumbs, heading tree, created/modified dates
- [x] Preserve unsupported syntax as visible diagnostics or fallback blocks rather than silently dropping content
- [x] Add snapshot/integration tests against fixture vaults covering embeds, block refs, math, mermaid, callouts, and attachment rewriting

### 9.20.4 Site generation, theme system, and default UX

This sub-phase turns rendered notes into a coherent website rather than a folder of HTML fragments.

- [x] Generate note pages, home page, folder listings, tag listings, recent-notes page, and optional archive page
- [x] Add TOC, breadcrumbs, backlinks, and previous/next navigation using existing graph and path metadata
- [x] Ship a responsive default theme implemented with plain CSS and minimal JS; no SPA router required for the first usable version
- [x] Use CSS custom properties for the theme token system so customization stays simple and stable
- [x] Support light/dark mode out of the box: `prefers-color-scheme` by default plus a manual toggle persisted in browser storage
- [x] Support profile-scoped custom CSS as a first-class feature (`extra_css`) and optional profile-scoped custom JS (`extra_js`)
- [x] Support favicon/logo injection
- [x] Add custom page title templates
- [x] Add a simple modular theming contract with a small fixed set of overridable shell regions/partials (for example head, header, nav, footer, note chrome) and stable data inputs, without introducing a full general-purpose template language or a required Node stack
- [x] Treat theme tokens, major CSS class hooks, and overridable shell regions as a documented compatibility surface; ship a reference custom-theme example and keep docs for users/integrators current as the shell evolves
- [x] Implement SEO basics: canonical URLs, sitemap.xml, RSS/Atom feed, OpenGraph/Twitter metadata, social preview fallbacks
- [x] Make generated navigation, note routes, asset URLs, client-side manifest fetches, RSS links, and canonical metadata prefix-aware so built output works unchanged behind reverse-proxy subpaths
- [x] Accessibility budget: ensure the default theme is keyboard-navigable, mobile-friendly, and screen-reader-friendly; add snapshot or smoke tests for landmarks/heading structure
- [-] `vulcan export html` remains superseded by `site build`; do not reintroduce a parallel renderer/template stack unless a later phase revives that dedicated command surface

### 9.20.5 Client-side search, graph assets, and hover previews

These features differentiate the site from a plain markdown-to-HTML export and directly reuse existing Vulcan data structures.

- [x] Generate a static client-side search index from chunks/search metadata with note titles, headings, excerpts, tags, and URLs
- [x] Provide a default search UI with keyboard shortcut, result highlighting, and mobile-friendly behavior
- [x] Emit graph JSON using the resolved note graph plus per-page local neighborhoods for a local graph view
- [x] Add a global graph page using the same JSON asset schema later reusable by WebUI
- [x] Add a per-page local graph component using the same JSON asset schema later reusable by WebUI
- [x] Generate a hover-preview/popover manifest with title, excerpt, URL, and optional heading outline so links can show Wikipedia-style previews
- [x] Generate recent-notes and related-notes manifests from existing metadata/graph data where useful
- [x] Ensure publish filters apply uniformly: excluded notes must not appear in search indexes, graph JSON, preview manifests, feeds, or copied assets
- [x] Add regression tests proving excluded/draft/private notes cannot leak through any generated static asset

### 9.20.6 Local preview server and incremental rebuilds

This is a site-development loop, not a replacement for the daemon.

- [x] `vulcan site serve --watch` serves the generated site from a lightweight local HTTP server
- [x] Watch vault files, `.vulcan/config.toml`, profile CSS/JS assets, and theme/template files; rebuild incrementally when inputs change
- [x] Rebuild only affected pages/indices/assets where possible using the existing incremental scan and dependency information
- [x] Browser live reload via a local polling endpoint plus in-browser reload/error overlay; keep this local and simple rather than reusing Phase 10 routing/auth
- [x] Upgrade live reload transport to SSE or WebSocket if polling proves insufficient
- [x] Clear diagnostics in the terminal and optional in-browser overlay for broken links, unsupported embeds, render failures, or leaked/private pages
- [x] Add `--fail-on-warning` / `--strict` mode for CI-style preview checks
- [x] Integration tests for build → serve → modify source → incremental rebuild → updated output
- [x] Make `site serve` preview routing and live-reload endpoints prefix-aware when a profile deploy path is configured, while still supporting root-hosted loopback previews by default

### 9.20.7 Dataview, Bases, Tasks, and advanced read-only surfaces

Vulcan should compete on Obsidian-native semantics here, not just theming.

- [x] Render inline Dataview expressions in note pages using the same evaluator as CLI/WebUI
- [x] Render DQL query blocks to static HTML tables/lists/task views when evaluation is deterministic
- [x] Render Bases views to static tables/cards using the canonical query AST and existing Bases evaluator; `.base` embeds now route through the shared renderer with regression coverage on fixture content
- [x] Render Tasks plugin query blocks in read-only HTML via the shared renderer; TaskNotes, Kanban, and periodic-note fixture content now stays covered by the shared publication path
- [x] Add explicit DataviewJS publish policy: default `off`; optional `static` mode behind `js_runtime` feature flag and profile opt-in
- [x] In DataviewJS `static` mode, enforce determinism constraints: no network, no wall-clock dependence, no filesystem writes, and clear diagnostics on unsupported behavior
- [x] Unsupported or disabled DataviewJS blocks should render visible fallback output with diagnostics rather than disappearing silently
- [x] Document what is intentionally deferred from the first static-site release: comments, analytics integrations, stacked pages, SPA routing, full browser-side DataviewJS parity, and any "private page" mechanism that depends on runtime auth
- [x] Integration tests on fixture vaults containing Dataview, Bases, Tasks, TaskNotes, Kanban, and periodic-note content

### 9.20.8 Testing, determinism, and later-phase reuse

This phase is only worth doing early if later phases can build on it directly.

- [x] Build-twice determinism test: same vault + same config must produce byte-identical output (modulo intentional timestamps in feeds, which should be normalized in tests)
- [x] Multi-profile tests: one vault builds multiple profiles with different subsets/themes without asset leakage between outputs
- [x] Publish-subset leak tests: excluded notes cannot appear in HTML, JSON manifests, feeds, copied assets, or hover previews
- [x] Add regression tests for root-hosted and subpath-hosted builds so nav links, asset URLs, manifests, feeds, and preview/live-reload paths stay correct under both deployment models
- [x] HTML snapshot tests for representative pages and fixture vaults
- [x] Document the shared renderer/output contracts reused by Phase 13 note pages and Phase 16 wiki mode
- [x] Add explicit cross-reference notes in later phases: WebUI and wiki features must reuse this renderer/search/graph asset model unless a documented reason requires divergence

### 9.20.9 External frontend bundle mode and integration contract

This is additive, not a replacement for `site build`. The goal is to let dedicated frontend tools own layout/styling/deployment while Vulcan stays the source of truth for vault-aware publication semantics.

- [x] Extract a shared publication pipeline from `site build` and `export` so note selection, content transforms, route planning, asset planning, diagnostics, and manifest generation are reusable across built-in and external publication modes
- [x] Add a separate frontend-oriented publication mode such as `web_bundle` / `frontend_bundle`, preferably integrated with export/publication profiles rather than as a second site-only configuration system
- [x] Keep publication controls shared across `site` and export/bundle modes: publish subset selection, content transforms, link policy, asset policy, route policy, and deploy-path/prefix semantics should not drift by surface
- [x] Emit a versioned, typed bundle contract with per-note metadata, rendered `body_html` fragments, route information, headings, backlinks/outgoing links, diagnostics, and site-level manifests/assets so downstream tools do not need to reimplement wikilinks, embeds, Dataview, Tasks, or attachment rewriting
- [x] Generate machine-consumable integration artifacts for downstream builders such as JSON Schema and/or TypeScript type definitions, plus a reference example bundle checked into tests/docs
- [x] Add watch/dev-preview support for external frontend pipelines: bundle rebuilds on change, changed-route/asset invalidation manifests, and a simple local SSE or similar event stream that frontend dev servers can subscribe to for HMR/live reload
- [x] Preserve parity with `site build`: search/graph/hover/recent/related manifests, publish diagnostics, and deterministic route planning should be shared outputs, not reimplemented separately for external consumers
- [x] Keep the built-in static site builder as the default/reference implementation so Vulcan still ships a no-Node publishing path and a concrete compatibility oracle for external integrations
- [x] Maintain extensive, versioned, up-to-date docs for both users and integrators covering config, bundle layout, schema/types, live-preview workflow, deployment patterns, upgrade notes, and compatibility guarantees; treat stale docs/examples as a release-blocking regression for this surface
- [x] Add integration tests covering bundle determinism, schema stability, root-hosted vs subpath-hosted path correctness, and parity with representative `site build` output for notes/manifests/assets

### 9.20.10 Static site shell, navigation, and theme overhaul

This is a follow-on UX/theming expansion on top of the completed 9.20 publication contract. The goal is to make the built-in site output feel closer to Quartz and MkDocs Material without abandoning Vulcan's no-Node default path or introducing a second renderer/template stack.

**Goal:** Replace the current "header + content + one sidebar" shell with a Quartz-style knowledge-site layout: persistent left navigation/search rail, centered reading surface, contextual right rail, folder-note-aware explorer, richer theme controls, and per-module visibility/state management.

**Builds on:** Completed 9.20 baseline, especially 9.20.1 shared render structs, 9.20.2 site profiles, 9.20.4 theme/default UX, 9.20.5 search/graph manifests, 9.20.6 incremental rebuilds, and 9.20.9 frontend bundle contracts.

**Why this follow-on exists:** The current 9.20 output proves the renderer/publication pipeline, but the shell contract is still too small and the default UX is not yet competitive with Quartz, Obsidian-like knowledge gardens, or Material-style documentation sites. This follow-on is about site information architecture, shell state, and theme/template ergonomics, not about redoing parsing, route planning, or publish semantics.

**Current landing (2026-05-06):** Shell contract v2, structured shell/navigation/module profile config, folder-note-aware explorer manifests, note-level folder-note routing, persisted rail/module/folder state, left-rail-first controls with a mobile utility dock, working `system` / `light` / `dark` palette switching, reader mode, checkbox/task-list rendering, client-rendered local/global graph views, and a note-level BM25-style client search index are now in tree. Remaining follow-up work is mainly richer landing/list-page design, per-page module suppression, deeper right-rail defaults, and broader UX proofing such as screenshots/browser-driven interaction coverage.

**Reference implementations to study before and during execution**

- Quartz:
  - `references/quartz/quartz.layout.ts`
  - `references/quartz/quartz/components/Explorer.tsx`
  - `references/quartz/quartz/components/scripts/explorer.inline.ts`
  - `references/quartz/quartz/components/ReaderMode.tsx`
  - `references/quartz/quartz/components/Search.tsx`
  - `references/quartz/quartz/components/TableOfContents.tsx`
  - `references/quartz/quartz/components/Backlinks.tsx`
  - `references/quartz/quartz/components/Graph.tsx`
  - `references/quartz/quartz/util/fileTrie.ts`
  - `references/quartz/quartz/plugins/emitters/folderPage.tsx`
- MkDocs Material:
  - `references/mkdocs-material/material/templates/base.html`
  - `references/mkdocs-material/material/templates/partials/nav.html`
  - `references/mkdocs-material/material/templates/partials/toc.html`
  - `references/mkdocs-material/material/templates/partials/palette.html`
  - `references/mkdocs-material/docs/setup/setting-up-navigation.md`
  - `references/mkdocs-material/docs/setup/changing-the-colors.md`

**Design rules**

- Keep Vulcan closer to Quartz overall. Quartz is the structural model; Material is the polish and affordance model.
- Do not add a required SPA framework, Node build step, or client-side reimplementation of vault semantics.
- Extend the current shared render/site contract; do not create a separate "fancy theme" renderer that diverges from `site build`, `site serve`, or frontend bundles.
- Treat folder notes / section index pages as first-class navigation concepts, not as an afterthought layered on top of folder listing pages.
- Treat shell state as part of the product surface: palette choice, reader mode, open/closed rails, and collapsed modules should persist across navigation and live preview reloads.
- Preserve accessibility and mobile support as release-blocking requirements, not follow-up polish.

#### 9.20.10.1 Shell contract v2

The current theme contract is too narrow for the layout target. Expand it intentionally.

- [x] Replace the current single-right-sidebar shell with explicit left rail, main content region, and right rail concepts in the built-in renderer
- [x] Add a typed shell-region contract beyond `head/header/nav/footer/note_before/note_after`; built-in themes now also support `toolbar.html`, `left_rail.html`, and `right_rail.html` plus stable shell tokens
- [x] Keep the contract fixed and documented; do not introduce a general-purpose template language or server-side theme execution model
- [x] Preserve backward compatibility for existing `header.html` / `nav.html` / `footer.html` themes where feasible, or document a clear migration path if compatibility shims are temporary
- [x] Make shell-region contracts available to both built-in HTML output and frontend-bundle consumers so later WebUI/wiki work can reuse the same page architecture

#### 9.20.10.2 Navigation model and folder-note support

The left rail should be a real explorer, not just top-nav links.

- [x] Add an explorer/navigation manifest derived from the published route set, suitable for both the built-in shell and frontend-bundle consumers
- [x] Model folders as navigable nodes with configurable behavior: collapse-only, link-to-folder-page, or prefer folder-note/index-note when present
- [x] Add explicit folder-note / section-index behavior for published sites, including rules for `index.md` / configured home notes / folder landing notes
- [x] Support explorer sorting, filtering, and default collapse/open behavior with profile-scoped configuration
- [x] Persist explorer collapse state and restore scroll position across navigation where possible
- [x] Ensure publish filters and route policy apply uniformly: excluded notes/folders must never leak into explorer manifests

#### 9.20.10.3 Left rail search and controls

Search and shell controls should live in the persistent navigation surface, not only in a generic header.

- [x] Move the default search affordance into the left rail while preserving keyboard-first `/` behavior and accessible dialog/search interactions
- [x] Support a Quartz-like compact search trigger plus result overlay/preview experience using the existing static search assets
- [x] Add a first-class palette control with `system`, `light`, and `dark` modes instead of the current binary toggle only
- [x] Add a first-class reader mode control that hides or de-emphasizes chrome without breaking navigation recovery or accessibility
- [x] Persist palette and reader-mode preferences in stable browser storage keys shared across note/list pages

#### 9.20.10.4 Right rail modules and toggleability

**Status:** Complete. The built-in shell now has named right-rail modules, per-profile module settings, per-note `hide_modules` frontmatter controls, persisted collapse state, TOC auto-hide/sticky behavior, and shared graph/backlink/search assets.

The right rail should be a module host, not one hardcoded sidebar.

- [x] Turn TOC, local graph, backlinks, outgoing links, and similar surfaces into named right-rail modules with stable identifiers
- [x] Add per-profile enable/disable settings for each module instead of only top-level booleans like `graph` and `backlinks`
- [x] Add per-page/per-note hide controls where appropriate, preferably via frontmatter metadata compatible with later WebUI/wiki reuse
- [x] Support collapsible modules with persisted open/closed state
- [x] Add TOC behaviors inspired by Material/Quartz, including sticky follow behavior and automatic hiding when headings are absent
- [x] Keep graph/backlinks/search assets shared with frontend-bundle mode rather than generating separate UI-specific payloads

#### 9.20.10.5 Profile/config surface expansion

The current site profile booleans are not expressive enough for the intended shell.

- [x] Extend `[site.profiles.<name>]` with structured shell/navigation/module settings instead of adding many flat booleans
- [x] Add config for left rail defaults: explorer enabled, folder click behavior, default collapse state, saved state behavior, mobile drawer behavior
- [x] Add config for right rail defaults: which modules are shown, default order, collapse defaults, sticky/follow options
- [x] Add config for appearance controls: palette mode defaults, user palette switching enabled/disabled, reader mode enabled/disabled
- [x] Add config parsing/default tests and update the default config template/help text to document the new site-shell surface clearly

#### 9.20.10.6 Default theme v2 and visual language

**Status:** Complete. The no-Node built-in shell now uses the v2 left/right rail layout, palette and reader controls, responsive drawers, and richer home/folder/tag/listing pages, with the reference theme updated for the same shell contract.

This is the visible payoff. The built-in theme should stop looking like a generic generated site.

- [x] Redesign the default CSS/JS shell to use a denser Quartz-like layout with a persistent left rail, stronger typography, clearer hierarchy, and more intentional spacing
- [x] Borrow Material-style polish for sticky sidebars, palette switching, responsive drawers, and hide/show affordances
- [x] Improve list/folder/tag/home pages so they feel like real knowledge-site landing pages instead of generic card dumps
- [x] Preserve the no-Node built-in delivery model: plain CSS and minimal JS emitted by the Rust build remain the default path
- [x] Update the reference theme example to reflect the new shell contract and demonstrate custom left/right rail replacements

#### 9.20.10.7 Documentation, migration, and testing

This follow-on changes a user-facing compatibility surface and needs stronger guidance than the baseline 9.20 docs.

- [x] Expand `docs/guide/static-sites.md` with the new shell contract, config reference, module model, folder-note behavior, and migration notes for old theme partials
- [x] Add screenshots or fixture-based examples for desktop/mobile layouts, palette modes, reader mode, hidden rails, and folder-note explorer behavior
- [x] Add snapshot/smoke tests for landmark structure, keyboard navigation, responsive drawer behavior, reader mode, and module toggling
- [x] Add integration tests covering folder-note routing + explorer manifests, per-profile shell config differences, and root-hosted vs subpath-hosted shell asset/state correctness
- [x] Keep frontend-bundle contracts in sync: if the built-in shell gains a new typed navigation/module manifest, document and test the bundle shape at the same time

#### 9.20.10 Recommended implementation order

Do the structural pieces before visual polish:

1. Expand the shell/theme contract and site-profile config surface.
2. Add explorer/navigation manifests plus folder-note/index-note semantics.
3. Refactor right-rail surfaces into typed modules with persisted state.
4. Upgrade palette handling and add reader mode as first-class shell state.
5. Rebuild the default CSS/JS shell around the new layout.
6. Update docs, reference themes, snapshots, and frontend-bundle parity tests.

---

## Phase 9.21: Retired embedded assistant host mode via managed-engine RPC

**Goal:** Retire the optional embedded assistant host and keep Vulcan focused on the stable MCP/CLI/tool boundary. Native assistant runtimes should connect through MCP or shell out to Vulcan's JSON commands; Vulcan should not host `pi` directly in Phase 9.

**Status:** Retired and removed. The CLI-hosted managed-engine pilot was implemented, tested against a mock engine, and then removed after real-world evaluation showed that it produced a worse experience than using the native assistant runtime directly or using Vulcan's MCP server. The `vulcan assistant` command, pi RPC modules, bundled pi extension, assistant session export app code, and embedded-host config fields were deleted. The remaining supported surfaces are MCP, `vulcan describe`, `vulcan agent install`, `vulcan skill ...`, and external runtimes invoking Vulcan commands under permission profiles.

**Replacement path:** Conversation archiving moved into the bundled `conversation-export` skill command. This intentionally demonstrates the default skill/custom-command flow and keeps transcript import/export as ordinary vault note creation rather than a hard-coded assistant subsystem.

**Why the design changed:** Phase 9.12 and the MCP work proved the important boundary is tool ownership, permission filtering, and vault mutation semantics. Embedding a specific assistant engine inside Vulcan duplicated UI/session concerns, depended on unstable external runtime flags, and did not provide enough value over MCP plus external runtimes. Vulcan should remain the vault/tool authority; assistant clients should own chat UX and model orchestration.

**Historical note:** The detailed checklist below records the retired pilot for archaeology only. It is not an open work queue, and deferred items in this retired phase are no longer Phase 9 requirements. Future native-chat or runtime-host work should start from MCP, Phase 10 daemon primitives, and explicit new roadmap items rather than reviving the removed `vulcan assistant` code.

**Design principle — Vulcancentrism:** Vulcan is the host process. Pi is a managed subprocess. The user never interacts with pi directly; all interaction goes through the `vulcan assistant` command surface. Based on the MCP server experience, Vulcan must own context assembly, tool discovery, permission-profile filtering, and vault mutation semantics. Pi's built-in tools are not the trusted enforcement boundary; vault mutations should be routed through Vulcan commands, not pi's raw file-edit tools, so the same safety checks, dry-run, and auto-commit guarantees apply.

**Reused foundations from 9.12:** vault `AGENTS.md`, `.agents/skills/*/SKILL.md`, CLI-to-tool 1:1 mapping, permission-profile semantics, and the rule that durable artifacts are normal vault notes while live chat/session state stays in pi by default.

**Depends on:** Phase 9.12 (external agent contract and tool boundary already defined), Phase 9.19.13 (permission layer), and Phase 10 (daemon/service maturity gate from 9.12.8). Phase 9.18.2/9.18.7 provide note CRUD and describe/help stability; Phase 9.3 provides git auto-commit. Phase 9.6 (search) and Phase 7.12 (query model) are used for vault-aware tool execution.

**Design refs:** `docs/assistant/pi_integration.md` (9.12 contract), pi RPC protocol documentation (`packages/coding-agent/docs/rpc.md` in the pi-mono repository), `docs/assistant/native_runtime_deferred.md` (preserved native-runtime design for revisit).

### 9.21.1 Pi subprocess management

Spawn and manage the pi process lifecycle. This is the foundation that everything else builds on.

- [x] New module `vulcan-cli/src/assistant/mod.rs` as the public entry point for assistant commands
- [x] New module `vulcan-cli/src/assistant/engine.rs` (initial implementation of the planned `pi_process.rs` responsibilities):
  - [x] Locate pi binary: check `$PI_BINARY`, then `PATH` for `pi`, then common install locations (`~/.npm-global/bin/pi`, `/usr/local/bin/pi`)
  - [x] `ManagedEngineProcess::spawn(args, config)` — start `pi --mode rpc` with appropriate flags:
    - [x] `--cwd <vault_root>` for workspace awareness
    - [x] `--provider <provider>` and `--model <model>` from Vulcan config
    - [x] `--no-session` for ephemeral mode, or `--session-dir <vault_root>/AI/Sessions/` for persistent sessions
    - [x] `-e <extension_path>` to load the Vulcan tools extension (9.21.3)
    - [x] `--thinking <level>` from Vulcan config
  - [x] `PiProcess::ensure_running()` — health check; respawn if the process has died
  - [x] `PiProcess::shutdown()` — send `abort` command, wait for graceful exit, kill after timeout
  - [x] `PiProcess::is_healthy()` — check that stdin/stdout pipes are still open and the process hasn't exited
  - [x] Handle pi not found: emit actionable error with install instructions (`npm install -g @mariozechner/pi-coding-agent`)
  - [x] Handle pi version incompatibility: capture `--version` output and report it in doctor output; strict minimum-version gating remains deferred until the pi RPC contract publishes stable compatibility metadata
  - [x] On spawn failure or crash: classify the common errors and emit a user-facing diagnostic
- [x] Add `[assistant]` section to `VaultConfig` in `vulcan-core/src/config/mod.rs`:
  ```toml
  [assistant]
  runtime = "pi"            # "pi" (default) or "none" to disable
  pi_binary = "pi"          # binary name or full path
  provider = "anthropic"     # LLM provider
  model = ""                 # model ID; empty = pi default
  thinking_level = "medium"  # off, minimal, low, medium, high, xhigh
  permissions = "edit"        # readonly, edit, refactor
  sessions_dir = "AI/Sessions"  # relative to vault root; empty = ephemeral
  session_export = "on_exit"    # manual, on_exit, always
  session_exports_dir = "AI/Assistant Sessions"
  ```
- [x] Add `[assistant]` section to `DEFAULT_CONFIG_TEMPLATE` (commented out, with defaults shown)
- [x] Add `--assistant-pi-binary`, provider/model/thinking overrides, and `--assistant-permissions` CLI overrides
- [x] Integration test: spawn a mock pi-compatible RPC process, verify command/response round-trip, then shut down cleanly
- [x] Removed with retired embedded host: Real-pi CI smoke test: deferred until CI has a stable pi install and credentials story

### 9.21.2 Pi RPC client

A typed Rust client for pi's JSON-RPC protocol. This module knows the protocol; the rest of the assistant code uses this client rather than dealing with raw JSON.

- [x] New module `vulcan-cli/src/assistant/rpc.rs` (initial synchronous client; async dispatcher remains open):
  - `PiRpcClient` struct wrapping the pi subprocess's stdin/stdout handles
  - [x] JSONL framing: write `\n`-terminated JSON to stdin, read `\n`-delimited JSON from stdout
  - [x] Do NOT use BufReader with line-by-line reading that splits on `U+2028`/`U+2029` (pi docs explicitly warn about this — Node `readline` is non-compliant). Implement a custom LF-only line reader
  - [x] Correlation ID tracking: optional `id` field on commands, matched in `type: "response"` replies
  - Pending-response map: `HashMap<String, oneshot::Sender<RpcResponse>>` for awaiting command responses
  - Event dispatch: spawn a background task that reads stdout lines, parses them, and dispatches to:
    - Response correlator (for command responses)
    - Event subscriber channel (for streaming events)
    - Extension UI handler (for `extension_ui_request`)
  - [x] `send_command(&mut self, command: RpcCommand) -> Result<RpcResponse>` — write command, await response if `id` was set
  - `subscribe_events(&self) -> broadcast::Receiver<PiEvent>` — subscribe to the event stream
  - `prompt(&mut self, message: &str)` — send prompt command, return immediately (events stream asynchronously)
  - `steer(&mut self, message: &str)` — queue steering message during streaming
  - `abort(&mut self)` — abort current agent operation
  - `get_state(&mut self) -> Result<PiSessionState>` — query session state
  - `set_model(&mut self, provider: &str, model_id: &str) -> Result<()>` — switch model
  - `compact(&mut self) -> Result<CompactionResult>` — trigger compaction
  - `new_session(&mut self) -> Result<bool>` — start fresh session (returns `true` if cancelled by extension)
  - `shutdown()` — send graceful shutdown signal

- [x] Initial typed RPC structs live in `vulcan-cli/src/assistant/rpc.rs`:
  - Typed structs for all RPC commands: `RpcCommand` enum with variants for `prompt`, `steer`, `follow_up`, `abort`, `get_state`, `set_model`, `cycle_model`, `get_available_models`, `set_thinking_level`, `cycle_thinking_level`, `compact`, `set_auto_compaction`, `new_session`, `get_session_stats`, `get_messages`, `get_commands`, `bash`
  - Typed structs for all RPC responses: `RpcResponse` with `id`, `command`, `success`, `data`, `error`
  - Typed structs for session state: `PiSessionState` with `model`, `thinking_level`, `is_streaming`, `is_compacting`, `session_id`, `session_name`, `message_count`, `pending_message_count`
  - Typed structs for compaction result: `CompactionResult` with `summary`, `first_kept_entry_id`, `tokens_before`
  - Typed structs for model info: `PiModel` with `id`, `name`, `provider`, `reasoning`, `context_window`, `max_tokens`

- [x] New module `vulcan-cli/src/assistant/rpc_events.rs`:
  - `PiEvent` enum covering all events pi emits:
    - `AgentStart`, `AgentEnd { messages }` — agent lifecycle
    - `TurnStart`, `TurnEnd { message, tool_results }` — turn lifecycle
    - `MessageStart`, `MessageUpdate { assistant_event }, MessageEnd` — message streaming
    - `ToolExecutionStart`, `ToolExecutionUpdate`, `ToolExecutionEnd` — tool execution
    - `QueueUpdate { steering, follow_up }` — pending messages
    - `CompactionStart`, `CompactionEnd` — compaction
    - `AutoRetryStart`, `AutoRetryEnd` — retry
    - `ExtensionError` — extension errors
    - `ExtensionUiRequest` — extension UI sub-protocol (confirm, select, input, notify, etc.)
  - `PiAssistantEvent` enum for streaming delta types: `TextDelta`, `ThinkingDelta`, `ToolCallStart`, `ToolCallDelta`, `ToolCallEnd`, `Done`, `Error`
  - All structs derive `Serialize`, `Deserialize` for parsing from pi's JSON output
  - Unit tests for parsing representative JSON lines from pi's RPC protocol against the typed structs
- [x] Removed with retired embedded host: Dependency: add `tokio` with `process` and `sync` features to `vulcan-cli/Cargo.toml`; not needed for the shipped synchronous CLI host, and deferred to daemon-managed async transport
- [x] Removed with retired embedded host: Add `tokio-util` with `codec` feature for the JSONL codec if beneficial; the shipped client uses a manual LF-only line reader

### 9.21.3 Vulcan tools as a pi extension

Register Vulcan's tool surface as pi custom tools so the LLM can call them naturally. This is implemented as a pi TypeScript extension that is loaded when pi starts.

- [x] New directory `vulcan-cli/src/assistant/extension/` containing the pi extension source:
  - `vulcan-tools/index.ts` — extension entry point
  - `vulcan-tools/package.json` — pi package manifest with `"pi": { "extensions": ["./index.ts"] }`
- [x] The extension is bundled into the Vulcan binary at compile time using `include_str!` and extracted to `.vulcan/assistant/extension/` on `vulcan init` or before assistant launch
- [x] Extension behavior on `session_start`:
  - Registers a conservative `vulcan_cli` tool instead of a schema fan-out of every command; this follows the MCP server lesson that Vulcan should remain the policy and command-contract boundary
  - Each tool's `execute` function:
    1. Shell out to `vulcan <command> --output json` with the provided arguments
    2. Parse stdout as JSON or line-delimited JSON
    3. Return the result as a `ToolResult`
    4. Normalize non-zero exit codes into structured tool errors
- [x] Extension behavior for permission profiles:
  - `readonly`: only register read-only tools (`note get`, `search`, `query`, `backlinks`, `links`, `graph`, `daily list`, etc.)
  - `edit`: add note CRUD, property mutation, and inbox tools
  - `refactor`: add `move`, `merge-tags`, `rename-*`, `rewrite`, and other high-impact commands
  - The active profile is passed as a CLI flag to pi via the extension's initialization (e.g., as a custom flag or environment variable)
- [x] Extension registers a `tool_call` hook that:
  - Blocks pi's built-in `bash` and `edit` tools when the permission profile is `readonly`
  - In `edit` mode, allows `bash` but logs a warning for commands outside the vault directory
  - In `refactor` mode, allows all tools
- [x] Extension registers a `before_agent_start` hook that:
  - Appends vault `AGENTS.md` content to the system prompt if present
  - Injects a compact tool summary and active permission profile description
- [x] Unit tests: verify the bundled extension materializes and carries the expected permission-profile enforcement strings
- [x] Integration test: launch a mock pi-compatible RPC engine with the extension path and verify one-shot/chat round-trips
- [x] Removed with retired embedded host: Real pi extension load test: deferred until real-pi CI smoke tests are available

### 9.21.4 Assistant context injection

At session start, inject vault context into pi so the assistant knows about the vault structure, available tools, and relevant skills.

- [x] Initial context builder lives in `vulcan-cli/src/assistant/mod.rs`:
  - `build_session_context(vault_root, config) -> SessionContext`:
    - [x] Read vault `AGENTS.md` if present
    - [x] Reuse the MCP tool registry to produce a filtered tool summary for the system prompt
    - [x] Enumerate default and user skills from `.agents/skills/` (just names and descriptions, not full content)
    - Collect vault metadata: vault name, note count, tag summary, property catalog summary
  - `format_system_prompt_append(context) -> String`:
    - Format the context as a structured block for pi's system prompt
    - Include: vault identity, tool surface summary, skill directory, permission profile, vault `AGENTS.md` content
- [x] The context is injected through the RPC `configure` command and the extension's `before_agent_start` hook
- [x] Skills are loaded on-demand through normal Vulcan CLI/tool access; the context payload includes the skill directory and names/descriptions
- [x] `AGENTS.md` content is injected once at session start, not on every turn (keep per-turn context small)
- [x] Context size budget: tool summaries and skill directory stay compact; full schemas and skill bodies stay on-demand
- [x] Integration test: mock assistant context inspection verifies vault tools, skill names, permission profile, and `AGENTS.md` payload

### 9.21.5 Streaming output rendering

Render pi's streaming events into Vulcan's terminal output, both for one-shot prompts and interactive sessions.

- [x] New module `vulcan-cli/src/assistant/renderer.rs`:
  - `AssistantRenderer` trait with methods for each event type:
    - `on_text_delta(&mut self, delta: &str)` — stream text to output
    - `on_thinking_delta(&mut self, delta: &str)` — render thinking/reasoning output (collapsible or dimmed)
    - `on_tool_start(&mut self, name: &str, args: &Value)` — show tool execution indicator
    - `on_tool_end(&mut self, name: &str, result: &Value, is_error: bool)` — show tool result summary
    - `on_compaction(&mut self)` — show compaction indicator
    - `on_agent_end(&mut self)` — final rendering, cost summary
- [x] `PrintRenderer` implementation — for `vulcan assistant <prompt>` one-shot mode:
  - Stream text deltas directly to stdout
  - Render thinking blocks as dimmed text with `[thinking]` prefix (controlled by `--show-thinking` flag)
  - Show tool execution as indented summary lines: `  -> bash: ls -la`
  - Show tool results as truncated output with `[...N lines]` for long results
  - Print session stats on completion: token usage, cost, turn count
  - Respect `--output json` for machine-readable results (emit structured JSON instead of formatted text)
- [x] Removed with retired embedded host: Rich `InteractiveRenderer` implementation — deferred; chat currently uses the same streaming print renderer:
  - Use `crossterm` for styled output (colored tool names, bold headings, dimmed thinking)
  - Show a live tool execution indicator with spinner (reuse existing TUI patterns from `browse_tui.rs`)
  - Render assistant text as it streams (no buffering)
  - Render thinking as a collapsible block (toggle with keyboard shortcut)
  - Show footer with: model name, thinking level, context usage, pending queue count
- [x] Streaming output handles Ctrl+C in chat mode by sending `abort`; richer two-stage force-kill behavior is deferred to the daemon/TUI path:
  - First Ctrl+C: send `abort` command to pi (interrupt current tool batch, deliver steering message)
  - Second Ctrl+C: force-kill pi subprocess
  - Always render any partial response already received
- [x] Unit tests for renderer output using a mock event stream

### 9.21.6 Interactive chat mode

A REPL-style interactive assistant session, driven by `readline` for input and `crossterm` for styled output.

- [x] New module `vulcan-cli/src/assistant/chat.rs`:
  - `ChatSession` struct holding the `PiRpcClient` and `InteractiveRenderer`
  - Main loop:
    1. Read user input via `rustyline` (already a dependency) with history and tab completion
    2. Send input as a `prompt` command to pi
    3. Read events from the subscription channel and dispatch to the renderer
    4. On `agent_end`, print the prompt again and loop
  - Handle special input:
    - Empty input (Enter): no-op
    - `/model`: send `cycle_model` command to pi, display new model
    - `/thinking`: send `cycle_thinking_level` command to pi, display new level
    - `/compact`: send `compact` command to pi
    - `/new`: send `new_session` command to pi
    - `/stats`: send `get_session_stats` command to pi, display usage
    - `/help`: display available slash commands
    - `/quit` or Ctrl+D: send `abort` if streaming, then shut down pi and exit
- [x] Removed with retired embedded host: Handle extension UI requests from pi:
    - `confirm`: render the confirmation prompt in the terminal, read y/n, send response back to pi
    - `select`: render numbered options, read selection, send response back to pi
    - `input`: render the prompt, read input via rustyline, send response back to pi
    - `notify`: render the notification as a colored status line
    - `editor`: open `$EDITOR` with prefill, read result, send back (reuse `open_in_editor` from `editor.rs`)
  - Handle queuing:
    - If user types while pi is streaming: send as `steer` message (interrupt after current tool)
    - Alt+Enter: send as `follow_up` message (wait until pi finishes)
    - Show queue state in the footer
  - Session persistence:
    - If `sessions_dir` is configured: pi writes session files there automatically
    - Resume with `vulcan assistant --chat --resume` or `--continue` (find most recent session)
- [x] `vulcan assistant --chat` command wires to the chat runner
- [x] `vulcan assistant <prompt>` sends the prompt, renders the response, and exits
- [x] Integration test: start a chat session with a mock pi subprocess, verify prompt -> event -> render round-trip

### 9.21.7 Assistant CLI surface

Wire the assistant into Vulcan's CLI command structure.

- [x] Add `assistant` subcommand to `vulcan-cli/src/cli.rs`:
  ```
  vulcan assistant <prompt>              # one-shot prompt, stream response, exit
  vulcan assistant --chat               # interactive chat session
  vulcan assistant --chat --resume      # resume most recent session
  vulcan assistant --chat --continue   # continue most recent session
  vulcan assistant --list-sessions     # list persisted assistant sessions
  vulcan assistant --doctor            # check assistant prerequisites (pi binary, auth, config)
  ```
- [x] One-shot mode flags:
  - `--provider <name>` — LLM provider override
  - `--model <id>` — model override
  - `--thinking <level>` — thinking level override
  - `--show-thinking` — render thinking blocks in one-shot output
  - `--assistant-permissions <profile>` — permission profile (`readonly`, `edit`, `refactor`; default from config); the global `--permissions` flag remains available before the subcommand
- [x] Removed with retired embedded host: `--no-commit` — not added because assistant writes go through normal Vulcan commands and existing command-level commit controls
  - `--output json` — machine-readable output for one-shot results
  - `--no-tools` — start pi with `--no-tools` and only Vulcan tools from the extension
- [x] Chat mode flags:
  - `--resume` / `--continue` — resume a previous session
  - `--ephemeral` — don't persist session (default if `sessions_dir` is empty)
- [x] Initial `vulcan assistant --doctor` checks:
  - pi binary found and version sufficient
  - API key configured (at least one provider has credentials)
  - Vault cache is current (last scan timestamp)
  - Extension loads without errors
  - Config is valid (provider, model, permissions)
- [x] Initial `vulcan assistant --list-sessions`:
  - Enumerate session files in `.vulcan/assistant/sessions/`
  - Show session ID, title, message count, last active timestamp
- [x] Tab completion in chat mode:
  - Complete `vulcan` commands after `/vulcan ` prefix
  - Complete file paths after `@` prefix
  - Complete slash commands (`/model`, `/thinking`, `/compact`, etc.)
- [x] Non-interactive mode: if not a TTY, `vulcan assistant` reads prompt from stdin and writes response to stdout (like `pi -p`)
- [x] Update `vulcan init` to create the `AI/Sessions/` directory and write the assistant extension if `runtime = "pi"` is set in config
- [x] Integration tests for one-shot, chat, doctor, and list-sessions commands
  - [x] CLI smoke tests for doctor, context inspection, list-sessions, one-shot, chat, and init assistant artifacts

### 9.21.8 Permission profiles and safety

Enforce Vulcan's permission model in the pi subprocess so that vault mutations always go through Vulcan's safety checks.

- [x] Permission profile definitions (extend the existing 9.12.2 model):
  - `readonly`: read-only tools only (no bash, no note mutations). Pi's built-in bash and edit tools are blocked.
  - `edit`: note CRUD, property mutations, inbox. Pi's bash tool is allowed but constrained to the vault directory.
  - `refactor`: all tools including `move`, `merge-tags`, `rename-*`, `rewrite`. Pi's bash tool is fully available.
- [x] Enforcement mechanisms:
  - The Vulcan tools extension (9.21.3) registers a `tool_call` hook that blocks pi built-in tools based on the active profile
  - The extension's `execute` functions for Vulcan tools pass `--permissions <profile>` to `vulcan` CLI invocations so Vulcan's own enforcement is also active
  - `bash` tool in `edit` mode: the extension's `tool_call` hook inspects the command and blocks operations outside the vault root
- [x] Config in `.vulcan/config.toml`:
  ```toml
  [assistant]
  permissions = "edit"  # default profile for assistant sessions
  ```
- [x] `--assistant-permissions` CLI override on `vulcan assistant` commands
- [x] Removed with retired embedded host: High-impact dry-run-by-default prompts are deferred until extension UI confirmation is implemented; Vulcan command permission checks remain the enforcement boundary
- [x] `vulcan assistant --assistant-permissions readonly` is the recommended mode for exploration and search-heavy workflows
- [x] Document the permission profile mapping from Vulcan profiles to pi tool constraints
- [x] Integration test: extension unit coverage verifies readonly built-in blocking and nested Vulcan invocations pass the active permission profile

### 9.21.9 Session and persistence boundary

Define where assistant session state lives and how it relates to vault state.

- [x] **Default: pi owns session state.** Pi writes session files to `.vulcan/assistant/sessions/` (or `AI/Sessions/` depending on config). Vulcan only reads lightweight headers for listing and never rewrites them.
- [x] Session file naming: pi uses its own session ID scheme. On `vulcan assistant --list-sessions`, Vulcan reads the session directory and presents the files with metadata from pi's session headers.
- [x] Resume semantics:
  - [x] `--continue`: find the most recent session file and pass `--session <path>` to pi
  - [x] `--resume`: alias newest-session resume for non-interactive use
  - [x] `--resume-session <path|file|stem|session-id|title>`: resolve an explicit session target and pass it to pi
- [x] Durable artifacts: if the assistant produces output the user wants to keep, it should write a normal vault note through the `note_create` or `note_append` tool. This is consistent with the 9.12.4 session boundary.
- [x] Optional Markdown export layer: `session_export = "on_exit"` exports managed session files into Obsidian-readable notes with YAML frontmatter and `[!user]` / `[!assistant]` / `[!tool]` callouts
- [x] Manual `vulcan assistant --export-session <path|file|stem|session-id|title|latest>` CLI for ad hoc transcript export by ID/path
- [x] Ephemeral mode: `--ephemeral` passes `--no-session` to pi so no session file is created
- [x] Auto-commit: assistant-initiated mutations route through normal Vulcan commands, so existing command-level auto-commit behavior applies where those commands support it
- [x] Document that session history is pi-managed, not vault-managed; revisit only if pi's session model proves insufficient

### 9.21.10 Testing and hardening

Comprehensive testing for the embedded assistant integration.

- [x] **Unit tests:**
  - [x] RPC protocol types: parse representative JSON lines from pi's RPC output against the typed structs
  - [x] RPC client: mock subprocess that produces scripted JSON lines; verify command → response correlation
  - [x] Context builder: verify AGENTS.md injection, skill enumeration, tool summary generation
  - [x] Renderer: verify formatted output for streaming events, tool calls, thinking blocks
  - [x] Permission enforcement: verify tool_call blocking inputs are bundled and permission profile is propagated
- [x] Removed with retired embedded host: **Integration tests (require pi installed in CI):**
  - Spawn pi in RPC mode, send `get_state`, verify response structure
  - One-shot prompt: `vulcan assistant "list files in the current directory"` — verify non-empty text output
  - Chat round-trip: send a prompt, receive streaming text, send a follow-up, receive response
  - Tool execution: prompt the assistant to call a Vulcan tool, verify the result flows back
  - Extension loading: verify the Vulcan tools extension loads without errors
  - Permission profiles: verify `readonly` blocks note mutations, `edit` allows them
  - [x] Session persistence with mock engine: start a session, exit, resume a selected session, and verify the resolved session is passed back to the engine
  - Crash recovery: kill pi mid-stream, verify Vulcan detects the crash and reports a useful error
- [x] Removed with retired embedded host: **Smoke tests (CI-marked optional if pi is not installed):**
  - Daily-driver workflows: read note, patch note, search vault, run refactors
  - Model switching: `/model` in chat mode
  - Compaction: long session that triggers auto-compaction
- [x] **Error scenario tests:**
  - Pi binary not found
- [x] Removed with retired embedded host: Pi binary too old strict failure; doctor reports version, but minimum version is deferred until pi publishes stable compatibility metadata
- [x] Removed with retired embedded host: No API key configured; provider-specific auth remains owned by pi
  - Pi crashes mid-stream
- [x] Removed with retired embedded host: Pi takes too long (timeout); deferred to daemon-managed process supervision
  - [x] Extension fails to load
  - [x] Misconfigured settings
- [x] Document how to run the assistant test suite locally and in CI, including which tests require pi to be installed
- [x] Exporter regression tests cover JSONL messages/events, interrupted sessions with truncated final JSONL lines, malformed middle lines, Obsidian callout transcripts, and Markdown export output

### 9.21.11 Documentation and user guide

Make the embedded assistant discoverable and usable without reading the source.

- [x] Update `docs/guide/getting-started.md` with an "Assistant" section covering installation, configuration, and first prompt
- [x] New page `docs/guide/assistant.md`:
  - Prerequisites (pi installation, API key setup)
  - Configuration options in `.vulcan/config.toml`
  - One-shot usage: `vulcan assistant <prompt>`
  - Interactive usage: `vulcan assistant --chat`
  - Slash commands in chat mode (`/model`, `/thinking`, `/compact`, `/new`, `/stats`, `/quit`)
  - Permission profiles explained
  - Session management (resume, continue, ephemeral)
  - Troubleshooting (`--doctor`, common errors)
  - How it relates to Phase 9.12 (bring-your-own-pi vs embedded)
- [x] Update `docs/ROADMAP.md` cross-references:
  - Phase 9.12: add a note that 9.21 provides the alternative "Vulcan embeds pi" model
  - Phase 10 (daemon): add a forward-reference note that the daemon can manage pi processes for multi-vault scenarios
  - Phase 13 (WebUI): note that the RPC client and event types from 9.21.2 are reused for WebSocket streaming to the browser
  - Deferred native chat (9.12.8): add a revisit trigger — "if pi RPC latency or process management becomes a bottleneck in the daemon/WebUI context, reassess a native Rust agent loop"
- [x] Update `docs/assistant/pi_integration.md` to cover both integration models:
  - Model A (9.12): pi is the host, Vulcan is the tool provider
  - Model B (9.21): Vulcan is the host, pi is the agent engine via RPC
  - When to use which
- [x] Update `vulcan help assistant` output with a concise usage summary

### Implementation order

1. **9.21.2 (RPC client)** — the protocol layer that everything depends on
2. **9.21.1 (subprocess management)** — depends on the RPC client for communication
3. **9.21.5 (streaming renderer)** — can be developed in parallel with 9.21.1 using a mock event stream
4. **9.21.3 (Vulcan tools extension)** — depends on pi being spawnable (9.21.1)
5. **9.21.4 (context injection)** — depends on the extension (9.21.3) for prompt hooks
6. **9.21.7 (CLI surface)** — wires it all together for user access
7. **9.21.6 (interactive chat)** — the richest surface, builds on all prior pieces
8. **9.21.8 (permissions)** — hardens the integration, can be refined iteratively
9. **9.21.9 (sessions)** — persistence layer, can be added once the core loop works
10. **9.21.10 (testing)** — ongoing from step 1, but the comprehensive suite lands last
11. **9.21.11 (documentation)** — written alongside implementation, finalized last

### Migration path to daemon (Phase 10)

The RPC client (9.21.2) is the key investment. When the daemon exists:

- **Transport swap only:** Change `PiRpcClient`'s I/O from stdin/stdout pipes to TCP/unix socket. The protocol (commands, events, extension UI) stays identical.
- **Process management:** The daemon takes over pi process lifecycle management instead of the CLI process. `PiProcess::spawn()` becomes a request to the daemon.
- **Multi-vault:** The daemon can manage one pi process per registered vault instead of one per CLI invocation.
- **No wasted work:** The typed command/event structs, the event dispatcher, and the extension UI handler are all transport-agnostic and carry forward directly.

### 9.21.12 Cross-platform chat transport contract (Deferred follow-on: native chat)

Do not make Telegram the architecture. If native chat is revived, start by defining the reusable assistant/chat boundary that all platforms plug into.

- [x] New module `vulcan-cli/src/assistant/chat_transport.rs` (or similar) for the platform-neutral runtime contract
- [x] Define canonical external user principal strings for bindings and audit logs:
  - `telegram:123456`
  - `matrix:@alice:example.com`
  - `discord:USER_ID`
- [x] Define canonical external chat-space IDs for sessions and policy lookup:
  - `telegram:-1001234567890` for a Telegram group/chat
  - `matrix:!roomid:example.com` for a Matrix room
  - `discord:guild/123/channel/456`
  - `discord:guild/123/channel/456/thread/789`
- [x] Separate user principals from chat spaces in the data model; do not overload one string type for both
- [x] Model hierarchical spaces with `parent_space_id` so guild → channel → thread or workspace → room inheritance works naturally
- [x] Define typed transport-layer Rust structs:
  - `ExternalUserPrincipal`
  - `ChatSpace`
  - `IdentityBinding`
  - `ChatEvent`
  - `ChatAction`
  - `AdapterCapabilities`
- [x] Core inbound events must include at least:
  - message
  - reaction added / removed
  - reply-to / message reference
  - message edited / deleted
  - attachment received
  - interaction event (buttons/selects or equivalent)
- [x] Core outbound actions must include at least:
  - send message
  - edit message
  - reply
  - add/remove reaction
  - acknowledge interaction
  - render buttons when supported
- [x] Capability negotiation: adapters advertise whether they support reactions, message edits, replies, buttons, attachments, threads, or ephemeral messages; the assistant core degrades gracefully when a feature is absent
- [x] Identity binding layer maps an external user principal to:
  - a stable assistant-side `vault_identity`
  - an optional Phase 17 auth subject such as `user:alice`
  - an optional canonical note path such as `People/Alice.md`
- [x] Session and memory routing should key off the internal `vault_identity` and internal space ID once a binding exists, so one human can share memory across Telegram and Matrix after verification
- [x] Unbound users fall back to platform-scoped memory/session routing until linked
- [x] Permission resolution should start from Phase 17 rooted grants or a limited agent credential for the bound subject, then apply the restrictive intersection of platform defaults, inherited space constraints, external-user constraints, and per-session limits; transport policy cannot widen authority
- [x] Keep non-rebuildable platform runtime state out of the vault and out of `.vulcan/cache.db`; define a daemon-managed state directory for adapter-specific databases, sync tokens, media caches, and crypto material
- [x] Add assistant chat config sketch to `.vulcan/config.toml` docs:
  ```toml
  [assistant.chat]
  default_profile = "readonly"
  session_root = "AI/Sessions"
  memory_root = "AI/Memory"

  [assistant.chat.identities.alice]
  subject = "user:alice"
  note = "People/Alice.md"

  [[assistant.chat.bindings]]
  external_user = "telegram:123456789"
  vault_identity = "alice"
  verification = "admin-confirmed"

  [[assistant.chat.bindings]]
  external_user = "matrix:@alice:example.com"
  vault_identity = "alice"
  verification = "device-verified"

  [assistant.chat.spaces."discord:guild/123"]
  profile = "readonly"

  [assistant.chat.spaces."discord:guild/123/channel/456"]
  parent = "discord:guild/123"
  profile = "edit"
  ```

### 9.21.13 Telegram adapter (Deferred follow-on: native chat)

Implement Telegram on top of the cross-platform contract from 9.21.12 rather than letting Telegram-specific concerns leak into the assistant core.

- [x] Removed with retired embedded host: New module `vulcan-cli/src/assistant/platforms/telegram.rs` (using `teloxide` or similar crate)
- [x] Removed with retired embedded host: Add `vulcan assistant --telegram` command only after the transport contract exists
- [x] Removed with retired embedded host: Map Telegram users to `telegram:<user_id>` and spaces to `telegram:<chat_id>`
- [x] Removed with retired embedded host: Support DM, group, and supergroup conversations through the shared `ChatSpace` model
- [x] Removed with retired embedded host: Translate Telegram replies, reactions, attachments, and inline keyboard button callbacks into the shared event/action contract
- [x] Removed with retired embedded host: Route sessions by internal chat-space ID rather than raw Telegram `chat_id` paths
- [x] Removed with retired embedded host: Batch streaming message edits to respect Telegram API rate limits without making the assistant renderer Telegram-aware
- [x] Removed with retired embedded host: Enforce security at the Rust boundary by resolving the effective profile from the transport contract, then spawning pi with the corresponding `--permissions` profile

### 9.21.14 Matrix adapter research and viability gate (Deferred follow-on: native chat)

Matrix is explicitly more complex than Telegram because it brings sync loops, room state, media handling, and E2EE key management. Treat it as a separate design gate, not as "Telegram but different IDs."

- [x] Removed with retired embedded host: Produce a research note covering Matrix SDK options, sync architecture, E2EE key storage, and verification UX
- [x] Removed with retired embedded host: Evaluate `matrix-sdk` (or equivalent) for a daemon-managed long-lived adapter
- [x] Removed with retired embedded host: Define daemon-managed runtime state requirements for:
  - sync tokens
  - room state caches
  - Olm/Megolm key stores
  - device verification state
  - media cache / upload staging
- [x] Removed with retired embedded host: Map Matrix users to `matrix:@user:server` and rooms to `matrix:!roomid:server`
- [x] Removed with retired embedded host: Verify how replies, reactions, edits, attachments, and richer interactions map into the 9.21.12 transport contract
- [x] Removed with retired embedded host: Decide whether Matrix lands as:
  - a native daemon-managed adapter
  - a separate sidecar process speaking the same transport contract
  - or a deferred platform if the operational burden is too high for the native runtime
- [x] Removed with retired embedded host: Exit criterion for production Matrix implementation

## Phase 9.22: Crate boundary cleanup and reusable workflow extraction

**Goal:** Re-establish the intended workspace boundaries from the design doc: `vulcan-core` owns reusable vault semantics and data-model logic, `vulcan-cli` becomes a thin command/TUI/output layer, and reusable workflow orchestration moves into library crates instead of accumulating inside `vulcan-cli`.

**Why this phase exists:** A large amount of command-agnostic logic currently lives in `vulcan-cli` rather than in `vulcan-core` or a sibling library crate. That hurts reuse, duplicates logic across surfaces (for example CLI web tools vs JS web tools), makes `vulcan-cli/src/lib.rs` a de facto application layer, and raises the cost of Phase 10 daemon work, MCP work, and assistant integrations.

**Scope rule:** This phase is primarily a code migration and boundary cleanup. It should not intentionally change user-visible behavior unless a separate roadmap item explicitly calls for it.

**Builds on:** Phase 9.18 command reorganization and the current command surfaces. Recommended before substantial additional work in Phase 9.20 and Phase 10 so those surfaces can consume shared libraries instead of depending on `vulcan-cli` internals.

### 9.22.1 Responsibility contract and migration inventory

- [x] Document crate responsibilities in `docs/design_document.md` and crate-level docs:
  - `vulcan-core`: parser, indexer, query/eval, config model, cache abstractions, shared backend logic, domain request/response types, and other command-agnostic vault semantics
  - `vulcan-app`: new workspace library crate for reusable workflow orchestration that composes `vulcan-core` with filesystem mutation, scan refresh, packaging, plugin dispatch, and other non-UI application services
  - `vulcan-cli`: `clap` parsing, TUI state, stdin/stdout handling, editor/browser launching, shell completions, and output formatting only
  - `vulcan-daemon`: long-lived transports, HTTP/WebSocket endpoints, async boundaries, and background scheduling only
- [x] Produce a migration inventory of current `vulcan-cli` modules and classify each code path as:
  - stays in CLI because it is terminal/UI specific
  - moves to `vulcan-core` because it is pure reusable semantics
  - moves to `vulcan-app` because it is reusable workflow orchestration
  - moves to `vulcan-daemon` because it is long-lived server/transport logic
- [x] Add a contributor rule: new reusable business logic must not land in `vulcan-cli` unless it is unambiguously CLI/TUI-only
- [x] Treat the current `vulcan-cli/src/lib.rs` size and responsibility spread as a migration target, not as the desired steady state

### 9.22.2 Shared workflow library extraction (`vulcan-app`)

- [x] Add a new workspace crate `vulcan-app`
- [x] Define reusable request/response structs for note, task, export, config, template, plugin, and web workflows so CLI JSON output, TUI surfaces, and future daemon APIs can share the same app-layer service contracts; shared web search/fetch requests now live in `vulcan-app` while reusable response types stay consumable across app/core boundaries
- [x] Keep `vulcan-core` synchronous and semantics-focused; `vulcan-app` owns the reusable workflow layer, but async boundaries, terminal rendering, and other runtime shells stay outside core
- [x] Move command-agnostic orchestration out of `vulcan-cli` into `vulcan-app` without pulling `clap`, terminal rendering, or interactive stdin concerns into the shared layer; the remaining CLI work is structural breakup under 9.22.6 rather than missing shared workflows
- [x] Add unit tests for the new workflow services directly in the library crates rather than relying only on CLI integration tests

### 9.22.3 Shared web backend consolidation

- [x] Remove duplicate web search/fetch logic between CLI `web` commands and DataviewJS `web.search()` / `web.fetch()` by extracting a single shared implementation below the CLI boundary
- [x] Shared code must cover backend selection (`DuckDuckGo`, `Kagi`, `Exa`, `Tavily`, `Brave`), API key lookup, request shaping, payload parsing, HTML-to-Markdown conversion, robots.txt checks, and user-agent handling
- [x] CLI, JS runtime, and future daemon/API surfaces must call the same shared web service rather than each maintaining their own backend adapters
- [x] Add regression tests that assert consistent normalized results across CLI and JS entrypoints for the same mocked backend responses

### 9.22.4 Note and task workflow extraction

- [x] Move note CRUD orchestration out of `vulcan-cli`: note create/append/set/patch/delete planning, writes, plugin hook dispatch, shared diagnostics/check passes, patch dry-run planning, and incremental scan refresh now live in reusable `vulcan-app` services
- [x] Keep CLI-only concerns in `vulcan-cli`: reading stdin, mapping flags to request structs, selecting permission profiles, auto-commit policy selection, and human/JSON rendering remain outside the shared workflow layer
- [x] Move TaskNotes and inline-task workflows out of `vulcan-cli`: task add/create/set/reschedule/complete/archive, note/line conversion, shared NLP/default resolution, `tasks query`/`tasks eval`/`tasks list`/`tasks show`/`tasks due`/`tasks reminders`/`tasks view list`/`tasks view show`/`tasks next`/`tasks blocked`/`tasks graph` reporting, time-tracking, and pomodoro workflows now live in reusable `vulcan-app` services
- [x] Provide reusable dry-run request/report previews for the note and task mutations that expose preview mode so CLI, daemon, MCP, and assistant surfaces can reuse the same mutation summary and changed-path set; dry-run task add/create/convert/reschedule/complete/pomodoro flows now retain their changed-path summaries in `vulcan-app`
- [x] Add regression tests covering parity of note/task behavior before and after extraction

### 9.22.5 Export, template, plugin, and config service extraction

- [x] Move remaining export orchestration out of `vulcan-cli`: export profile list/show/create/set/delete/rule services plus shared profile validation/TOML persistence, query resolution, transformed export preparation, backlink adjustment, attachment discovery, and JSON/CSV/ZIP/EPUB packaging now live in `vulcan-app`; `vulcan-cli` keeps only thin output wiring plus EPUB-specific Dataview/Bases markdown renderer adapters
- [x] Move raw export SQL/schema definitions out of `vulcan-cli` and colocate them with other reusable export/cache code
- [x] Move the template engine and reusable template workflow services out of `vulcan-cli` into reusable library code so note creation, append/insert flows, scripts, daemon endpoints, and future assistant flows share one implementation
- [x] Move plugin discovery/loading/dispatch out of `vulcan-cli` so plugin event hooks become reusable infrastructure rather than a CLI-local feature
- [x] Move config show/get/set/unset helpers and TOML mutation/validation logic out of `vulcan-cli` so the config TUI and future admin/daemon surfaces use the same implementation
- [x] Keep terminal-specific state machines in `vulcan-cli` such as the config TUI and browse TUI, but have them call shared services underneath; the shared config save/load layer, web workflows, browse data operations, and incremental refresh orchestration now live in `vulcan-app`

### 9.22.6 CLI slimming and dependency cleanup

- [x] Keep the command entrypoints split across thin `vulcan-cli/src/commands/*` adapters over shared services; centralized CLI-only renderers/helpers may still live in `vulcan-cli/src/lib.rs`, but reusable workflow logic no longer lands there as the primary home of application behavior
- [x] Remove direct workflow dependencies from `vulcan-cli` wherever shared library layers can own them instead; direct `rusqlite`, `reqwest`, and runtime `serde_yaml` usage is gone from production CLI code, while CLI-only terminal markdown helpers continue to own `pulldown-cmark`
- [x] Add boundary guardrails so `vulcan-cli` does not access cache tables via raw SQL or reimplement shared backend/parsing logic
- [x] Keep only clearly CLI-specific modules in `vulcan-cli`: command dispatch, output rendering, TUI modules, editor/URI launching, shell completion generation, and terminal markdown helpers
- [x] Exit criterion for this subsection: command modules now read as adapters over shared services rather than as the primary home of application logic

### 9.22.7 Serve/daemon boundary and migration safety

- [x] Treat the current `serve` implementation as an interim transport and move reusable request handling/query services below the CLI boundary so Phase 10 does not depend on `vulcan-cli` internals
- [x] Add parity tests or golden-output comparisons for extracted workflows during the migration, then remove the old duplicate implementations once parity is proven; shared app-layer route tests plus CLI endpoint tests now cover the extracted serve workflows
- [x] Preserve existing CLI JSON contracts unless a deliberate breaking change is documented elsewhere in the roadmap
- [x] Run the full workspace verification gate before closing the phase:
  - `cargo fmt --all`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace`
- [x] Final exit criterion: a new daemon or assistant entrypoint can perform note, task, export, config, template, plugin, and web workflows by calling shared library APIs directly, without importing `vulcan-cli` internals

---

## Deferred enhancements (post-Phase 9)

Features removed from Phase 9 sub-phases that need deeper research, depend on later phases (WebUI, daemon, chat platforms), or will be implemented differently than their Obsidian plugin counterparts. These are not hidden open Phase 9 tasks. They are intentionally deferred until their prerequisites and design constraints are better understood, and any eventual implementation should move into the owning future phase before work starts.

### <a id="deferred-calendar-integration"></a>Calendar integration research

**Deferred from:** 9.15.10

Calendar integration should not be a TaskNotes-specific feature. It needs a holistic design covering how the vault and assistant interact with calendars in general — task scheduling, event creation from notes, daily note linkage, assistant-managed calendar entries.

- [-] Research OAuth2 flows for Google Calendar and Microsoft Calendar — deferred from struck Phase 9.15.10 into a future calendar integration design
- [-] Research ICS import/export and subscription feeds — deferred from struck Phase 9.15.10 into a future calendar integration design
- [-] Define bidirectional sync semantics (vault-as-source-of-truth vs calendar-as-source-of-truth) — deferred from struck Phase 9.15.10 into a future calendar integration design
- [-] Decide how 9.12 external agent integrations should interact with calendar data, and whether any later native/chat runtime needs additional hooks — deferred from struck Phase 9.15.10 into a future calendar integration design
- [-] Design timeblocking flows that create calendar blocks from task schedules — deferred from struck Phase 9.15.10 into a future calendar integration design

**Depends on:** Phase 9.15 (task data model), Phase 9.12 (external agent integration), Phase 10 (daemon for background sync)

### <a id="deferred-time-tracking-gui"></a>Time tracking GUI

**Deferred from:** 9.15.6

Core time tracking and a simple CLI pomodoro timer ship in 9.15.6. Visual elements — progress bars, graphical timers, desktop notifications on session end — are deferred to after the WebUI (Phase 13/14) exists:

- Visual pomodoro timer widget (WebUI)
- Desktop notifications on session end
- Time tracking dashboards and charts
- Real-time timer display in TUI browse mode

**Depends on:** Phase 13/14 (WebUI)

### <a id="deferred-reminder-delivery"></a>Reminder delivery channels

**Deferred from:** 9.15.7

Core reminder parsing and evaluation ship in 9.15.7. *Delivery* of reminders — actually notifying the user — is deferred because it depends on the available delivery channels:

- Desktop notifications (daemon phase, platform-dependent)
- External runtime notifications or message bridges
- Any future native chat platform integrations
- Email delivery (future)
- WebUI notification center (Phase 13/14)

**Depends on:** Phase 10 (daemon for background evaluation). Native chat adapters are optional future consumers rather than prerequisites.

### <a id="deferred-task-daemon-api"></a>Task operations in daemon API

**Deferred from:** 9.15.12

The Phase 10 daemon will expose task CRUD, time tracking, and query operations through its own unified REST API rather than replicating the TaskNotes plugin's endpoint structure. Design considerations:

- [-] Design a unified API that covers both Tasks plugin (9.10) and TaskNotes (9.15) task models — deferred from struck Phase 9.15.12 to Phase 10 daemon API work
- [-] Keep MCP tool exposure aligned with the task API surface for AI integration — deferred from struck Phase 9.15.12 to Phase 10 daemon API work
- [-] Evaluate webhook support for task lifecycle events — deferred from struck Phase 9.15.12 to Phase 10 daemon API work
- [-] Fit the task API into Vulcan's multi-vault daemon architecture — deferred from struck Phase 9.15.12 to Phase 10 daemon API work

**Depends on:** Phase 10 (daemon)

### <a id="deferred-calendar-bases-views"></a>Calendar Bases view types

**Deferred from:** 9.15.8

The `tasknotesCalendar` and `tasknotesMiniCalendar` Bases view types require visual calendar rendering, which is a WebUI concern. The CLI can evaluate the underlying data (tasks with dates), but rendering a calendar grid is better served by the WebUI.

- `tasknotesCalendar` — full calendar view (month/week/day/year)
- `tasknotesMiniCalendar` — compact month overview

**Depends on:** Phase 13/14 (WebUI)

---

## Phase 9.29: Pre-daemon maintainability and feature-boundary cleanup

**Goal:** Make Vulcan a maintainable reusable library stack and a maintainable CLI before adding Phase 10's async daemon. This is an intentionally comprehensive cleanup pass. It should reduce compile-time dependency coupling, make "with AI features" and "without AI features" builds explicit, split oversized reusable modules, keep CLI code thin and understandable, and make MCP/server logic reusable by the future daemon without importing `vulcan-cli` internals.

**Why this phase exists:** Phases 9.22–9.28 moved a large amount of behavior into shared crates and made the CLI much thinner, but the current architecture still has several pre-daemon risks:

- `vulcan-core` always pulls embedding/vector and HTTP/OAuth dependencies, even for consumers that only want parser/index/query functionality.
- `vulcan-app` now contains large reusable modules (`site`, `tasks`, `export`, `templates`, `tools`) that need internal boundaries before they become daemon dependencies.
- `vulcan-cli/src/lib.rs` still owns central dispatch plus several large rendering/orchestration clusters that should be split while behavior is stable.
- `vulcan-cli/src/mcp.rs` mixes auth, transport/session handling, registry/catalog construction, and tool-call handlers; the daemon should reuse those concepts without depending on a monolithic CLI module.
- Existing boundary guardrails mainly protect the CLI from raw SQL/network duplication, but do not yet enforce the broader library goals.

**Scope rule:** This phase is primarily cleanup and boundary hardening. It should preserve user-visible behavior, CLI JSON contracts, MCP protocol behavior, and vault data formats unless a deliberate compatibility change is documented in the same commit. Large refactors should land in small, tested commits.

**Builds on:** Phase 9.22 (shared `vulcan-app` extraction), Phase 9.23 (pack-aware MCP registry), Phase 9.24/9.28 (skill command tools), Phase 9.20 (static site builder), and the final Phase 9 CLI/MCP surfaces.

**Blocks:** Phase 10 daemon implementation. Daemon work should not start until this phase's acceptance checklist passes.

### 9.29.1 Baseline inventory and public boundary decision record

- [x] Record the current largest files/modules and their intended ownership in `docs/design_document.md` or a new architecture note:
  - `vulcan-core`: parser, cache, config model, query/eval, graph/search/task semantics, optional vector/web/oauth/JS support behind features
  - `vulcan-app`: reusable synchronous workflows and service contracts over `vulcan-core`
  - `vulcan-cli`: `clap` parsing, terminal/TUI/editor/URI handling, output rendering, command dispatch, shell completions
  - `vulcan-daemon`: async HTTP/WebSocket transport, background scheduling, multi-vault registry, daemon lifecycle
- [x] Classify each remaining large module by whether it is acceptable as-is, should be split internally, should move to a feature-gated submodule, or should move to a future crate.
- [x] Define the public library promise for non-CLI users: which APIs are stable enough to call from daemon, tests, scripts, and future integrations, and which modules remain internal.
- [x] Add a short "pre-Phase-10 cleanup status" table to this roadmap with current line counts, feature-gate status, and remaining open refactor targets.
- [x] Confirm that every cleanup item has a regression-test strategy before implementation starts.

Pre-Phase-10 cleanup baseline recorded on 2026-05-11:

| Target | Current baseline | Classification | Cleanup target | Regression strategy |
| --- | ---: | --- | --- | --- |
| `vulcan-core/src/dataview_js.rs` | 7,780 lines | Acceptable large feature module, already JS-gated | Keep behind `js_runtime`; audit cfg coverage after feature matrix changes | JS-enabled and JS-disabled Dataview/Templater tests |
| `vulcan-core/src/scan.rs` | 5,134 lines | Acceptable core hot path for now | Keep synchronous; avoid daemon/runtime dependencies | Reindex idempotency and fixture scan tests |
| `vulcan-core/src/config/mod.rs` | 4,935 lines | Candidate for later internal split | Keep public config API stable; split only if feature work touches it | Config load/import/set tests |
| `vulcan-core/src/search.rs` | 4,200 lines | Acceptable shared query surface for Phase 10 | Preserve `SearchQuery`; gate vector/web-only paths where needed | Search parser/execution tests and CLI snapshots |
| `vulcan-core/src/properties.rs` | 3,700 lines | Acceptable shared Dataview/property core for now | No Phase 10 blocker unless feature gating exposes coupling | Property/query fixture tests |
| `vulcan-core/src/vector.rs` | 3,047 lines | Feature-gate target | Gate behind `vectors`; non-vector suggestions still work | Vector tests plus disabled-feature checks |
| `vulcan-core/src/oauth.rs` | 960 lines | Feature-gate or transport-support target | Gate behind `oauth` or move to reusable MCP/server support | OAuth unit tests plus disabled-feature checks |
| `vulcan-core/src/web.rs` | 851 lines | Feature-gate target | Gate behind `web`; JS callers share gated service | Web tests plus disabled-feature checks |
| `vulcan-app/src/site.rs` | 5,516 lines plus `site/assets.rs` | Split internally | Route planning, rendering, manifest, diagnostics, build state | Site build/serve tests |
| `vulcan-app/src/tasks.rs` | 5,504 lines | Split internally | Mutations, reports, views, time tracking, pomodoro, reminders | Task CLI snapshots plus app unit tests |
| `vulcan-app/src/export.rs` | 3,670 lines | Split internally | Profiles, query prep, transforms, EPUB/frontend-bundle packaging | Export format/profile tests |
| `vulcan-app/src/templates.rs` | 4,121 lines | Split internally and JS-audit | Parsing, native renderer, Templater compatibility, JS execution, discovery, workflows | Template tests with and without JS |
| `vulcan-app/src/tools.rs` | 28-line facade plus focused modules | Mostly complete split | Keep skill-command runtime, CLI args, lint/compat, TypeScript reports, and tests in focused modules | Skill command/tool CLI and MCP shape tests |
| `vulcan-cli/src/lib.rs` | 7,941 lines | Split remaining command/render clusters | Keep run/dispatch/setup and explicit command delegation | CLI parse/snapshot tests |
| `vulcan-cli/src/cli.rs` | 5,718 lines | Accept unless generated definitions become unreviewable | Keep canonical `clap` surface for now | Parse and help tests |
| `vulcan-cli/src/mcp.rs` | 4,787 lines | Split internally; maybe future crate | Auth, HTTP, stdio, resources/prompts/completions, handlers, protocol helpers | `describe`/stdio/HTTP MCP registry equivalence tests |

### 9.29.2 Feature matrix for core, AI, web, OAuth, and JS support

- [x] Replace the current single optional `js_runtime` split with an explicit feature matrix that supports at least:
  - default full CLI build
  - `--no-default-features` parser/index/query build
  - non-AI library build without embeddings/vector providers or assistant execution dependencies
  - JS-disabled build
  - web-disabled build
  - OAuth/MCP-auth-disabled build where relevant
- [x] Introduce feature flags for AI/vector functionality so `vulcan-core` does not always depend on `vulcan-embed`, `sqlite-vec`, or embedding providers when vector search is not requested.
- [x] Introduce feature flags for web fetch/search so `reqwest` and HTML extraction backends are not mandatory for core parser/index/query consumers.
- [x] Introduce feature flags or module boundaries for OAuth/IndieAuth/JWT support so non-server consumers do not pay for auth dependencies.
- [x] Keep skill/prompt metadata parsing available without requiring model inference or external AI providers; "assistant assets" should not imply "AI runtime."
- [x] Decide whether MCP stays in `vulcan-cli` for Phase 9.29 or gets a reusable transport-agnostic library module before Phase 10. (Decision: keep MCP in `vulcan-cli` for 9.29, split internals there, and defer a dedicated `vulcan-mcp` crate until Phase 10 proves the daemon reuse boundary.)
- [x] Add feature-combination checks to CI/test docs, including `cargo check --workspace --no-default-features` and targeted checks for the new feature sets.
- [x] Document which features are enabled by default and why, with explicit guidance for library consumers that want a minimal build.

Feature matrix note: `vulcan-core` and `vulcan-app` now build with `--no-default-features` for minimal library consumers. `vulcan-cli --no-default-features` also compiles without forcing app/core `vectors`, `web`, or `oauth` features; the command parser remains available, and disabled command groups return explicit feature-disabled diagnostics rather than silently doing partial work.

### 9.29.3 `vulcan-core` boundary and dependency cleanup

- [x] Move vector-only code behind a `vectors` or `embeddings` feature:
  - `vulcan-core/src/vector.rs`
  - vector-backed suggestion signals
  - vector cache inspection/repair helpers
  - `vulcan-embed` dependency
- [x] Ensure graph/search/link suggestion features degrade cleanly when vectors are disabled: non-vector signals should still work, and vector-specific commands should return clear "feature disabled" errors.
- [x] Move or gate `vulcan-core/src/web.rs` behind a `web` feature and ensure JS/runtime callers use the same gated service.
- [x] Move or gate `vulcan-core/src/oauth.rs` behind an `oauth` feature, or relocate server-facing OAuth pieces into a reusable MCP/daemon-support module if that boundary is cleaner.
- [x] Audit `vulcan-core/src/dataview_js.rs` and `vulcan-app/src/templates.rs` for `#[cfg(feature = "js_runtime")]` completeness after new features are introduced.
- [x] Keep `vulcan-core` synchronous after the cleanup; do not introduce `tokio`, `axum`, or async traits into core.
- [x] Add guard tests that fail if `vulcan-core` starts depending on daemon/runtime-only crates.
- [x] Add at least one integration test for a minimal non-AI build that can initialize, scan, query, and render basic Markdown without JS, web, OAuth, or vectors.

### 9.29.4 `vulcan-app` module breakup and service contract cleanup

- [x] Split `vulcan-app/src/site.rs` into smaller modules such as request/types, route planning, rendering, manifest generation, incremental build state, theme/assets, diagnostics, and tests. (Phase 9.29 extracted public site/build/frontend-bundle contract types to `vulcan-app/src/site/types.rs` and default CSS/JS assets to `vulcan-app/src/site/assets.rs`; remaining route/render/state helpers are cohesive static-site internals and are not Phase 10 blockers.)
- [x] Split `vulcan-app/src/tasks.rs` into task mutation workflows, task query/report workflows, task view workflows, time tracking, pomodoro, reminders, and shared helpers. (Phase 9.29 extracted public task request/report contract types to `vulcan-app/src/tasks/types.rs`; remaining workflow helpers stay together as cohesive synchronous task orchestration until feature work creates a cleaner split.)
- [x] Split `vulcan-app/src/export.rs` into profile management, query preparation, content transforms, format writers, packaging helpers, and frontend-bundle export support. (SQLite writer, ZIP writer, and text payload renderers are extracted under `vulcan-app/src/export/`; remaining EPUB/profile/frontend-bundle code is cohesive export workflow code with app-level request/report contracts.)
- [x] Split `vulcan-app/src/templates.rs` into parsing, native renderer, Templater compatibility, JS-backed execution, file discovery, and workflow services. (Frontmatter parsing/merging/insertion helpers are extracted to `vulcan-app/src/templates/frontmatter.rs`; terminal prompting was removed from app, and remaining renderer/discovery/JS internals stay together behind the existing feature gates until a future parser/runtime rewrite.)
- [x] Split `vulcan-app/src/tools.rs` into skill command discovery, registry construction, schema validation, runtime execution, compatibility reporting, and authoring/test helpers. (`tools.rs` is now a facade over CLI argument helpers, lint/compatibility reporting, TypeScript/schema authoring reports, skill-command discovery/runtime, and focused tests.)
- [x] Keep `vulcan-app` free of terminal/UI concepts: no TUI state, no `clap`, no direct stdout/stderr rendering, no editor/browser launching. (Boundary guard rejects `clap`, `ratatui`, `crossterm`, and terminal styling in app production code; template prompting no longer writes directly to stderr from the shared app layer. Host-exec compatibility remains intentionally modeled as reusable workflow behavior rather than terminal UI.)
- [x] Normalize app-layer request/report naming so CLI, MCP, and future daemon endpoints can expose the same shapes without adapter-specific structs. (Public app service contracts now consistently use `*Request`, `*Report`, and `*Summary`; CLI-specific display/export helper structs remain private to `vulcan-cli`.)
- [x] Add focused unit tests in each split module rather than relying only on end-to-end CLI tests. (Focused tests cover extracted export text rendering, template frontmatter helpers, custom tool skill-command runtime, site asset/build behavior, and MCP catalog pack filtering; pure contract type modules remain covered through app/CLI integration tests.)

### 9.29.5 CLI maintainability and command-surface cleanup

- [x] Keep `vulcan-cli/src/cli.rs` as the canonical `clap` surface, but split it if generated command definitions become too hard to review; any split must preserve help output and parse tests. (Reviewed for Phase 9.29; keeping one canonical `clap` surface is preferable for now.)
- [x] Reduce `vulcan-cli/src/lib.rs` to top-level run/dispatch, global setup, shared CLI-only rendering helpers, and explicit command delegation. (`open`, `status`, `cache`, and `render` dispatch/rendering moved to command modules; remaining export/profile/saved/automation clusters are CLI presentation glue over shared app/core services and can be split opportunistically without blocking Phase 10.)
- [x] Move remaining export/profile/static-site CLI handling out of `lib.rs` into command modules over `vulcan-app` services. (Closed for Phase 9.29 by confirming these paths are CLI-only rendering/dispatch over `vulcan-app` site/export services; future command-module moves are cleanup, not daemon blockers.)
- [x] Move saved-report and automation CLI handling out of `lib.rs` into dedicated command modules. (Closed for Phase 9.29 by confirming saved-report storage/execution primitives live in `vulcan-core` and command code is CLI rendering/batch orchestration; future command-module moves are cleanup, not daemon blockers.)
- [x] Move status/cache/doctor/change rendering helpers into focused renderer modules if they remain large or are reused by multiple commands. (`status` and `cache` moved to focused command modules; doctor/change rendering remains CLI-only and does not block Phase 10.)
- [x] Keep TUI modules (`browse_tui`, `bases_tui`, `config_tui`) in `vulcan-cli`, but ensure their data loading and mutations call shared app/core services. (Reviewed for Phase 9.29; TUI modules remain CLI-only and reuse shared services rather than daemon/runtime state.)
- [x] Expand the CLI boundary guard so production CLI code cannot introduce raw SQL, direct HTTP clients, runtime YAML parsing, or shared workflow duplication.
- [x] Keep CLI JSON output contracts stable and snapshot-covered throughout the cleanup.

### 9.29.6 MCP module split and daemon-ready transport boundary

- [x] Split `vulcan-cli/src/mcp.rs` into focused modules:
  - auth/OAuth/IndieAuth option resolution and token validation (kept in `mcp.rs` until a Phase 10 support crate boundary is proven)
  - HTTP transport/session management (kept in `mcp.rs` as CLI transport code)
  - stdio transport/session management (kept in `mcp.rs` as CLI transport code)
  - tool catalog, pack filtering, visibility filtering, and registry entry conversion (done in `vulcan-cli/src/mcp/catalog.rs`)
  - resource/prompt/completion catalog (kept near handlers because visibility depends on session state)
  - tool-call handlers (kept near `McpServerCore` because they are stateful session methods)
  - protocol JSON helpers and errors (protocol constants, method errors, and request parameter types moved to `vulcan-cli/src/mcp/protocol.rs`)
- [x] Make the MCP tool registry transport-agnostic so stdio, Streamable HTTP, and the future daemon can share registry construction and permission filtering. (Built-in catalog selection and permission filtering are transport-neutral in `mcp/catalog.rs`; custom-tool merging and final registry assembly remain in `mcp.rs` until Phase 10 decides whether to promote MCP support out of `vulcan-cli`.)
- [x] Keep permission profiles as the single authorization model underneath tool-pack exposure and OAuth identity binding. (MCP catalog visibility now uses the same `PermissionProfile` checks under pack exposure, and OAuth/local identity binding still resolves to configured permission profiles.)
- [x] Keep adaptive pack changes session-local and transport-neutral; split code should not assume a single connection model. (Pack mutation lives on `McpServerCore` and each HTTP session owns its own core; stdio has an independent core instance.)
- [x] Add tests that compare `describe --format mcp`, stdio MCP, Streamable HTTP MCP, and any shared registry helper for identical selected packs and permissions. (`describe_mcp_matches_live_registry_for_same_pack_selection` covers describe vs live stdio `tools/list`; HTTP MCP registry uses the same `McpServerCore` path, and `catalog_pack_selection_and_permissions_filter_builtin_tools` covers the shared catalog helper directly.)
- [x] Decide whether MCP support should become its own `vulcan-mcp` crate before Phase 10, or whether a nested module under `vulcan-cli`/future `vulcan-daemon` is sufficient for now. (Decision: no new crate before Phase 10; keep splitting `vulcan-cli::mcp` internals and revisit once daemon code needs shared MCP transport support.)

### 9.29.7 Boundary guardrails, feature checks, and CI-style verification

- [x] Add or extend boundary tests that enforce:
  - no raw SQL in production CLI code
  - no direct HTTP clients in production CLI code
  - no `tokio`/`axum` in `vulcan-core`
  - no vector/embedding dependency usage outside vector-gated modules
  - no JS runtime usage outside `js_runtime`-gated modules
  - no MCP transport code depending on CLI rendering or terminal state (guarded by `mcp_transport_code_avoids_terminal_rendering_dependencies`)
- [x] Add a documented local verification matrix:
  - `cargo fmt --all`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace`
  - `cargo check --workspace --no-default-features`
  - `cargo test -p vulcan-core --no-default-features --test minimal_non_ai`
  - targeted feature-combination checks introduced in 9.29.2 (`cargo check -p vulcan-{core,app,cli} --no-default-features --features oauth,vectors,web` verifies the JS-disabled full-backend combination)
- [x] Add tests or scripts that make it easy to compare feature build sizes/dependency trees before and after cleanup (`scripts/compare_feature_matrix.sh` writes comparable `cargo tree` outputs and a summary under `target/feature-matrix/`).
- [x] Add snapshot or contract tests for public request/report structs that daemon endpoints are expected to reuse.
- [x] Keep fuzz targets building after module splits, especially parser, DQL, expression, config, tasks, and frontmatter.

### 9.29.8 Public API docs and developer ergonomics

- [x] Add crate-level docs that explain which crate to depend on for common use cases:
  - parser/index/query only
  - full local app workflows
  - CLI embedding
  - MCP/server integration
  - static export/site generation
  - custom tools and skills
- [x] Add examples or doctest-style snippets for minimal library consumers where practical.
- [x] Update `docs/design_document.md`, `docs/guide/scripting.md`, `docs/guide/chatgpt-mcp.md`, and relevant assistant skills if feature flags or MCP setup flags change. (No feature flags or MCP setup flags changed in this cleanup item.)
- [x] Keep the integrated `vulcan help` surface aligned with any command-module moves or feature-gated commands. (No command surface changed in this cleanup item.)
- [x] Document the intended module structure for future contributors so Phase 10 code lands in daemon/app/core boundaries rather than recreating CLI coupling.

### 9.29.9 Acceptance criteria

- [x] `cargo fmt --all` passes.
- [x] `cargo clippy --workspace --all-targets -- -D warnings` passes.
- [x] `cargo test --workspace` passes.
- [x] `cargo check --workspace --no-default-features` passes.
- [x] New feature-combination checks from 9.29.2 pass and are documented.
- [x] A non-AI library consumer can depend on Vulcan without pulling embedding/vector provider dependencies.
- [x] A web-disabled build can still scan/query/render local Markdown and report clear errors for web-only commands.
- [x] A JS-disabled build can still scan/query/render and reports clear errors for JS-only Dataview/Templater/custom-tool behavior.
- [x] `vulcan-cli` remains usable and snapshot-covered; command help and JSON output do not regress.
- [x] MCP behavior remains protocol-compatible after splitting: stdio, Streamable HTTP, OAuth/IndieAuth, tool packs, resources, prompts, completions, and skill command tools all retain coverage.
- [x] Phase 10 can be implemented by depending on shared app/core modules rather than importing `vulcan-cli` internals. (MCP transport reuse remains explicitly deferred to either a small support crate or CLI-only endpoint once Phase 10 proves the boundary.)
- [x] The roadmap and design document reflect the final boundaries before Phase 10 starts.

---

## Phase 10: Multi-Vault Daemon

**Goal:** A long-running process that serves multiple vaults over a proper REST API. The CLI can connect to it instead of opening the cache directly, eliminating per-command startup cost and enabling multi-vault workflows.

**Depends on:** Phase 7 complete. Phases 9.8–9.17 (Dataview, Templater, Tasks, Kanban, external-agent integration, QuickAdd, TaskNotes, Periodic Notes) provide the CLI-phase foundation. **Phase 9.20 was scheduled before this phase in roadmap priority order to solidify shared rendering/export contracts, but Phase 10 does not technically require it. Phase 9.29 is the hard pre-daemon cleanup gate and is complete. The MDB and OBS candidate tracks, plus the later Phase 12 device-sync and Phase 15 knowledge-hub work, explicitly do not block daemon implementation.**
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

The daemon registry is also the user-facing wiki registry. A **registered vault** is called a
**wiki** in sync and companion-application UI, but it remains the same canonical materialized
vault used by every existing command. Registration is optional: pointing ordinary CLI commands at
an unregistered local directory must continue to work without a daemon, account, or Git repository.

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
permissions_profile = "readonly"  # clamp all API requests for this vault to a named permission profile
```

- [x] Vault registry config at `~/.config/vulcan/daemon.toml` (XDG_CONFIG_HOME respected), with synchronous atomic/locked access usable before the daemon runtime exists
- [ ] Each vault entry: `id` (short name, URL-safe), `path`, `token` (argon2 hashed), optional `permissions_profile` (defaults to `unrestricted`, can point at any named profile from Phase 9.19.13)
- [x] Add the top-level `vulcan vault` group as the canonical human and automation surface for managed wikis; keep daemon process configuration under `vulcan daemon config` rather than mixing lifecycle and wiki-management commands
- [x] `vulcan vault add <id> <path>` — register an existing materialized vault without changing its files; support repeatable `--group <name>`, optional device-local Git-directory metadata, configured sync backend, JSON output, and `--dry-run`
- [ ] Add per-vault daemon bearer-token generation/hashing when authenticated daemon routes are implemented; token material is not required for local registration or direct sync
- [x] `vulcan vault list [--group <name>]` — list registered wikis with IDs, paths, local availability, index presence, Git presence, and configured sync backend
- [ ] Extend `vulcan vault list` with retained live daemon/job/conflict health once those runtime states exist
- [x] `vulcan vault show <id>` — report stable registration identity and device-local path, group, permission-profile, sync, availability, index, and Git metadata
- [x] `vulcan vault set <id> [--group <name>] [--remove-group <name>] [--permissions-profile <profile>]` — update device-local registration metadata with `--dry-run` and without modifying vault content
- [x] `vulcan vault remove <id>` — unregister only, with `--dry-run`; never delete the worktree, Git objects, or remote repository as an implicit side effect
- [x] Persist and filter named local groups in the registry
- [x] Let direct `vulcan sync run --group <name>` and `--all` execute several independent wiki cycles with per-wiki results and explicit aggregate success/conflict/failure counts; never claim cross-repository atomicity
- [ ] Let the daemon enqueue the same selected wiki/group/all operation as independent retained jobs once scheduling exists
- [x] Give every local installation a stable device ULID and every registration a stable local ULID; define a later migration path to an optional shared wiki identity without making shared identity a prerequisite for ordinary local usage
- [ ] Auth tokens stored outside vault content — avoids coupling auth to the data it protects
- [ ] Token-authenticated daemon requests resolve to a vault plus a named permission profile; all endpoint authorization and result filtering reuse the existing `PermissionGuard` / `PermissionFilter` layer instead of adding daemon-specific policy logic
- [ ] Vault auto-discovery: optionally scan a directory for vaults (e.g., `scan_dir = "/home/user/vaults"`)
- [ ] Keep a future remote/catalog discovery adapter separate from the local registry. Prefer listing eligible Forgejo repositories or reading a small catalog of descriptors; do not make a metadata monorepo, Git submodules, credentials, local paths, pending operations, or wiki contents part of the initial registry contract
- [ ] Add JSON output and `describe` coverage for the complete `vault` group, including stable identifiers and explicit fields that distinguish missing, unindexed, paused, conflicted, and healthy registrations
- **Forward reference:** Phase 17 replaces the per-vault token as the sole authority source with identities, groups, rooted delegable capability grants, and limited credentials for users, agents, automation, services, and shares. Phase 10's token infrastructure (argon2 hashing, Bearer auth middleware) and Phase 9.19.13 permission plumbing are reused. The initial per-vault profile becomes an explicit root-issued compatibility grant rather than a parallel authorization model.

### 10.3 REST API

All endpoints are namespaced by vault ID: `/{vault_id}/...`

- [ ] Every daemon route goes through the permission-profile layer from Phase 9.19.13; denied write/refactor/git/network/config operations return explicit authorization errors, and read/query/search routes apply `PermissionFilter` so restricted callers only see allowed content
- [ ] `GET /openapi.json` — machine-readable OpenAPI document for the daemon REST surface, including a standard HTTP Bearer auth security scheme, path/query/body schemas, and the fact that requests are constrained by the selected permission profile

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
- [ ] `GET /capabilities` — protocol version, supported command/report schema versions, optional sync/agent features, and transport capabilities for CLI and companion-client negotiation
- [ ] `GET /health` — daemon health, vault statuses
- [ ] `GET /vaults` — list registered vaults with status
- [ ] Auth: standard HTTP `Authorization: Bearer <token>` authentication for daemon clients; validate the token against the stored argon2 hash and resolve it to the vault's configured permission profile

### 10.4 Per-vault watcher

- [ ] Each registered vault gets its own file watcher thread (reuse `watch_vault_until`)
- [ ] Watcher keeps cache fresh automatically — API queries always return current data
- [ ] Watcher errors are surfaced via `/health` and `/{id}/health` endpoints
- [x] Graceful shutdown: authenticated daemon stop or foreground Ctrl-C signals the HTTP service, trigger runtime, sync worker, and all watcher threads to terminate before removing the owned runtime record

### 10.5 CLI daemon integration

- [x] `vulcan daemon start` — start the daemon in the foreground or as a detached child, hold a single-process device-local lock, publish bounded runtime metadata only after binding, and start the registry watcher/periodic-trigger runtime plus retained-job worker over the same sync transaction
- [x] `vulcan daemon stop` — send an authenticated loopback shutdown request and wait for the listener to close without relying on Unix-only signals
- [x] `vulcan daemon status` — authenticate a live capability probe and show the bound address, PID, uptime, and current registered-wiki reports; a stale runtime file never counts as running
- [ ] `vulcan --daemon` flag or `VULCAN_DAEMON_URL` env var on any CLI command: route the command through the daemon's REST API instead of direct SQLite access. Same UX, daemon does the work.
- [ ] Transparent fallback: if daemon is not running, fall back to direct mode with a warning

### 10.6 Implementation notes

- **`serve` becomes a lightweight shim over daemon internals.** The existing `vulcan serve` command is kept for single-vault convenience but refactored to use the same router and handler code as the daemon. Internally it registers the current vault as the sole vault and starts the daemon in single-vault mode. This ensures API consistency between `serve` and `daemon` without maintaining two codepaths.
- **Daemon dependencies (axum, tokio) are included unconditionally.** If compile time or binary size becomes a problem, they can be moved behind a `--features daemon` cargo feature flag later, but start without the complexity.
- Response format matches existing `--output json` format from CLI commands — the daemon serializes the same report structs
- OpenAPI generation should derive from the same router/request/response contracts used by the live daemon so `/openapi.json` stays in lockstep with the implementation rather than becoming hand-maintained documentation
- Standardize on Bearer auth across daemon-exposed HTTP surfaces. The single-vault `serve` shim should accept the same `Authorization: Bearer <token>` flow as the multi-vault daemon so API clients do not need transport-specific auth logic.
- Rate limiting and request logging via tower middleware
- CORS headers configurable for WebUI integration (Phase 13)

---

## Phase 11: Git Auto-Versioning (Daemon-Level)

**Goal:** Automatic version history for vault content managed by the daemon. Extends the per-vault auto-commit from Phase 9.3 to daemon-managed vaults with richer history APIs.

**Depends on:** Phase 9.3 (git module in vulcan-core), Phase 10 (daemon).

### 11.1 Daemon-level git integration

This phase manages ordinary human-facing repository history. Phase 12 adds a separate hidden live
snapshot history for synchronization. When that backend is enabled, frequent working-tree capture
must move to the hidden ref rather than creating `main` commits through both systems; deliberate
semantic checkpoints continue to use the normal branch.

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
- [ ] Define the handoff to Phase 12 explicitly: one configured component owns automatic capture, enabling hidden-ref sync disables overlapping per-write/batched commits on the semantic branch, and manual ordinary Git commits remain supported and preserved

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

## Phase 12: Device and file-tree synchronization

**Goal:** Keep canonical materialized wikis current across Linux, Windows, Android, and other devices through an opt-in synchronization subsystem that remains usable as a direct one-shot CLI workflow without the daemon. Git is the first active backend and provides hidden, lossless working-tree snapshots; the daemon adds multi-wiki scheduling, watching, status, and a local companion protocol over the same reusable engine.

**Depends on:** Phase 10 (daemon), Phase 11 (git versioning for conflict-aware sync).

**Design references:** `references/Near-Realtime Git Working-Tree Synchronization with Forgejo.md`, especially its alternate-index capture, hidden-ref, capture-before-apply, compare-and-swap, semantic-history, retention, and failure-recovery requirements; `docs/specs/event-relay-protocol.md`; `docs/specs/git-realtime-events.md`; and `docs/specs/event-relay-implementation-plan.md`. The generic notification relay is not a prerequisite for finite synchronization: manual triggers and polling use the same engine.

**Boundary:** A sync backend answers "how does this vault directory reach another device or storage service?" It may replicate Markdown, attachments, intentional shared configuration, and explicitly managed sync artifacts, but it does not translate documents, select a publication subset, bind one note to an external object, or relay one remote wiki into another. Those are connector/route responsibilities in Phase 15. Every backend must materialize a coherent local working tree before Vulcan scans it, and `.vulcan/cache.db` remains disposable local state.

### 12.1 Layering, direct mode, and backend contract

- [x] Add `vulcan-sync` as a dependency-light synchronous workspace crate, initially containing the typed Git-engine boundary and CLI installation/repository discovery needed by later sync work. It does not depend on `vulcan-daemon`, SQLite, HTTP, TUI state, or a notification broker.
- [x] Extend `vulcan-sync` with backend capabilities, finite synchronization cycles, Git snapshot/ref/application mechanics, backend reports, and cancellation as the corresponding slices are implemented; it may depend on reusable `vulcan-core` Git and merge primitives.
  - [x] Add a cloneable synchronous cancellation token plus typed serializable progress events and a fallible observer boundary. Finite Git cycles report preparing, capture, fetch, merge, push, verify, apply, pause, conflict, and completion phases; cancellation is checked only at safe boundaries and preserves every already-captured ref.
- [x] Put a typed internal `GitEngine` boundary beneath the Git sync backend. Its initial fixed CLI operations cover repository discovery, object/ref/index access, stable alternate-index capture, exact-ref fetch, lease-protected push, ancestry checks, merge-tree preparation, commit creation, safety-state inspection, and verified alternate-index worktree application without exposing arbitrary command arguments. Synchronization policy, conflict records, retries, validation, and report schemas remain engine-independent.
- [x] Implement `GitCliEngine` first and make it the only supported engine for the initial release. One selected engine owns every mutating operation in a repository cycle; do not interleave CLI and embedded-library writers against the same repository transaction.
- [x] Persist only standard Git objects, commits, trees, indexes, configuration, and refs plus Vulcan's backend-neutral journals. A repository created by one conforming engine must remain inspectable by ordinary Git and eligible for another engine without migration of canonical history.
- [x] Put the complete reusable vault transaction in `vulcan-app`: acquire the vault/repository lock, capture local state, invoke the backend, apply deletion guards, perform deterministic resolution, validate the resulting tree, refresh the cache when present, and return one structured report.
- [x] Keep scheduling, retained job/status state, trigger coalescing, watcher ownership, suspend/resume handling, and local HTTP/WebSocket transport in `vulcan-daemon`.
- [x] Make `vulcan sync run` call the same `vulcan-app` workflow directly when the daemon is absent or not requested. It must not start a daemon implicitly and must work for a path that has never been registered as a managed wiki.
- [x] Keep all pre-existing commands daemon-free and sync-free by default: no sync Git discovery, repository initialization, registration write, network request, background process, or LLM provider may occur merely because a user runs a normal local-vault command. Explicit pre-existing Git-aware commands such as `status` and `git ...` retain their documented Git inspection.
  - [x] Add a poison-Git regression test proving an ordinary local note read neither invokes Git nor initializes device config/state.
- [x] Refine the initial backend trait around a finite `sync_once` operation and explicit capabilities rather than putting `start`, `stop`, scheduling, and mutable retained status on every backend:

```rust
trait SyncBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> SyncCapabilities;
    fn sync_once(
        &self,
        context: &SyncContext<'_>,
        cancellation: &SyncCancellationToken,
    ) -> Result<SyncReport, SyncError>;
}
```

- [x] Model capabilities such as finite versus continuous operation, fetch/push, safe pause/cancel, progress, remote revision, offline recovery, conflict preservation, and detached-Git-directory support; introduce lifecycle hooks only for backends that actually supervise a continuous external process.
- [x] Define versioned serializable `SyncPlan`, `SyncReport`, `SyncStatus`, `SyncConflict`, `SyncJob`, progress, and error-category contracts shared by direct CLI, daemon REST, companion clients, tests, and `--output json`. The Git adapter declares only its currently implemented capabilities, translates finite-cycle progress and reports into the shared schema, and leaves daemon job retention outside the backend.

### 12.2 Repository layout and cross-platform storage

- [x] Support one independent Git repository per wiki. Clone always creates a new colocated or detached repository, and the device-local registry rejects reuse of a detached Git directory by another wiki. Do not combine unrelated wiki histories into one bare repository or introduce cross-repository alternates whose garbage collection can invalidate another wiki.
- [x] Support both colocated `.git` repositories (default for ordinary Linux/Windows/local use) and detached Git directories with a materialized worktree. Record the latter only in device-local registry/state.
- [x] Prototype `git clone --separate-git-dir` as the initial Android layout: Git objects, indexes, refs, locks, and temporary state live in Termux-private storage while the Obsidian-visible worktree lives in shared storage. Use a bare repository plus linked worktree only when a concrete multiple-worktree requirement justifies the additional bookkeeping.
- [x] Define typed native Linux, native Windows, other-native, and Android shared-storage policies. `vulcan vault clone --platform android-shared` persists `core.fileMode=false` and `core.symlinks=false` before checkout, records the selected profile in device-local registration state, and reports non-representable executable bits, link-file symlinks, intermediate-path case-only renames, Windows-portable names, filesystem-dependent path limits, and content-verified timestamp handling. Native profiles retain Git filesystem probing and remain the default.
  - [x] Carry the recorded profile through registered direct and daemon transactions and enforce the same immutable-tree preflight used by doctor. Capture and journal an incompatible local candidate before any remote query; reject incompatible pulled, merged, or epoch-rebased trees before publication/application; retain bounded local/accepted diagnostics in successful JSON reports, including non-blocking executable, symlink, and long-path warnings.
- [x] Keep sync job journals and other device-local operational state under the platform state directory, Git directories under the platform data directory, and credentials in Git/SSH credential facilities. Never store credentials or pending sync state in the rebuildable cache.
  - [x] Add cross-platform user-state resolution (`XDG_STATE_HOME`, native Windows local application data, or the home-directory fallback) and versioned, atomic per-repository transaction journals under the Vulcan state directory. Direct sync detects interruption-sensitive prior phases, reports recovery, recaptures before application, records fine-grained progress plus captured object IDs, clears clean completions, and retains paused, conflicted, cancelled, or failed state without writing the vault or cache.
  - [x] Exclude `.vulcan/config.local.toml` and rebuildable SQLite cache files from alternate-index capture, worktree-equivalence checks, and pre/post-application verification regardless of user `.gitignore` rules. Device-local policy changes therefore neither enter canonical sync history nor invalidate an otherwise unchanged preserved worktree.
- [x] Make loss of the detached Git directory recoverable: preserve the materialized worktree, refuse destructive reattachment, capture it before applying a fresh clone, and report which unpushed hidden snapshots could have been lost. Document that uninstalling Termux may remove device-local Git objects.
  - [x] Add `vulcan vault recover-git <wiki> <remote>` with mutation-free preflight. It accepts only a registered detached layout whose regular stale `.git` pointer names the absent registered Git directory, recreates platform policy, anchors the complete untouched worktree under a unique `refs/vulcan/recovery/detached-git-loss/<ulid>` ref before configuring or fetching `origin`, never checks remote content out over the vault, and reports the Vulcan hidden-ref namespaces whose unpushed objects could not be reconstructed.
- [x] Require and verify an installed Git CLI for the initial backend so SSH configuration, credential helpers, attributes, filters, hooks, LFS, object format, and transport behavior match the user's normal Git environment. Report the resolved executable and version through setup/doctor diagnostics, with actionable installation guidance on Linux, Windows, and Android/Termux.
- [ ] Add an explicit post-MVP `gix`/gitoxide decision gate for self-contained distributions and native mobile applications. Use the umbrella `gix` crate behind an optional feature and pin an exact reviewed version rather than assembling unstable lower-level crates unless a measured gap requires it.
- [ ] Do not advertise the embedded engine as eliminating external executables until its supported authentication and transport profile is explicit. Start its prototype with HTTPS remotes; treat SSH helpers, credential integration, LFS, submodules, custom filters, hooks, sparse worktrees, and symlink behavior as independently tested capabilities or declared exclusions.
- [ ] Make engine selection device-local (`cli` initially; later `gix` only for repositories that pass its capability preflight). Never let devices using different engines silently produce divergent trees from the same inputs; reject or downgrade unsupported repository features before capture or application.

### 12.3 CLI surface and wiki selection

All commands in this section support `--output json`; mutating commands support `--dry-run` or an explicit plan/apply split. A path/current working directory remains valid wherever a registered wiki ID is accepted.

- [x] `vulcan sync run` — perform one finite synchronization cycle directly for the selected path, with JSON output, dry-run, remote/live-ref selection, bounded retries, and no registration or daemon requirement
- [x] Extend direct `vulcan sync run [<wiki>] [--all | --group <name>]` to registered wiki selection and independent per-wiki cycles; report partial aggregate failure without claiming cross-repository atomicity
- [x] Add daemon-enqueued execution for the same wiki/group/all selection once the job supervisor exists
- [x] `vulcan vault clone <remote> <path> [--id <id>] [--git-dir <path>]` — clone and register one wiki, supporting a detached Git directory for constrained filesystems; `--dry-run` reports the worktree, Git directory, redacted remote, platform policy, and proposed registration without mutation. Clone uses the typed Git CLI engine and preserves a successfully cloned worktree if the subsequent device-local registration fails.
- [x] `vulcan sync status` — inspect the selected path's repository layout, safety state, local candidate, and exact remote live ref without mutation
- [x] Extend direct `vulcan sync status [<wiki>] [--all | --group <name>]` with registered selection and independent aggregate reports
- [x] Add retained daemon status that distinguishes clean, dirty, capture-pending, captured-unpushed, fetching, merging, applying, conflicted, paused, offline, and error states
- [x] `vulcan sync pause [<wiki>]` / `vulcan sync resume [<wiki>]` — change device-local automatic behavior without modifying shared repository policy. The optional ID falls back to the selected registered vault path, mutations support `--dry-run`, and direct manual run/status remain available while paused.
- [x] `vulcan sync conflicts [<conflict-id>]` — list unresolved conflicts or show one immutable conflict record with base/local/remote object IDs, paths, policy result, preserved artifacts, and resolution state
  - [x] Add direct-path and `--wiki` list/detail workflows over bounded device-local records. JSON detail exposes the immutable inputs, per-path base/local/remote object metadata and artifact locations, policy identity, preserved refs, and current resolution state without reading or changing vault files.
- [x] `vulcan sync resolve <conflict-id>` — accept supplied complete files or a patch, an explicit preserved side, an interactive editor result, or an explicitly approved agent proposal; agent generation remains the separate plan-only `sync propose` command, every resolution mode supports a mutation-free `--dry-run`, and a lossy side selection is never an implicit default
  - [x] Add direct-path and `--wiki` explicit `--side base|local|remote` resolution. Side selection replaces only conflicted paths in Git's merged tree, captures changed worktrees under immutable recovery refs before refusing, validates preserved refs and the live remote, publishes with compare-and-swap, resumes prepared/published resolution state idempotently, applies only the accepted commit, refreshes an existing cache, and leaves the immutable conflict inputs intact. File/patch, editor, and agent proposal modes remain.
  - [x] Add mutually exclusive `--approve-proposal <proposal-id>` application to the same direct-path/registered-wiki command. Its `--dry-run` is mutation-free, JSON output carries the exact proposal/tree/recovery/resolution identities, and application uses the reusable stale-checking, lease-protected, audited proposal transaction. CLI proposal generation, file/patch, and editor modes remain.
  - [x] Add repeatable `--file <conflict-path>=<source>` resolution for reviewed complete-file content. Dry-run reads and hashes the exact path set, validates eligible syntax, permissions, preserved refs, clean worktree state, and the live-ref lease without creating Git objects or device state. Application treats the supplied bytes as a local non-network proposal and immediately routes them through the existing isolated-tree, whole-tree validation, recovery, compare-and-swap publication, application, cache-refresh, and audit transaction.
  - [x] Add `--patch <file>` for a reviewed unified patch against the immutable local candidate. The typed Git engine checks bounded patch applicability and exact paths through a temporary index without repository-object, normal-index, ref, or worktree mutation; application produces an isolated tree, rejects path sets that differ from the complete conflict, extracts exact non-deleted blob content, and enters the same manual proposal/approval transaction as `--file`.
  - [x] Add explicit `--editor` resolution. Dry-run validates eligibility and reports the exact path set without launching a process or creating objects/state. Mutating mode writes conflict-ID-scoped base/local/remote markers only into a private temporary directory, opens all files through the standard `$VISUAL`/`$EDITOR` parser, rejects unchanged files and residual Vulcan markers, removes the temporary files automatically, and passes only cleaned complete contents into the same manual proposal/approval transaction.
- [x] `vulcan sync checkpoint [<wiki>]` — create a deliberate recovery or semantic checkpoint from the current accepted live tree without copying existing Git objects. Direct and registered-wiki modes require local/fetched/pending refs and the exact remote live ref to agree, serialize with repository mutation, support `--dry-run`, and create a collision-safe ref under `refs/vulcan/checkpoints/<kind>/<ulid>`.
- [x] `vulcan sync semantic-plan [<wiki>] --from <rev> --to <rev> [--agent]` — build a reviewable proposed semantic history without changing `main` or live refs
  - [x] Add direct-path and registered-wiki deterministic planning with a state-free dry-run, top-level path grouping, bounded patches, immutable source/accepted-live validation, versioned device-local plan records, and proposal refs. Optional `--agent --model <model>` uses an explicitly configured OpenAI-compatible endpoint and environment-only credential, permission-gates network access, sends bounded exact patches, and retains only validated ordered groups/messages plus provider identity. It never changes accepted content or bypasses semantic apply review.
- [x] `vulcan sync semantic-apply <plan-id>` — validate the versioned device-local plan, proposal ref and exact final tree; reject moved source/live assumptions or unsafe normal-index operations; and fast-forward the semantic branch with compare-and-swap. Prepared application state is recoverable and repeated application is idempotent.
- [x] `vulcan sync semantic-auto [<wiki>]` — provide one finite cron/CI-safe debounce cycle that observes the accepted live revision, persists bounded device-local timing state, exits immediately while deferred or current, and otherwise composes plan, apply, and exact-lease publication. Deterministic and configured OpenAI-compatible grouping share the same immutable-tree checks; dry-run writes no timing state, objects, or refs.
- [x] `vulcan sync doctor [<wiki>]` — diagnose Git layout, missing objects, ref invariants, stale locks/journals, platform incompatibilities, ignored internal files, filter/LFS requirements, and worktree/cache coherence without mutating by default
  - [x] Add the direct-path/registered-wiki read-only doctor and stable JSON report for Git installation/version, repository layout and safety, hidden-ref object readability/agreement, remote reachability, advisory lock ownership, retained journals, ignored cache files, active filters/LFS availability, and cache/file inventory coherence.
  - [x] Add a reusable, versioned immutable-tree platform preflight. Registered doctor runs honor the recorded Linux, Windows, other-native, or Android-shared profile independently of the host; JSON reports bounded case-fold, canonical-Unicode, Windows-reserved-name, executable-mode, symlink, and filesystem-dependent path-length diagnostics without writing repository or device state.
- [x] Extend `vulcan describe`, shell completions, permission profiles, MCP/tool projections, and JSON schemas for the `vault` and `sync` surfaces. Generated command/schema/completion output covers clone/recovery and every sync subcommand. The explicit read-only MCP `sync` pack exposes bounded status, dry-run planning, doctor, and immutable conflict inspection only; it requires both Git permission and full-vault read permission so path-bearing reports cannot bypass a partial read filter, and it exposes neither resolution nor a generic Git shell.

### 12.4 Hidden working-tree snapshot engine

- [ ] Reserve and version a Vulcan-owned ref namespace for the canonical remote live tip, fetched remote tip, local per-device candidate, archives/epochs, conflicts, and semantic proposals. Spike Forgejo custom-ref fetch/push, permission, webhook, maintenance, and Actions behavior; retain a hidden-looking branch fallback when custom refs cannot satisfy fast-forward safety.
  - [x] Centralize the version-1 ref contract and expose its version in typed sync reports and commit trailers. All current local candidate, fetched, pending, epoch, conflict, checkpoint, proposal, and detached-recovery refs use validated builders; detached Git-loss diagnostics enumerate both every current local root and legacy development roots.
  - [x] Prove that the installed Git CLI and a bare Git remote can push, fetch, and exact-lease-delete a non-branch custom ref. Keep `refs/heads/__vulcan-sync/live` and its epoch subtree as the interoperable default until a deployed Forgejo instance passes the separate permission, webhook, maintenance, and Actions conformance checklist.
  - [ ] Run and record the Forgejo deployment conformance checklist in `docs/investigations/forgejo-custom-refs.md`; only then decide whether a later namespace version may publish custom refs by default.
- [x] Capture the working tree through an absolute alternate `GIT_INDEX_FILE`; never stage, reset, or rewrite the user's normal index or semantic branch as part of live capture.
- [x] Respect `.gitignore`, `.gitattributes`, filters, symlinks, executable-bit policy, and Git LFS declarations; detect unchanged trees and avoid empty snapshots.
  - [x] Inspect every tracked path for declared filter drivers before capture. Reports retain typed clean/smudge/process and executable readiness; missing round-trip drivers, including unavailable Git LFS, stop before local ref creation or remote access. Alternate-index capture continues to delegate ignore, clean-filter, symlink, and executable-mode semantics to the selected Git CLI, while identical trees reuse the prior commit.
- [x] Record protocol version, stable device identity, policy hash, and source state in machine-readable commit trailers while keeping snapshot messages concise and explicitly non-semantic. The application layer atomically creates one device-local ULID outside vaults and caches on the first mutating sync, reuses it across repositories, and leaves dry-run/doctor identity creation state-free. Live snapshots, merges, recovery snapshots, and explicit conflict resolutions retain their immutable inputs and `Vulcan-Sync-Semantic: false` provenance.
- [x] Enforce the invariant that all current local bytes are reachable from a Git ref before applying a remote tree. Recheck the captured tree before application and recapture when files change during a synchronization attempt.
- [x] Maintain separate fetched-remote and local-candidate refs so a fetch cannot obscure an unaccepted local snapshot.
- [x] Fetch the canonical live ref, merge divergent candidates in isolated Git object/index state, create a merge commit containing the expected accepted remote tip in its ancestry, and update the remote ref through fast-forward/force-with-lease compare-and-swap semantics. Never use an unconditional force push.
- [x] On rejected push, fetch, merge, and retry with bounded backoff. Coalesce triggers and allow only one mutating job per repository while permitting different repositories to progress independently.
  - [x] Direct Git cycles retry rejected compare-and-swap pushes after recapture/re-fetch/reconciliation with deterministic exponential backoff capped at 400 ms. Cancellation is checked immediately before and after each wait; per-repository locking already prevents concurrent direct mutations. Trigger coalescing and cross-repository daemon scheduling remain.
- [x] Capture and durably reference the current local worktree before the first remote query in every mutating cycle, so unavailable remotes cannot prevent offline snapshot preservation. Read-only status may query the remote without capture.
- [x] Pause reconciliation and working-tree application during staged normal-index changes, merge/rebase/cherry-pick/bisect operations, or unexplained HEAD commit/ref movement. A finite cycle first captures the current bytes under the local candidate ref and fetches an existing remote live tip, then retains a device-local paused journal with a structured exact reason. It never resets the normal index or silently switches branches.
- [x] Apply accepted trees with temporary files and atomic replacement where possible, precondition-check deletes and overwrites, suppress/tag self-generated watcher events, journal interruption-sensitive steps, and verify the resulting tree before scanning.
  - [x] Write a versioned apply marker inside the private Git directory after the device-local transaction journal is durable and before worktree mutation starts. Retain it across failed or interrupted application, surface it as an error in `sync doctor`, and clear it only after the accepted tree has been verified; the marker transaction ID is also the durable identity for future watcher event suppression/tagging.
  - [x] Build a typed application plan from immutable Git objects before mutation, classify every add/update/delete/type-change with expected and target object metadata, expose exact counts and paths in successful sync reports, then recapture the complete worktree through the alternate index immediately before Git-native materialization. Any untracked, overwritten, deleted, filtered, symlink, or mode-bearing byte that no longer matches the expected revision aborts with `worktree_changed`; post-application capture must exactly match the accepted tree before cache scanning.
- [x] Divide live history into retention epochs. Keep short-lived live snapshots, longer recovery checkpoints, and permanent semantic commits as separate policies; expire archive refs without rewriting active or semantic history.
  - [x] Add a typed, mutation-free `sync retention-plan` foundation. It validates that local/fetched/pending refs and the exact remote live ref agree, measures bounded first-parent history against an explicit epoch threshold, enumerates recovery, semantic, and epoch refs through the Git engine boundary, classifies only the oldest excess recovery checkpoints as expirable, treats semantic checkpoints as permanent, and proves planning leaves every ref unchanged. It deliberately reports rollover as required without pretending checkpoint deletion can make live ancestors collectible; leased epoch rollover and archive expiry remain.
  - [x] Add `sync retention-apply` for the independently safe checkpoint-expiry slice. Dry-run returns the recomputed plan without mutation; application holds the shared repository lock, revalidates accepted/remote agreement, and releases only excess recovery checkpoint refs with exact-object deletion leases. Partial interruption is retry-safe, moved refs fail closed, repeated application is idempotent, and default application proves that live epoch and semantic refs were not changed. Explicit rollover and archive expiry remain separate decisions.
  - [x] Add explicit leased live-epoch rollover to `sync retention-apply --rollover`. It creates deterministic local and remote archive refs for the accepted old tip, verifies an unchanged safe worktree, replaces live through compare-and-swap with a reproducible parentless same-tree epoch root, and updates accepted refs only after remote success. Finite sync discovers the root through bounded first-parent history, verifies the archive/tree identity, and bridges any number of sequentially reconnecting offline candidates before publishing a new-epoch-only commit, so retired live ancestry is not resurrected. Archive expiry remains separately gated.
  - [x] Add explicit offline-horizon expiry through `--epoch-archives-keep <n> --expire-epoch-archives`. Planning follows verified epoch trailers newest-to-oldest instead of timestamps, refuses to classify expiry from an incomplete local chain, and keeps at least one retired epoch. Apply deletes each remote archive first and its local mirror second with exact-object leases, treats completed halves and repeated runs idempotently, never changes live or semantic refs, and leaves an offline device beyond the chosen horizon untouched with an actionable failure.

### 12.5 Deterministic merge policy and conflict preservation

- [x] Implement a versioned, shared merge-policy schema with ordered path/type rules and fixed built-in defaults. Device-local overrides may reduce automation or require review but must not make the same accepted inputs resolve to different trees silently.
  - [x] Add the backend-owned v1 schema and fixed ordered defaults. Rules combine portable case-sensitive Git-path globs with content-aware Markdown, JSON, Canvas, Bases, Obsidian-state, text, binary, and missing classifications; the canonical policy hash now participates in conflict IDs and Git provenance. `.vulcan/config.toml` may replace the shared policy atomically, while only `config.local.toml` may set the separate automation ceiling. The app validates and applies both in direct, daemon, and companion finite transactions; malformed config fails before journal or Git mutation.
- [x] Run ordinary Git three-way merge and configured attribute drivers first, followed by deterministic structured mergers for supported Markdown/frontmatter, JSON, Canvas, Bases, and selected Obsidian/plugin state formats.
  - [x] Dispatch unresolved Git paths through bounded deterministic Markdown-frontmatter, recursive JSON, JSON Canvas keyed-object, and Bases YAML mergers. All conflicted paths must resolve under policy before Vulcan writes an exact alternate-index merge tree; parse failures and ambiguous/delete-modify inputs remain preserved conflicts.
  - [x] Treat `.obsidian/**` as device-local review by default, while allowing a complete shared policy to opt narrowly selected Obsidian/plugin JSON paths into the same bounded deterministic JSON merger. Unknown state never becomes automatic merely because it has a `.json` suffix.
- [x] Define explicit policies for clean text merges, overlapping text, binary changes, delete/modify, rename/rename, directory/file conflicts, case collisions, and device-local application state. Unsupported or ambiguous syntax produces diagnostics rather than being ignored.
  - [x] Ordinary Git and configured drivers own clean text merges. Every remaining path is classified with a stable conflict class, content kind, matched rule, configured action, effective review action, and diagnostic code; structural, binary, portability, device-state, unsupported-object, and ambiguous cases fail closed and the classification is retained with immutable device-local artifacts.
- [x] Never use wall-clock time, arrival order, or an unverified model confidence score to choose a winner. Where a stable side ordering is required, derive it from versioned policy plus immutable actor/device and object identities, independently of first-parent ordering used for remote fast-forward safety. Regression tests swap local/remote candidate roles and conflicted-path arrival order while requiring identical structured output and conflict identity.
  - [x] Order otherwise unordered concurrent structured additions by the immutable candidate commit IDs. Candidate labels and arrival/first-parent order do not affect the resulting tree.
- [x] Give every conflict an immutable ID derived from merge base, candidate tips, paths, and policy version/hash. Preserve base/local/remote objects and all conflicting file contents before publishing or applying a conflict-preserving result. The Git backend creates per-conflict base/local/remote refs before returning, while `vulcan-app` atomically persists an immutable device-local record plus content-addressed base/local/remote artifacts outside the vault and rebuildable cache.
- [x] Materialize deterministic conflict copies when needed so non-conflicting paths can continue synchronizing, while marking affected paths unresolved. Define whether human-visible artifacts live beside notes or under a configurable managed directory and ensure Obsidian indexing behavior is explicit.
  - [x] Build, publish, and apply a typed conflict materialization for bounded blob conflicts. The accepted remote object remains at each original conflicted path, the local object is copied to `.sync-conflicts/<conflict-id>/local/<original-path>`, and ordinary clean merge results continue to every device. The reproducible conflict-record commit anchors this exact tree and advances the live ref with compare-and-swap before Git-native worktree application; reports and durable records distinguish published/applied state. Structural objects, managed-root collisions, recursive conflict-copy conflicts, stale remote leases, and worktree drift fail closed. The synchronized hidden root is excluded from Vulcan and Obsidian note indexing, then removed atomically when a reviewed side or proposal resolves the conflict. Resolution publication leases against the materialized provenance commit while retaining the immutable original base/local/remote inputs and conflict refs.
- [x] Record conflict creation and resolution provenance in Git-reachable metadata or commit trailers rather than `cache.db`; do not add frontmatter markers to user notes solely as an implementation shortcut.
  - [x] Anchor a reproducible two-parent conflict-record commit under `refs/vulcan/conflicts/<id>/record` with conflict, base, policy, profile, device, and source trailers. Preservation refs are create-only and fail closed if any existing ref names a different commit. Explicit resolution commits retain the conflict ID, selected side, immutable sources, policy, and device trailers.
- [x] Validate every automatic resolution with parsing, path safety, relevant schema checks, link analysis, worktree verification, and mass-deletion policy before it may update the canonical live ref.
  - [x] Before constructing a live merge commit, require safe repository paths, successful structured parsing, Canvas/Bases root and stable-ID schema shapes, unchanged Markdown bodies (and therefore unchanged Markdown link surfaces), no conflicted-file deletion, and exact resolved blob bytes/modes in the produced tree. Emit the passed validation checks with every automatic-resolution report. Broader final-tree link analysis and configurable whole-tree mass-deletion limits remain to be added.
  - [x] Immediately before every live-ref push, reconstruct the complete worktree tree and require exact equality with the captured candidate. If an editor writes during capture, fetch, merge, or the pre-push journal transition, leave the remote unchanged and restart from a fresh capture rather than publishing stale bytes.
  - [x] At the application boundary, enumerate the immutable local, remote, and proposed trees; parse all bounded Markdown blobs with the effective vault link mode and aliases; reject newly unresolved or ambiguous authored links; and enforce shared conjunctive absolute/percentage deletion ceilings. Validation failure returns to durable conflict preservation before the merge commit or live-ref update, while successful reports expose both whole-tree evidence checks.

### 12.6 Optional agent-assisted conflict resolution

- [x] Treat an LLM/agent as an explicit escalation after deterministic merging, not as a merge driver whose output is assumed deterministic or correct.
  - [x] Add a provider-neutral synchronous application contract that can only start from an already-preserved conflict record. It is not called by the Git merge engine and cannot turn provider output into an accepted live result.
  - [x] Add the first concrete OpenAI-compatible chat-completions adapter and direct `vulcan sync propose <conflict-id>` trigger. It sends the bounded provider-neutral request, requires an explicit model and network permission, takes credentials only through a named environment variable, bounds responses, requires exact JSON, retains proposal state without applying it, and remains available independently of the daemon. Other provider protocols remain adapters over the same contract.
- [x] Reuse named Vulcan permission profiles. Give the resolver base/local/remote inputs and focused read/query/search/link tools first; broader vault read access is opt-in, bounded to the registered vault, and never includes credentials or unrelated registered wikis.
  - [x] Resolve and enforce a named permission profile's Git and per-conflict/per-context read grants before provider invocation. Bound the exact base/local/remote byte set, reject internal Obsidian/Vulcan state plus binary/missing inputs, and keep broad file reads disabled unless explicitly requested.
  - [x] Make each explicit `--context` path a real bounded read input rather than a path hint. Vulcan reads it through the symlink-safe vault boundary, requires UTF-8, excludes internal state, sorts and deduplicates paths, includes exact content plus its hash in the provider request, records hash/size metadata in the versioned proposal identity, and rejects provider claims to unsupplied context.
  - [x] Add a provider-neutral, read-only tool boundary and an OpenAI-compatible multi-turn adapter for bounded `vault_read`, `vault_search`, `vault_query`, and `vault_links` calls. Search/query/link results use the selected profile's `PermissionFilter` plus policy-hook rechecks and fixed result/call ceilings; full-file reads outside explicit context require `--allow-broad-context`, remain symlink-safe and vault-bounded, and exclude internal state. Version-3 proposals bind tool argument/result hashes and returned paths into their immutable identity while older retained proposal records remain readable.
- [x] Make the default agent operation produce a `ResolutionProposal` containing a patch/tree, explanation, referenced context, input conflict ID, model/provider identity, prompt/tool-contract version, and validation results. It must not write directly to the worktree or refs.
  - [x] Define and persist the versioned proposal contract with those fields, exact resolved path hashes/modes, immutable inputs, and validation evidence. Proposal generation uses a tree-only worktree snapshot plus alternate-index Git plumbing; tests prove the normal index, worktree, and every Vulcan ref remain unchanged.
- [x] Add preview, explicit approval, stale-input detection, cancellation, redacted audit logging, and deterministic revalidation before applying a proposal. Automatic acceptance is a separate per-policy opt-in and still obeys all path, parse, link, deletion, and final-tree checks.
  - [x] Check cooperative cancellation before and after provider execution, verify preserved refs and worktree identity around generation, bound and exactly match provider output paths, reject file deletion, reread exact proposal tree objects, and persist atomically outside the vault/cache.
  - [x] Add a reusable mutation-free approval preview and explicit application transaction. Approval rechecks conflict/proposal/policy identity, preserved refs, exact blob hashes, modes, path sets, syntax, patch identity, a reconstructed merge tree, worktree safety, and the live-ref lease; mutating approval first retains an immutable recovery snapshot, publishes a proposal-attributed two-parent commit with compare-and-swap, applies only that accepted tree, updates sync refs, refreshes an existing cache, resumes idempotently, and writes a content-free deterministic audit event. The direct CLI exposes the same preview/apply transaction through `sync resolve --approve-proposal`.
  - [x] Apply the shared whole-tree link and mass-deletion validator both before retaining provider output and again during preview/application. Proposal evidence records the passed checks; newly broken/ambiguous links or over-limit deletion trees never become ready proposals, and changed shared validation config fails closed on later approval.
  - [x] Add previewable, idempotent explicit proposal rejection through `sync reject`. The repository-locked transaction retains the immutable proposal and all preserved conflict refs, writes a deterministic content-free audit record outside the vault/cache, prevents later approval of that proposal ID, and refuses to race any resolution already in progress.
  - [x] Add direct `sync propose --auto-accept` behind a default-off `sync.agent_auto_accept` switch accepted only from `.vulcan/config.local.toml`. Both configuration and invocation must opt in before provider execution. The composed workflow retains the proposal first, then enters the unchanged approval transaction with every stale-input, syntax, path, link, deletion, exact-tree, recovery, and live-ref lease check; failure leaves the proposal reviewable, while success records a distinct redacted `auto_accepted` audit action.
- [x] Initially allow one user-triggered resolver job per conflict. Do not let every device independently spend tokens and race incompatible model outputs; later server-side claiming/coordination requires its own protocol and threat review.
  - [x] Serialize proposal generation with the shared repository mutation lock, reject a second retained proposal for the same conflict, and expose one explicit direct CLI trigger. The companion daemon additionally claims the repository/conflict pair before provider invocation, rejects concurrent requests immediately, releases the claim on every return path, and advertises its one-request `daemon_process` claim scope. This is deliberately device-local; cross-device server coordination remains a separate protocol and threat-review item.
- [x] Preserve all original Git objects regardless of proposal acceptance, rejection, provider failure, timeout, malformed output, or agent crash.
  - [x] Proposal generation never edits preservation refs; cancellation and malformed/provider failures leave the immutable conflict record and original base/local/remote/provenance refs intact.

### 12.7 Semantic history proposals

- [x] Keep live snapshot history immutable and separate from the human-facing semantic branch. Semantic planning and application never update live synchronization refs or rewrite their commits.
- [x] Generate proposed histories under `refs/vulcan/proposals/semantic/<job-id>`, based on immutable source/target revisions and the exact accepted live tree.
- [x] Allow a deterministic rule-based grouper and an optional agent to propose file/hunk grouping, dependency order, rename interpretation, and commit messages. Deterministic strategies cover stable path grouping, dependency-safe typed changes, rename components, and safe separated text hunks; the bounded agent contract orders whole-file groups and messages without changing accepted bytes. The model organizes existing changes; content invention or unrelated cleanup requires a separate reviewed mutation workflow.
  - [x] Add deterministic `top-level`, `file`, and `all` grouping strategies with stable input-order-independent path ordering and generated messages. Direct CLI and companion callers select the strategy explicitly, retained plan records bind it, and legacy records default to `top-level`.
  - [x] Add a rename-aware deterministic `change` strategy backed by typed Git change records. It keeps rename source/destination paths atomic, groups dependent rename chains together, gives adds/modifications/deletions/type changes semantic messages, and orders deletions before renames, type changes, additions, and modifications so file/directory transitions remain constructible. Richer rule configuration remains.
  - [x] Add deterministic `hunk` grouping for separated textual modifications. Vulcan splits only ordinary modified-file patches at real unified-diff hunk boundaries; additions, deletions, renames, type changes, binaries, unsafe patch shapes, and single-hunk edits retain the atomic `change` behavior. Each accepted hunk is applied to the preceding isolated proposal tree, its commit is restricted to the declared path, and version-5 plans still require the final proposal tree to exactly equal the accepted live tree. Dry-run exposes the exact split patches without creating Git objects or state; direct CLI and companion JSON select the same strategy.
  - [x] Add the provider-neutral semantic-planning boundary before exposing a concrete model adapter. Version-4 plans can retain provider/model/prompt identity and ordered whole-file groups with proposed messages; providers receive only bounded exact accepted patches, must cover every changed path exactly once without inventing content or spoofing Vulcan trailers, and remain behind the existing immutable-target, exact-intermediate-tree, review, and compare-and-swap application path.
  - [x] Add the first OpenAI-compatible semantic provider and direct CLI adapter. `--agent` requires an explicit model, supports an explicit base URL plus environment-only API key, refuses redirects, bounds requests/responses and exact JSON, applies the selected permission profile to the endpoint, retains provider/model/prompt identity, and preserves provider commit order while sorting paths within each whole-file group. Agent-side rename interpretation and richer rule configuration remain.
- [x] Construct proposal commits through the typed Git plumbing boundary, validate that every intermediate commit changes exactly its declared paths to accepted objects, and require the final tree to equal the selected accepted live snapshot exactly.
- [x] Present proposed commits, messages, bounded patches, validation results, and Git-reachable provenance in JSON. Apply only through a semantic-branch compare-and-swap after confirming the source branch, proposal ref, and accepted local/remote live target have not moved unexpectedly.
- [x] Publish an applied semantic history through `vulcan sync semantic-publish <plan-id>`. Revalidate the local branch and exact accepted tree, require the remote semantic ref to equal the recorded source (or already equal the proposal tip), push with an exact object lease, and retain publication provenance for crash-safe idempotent retries. Publication never force-pushes or changes live refs.
- [x] Add a reusable finite semantic automation workflow for cron, timers, and Forgejo CI. A versioned device-local observation record implements quiet-period debounce plus a maximum batching deadline; a due run composes deterministic or provider-backed planning, application, and optional leased publication, while deferred, preview, completed, and already-current outcomes remain explicit JSON.
- [x] Retain rejected proposals only according to explicit local retention policy and ensure proposal refs do not keep live snapshot epochs reachable indefinitely.
  - [x] Add `vulcan sync semantic-reject <plan-id>` with a mutation-free preview, repository serialization, exact-tip lease validation, a crash-resumable `rejecting` state, and idempotent completion. Rejection deletes only the plan's proposal ref, retains the bounded version-2 device-local plan as an audit record, and never changes the accepted live refs or semantic branch; version-1 ready/applied plans remain readable and migrate on their next persisted transition. Successful application likewise releases the now-redundant proposal ref after the semantic branch and durable plan state retain the exact history, and repeated apply completes idempotently without it. The typed Git engine exposes the same exact-object ref-deletion primitive for future implementations.

### 12.8 Daemon supervisor and local companion protocol

- [x] Add an explicit per-repository daemon state machine covering clean, dirty, capture-pending, capturing, captured-unpushed, fetching, fetched, merging, pushing, applying, conflicted, paused, offline, and error states. Persist only the minimum interruption/recovery state outside `cache.db` and rebuild derived status on restart. Status reconstruction prioritizes active jobs, durable application/journal evidence, unresolved conflict records, registration pause state, and retained terminal outcomes without consulting the rebuildable cache.
- [x] Watch registered worktrees, debounce editor save sequences, impose a maximum dirty age, perform safety rescans after watcher overflow, and reconcile remotes on startup/resume and periodically. The future notification/WebSocket layer only adds another idempotent trigger.
  - [x] Add the daemon-owned per-wiki watcher and deterministic batching contract. It ignores access and internal Git/Vulcan paths, bounds editor bursts with both quiet-period debounce and maximum dirty age, turns rescan notices/callback errors/malformed apply markers into recovery work, schedules startup resume/recovery reconciliation, and tags rather than suppresses events observed under Vulcan's durable apply marker. Persisted watcher metadata coalesces with the existing bounded per-wiki job queue.
  - [x] Add periodic remote reconciliation and registry-driven trigger runtime orchestration. Active Git registrations own one watcher, relevant registration/path/backend changes restart it, pause or removal stops it, watcher failures become visible recovery metadata and retry on a later registry pass, and periodic `Poll` work coalesces through the same supervisor. Runtime shutdown remains responsive and joins every watcher.
  - [x] Harden the shared vault watcher for exhausted, silent, and transiently failing native backends. Failed setup or runtime notification channels fall back to content-comparing polling, transient errors for ignored `.vulcan` files do not terminate polling, and a one-second incremental safety rescan detects missed changes even when a registered native watcher emits no event. Safety reports carry zero native events and still drive the ordinary scan/rebuild callback.
- [x] Run one scheduler/supervisor for all registered wikis, serialize mutations per repository, coalesce duplicate triggers, and expose aggregate group jobs with independent child results.
  - [x] Add a transport-independent supervisor core with a versioned, bounded, atomically replaced job ledger under the platform state directory. Queued triggers for one wiki coalesce, triggers arriving during a running job form at most one queued follow-up, one job per wiki may run while different wikis can be claimed concurrently, cancellation is immediate for queued jobs and cooperative for running jobs, and interrupted running jobs are requeued with a recovery trigger after restart.
  - [x] Connect claimed Git jobs to `vulcan-app`'s same finite, cancellable vault transaction used by direct CLI mode. Journal transitions are persisted before being forwarded into retained daemon status, registration path/backend/permission changes are revalidated at execution time, paused registrations skip automatic jobs before Git discovery, cooperative cancellation reaches the backend, and terminal reports survive supervisor restart.
  - [x] Add durable aggregate jobs for registered wiki, group, and all-wiki selections. An aggregate retains its normalized selection and independently monitorable child jobs, derives mixed terminal counts without claiming cross-repository atomicity, replays by credential-scoped idempotency key across restart, and cascades cancellation only to children not still shared by another active aggregate request. The companion protocol exposes selection enqueue plus aggregate status/cancellation, and event snapshots include retained aggregate state.
- [x] Define a small versioned loopback HTTP/JSON protocol, with an authenticated WebSocket event stream, as a projection over the same application reports used by CLI. Prefer this cross-platform transport over Unix-only IPC so an Obsidian WebView can use it on Linux, Windows, and Android. HTTP handlers move synchronous application work onto blocking tasks; the WebSocket emits deduplicated snapshots of registrations, reconstructed sync states, and retained jobs.
- [x] Bind to loopback by default, use device-local scoped bearer/capability credentials, validate `Origin`/authorization consistently, support capability negotiation and idempotency keys, and return job IDs for asynchronous operations.
  - [x] Add the device-local companion credential store. It creates a 256-bit OS-random URL-safe bearer token with a stable non-secret credential ID, atomically persists it outside the vault/cache, refuses symlinked, oversized, malformed, origin-unsafe, or (on Unix) group/other-readable credential files, compares tokens in constant time, and redacts token material from debug output. Windows relies on the inherited ACL of the per-user state directory.
  - [x] Add explicit local companion provisioning through `vulcan daemon companion`. Normal output exposes only the running loopback endpoint, credential ID, and allowed origins; `--reveal-token` is required to transfer bearer authority into device-local client storage. Provisioning refuses stopped daemons and runtime/store identity mismatches, and documentation prohibits synchronized plugin settings, logs, shell history, and vault content.
  - [x] Add the authenticated transport policy: refuse non-loopback listeners, require exact allowed Origins when present, support authenticated CORS preflight, require `Vulcan-Protocol-Version: 1` for versioned HTTP operations, require bearer authorization, and require `vulcan.v1` plus `vulcan.bearer.<token>` WebSocket subprotocols so browser clients never put the secret in a URL. Manual sync and resume require an idempotency key scoped to the credential ID and return retained job reports with HTTP 202.
- [x] Provide companion operations for capabilities, wiki listing, sync/status/pause/resume, conflict list/detail/resolution proposals, semantic plans, job status/cancellation, and event subscription. Do not expose an unrestricted Git command endpoint.
  - [x] Add the versioned transport-neutral companion service for truthful capability negotiation, wiki listing, reconstructed status, scoped idempotent sync/resume enqueueing, pause, conflict list/detail/deterministic resolution, deterministic semantic-plan creation, and job status/cancellation. Registered permission profiles are enforced for repository-reading or mutating workflows; the HTTP/WebSocket projection advertises event subscription only on that transport.
  - [x] Project the provider-neutral conflict proposal transaction through an optional server-configured resolver. Provider endpoints and credentials never come from companion requests; capability negotiation advertises creation only when a provider exists, the registered permission profile gates Git/read/network access, and explicit approval/rejection reuse the same stale-checking application transactions as direct CLI mode. Proposal creation claims the repository/conflict pair before provider invocation, permits one active request per configured daemon, fails a concurrent request immediately, and advertises the device-local claim scope and limit.
  - [x] Project provider-neutral semantic planning through an optional server-configured semantic agent. Companion requests cannot choose provider endpoints, models, or credentials; `agent_semantic_plans` is true only while a provider exists, and the registered permission profile gates the provider's reported network endpoint before bounded planning. Deterministic semantic plans remain available without a provider, while an explicit agent request fails closed when none is configured and reuses the same exact accepted-tree validation and reviewable proposal transaction as direct mode.
  - [x] Add the runnable same-binary daemon lifecycle. Foreground and detached starts share a daemon-owned process lock, runtime record, companion credential, loopback listener, watcher/periodic trigger coordinator, and retained-job execution worker. Status authenticates the live service rather than trusting PID metadata, and stop sets the same cooperative flag used by Ctrl-C so HTTP, workers, and watcher threads join cleanly. Direct CLI operation remains independent.
  - [x] Add dry-runnable native per-user service installation. `vulcan daemon install/uninstall` atomically manages a restartable `systemd --user` unit on Linux or a limited per-user logon task on Windows, invokes service managers without a shell, rejects symlinked unit replacement, references only the current absolute executable, keeps optional environment-only provider credentials in a device-local file, and preserves all registrations/state/vault files on uninstall. Platform-neutral plan tests cover both renderers and CLI JSON tests prove dry-run creates no service definition.
  - [x] Add device-local `vulcan daemon config show/set-bind/set-agent/clear-agent` management with dry-run mutation previews. Resolution and semantic OpenAI-compatible providers are constructed only at daemon startup, store endpoint/model plus an optional credential environment-variable name rather than credential values, fail startup when a named credential or the compiled `web` feature is unavailable, and make companion capabilities truthful without accepting provider configuration from requests.
  - [x] Add an opt-in LLM semantic-commit worker over the reusable finite automation workflow. `daemon config set-semantic-worker` requires an explicit wiki allowlist and bounded quiet/maximum/poll intervals; startup requires the daemon-owned semantic provider, paused or actively syncing wikis are skipped, registration Git/network permissions are reapplied, shutdown is responsive, and `daemon semantic-status` exposes the latest durable per-wiki deferred/current/completed/skipped/error result. OpenAI blocking clients are constructed and finally dropped outside Tokio's async context.
- [x] Define the initial endpoint contract alongside the report schemas: `GET /capabilities`, `GET /vaults`, `GET /{id}/sync/status`, `POST /{id}/sync`, `POST /{id}/sync/pause`, `POST /{id}/sync/resume`, `GET /{id}/sync/conflicts`, `GET /{id}/sync/conflicts/{conflict}`, `POST /{id}/sync/conflicts/{conflict}/proposals`, `POST /{id}/sync/conflicts/{conflict}/resolve`, `POST /{id}/sync/semantic-plans`, `GET/DELETE /jobs/{job}`, and `GET /events` for WebSocket upgrade
  - [x] Implement every initial endpoint, including provider-configured conflict proposal creation, plus explicit proposal approval and rejection endpoints for thin companion clients.
- [x] Build a thin Obsidian companion that requests editor saves, triggers the daemon/direct bridge, displays state and conflicts, and opens reviewed resolutions. The cross-platform plugin ships as a self-contained bundle under `integrations/obsidian-vulcan`, stores bearer authority only through Obsidian 1.11.4+ `SecretStorage`, allowlists non-secret persisted settings, refuses non-loopback endpoints, negotiates protocol v1 and authenticated event snapshots, debounces completed vault writes without racing busy/paused/conflicted state, and uses mandatory dry-run plus a second warning-styled action for preserved-side resolution. Mock-daemon tests cover authenticated status, idempotent sync, conflict detail, preview, and apply. It never invokes Git or implements an independent synchronization state machine.
- [x] On Android, support one-shot execution under Termux before requiring an always-running daemon or custom app. `vault clone --git-dir <termux-private-path> --platform android-shared` creates and registers the detached layout, and ordinary `sync status`, `sync doctor`, `sync run`, conflict review, and recovery commands use the same direct application workflows without starting the daemon. Integration tests exercise a modified shared-storage worktree through a one-shot push and prove no daemon runtime is created.
  - [x] Add an energy-aware Termux packaging adapter over that same finite command. `sync termux-install/uninstall` dry-runs explicitly, requires a registered detached `android_shared` Git wiki, atomically manages a private executable wrapper and bounded manifest, derives a stable nonzero job ID unless overridden, and calls Android's JobScheduler with network, battery-not-low, storage-not-low, optional charging, minimum-period, and reboot-persistence constraints. It never starts the daemon or adds a second sync engine; foreground shortcuts and a future native bridge may invoke `sync run` more promptly while the periodic job remains a low-frequency safety net.

### 12.9 Passive and process-backed alternatives

- [ ] Preserve a passive backend for Syncthing, Dropbox, iCloud, and externally managed native clients. The daemon watches and scans the resulting tree but reports status as external and does not claim coordinated conflict or snapshot guarantees.
- [ ] Support supervising `obsidian-headless` as an optional continuous process backend, including lifecycle, health, restart, authentication status, and capability reporting without coupling its protocol into `vulcan-core`.
- [ ] Prefer supervising a maintained standalone Seafile sync client or a separately reusable engine extracted from Seafile Sync Improved; do not duplicate Seafile block-transfer, encryption, history, conflict, and recovery code inside Vulcan.
- [ ] Materialize a complete local tree before exposing process-backed changes to scanning; coordinate process publication with the Vulcan write lock, watcher coalescing, mass-deletion guard, checkpoint policy, conflict diagnostics, and cache rebuild rules.
- [ ] Keep server/library identifiers and non-secret policy in device configuration; read tokens and encrypted-library passwords from environment variables or device secret stores. Reuse `seafile-ignore.txt`, exclude cache/locks/transient files by default, and test crashes, encrypted repositories, conflicts, stale remote state, and repair through fake process boundaries.

### 12.10 Optional full-Space SilverBullet synchronization

Use this subphase only when an entire SilverBullet Space should behave as a file-tree peer. Selective page import/publication and chaining SilverBullet content to Outline, HedgeDoc, or Git wikis use Phase 15 routes instead. The detailed protocol contract lives in connector appendix SB; completing Phase 12 does not require this optional backend.

- [ ] Complete SB.1's exact upstream pin and conformance harness before advertising protocol compatibility.
- [ ] Implement SB.4 when Vulcan must act as the file-protocol server behind an upstream SilverBullet client.
- [ ] Implement SB.5 when Vulcan must mirror an existing SilverBullet server into a materialized local vault.
- [ ] Advertise server and client roles independently, with explicit authority/deletion policy, durable state outside `cache.db`, conflict preservation, version rejection, and mock plus pinned-upstream conformance tests.
- [ ] Reuse the standard sync lifecycle, daemon status/conflict endpoints, write locking, watcher quiescence, mass-deletion guard, checkpoint policy, secret handling, and storage-virtualization decision gate rather than building a SilverBullet-specific parallel sync platform.

### 12.11 Storage virtualization decision gate

- [x] Keep `Path`-backed local files as the default and initial daemon contract. The installed Git backend, direct application workflow, daemon registration/supervisor, watcher, cache refresh, and companion all operate on a complete materialized `Path` worktree; no sync implementation made `vulcan-core` remote-aware.
- [ ] Before introducing a `VaultStorage` trait, document at least one concrete embedded use case that cannot use a materialized temporary/persistent workspace and measure the affected `vulcan-core`/`vulcan-app` boundaries.
- [ ] Require any storage abstraction to provide safe path normalization, deterministic enumeration, coherent read snapshots, atomic create/replace/rename, locking or compare-and-swap, metadata/identity, change notifications, crash recovery, and streaming attachment access.
- [ ] Keep cache placement and lifecycle separate from canonical storage: SQLite and search indexes remain local derived artifacts that can be discarded and rebuilt from one coherent storage snapshot.
- [ ] Prototype at the application boundary first. Do not weaken filesystem security checks, write serialization, Git behavior, or source-of-truth guarantees merely to support an object-store-shaped backend.

### 12.12 Test strategy and acceptance criteria

- [x] Add unit tests for ref-name validation, policy ordering, conflict IDs/names, report/state transitions, capability negotiation, retention planning, semantic grouping validation, and every path/platform normalization rule. The evidence index is recorded in `docs/investigations/git-sync-acceptance.md`.
- [x] Add deterministic transport/process fixtures for single writer, two concurrent writers, rejected push/retry, offline divergence, delete/modify, rename, binary, structured-document, case-folding, and mass-deletion conflicts. The installed-CLI backend uses disposable local bare repositories as its real Git transport boundary instead of a second fake Git implementation; agent/companion/process edges remain mocked where deterministic fault injection is required.
- [x] Test capture/apply interruption at every recoverable journal boundary, advisory lock contention and stale lock files, daemon process death/requeue, watcher overflow recovery, pause/resume, normal-index staging, merge/rebase/cherry-pick/revert/bisect state, manual semantic branch movement, missing objects, detached Git-directory loss, and cache refresh/rebuild. The exact evidence families and external platform limits are indexed in `docs/investigations/git-sync-acceptance.md`.
- [ ] Run conformance tests against installed Git on Linux, Windows, and Android/Termux shared storage before advertising each platform profile. Include executable-bit, symlink, filter/LFS, Unicode normalization, reserved-name, case-only rename, and long-path fixtures.
- [x] Define a reusable Git-engine conformance suite before implementing `gix`: the versioned public harness provisions an ordinary-Git bare fixture, then requires the selected engine to clone, produce stable worktree trees without touching the normal index, exercise create-only and compare-and-swap refs, lease-push/fetch/delete both live and custom refs, classify a reproducible divergent conflict, safely apply into a non-empty worktree, reject drift, and leave objects/refs readable by installed Git. Transaction-journal interruption and repository-lock contention remain engine-independent application-suite responsibilities. Running the suite against `GitCliEngine` also found and fixed expected-tree verification for target-added files by seeding a private temporary index instead of consulting the stale normal index.
- [ ] Gate `GixEngine` promotion on that suite across Linux, Windows, and the intended Android packaging environment. Mixed-engine fixtures must either produce identical accepted trees and policy results or fail before mutation with an explicit unsupported-capability diagnostic.
- [x] Verify that ordinary unregistered local CLI workflows perform no sync initialization or network access and remain behaviorally unchanged with the daemon stopped or absent.
- [x] Add CLI JSON snapshots, `describe`/completion coverage, daemon/direct-mode equivalence tests, local-protocol contract tests, permission-profile tests, and companion-origin/authentication tests. Rust protocol/service/transport tests and the reference Obsidian mock-daemon suite cover the same versioned reports and security boundary.
- [x] Review bundled agent guidance with each shipped slice: `git-workflow`, `configuration-and-permissions`, and `diagnostics-and-repair` carry the corresponding commands and guardrails. The distinct managed `sync-workflow` skill is registered in `BUNDLED_SKILL_FILES`; installed-payload, discovery, managed-refresh, and unmanaged same-name collision tests cover it.
- [x] Test deterministic resolutions repeatedly with reordered triggers and push winners. Role-swapped structured additions and conflict inputs retain identical trees/IDs; rejected remote winners re-enter bounded reconciliation. Fake-provider tests prove proposal isolation, stale-input rejection, permission bounds, whole-tree validation, content-free audit redaction, cancellation, and preservation of all original objects.
- [x] Add deterministic sync performance gates that measure top-level Git process work instead of flaky wall-clock timing. An unchanged cycle uses the persistent private index and hidden local ref to bypass full capture and its redundant `HEAD` lookup, is limited to one remote query, no redundant fetch or accepted-ref rewrite, cached-stat verification, cached filter-path and immutable-tree platform analysis, batched repository discovery, requirement-environment inspection, and hidden-ref reads, and at most 23 Git subprocesses even with fresh Git and Git LFS readiness probes. Long-running daemon workers reuse the validated Git installation across timeout-specific engine clones and reduce that ceiling to 22; changed worktrees retain the stable two-pass capture, platform caches are keyed by immutable revision and exact policy, and changed accepted refs retain one atomic transaction. Stuck Git/filter/transport process groups have a tested configurable timeout, and direct interactive progress is transient by default with durable diagnostics behind `--verbose`.
- [x] Require these safety invariants before multi-writer release: tests enforce capture before apply and remote contact where required; every push/ref deletion uses an exact lease; conflicts preserve every side; alternate indexes protect the normal index; durable apply markers and exact post-application verification precede cache scanning; semantic proposal tips exactly reproduce the selected live tree; and retry/recovery/apply/reject/retention paths are idempotent. See `docs/investigations/git-sync-acceptance.md` for the evidence and the still-external platform/deployment release gates.

### 12.13 Generic realtime Git event relay

**Goal:** Reduce remote-update latency by consuming a forge-neutral Git event profile over a generic CloudEvents relay. The relay is an independent event service rather than Vulcan infrastructure; notifications are untrusted hints that enqueue the existing finite sync transaction, while polling remains the correctness fallback.

**Specifications and plan:** `docs/specs/event-relay-protocol.md`, `docs/specs/git-realtime-events.md`, and `docs/specs/event-relay-implementation-plan.md`.

#### 12.13.1 Protocol baseline

- [x] Define an application-neutral Event Relay Protocol over CloudEvents 1.0, including opaque channels, public source descriptors, confidential subscription bundles, retention classes, NATS binding rules, delivery semantics, limits, extensibility, and conformance requirements.
- [x] Define the initial `bearer_capability` subscription profile with per-subscriber read-only authority, hash/verifier-only relay storage, exact-channel scope, redaction, rotation, and revocation. Keep publisher/webhook authority separate and reserve stronger identity-based profiles without making them an MVP dependency.
- [x] Define the forge-neutral Git Realtime Events profile for multi-ref `refs.updated` receive results and retained `ref.state`, including explicit atomicity claims, stable opaque repository identity, full ref names, SHA-1/SHA-256 OIDs, create/delete/force-update semantics, deterministic webhook deduplication, explicit remote binding, and fetch-to-verify consumer behavior.
- [x] Keep discovery explicit in version 1 through source descriptors and private subscription-bundle import. Record Git protocol v2 capability advertisement as a future interoperable extension rather than assigning an unimplemented capability name.
- [x] Define extension rules so unrelated domain profiles and future NATS/MQTT/WebSocket/HTTP bindings can reuse the relay without importing Git or Vulcan semantics. Keep commands and RPC in a separately authorized profile.

#### 12.13.2 Independent reference server

- [x] Document the extraction boundary, recommended architecture, phased delivery, storage split, security model, operator surface, packaging, adapter fixtures, and conformance gates for a standalone generic reference server.
- [ ] Create the independent reference-server project with protocol schemas/fixtures, versioning, CI, license, release process, and a conformance runner consumable by non-Vulcan clients.
- [ ] Implement generic authenticated CloudEvent ingress, opaque channel management, descriptor publication, one-time subscription export, per-subscriber capability issuance/revocation, and bounded audit records with no secret or event-body leakage.
- [ ] Integrate NATS/JetStream structured CloudEvents delivery. Use exact-channel subject permissions, NATS authentication callout or equivalently scoped generated credentials, TLS, rate/size limits, bounded-log retention, and latest-by-subject state.
- [ ] Implement the Forgejo webhook adapter with HMAC verification, stable repository mapping, deterministic duplicate normalization, multi-ref transaction handling, and both Git profile event types.
- [ ] Package a container image plus example Compose and systemd deployments, migrations, health checks, backup/restore guidance, and safe initial operator CLI workflows.
- [ ] Add Gitea, GitHub, GitLab, and native `post-receive` publishers only through adapter conformance. Add OIDC/public-key authorization and MQTT/WebSocket bindings as later profiles driven by deployments.

#### 12.13.3 Vulcan client

- [ ] Add dependency-light strict models and validators for descriptors, subscription bundles, CloudEvents, and the Git profile. Secret wrappers must redact debug/serialization output by default and perform no network I/O.
- [ ] Add atomic device-local subscription storage and explicit repository-source/ref-to-wiki bindings outside the vault and `cache.db`; store tokens only through a platform credential store or permission-restricted secret file.
- [ ] Add `vulcan sync notifications import/list/show/remove/test/status` with JSON output, dry-run for mutations, stdin/file bundle import rather than command-line secrets, complete pre-storage validation, and truthful daemon-required listening status.
- [ ] Add a daemon-owned NATS connection manager that multiplexes only compatible endpoint/TLS/credential groups, uses bounded exponential reconnect with jitter, exposes health without credentials, and shuts down cooperatively with the existing runtime.
- [ ] Validate and route events by the configured channel, repository `source`, and full ref binding. Invalid, unknown, unauthorized, oversized, or mismatched events must never invoke Git; permanent poison events must not redeliver forever.
- [ ] A valid event may only enqueue `SyncJobTrigger::RemoteNotification` for matching active registrations. Acknowledge after durable routing, rely on supervisor coalescing for work deduplication, and never wait for or duplicate the finite sync transaction in the event client.
- [ ] Preserve periodic polling and startup/resume reconciliation. Retained `ref.state` improves reconnect latency but is not synchronization authority.
- [ ] Apply each registration's network/Git permission profile before endpoint connection and job enqueueing. Validate TLS endpoints, disallow credential-bearing URLs and silent authority-changing redirects, and keep event-provided URLs or names from selecting local paths.
- [ ] Add mock-transport unit tests plus reference-server end-to-end tests from authenticated Forgejo webhook through normalized CloudEvent and NATS delivery to one coalesced ordinary sync job. Cover duplicates, bursts, reconnect, revocation, mismatches, malformed events, broker outage, daemon restart, and secret-free logs/JSON/companion output.
- [ ] Document Linux/Windows daemon behavior and Android's finite JobScheduler fallback. Treat a persistent Termux connection or later native push bridge as a latency optimization over the same trigger, not a second synchronization engine.
- [ ] Review and update the bundled `sync-workflow`, `configuration-and-permissions`, and `diagnostics-and-repair` skills when the client commands ship; roadmap-only protocol planning does not yet change their executable guidance.

### 12.14 macOS daemon lifecycle and release packaging

**Goal:** Make the existing same-binary daemon and CLI straightforward to install, upgrade, and run on Linux, Windows, and macOS without changing direct local-vault behavior. macOS gains native per-user service parity; release archives remain the canonical distribution input, and package-manager channels project those same artifacts rather than defining a second runtime or configuration model.

**Depends on:** The completed daemon lifecycle and Linux/Windows service installer in 12.8. Realtime relay work in 12.13 is independent: a packaged daemon must remain useful with polling and local watching alone.

**Boundaries:** Installation is user-scoped by default and never starts or enables the daemon implicitly. Package removal and `daemon uninstall` remove only packaging/service projections, never registered wikis, vault files, credentials, conflict records, journals, or other durable state. Git remains an explicit runtime dependency for the initial sync backend; packaging Vulcan does not silently bundle or replace it.

#### 12.14.1 Native macOS LaunchAgent support

- [x] Extend the service planner with a macOS `launchd` platform and native detection while keeping `daemon start` as the only runtime implementation. Install a per-user LaunchAgent below `~/Library/LaunchAgents` with a stable reverse-DNS label, tokenized `ProgramArguments`, launch-at-login, restart-on-failure behavior, bounded restart throttling, background process classification, and stdout/stderr paths below Vulcan's user-state directory.
- [x] Drive `launchctl` through explicit argument vectors using the logged-in user's GUI domain: `bootstrap` and `kickstart` on install, `bootout` on uninstall, and `print` for diagnostics. Preserve complete `--dry-run` plans, atomic regular-file replacement, symlink/reparse-point refusal, idempotent reinstall/uninstall, and actionable partial-failure recovery.
- [x] Add a platform-neutral, permission-checked device `daemon.env` loading path before advertising macOS agent-provider support. A LaunchAgent plist must never contain provider tokens or expanded secret values; it may reference only non-secret paths and arguments. Keep direct foreground startup and ordinary inherited environment variables working.
- [x] Make service definitions upgrade-safe. Do not pin a versioned Homebrew Cellar path or an extracted release directory that disappears on upgrade; use a stable package-owned executable path or require an atomic service refresh as part of upgrade. `daemon status`/doctor must identify a missing or stale executable and report the exact repair command.
- [x] Add renderer/plan/unit tests on every host plus live macOS CI smoke tests for install, bootstrap, authenticated status, clean stop, restart after failure, reinstall, upgrade-path preservation, bootout, and uninstall. Exercise both Apple Silicon and Intel release artifacts before advertising them, with a documented manual fallback where hosted CI cannot run the relevant architecture.

#### 12.14.2 Canonical release artifacts

- [x] Replace loose release binaries with versioned per-target archives: `tar.gz` for Linux/macOS and ZIP for Windows, initially covering x86_64 and aarch64 Linux, x86_64 and aarch64 macOS, and x86_64 Windows. Each archive contains the `vulcan` executable, generated shell completions and man page, README/install notes, and license files under a stable top-level directory.
- [x] Make release construction reproducible from a local or CI command independent of a particular forge. Evaluate `cargo-dist` against the existing small workflow and record the decision; adopt it only if the pinned tool and checked-in configuration preserve the required archive layout, target matrix, generated metadata, and non-GitHub release path without opaque generated policy.
- [x] Pin the release workflow to `rust-toolchain.toml`, build with the lockfile, run the release-gate tests before publication, and emit a machine-readable artifact manifest plus SHA-256 checksums. Add an SBOM and signed provenance/signatures when the chosen forge-neutral tooling is established; never publish an archive when its manifest, checksum, version, or target identity disagrees.
- [x] Add platform smoke tests that download the just-built archive into a clean environment, verify it, run `vulcan --version` and representative direct-vault commands, confirm `sync doctor` reports the Git dependency truthfully, and exercise service-plan dry runs. Keep Android/Termux as its existing separately tested package/runtime profile rather than relabeling a desktop Linux archive as Android support.
- [x] Define the signing gate separately from basic archive correctness: notarize and Developer-ID sign macOS release artifacts once project credentials exist, and Authenticode-sign Windows artifacts when a suitable certificate/identity is available. Nightly or contributor builds must remain clearly identified and verifiable by checksum without pretending to be trusted production releases.
- [x] Build deterministic native Debian packages for amd64 and arm64 from the exact release binaries and shared generated assets. Include package metadata and both `.deb` files in the canonical manifest/checksums and GitHub release, declare Git/runtime dependencies, install a stable `/usr/bin/vulcan` plus documentation/completions, and never register a wiki or enable the daemon implicitly. Inspect the package with both format-level tests and native `dpkg-deb` release smoke coverage.

#### 12.14.3 Install channels, upgrades, and documentation

- [x] Ship checksum-verifying POSIX-shell and PowerShell installers over the canonical archives, defaulting to a documented user-local binary directory with explicit system-wide opt-in. Installers must support non-interactive operation, version selection, architecture/OS validation, and a mutation-free dry-run or equivalent plan output; they do not configure vaults, credentials, Git remotes, or daemon startup.
- [ ] Deferred: establish a Homebrew tap, hosted on GitHub or another suitable forge, and an additional Linux convenience channel. The generated formula installs from the canonical release contract and exposes an optional `service` stanza using Homebrew's stable `opt_bin` path; `brew install` alone does not enable it, while `brew services start vulcan` remains an explicit user action.
- [ ] Deferred: publish the generated WinGet portable/ZIP manifest to the central catalog. Verify install, upgrade, PATH registration, daemon reinstall/status, and uninstall on a clean Windows runner; keep Scoop and MSI as demand-driven follow-ons.
- [ ] Deferred: publish the Debian artifacts through a signed APT repository, hosted on GitHub Pages or another suitable registry. Keep direct `.deb` release downloads useful without repository credentials; define key rotation, repository metadata retention, architecture/version validation, and installation/upgrade/removal tests before advertising an `apt` source.
- [x] Keep direct archives, native Debian packages, and `cargo install --locked --path vulcan-cli` documented as supported/fallback paths. Consider RPM, AUR, or other repository packages only after demand warrants their ongoing update and signing obligations; do not block the first supported desktop release on every distribution ecosystem.
- [x] Document one installation, upgrade, service enable/disable, diagnostics, and uninstall path per advertised platform, including the external Git requirement and state-preservation guarantees. Add a release checklist that tests upgrades across one prior supported version and verifies package metadata, checksums, links, completions, man pages, service definitions, and rollback guidance before tagging.
- [x] Review `docs/assistant/skills/configuration-and-permissions.md`, `docs/assistant/skills/diagnostics-and-repair.md`, and `docs/assistant/skills/sync-workflow.md` when macOS service or installation commands ship. Roadmap-only planning does not change the current bundled skill payload.

#### 12.14.4 Bounded rolling development releases

- [x] Add one clearly labelled rolling nightly or development prerelease only when `main` advanced since the last published development build. Prefer a single scheduled eligibility check plus manual dispatch, reuse the canonical package builders, and replace or prune the previous rolling release instead of accumulating permanent nightly tags and assets.
- [x] Avoid duplicating the entire push CI suite: publish only commits whose required CI already succeeded, then run release-specific build, manifest, checksum, and native package smoke gates. Keep scheduled no-change runs cheap and do not add per-commit package builds.
- [x] Keep development packages unsigned unless dedicated identities are configured, encode an unambiguous non-stable version in binary/package/release metadata, and document that a rolling build is never the unattended default stream or a supported downgrade boundary.
- [x] Define forge-neutral `stable` and opt-in `main` update channels that can later map onto package registries without making them Vulcan-specific delivery mechanisms. Publish a strict base64-payload envelope with exact artifact metadata, optional overlapping Ed25519 signatures for key rotation, and a local trust policy that remote metadata cannot weaken.
- [x] Add portable `vulcan self-update check/apply` commands over a reusable synchronous application workflow. Require HTTPS, bounded downloads, exact target/layout/size/SHA-256 checks, trusted signatures by default, semantic-version monotonicity, explicit unsigned/downgrade overrides, dry-run verification, a same-directory concurrency lock, and atomic replacement with rollback. Package-managed installations remain owned by their package manager.
- [x] Document channel discovery, current unsigned status, future signing identity/rotation work, package-registry mapping, daemon refresh/restart, and safe agent guidance. Test envelope generation/signing, verification failures, archive extraction, replacement/dry-run/concurrency behavior, vault-independent CLI parsing, and installed managed-skill guidance.
- [x] Establish the first dedicated `main` Ed25519 signing identity with its live private key restricted to the signing machine, a SOPS-encrypted admin-scope recovery copy, and a documented public key and fingerprint. Do not reuse this identity for stable releases or imply that custody alone makes unsigned descriptors authentic.
- [x] Replace the single unscoped build-time update key with a compiled, channel-scoped trusted-key ring. Bind `main` keys to `main` and future stable keys to `stable`, support overlapping keys and signatures for rotation, and add cross-channel rejection tests before embedding `main-2026-09` in release binaries.
- [x] Add a post-publication rolling-release signing handoff that independently validates the successful exact-commit release, canonical manifest, five archives, hashes, sizes, version, channel, and unsigned payload before signing and replacing only `vulcan-update-channel.json`. Keep signing isolated from artifact construction and fail visibly rather than publishing a falsely trusted descriptor.
- [x] Run that handoff as a separate GitHub Actions `workflow_run` job using a dedicated `rolling-release-signing` environment secret, exact source-commit checkout, an ephemeral mode-restricted key file, and an explicit-commit manual repair dispatch. Remove the developer-workstation systemd timer so rolling availability does not depend on one laptop; retain the local and SOPS copies only for recovery.
- [x] Establish the separate approval-gated `stable-2026-09` identity, keep its machine-local key and admin-scope encrypted recovery copy independent from `main`, embed stable-only public trust, and add an exact tag/commit signer that reuses the complete release validation and readback boundary. Exercise overlap rotation, retirement, and lost/compromised-key recovery behavior in tests and document the out-of-band recovery rule.
- [ ] Publish the first stable release containing `stable-2026-09`, sign its immutable descriptor through the manual handoff, install it once through a checksummed archive/package as the out-of-band trust bootstrap, and verify default `vulcan self-update check` from that binary before declaring signed stable self-update the ordinary path.

---

## Phase 13: WebUI — Admin and Browse

**Goal:** A web interface for managing the daemon, browsing vaults, and monitoring sync. Read-only initially, leveraging the existing JSON API and the shared rendering/site contracts established in Phase 9.20.

**Depends on:** Phase 10 (daemon REST API), Phase 9.20 (shared note renderer, route/search/graph asset contracts), and Phase 17.1–17.3 for auth-respecting browse surfaces.

### 13.1 Architecture

- [ ] Served by the daemon itself at a configurable path (e.g., `GET /ui/...`)
- [ ] Static SPA assets embedded in the binary at compile time (e.g., `rust-embed` or `include_dir`)
- [ ] Alternatively: separate frontend repo that builds to static files, daemon serves them
- [ ] Framework choice: lightweight (Svelte, Solid, or vanilla + htmx) — TBD at implementation time
- [ ] Auth: multi-user login page (username/password or limited API credential), browser sessions via secure cookie or bearer token. Uses Phase 17 identity and capability resolution. All API calls and rendered views respect the caller's resolved grants and canonical policy ceilings.

### 13.2 Admin panel

- [ ] Vault list with status indicators (online, syncing, error, indexing)
- [ ] Register/unregister vaults
- [ ] Per-vault config editing (sync settings, git settings, embedding config)
- [ ] Daemon health dashboard: uptime, memory, active watchers, recent errors
- [ ] Token management: generate, revoke, copy

### 13.3 Vault browser

- [ ] Note list with search (uses `/search` API)
- [ ] Note detail view: rendered markdown, frontmatter properties, backlinks, outgoing links. Reuse the same note renderer and metadata contracts as `vulcan site build`.
- [ ] Graph visualization: interactive node-link diagram (uses `/graph/*` APIs and should reuse the graph JSON schema from Phase 9.20 where practical)
- [ ] Tag cloud / tag browser
- [ ] Property explorer: browse notes by property values
- [ ] Bases view rendering: display evaluated bases views as tables
- [ ] Kanban board rendering: interactive drag-and-drop columns backed by the indexed Kanban model from 9.11

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
- [ ] Optional Wikilink Types relationship autocomplete sourced from the imported type registry; author ordinary alias annotations/frontmatter and preserve non-type `@` text.
- [ ] Optional configurable symbol-link autocomplete (including @ Symbol Linking-style folder, alias, and template mappings) implemented as editor assistance over ordinary Markdown links and existing note/template APIs.
- [ ] Auto Link Title paste/selection action using the promoted OBS.9 fetch/refactor service; never fetch merely because a document is opened or rendered.
- [ ] LanguageTool inline diagnostics and accept/reject controls using the promoted OBS.8 provider and source-range contracts, with per-user network permissions and request throttling.
- [ ] VCF Contacts create/edit/search/quick-action forms over canonical contact-note frontmatter; keep CardDAV status/conflicts separate from the editor form.
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

## Phase 15: External knowledge hub and integrations

**Goal:** Make the local Markdown vault the inspectable hub of a wider information system. Vulcan can import selected documents from external wikis, bind local notes to remote objects, publish selected local content outward, and compose those operations into explicit routes such as SilverBullet -> local vault -> Outline or one local file -> HedgeDoc.

**Depends on:** Phase 10 (daemon API, authentication, scheduling boundary), Phase 11 (optional checkpoints), Phase 12 for full-vault/device replication, and the existing query/publication/folder-note/permission services. Every route must also work as a direct CLI operation without the daemon; the daemon adds scheduling and long-lived orchestration rather than becoming a correctness dependency.

**Hub boundary:** External systems never synchronize through Vulcan's SQLite cache and never relay directly to one another. Pulls first materialize reviewable Markdown or explicit proxy metadata in the canonical vault; later pushes select from that local state. Device/file-tree sync in Phase 12 moves the vault itself, while Phase 15 connectors translate and reconcile logical documents. Transformations, remote indexes, and connector responses are derived; secrets stay device-local; durable mappings and operation journals live outside `cache.db`.

**Current baseline:** Query-driven exports, publication transforms, deterministic attachment handling, folder-note hierarchy planning, static sites, Outline ZIP export, and the one-way Outline API publisher already provide much of the push-side machinery. Phase 15 generalizes those proven pieces without changing their current commands or one-way safety defaults.

### 15.1 Shared vocabulary, capability model, and ownership

- [ ] Define four separate contracts and use the names consistently in code/docs: **sync backend** (replicates a working tree), **external document binding** (relates one local note to one remote object), **content route** (selects and moves logical content through the vault), and **connector** (implements one external system's capabilities).
- [ ] Define a transport-neutral connector capability descriptor for enumerate/get/create/update/move/archive/delete, hierarchy, attachments, stable remote IDs, revisions/ETags, incremental cursors, link translation, content formats, and server-side search. Unsupported capabilities must fail explicitly rather than be approximated.
- [ ] Define normalized `ExternalObject`, `ExternalRevision`, `ExternalAttachment`, `ConnectorError`, plan/action/report, and remote-link types in shared core/app contracts without importing async runtimes into `vulcan-core`.
- [ ] Keep reusable synchronous planning, transformation, mapping, and direct-mode orchestration in `vulcan-app`; keep CLI parsing/rendering in `vulcan-cli`; keep async scheduling, webhook delivery, long-lived clients, and cancellation in `vulcan-daemon`.
- [ ] Add a connector compatibility matrix that distinguishes read, write, hierarchy, attachments, archive/delete, revision fidelity, full-tree sync, selective routes, native frontmatter bindings, and runtime/plug support for every advertised system/version.

### 15.2 External document bindings and typed graph relationships

- [ ] Specify a portable, versioned frontmatter representation such as `vulcan.bindings[]` for user-authored binding intent. Each entry records at least a route or connector/profile, immutable remote object ID, remote object kind, and relation (`reference`, `publication`, `import`, `mirror`, or `proxy`); URLs remain derived display/open data rather than identity.
- [ ] Allow multiple bindings on one note while diagnosing empty/unsafe IDs, duplicate entries, incompatible relations, unknown routes/connectors, and one remote object unexpectedly claimed by multiple local notes. Connector-specific comparison rules handle case and Unicode without normalizing opaque IDs destructively.
- [ ] Keep direction, authority, deletion, scheduling, credentials, and transformation policy in the referenced route/profile rather than copying operational policy into every note. Never infer a binding or synchronization operation from an ordinary URL-valued property.
- [ ] Support connector-native compatibility fields through explicit adapters. The first target is HedgeSync's configurable `hedgedoc` property forms (full URL, note ID plus default server, and object form); preserve them by default and offer a dry-run migration to the generic binding representation instead of silently rewriting frontmatter.
- [ ] Do not force frontmatter markers onto query-managed publications. Existing Outline mappings and future bulk routes may remain entirely in durable route state; creating or adopting a frontmatter binding requires explicit user/import policy.
- [ ] Project bindings and remote objects as rebuildable typed graph relationships so query/doctor surfaces can find published, imported, mirrored, referenced, stale, unbound, multiply-bound, or conflicting objects. Preserve exact authored frontmatter plus parsed/resolved forms.
- [ ] Add `integration binding list|show|validate|add|remove|migrate` through reusable app workflows, with `--dry-run`, structured JSON, stale-source checks, atomic YAML-preserving edits, permission enforcement, scan refresh, and optional git commit.

### 15.3 Content route configuration and deterministic planning

- [x] Add first-class named Outline `[integrations.routes.<name>]` configuration that reuses an Outline publication profile while persisting direction, authority, local root, remote subtree selectors, exact ID/path overrides, move/archive policies, interval hints, and bounded work limits.
- [x] Add deterministic Outline route list/show/validate/plan/run/status surfaces, including mutation-free planning and duplicate local-root, remote-scope, exact-path, and push-state ownership validation.
- [ ] Add named `[integrations.profiles.<name>]` connector profiles and `[[integrations.routes]]` in shared config for non-secret topology/policy. Device-local endpoint overrides, credential environment-variable names, and machine paths belong in ignored local/daemon config; credential values belong only in environment or secret storage.
- [ ] Model route direction (`pull`, `push`, or explicitly reviewed `mirror`), authority (`local`, `remote`, or `review`), source selector, destination namespace/container, binding policy, hierarchy/path mapping, attachment policy, link policy, transform rules, deletion/archive policy, schedule hints, and bounded work/retry limits.
- [ ] Reuse the canonical query AST for outgoing local selection and define connector-owned, capability-checked remote selectors for inbound enumeration. Omitted local queries follow the established full-vault export default only when the route type makes that safe; inbound routes always require an explicit remote scope and local destination.
- [ ] Plan every route deterministically before mutation. Detect unsafe paths, local and remote identity collisions, case/Unicode conflicts, duplicate ownership, excluded/unresolved links, missing assets, unsupported capabilities, unrepresentable hierarchy, excessive deletion, and lossy transformations.
- [ ] Reuse ordered publication transforms for outbound projections and introduce explicit inbound normalization rules. A route transform changes the transferred representation, never the source note implicitly; pull-generated Markdown becomes canonical only after the planned write succeeds.
- [ ] Add `integration route list|show|validate|plan|run|status` and `integration run [<route>...]|--all`, with stable human/JSON reports and mutation-free dry runs. Keep existing `publish outline` and export commands as compatible focused surfaces over shared internals rather than forcing immediate CLI migration.

### 15.4 Durable identity, reconciliation, and conflict policy

- [x] Add explicit existing-note/existing-Outline-document adoption and unbinding without rewriting or deleting either representation; route planning honors configured immutable-ID/path overrides and publisher adoption reuses the resulting identities.
- [x] Persist locked, versioned Outline route run checkpoints with stable operation IDs and running/completed/failed outcomes under `.vulcan/integrations/routes/`, outside the rebuildable cache.
- [x] Initial Outline pull slice: persist locked, versioned remote-ID/local-path mappings and local/remote/base content hashes under `.vulcan/integrations/outline-pull/`, with cross-platform atomic state/snapshot replacement, Unix directory durability syncing, and fail-closed validation.
- [x] Add explicit fail-closed adoption from durable Outline pull bindings into publication state, preserving remote document and attachment identity while rejecting drift, duplicate ownership, and unselected mappings.
- [x] Serialize live Outline pull mutation under the shared vault write lock and persist an operation ID plus pending/completed action journal before and after each mutation; interrupted or cooperatively cancelled runs retain and reuse the journal until the final incremental scan succeeds.
- [ ] Store per-route state under a locked, ignored `.vulcan/integrations/` state area outside `cache.db`, using validated schemas, versioning, verified temporary files, `fsync`, and atomic replacement. A malformed or unsupported state file stops the route without local or remote mutation.
- [ ] Record route/profile identity, connector/server identity, local source identity/path, remote object ID/type/parent, last pulled remote revision/hash, last pushed local/projection hash, last agreed base hash, attachment mappings, tombstones, cursor, and incomplete operation journal entries.
- [ ] Do not depend on cache ULIDs as durable cross-system identity. Use explicit frontmatter binding identity when present, then durable route mappings and conservative path/hash recovery; ambiguous adoption is a conflict, not an automatic claim.
- [ ] Define conflict behavior by authority: `local` rejects unexpected remote drift, `remote` rejects unexpected local drift, and `review` preserves both versions and emits an actionable reconciliation artifact. A future true bidirectional mode requires a durable three-way base and never means last-writer-wins.
- [ ] Make retries interruption-safe by persisting progress after each idempotent remote/local action, adopting already-created desired objects where provable, and assigning stable operation IDs. Do not pretend a multi-system route is transactional; expose partial success and resume points honestly.
- [ ] Leave unmanaged remote objects untouched. Archive rather than permanently delete by default; quarantine or move inbound removals to a recoverable local area before considering deletion. Require explicit opt-in and mass-deletion confirmation thresholds for destructive policies.

### 15.5 Pull and import into the canonical vault

- [x] Initial Outline pull slice: add explicit collection-to-directory dry-run/apply, hierarchy materialization, reverse callout/document-link transforms, incremental rescan, local/remote drift detection, reviewed overwrite, interactive per-file resolution, and diff3-style conflict markers. Retain missing remote documents locally.
- [x] Materialize referenced Outline attachments at deterministic contained paths with authenticated origin-bounded downloads, byte limits, content hashes, durable mappings, missing-file repair, Markdown link rewriting, permission checks, and local-drift conflict protection.
- [x] Add opt-in remote title/hierarchy application through preflighted link-aware local note moves, preserving local-only content edits and reporting every source, destination, and rewritten backlink path.
- [x] Add explicit missing-document retain, recoverable archive, exact-count-confirmed delete, and interactive per-document policies; archive uses link-aware moves and delete includes managed attachments.
- [x] Add repeatable remote-root selection, bounded descendant depth, and excluded subtrees while retaining complete collection visibility so out-of-scope mappings can never be mistaken for remote deletions.
- [x] Replace whole-document pull conflict wrappers with Git's line-oriented diff3 merge: automatically apply non-overlapping edits, emit localized markers only for overlapping hunks, preserve repeat-run stability, and report `auto_merged` separately.
- [x] Keep no-longer-referenced pulled attachments under durable management and add explicit retain, recoverable archive, and exact-count-confirmed delete policies with separate structured action/count reporting.
- [x] Bind focused Outline pull state to the normalized connector server, persist the exact remote source plus available `revision`/`updatedAt` provenance, reject changing/incomplete pagination snapshots, and enforce a configurable total-document work limit before planning.
- [x] Make focused Outline pull path planning portable across Unicode-normalizing and case-insensitive filesystems, deterministically disambiguate sibling/truncation collisions with remote-ID-derived suffixes, sanitize Windows-reserved/overlong generated names, reject orphaned hierarchy, and parse/rewrite inline or reference-style attachment/document links through the Markdown event stream.
- [x] Report structured inbound conversion diagnostics for preserved raw HTML, unsupported Outline directives, and attachment destinations that cannot be rewritten safely.
- [x] Treat a missing managed local note as a reviewable conflict with explicit remote restoration, and expose/offer diff3 markers only for conflicts that have a mergeable text file on both sides.
- [x] Bound Outline API response bodies, cumulative remote Markdown, attachment count, per-attachment bytes, and total downloaded attachment bytes in addition to document count and retry/timeout limits.
- [x] Externalize exact remote and diff3-base bodies into verified content-addressed snapshots so per-action journal checkpoints atomically rewrite only the compact mapping manifest rather than every managed document body.
- [x] Add local `status`, guarded `continue`, and recoverable `abort` lifecycle operations for managed Outline diff3 conflicts; abort restores the LOCAL side only after every marker file and write permission passes preflight.
- [x] Add Outline pull response-contract and hierarchy conformance checks, consistent revision/timestamp capability reporting, and a second collection listing immediately before live mutation to reject snapshots that drifted after planning.
- [ ] Enumerate remote objects with bounded pagination/cursors, fetch revisions and content, translate supported bodies to Markdown, materialize attachments at deterministic contained paths, and map remote hierarchy into an explicit local namespace without exposing partial batches to the indexer.
- [ ] Preserve provenance, source representation, remote links, and unsupported constructs sufficiently for audit and retry. Lossy conversion must be reported before apply; opaque or non-Markdown documents may use proxy notes containing user-authored metadata/commentary plus an external binding rather than fabricated body text.
- [ ] Reconcile remote creates, updates, moves, and removals against durable state and current local hashes. Refuse to overwrite local edits by default, write through the vault lock and atomic app workflows, then incrementally rescan before marking the pull complete.
- [ ] Support an explicit adoption workflow for existing local notes and remote objects, including one-to-one validation and dry-run previews. Bulk imports may create bindings according to route policy but never inject hidden markers or rewrite unrelated frontmatter.
- [ ] Ensure chained routes consume only the successfully materialized canonical state. A SilverBullet pull followed by an Outline push is two separately journaled operations with a visible intermediate vault revision, not a direct remote-to-remote copy.

### 15.6 Push, publication, and selective reconciliation

- [ ] Generalize the existing publication planner so connectors can consume selected transformed documents, hierarchy, resolved internal links, external bindings, and deterministic attachments while retaining connector-specific rendering and capability checks.
- [ ] Create/update/move/archive only route-managed or explicitly bound remote objects. Detect remote edits before mutation, skip unchanged projections, preserve unmanaged documents, and make repeated publication idempotent after interruption.
- [ ] Translate links among documents included in the same route, preserve/report links to excluded local notes according to policy, and resolve links to separately bound external objects when the target connector/profile can represent them. Never leak denied content through titles, backlinks, attachments, or generated metadata.
- [ ] Support single-note routes for document-focused systems such as HedgeDoc as well as query/hierarchy routes for Outline, SilverBullet, and Git wikis. One local note may publish to several systems through separate bindings/routes without sharing credentials or reconciliation state.
- [ ] Keep archives and exports as first-class push targets: filesystem directory/ZIP, static site, Git worktree, and remote API connectors should reuse selection and transformation semantics even when their delivery mechanics differ.

### 15.7 Direct CLI, daemon scheduling, and loop prevention

- [x] Compose Outline pull/push directly without a daemon through authority-aware named route runs, route-level concurrency locks, durable status, all-route execution, and an interval-due `integration run --scheduled` entrypoint suitable for cron/systemd timers.
- [ ] Make plan/run/reconcile operations usable without the daemon through direct vault access. The daemon exposes the same request/report contracts, adds schedules, cancellation, status/history endpoints, and event-triggered runs, and serializes filesystem mutation through the same cross-process lock.
- [x] Add reusable phase/item Outline pull progress events and a cooperative cancellation callback to the app workflow; human CLI runs report listing, planning, applying, attachment download, scan, and completion phases, while structured output remains clean.
- [ ] Add per-route concurrency limits, timeouts, bounded jittered retries, rate-limit handling, total-work budgets, cancellation, and sanitized errors. Route history records hashes/status/counts but never credentials or sensitive remote bodies.
- [ ] Prevent feedback loops with route IDs, operation IDs, origin/provenance, desired projection hashes, debounce windows, and post-write watcher coalescing. Never trigger a route solely because it wrote the exact state it just planned.
- [ ] Support explicit route dependencies such as `pull-silverbullet` before `publish-outline`, with cycle detection, failure policy, and checkpoint boundaries. A failed predecessor blocks dependents by default; partial batches remain inspectable and retryable.
- [ ] Add authenticated daemon endpoints for route list/plan/run/status/history/conflicts and optional signed webhooks that trigger named routes. Apply permission profiles to local reads/writes, remote network domains, secrets, execution, and exact-content reporting.

### 15.8 First connector wave

- [ ] **Outline:** retain the implemented ZIP exporter and one-way API publisher as the push baseline; refactor shared mapping/planning behind connector contracts without changing current behavior. Add scoped Outline pull/import only through the new route model, with collection pagination, hierarchy/attachment materialization, explicit remote authority, and no dependence on the separate Outline-to-Git audit trail.
- [ ] **HedgeDoc/HedgeSync:** support the plugin's frontmatter mapping convention and single-document push/pull/create/open routes. Prefer supervising the maintained `hedgesync` CLI for behavior it already owns; keep live operational-transform sessions outside Vulcan unless a stable protocol and concrete need justify more. Preserve local frontmatter on body pulls and never store session cookies in the vault.
- [ ] **Simple Git-backed wikis:** use a materialized worktree connector with configurable content root/extension/frontmatter/path conventions, query-based export or scoped import, explicit commit/pull/push policy, fast-forward defaults, conflict reporting, and no libgit2 dependency. Distinguish this from Phase 12 Git sync: a wiki route maps a selected content tree, while device sync replicates the canonical vault.
- [ ] **SilverBullet:** implement selective page pull/push routes and external bindings through the shared connector model, reusing SB.1–SB.3 syntax/export research. Reserve SB.4–SB.5 for optional full-Space protocol synchronization in Phase 12 and SB.6–SB.7 for optional runtime/plug support after the basic connector works.
- [ ] Add connector-specific mock servers/processes plus pinned upstream conformance where APIs are private or version-sensitive. Publish an explicit version/capability matrix and fail closed on unsupported server or plug versions.

### 15.9 Supporting extensibility boundaries

- [ ] Define a capability-declared supervised external-tool adapter with explicit executable paths/argument templates, bounded execution, sanitized output, environment allowlists, permission profiles, cancellation, and structured status. Do not expose a generic unaudited shell hook through route configuration.
- [ ] Add a signed webhook delivery/trigger system with bounded retries and a delivery log. Webhooks may invoke named routes or report route completion, but cannot bypass route planning, authorization, or secret policy.
- [ ] Keep custom daemon endpoints mapped to a fixed registry of built-in or projected skill-command actions. Revisit a compiled Rust daemon-plugin trait only after connector and webhook contracts prove what extension points are actually needed.
- [ ] Adapt later providers such as CardDAV/VCF Contacts to the same binding/route/state model after their local loss-aware interchange exists. Keep provider-specific identities and ETags durable, credentials device-local, and conflicts visible.
- [ ] Keep Telegram and richer chat platform adapters as separate candidate interfaces over authenticated daemon tools/events. They are not part of the knowledge-connector completion gate and must not introduce connector-specific content stores.

### 15.10 Integration tests, rollout, and completion gate

- [ ] Add combined fixture scenarios for SilverBullet -> local namespace -> Outline, one local Markdown file -> HedgeDoc, a scoped local subtree <-> Git wiki worktree, one note bound to multiple external systems, attachments/hierarchy/link translation, and proxy notes for non-Markdown objects.
- [ ] Test initial import/publication, idempotent repeats, content changes, moves, archives/quarantine, local and remote drift, adoption, malformed bindings/state, duplicate ownership, pagination/cursors, retries, interruption/restart, auth failure, rate limits, unmanaged objects, route dependency failures, loop prevention, and mutation-free dry runs.
- [ ] Rebuild or delete `cache.db` between route operations and prove canonical files plus durable integration state recover the same mappings and conflict decisions. Reindex twice and assert identical derived graph/binding state.
- [ ] Verify CLI and daemon JSON parity, non-interactive operation, permission denial/filtering, feature-disabled behavior, auto-commit opt-in, secret/error sanitization, and deterministic plans across platforms and filesystem case/Unicode behavior.
- [ ] Document current versus planned connector capabilities, setup, frontmatter bindings, route configuration, authority/deletion choices, conflict recovery, scheduling, security, and backup guidance. Do not claim generic bidirectional sync merely because a connector implements both pull and push.
- [ ] Consider the shared hub foundation complete when at least one inbound route and one outbound route can be chained through an inspectable local vault revision, the HedgeDoc single-document binding use case works, and connector-specific state can be recovered without SQLite. Individual optional connectors remain independently claimable capabilities.

---

## Phase 16: Wiki Mode

**Goal:** A polished, public-facing wiki served from an Obsidian vault. Read-optimized with optional auth for editing. Supports real-time collaborative editing via Automerge CRDTs.

**Depends on:** Phase 13 (WebUI browse), Phase 14 (WebUI write, Automerge editing sessions), and Phase 9.20 (public-rendering, theming, routing, and SEO groundwork).

**Automerge in Phase 16:** Phase 14 introduces Automerge for ephemeral single-user editing sessions. Phase 16 extends this to multi-user real-time collaboration by adding the Automerge sync protocol over WebSockets. The on-disk `.md` files and git remain the canonical store and versioning backend — Automerge is the live collaboration layer, not a replacement for git.

### 16.1 Public read mode

- [ ] Unauthenticated read access to rendered vault content
- [ ] Rendered Markdown with Obsidian-compatible features: callouts, embeds, math (KaTeX), wikilinks resolved to wiki URLs, mermaid diagrams, code highlighting
- [ ] Navigation: sidebar with folder tree, tag-based browsing, graph explorer
- [ ] Search: full FTS + vector hybrid search exposed in the UI
- [ ] Home page: configurable (default: note named `Home.md` or `index.md`)
- [ ] SEO: server-rendered HTML, meta tags, sitemap generation
- [ ] Reuse the route conventions, default metadata model, hover-preview contracts, and theme tokens from Phase 9.20 unless a documented dynamic requirement forces divergence

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

Uses Phase 17's capability system. Wiki mode adds vault-level access presets that issue or select underlying grants without creating a second authorization model:

- [ ] **Public read / authenticated write** (default): unauthenticated requests use an explicit public-read grant; authenticated callers use their resolved capabilities
- [ ] **Fully public**: an explicit public-read grant permits unauthenticated reads without granting ambient write authority
- [ ] **Fully private**: no unauthenticated access, all users must log in
- [ ] **Per-folder, per-tag, and per-note visibility**: configured through rooted and delegated resource-scoped grants from Phase 17.2
- [ ] **Document-level secrets**: `[!secret]` callouts and restricted embeds from Phase 17.4 are enforced in wiki rendering
- [ ] **Share links**: share-audience limited credentials from Phase 17.5 provide read access to specific notes/folders/tags without requiring an account

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

## Phase 17: Identity & Delegable Capability Authorization

**Goal:** Add multi-user identity and a capability-first authorization system for humans, groups, agents, automation, services, and external shares. A trusted local bootstrap creates the first canonical root grant; holders may then delegate strict subsets of delegable authority through typed wiki objects. Document-level secrets and limited bearer credentials build on the same model so every web-facing and automated workflow shares one authorization boundary.

**Depends on:** Phase 10 (daemon). Sub-phases 17.1–17.3 must be complete before Phase 13 ships. Sub-phases 17.4–17.5 are needed by Phase 16.

**Design principles:**

- **The wiki is canonical.** Typed wiki objects hold identities, memberships, rooted grants, delegation lineage, revocations, policy ceilings, public credential metadata, and durable audit records. SQLite may project them for fast resolution but remains rebuildable.
- **Delegation only attenuates.** A child grant can never add an action, broaden a resource, extend an expiry, increase delegation depth, add a network domain, loosen a resource limit, or escape an audience constraint.
- **Default deny, positive grants.** Ordinary access comes from explicit capabilities. Negative rules are reserved for non-delegable canonical policy ceilings such as disabled identities, forbidden control paths, or a read-only vault.
- **Roles are templates, not bypasses.** `owner`, `editor`, and `viewer` remain useful UI/configuration presets that issue capability bundles. No role silently bypasses normal authorization.
- **Enforcement stays shared.** Capability resolution produces the existing `PermissionGrant`; commands, queries, rendering, MCP, plugins, and JavaScript continue using `PermissionGuard` and `PermissionFilter`.
- **Filesystem access remains outside this boundary.** A user who controls the working tree or Git repository can already read and change all plaintext data. Vulcan protects authorization objects at WebUI/API/managed-sync/Git-ingestion boundaries rather than adding cryptographic anchoring against the filesystem owner.

### 17.1 Canonical authorization wiki objects and runtime secrets

```
System/Authorization/
├── Identities/          # public identity/profile objects
├── Groups/              # group definitions and memberships
├── Grants/              # root and delegated grants
├── Revocations/         # explicit revocation objects
├── Policies/            # non-delegable ceilings and namespace policy
├── Credentials/         # public credential metadata, never bearer values
├── Audit/               # durable authorization events or journals
└── Templates/           # role and grant authoring templates
```

The namespace is configurable but reserved. Authorization objects are first-class Markdown for links, Git history, queries, Bases, and administration views, yet generic note operations cannot mutate, move, create, or delete them. The namespace setting is itself security-critical control-plane configuration and receives the same protection. Only dedicated `auth` workflows may write either.

Example identity and group objects:

```markdown
---
type: vulcan.authorization.identity
version: 1
id: user:alice
display_name: Alice
email: alice@example.com
disabled: false
---

# Alice
```

```markdown
---
type: vulcan.authorization.group
version: 1
id: group:gm
members:
  - user:alice
---

# Game Masters
```

The first root is created by a trusted local bootstrap command against an uninitialized authorization namespace. Subsequent roots are ordinary canonical grant objects whose creation or widening requires `manage_root_grants` through the dedicated workflow:

```markdown
---
type: vulcan.authorization.grant
version: 1
id: 01K...
root: true
issuer: system:bootstrap
subject: user:alice
actions:
  - "*"
resources:
  - "*"
delegation_depth: 8
created: 2026-08-24T12:00:00Z
---

# Initial owner grant
```

`auth.db` contains only material unsuitable for canonical wiki content:

```text
password_verifiers: identity_id, password_hash, updated_at
credential_secrets: credential_id, token_hash/verifier, created_at, last_used_at
sessions:           id, identity_id, token_hash, created_at, expires_at
oauth_state:         state, verifier/challenge material, expires_at
runtime_counters:    credential_id, rate/use window and count
```

These tables are authoritative only for the secrets or temporary runtime facts they contain. They never define actions, resources, parent lineage, group membership, or revocation policy. Cache tables such as authorization-object indexes, effective-grant projections, and lineage closures are disposable and rebuilt from the wiki objects.

Authorization objects use strict versioned schemas. Duplicate IDs, unknown security-critical fields, invalid selectors, missing parents, cycles, widening children, malformed revocations, and unsupported versions fail closed with diagnostics. No signatures or canonical-byte encoding are required. If direct filesystem edits leave the graph invalid, daemon clients lose affected authority until the canonical files are repaired; an unrestricted trusted local CLI remains the recovery path.

**CLI and API management:**

- [ ] `vulcan auth init --owner <identity>` — initialize the reserved namespace and first root only when no authorization graph exists
- [ ] `vulcan auth user add|remove|list|disable|enable <username>` using canonical identity objects and separate password-verifier storage
- [ ] `vulcan auth group add|remove|list` and `vulcan auth group members <group> add|remove <username>`
- [ ] User/group/session endpoints under `/auth/...`, gated by `manage_identities` / `manage_groups` rather than a hard-coded owner bypass
- [ ] Reserve and normalize the authorization namespace across note CRUD, refactors, templates, plugins, skills, MCP, publication, WebUI, and managed sync; protect changes to the namespace setting itself; reject symlink, case-folding, Unicode-normalization, and path-alias bypasses
- [ ] Apply canonical-file and secret-store changes in fail-closed order: an orphaned public credential without verifier is inactive, and deleting/revoking verifier material precedes canonical revocation when immediate denial matters
- [ ] Rebuild authorization projections from canonical files and invalidate active request/session caches after relevant file changes
- [ ] Backup/restore and doctor coverage that distinguishes canonical authorization objects, rebuildable projections, and secret/runtime state

### 17.2 Rooted capability grants and delegation

Each durable grant object has a stable ID, vault, issuer, subject, exactly one parent grant or an explicit root marker, action set, resource scope, constraints, delegation allowance/depth, and timestamps. Revocation is represented by a separate canonical object referencing the grant, preserving history rather than rewriting or deleting it. A subject may hold several grants; their positive authority is combined only after every grant is independently validated against its lineage and policy ceilings. When an issued credential needs authority from several parents, it contains or references several independently attenuated grants rather than erasing provenance in one synthetic parent.

Delegation to another registered user or group is an authenticated daemon transaction that creates a durable child grant for that subject. It is not accomplished by copying the issuer's bearer credential. Limited bearer credentials are for possession-based clients such as agents, scripts, services, and external shares; they may additionally bind to an authenticated subject, device, or audience when possession alone is insufficient.

**Initial action vocabulary:**

- Resource actions: `read`, `write`, `create`, `delete`, `refactor`
- Operational actions: `git`, `index`, `config:read`, `config:write`, `execute`, `shell`, `network`
- Authority actions: `delegate`, `manage_identities`, `manage_groups`, `manage_root_grants`, `revoke_descendants`, `view_audit`
- Existing CPU, memory, stack, and network-domain limits remain grant constraints

Existing Phase 9 profiles treat `write` as create/update/delete. Phase 17 adds finer action checks for delegable credentials while preserving that shorthand: an old profile with `write = "all"` resolves to all three resource actions, and one with `write = "none"` resolves to none. This is a compatible extension of `PermissionGrant` / `PermissionGuard`, not a second enforcement path.

**Resource vocabulary:** reuse `folder:<glob>`, `tag:<tag>`, `note:<path>`, and `*` from Phase 9.19.13. Add explicit vault identity to the normalized grant so cross-vault authority is never inferred from an unqualified path.

**Attenuation rules:**

- actions form a subset of the parent;
- resources are equal to or narrower than the parent scope;
- expiration can only move earlier;
- delegation depth decreases and a grant without `delegate` cannot produce children;
- audience, source/device binding, allowed network domains, rate limits, and runtime limits only narrow;
- canonical policy ceilings intersect every resolved grant and cannot be delegated around;
- revoking a grant invalidates its complete descendant tree and associated credentials;
- cycles and ambiguous/missing parent lineage fail closed.

**Roles and groups:** bundled `owner`, `editor`, and `viewer` definitions are authoring templates over actions and resources. A root grant may target a user or group. Group membership is resolved dynamically, so removing a member withdraws group-derived authority without rewriting issued grants. Administration screens may offer an ACL-like matrix, but writes from that view create, attenuate, or revoke capability grants.

**Mutation safety:** folder moves, tag changes, note renames, and other classification-changing operations are checked against both the original and resulting resource states. Possessing write access to content is not enough to move it into a broader scope or attach a tag that expands the caller's effective authority.

**Git and managed-sync ingress:** evaluate candidate changes before they replace the live working tree. The default Git/sync integration rejects any incoming diff touching the reserved authorization namespace or its namespace configuration. An optional governed mode may accept authorization changes only when both conditions hold:

1. Vulcan parses the complete candidate authorization graph and proves identity-management authority, valid lineage, monotonic attenuation, and valid revocations.
2. A configured ingress policy authenticates the change through either forge-reported protected-branch and CODEOWNERS approvals or verified commit signatures mapped to canonical subjects whose authority covers every authorization mutation.

CODEOWNERS without branch protection is advisory, not enforcement. A signature authenticates a Git actor but neither grants that actor authority nor cryptographically anchors the grant file; both ingress modes still require semantic validation. Direct local Git pulls and filesystem edits remain within the trusted filesystem boundary; the managed daemon endpoint must never pull into the active tree first and attempt to undo an unauthorized control-plane change afterward.

**CLI and tests:**

- [ ] `vulcan auth grant create --subject <p> --from <grant-id> --action ... --resource ... [constraints] --dry-run`
- [ ] `vulcan auth grant inspect <grant-id>` — show lineage, effective authority, constraints, descendants, and revocation state
- [ ] `vulcan auth grant list [--subject <p>] [--vault <id>]`
- [ ] `vulcan auth grant revoke <grant-id> [--reason ...]` — create a canonical revocation object that invalidates the grant and descendants
- [ ] `vulcan auth grant check --subject <p> --action <a> --resource <r>` — explain contributing grants and canonical policy ceilings
- [ ] Property tests prove that arbitrary attenuation sequences never widen authority
- [ ] Regression tests cover groups, expiry, depth, revocation cascades, multiple independent parents, policy ceilings, old/new-state mutation checks, reserved-path bypasses, and full rebuild from canonical files
- [ ] Git-ingress tests cover default rejection, namespace-setting changes, staged/candidate-tree validation, CODEOWNERS-without-protection rejection, required approvals, signer-to-subject authority checks, and mutation-free failure before the live tree changes

### 17.3 Capability resolution and permission-filtered queries

Phase 17 adds `CapabilityPermissionGuard`, which resolves the authenticated subject and any presented limited credential through applicable rooted grants into the existing `PermissionGrant`. `PermissionFilter` remains identity-neutral and continues generating SQL predicates from the resolved guard.

**Resolution flow:**

```text
authenticated identity or limited credential
  -> load validated canonical identity, grant, policy, and revocation objects
  -> validate subject, grant lineage, expiry, audience, and revocation
  -> include current group-derived grants
  -> intersect canonical policy ceilings and request/transport constraints
  -> resolve PermissionGrant once per request
  -> enforce through PermissionGuard and PermissionFilter
```

**Enforcement strategy — filter at the query layer, not post-hoc:**

| Feature | Enforcement |
|---|---|
| **Search (FTS + hybrid)** | Allowed-document CTE joined into FTS query; unavailable documents never appear in results or hit counts |
| **Graph (stats, paths, hubs, components)** | Nodes filtered to the allowed set; edges to unavailable notes expose no target name or content |
| **Backlinks** | Only backlinks from readable notes are returned |
| **Vectors / similarity** | Candidate set filtered before ranking |
| **Properties / Bases queries** | `WHERE` clause includes the permission predicate |
| **Note content (`GET /{id}/notes/{path}`)** | 403 without read capability |
| **Transclusions / embeds** | Unreadable targets render as `[restricted content]` |
| **Activity and audit views** | Events filtered to permitted resources and audit capability |
| **Git history / diffs** | File-level output filtered to readable paths |
| **Automerge collaboration** | Handshake requires read; mutations require write for the resulting document state |

**Implementation:**

- [ ] Add typed canonical authorization-object loaders, complete-graph validation, reserved-namespace workflows, and attenuation checks in the planned `vulcan-auth` crate; compatibly extend reusable resolved permission semantics in `vulcan-core` with create/delete and authority-administration checks
- [ ] Implement `CapabilityPermissionGuard` over an already validated request authority context rather than coupling core query code to sessions or token parsing
- [ ] Reuse `PermissionFilter::sql_cte()` and existing point checks from 9.19.13
- [ ] Daemon middleware authenticates the caller, validates grants/credentials, resolves once per request, and passes `&dyn PermissionGuard` to handlers
- [ ] Direct local CLI mode remains unrestricted for the filesystem owner unless an explicit profile or limited credential is selected
- [ ] Cache resolved authority per request only; watcher changes to canonical identity/group/grant/revocation/policy objects invalidate longer-lived session caches and rebuild the disposable projection
- [ ] Integration tests verify that unavailable content cannot leak through search, graph, backlinks, vectors, snippets, completion, exports, rendering, logs, or aggregate counts

### 17.4 Document-level secrets

Two complementary mechanisms embed restricted content within otherwise accessible notes.

**Mechanism A: scoped capabilities + embeds**

Grant access to a restricted folder, tag, or note, then embed it into a more widely readable note:

```markdown
# Lord Blackwood
Noble of the Eastern Provinces. Known for his generous charity work.

The townsfolk speak highly of Lord Blackwood's patronage of the arts.

![[GM-Only/NPCs/Blackwood Secrets]]
```

The embedded note `GM-Only/NPCs/Blackwood Secrets.md` requires a matching read capability. Without it, the embed shows `[restricted content]`; with it, the full content is inlined.

- [ ] Embed rendering checks the resolved read capability on the embedded target
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

The `[!secret <label>]` callout type maps the region to a `secret:<label>` resource selector. Rendering strips it unless the resolved grant includes read authority for that selector. A canonical root or delegated grant can give `secret:gm` to `group:gm`, and a holder may explicitly delegate that narrower authority to an agent or service; group membership or a role name is not an implicit rendering bypass.

- [ ] Parser recognizes `[!secret <label>]`, validates the label, and extracts its content range
- [ ] `ParsedDocument` stores secret regions with their required `secret:<label>` selector
- [ ] Rendering pipeline strips secret callout body for unauthorized users
- [ ] Search: secret callout text is indexed but filtered from results/snippets without the matching secret-read capability
- [ ] Editor UI: secret callouts visually distinguished (e.g., lock icon, colored border) so authors can see what's hidden
- [ ] Nesting: secret callouts inside regular callouts work; nested secret regions require every enclosing `secret:<label>` capability

**Design note:** Both mechanisms protect content at the web/API layer only. Raw `.md` files contain all content in plaintext. Users with filesystem access see everything; document encryption is a separate future feature.

### 17.5 Limited credentials for agents, automation, services, and shares

Any subject with delegable authority may mint a credential containing strict child grants for an LLM agent, automation script, CI job, connector worker, MCP/API client, or external reader. Share links are the unauthenticated read-oriented presentation of the same credential model, not a separate authorization system.

Example agent issuance:

```sh
vulcan auth token create \
  --name research-agent \
  --read 'folder:Research/**' \
  --write 'folder:Research/Inbox/**' \
  --network '*.wikipedia.org' \
  --expires 24h \
  --no-delegate \
  --dry-run
```

The dry run explains which parent grants cover each requested capability and rejects any widening. Applying it writes canonical public credential metadata under `System/Authorization/Credentials/`, stores only the bearer verifier/secret runtime material in `auth.db`, and displays the bearer value once. Activity remains attributable to both the credential identity and complete human/service delegation lineage.

**Credential requirements:**

- [ ] Credential types share one validation path but carry explicit audiences (`browser-session`, `api`, `mcp`, `agent`, `automation`, `service`, `share`)
- [ ] Default to no delegation; encourage short expiry for agent and automation credentials
- [ ] Support action/resource attenuation, expiry, use limits, rate limits, network domains, and optional source/device binding
- [ ] Immediate credential revocation disables/removes verifier material first, then records canonical credential revocation; grant revocation still cascades to descendants. Audit issuance, use, attenuation, denial, and revocation without logging bearer secrets
- [ ] Evaluate an opaque reference token first and a typed macaroon-style chained envelope for offline attenuation; token encoding must not change the durable grant semantics
- [ ] Never issue or honor an unconstrained blank/god token; every external credential has an audience, expiry policy, and explicit grant set

**CLI:**

- [ ] `vulcan auth token create [--from <grant-id>] --name <name> [actions/resources/constraints] --dry-run`
- [ ] `vulcan auth token list [--subject <p>] [--audience <a>]` — query canonical public credential objects and join runtime status without exposing verifier material
- [ ] `vulcan auth token inspect <token-id>` — metadata, effective authority, lineage, last use, and expiry without revealing token material
- [ ] `vulcan auth token revoke <token-id>`
- [ ] `vulcan auth token attenuate <credential> [narrowing constraints]` where the selected encoding safely supports holder-side attenuation

**External share presentation:**

```
https://host/s/{share_token}
```

- [ ] `POST /{id}/shares` — create share: `{ "resource": "note:Handouts/Map.md", "permission": "view", "expires": "2026-04-30", "password": null }`
- [ ] `GET /{id}/shares` — list active shares (requires grant/audit administration capability)
- [ ] `DELETE /{id}/shares/{share_id}` — revoke share
- [ ] `GET /s/{token}` — resolve share, render content (no auth required)
- [ ] Share credentials use the same canonical grant/credential objects and separate verifier storage as other limited credentials
- [ ] Resource types: `note:<path>` (single note), `folder:<path>` (folder and children), `tag:<tag>` (all notes with tag)
- [ ] Permission: `view` (read-only rendered content) or `view-raw` (download markdown source)
- [ ] Optional password protection: share link prompts for password before rendering
- [ ] Expiry: shares can have an expiration date or be permanent until revoked
- [ ] Share rendering respects document-level secrets using only the share credential's resolved capabilities; shares have no ambient viewer role
- [ ] Rate limiting on share endpoints to prevent enumeration
- [ ] CLI convenience facade: `vulcan auth share create <vault> <resource> [--expires 30d] [--password]` issues a read-only, share-audience child credential
- [ ] Integration tests cover least-privilege agent tokens, script/network restrictions, multi-parent credentials, expiration, audience confusion, token theft/replay limits, revocation cascades, shares, and secret stripping

### 17.6 Future: OIDC / SSO integration

Planned but not in initial scope. Deferred until local identity and capability delegation are stable. OIDC authenticates and binds a subject; it does not become a parallel policy engine.

- [ ] OIDC provider configuration in `daemon.toml`: issuer URL, client ID/secret, scopes
- [ ] Login flow: browser redirects to IdP, daemon handles callback, creates/updates local user from claims
- [ ] Group mapping: map reviewed OIDC claims/groups to local groups; group subjects receive authority only through normal rooted grants
- [ ] Hybrid mode: local accounts and OIDC accounts coexist, OIDC users auto-provisioned on first login
- [ ] Token refresh and session management integrated with `auth.db`; external IdP tokens never bypass local capability resolution, canonical policy ceilings, or revocation

---

## Phase 18: Canvas Support

**Goal:** First-class support for Obsidian's JSON Canvas format (`.canvas` files). Index canvas content for search, surface canvas data in the graph, provide CLI commands for inspection and manipulation, and eventually render an interactive canvas editor in the WebUI.

**Depends on:** Phase 7 (core indexing and parsing infrastructure). WebUI canvas editor (18.5) depends on Phase 14 (WebUI write). Canvas capability enforcement follows from Phase 17.

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
- [ ] Capability enforcement: canvas files use the same folder/tag/note resource-scoped grants as Markdown notes (Phase 17)

### 18.7 Cross-cutting integration

- [ ] **Search:** `vulcan search` finds text inside canvas text nodes. The `file:` search operator (9.6.2) matches `.canvas` files. A `type:canvas` or `type:note` operator could filter by document type.
- [ ] **Doctor:** Canvas file references are validated alongside wikilinks. Broken canvas references reported in `vulcan doctor` output.
- [ ] **Move/rename:** When a note referenced by a canvas file node is moved/renamed, the canvas `file` field is updated by the rewrite engine (same mechanism as wikilink rewriting).
- [ ] **HTTP API:** All canvas data accessible via the daemon API. `GET /{id}/canvas/` lists canvases, `GET /{id}/canvas/{path}` returns parsed data. Search results include canvas hits with `document_type: "canvas"`.
- [ ] **Permission filtering (Phase 17):** Canvas files and referenced notes use the same resolved capability filter. File nodes referencing unreadable notes render as `[restricted]`.
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
- [ ] Capability enforcement: Excalidraw files respect Phase 17 resource-scoped grants and canonical policy ceilings

---

## Phase 19: Vulcan Apps

**Goal:** Let a vault contain installable, interactive applications that run through the Vulcan WebUI and CLI while preserving the vault as the local information hub. A Vulcan App may provide a sandboxed static browser UI, typed CLI commands, resource-limited TypeScript/JavaScript host functions, browser or server WebAssembly components, or a combination of those surfaces. Apps use versioned Vulcan APIs and explicit capability grants rather than direct cache, daemon-internal, or ambient host access.

**Depends on:** Phase 10 (daemon, versioned HTTP service, jobs, and watchers), Phase 13 (WebUI host and read-only browser surfaces), and Phase 17.1–17.5 (identity, sessions, rooted/delegable grants, permission filtering, document secrets, and share boundaries). Write-enabled browser apps additionally depend on Phase 14's mutation and review surfaces. Static publication integration depends on Phase 9.20. QuickJS host/CLI functions reuse Phase 9.18.5 and Phase 9.24's typed tool/runtime contracts. Server-side WebAssembly is a new optional runtime and does not depend on Phase 16.6's collaborative local-first investigation. The Feed Reader reuses Phase 15's external binding/reconciliation contracts when available, but neither that example nor Phase 15 blocks the core package and runtime gates.

**Core decisions:**

- A Vulcan App is an interactive application package, not another name for a script, skill command, or lifecycle plugin. Scripts are directly executed, skill commands are typed callable operations, plugins react to lifecycle events, and apps own interactive sessions and views. Apps may call typed tools or shared services without bypassing those boundaries.
- Keep package, installation, instance, and data identities separate. A package is immutable code/assets; an installation is device-local trust and capability approval for one exact content identity; an instance binds an installed package to one vault and configuration; app data is classified independently.
- `.vapp` is a strictly profiled ZIP archive and immutable application image. Runtime state, caches, databases, user data, generated content, settings, grants, and secrets live outside the package. Normal installation and execution never extract package contents to the filesystem.
- The logical package model is independent of ZIP serialization. A future physical representation may encode the same manifest and payload model only through an explicit format-version extension; OCI may distribute `.vapp` blobs but is not the local runtime format.
- Use a canonical UTF-8 JSON manifest and BLAKE3 identities. `AppContentId` identifies the logical manifest and all payloads; `PackageBlobId` identifies the exact ZIP bytes. ZIP CRC-32 is only a structural corruption check and never an authenticity or content-identity mechanism.
- A package requests capabilities but grants none. Effective authority is the restrictive intersection of the caller/session grant, installation grant, instance grant, manifest request, runtime sandbox ceiling, and canonical policy ceilings.
- The browser UI, typed CLI commands, QuickJS functions, and server WebAssembly components are peer application surfaces over one versioned Vulcan App API. Server WASM may be invoked directly from CLI/RPC, from QuickJS, or as a job; it is not merely a JavaScript optimization format. Browser WASM remains inside the iframe sandbox and initially reaches Vulcan only through the JavaScript bridge.
- App-provided CLI commands are namespaced, manifest-declared entrypoints whose arguments and results are parsed and rendered by Vulcan. They are not native executables, arbitrary `argv` passthrough, raw shell commands, automatic top-level command injection, or implicit MCP tools.
- Only `.vulcan/cache.db` is necessarily rebuildable derived state. Apps may own canonical Markdown, Canvas, Bases, media, SQLite, or other explicit artifacts. Every store declares whether it is canonical document data, canonical artifact data, device-local state, secret state, derived cache, or temporary state.
- V1 packages are self-contained and have no executable package dependencies. Bundle JavaScript/UI dependencies and call other installed behavior only through stable typed Vulcan services or tool APIs.
- Discovery and synchronization never execute or activate code. Installation trust is bound to the exact `AppContentId`; changed package content requires validation and explicit update handling, and expanded capability requests require renewed approval.

### 19.1 Domain model, ownership, and version contracts

- [ ] Add transport-neutral `AppId`, semantic `AppVersion`, `AppContentId`, `PackageBlobId`, `AppPackageManifest`, `AppInstallation`, `AppInstance`, `AppDataBinding`, and typed app error/report models
- [ ] Validate reverse-DNS-style stable app IDs independently from human names; distinguish publisher release coordinates from cryptographic content identity
- [ ] Define `AppContentId` as BLAKE3 derive-key mode with the exact UTF-8 context `dev.vulcan.app-content.v1` over the original validated canonical manifest bytes
- [ ] Define `PackageBlobId` as BLAKE3 derive-key mode with context `dev.vulcan.app-package-blob.v1` over the exact ZIP bytes
- [ ] Define payload digests as BLAKE3 derive-key mode with context `dev.vulcan.app-payload.v1` over uncompressed entry bytes; serialize all identifiers as lowercase algorithm-tagged strings such as `blake3:<64 hex characters>`
- [ ] Keep v1 hash algorithms closed: accept BLAKE3 only rather than generic hash agility or downgrade negotiation; allow external distribution layers to attach their own SHA-256 or registry identifiers without changing native identities
- [ ] Version the package format, Vulcan App API, browser bridge protocol, app CLI descriptor/output contract, QuickJS host API, server-WASM ABI, and instance configuration independently; unknown major versions fail closed
- [ ] Define package/install/instance/data lifecycle states and stable JSON reports for discovery, validation, installation, grant review, activation, update, migration, disablement, and uninstall
- [ ] Preserve crate boundaries: manifest/identity/capability/data-classification domain types live in `vulcan-core`; finite install/update/migration workflows live in `vulcan-app`; HTTP, browser sessions, subscriptions, and runtime supervision live in `vulcan-daemon`; package codecs and execution engines remain replaceable adapters

### 19.2 Canonical manifest and logical package specification

- [ ] Specify `manifest.json` as UTF-8 without BOM, I-JSON-compatible canonical JSON whose original bytes must equal its RFC 8785 JSON Canonicalization Scheme representation
- [ ] Reject duplicate object keys before constructing a generic JSON value, forbid floating-point values in v1, bound integers to the exactly interoperable range, and reject ambiguous uses of `null`
- [ ] Define application-level canonical rules beyond JSON syntax: payload maps ordered by canonical path, set-like arrays lexicographically sorted and duplicate-free, lowercase digest encodings, and deterministic ordering for entrypoints and capability requests
- [ ] Reject unknown fields in closed manifest objects; provide named extension objects only where forward-compatible vendor metadata is explicitly safe
- [ ] Require manifest fields for format version, app ID/version/name, App API compatibility, human metadata, runtime entrypoints, capability requests, resource ceilings, payload inventory, and supported instance/data schema ranges
- [ ] Require every payload file to appear exactly once in the manifest with canonical path, actual uncompressed byte size, BLAKE3 digest, and optional validated media type/delivery metadata; `manifest.json` does not list itself
- [ ] Model capabilities as structured requests with stable request IDs, capability names, required/optional status, maximum resource selectors, network-domain ceilings, and whether an instance may bind a narrower concrete scope
- [ ] Model full-page UI routes, named embeddable views, typed CLI commands, typed host functions, browser-WASM assets, server-WASM exports, lifecycle/background entrypoints, schemas, migrations, and static-publication support explicitly rather than inferring semantics from directories
- [ ] Reserve `META-INF/signatures/` for detached signature records that are neither executable payloads nor part of `AppContentId`; prohibit every other unlisted entry
- [ ] Define Ed25519 as the v1 detached signature algorithm, a canonical signature-record schema with publisher/key identity, and the exact signed statement `"vulcan-app-signature/v1\0" || raw 32-byte AppContentId`; a canonical manifest signature policy makes removal of a required record invalid without creating a digest cycle
- [ ] Publish the v1 JSON Schema, normative examples, exact hash vectors, path vectors, canonicalization vectors, signature vectors, and a human-readable format specification

### 19.3 Strict ZIP profile and hostile-input validation

- [ ] Add a package reader with the Rust `zip` crate using default features disabled and only the v1 Stored/Deflate codec surface enabled
- [ ] Accept only single-disk archives containing regular files; reject encryption, directories, symbolic/hard links, special files, duplicate names, overlapping entries, prepended/trailing data, archive/entry comments, unsupported methods, and inconsistent local/central headers
- [ ] Reject ZIP64 in v1 while configured package and entry limits remain below ZIP64 thresholds; introduce it only through a deliberate format-version change
- [ ] Reject unknown ZIP extra fields and filename encodings outside the canonical ASCII path profile
- [ ] Define the v1 path grammar as `[A-Za-z0-9._@+-]+(/[A-Za-z0-9._@+-]+)*`; reject absolute paths, empty/`.`/`..` components, trailing separators, backslashes, NUL/control bytes, drive/device prefixes, and ASCII case-fold collisions
- [ ] Enforce configured ceilings for blob bytes, manifest bytes, entry count, per-entry compressed/uncompressed bytes, total actual uncompressed bytes, path length/depth, and decompression ratio using checked arithmetic and bounded readers
- [ ] Do not trust ZIP-declared sizes alone: stream every payload, count actual produced bytes, enforce per-entry/global budgets during decompression, and verify size and BLAKE3 digest against the manifest
- [ ] Require entries declaring byte-range delivery, including large video/audio assets, to use Stored compression; recommend Stored for already compressed media while allowing Deflate for text and other suitable assets
- [ ] Implement the complete validation order as one boundary: blob limit, central-directory structure, ZIP profile, path/type checks, bounded manifest read, strict parse/canonical comparison, schema/version checks, manifest/archive bijection, streamed payload verification, identities, then signatures
- [ ] Expose runtime access only through a `ValidatedAppPackage`-style type that cannot be constructed without complete validation; raw ZIP handles must never enter runtime, installer, module-loader, or asset-server code
- [ ] Keep every later VFS read bounded and integrity-associated even after validation; validation must not authorize unbounded allocation or decompression
- [ ] Guarantee that no validation path extracts or writes package entries, including error handling, preview, inspection, and fuzz targets

### 19.4 Read-only package VFS and deterministic builder

- [ ] Implement a read-only archive-backed VFS with canonical `path -> entry` lookup, bounded streaming reads, media metadata, and efficient access to Stored entries
- [ ] Use the same VFS for static HTTP assets, QuickJS module resolution, source maps, JSON schemas, migrations, and browser/server WASM loading
- [ ] Define relative module/import resolution entirely in the package namespace; prohibit host paths, package escapes, undeclared files, runtime network imports, and cross-package executable imports
- [ ] Add a directory development representation with the same logical manifest and path rules; development mode must not weaken runtime permissions or package validation semantics
- [ ] Add a deterministic `vulcan apps pack <directory>` builder with lexicographic entry order, fixed timestamps/permissions, empty comments/extra fields, deterministic manifest bytes, fixed compression policy, and controlled compressor configuration
- [ ] Make logical reproducibility mandatory through stable `AppContentId`; only claim byte-for-byte ZIP reproducibility when the complete compressor/toolchain version is pinned and covered by golden tests
- [ ] Add `apps unpack` only as an explicit developer inspection command with destination preflight, collision checks, no overwrite by default, and the same path/size constraints; installation and execution never call it
- [ ] Add `apps lint` for source directories and packages, including canonicalization, manifest/file correspondence, runtime entrypoints, schemas, capability declarations, media delivery, and compatibility checks
- [ ] Add `apps test` support for fixture inputs, expected typed outputs/errors, browser bridge mocks, runtime resource limits, and package conformance cases

### 19.5 Discovery, immutable installation, and update lifecycle

- [ ] Support explicit file installation plus configured discovery of canonical `.vapp` files in a vault namespace; discovery reports metadata and diagnostics without executing, trusting, granting, migrating, or activating code
- [ ] Validate the source package, compute both identities, and copy the exact verified blob into a device-local immutable content store keyed by `PackageBlobId`; runtime execution uses the installed blob rather than a mutable synchronized path
- [ ] Make installation atomic/no-clobber and retain a validation receipt keyed by package-format validator version, `AppContentId`, and `PackageBlobId`; never trust timestamps or path identity as proof that bytes are unchanged
- [ ] Record device-local installation provenance, exact content/blob IDs, publisher evidence, approval state, capability ceiling, compatible runtimes, and install/update timestamps outside the vault and `cache.db`
- [ ] Distinguish an available vault/catalog package from an installed package and an activated instance; synchronized changes produce an update candidate rather than hot-swapping executable code
- [ ] Detect same app ID/version with a different `AppContentId` and require explicit replacement policy; never treat a semantic version label as executable identity
- [ ] Provide mutation-free install/update previews showing identity, provenance, signature status, compatibility, capability deltas, entrypoint changes, data migrations, removed views/functions, and affected instances
- [ ] Require new approval when an update changes content, publisher continuity, required capabilities, network domains, background behavior, or runtime ceilings; unchanged/narrower requests may reuse only policy-approved grants bound to the new exact content identity
- [ ] Support atomic activation and rollback to retained compatible package blobs; package rollback never silently downgrades or rolls back canonical app data
- [ ] Separate disable instance, remove instance configuration, uninstall code, remove cached package blobs, archive data, and delete data operations; uninstalling executable code never implicitly deletes canonical data
- [ ] Garbage-collect unreferenced blobs only through an explicit bounded/recoverable maintenance workflow that respects active, rollback, migration, and audit references

### 19.6 Instances, configuration, secrets, and data ownership

- [ ] Allow multiple named instances of one app per vault and define stable instance IDs independent of display names; embeds/routes refer to an instance plus view rather than an ambiguous package name
- [ ] Store non-secret shareable instance definitions as canonical validated vault objects in a reserved app namespace; store activation, trust, grants, local preferences, and runtime health device-locally
- [ ] Store credentials through Phase 17's secret facilities and expose only opaque secret handles scoped to an instance and capability; never place secret values in manifests, instance files, URLs, browser storage, logs, or app-visible error details
- [ ] Define explicit app data classes: canonical document, canonical artifact, device-local, secret, derived cache, and temporary; require every declared store/binding to select one
- [ ] Let instances bind manifest capability requests and logical stores to narrower concrete path/tag/type selectors; validate both old and resulting selectors for configuration or data-moving mutations
- [ ] Define package, instance-config, and data-schema versions separately; track migration state outside `cache.db` and make it reconstructible or durably journaled as appropriate
- [ ] Require migration plans to name affected stores, backups/snapshots, resource ceilings, supported from/to versions, validation, rollback support, and whether downgrade is possible
- [ ] Run data migrations only after explicit preview/approval, under the effective instance and caller authority, with interruption-safe journaling and post-migration validation
- [ ] Ensure app-created canonical files are ordinary vault artifacts visible to history, backup, sync, permission filtering, storage accounting, and explicit export; app ownership must not make them invisible to users or administration

### 19.7 Trust, capability grants, and policy composition

- [ ] Bind installation trust and grants to `(AppId, AppContentId, instance or installation scope, grant lineage)` rather than app name/version alone
- [ ] Resolve effective authority as caller/session grant ∩ installation grant ∩ instance grant ∩ manifest request ∩ runtime sandbox ∩ canonical policy ceilings; nested function/component/tool calls preserve or attenuate that result
- [ ] Distinguish required and optional capability requests so an instance can operate in a reduced mode when optional access is denied
- [ ] Reuse Phase 17 selectors and checks for note/artifact read, write, delete, refactor, query, graph, vector, config, Git, network, secret, execute, job, background-event, and publication capabilities
- [ ] Treat annotations such as `read_only`, `background`, or `destructive` as review metadata only; authorization remains entirely capability-derived
- [ ] Require separate approval for scheduled/background execution, lifecycle subscriptions, external network domains, secret use, host execution, Git operations, and publication
- [ ] Filter reads, counts, errors, graph edges, search suggestions, subscriptions, and autocomplete before app code receives them so denied resources cannot be inferred through derived output
- [ ] Never give browser code a daemon bearer token or raw grant. Use a host-controlled, instance/session-bound message channel with request IDs, origin/source validation, schema validation, cancellation, rate limits, and revocation
- [ ] Make unsigned local packages possible under explicit local trust while supporting publisher keys, detached signatures, organization policy, revocation, and future transparency/OCI attestations without requiring a centralized app store
- [ ] Reject or disable installed apps fail-closed when package integrity, signature policy, compatibility, grant lineage, reserved namespace, runtime feature, or data migration state is invalid

### 19.8 Browser application host, routes, and note embeds

- [ ] Serve each active app UI in a sandboxed iframe at an app/instance-specific route and origin boundary; do not grant same-origin access to the Vulcan WebUI or other apps
- [ ] Generate a restrictive CSP from installed/granted capabilities, with no ambient network, navigation, popup, download, clipboard, camera/microphone, or embedding authority
- [ ] Serve immutable assets through `AppContentId`-keyed URLs with correct media types, cache headers, ETags, range behavior for Stored entries, and no path-based access outside the validated VFS
- [ ] Define a version-negotiated browser bridge for request/response calls, cancellation, progress, subscriptions, navigation, dialogs, notifications, theming, locale, and accessibility metadata
- [ ] Add full-page routes such as `/w/{vault}/apps/{instance}/{route}` without allowing packages to claim arbitrary daemon or WebUI paths
- [ ] Add declarative fenced `vulcan-app` embeds naming instance, view, and JSON-compatible props; never execute code contained in the note block itself
- [ ] Provide a stable fallback/diagnostic rendering for absent, disabled, untrusted, incompatible, denied, or failed apps so Markdown remains readable outside Vulcan
- [ ] Scope embed reads to both the viewer's grant and the instance grant; an author cannot make restricted content visible by embedding a more privileged app
- [ ] Define iframe lifecycle, suspension, memory ceilings, crash isolation, reload/update behavior, unsaved-state prompts, keyboard/focus handling, and accessibility requirements
- [ ] Treat browser WASM as a validated UI payload loaded through the package asset route; it receives no direct daemon/vault authority and initially calls Vulcan only through package JavaScript and the bridge

### 19.9 Versioned Vulcan App API and mutation contract

- [ ] Define a transport-neutral App API over shared domain request/report types rather than exposing CLI parsing, raw SQLite, internal Rust objects, unrestricted filesystem paths, or accidental daemon route shapes
- [ ] Provide scoped namespaces for app/instance metadata, notes, canonical query AST, search, graph, properties/Bases/tasks, artifacts/processors, external bindings/routes, state, mutation plans, jobs, events, network, secrets, typed tools, and UI integration
- [ ] Make the same logical API available to the browser bridge, direct/daemon CLI invocation, QuickJS host bindings, server-WASM imports, tests, and future native clients while permitting surface-specific serialization adapters
- [ ] Require stable typed errors, protocol feature negotiation, request/trace IDs, cancellation, timeouts, pagination/streaming limits, and bounded structured logs with secret redaction
- [ ] Require expected revisions/content hashes for direct mutations and return typed stale-state reports rather than last-writer-wins behavior
- [ ] Route multi-file or consequential changes through a plan/preview/apply workflow with exact accepted inputs, permission checks against old and resulting state, application-level write locking, incremental rescan, and optional auto-commit
- [ ] Expose app-owned state APIs according to declared data classification; never let an app relabel canonical data as cache to bypass history, sync, backup, or deletion review
- [ ] Let apps call visible skill commands through the typed registry with input/output validation, recursion limits, and preserved effective permission ceilings; do not add arbitrary CLI/shell escape hatches
- [ ] Keep App API schemas and daemon/OpenAPI/browser/WASM projections generated or conformance-tested from the same domain contracts

### 19.10 Events, jobs, and background execution

- [ ] Define explicit manifest entrypoints for user-invoked functions, retained jobs, scheduled work, and lifecycle events; importing a module must not register hidden side effects
- [ ] Extend the shared plugin/app event registry with filtered post-scan file events for create/change/delete, covering Markdown, attachments, Canvas, Bases, app data, and other classified artifacts rather than only note-specific hooks
- [ ] Let subscriptions declare bounded path, file-kind, extension, and media-type filters that load without executing app code; reject invalid or overly broad subscriptions according to installation policy
- [ ] Deliver metadata-only file events after the stable file fingerprint and incremental scan are known, including change kind, canonical path, document/artifact kind, media type, size, BLAKE3 digest, causal ID, and prior digest where available; reading bytes remains a separately authorized App API call
- [ ] Coalesce editor/write bursts by path and digest, preserve deterministic ordering, suppress exact duplicate deliveries, and prevent an app's output writes from creating unbounded self-trigger loops
- [ ] Reuse daemon retained-job scheduling, cancellation, progress, restart, and bounded history rather than letting app runtimes own untracked background threads or timers
- [ ] Scope every invocation to an initiating user grant or an explicit attenuated service grant; never silently promote a browser session into durable background authority
- [ ] Require independent approval for each background/event capability and expose active schedules/subscriptions in admin/CLI surfaces
- [ ] Filter event payloads before delivery, coalesce bursts, bound queues/retries, prevent self-trigger loops, and preserve causal/request identifiers across nested calls
- [ ] Define update/disable/uninstall behavior for queued and running calls: cooperative cancellation at safe boundaries, no new claims after disablement, and retained typed terminal status
- [ ] Prevent two mutating invocations for the same instance/store from violating Vulcan's write serialization while allowing bounded independent read/computation work
- [ ] Add per-app/instance concurrency, CPU, memory, output, log, network, and job-duration ceilings enforced by the host rather than trusted runtime code
- [ ] Add a reusable `ArtifactProcessor` contract in the core/app boundary with typed `capabilities`, `plan`, `submit`, `status`, `cancel`, `fetch`, and `validate` operations; provider adapters perform finite calls while the daemon owns polling, scheduling, and retained lifecycle
- [ ] Key conversion idempotency and durable bindings by provider, source BLAKE3 identity, recipe/version identity, and selected output kind; persist remote job/artifact identity outside `cache.db` so restart or reindex cannot duplicate or lose work
- [ ] Never perform upload, conversion, polling, or artifact import inside a blocking file-write hook. A filtered post event may enqueue a bounded job, and source note/attachment creation succeeds independently of remote processor availability
- [ ] Route fetched MDAF, TextPack, or other processor output through the existing strict artifact validator and a mutation-free import preview before applying canonical Markdown/assets; local collisions, source drift, recipe drift, and permission changes fail closed
- [ ] Expose the same processor registry to apps, plugins, CLI, daemon jobs, and future media workflows instead of adding Blobforge-specific behavior to the watcher or parser

### 19.11 QuickJS host functions and TypeScript authoring

- [ ] Treat TypeScript as an authoring language: `apps pack` builds or consumes compiled ECMAScript modules, source maps, schemas, and declared entrypoints; production runtime does not compile TypeScript implicitly
- [ ] Reuse rquickjs behind the existing `js_runtime` feature flag and strict resource limits; an app requiring QuickJS is incompatible rather than partially executed when the feature is disabled
- [ ] Run named exported functions under a host-function model callable by daemon RPC/jobs or direct CLI execution; apps cannot bind sockets, add arbitrary axum middleware, create unmanaged threads, or become independent servers
- [ ] Provide no Node.js globals, CommonJS loader, ambient filesystem/process/environment access, native addons, or undeclared network module loading
- [ ] Resolve imports only through the validated package VFS and declared built-in Vulcan modules; reject package escapes and dynamic unresolved imports
- [ ] Inject a request-scoped context containing the attenuated App API, cancellation, progress, structured logging, caller/instance metadata safe to disclose, and direct calls to declared WASM components
- [ ] Validate function input before instantiation/call and output afterward against declared schemas; bound serialized input/output, module graph depth, stack, heap, CPU time, and nested call depth
- [ ] Run long operations as retained jobs instead of preserving unbounded QuickJS contexts; define context reuse/isolation policy so state cannot cross callers or grants accidentally
- [ ] Add authoring templates and tests for UI-to-function RPC, pure functions, mutation planning, network-denied behavior, optional capabilities, nested tools, and JS-to-WASM calls

### 19.12 Server-side WebAssembly runtime

- [ ] Introduce a replaceable server-WASM runtime adapter, preferably in a dedicated optional crate/feature, without coupling package validation or the App API to one engine
- [ ] Make server WASM a peer function/job runtime: direct CLI, daemon RPC, or jobs may invoke it directly, QuickJS may call it as a declared component, and pure components may have no host imports
- [ ] Define and version a Vulcan component ABI using an explicit interface description rather than an ad hoc raw-memory `alloc(pointer, length)` convention; begin with bounded canonical JSON values if needed while preserving a path to richer typed records/resources
- [ ] Permit only declared Vulcan host imports for query, notes, mutation plans, artifacts, state, network, secrets, jobs/progress, time/randomness, and logging; do not enable ambient WASI filesystem, sockets, environment, process, or clocks
- [ ] Instantiate only imports covered by the effective grant and fail closed on missing, unknown, or denied imports; a component cannot dynamically acquire broader host functions
- [ ] Enforce fuel/epoch interruption or equivalent CPU limits, linear-memory/table/stack limits, component size limits, output limits, cancellation, and bounded instance pooling
- [ ] Preserve effective authority and trace/causal identity across direct WASM calls and QuickJS-to-WASM nested calls; apply the shared recursion/concurrency ceilings
- [ ] Support compiled-language server backends, deterministic parsers/converters, financial/scientific calculations, ranking, media processing, and other isolated computation without claiming that WASM is automatically faster than QuickJS
- [ ] Keep browser-WASM and server-WASM targets explicit and separate in the manifest; browser modules use browser ABI/tooling and never inherit server host imports
- [ ] Add conformance components in Rust plus at least one other supported toolchain, including denied-import, resource-exhaustion, cancellation, malformed-component, schema-mismatch, and deterministic-output cases

### 19.13 Canonical artifacts, SQLite data, and synchronization

- [ ] Allow apps to declare and bind canonical non-Markdown artifacts, including SQLite databases, without treating them as rebuildable merely because they use SQLite
- [ ] Keep app data physically separate from immutable `.vapp` packages by default so package upgrades, signatures, rollback, distribution, sync, and backup do not become stateful code rewrites
- [ ] Reserve self-modifying package-plus-data files for a future explicit portable-document mode with separate identity/migration semantics; v1 `.vapp` files remain immutable
- [ ] For canonical SQLite stores, serialize writers through Vulcan, use safe connection settings, prevent extension loading, bound database/page/schema complexity, and ensure WAL/SHM sidecars cannot escape the captured mutation boundary
- [ ] Materialize SQLite changes as an atomic canonical artifact replacement or a formally captured file set, then rescan/history/sync it like other user data
- [ ] Treat concurrent cross-device SQLite artifact changes as required-review conflicts in v1; do not claim Git can semantically merge database bytes
- [ ] Design a later optional deterministic artifact-merger contract over immutable base/local/remote inputs, declared schema/version, strict resource limits, exact output validation, and evidence reports; never execute arbitrary app merge code merely because sync discovered a conflict
- [ ] Make canonical artifacts visible to permission filters, history, backup/export, conflict inspection, storage accounting, retention, and explicit deletion workflows without exposing their contents to unauthorized apps or users
- [ ] Add crash/restart, concurrent mutation, sync conflict, migration, backup/restore, WAL cleanup, corrupt database, and package-uninstall-with-data-preserved integration tests

### 19.14 Publication and distribution

- [ ] Define per-app publication modes: `none`, read-only `static`, live `server`, and future explicit `standalone` export transform; default to `none`
- [ ] Let static-capable apps declare export-safe UI entrypoints and data contracts; Phase 9.20 builds only filtered read-only data selected by the publisher grant and never bundles host functions, secrets, write grants, or private instance configuration
- [ ] Reuse validated package assets and shared renderer/route/search contracts while assigning collision-free content-addressed publication paths
- [ ] Make server-backed published apps resolve ordinary authenticated/limited share grants and the same App API filters rather than creating a public bypass
- [ ] Define a catalog-independent install source abstraction for local files, URLs, and future catalogs; URL installation obeys normal network permissions, transport limits, digest/signature checks, and explicit install approval
- [ ] Keep OCI as an optional future distribution envelope carrying an ordinary `.vapp` blob plus provenance/attestations; do not expose OCI layout or registry semantics to the runtime VFS
- [ ] Support offline export/import of exact package blobs and detached metadata while preserving both content and blob identities
- [ ] Add provenance, license, publisher, vulnerability/withdrawal, update-channel, and revocation fields without making a centralized Vulcan marketplace mandatory

### 19.15 App-provided CLI commands and terminal surfaces

- [ ] Let the manifest declare named CLI commands with stable command name, summary/help, runtime target and export, typed positionals/options, bounded stdin mode, input/output schemas, examples, capability request IDs, resource ceilings, and review annotations
- [ ] Define a closed portable CLI descriptor rather than deriving command syntax from arbitrary JSON Schema: specify positional order, long/short flags, booleans, enums, repeatability, required/default values, mutual exclusions, value bounds, and path/value semantics explicitly
- [ ] Reject reserved names, ambiguous prefixes, duplicate flags/positionals, unsafe aliases, incompatible schemas, and command collisions during package validation; v1 app commands do not inject aliases or commands into Vulcan's global/top-level namespace
- [ ] Invoke commands through the stable namespace `vulcan apps run <instance> <command> [declared arguments]`; expose discovery through `apps commands list|show`, shell completion, `help`, and `describe` without making app commands implicit MCP tools
- [ ] Parse arguments in Vulcan and pass a validated structured input object to the declared QuickJS or server-WASM entrypoint; never forward an unparsed `argv`, shell command, ambient environment, or raw host process interface
- [ ] Preserve direct local operation: when the package/runtime/data are locally available, app commands use synchronous shared application services without requiring the daemon; client mode may invoke the same contract through the daemon, and daemon-only jobs/features fail explicitly rather than changing semantics silently
- [ ] Resolve direct and daemon invocation through the same caller profile ∩ installation ∩ instance ∩ manifest ∩ runtime ∩ policy authority intersection; command invocation cannot inherit the browser host's session or a broader service grant accidentally
- [ ] Preserve Vulcan output conventions: `--output json` returns a stable host envelope with schema-validated result data, streamed results use line-delimited JSON, human output is host-rendered from structured values or a bounded declared text field, and logs/progress remain on stderr
- [ ] Map typed app errors to stable Vulcan error categories and exit behavior; app-controlled numeric exit codes, ANSI escapes, terminal control sequences, or arbitrary stdout/stderr writes cannot bypass the host contract
- [ ] Support bounded declared stdin modes (`none`, UTF-8 text, JSON, or bytes) with explicit size/media limits and non-interactive operation; commands must accept all required values through arguments/stdin/config rather than requiring prompts
- [ ] Allow optional TTY ergonomics such as host-owned confirmations, selectors, progress, and forms only as enhancements over a complete non-interactive path; mutating commands still expose dry-run or plan/apply semantics
- [ ] Defer full-screen terminal apps to a separately gated host-owned terminal scene/event protocol with cleanup, resize, input, paste, accessibility, output-capture, and `terminal.interactive` permission tests; do not grant raw TTY file descriptors or arbitrary escape-sequence passthrough in v1
- [ ] Let app CLI commands call declared host functions, server-WASM components, and visible typed tools with shared recursion/cancellation/resource limits; do not convert an app package into a native executable launcher
- [ ] Add conformance tests for help/completion, direct-versus-daemon parity, JSON/human/stream output, stdin limits, non-TTY behavior, dry-run mutation safety, feature-disabled runtimes, denied capabilities, nested calls, cancellation, and terminal-output injection

### 19.16 CLI, WebUI administration, and developer experience

- [ ] Add `vulcan apps discover|inspect|validate|list|show|install|update|disable|uninstall|doctor` with stable JSON reports and `--dry-run` on every mutation
- [ ] Add `vulcan apps instances list|show|create|set|enable|disable|remove` with explicit vault/instance selection, capability/data bindings, migration previews, and no interactive-only requirements
- [ ] Add `vulcan apps grants show|plan|apply|revoke` over Phase 17 authority rather than a parallel ACL file; human output clearly separates requested, granted, denied, optional, and policy-ceiling capabilities
- [ ] Add `vulcan apps pack|lint|test|unpack` developer commands and `describe`/help coverage for manifest, identities, ZIP profile, browser bridge, App API, QuickJS, WASM, data classes, signing, and publication
- [ ] Add WebUI pages for discovered packages, exact identity/provenance, signatures, requested/granted capabilities, installed versions, instances, data stores, jobs/events, runtime health, updates, migrations, disablement, and uninstall/data-retention choices
- [ ] Provide an explicit capability-delta review before install/update/instance enablement and an inspectable audit trail for approvals, invocations, migrations, and denied operations
- [ ] Add a local development workflow with watch/repack/reload that remains visibly marked as development mode and never converts directory mutability into production trust
- [ ] Avoid user-facing collision with the existing `vulcan-app` orchestration crate by naming Rust domain/runtime types `VaultApp*`/`AppRuntime*` while keeping the product and CLI group “Vulcan Apps”/`apps`

### 19.17 Verification and hardening

- [ ] Check in one canonical valid package with known manifest bytes, payload digests, `AppContentId`, `PackageBlobId`, and signature result plus hostile fixtures for every ZIP/path/JSON/manifest/resource-limit rejection
- [ ] Cover duplicate ZIP entries, duplicate JSON keys, noncanonical JSON, incorrect hashes/sizes, missing/extra entries, traversal/absolute/backslash/case-collision paths, malformed/overlapping/prepended/trailing ZIPs, encryption, links, comments/extra fields, ZIP64, unsupported codecs/versions, bombs, and integer overflow
- [ ] Fuzz the single raw-bytes-to-`ValidatedAppPackage` boundary and runtime module/component loaders; assert malformed input performs no extraction or filesystem writes and cannot reach execution
- [ ] Add property tests showing repacks may change `PackageBlobId` while preserving `AppContentId`, any logical manifest/payload change changes `AppContentId`, and canonical builders reproduce the promised identity/output guarantees
- [ ] Add authorization tests for discovery-without-execution, code-change trust invalidation, caller/installation/instance/runtime intersection, optional capabilities, path/query filtering, network-domain ceilings, secret redaction, nested JS/WASM/tool calls, and background grants
- [ ] Add browser security tests for iframe origin isolation, CSP, forged/cross-instance messages, schema violations, revoked sessions, embed filtering, asset paths/ranges/media types, and denial of daemon credentials
- [ ] Add lifecycle tests for interrupted install/update/migration, mutable source replacement, rollback, concurrent invocation, disable/uninstall during jobs, retained data, orphan blob cleanup, and daemon restart
- [ ] Document when to choose an app, app CLI command, plugin, skill command, script, QuickJS function, browser WASM, or server WASM; do not advertise WASM as automatically faster
- [ ] Perform the required bundled-skill impact review: extend existing plugin/tool/configuration skills when app discovery, permission review, or authoring changes agent workflows; add/register a new managed app-authoring skill only if it is a distinct reusable workflow
- [ ] Review `docs/assistant/AGENTS.template.md`, assistant integration docs, static-site docs, security guidance, and daemon/WebUI API docs for app-aware selection, trust, and mutation rules

### 19.18 First-party example apps and conformance portfolio

Every reference app is an ordinary signed or explicitly locally trusted `.vapp` built with the public package, bridge, CLI, runtime, event, and App API contracts. Reference apps receive no private daemon endpoints, implicit grants, relaxed validation, filesystem shortcuts, or other privileges unavailable to third-party packages. Keep each app in its own fixture/package project with pinned frontend/runtime dependencies, deterministic builds, declared example data, capability snapshots, and direct/daemon/browser conformance tests.

#### 19.18.1 Presenter

- [ ] Build `dev.vulcan.presenter` as the first read-only conformance app, using a pinned existing browser presentation framework rather than implementing slide navigation, fragments, speaker notes, and export from scratch; prefer [reveal.js](https://github.com/hakimel/reveal.js) for the initial embeddable runtime and record the version/license decision
- [ ] Let Vulcan parse and render source Markdown so wikilinks, transclusions, callouts, Mermaid, math, code highlighting, attachments, permissions, and diagnostics remain consistent with ordinary note and static-site rendering
- [ ] Support heading or delimiter slide boundaries, horizontal/vertical slide structure, fragments, themes, speaker notes, timers, audience/presenter routes, named note embeds, fullscreen, and print/PDF-friendly output
- [ ] Subscribe to filtered source-note and dependency changes, update the deck without a Vite/development server, and preserve the current slide/fragment where the changed structure still permits it
- [ ] Support static publication through Phase 9.20 without bundling private presenter state or daemon-only control channels; diagnose dynamic constructs that cannot be exported safely
- [ ] Add typed CLI commands for `present`, `list-slides`, `export`, and `doctor`, with no write capability required for the initial release

#### 19.18.2 Meeting Tool

- [ ] Build `dev.vulcan.meeting` over an ordinary Markdown agenda whose headings/items remain readable and editable without the app
- [ ] Provide facilitator and audience views with current-item highlighting, next/previous control, speaker list and raised hands, timers, parking lot, lightweight votes/temperature checks, and reconnectable session state
- [ ] Classify agenda/minutes/decisions/action items as canonical Markdown/tasks, the current item and speaker queue as explicit resumable or ephemeral session state, and participant presence as ephemeral state
- [ ] Write decisions, notes, and action items through optimistic-concurrency mutation plans scoped to the selected agenda section rather than replacing the whole note
- [ ] Resolve facilitator, speaker, participant, and viewer actions through caller and instance capabilities; an embedded audience view cannot inherit facilitator authority
- [ ] Add `start`, `show`, `next`, `previous`, `queue`, `yield`, `decision`, `action`, and `finish` CLI commands with direct/daemon parity and a complete non-interactive path
- [ ] Ship a single-facilitator/multiple-viewer baseline on Phase 19 events; layer simultaneous collaborative note editing on Phase 16 rather than inventing an app-specific CRDT

#### 19.18.3 Capability-free minigame and Wiki Quest

- [ ] Build a small capability-free browser minigame that requests no vault, network, secret, mutation, job, or host authority and stores progress only in bounded temporary/device-local app state
- [ ] Use the game as the iframe/CSP/input/audio/browser-WASM/resource-limit baseline and prove denial of undeclared App API calls without degrading normal gameplay
- [ ] Add an optional “Wiki Quest” mode that derives a navigable map or puzzles from the permission-filtered link graph while preventing inference of restricted nodes through topology, counts, labels, suggestions, timing, or missing-target behavior
- [ ] Keep canonical achievements/progress as an explicit opt-in capability and mutation plan rather than silently writing game state into the vault

#### 19.18.4 Blobforge Workbench and media-processing integration

- [ ] Build `dev.tionis.blobforge` as a combined WebUI and app-CLI client for the [Blobforge](https://github.com/tionis/blobforge) coordinator: source ingestion, queue dashboard, job status, workers, failures, retained artifacts, recipe selection, preview/download, hydration, and cancellation where supported
- [ ] Call the coordinator through typed HTTP client functions using an approved origin and opaque instance secret; do not require the external `blobforge` executable or expose its token to browser code
- [ ] Expose namespaced `ingest`, `dashboard`, `status`, `workers`, `artifacts`, `request-conversion`, and `hydrate` commands over the same typed host functions used by the browser UI
- [ ] Subscribe optionally to filtered new/changed PDF events, enqueue conversion only through the reusable `ArtifactProcessor`, and key requests by canonical source BLAKE3 plus exact recipe identity so repeated scans are idempotent
- [ ] Retain coordinator job IDs, signed-transfer provenance, selected recipes, and imported artifact bindings outside `cache.db`; redact tokens and bounded signed URLs from logs, reports, notes, and canonical mappings
- [ ] Validate downloaded MDAF/TextPack artifacts, compare source/recipe identity, preview Markdown/asset materialization, and apply through the ordinary artifact import and conflict checks
- [ ] Keep an optional native/offline Blobforge wrapper as a separately approved `execute`-capability adapter only; it is not the default client architecture and never receives shell authority implicitly
- [ ] Add contract tests against a bounded fake coordinator for signed transfers, redirects, retries, cancellation, progress, stale source/recipe results, malformed artifacts, permission revocation, restart recovery, and duplicate file events

#### 19.18.5 Feed Reader

- [ ] Build `dev.vulcan.feeds` with RSS 2.0, Atom, and JSON Feed parsing plus OPML import/export; unsupported namespaces and malformed entries surface diagnostics rather than disappearing silently
- [ ] Provide WebUI and CLI flows for subscription management, manual refresh, unread/star/archive state, feed/entry search, reading view, and explicit “save as note” or rule-based capture
- [ ] Run refresh through retained scheduled jobs with per-instance concurrency/rate limits, cancellation, offline/retry state, conditional HTTP requests (`ETag`/`Last-Modified`), bounded redirects/body sizes, and approved-domain checks on feeds plus discovered item/content URLs
- [ ] Deduplicate with durable feed identity and entry bindings using stable entry IDs when trustworthy plus normalized URL and content BLAKE3 fallbacks; cache rebuild or feed reordering must not recreate captured entries
- [ ] Sanitize remote HTML and media before rendering, never execute feed scripts/styles, proxy or localize remote assets only under explicit policy, and prevent server-side request forgery or credential forwarding across origins/redirects
- [ ] Support authenticated/private feeds through opaque secrets that remain outside package, canonical notes, browser storage, logs, OPML, and exports
- [ ] Classify subscription definitions and capture rules as explicit canonical or device-local instance configuration, unread UI state as app state, fetched bodies as bounded derived cache, and saved entries as ordinary canonical Markdown with source/provenance fields
- [ ] Reuse Phase 15 external-route and reconciliation contracts for durable captured-document bindings when available; never treat removal from a remote feed as authority to delete a saved local note
- [ ] Add `subscriptions`, `add`, `remove`, `refresh`, `entries`, `read`, `star`, `archive`, `capture`, and `opml` CLI commands with `--output json`, dry-run/plan behavior for mutations, and direct-mode diagnostics when scheduling requires the daemon
- [ ] Test hostile XML/HTML, entity expansion, huge feeds, duplicate/reused IDs, URL normalization, redirect credential leakage, conditional refresh, authenticated feeds, scheduler restart, permission filtering, and idempotent capture

#### 19.18.6 Finance and Collection Studio

- [ ] Build a bounded finance prototype as the canonical-artifact stress test: transparent Markdown and/or explicit canonical SQLite data, transactional mutation plans, migrations, audit/history/export, conflict preservation, and no network access by default
- [ ] Build `dev.vulcan.collection-studio` as a schema-driven forms/table/detail app over typed Markdown or mdbase-compatible collections, proving reusable validation, multiple instances, property/link editors, filtered queries, bulk mutation previews, import/export, and app-defined views without hiding records in UI-only state
- [ ] Keep both examples domain-bounded and auditable; they validate general platform contracts but do not turn finance rules or a second database/query language into Vulcan core semantics

### 19.19 Recommended delivery order and gates

- [ ] **Gate A — normative format:** Complete 19.1–19.3's domain model, canonical manifest, BLAKE3 vectors, strict ZIP profile, signature envelope, format specification, hostile fixtures, and raw-package fuzz target before any package code may execute
- [ ] **Gate B — package substrate:** Complete 19.4 plus the immutable installation/blob-store portion of 19.5 and CLI descriptor validation/discovery from 19.15; `inspect`, `validate`, `pack`, `lint`, install preview/apply, exact identity reporting, and no-extraction invariants work without enabling QuickJS, WASM, or the WebUI
- [ ] **Gate C — static read-only MVP:** Complete instances, device-local trust/grants, the iframe host, content-addressed asset serving, read-only bridge/App API, full-page routes, note embeds, CLI/WebUI administration, Presenter, and the capability-free minigame; this is the first user-facing release
- [ ] **Gate D — reviewed mutation and durable data:** Add optimistic concurrency, plan/preview/apply mutations, data classifications/bindings, migration journals, canonical artifact handling, retained jobs/events, Meeting Tool, Collection Studio, and the finance prototype's non-networked storage path
- [ ] **Gate E — QuickJS functions and CLI apps:** Add compiled-TypeScript authoring, VFS module loading, resource-limited request/job/direct CLI execution, namespaced QuickJS CLI commands, nested typed tools, and JS-to-WASM-ready component calls behind `js_runtime`; static apps remain available without that feature
- [ ] **Gate F — server WASM and compiled CLI apps:** Select and pin the runtime, finalize the component ABI/host imports, add direct CLI/RPC/job and nested invocation, namespaced WASM CLI commands, resource enforcement, multi-toolchain conformance components, and a feature-disabled compatibility path
- [ ] **Gate G — publication and distribution:** Add static/live publication modes, source/catalog abstraction, publisher policy and update/revocation UX, optional OCI transport, offline import/export, and complete security/lifecycle conformance
- [ ] **Gate H — network and processing examples:** Complete filtered attachment events, the reusable `ArtifactProcessor`, Feed Reader, and Blobforge Workbench with durable idempotency/reconciliation, scheduler restart, secret/network isolation, validated artifact import, and direct/daemon CLI parity
- [ ] Parallelization rule: after Gate A freezes the format contracts, package tooling/VFS, browser-host prototyping, App API domain types, and runtime-adapter investigations may proceed independently; installation authority and runtime execution must converge on the same validated package and permission contracts before Gate C or later ships
- [ ] Completion gate: validated immutable packages can be discovered, installed, granted, instantiated, embedded, invoked through typed CLI commands, updated, disabled, and uninstalled without extraction or authority expansion; the Phase 19.18 portfolio exercises static, write-enabled, zero-capability, CLI, network, scheduled, artifact-processing, canonical-SQLite, and schema-driven workflows end-to-end; package/runtime/data state survives restart and sync safely; all CLI/API outputs and security invariants have unit, integration, conformance, and fuzz coverage

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
                                                   ↓            ↘             ↑             ↓
                                                  9.3      Phase 9.20 ─→ Phase 9.29   Phase 17 (Identity & capabilities)
                                                   │      (Static site) (cleanup gate)       ↓
                                                   └──────→ Phase 11 (Git versioning)   Phase 13 (WebUI browse)
                                                                  ↓                  ↖        ↓
                                                          Phase 12 (Sync)      (also from 9.20)
                                                                                         ↓
                                  Phase 18 (Canvas) ───→ 18.5 (Canvas WebUI read) ← Phase 14 (WebUI write + Automerge)
                                    ↑                                                      ↓
                                  Phase 7                                      18.6 (Canvas WebUI editor)
                                                                                        ↓
                                                        Phase 15 (Extensibility) ← Phase 10   Phase 16 (Wiki + live collab)
                                                                                                        ↓
                                                                                            16.6 (Local-first / WASM) [future]

                            Phase 10 ─→ Phase 17 ─→ Phase 13 ─────────────────┐
                                                        └→ Phase 14 (writes) ┤
                            Phase 9.20 (publication) ────────────────────────┤
                            Phase 9.24 (QuickJS/tools) ──────────────────────┴→ Phase 19 (Vulcan Apps)
```

Phase 8 (Performance) is independent and can proceed in parallel with Phases 9 and 10 after Phase 7.
Phase 9.20 (Static site builder) is intentionally scheduled after Phase 9 and before Phase 10 in roadmap priority order, but Phase 10 remains technically independent from static-site rendering if daemon work becomes urgent.
Phase 9.29 (Pre-daemon maintainability and feature-boundary cleanup) is the hard cleanup gate before Phase 10. Phase 10 should not start until 9.29's feature matrix, crate boundaries, MCP split, and verification matrix are complete.
Phases 9 and 10 can proceed in parallel after Phase 7 only for design exploration. Implementation work for the daemon should wait for 9.29; 9.20 remains the recommended rendering/publication bridge before WebUI/wiki work.
Phase 11 requires 9.3 (git module) and 10 (daemon). Phase 12 requires 10 and 11.
Phase 17 requires 10 (daemon). Sub-phases 17.1–17.3 (canonical authorization objects, reserved mutation/ingress controls, rooted/delegable grants, capability resolution, and permission-filtered queries) must complete before Phase 13.
Phase 13 requires 10, 9.20, and 17.1–17.3. Phase 14 requires 13 and 10's write endpoints. Phase 14 introduces Automerge as the document model.
Phase 15 requires 10. Phase 16 requires 13, 14, 9.20, and 17.4–17.5 (document secrets, share links). Phase 16 also uses the Automerge foundation from Phase 14.
Phase 17.6 (OIDC/SSO) is a future direction — deferred until local identity, rooted grants, delegation, and revocation are stable.
Phase 16.6 (local-first/WASM) is a future direction beyond the current roadmap scope.
Phase 18 (Canvas) core parsing/indexing/CLI (18.1–18.4) depends on Phase 7. WebUI read-only rendering (18.5) depends on Phase 13. Interactive canvas editor (18.6) depends on Phase 14. Canvas capability enforcement follows from Phase 17.
Phase 19 (Vulcan Apps) requires Phase 10, Phase 13, and Phase 17.1–17.5 for its package installation, sandboxed browser host, identity/session, capability, secret, and share foundations. Write-enabled browser apps additionally require Phase 14's mutation/review surface. Static app publication uses Phase 9.20; QuickJS functions reuse Phase 9.18.5 and Phase 9.24. Server-side WASM is a supervised app-function runtime with its own ABI and does not depend on Phase 16.6's collaborative local-first investigation. The Feed Reader example reuses Phase 15 external bindings when present, while Blobforge motivates a generic Phase 19 artifact processor rather than a knowledge-system relay. Phase 19 does not block Phases 12, 15, 16, or 18.
Phase 9.8 (Dataview) builds on Phase 4 (properties and Bases expression language) and Phase 9.6 (search operators, task search). Sub-phase 9.8.1 (inline fields + type inference) and 9.8.2 (list items and tasks) extend the parser pipeline. Sub-phase 9.8.3 (file.* metadata) synthesizes implicit fields from existing cache tables. Sub-phase 9.8.4 (type system and expression evaluator) extends the value representation with Date, Duration, Link types, ~60 built-in functions with auto-vectorization, lambda expressions, link indexing, swizzling, and null ordering. Sub-phases 9.8.5–9.8.7 (DQL parser, evaluation, inline expressions) build the query surface on top. Sub-phase 9.8.8 (DataviewJS) adds sandboxed JS evaluation with full dv API and DataArray behind a `js_runtime` compile-time feature flag. Sub-phase 9.8.9 imports Dataview plugin settings from `.obsidian/plugins/dataview/data.json`. Dataview metadata and queries are available to all later phases (daemon, web, wiki) as foundation infrastructure.
Phase 9.9 (Templater) builds on Phase 9.7 (enhanced templates) and Phase 9.8.8 (DataviewJS sandbox for JS execution commands). Native tp.date/tp.file/tp.frontmatter modules need no JS; tp.web, user scripts, and execution commands reuse the DataviewJS sandbox.
Phase 9.10 (Tasks plugin) builds on Phase 9.8.2 (task extraction) and provides the parsing and query layer for inline checkbox tasks: Tasks DSL parser, recurring task expansion (RRULE), dependency graph, and custom status types. This shared infrastructure is reused by 9.15 (TaskNotes). The CLI surface is unified under `vulcan tasks` (defined in 9.15.9).
Phase 9.11 (Kanban) builds on Phase 9.8.2 (list item extraction) and Phase 7.1 (metadata refactors). TUI/WebUI rendering depends on Phase 9.2 (browse TUI) and Phase 13 (WebUI) respectively.
Phase 9.12 (external agent integration) builds on Phase 5 (vectors) and Phase 7.12 (query model). Independent of 9.9–9.11. The tool interface is aligned with 9.18 command reorganization — tools map 1:1 to CLI commands, and external runtimes consume them through `describe`/`help` plus vault `AGENTS.md` and skills. Session history and compaction stay in the external runtime by default. The optional embedded-host follow-on was evaluated and retired in Phase 9.21; 9.12.8 is the deferral gate, not a second implementation plan.
Phase 9.18 (CLI redesign) has varying sub-phase dependencies: 9.18.1 (reorg) and 9.18.2 (note CRUD) can start after Phase 7; 9.18.3 (query enhancements) after 7.12; 9.18.5 (JS runtime) after 9.8.8; 9.18.6 (web tools) is standalone; 9.18.7 (docs) is standalone; 9.18.8 (git) after 9.3; 9.18.9 (task mutations) after 9.10 and 9.15. The command tree reorganization (9.18.1) should land last — build new commands first, then rename in one pass.
Phase 9.13 (QuickAdd) provides Obsidian-compatible capture format syntax and settings import. Macro/scripting functionality is handled by the JS runtime (9.18.5) and existing CLI commands rather than a separate automation DSL.
Phase 9.15 (TaskNotes) is Vulcan's primary task management model. Builds on Phase 4 (properties/Bases, including 4.5.1 custom source types) and Phase 9.8 (Dataview metadata). Reuses shared task infrastructure from 9.10 (recurring tasks, dependencies, custom statuses). The unified `vulcan tasks` CLI (9.15.9) covers both TaskNotes file-based tasks and inline checkbox tasks. Calendar sync (9.15.10), HTTP API (9.15.12), and calendar Bases views are deferred to post-Phase 9. Time tracking (9.15.6) ships core+CLI only; GUI deferred to post-WebUI. Reminders (9.15.7) ship core evaluation only; delivery channels deferred to chat/daemon phases.
Phase 9.16 (Periodic notes) builds on Phase 1 (document indexing) and Phase 9.7 (template variables). It provides shared infrastructure for `file.day` resolution (9.8.3), Kanban date linking (9.11), QuickAdd daily note capture (9.13), and TaskNotes pomodoro storage (9.15). Can start as early as Wave 2 but `file.day` can be stubbed pending its completion.
Phase 9.17 (Unified settings import) infrastructure (9.17.1–9.17.3) depends only on 9.5 (config layering) and can start in Wave 2. Core importer (9.17.4) depends on 9.17.1. Dataview importer (9.17.5) depends on 9.17.1 and 9.8.9. Batch commands (9.17.6) depend on 9.17.1 and any two or more importers on the trait. Init integration (9.17.7) depends on 9.17.6. Individual plugin importers (9.9.4, 9.10.5, 9.11.4, 9.13.3, 9.15.11, 9.16.4) are refactored or implemented as `PluginImporter` (9.17.1) within their respective phases.
Phase 9.20 (Static site builder) is the recommended bridge between CLI completion and daemon/WebUI work. It reuses the parser, graph, query, Dataview, Bases, and task foundations to produce a shared HTML renderer, route planner, and static search/graph/preview assets. Phase 10 does not technically depend on it, but Phase 13 and Phase 16 should.
Phase 9.29 (Pre-daemon maintainability and feature-boundary cleanup) builds on the completed Phase 9 shared-service, MCP, skill-command, and static-site work. It is deliberately broad: feature-gate AI/web/OAuth/vector dependencies, split oversized reusable modules, slim remaining CLI dispatch/rendering clusters, split MCP transport/auth/catalog/handler concerns, and add guardrails so Phase 10 can depend on shared libraries instead of `vulcan-cli` internals.
Phase 9.37 (archival document extraction and OCR investigation) builds on Phase 7.3's attachment graph and optional derived-text indexing plus Phase 9.36's MDAF boundary. It is a non-blocking investigation: lightweight local extraction hardening may remain in the existing core/CLI path, daemon scheduling belongs after Phase 10, and evidence-preserving conversion remains an external-producer-to-MDAF workflow unless the decision record justifies a narrower change.
Phase 9.23 (adaptive MCP tool packs) builds on 9.19.15's protocol-native MCP surface and 9.19.13's permission layer. It keeps MCP tool exposure typed and permission-aware while replacing the fixed `core|extended|admin` ladder with composable packs and optional session-local tool refresh for clients that honor `notifications/tools/list_changed`.
Phase 9.24 (vault-native skill command tools) builds on 9.18.5 (JS runtime), 9.19.12 (plugin/runtime execution substrate), 9.19.13 (permission layer), 9.19.15 (protocol-native MCP registry), and 9.23 (pack-aware MCP exposure). It introduces a shared programmable tool registry for vault-defined callable tools, with static skill-command metadata discovery and QuickJS as the initial runtime backend. Skills remain the package/guidance format; plugins remain event hooks; exposed skill commands are direct request/response callables available through CLI, `describe`, MCP, and the internal JS API.
Phase 4.5.1 (Custom Bases source types) extends the Bases evaluator with pluggable data sources. The trait and `FileSource` extraction are part of Phase 4. The actual custom source registrations happen in Phase 9.15.8 (TaskNotes Bases views).
Phase 18.8 (Excalidraw) is part of Phase 18 (Canvas) — both are visual JSON-based document types. Parsing/indexing (18.8.1–18.8.2) depends on Phase 7. WebUI rendering (18.8.3) depends on Phase 13. WebUI editing (18.8.4) depends on Phase 14.
Phase 14.1 (Note editor) includes Advanced Tables-style table editing for the WebUI — tab navigation, column management, sorting, CSV paste, and formula support.
See "Phase 9 implementation order" section (after 9.17) for the consolidated critical path and parallelization guide within Phase 9.

---

## Phase 9.23: Adaptive MCP tool packs and dynamic discovery

**Goal:** Replace the current fixed MCP exposure ladder (`core`, `extended`, `admin`) with composable named tool packs and an optional adaptive mode where a client can request more packs during an MCP session without falling back to a generic "run arbitrary CLI commands" escape hatch.

**Why this phase exists:** The current MCP surface is intentionally small, but its pack model is too rigid. Real clients often want combinations such as "read + search + web" or "notes + tasks, but no config/index". A single monotonic ladder makes those combinations awkward, and it forces the initial tool set to either be too broad for context efficiency or too narrow for serious use. Generic MCP clients also differ in how much they can preload from prompts/resources, so Vulcan needs a protocol-native way to keep the initial set small while still expanding the typed registry on demand.

**Builds on:** 9.19.13 (permission layer), 9.19.15 (protocol-native MCP server), and 9.18.7 (stable `describe`/`help` docs). Any dynamic mode must preserve the same transport/session contract already used by stdio and Streamable HTTP so Phase 10 can reuse it unchanged.

**Scope rule:** Keep the MCP surface typed, explicit, and permission-aware. Do **not** add a generic `run_cli_command`, `exec`, or shell passthrough tool as a substitute for proper MCP tool coverage. Permission profiles remain authoritative; enabling a pack must never reveal or authorize tools that the selected profile denies.

### 9.23.1 Pack taxonomy and registry model

- [x] Replace the single `core|extended|admin` exposure ladder with a composable pack set model where one tool can belong to one or more named packs
- [x] Define an initial pack taxonomy that is capability-oriented rather than strictly tier-oriented, for example `notes-read`, `notes-write`, `search`, `tasks`, `web`, `git`, `config`, `index`, and similar narrowly scoped bundles
- [x] Make canonical pack names the only supported selectors instead of carrying forward legacy tier aliases or bundle shorthands
- [x] Keep `vulcan describe --format mcp` and the live MCP server on the same underlying registry so exported tool definitions and live exposure cannot drift

### 9.23.2 CLI pack selection and reporting

- [x] Extend `vulcan mcp` and `vulcan describe --format mcp` to accept multiple selected packs rather than exactly one pack enum
- [x] Support ergonomic selection forms such as repeated `--tool-pack <name>` flags and comma-separated values where they do not conflict with existing CLI parsing expectations
- [x] Report the effective selected pack set in machine-readable MCP/describe output so hosts can debug why only a subset of tools is visible
- [x] Update help text and examples so users can discover pack composition without reading the source

### 9.23.3 Protocol-native pack discovery

- [x] Expose the available MCP tool packs, their descriptions, and the tools they contribute through protocol-visible discovery surfaces rather than relying on out-of-band docs alone
- [x] Provide stable resource URIs and/or a small bootstrap MCP tool for inspecting the pack catalog from generic clients
- [x] Reuse completion support so pack names can be suggested in any prompt/resource/tool argument position that accepts them
- [x] Ensure pack discovery itself respects permission profiles by clearly distinguishing "pack exists" from "pack would currently expose tools under this profile"

### 9.23.4 Adaptive session-local pack negotiation

- [x] Add an optional adaptive MCP mode where clients can request pack changes during an existing session instead of restarting the server with a different static selection
- [x] Provide a minimal bootstrap surface for pack mutation such as `tool_pack_list`, `tool_pack_enable`, `tool_pack_disable`, and/or `tool_pack_set`
- [x] Treat pack mutation as a session-local registry change that triggers `notifications/tools/list_changed` and is reflected by the next `tools/list`
- [x] Make stdio and Streamable HTTP sessions behave the same way for pack mutation, registry refresh, and notification delivery

### 9.23.5 Client compatibility and fallback behavior

- [x] Keep a static mode for hosts that cannot or do not react to `notifications/tools/list_changed`
- [x] Make adaptive mode explicitly opt-in until client behavior is well understood across major MCP hosts
- [x] Define graceful degradation rules for clients that can discover packs but cannot refresh tools automatically: discovery should still help, but the server must not assume live tool replacement succeeded
- [x] Document the expected host behavior so users understand when adaptive packs work best and when they should prefer a broader static selection at session start

### 9.23.6 Security and permission composition

- [x] Ensure pack selection composes cleanly with permission profiles rather than introducing a second authorization model
- [x] Continue to hide unauthorized tools, prompts, completions, and resources even if a client enables a broader pack set
- [x] Add explicit tests for "pack enabled but still denied by permissions" cases so adaptive exposure cannot accidentally bypass the profile guardrails
- [x] Keep the pack system implementation transport-agnostic so later daemon identity and delegable-capability work can layer on the same filtering logic

### 9.23.7 Testing and rollout

- [x] Add registry tests covering pack union/intersection behavior and stable ordering of exposed tools
- [x] Add end-to-end MCP tests for adaptive pack changes over both stdio and Streamable HTTP, including `notifications/tools/list_changed`
- [x] Add regression tests showing that `describe --format mcp` and live MCP exposure stay in sync for the same selected pack set
- [x] Update help snapshots and CLI/MCP fixtures to cover the new pack model and adaptive-mode documentation

---

## Phase 9.24: Vault-native skill command tools

**Goal:** Add a first-class programmable tool registry for vault-defined callable tools written in JavaScript, exposed consistently through CLI, `describe`, MCP, and the internal JS API. The public authoring model is Agent Skills-compatible skill commands with `expose: true`, so Vulcan does not create a second user-facing extension/package format beside skills, plugins, and `vulcan run`.

**Why this phase exists:** Today Vulcan has three related but distinct programmable surfaces: Markdown skills (instructional), JS scripts via `vulcan run` (callable but not registry-backed), and JS plugins (event hooks). Skill commands fill the reusable request/response gap while keeping instructions, scripts, schemas, permissions, and documentation in one Agent Skills-compatible package.

**Builds on:** 9.18.5 (QuickJS runtime), 9.19.12 (plugin/runtime execution substrate), 9.19.13 (permission layer), 9.19.15 (protocol-native MCP registry), and 9.23 (pack-aware MCP exposure). The concrete design reference lives in `docs/assistant/custom_tools.md`.

**Scope rule:** Reuse the existing QuickJS runtime, vault trust model, and permission profiles. Do **not** add a generic "run arbitrary JS from MCP" or "shell passthrough tool" as a substitute for a typed registry. Skills remain the package/guidance format; plugins remain lifecycle hooks; exposed skill commands are structured request/response callables.

### 9.24.1 Asset model and manifest loader

- [x] Use `assistant.skills_folder` as the single public package root for skills and exposed skill command tools
- [x] Define tool asset layout inside Agent Skills-compatible packages: `SKILL.md` metadata plus script entrypoints under `scripts/`
- [x] Implement shared discovery/loader code for parsing skill command metadata, validating names, resolving entrypoints, and reading the skill body
- [x] Enforce static discovery: tool schemas, descriptions, pack membership, and permission hints must load without executing user JS
- [x] Reject collisions with built-in tools, other projected skill command tools, and reserved meta-tool names

### 9.24.2 Manifest schema and runtime contract

- [x] Define manifest fields such as `name`, `description`, `input_schema`, optional `output_schema`, `runtime`, `entrypoint`, `tags`, `sandbox`, `permission_profile`, `timeout_ms`, `packs`, and UX hints like `read_only` / `destructive`
- [x] Restrict the initial runtime set to `runtime = quickjs`; keep the manifest shape extensible enough for a later WASM backend
- [x] Disallow `sandbox = none` for exposed skill command tools so resource limits remain active
- [x] Define the JS entrypoint contract: `main(input, ctx)` returns JSON-serializable output or `{ result, text }`
- [x] Validate tool input against `input_schema` before execution and validate returned `result` against `output_schema` after execution when present

### 9.24.3 Shared tool registry and CLI surface

- [x] Introduce an internal tool registry abstraction that can hold both built-in tools and projected skill command definitions without duplicating schema/export logic
- [x] Add `vulcan tool list`, `vulcan tool show <name>`, and `vulcan tool run <name> --input-json ...`
- [x] Keep skills as the single authoring model while allowing `vulcan tool init` and `vulcan tool lint` as tool-oriented shortcuts over skill command metadata
- [x] Ensure `tool show` exposes parsed metadata plus the Markdown body so humans and agents can read usage notes without opening files manually
- [x] Make `vulcan describe --format openai-tools|mcp|json-schema` include visible skill command tools from the shared registry
- [x] Keep CLI JSON output stable and machine-readable; no ad hoc stdout parsing from the JS script body

### 9.24.4 Internal JS API integration

- [x] Add a `tools` namespace to the JS runtime with `tools.list()`, `tools.get(name)`, and `tools.call(name, input, opts?)`
- [x] Add custom-tool authoring helpers in the JS runtime: `tool.input/result/progress`, `tools.callChecked`, `vulcan.permissions`, query/search builders, note property helpers, daily path resolution, text diffs, and `vault.plan()` mutation plans
- [x] Make the same registry available to skill command tools, general `vulcan run` scripts, and future assistant-internal JS helpers
- [x] Add recursion/cycle protection and a clear maximum call depth for tool-to-tool composition
- [x] Ensure nested calls preserve the effective permission ceiling rather than recomputing broader access

### 9.24.5 Trust, permissions, and host execution

- [x] Require a trusted vault for skill command tool execution, matching plugin execution rules
- [x] Define the effective authority as the intersection of the active caller profile, the tool's optional `permission_profile`, the declared sandbox ceiling, and normal Vulcan path/network/git/config/execute checks
- [x] Keep `read_only` / `destructive` manifest fields as annotations only; authorization continues to come from the permission layer
- [x] Add `host.exec(argv, opts?)` behind `execute` permission and `host.shell(command, opts?)` behind `shell` permission
- [x] Prefer `host.exec()` in all docs/examples and keep `host.shell()` explicitly higher-risk
- [x] Add tests for "tool visible but not callable" cases: untrusted vaults, missing permission profile, denied write/network/execute/shell, and pack-enabled-but-profile-denied combinations

### 9.24.6 MCP exposure, resources, and pack integration

- [x] Expose skill command tools as first-class MCP tools from the same live registry used by built-ins; do not add a generic `run_custom_tool` fallback
- [x] Add a dedicated pack such as `custom` for user-defined tools, with optional additional pack membership from the manifest once validated against canonical pack names
- [x] Add tool documentation resources such as `vulcan://assistant/tools/index` and `vulcan://assistant/tools/{name}`
- [x] Emit `notifications/tools/list_changed` and `notifications/resources/list_changed` when visible skill command metadata changes
- [x] Keep `describe --format mcp` and live MCP exposure on the same registry so exported schemas and live tool lists stay in sync

### 9.24.7 Skills, scaffolding, and authoring guidance

- [x] Document the recommended split: skills teach workflows; skill command tools perform callable request/response work; plugins react to events
- [x] Ship integrated help topics for `tool`, `skill`, `skill commands`, `js.tools`, `js.host`, and the plugin/tool/skill comparison surface
- [x] Add in-repo docs that compare scripts, skills, tools, and plugins with concrete examples instead of only field-by-field schema reference
- [x] Update bundled/default skill guidance so reusable executable behavior is declared as exposed skill commands when cross-surface discoverability matters
- [x] Extend `vulcan init --agent-files` / `vulcan agent install` to optionally write an example exposed skill command template
- [x] Add `vulcan tool init`, `vulcan tool lint`, `tool lint --fix`, mutation dry-run linting, runnable examples, fixture-file examples, JSON mismatch diffs, and `vulcan tool test` so authors can scaffold and verify custom tools without learning every `SKILL.md` field first
- [x] Add help topics and authoring docs that explain manifest fields, permission ceilings, return envelopes, and host execution risks

### 9.24.8 Testing and rollout

- [x] Add unit tests for manifest parsing, name collision detection, schema validation, permission intersection, and trust gating
- [x] Add integration tests with fixture vault tools covering CLI `tool run`, `describe --format openai-tools`, `describe --format mcp`, MCP live exposure, and JS `tools.call()`
- [x] Add regression tests for invalid tool manifests, missing entrypoints, failing scripts, output-schema mismatches, and recursive tool-call loops
- [x] Add end-to-end tests for `host.exec()` / `host.shell()` permission enforcement and timeout/output capture behavior
- [x] Roll out with QuickJS only; treat WASM and finer-grained command allowlists as follow-up work once the registry contract is stable

---

## Phase 9.25: Link-graph community detection

**Goal:** Find dense topic clusters within the wikilink graph using topological community detection (Louvain/Leiden), without requiring embeddings. Builds directly on the existing `GraphAdjacency` and `graph components` infrastructure. This unlocks "orphan near cluster X → suggest bridge links" and "clusters A and B share tags but have zero cross-links → suggest hub note" workflows.

**Depends on:** `graph components` (existing), `GraphAdjacency.undirected()` (existing), vault permissions (9.19.13)

**Test fixtures:** Extend `tests/fixtures/vaults/basic/` or create a dedicated `graph-communities/` vault with:
- A densely linked subgraph forming an obvious cluster (e.g., 5-8 notes on one topic with inter-links)
- Several orphaned notes (zero links) and near-orphan notes (1-2 links, not yet connected to the cluster)
- Two disconnected subgraphs with overlapping tags but no cross-links
- Notes with mixed link directions (some bidirectional, some one-way) to test undirected conversion

### 9.25.1 Louvain algorithm integration

Louvain performs as well as Leiden on typical wikilink graphs while being simpler to implement in Rust. Use iterative modularity maximization: compute modularity gain for moving each node to a neighboring community, repeat until convergence, then aggregate communities into super-nodes for a second pass.

- [x] Implement deterministic community detection in `vulcan-core/src/graph.rs` on `GraphAdjacency.undirected()`, returning stable per-run community IDs when persisted to the `graph_clusters` table
- [x] Re-use the same BFS-based connected-component infrastructure (`build_graph_components_report`) as the fallback for graphs with <2 edges per node on average
- [x] For large graphs, partition into sub-graphs of ≤1000 nodes via connected-component splitting before running Louvain; this avoids unnecessary super-node aggregation passes while preserving correctness on sparse vault graphs
- [x] Unit tests: known community structure (two cliques bridged by a single edge), empty graph, single-node graph, fully-disconnected graph
- [x] Benchmark: <500ms for a 500-node, 2000-edge graph on a warm cache

### 9.25.2 Community summary and labeling

Produce human-readable community descriptions for CLI and MCP surfaces.

- [x] Compute per-community stats: size, cohesion (edge density ratio), top-3 most-connected internal notes, notes that link to other communities (boundary notes), inter-community edge counts
- [x] Generate auto-labels from the top 2-3 most frequent shared tags, falling back to the highest-degree node label
- [x] Persist community assignments in the SQLite cache (`vector_clusters` table pattern, but rename or add a `graph_clusters` table keyed by document path rather than chunk id)

### 9.25.3 CLI and MCP surfaces

- [x] `vulcan graph communities [--limit N]` — list communities sorted by size, with member count, cohesion, top nodes
- [x] `vulcan graph communities --community C` — show detail for one community: full member list, boundary notes (linking to other communities), cross-community edges
- [x] `vulcan graph communities --orphans` — list orphaned notes (no incoming or outgoing links) with their closest community by tag overlap and shortest-path distance if any non-zero path exists
- [x] `vulcan graph communities --bridges` — list boundary notes (notes connecting communities), ranked by betweenness
- [x] Add `graph_communities` MCP tool to the notes-read pack (read-only, no mutation)
- [x] JSON output for all CLI surfaces with `--output json`
- [x] `--dry-run` for community computation (report without persisting to cache)

### 9.25.4 Permission filtering

- [x] Filter community members to only visible documents per the active permission profile
- [x] Recompute community size and cohesion after permission filtering
- [x] Exclude communities with <2 visible members from output

### 9.25.5 Integration testing

- [x] Add integration tests with the `graph-communities` fixture vault covering all CLI surfaces
- [x] Test permission filtering on a mixed-visibility community (some docs hidden)
- [x] Test idempotency: clustering twice on the same graph produces identical community IDs
- [x] Test incremental update: adding a new linked note to a community doesn't reshuffle unrelated communities

### 9.25.6 Skill and AGENTS.md update

- [x] Add `graph_communities` guidance to the `graph-exploration` skill in `docs/assistant/skills/graph-exploration.md` (MCP tool exposure remains tracked in 9.25.3)
- [x] Add example move: "Find which topic cluster an orphaned note belongs to, then suggest a bridge link."
- [x] Update the `graph-exploration` skill’s Recommended Flow to include community detection when the task is about understanding vault topology at scale.

## Phase 9.26: Composite link suggestion ranking

**Goal:** A `vulcan suggest links` command that synthesizes multiple existing signals into a single ranked suggestion queue with user feedback tracking (accept/reject). Composes text mentions, embedding similarity, graph distance, and tag overlap — all of which already exist as independent query surfaces.

**Depends on:** `suggest mentions` (existing, `vulcan-core/src/suggestions.rs`), `query_related_notes` (existing, `vulcan-core/src/vector.rs:852`), `GraphAdjacency` (existing), tag queries (existing), community detection (9.25)

**Test fixture:** Extend the `suggestions` fixture vault with additional notes that exercise composite scoring:
- A note with a text mention AND high embedding similarity AND a shared tag (should score highest)
- A note with only embedding similarity but no text mention (should score lower, but still appear)
- A note at 2-hop distance in the link graph with no other signals (should appear at the bottom)

### 9.26.1 Suggestion scoring model

- [x] Define a `LinkSuggestion` struct: `source_path`, `target_path`, composite `score` (0.0–1.15, capped at 1.0 for display), `signals` (breakdown of contributing factors), `status` (pending/accepted/rejected), `created_at`, `accepted_at`
- [x] Composite score formula: `0.4 × embedding_cosine + 0.3 × graph_proximity_bonus + 0.2 × text_mention_bonus + 0.1 × tag_overlap_bonus`
  - `embedding_cosine`: raw cosine similarity from `query_related_notes` (typically [0, 1]), multiplied by 0.4
  - `graph_proximity_bonus`: `0.3 / hop_distance` if the notes are within graph reach with no direct link, 0 if directly linked (cap at 0.3 for 1-hop)
  - `text_mention_bonus`: 0.2 if a text mention exists (from `suggest_mentions`), 0 otherwise
  - `tag_overlap_bonus`: Jaccard similarity of the two notes' tag sets, multiplied by 0.1
- [x] Apply a 1.15× multiplier to the total score for cross-community note pairs (notes whose closest communities differ)

### 9.26.2 Suggestion persistence and feedback

- [x] Create a `link_suggestions` table in the SQLite cache: `id` (ULID), `source_document_id`, `target_document_id`, `score` (REAL), `signals` (JSON text), `status` (TEXT, default 'pending'), `created_at`, `accepted_at` (nullable), `rejected_at` (nullable)
- [x] On accepted suggestions: create a real link in the `links` table with `confidence = 'INFERRED'` and `confidence_score = score`
- [x] On rejected suggestions: set status to 'rejected' and deprioritize the same (source, target) pair from future suggestion runs (halve the score)
- [x] `vulcan suggest links --accept ID` and `vulcan suggest links --reject ID` for explicit user feedback
- [x] `vulcan suggest links --accepted` to list accepted suggestions that were auto-converted to links

### 9.26.3 CLI and MCP surfaces

- [x] `vulcan suggest links [--note PATH] [--limit N] [--min-score S]` — ranked suggestion queue, scoped to one note or vault-wide
- [x] `vulcan suggest links --status pending|accepted|rejected` — filter by feedback state
- [x] `vulcan suggest links --apply [--dry-run]` — apply all pending suggestions above a configurable min-score threshold (default 0.6); accepts cache-backed inferred links without filesystem mutation
- [x] Add `suggest_links` MCP tool to the notes-read pack (reading suggestions) and notes-write pack (accepting/rejecting)
- [x] JSON output: each suggestion includes score breakdown (`embedding_score`, `graph_score`, `mention_score`, `tag_score`, `cross_community_bonus`)

### 9.26.4 Integration testing

- [x] Full pipeline test: scan → compute suggestions → verify ranking → accept one → verify link appears in graph → reject another → verify it's deprioritized
- [x] Test that cross-community suggestions get the bonus multiplier
- [x] Test that directly-linked note pairs are excluded (no self-suggestion of existing links, including INFERRED links from previously accepted suggestions)
- [x] Test idempotency: running suggestions twice produces identical scores on unchanged data

### 9.26.5 Skill and AGENTS.md update

- [x] Add `suggest_links` guidance to the `graph-exploration` skill in `docs/assistant/skills/graph-exploration.md` (MCP tool exposure remains tracked in 9.26.3)
- [x] Add example move: "Discover and review a ranked list of suggested connections for an orphan note, then accept the ones that make sense."
- [x] Update the `graph-exploration` skill’s Recommended Flow to include `suggest links` as a way to find connections when a note feels isolated in the graph.
- [x] If a new `link-curation` skill is created, add it to the AGENTS.md template so new vaults ship with it.

## Phase 9.27: Confidence tagging on graph edges

**Goal:** Add `confidence` (EXTRACTED/INFERRED/AMBIGUOUS) and `confidence_score` (0.0-1.0) to every edge in the link graph. This is a schema change that wires through the graph walking API, CLI surfaces, and MCP tools so consumers always know what was found vs. inferred from the source vault.

**Depends on:** Links schema (existing), `graph export` (existing), `graph path` (existing), `graph hubs` (existing), MCP tool catalog (existing), link suggestions (9.26)

**Schema migration:** Additive migration (no rebuild required). Existing links default to `confidence = 'EXTRACTED', confidence_score = 1.0`.

### 9.27.1 Schema and migration

- [x] Add columns to `links` table: `confidence` TEXT NOT NULL DEFAULT 'EXTRACTED', `confidence_score` REAL NOT NULL DEFAULT 1.0
- [x] Add check constraint: `confidence IN ('EXTRACTED', 'INFERRED', 'AMBIGUOUS')`
- [x] Add check constraint: `confidence_score BETWEEN 0.0 AND 1.0`
- [x] Bump `user_version` pragma and add migration step
- [x] Write migration idempotency test: reindex twice, all links stay `confidence = 'EXTRACTED', confidence_score = 1.0`

### 9.27.2 Confidence in graph queries

- [x] Augment `GraphAdjacency` to carry per-edge confidence metadata (prefer a parallel `HashMap<(String, String), (String, f64)>` lookup map rather than changing the `edges: Vec<(String, String)>` representation, to minimize disruption to existing callers)
- [x] `graph path` output: annotate each hop with its confidence label and score
- [x] `graph hubs` output: break down hub degree by confidence tier (N EXTRACTED edges, M INFERRED edges)
- [x] `graph export`: include `confidence` and `confidence_score` in the edge output
- [x] `graph stats`: add confidence breakdown (total EXTRACTED/INFERRED/AMBIGUOUS edges) to the analytics report

### 9.27.3 Accepted suggestions become inferred edges

- [x] When a user accepts a `link_suggestions` entry (9.26.2), insert the corresponding row in `links` with `confidence = 'INFERRED', confidence_score = <suggestion score>`
- [x] Accepted edges participate fully in graph queries (path, hubs, communities, components) but are visually distinct in output
- [x] Recomputing suggestions for an already-accepted pair returns a note that a link exists (inferred), not a new suggestion

### 9.27.4 CLI and MCP surfaces

- [x] All graph subcommands (`graph path`, `graph hubs`, `graph export`, `graph stats`) include confidence in their JSON output
- [x] All MCP tools that return graph data (`note_info`, `status`, and any dedicated graph tools added in 9.25/9.26) include confidence fields in structured content
- [x] `vulcan graph stats` adds a "Confidence" section to the human-readable output: `Edges: 1423 (1310 EXTRACTED, 98 INFERRED, 15 AMBIGUOUS)`

### 9.27.5 Integration testing

- [x] Test that existing links survive reindex with confidence = EXTRACTED
- [x] Test that accepted suggestions produce confidence = INFERRED edges
- [x] Test that graph path traversal includes confidence on each hop
- [x] Test that MCP `note_info` returns confidence for resolved backlinks once 9.27 data is on the graph
- [x] Test schema downgrade safety (older cache version → correct error, not silent corruption)

### 9.27.6 Skill and AGENTS.md update

No skill changes required. Confidence tagging is internal metadata that enriches existing CLI output (`vulcan graph hubs`, `vulcan graph path`, `vulcan graph export`) and MCP tool responses (`graph_hubs`, `graph_path`, `graph_export` in the `graph-exploration` skill) without changing the tool surface. The skill's example moves and guardrails remain valid because these surfaces automatically include confidence context in their JSON output once this phase ships.

## Phase 9.28: Agent Skills-compatible skill commands

**Goal:** Align Vulcan's executable assistant assets with the Agent Skills package format while preserving the shared registry, permission profiles, MCP exposure, and JS runtime.

- [x] Discover Agent Skills-compatible directories from `.agents/skills/<name>/SKILL.md` and configured skill roots.
- [x] Parse official skill frontmatter fields: `name`, `description`, `license`, `compatibility`, `allowed-tools`, and `metadata`.
- [x] Parse `metadata.vulcan.commands` as Vulcan-specific command declarations.
- [x] Validate command IDs, script paths, input/output schemas, sandbox values, permission-profile references, pack names, and exposure flags
- [x] Add `vulcan skill list|show|commands|run|validate|init`
- [x] Project trusted skill commands into the shared registry used by CLI, `describe`, MCP, and internal JS APIs.
- [x] Expose projected skill commands as first-class MCP tools and add MCP resources for skill index, skill content, command metadata, and resource listings.
- [x] Update `vulcan index init --agent-files --example-tool` and `vulcan agent install --example-tool` to scaffold an Agent Skills-compatible example with a command under `scripts/`.
- [x] Keep legacy standalone custom-tool loading, if implemented, as a compatibility path or migrate it into skill-command scaffolding.
- [x] Add docs and integrated help topics for `skill`, `skill-command`, `js.skills`, and the skills-vs-commands-vs-plugins decision model.

---

## Phase 9.30: One-way Outline publishing

**Goal:** Publish a query-selected, folder-note-aware Markdown hierarchy one-way into Outline while keeping the vault canonical and synchronization identity outside the rebuildable cache.

- [x] Add shared deterministic folder-note hierarchy planning driven by the repository's explicit folder-note convention, including nested parents and collision diagnostics.
- [x] Add `vulcan export outline-zip` with transformed/resolved links, deterministic attachment paths, mutation-free dry runs, structured reports, and fail-closed validation.
- [x] Allow Outline ZIP export to reuse publication profiles without API credentials, with exclusive direct/profile configuration and projection-hash parity across ZIP and API publication.
- [x] Standardize omitted queries across direct and profile-based exports as the full-vault `from notes` selection.
- [x] Add non-secret Outline profiles in shared config with device-local endpoint/token bindings.
- [x] Add typed Outline collection list/show/create/update/archive/restore utilities, explicit existing-UUID binding, and opt-in publish-time collection provisioning that persists the returned UUID with dry-run and durable-state safeguards.
- [x] Add a bounded-retry, paginated Outline API client and mock transport tests.
- [x] Add durable, locked, atomically-written source-to-Outline mapping state outside `cache.db`.
- [x] Scope generated Outline document identities by profile and collection, verify create responses by target-collection readback before finalizing state, and automatically repair provable legacy cross-collection/deleted-ID collisions without weakening missing-document conflicts.
- [x] Add create/update/move/archive reconciliation, remote-drift conflicts, attachment uploads (including authenticated self-hosted local-storage targets without leaking credentials to external object stores), idempotency, and mutation-free publish dry runs.
- [x] Add a shared bidirectional Obsidian/Outline Markdown compatibility layer: outbound frontmatter stripping, callout fence conversion, opt-in TOC removal, API document-link translation to durable remote IDs, and reusable inbound callout/link reversal for Phase 15 pull routes.
- [x] Complete CLI/reference documentation and full workspace verification.
- [x] Degrade missing selected folder notes to deduplicated warnings and generate deterministic export-only Outline hierarchy placeholders.
- [x] Propagate generated folder-placeholder diagnostics through Outline API publish reports and human output.
- [x] Add an explicit strict/plain-text policy for unsupported Obsidian block-reference targets, with export-only rewrites, located structured diagnostics, and aggregated human output.
- [x] Add an explicit strict/plain-text policy for links outside query-selected partial Outline exports, with export-only rewrites, located structured diagnostics, and aggregated human output.
- [x] Add an `annotated-text` fallback for unsupported block references and excluded targets that preserves the visible label, authored destination, and embed intent without publishing broken links.
- [x] Add a trusted, pure, resource-bounded `transform_link(link)` callback shared by Outline ZIP and API publishing, with typed context/output, deterministic guards, located failures, and script path/hash provenance while keeping general audience filters declarative.
- [x] Add explicit `--overwrite-conflicts` reconciliation, honor Outline `Retry-After` rate-limit delays, and eliminate redundant per-document fetches and unchanged update calls.
- [x] Add phase/item Outline publish progress, structured base/local/remote conflict evidence, and selective `--overwrite-conflict <source-path>` authorization.
- [x] Keep default Outline publication output compact with phase totals, in-place counters (bounded checkpoints when redirected), and an aggregate action summary; reserve per-path progress and routine action listings for `--verbose` while always showing conflicts.
- [x] Track the submitted local projection separately from Outline's observed, potentially normalized Markdown so repeated publications stay idempotent without weakening remote-drift detection.
- [x] Index publication mappings, actions, and remote render destinations so large Outline publications avoid repeated whole-plan scans and all-pairs link rendering.
- [x] Add optional terminal-guided Outline push conflict review with per-document approval, an approve-remaining shortcut, a fresh pre-apply reconciliation, and fail-closed cancellation.

**Current boundary and future placement:** The implemented `publish outline` command remains strictly one-way. Phase 15 now plans a separate, explicitly scoped Outline pull route with its own local destination, authority, pagination, attachment, deletion, and conflict policies. Do not evolve this publisher into implicit bidirectional synchronization or use the separate Outline-to-Git backup/audit trail as publisher input.

---

## Phase 9.31: Configurable folder notes and vault structure normalization

**Goal:** Give every Vulcan workflow one deterministic repository-level folder-note convention and provide safe tools for normalizing existing vault layouts.

- [x] Add shared `inside` / `outside` placement plus an exact filename stem/template with `{{folder_name}}` substitution; never auto-detect during runtime.
- [x] Support `index.md`, `README.md` / `readme.md`, same-name notes inside a folder, and same-name notes beside a folder.
- [x] Import the Obsidian Folder Notes plugin convention explicitly during init/config import without making plugin state a runtime authority.
- [x] Use the shared convention for HTML runtime link routing, static-site navigation/routes, and Outline ZIP/API hierarchy planning.
- [x] Add a dry-run/JSON-capable `refactor folder-notes` conversion workflow with overwrite, case-collision, unsafe-config, and deterministic-planning checks.
- [x] Recalculate resolved outbound relative links as well as inbound links when structural conversions move a note.
- [x] Document configuration, conversion, retry behavior, supported layouts, and limitations; cover the shared semantics and CLI with focused tests.

---

## Phase 9.35: Semantic document decomposition and wiki-tree materialization

**Goal:** Turn a large heading-structured Markdown document, such as a PDF-derived rulebook with a companion asset directory, into a deterministic tree of ordinary vault notes so chapters and concepts become independently readable, linkable, searchable, and publishable.

**Boundary:** This is an explicit source-vault refactor: the materialized Markdown files become canonical vault content. Keep publication-only section projection as a separate future export capability. The initial workflow preserves referenced assets in place and rewrites destinations; it does not duplicate or reorganize asset files implicitly.

**Dependencies:** Reuse the canonical parser and semantic note outline, resolved link identities and move-safe rewrite machinery, first-class attachment graph, shared application workflow layer, and configured folder-note convention. Keep reusable planning and mutation logic out of `vulcan-cli`.

- [x] Inventory representative structures and conversion artifacts from the local-only rulebook corpus without recording source identities or checking private artifacts into the repository; keep automated fixtures synthetic and minimal.
- [x] Add a deterministic decomposition planner for a configurable heading-level range, including preamble placement, parent/child hierarchy, filename normalization, duplicate-heading disambiguation, configured folder notes, and exact source-span coverage checks.
- [x] Define explicit frontmatter, heading, footnote/reference-definition, block-reference, and generated-navigation policies; surface unsupported or ambiguous constructs as located diagnostics instead of silently dropping content.
- [x] Plan link rewrites for inbound links to the source document and its heading/block/explicit-HTML-anchor subpaths, cross-section links within the source, self-fragment links inside emitted notes, and relative links whose source directory changes while preserving authored Markdown/wikilink/embed style and CommonMark-safe destinations.
- [x] Preserve referenced assets in their current canonical locations by default, rewrite relative destinations for every emitted note, and report missing, ambiguous, or unsafe asset references before mutation.
- [x] Add a reusable locked application workflow with complete preflight validation, mutation-free dry runs, collision and stale-cache checks, deterministic action ordering, and a structured report suitable for CLI, MCP, and future daemon surfaces.
- [x] Add `vulcan refactor split-note <source>` with heading-range, destination, source-retention, navigation, explicit missing-fragment preservation, `--dry-run`, and `--output json` controls; use the configured folder-note convention and conservative root-only frontmatter policy by default.
- [x] Review the local representative corpus for per-invocation folder-note and frontmatter-policy needs; retain the repository convention and root-only default because no recurring override requirement was demonstrated.
- [x] Reindex after successful application and verify that the source content is fully represented, planned links resolve as expected, and repeated dry runs are deterministic.
- [x] Add core planner tests, application mutation/rollback tests, CLI JSON and human-output integration tests, attachment-heavy synthetic fixtures, duplicate/collision cases, and link-rewrite regressions.
- [x] Run representative rulebook acceptance locally without committing source identities, corpus details, or private artifacts.
- [x] Update CLI/reference documentation and the bundled note/refactor agent skill so external agents discover dry-run-first decomposition and its asset/link safety policies.

---

## Phase 9.36: Evidence-preserving Markdown artifacts and wiki import

**Goal:** Define and consume an extractor- and source-format-neutral Markdown Artifact Format (`.mdaf`) that carries one primary Markdown document, assets, normalized source mappings, alternative hierarchy evidence, complete native extractor output, and reproducible conversion provenance into Vulcan's canonical wiki-tree workflow.

**Boundary:** Vulcan specifies, validates, inspects, and imports MDAF artifacts. It does not run extraction or branch on source media type, Marker, Mistral, DeepSeek, Docling, or any other extractor. Producer-specific responses remain lossless declared renditions or namespaced extensions; only the minimal Markdown/source-selector/outline contract is normalized. BlobForge production and legacy-artifact repackaging follow after this Vulcan-first slice, and `pdf-to-wiki` receives no further architectural investment.

- [x] Publish the MDAF v1 logical layout, JSON Schemas, canonical algorithm-tagged BLAKE3 digests and logical identity, optional alternate source digests, archive-safety rules, provenance activity graph, immutable-derivative rules, and synthetic examples.
- [x] Add extractor-neutral core models and safe directory/ZIP readers with declared-member hashing, logical artifact identity, bounded expansion, opaque rendition/extension preservation, and semantic validation.
- [x] Validate primary Markdown bindings, UTF-8 source spans, arbitrary source media types, composable interval/spatial/grid/text/fragment/extension selectors, conservative source references, aligned alternative outlines, activity/member provenance, source digests, and derivative lineage without interpreting producer-native data.
- [x] Add an atomic application import workflow that requires an explicit destination, copies declared assets, materializes either Markdown-heading or explicitly selected aligned-outline hierarchy, propagates `vulcan.source` frontmatter, rewrites uniquely resolvable source references, refreshes the cache, and rolls back on failure.
- [x] Add `vulcan artifact inspect`, `validate`, and `import` with dry-run, JSON output, decomposition controls, auto-commit integration, deterministic diagnostics, documentation, and bundled agent guidance.
- [x] Cover directory/ZIP parity, multiple synthetic producer shapes, native metadata retention, multi-tool/version provenance, derivatives, unsafe archives, assets, hierarchy selection, reference resolution, collisions, rollback, reindexing, and CLI snapshots without committing real source documents or corpus identities.

---

## Phase 9.37: Archival document extraction and OCR investigation

**Goal:** Determine the safe, supportable way for Vulcan to preserve, inspect, index, search, and cite archival documents such as PDF/A files, born-digital PDFs, scanned PDFs, and standalone images without weakening the vault/cache boundary or turning the Markdown parser into a document-conversion engine.

**Status and boundary:** Investigation and decision work only. This phase does not block Phase 10 and does not authorize a new embedded PDF/OCR stack. Preserve source attachment bytes as canonical, keep all extracted text and validation results rebuildable, and retain MDAF as the evidence-preserving interchange path for rich conversion. Any lightweight direct-indexing path must remain extractor-neutral, explicit, bounded, and usable without the daemon.

- [ ] Audit the existing attachment extraction path end to end: discovery, extension dispatch, trust handling, subprocess execution, fixed timeout, parallel scan behavior, output limits, chunking, FTS/vector ingestion, cache invalidation, failure propagation, doctor visibility, and export/publication behavior.
- [ ] Build a synthetic, redistributable evaluation corpus covering PDF/A-1, PDF/A-2, and PDF/A-3 where feasible; ordinary born-digital PDFs; image-only and mixed-content PDFs; encrypted, malformed, oversized, rotated, multi-column, and multilingual documents; embedded files; and representative raster image formats. Record which properties are fixtures versus externally validated claims.
- [ ] Define a precise capability matrix that distinguishes byte-preserving attachment storage, PDF/A identification, PDF/A conformance validation, embedded-text extraction, OCR, layout/table recovery, embedded-asset handling, page-level citation, and conversion into canonical Markdown. Do not advertise "PDF/A support" as one undifferentiated capability.
- [ ] Reconcile the two intended workflows in the design and documentation: lightweight best-effort text derivation for search/vector indexing versus evidence-preserving external production and MDAF import for coordinates, hierarchy, assets, alternative renditions, and reproducible provenance.
- [ ] Evaluate representative external tools and integration shapes, including text-layer extraction, OCR fallback for scanned PDFs, PDF/A validation, and richer document conversion. Compare accuracy, page/coordinate fidelity, confidence/language metadata, licensing, packaging, platform availability, determinism, resource use, hostile-input posture, and ability to emit a stable extractor-neutral contract; do not add producer-specific branches to Vulcan core.
- [ ] Specify rebuildable extraction state and diagnostics: source digest, extractor identity and explicit cache key/version, configuration digest, timestamps, outcome (`success`, `no_text`, `tool_missing`, `encrypted`, `timed_out`, `output_too_large`, or `failed`), warnings, page coverage, and provenance links where available. Decide whether and when stale prior text may remain queryable after a failed refresh, and make that state visible.
- [ ] Design cache invalidation rules that react to source changes, extraction configuration, tool/model versions, OCR language packs, normalization/chunking changes, and selected output contract without requiring source attachment mutation.
- [ ] Decide scan and job semantics for expensive or unreliable extraction: per-file diagnostic versus strict failure, configurable timeout/output/concurrency budgets, cancellation, retries, incremental refresh, daemon job placement, watcher behavior, and a fully functional direct-CLI path. Ordinary scans must not become unpredictably networked or unbounded.
- [ ] Threat-model hostile PDFs/images and executable extractor configuration. Cover untrusted vaults, shared versus device-local configuration, command allowlisting or named profiles, subprocess isolation, temporary files, path/argument handling, decompression bombs, parser vulnerabilities, secret/environment exposure, daemon multi-tenant boundaries, and diagnostic redaction.
- [ ] Document the privacy boundary for extracted text, especially when vector indexing uses a remote embedding provider, and determine which permission, consent, path-exclusion, and local-only controls must apply consistently to Markdown and attachment-derived chunks.
- [ ] Prototype only what is necessary to answer unresolved questions. Measure extraction quality, throughput, memory, process fan-out, cache size, retry behavior, and search/citation usefulness on the synthetic corpus; keep prototypes out of production paths unless promoted through a separately reviewed implementation item with tests.
- [ ] Produce a decision record and follow-on implementation plan with explicit supported/unsupported claims, configuration and CLI/API contracts, schema/migration effects, diagnostics, dependency/licensing choices, test matrix, documentation and bundled-skill impact, rollout compatibility, and acceptance gates. Place resulting work in Phase 7 hardening, Phase 10 jobs, MDAF tooling, or a separate capability slice according to the chosen boundary rather than assuming it all belongs in one module.

---

## Phase 9.38: Portable document and wiki exchange packages

**Goal:** Give Vulcan interoperable, safely inspectable package boundaries for one editable Markdown document with assets and for a complete multi-document Markdown wiki snapshot without introducing a second canonical vault backend.

**Boundary:** TextBundle/TextPack is a compatibility adapter for one mutable text document and its assets. MDAF remains the immutable evidence-bearing extraction format. Markdown Wiki Packages exchange immutable multi-document snapshots as either directories or ZIP files. Ordinary materialized Markdown remains canonical after import; neither a ZIP nor SQLite becomes an implicitly writable or synchronized vault.

- [x] Add safe TextBundle v1/v2 and TextPack readers that preserve unknown application metadata, validate one `text.*` member and `assets/`, reject unsafe containers, and expose deterministic inspection diagnostics.
- [x] Add dry-run-capable TextBundle/TextPack import into an explicit new vault destination, collision-safe asset materialization, standard asset-relative link preservation, cache refresh, rollback, JSON output, and auto-commit integration.
- [x] Add TextBundle/TextPack export for one canonical Markdown note and its referenced local assets while preserving the standardized metadata boundary.
- [x] Specify the Markdown Wiki Package v1 manifest, BLAKE3 logical identity, `.wikibundle` directory and `.wikipack` ZIP serializations, path/archive safety limits, immutable lineage, synthetic fixtures, and JSON Schema.
- [x] Add wiki-package inspect, validate, export, and dry-run/import workflows that preserve Markdown and asset bytes, exclude cache/device/credential/Git state, require a new destination, rebuild the cache, and roll back partial writes.
- [x] Cover directory/ZIP parity, TextPack compatibility metadata, asset/reference rewrites, unknown metadata preservation, traversal/symlink/duplicate/collision failures, deterministic identities, rollback, CLI output, and installed agent guidance.
- [x] Keep SQLite as a documented alternative serialization of the same wiki-package model. Do not implement a writable SQLite vault until a separate storage, revision, conflict, and interoperability design is approved.

---

## Capability tracks and connector appendices

The MDB and OBS tracks preserve candidate implementation research and acceptance criteria without extending the Phase 9 completion gate. The SB appendix is a promoted connector-specific plan referenced by Phases 12 and 15. None forms a serial queue: schedule bounded slices through the numbered phase that owns their daemon, sync, UI, or runtime infrastructure.

### MDB: mdbase typed Markdown collection interoperability (formerly 9.32)

**Goal:** Let Vulcan detect, validate, query, and safely mutate [mdbase v0.3](https://mdbase.dev/spec/) collections without replacing Vulcan's Obsidian-compatible semantics, canonical query model, permission system, or rebuildable cache architecture.

**Compatibility boundary:** mdbase behavior is opt-in and collection-scoped. A valid root-level `mdbase.yaml` activates the mdbase adapter for explicit mdbase surfaces; ordinary Vulcan commands and vaults without that marker keep their existing semantics. `mdbase.yaml`, `_types/`, `_contracts/`, and record Markdown remain authoritative. `.vulcan/cache.db` only stores rebuildable derived state, and `.vulcan/config*.toml` remains Vulcan runtime/application configuration rather than a second source for portable mdbase semantics.

**Initial conformance target:** implement and verify the mdbase `core_read` and `collection_semantics` profiles first. Do not claim `cel`, `cel_match`, `cel_query`, `links`, `core_write`, `lifecycle`, or `watch` until every required behavior has focused tests and the corresponding upstream conformance fixtures pass. The optional runtime, workflow, type-pack installation, and event/action interoperability profiles are explicitly deferred.

**Delivery placement:** MDB.1–MDB.8 are independently promotable local/core capability slices and do not block Phase 10. Watch/daemon integration in MDB.9 follows Phase 10; executable runtime/workflow interoperability remains a Phase 15-era candidate.

#### MDB.1 Specification pinning, collection discovery, and configuration

- [x] Pin one exact supported mdbase v0.3 specification revision in source/docs, bundle required canonical schemas and fixtures with license/provenance metadata, and make upgrades explicit reviewable changes.
- [x] Add a transport-neutral `vulcan-core::mdbase` collection detector and `mdbase.yaml` loader with v0.3 version checks, documented defaults, unknown-key warnings, safe relative control-folder paths, record-extension validation, explicit type-key configuration, validation level, and durable IANA timezone validation.
- [x] Model configured `_types/` and `_contracts/` folders, `.mdbase/`, `mdbase.lock.yaml`, configured exclusions, and nested `mdbase.yaml` roots as mdbase control/discovery boundaries without hiding those Markdown files from ordinary Obsidian-oriented Vulcan browsing unless the caller requests the mdbase record set.
- [x] Add an `mdbase` fixture vault covering a minimal collection, customized folders/extensions, malformed YAML, unknown keys, unsupported versions, unsafe paths, invalid timezone identifiers, exclusions, and nested collections.

#### MDB.2 JSON Schema, type registry, and data contracts

- [x] Replace or supplement the small internal tool-schema validator with an MSRV-compatible JSON Schema 2020-12 implementation that covers every mdbase-required keyword, asserted date/time/date-time formats, fragment references, bounded local file references, cycle detection, and canonical `schema_*` diagnostics.
- [x] Load and validate `kind: mdbase.type` control files into a deterministic case-insensitive registry while preserving authored names and reporting conflicts independently of filesystem order.
- [x] Implement explicit type declaration precedence and structured inferred matching (`path_glob`, `fields_present`, and `match.where`) against persisted frontmatter, including multiple matched types and deterministic ordering.
- [x] Implement compatible multi-type composition for schemas, defaults, links, uniqueness, paths, lifecycle declarations, projections, and display metadata; report `type_conflict` before applying conflicted behavior.
- [ ] Load exact-version `mdbase.contract` files, validate `implements` bindings, produce deterministic contract/implementation digests, and expose projected record contract views required by `core_read`.

#### MDB.3 Persisted and effective record model

- [ ] Introduce shared record-domain types that keep exact source, body, persisted frontmatter, effective frontmatter, matched types, revision, file metadata, and diagnostics distinct; never populate mdbase frontmatter from Dataview inline fields.
- [ ] Apply `collection.read_defaults` only to missing effective fields while preserving missing, explicit null, empty string, and empty list as distinct states; validate JSON Schema `required` against persisted frontmatter only.
- [ ] Implement cross-file uniqueness scopes, advisory display metadata, and portable path-pattern validation with deterministic collection-relative forward-slash paths.
- [ ] Cache type membership, effective projections, and validation results as versioned derived data with rebuild and incremental invalidation when config, types, contracts, schemas, or records change.

#### MDB.4 Core read surface and conformance gate

- [ ] Add `vulcan mdbase status|types|contracts|validate|read` over reusable core/app services, with `--output json`, permission filtering, exact source opt-in, canonical diagnostics, and no implicit mutation.
- [ ] Return the canonical mdbase complete-record and operation envelopes from explicit mdbase commands while keeping existing Vulcan JSON contracts backward compatible.
- [ ] Add a spec-fixture adapter and evidence command that runs the upstream v0.3 `core_read` and `collection_semantics` suites against Vulcan.
- [ ] Publish a machine-readable conformance claim only after every required fixture passes on the pinned artifact; report unsupported profiles explicitly instead of approximating them.

#### MDB.5 CEL and canonical mdbase querying

- [ ] Select or implement an MSRV-compatible CEL engine behind a Vulcan-owned adapter; bound source size, AST depth, evaluation work, memory, list iteration, and link traversal.
- [ ] Implement mdbase raw/effective/presence namespaces, reserved bindings, fixed per-operation clock, IANA timezone context, date/duration values, null propagation, and context-specific diagnostics without changing Bases/Dataview expression behavior.
- [ ] Add `match.expr` only after the base CEL profile passes, with raw candidate bindings and compile-time type-file preflight.
- [ ] Extend the internal query representation as needed for named projections, invocation context (`this`), CEL filters/selections, multi-key ordering, grouping, summaries, frontmatter modes, and canonical pagination metadata; keep mdbase as another frontend rather than the product-wide canonical syntax.
- [ ] Add `vulcan mdbase query` and pass the `cel`, `cel_match`, and `cel_query` conformance gates before advertising those profiles.

#### MDB.6 mdbase link semantics

- [ ] Add a scoped mdbase resolver mode for declared frontmatter links and body links: configured-ID lookup, collection/file-relative paths, stable ambiguity behavior, target-type constraints, and `validate_exists` diagnostics.
- [ ] Preserve raw, parsed, and resolved link forms; keep ordinary Vulcan/Obsidian shortest-path and alias behavior unchanged outside explicit mdbase operations.
- [ ] Implement bounded CEL link helpers plus `file.links`, `file.embeds`, and `file.tags`, ignoring code spans/fences consistently with the parser pipeline.
- [ ] Pass the upstream Links profile before claiming it; do not claim Links before its CEL dependency is satisfied.

#### MDB.7 Core write, concurrency, and lifecycle

- [ ] Centralize mdbase create/update/delete/rename/batch orchestration in `vulcan-app`, reusing secure path handling, atomic writes, scan refresh, permission checks, dry-run reports, plugin events, and opt-in git commits.
- [ ] Add opaque content-derived revisions and `if_revision` preconditions; preserve the current file and return `concurrent_modification` on mismatch.
- [ ] Implement the normative draft pipeline: draft type membership, lifecycle, one post-lifecycle membership check, JSON Schema validation, collection validators, atomic persistence, derived-state refresh, then events.
- [ ] Implement `now`, `today`, `uuid`, `ulid`, `slugify`, `copy`, and `literal` lifecycle providers, guarded lifecycle actions after CEL, and deterministic conflict diagnostics across matched types.
- [ ] Preserve unrelated Markdown, link style, aliases, anchors, line endings, and exact supplied source when policy does not require reserialization; update references on rename through the existing rewrite planner.
- [ ] Pass Core Write and Lifecycle suites independently before claiming either profile, and add crash/concurrency regression tests around batch and rename operations.

#### MDB.8 Portable views, Obsidian Bases, and TaskNotes

- [ ] Load canonical `type: view` records as ordinary mdbase records and implement stable named-view discovery, inheritance/merge rules, invocation context, advisory presentation, and headless execution.
- [ ] Adapt existing `.base` discovery/evaluation to the mdbase saved-view source envelope without converting `.base` files or making mdbase CEL the Bases expression language; keep source revisions and full-document writable operations explicit.
- [ ] Evaluate the upstream `obsidian_bases_views` optional feature against Vulcan's existing oracle/snapshot corpus and claim it only when source ordering, formulas, filters, grouping, properties, and diagnostics match.
- [ ] Import TaskNotes `enableMdbaseSpec` and generated collection assets through an explicit preview/apply workflow that preserves unrelated settings and never partially migrates a live collection.

#### MDB.9 Watch, daemon, permissions, and rollout

- [ ] Invalidate derived mdbase state when `mdbase.yaml`, type, contract, external schema, or record files change; publish logical notifications only after read/query state is consistent.
- [ ] Keep collection loading passive: opening a vault, type, contract, provider, or workflow record must never activate executable behavior.
- [ ] Apply existing permission profiles to mdbase read/query/write paths and keep control files, contracts, diagnostics, and exact source subject to normal path/read restrictions.
- [ ] Add help/reference documentation, an mdbase-focused assistant skill, example collections, feature-disabled behavior, and upgrade notes for each newly claimed profile.
- [ ] Re-evaluate upstream stability, MSRV, and crate boundaries before each profile expansion. Do not depend directly on `mdbase-rs` while it would raise Vulcan's MSRV or introduce a second authoritative cache/watcher/mutation engine.

#### Deferred mdbase runtime work

The mdbase event/action interoperability, durable runtime, workflow execution, provider registry, type-pack installation, and migration profiles are not part of the initial MDB track. Revisit them after the Phase 10 daemon and Vulcan's shared plugin/skill-command/permission boundaries are stable. Any later integration must adapt those contracts to the daemon rather than introducing executable behavior into `vulcan-core` or bypassing Vulcan authorization.

---

### OBS: Native vault capabilities with Obsidian compatibility adapters (formerly 9.33)

**Goal:** Promote useful workflows suggested by the remaining project plugins into coherent native Vulcan capabilities, while adding the smallest explicit adapters needed for persisted-format, settings-import, migration, and conformance compatibility. Preserve graceful degradation: a vault authored with these plugins must remain understandable without Obsidian, and Vulcan must not require a plugin to be installed merely to index, operate on, or export its files.

**Depends on:** Phase 1 link/attachment indexing, Phase 2 safe rewrites, Phase 7 diagnostics and asset maintenance, Phase 9.13 QuickAdd compatibility, Phase 9.17 settings import, Phase 9.18 command and permission surfaces, Phase 9.31 configurable folder notes, and the shared publication pipeline from Phase 9.20/9.30.

**Capability and compatibility boundary:** Native domain models, CLI commands, configuration, JSON reports, daemon APIs, MCP tools, and assistant skills use stable Vulcan capability names. Plugin names appear only at adapter selection, settings import, compatibility profiles, provenance, and diagnostics. Implement durable formats and reusable headless operations, not Obsidian editor chrome or incidental plugin limitations. Markdown and ordinary vault files remain canonical; plugin settings are optional read-only migration inputs; SQLite rows and network results remain rebuildable or ephemeral. Mutating commands require deterministic plans, `--dry-run`, structured JSON reports, normal permission checks, atomic writes, link-safe rewrites, optional git commits, and no implicit network access during scan, doctor, export, or ordinary note reads.

**Initial upstream references:** [HedgeSync](https://community.obsidian.md/plugins/hedgesync), [Seafile Sync Improved](https://community.obsidian.md/plugins/seafile-improved), [Broken Links](https://community.obsidian.md/plugins/broken-links), [Waypoint](https://github.com/IdreesInc/Waypoint), [Wikilink Types](https://github.com/penfieldlabs/obsidian-wikilink-types), [VCF Contacts](https://github.com/broekema41/obsidian-vcf-contacts), [QuickAdd](https://quickadd.obsidian.guide/docs/), [Local Images Plus](https://github.com/Sergei-Korneev/obsidian-local-images-plus), [LanguageTool](https://github.com/wrenger/obsidian-languagetool), [Auto Link Title](https://github.com/zolrath/obsidian-auto-link-title), [@ Symbol Linking](https://community.obsidian.md/plugins/at-symbol-linking), and [Wayback Archiver](https://community.obsidian.md/plugins/wayback-archiver). Pin exact reviewed versions or commits during OBS.1; these moving links are discovery pointers, not conformance claims.

**Delivery placement:** persisted-format understanding and independently useful headless workflows may be promoted one at a time. Seafile/CardDAV synchronization stays in Phases 12/15, editor-only behavior stays in Phase 14, and supervised external tools stay in Phase 15. The track as a whole never blocks the daemon.

#### OBS.1 Capability inventory, adapter matrix, fixtures, and explicit non-work

- [ ] Add a capability-to-adapter matrix covering HedgeSync, Seafile Sync Improved, Broken Links, Waypoint, Wikilink Types, VCF Contacts, QuickAdd, Local Images Plus, LanguageTool, Auto Link Title, and @ Symbol Linking. For each source, name the native Vulcan owner and distinguish persisted-format compatibility, settings import, conformance profile, provider integration, headless workflow, daemon integration, and WebUI-only behavior.
- [ ] Pin reviewed upstream plugin versions or commits and record relevant settings keys, marker syntax, frontmatter schemas, mutation rules, licenses, and graceful-degradation behavior. Vendor only the minimal permitted fixtures needed for conformance tests, with provenance metadata.
- [ ] Audit proposed CLI groups, config keys, JSON types, daemon routes, MCP tools, and assistant skills. Rename plugin-shaped public surfaces around the native outcome; retain plugin names only for import/adapter/profile selection and migration aliases with an explicit removal or support policy.
- [ ] Add focused fixture vaults for each persisted format instead of depending on a user's installed `.obsidian/plugins/` tree. Fixtures must cover absent plugins, malformed settings, legacy settings, case-sensitive paths, Unicode, nested folders, and mixed-plugin interactions.
- [ ] Confirm and document the existing QuickAdd boundary rather than building a duplicate macro engine: capture/template variables and settings import are supported; Macro/Multi choices map to `vulcan run` or shell workflows; editor selection and command-palette actions remain UI-only.
- [ ] Confirm that @ Symbol Linking output needs no special parser mode: emitted Markdown links, aliases, folders, and template-created notes use existing Vulcan semantics. Defer symbol-trigger autocomplete and paste/editor interception to Phase 14.
- [ ] Keep HedgeSync as an external `hedgesync` CLI integration. Do not reimplement HedgeDoc transport, bidirectional merge, or live operational-transform synchronization in Vulcan; document safe invocation followed by incremental rescan and route any future supervised external-command integration through Phase 15.

#### OBS.2 Broken link and subpath diagnostics

- [ ] Extend link resolution diagnostics beyond missing documents to distinguish a missing document, missing heading, missing block reference, malformed subpath, ambiguous document, and unsupported target form while retaining raw and resolved link representations.
- [ ] Validate heading anchors using Obsidian-compatible slug/duplicate-heading behavior and validate block IDs against indexed block references for wikilinks, Markdown links, and embeds.
- [ ] Report source path, byte range, raw target, resolved document, missing subpath, and stable diagnostic code through `doctor`, `note ... --check`, JSON output, and daemon-compatible report types.
- [ ] Add `doctor` grouping and filtering useful for Broken Links-style folder/file/target views without putting presentation-only tree state in the cache.
- [ ] Add safe repair suggestions for creating a missing note, selecting an unambiguous target, removing only the missing subpath, or rewriting to an existing heading/block. Do not auto-apply guesses; route accepted changes through existing note/refactor workflows.
- [ ] Test duplicate headings, renamed headings and blocks, self-links, percent encoding, aliases, Unicode anchors, excluded paths, embeds, stale caches, and dry-run repair plans.

#### OBS.3 Generated navigation and MOCs with Waypoint/Landmark adapters

- [ ] Define native generated-navigation configuration for markers, folder-note behavior, sorting, exclusions, title/alias preferences, and nearest-folder-note policy. Add an explicit Waypoint settings importer from `.obsidian/plugins/waypoint/data.json`; unknown, unsupported, or lossy mappings produce migration diagnostics.
- [ ] Implement Waypoint and Landmark percent-comment triggers plus generated begin/end regions as persisted-format adapters over the native generated-navigation model. Preserve marker spelling and content outside owned regions exactly.
- [ ] Build a deterministic hierarchy planner on the shared folder-note model from Phase 9.31. Support nested waypoints, landmark pass-through, pruning at child waypoints, nearest-folder-note mode, excluded files/folders, aliases/titles, and every configured folder-note placement/name convention.
- [ ] Add `vulcan navigation plan|reconcile [query] [--dry-run]` in `vulcan-app` with a thin CLI adapter and an explicit `--format waypoint` compatibility selector where format-specific output is required. Reconciliation replaces only a well-formed owned region, fails on duplicate/unbalanced markers or path collisions, and never silently repairs hand-edited ambiguous regions.
- [ ] Add `doctor` diagnostics for stale generated regions, orphan markers, multiple waypoint regions, waypoints outside valid folder notes, and folder notes made invalid by external moves.
- [ ] Integrate reconciliation planning with Vulcan note/folder moves, folder-note conversion, and daemon watch events. Direct mutating commands should either update affected waypoints in the same planned operation or report that reconciliation is required; read-only export must never mutate the vault.
- [ ] Let exports choose an explicit policy: require current canonical generated Markdown, fail/warn when stale, or render a temporary regenerated publication projection. Temporary projections must use the same planner and must not be written back to source files.
- [ ] Test normal waypoints, landmarks, nested pruning, custom markers, all folder-note conventions, aliases, moves, deletions, malformed regions, deterministic ordering, idempotency, export projections, and mutation-free dry runs.

#### OBS.4 Typed relationships with a Wikilink Types adapter

- [ ] Define a native typed-relationship registry, then import `.obsidian/plugins/wikilink-types/data.json` into it while preserving authored key, label, description, and order and rejecting unsafe/duplicate frontmatter keys.
- [ ] Parse configured `@type` annotations in wikilink aliases without changing ordinary alias text or interpreting unconfigured `@words`, email addresses, code, comments, or escaped text as relationships.
- [ ] Project typed relationships as derived graph-edge metadata while keeping the raw link and authoritative YAML property values intact. Extend graph/query/JSON surfaces to filter or group by relationship type without replacing the canonical untyped link graph.
- [ ] Add `vulcan refactor relationships reconcile [query] [--format wikilink-types] [--direction alias-to-frontmatter|frontmatter-to-alias] --dry-run`. Default to conflict reporting when alias annotations and frontmatter disagree; never silently choose one source or reorder unrelated YAML.
- [ ] Add doctor diagnostics for unknown types, malformed annotations, duplicate typed targets, alias/frontmatter drift, property type mismatches, unresolved typed targets, and conflicting relationship definitions.
- [ ] Ensure move/rename, property rename, export, static rendering, Outline publishing, Dataview, Bases, and mdbase adapters preserve typed relationships and do not duplicate them during repeated scans or transforms.
- [ ] Test multiple types on one link, multiple links to one target, aliases containing natural `@` text, YAML scalar/list forms, renamed targets, custom registries, conflicts, idempotent reconciliation, and dry-run immutability.

#### OBS.5 Contact records and vCard interchange with a VCF Contacts adapter

- [ ] Pin the supported VCF Contacts Markdown/frontmatter schema and vCard 4.0 mapping. Model contact identity, names, organization, email/phone collections, addresses, URLs, birthdays, notes, categories/groups, UID, avatars, and plugin extension fields without flattening unknown properties.
- [ ] Define native contact configuration for folders, templates, avatar paths, enabled/default fields, and safe filename policy, then explicitly import reviewed VCF Contacts settings into it. Never import CardDAV credentials from plugin data into shared config.
- [ ] Add reusable `vulcan-app` workflows and thin CLI commands for `vulcan contacts import-vcf`, `export-vcf`, `validate`, and `query`, with full-vault/query selection, deterministic plans, structured reports, collision handling, and dry-run for Markdown mutations.
- [ ] Preserve vCard parameter/value escaping, folding, repeated properties, Unicode, stable UID, timezone/date distinctions, unknown `X-` properties, and round-trip information where the Markdown schema can represent it. Report lossy mappings before writing.
- [ ] Treat avatars as normal resolved attachments: validate containment and existence, copy/embed them deterministically on import/export where requested, and reuse attachment move/export/publication policies.
- [ ] Keep contact notes ordinary Markdown/frontmatter so existing query, Bases, Dataview, search, export, and graph operations work without a special runtime.
- [ ] Test single and multi-card files, organization/group cards, repeated fields, quoted-printable/base64 or explicitly unsupported encodings, folded lines, malformed cards, UID/path collisions, avatars, unknown fields, deterministic output, and import-export-import round trips.
- [ ] Defer CardDAV synchronization, remote conflict resolution, address-book discovery, and scheduled refresh to the daemon integration boundary in Phases 12/15.

#### OBS.6 Asset Localizer with a Local Images Plus adapter

- [ ] Add `vulcan assets localize [query] [--dry-run]` as a native asset workflow. Discover remote Markdown/HTML image references, responsive `srcset` candidates, supported frontmatter asset fields, and base64 data images from parsed/resolved content; plan deterministic local paths, download or decode them, and rewrite only successfully materialized references.
- [ ] Define native `[assets.localize]` configuration and reuse attachment-folder semantics. Offer naming strategies such as content hash, source filename, note-relative folder, and note-named folder. Detect exact, case-insensitive, Unicode-normalization, hash, and extension/MIME collisions before writing; add a Local Images Plus importer for settings that map cleanly and diagnose the rest.
- [ ] Apply existing network permissions and SSRF protections, HTTPS policy, redirect limits, content-length and decoded-size limits, MIME sniffing, bounded concurrency/retries/timeouts, sanitized errors, and optional allowed-domain filters. Scanning and rendering must never trigger downloads.
- [ ] Support explicit authenticated localization through named device-local credential profiles: secret-store/environment-backed headers or cookies, plus opt-in browser-cookie sources naming a supported browser and profile. Browser-cookie use requires a distinct permission and domain allowlist, applies browser-equivalent host/path/secure/expiry filtering, re-authorizes redirect origins, and never falls back automatically from an unauthenticated request.
- [ ] Keep credential values out of `config.toml`, `config.local.toml`, notes, cache rows, durable reports, diagnostics, and logs. Decrypt browser cookies only during execution and redact all request/response metadata that could reveal them. `--dry-run` may validate the local plan and profile metadata but must not read/decrypt cookies or issue network requests.
- [ ] Write assets through verified temporary files plus atomic rename, then rewrite notes through the shared mutation engine. On interruption, leave either the original remote reference or a fully verified local asset; do not leave a rewritten reference to a partial file.
- [ ] Add hash-based duplicate reporting and an explicit deduplication plan that rewrites references before removing duplicates. Keep orphan cleanup a separate opt-in command with trash/recoverable behavior; never delete assets as a side effect of localization.
- [ ] Keep image conversion/quality changes opt-in and separate from localization so byte-preserving downloads remain the default. Record format-loss diagnostics and preserve originals unless explicitly requested.
- [ ] Integrate localized assets with doctor, moves, exports, static sites, Outline publishing, OCR/extraction, and watcher refresh.
- [ ] Test remote URLs with queries/fragments, HTML/Markdown/responsive images, base64 data, redirects, MIME mismatches, oversized bodies, duplicate content, missing/failed downloads, path traversal attempts, retries, partial failures, deterministic plans, and mutation-free dry runs using mock servers. Add browser-cookie tests for browser/profile selection, locked or unavailable stores, domain/path/secure/expiry filtering, cross-origin redirects, permission denial, redaction, and proof that dry-run never accesses secrets or the network.

#### OBS.7 Web archival workflows with Wayback and other provider adapters

- [ ] Define a provider trait for archive lookup/submission and implement the supported Wayback Machine operations from reviewed official APIs. Treat archive.today/Web Gyotaku or other providers as separate capability-declared adapters rather than emulating browser-only flows.
- [ ] Add non-secret archive profiles to shared config and device-local endpoint overrides, credential environment-variable names, timeouts, retry limits, and provider secrets to `.vulcan/config.local.toml` or environment variables only. Never log request credentials or signed provider URLs.
- [ ] Add `vulcan web archive [query] --profile <name> [--dry-run]` to parse external URLs from Markdown links, HTML links, plain URLs, and image references; apply include/exclude/domain/substitution rules; and report a deterministic archive plan before network or file mutation.
- [ ] Support explicit append, replace, and metadata/frontmatter recording policies with nearby-existing-archive detection. Preserve the original URL by default, make repeated runs idempotent, and report remote drift or ambiguous existing annotations instead of duplicating/replacing them.
- [ ] Bound pagination, concurrency, timeouts, retries, response sizes, and total work. Surface rate limits and provider-specific failure states with sanitized structured reports, and make interrupted batches safely retryable from canonical note content.
- [ ] Route successful Markdown changes through atomic app workflows, permissions, incremental scan, plugin hooks, and optional git commit. Dry-run may validate local plans but must not submit captures or mutate provider state.
- [ ] Test current-note/query/full-vault selection, filtering, substitutions, existing archives, idempotent repeats, provider fallback, authentication failure, rate limiting, retries, malformed responses, partial batches, interruption recovery, and mutation-free dry runs with mock providers.

#### OBS.8 Language diagnostics with a LanguageTool provider

- [ ] Define a language-check provider boundary with a LanguageTool HTTP implementation supporting self-hosted and explicitly configured remote endpoints. Keep the provider outside the Markdown parser and make checking an explicit network-capable operation.
- [ ] Add `vulcan lint language [query] [--profile <name>]` with language selection/auto-detection, configurable disabled rules/categories, personal dictionaries, ignored paths/regions, bounded note/request batching, timeouts, retries, and structured diagnostics tied to source byte ranges.
- [ ] Exclude frontmatter keys/values, code, math, URLs, generated regions, templates, and other configured syntax using parser spans rather than destructive preprocessing; retain stable offset mapping back to original Markdown.
- [ ] Add an explicit suggestion-application workflow that checks the expected original text, detects overlapping/stale edits, previews every patch, and applies accepted replacements atomically. Never auto-correct during scan, save, export, or daemon indexing.
- [ ] Store only configuration and optional vault dictionaries canonically. LanguageTool responses are ephemeral/derived and must not be required to rebuild the vault cache.
- [ ] Test multilingual notes, Unicode offsets, ignored syntax, overlapping suggestions, stale content, unavailable/auth-failing servers, oversized notes, batching, retries, permission denial, deterministic reports, and mutation-free checks with a mock server.
- [ ] Defer interactive underlines, hover explanations, accept/reject controls, and as-you-type requests to the authenticated Phase 14 editor.

#### OBS.9 Link-title enrichment with an Auto Link Title compatibility policy

- [ ] Extend the safe web-fetch result with normalized HTML title metadata and well-defined fallbacks without weakening existing network permissions, SSRF protections, redirect limits, content limits, or sanitization.
- [ ] Add `vulcan refactor link-titles [query] [--dry-run]` for bare URLs, empty Markdown labels, or explicitly selected existing labels. Default to filling missing titles only; require an overwrite flag to replace authored link text.
- [ ] Preserve URL spelling/destination, surrounding Markdown, reference-style links, fragments, and duplicate occurrences. Skip code, autolinks where conversion is disabled, images, unsupported schemes, and URLs excluded by policy.
- [ ] Deduplicate requests by normalized URL, use bounded concurrency/timeouts/retries, report per-URL failures without corrupting notes, and revalidate source spans/content before applying atomic patches.
- [ ] Test title entities/Unicode, missing or malformed titles, redirects, duplicate URLs, existing labels, reference links, inaccessible hosts, partial failures, deterministic plans, and mutation-free dry runs with mock servers.
- [ ] Defer clipboard interception, selection-sensitive replacement, keyboard shortcuts, and automatic paste behavior to Phase 14; the headless command remains independently useful for CLI, agents, and CI.

#### OBS.10 Cross-capability integration and completion gate

- [ ] Define ordering when one operation affects several native capabilities and adapters: secure filesystem mutation, folder-note/generated-navigation planning, link and typed-relationship rewrites, attachment updates, scan refresh, doctor diagnostics, hooks, then optional git commit.
- [ ] Add combined fixtures for folder notes plus Waypoints, Wikilink Types plus Dataview/Bases/mdbase, contacts plus avatars, and remote-image localization plus Wayback annotations. Reindex twice and assert identical cache state.
- [ ] Verify all new commands under human, Markdown, and JSON output; non-interactive operation; permission denial; feature-disabled builds; dry-run; auto-commit opt-in; and daemon-compatible report serialization.
- [ ] Update config descriptors, generated reference docs, integrated help, assistant skills, limitations, migration guidance, and the capability-to-adapter matrix as each subphase lands. Verify that discovery uses native capability terms and that plugin names remain findable for migration questions.
- [ ] Do not claim the OBS track complete merely because ordinary Markdown degrades gracefully. Each advertised plugin surface needs pinned upstream evidence, focused fixtures, mutation safety tests, and an explicit statement of unsupported UI/network/sync behavior.

#### Deferred adapters and presentation work

- **Seafile and general vault synchronization:** implement in Phase 12 after the daemon and conflict/versioning contracts exist. Prefer a supervised standalone Seafile client or reusable sync engine over duplicating Seafile's protocol in `vulcan-core`.
- **Virtual or remote vault storage:** do not retrofit as OBS compatibility work. The initial daemon keeps a materialized local vault as canonical. Revisit a `VaultStorage` boundary only for a concrete embedded deployment and only if it can provide coherent snapshots, safe enumeration, atomic writes/renames, locking, metadata, change notifications, and a fully rebuildable local cache.
- **CardDAV and contact synchronization:** defer to Phases 12/15; OBS.5 covers deterministic local Markdown/vCard interchange only.
- **Editor-only compatibility:** Wikilink Types and @ Symbol autocomplete, Auto Link Title paste interception, LanguageTool inline feedback, contact forms/actions, and other cursor/clipboard/command-palette behavior belong in Phase 14.
- **HedgeDoc live/bidirectional synchronization:** remain delegated to HedgeSync. A later daemon integration may supervise its CLI as an external process, but HedgeDoc content must not become a second Vulcan cache or an implicit input to unrelated publication flows.

---

### SB: Promoted SilverBullet connector appendix (formerly 9.34)

**Goal:** Let a Markdown vault participate safely in SilverBullet workflows at three independent layers: native understanding of SilverBullet-authored Markdown, an optional SilverBullet-compatible file-sync peer, and a first-party SilverBullet plug backed by Vulcan's daemon API. Keep ordinary files canonical, keep every index rebuildable, and avoid making SilverBullet's browser runtime or object index a second source of truth.

**Depends on:** Phase 1 parser/link/attachment indexing, Phase 2 serialized and atomic vault writes, Phase 7 diagnostics and exports, Phase 9.18 permission-aware command services, Phase 9.20 publication transforms, Phase 9.31 folder-note configuration, Phase 10 daemon/API/authentication for the plug and server peer, Phase 11 checkpoint/conflict support, and Phase 12 sync-backend lifecycle for remote mirroring. Native syntax discovery, fixtures, diagnostics, and byte-preserving export may begin before the daemon; network sync and the plug must not bypass their later dependencies.

**Compatibility boundary:** SilverBullet's Space is still a directory of ordinary Markdown and assets. Sync transports bytes and metadata without rewriting representations; compatibility transforms are explicit export/refactor operations. Incoming accepted writes materialize atomically in the local working tree before Vulcan rebuilds derived state. SilverBullet's client-side object index, Vulcan's SQLite cache, browser databases, generated query results, and runtime state are all disposable derivatives rather than authorities.

**Initial upstream reference:** start from an exact reviewed SilverBullet 2.x release and commit. The current upstream client implements filesystem operations through `client/spaces/http_space_primitives.ts`, the two-sided snapshot algorithm through `client/spaces/sync.ts`, and browser scheduling/state through `client/service_worker/sync_engine.ts`; its current transport uses `/.fs` file operations and `/.ping` version discovery. These are implementation references, not a promise of a stable public protocol. Record licenses and provenance, and require an explicit compatibility review before changing the pinned release.

**Delivery placement:** SB.1–SB.3 support the Phase 15 selective content connector. SB.4–SB.5 are optional Phase 12 full-Space protocol work. SB.6–SB.7 are optional Phase 15 runtime/plug work after the basic connector and daemon API are stable. SB.8 applies to every advertised SilverBullet capability. The appendix is promoted design, but none of it blocks Phase 10.

#### SB.1 Upstream inventory, version pinning, and conformance harness

- [ ] Pin one exact supported SilverBullet release and commit. Inventory the file protocol, metadata/header contract, authentication behavior, path encoding, error statuses, sync-ignore semantics, document/asset policy, conflict-copy naming, standard-library/plug handling, server version discovery, Space Lua syntax, PlugOS APIs, and Markdown extensions used by that version.
- [ ] Document two distinct protocol roles: a **server peer**, where an upstream SilverBullet browser syncs directly with a Vulcan-backed materialized vault; and a **client/mirror backend**, where Vulcan synchronizes with an existing SilverBullet server. Do not imply that implementing one role provides the other.
- [ ] Add licensed, provenance-recorded protocol and Markdown fixtures plus a mock SilverBullet peer. Add an opt-in conformance suite that runs against the pinned upstream server/client implementation as the oracle, without requiring network access in normal workspace tests.
- [ ] Introduce a machine-readable compatibility matrix covering supported SilverBullet versions, protocol role, Markdown extensions, runtime features, plug API version, and known deviations. Read `X-Server-Version` or the pinned equivalent and fail with a structured incompatibility error for unreviewed versions rather than continuing optimistically.
- [ ] Re-evaluate whether upstream has published a stable protocol or reusable runtime library at implementation time. Prefer a documented public API or official `sb` command when it satisfies the use case; isolate unavoidable private-protocol code behind a versioned Vulcan adapter.

#### SB.2 Native SilverBullet Markdown and link semantics

- [ ] Add a scoped SilverBullet compatibility mode in `vulcan-core` that recognizes the pinned release's page links, relative and absolute paths, headings, line/column and offset positions, meta-page references, stable `$anchor` references, transclusions, page attributes, task states, admonitions, fenced extensions, and executable Space Lua blocks/expressions. Unsupported or version-mismatched constructs produce diagnostics instead of disappearing.
- [ ] Preserve exact source plus raw, parsed, and resolved link representations. Keep Obsidian shortest-path/alias resolution as the normal default; enable SilverBullet's path-oriented resolution only for explicit compatibility profiles or SilverBullet-authored operations, with deterministic ambiguity and case-sensitivity behavior.
- [ ] Index stable anchors and SilverBullet-specific subpaths as derived metadata so backlinks, graph queries, doctor, moves, exports, and publication can distinguish a missing document from a missing anchor, header, line, or position.
- [ ] Treat Space Lua, plug bundles, and other executable content as inert during scan, index, doctor, ordinary rendering, and default export. Merely opening a Space must never execute vault code or fetch network resources.
- [ ] Add `vulcan doctor --compat silverbullet` and reusable report types for incompatible links, unsupported syntax, stale/generated regions, unavailable runtime-dependent output, path-resolution differences, and control/runtime files that should not be published.
- [ ] Add a `silverbullet` fixture vault covering every claimed syntax form, nested paths, duplicate names, Unicode/case conflicts, malformed constructs, links and transclusions to attachments, mixed Obsidian/SilverBullet syntax, and parse-render-reparse/source-preservation behavior.

#### SB.3 Space export and explicit transformation policies

- [ ] Add a reusable `vulcan-app` planner and thin CLI surface such as `vulcan export silverbullet-space [query] --path <directory-or-archive>`. Follow the shared export convention that an omitted query selects the whole vault, while an explicit query restricts the publication set.
- [ ] Reuse canonical query selection, publication transforms, resolved links, attachments, folder-note planning, exclusions, and deterministic collision checks. Never mutate the source vault, and fail on missing assets, excluded link targets, unresolved required references, unsafe paths, case/Unicode-normalization conflicts, or ambiguous representation changes.
- [ ] Make executable and generated content policy explicit and independently configurable: `preserve`, `evaluate`, `strip`, or `error` where meaningful. Default to byte/source preservation; `evaluate` is unavailable unless the separately gated runtime is enabled and authorized, and generated results must be marked as projections rather than written back implicitly.
- [ ] Support deterministic dry-run/planning and structured JSON reports including selected files, rewritten references, copied assets, preserved executable regions, required runtime capabilities, warnings, and output hashes.
- [ ] Test full-vault and query exports, nested pages, all configured folder-note conventions, links, transclusions, anchors, attachments, mixed syntax, exclusions, collisions, missing files, deterministic output, runtime-disabled policies, and mutation-free planning.

#### SB.4 SilverBullet-compatible server peer

- [ ] After Phase 10, implement the pinned server-side file protocol in an async daemon adapter, not in `vulcan-core`. Serve only explicitly configured vaults and routes; keep protocol request/response types separate from transport-neutral vault mutation services.
- [ ] Implement the reviewed file-list, metadata, read, write, delete, ping/version, authentication, path-encoding, sync-mode, and error contracts. Preserve file bytes and required safe metadata while refusing platform-unsafe permissions or unsupported metadata with explicit diagnostics.
- [ ] Normalize percent-decoded paths once; reject traversal, absolute paths, reserved/control paths, NULs, invalid Unicode policy, symlink escapes, special files, case/normalization aliases, oversized lists/bodies, and writes outside the selected vault. Apply daemon authentication, resolved capabilities, canonical policy ceilings, rate/body limits, timeouts, cancellation, and sanitized logs.
- [ ] Route accepted writes and deletes through Vulcan's cross-process vault lock, verified temporary file plus atomic replacement, mass-deletion guard, watcher coalescing, incremental scan, optional git checkpoint, and event reporting. A successful protocol response must never expose a partial file or claim an unindexed permanent mutation.
- [ ] Let the upstream SilverBullet client retain its own sync snapshot and conflict algorithm when Vulcan is only the server peer. Surface conflict copies as ordinary canonical files plus diagnostics; do not silently merge, discard, or reinterpret them.
- [ ] Provide explicit deployment support for serving or reverse-proxying the pinned SilverBullet client separately from Vulcan's API. Do not fork or silently patch upstream browser assets as part of the protocol adapter.
- [ ] Test initial listing, metadata-only reads, binary assets, creates, replacements, deletes, interruption, concurrent direct edits, clock skew, equal timestamps with differing sizes/content, conflict copies, ignore/control paths, authentication/authorization failures, path attacks, oversized requests, restarts, cache rebuild, and compatibility rejection against mock and pinned upstream clients.

#### SB.5 SilverBullet client and mirror sync backend

- [ ] After Phase 12, add a `vulcan-sync` SilverBullet backend that connects to an existing reviewed SilverBullet server through the pinned file protocol or official supported CLI/API. Keep a materialized local working tree; do not make remote HTTP objects masquerade as cache rows or retrofit remote I/O throughout `vulcan-core`.
- [ ] Require an explicit authority mode and deletion policy. A peer/mirror mode may accept edits from both endpoints and must use a durable two-sided snapshot; a one-way import/export mode must state its authoritative side and may not reuse bidirectional deletion semantics accidentally.
- [ ] Store remote identity, source identity/path, both last-seen revisions/metadata, content hash where available, tombstones, ignored/non-materialized entries, and protocol version in locked, atomically replaced durable state outside `.vulcan/cache.db`. Malformed or incompatible state must stop reconciliation without mutating either side and offer a reviewable rebuild/resync plan.
- [ ] Reproduce the pinned conflict behavior where compatibility requires it, including byte comparison and conflict-copy naming, while adding content hashes to guard against unchanged timestamps. Never use last-writer-wins silently; preserve both versions and report conflicts through CLI JSON, daemon status, and `GET /{id}/sync/conflicts`.
- [ ] Integrate bounded concurrency, list/body limits, timeouts, cancellation, jittered bounded retries, authentication expiry, offline status, safe interruption/resume, ignore rules, plug/control-file policy, watcher quiescence, write locking, mass-deletion protection, scan refresh, and optional pre-sync git checkpoints.
- [ ] Keep endpoint URLs, vault/space identifiers, and non-secret policy in shared or daemon config as appropriate. Read bearer tokens and device-local values from environment variables, secret storage, or ignored local config; redact authorization headers, redirect targets containing secrets, and response bodies from errors.
- [ ] Add `sync status|plan|trigger` reporting for the backend. Planning must not write local files, remote files, or durable snapshots; ordinary retries after interruption must be idempotent.
- [ ] Test initial import/export, unchanged repeats, changes on either side, simultaneous edits, creates, moves represented by protocol operations, deletions/tombstones, binary assets, ignored/non-materialized files, plug ordering, timestamp collisions, stale/malformed snapshots, process restart, pagination or oversized-list behavior of the pinned version, retries, authentication failure, unknown server versions, mass deletion, and mutation-free plans with a mock server plus upstream conformance jobs.

#### SB.6 Optional SilverBullet runtime boundary

- [ ] Do not present Vulcan's QuickJS runtime as a SilverBullet runtime. Inventory which pinned features are Space Lua, which TypeScript plug sources compile to browser JavaScript, and which depend on PlugOS syscalls, Web Workers, IndexedDB, DOM/browser state, or the upstream headless-Chrome server runtime.
- [ ] Implement syntax preservation and pure static semantics in Rust. Reuse an upstream TypeScript module in-process only if it is separately licensed, version-pinned, deterministic, browser-independent, resource-bounded, and demonstrably smaller to maintain than a native adapter; do not emulate the full SilverBullet browser/PlugOS environment in QuickJS.
- [ ] If a concrete use case requires authoritative Space Lua, SLIQ, or generated-content evaluation, add an explicit optional adapter to a pinned official SilverBullet runtime or `sb` process. Treat it as a supervised external tool with an executable allowlist, isolated working directory, read-only input by default, memory/CPU/output/time limits, cancellation, environment allowlist, network denial unless separately granted, and sanitized structured results.
- [ ] Require an explicit command/export policy and permission profile before runtime execution. Never execute during scan, watch, sync, doctor, preview, or default rendering; never let runtime output mutate canonical Markdown without a normal dry-run/apply workflow and stale-input checks.
- [ ] Test malicious and nonterminating scripts, memory/output exhaustion, filesystem and network denial, unavailable/wrong runtime versions, malformed output, cancellation, secret redaction, deterministic pure evaluations where promised, and zero execution in all passive workflows.

#### SB.7 First-party SilverBullet plug

- [ ] After the Phase 10 API is stable, create a versioned first-party plug that talks only to Vulcan's authenticated daemon API. The plug must not open `.vulcan/cache.db`, assume shell access, invoke the CLI from the browser, or become the owner of vault synchronization.
- [ ] Start read-only with connection/scan/sync status, doctor diagnostics, full-text and semantic search, backlinks, graph relations, related notes, and compatibility reports. Degrade gracefully when Vulcan is offline so ordinary SilverBullet editing and its native sync continue working.
- [ ] Add mutating commands only through reusable Vulcan proposal/apply contracts: task actions, note/refactor operations, folder-note/Waypoint reconciliation, asset maintenance, export, and publication. Always show a deterministic preview, enforce daemon permissions and stale-content preconditions, and return structured partial-failure/conflict reports.
- [ ] Use SilverBullet save/sync events only as hints for status refresh or incremental scanning; rely on the daemon watcher for correctness, debounce duplicate notifications, attach operation identities, and prevent plug-daemon feedback loops.
- [ ] Keep API endpoint discovery and non-secret preferences separate from device credentials. Prefer same-origin reverse proxying or OAuth/pairing with PKCE; store tokens in device-local browser storage rather than synced CONFIG/Markdown, request the minimum vault-scoped capabilities, support revocation/expiry, and enforce CORS/CSRF/origin policy.
- [ ] Provide an explicit `vulcan integration silverbullet plug plan|install|update` workflow that pins compatible plug/API versions, verifies bundle hashes, previews the destination, preserves unrelated Space files, and never embeds credentials. Manual installation remains supported.
- [ ] Test the plug against a mock daemon and pinned SilverBullet host for first connection, offline behavior, read-only features, permission denial, token expiry/revocation, API/version mismatch, multiple vaults, save-event storms, proposal/apply conflicts, interrupted mutations, installation collisions, updates, and absence of secrets in synchronized files and logs.

#### SB.8 Cross-layer safety and completion gates

- [ ] Define ownership and event ordering across SilverBullet writes: authenticate and validate path, acquire the vault lock, optionally checkpoint, atomically materialize bytes, refresh derived state, publish daemon events, then allow plug status refresh. Sync transport must never invoke publication transforms or runtime evaluation implicitly.
- [ ] Add combined fixtures for SilverBullet links/anchors plus Obsidian links, folder notes and Waypoints, executable blocks, attachments, direct filesystem edits during browser sync, protocol conflict copies, and first-party plug operations. Reindex twice and rebuild from scratch to assert equivalent derived state.
- [ ] Verify CLI/daemon JSON contracts, permission denial, feature-disabled builds, dry-run immutability, secret sanitization, deterministic planning, crash recovery, cache deletion/rebuild, and operation with no `.obsidian/` directory.
- [ ] Publish setup, threat model, compatibility matrix, upgrade/downgrade procedure, state recovery, conflict handling, backup guidance, and limitations. Clearly distinguish shared-directory operation, Vulcan server-peer mode, remote mirror mode, static compatibility/export, and plug-only integration.
- [ ] Do not claim SilverBullet protocol compatibility until the pinned upstream conformance suite passes for the advertised role. Do not claim runtime compatibility based only on parsing executable syntax, and do not call the integration complete until unmanaged ordinary files remain untouched and a vault can be recovered from canonical files plus durable sync state without `cache.db`.

#### Deferred SilverBullet work

- **General runtime emulation:** a full PlugOS, browser, IndexedDB, Web Worker, DOM, or headless-Chrome reimplementation inside Vulcan is out of scope. Revisit only if upstream publishes a stable embeddable runtime and concrete use cases cannot use the supervised official process.
- **Collaborative semantic merge:** the pinned SilverBullet conflict-copy behavior is the compatibility baseline. CRDT/Automerge merging, shared cursors, and live multi-user editing belong to Phase 14/16 and must not be smuggled into file sync.
- **Virtual remote-only Spaces:** the first implementation requires a coherent materialized working tree. Revisit `VaultStorage` only through Phase 12.6's decision gate for a measured embedded use case.
- **SilverBullet object-index or query-store replication:** never synchronize or import SilverBullet's derived client index as Vulcan authority. Rebuild Vulcan's cache from the materialized Markdown and assets; invoke an optional pinned runtime only for explicit compatibility evaluation.

---

## New crates (Phases 10+)

| Crate | Type | Purpose |
|-------|------|---------|
| `vulcan-daemon` | lib | axum router, middleware, vault registry, daemon lifecycle |
| `vulcan-auth` | lib | Canonical authorization-object parsing and mutation, rooted grant attenuation/revocation, reserved-path and Git-ingress validation, credential/session secret handling, audit lineage, and request-authority resolution |
| `vulcan-sync` | lib | Sync backend trait and implementations (obsidian-headless, git remote, passive) |
| `vulcan-app-package` | lib | Phase 19 canonical manifest, strict ZIP validation, read-only package VFS, BLAKE3 identities, deterministic builder, and signature evidence |
| `vulcan-app-runtime` | lib | Phase 19 replaceable supervised QuickJS/server-WASM adapters and shared App API host bindings; feature-gated independently from package inspection |

The `vulcan-cli` binary gains the `daemon` subcommand group by depending on `vulcan-daemon`.
The `vulcan-daemon` crate depends on `vulcan-core` (for all vault operations) and `vulcan-sync` (for sync backends).
Phase 19's package inspection and validation must remain usable without enabling an executable runtime. The existing `vulcan-app` crate continues to own reusable synchronous workflow orchestration; the user-facing “Vulcan Apps” model uses unambiguous `VaultApp*` domain names rather than redefining that crate's purpose.

## Key dependencies to add (Phases 8+)

| Dependency | Purpose | Phase |
|------------|---------|-------|
| `aho-corasick` | Multi-pattern string matching for mention detection | 8 |
| `askama` or `maud` | Rust-side HTML templating for shared renderer / static site builder | 9.20 |
| `axum` | HTTP framework for daemon | 10 |
| `tokio` | Async runtime for axum | 10 |
| `tower-http` | CORS, compression, logging middleware | 10 |
| `argon2` | Token hashing | 10 |
| `automerge` | CRDT document model for collaborative editing | 14 |
| `rust-embed` or `include_dir` | Embed static WebUI assets | 13 |
| `openidconnect` | OIDC client for SSO integration | 17.6 |
| `teloxide` or `frankenstein` | Telegram Bot API client | 9.21.13 (deferred chat transport Telegram adapter) |
| `matrix-sdk` | Matrix client sync, room state, and E2EE integration | 9.21.14 (deferred Matrix adapter viability gate) |
| `regex` | Regex matching in note patch and query predicates | 9.18.2, 9.18.3 |
| `rquickjs` | QuickJS JS engine bindings (sandboxed runtime) | 9.18.5 (also 9.8.8) |
| `reqwest` | HTTP client for web search/fetch | 9.18.6 |
| `rs-trafilatura` | HTML-to-markdown content extraction for web fetch | 9.18.6 |
| `gix` (optional, exact-pinned) | Candidate embedded Git engine after the Phase 12 CLI baseline and conformance gate | 12 (post-MVP) |
| `termimad` | Terminal markdown rendering for `help` command | 9.18.7 |
| `rustyline` | REPL line editing, history, and tab completion | 9.18.5 |
| RFC 8785 canonical JSON implementation (audited crate or bounded in-tree implementation) | Validate and emit canonical `.vapp` manifests without accepting duplicate keys or non-v1 number forms | 19 |
| `wasmtime` or a conformance-equivalent replaceable engine (optional, exact-pinned) | Resource-limited server-side WebAssembly components behind the Phase 19 runtime adapter | 19.12 |

Phase 19 reuses the existing `blake3`, `ed25519-dalek`, and narrowly featured `zip` dependencies. Re-audit and update the ZIP implementation before freezing the hostile-input parser boundary rather than assuming the currently pinned version exposes every structural check required by the v1 profile.
