# Design Document
## Headless Obsidian Vault CLI with SQLite Graph Cache and Native Vector Search

**Implementation brief for the engineering agent**  
Date: 19 March 2026

> **Recommended direction:** Build a single-binary CLI in Rust, treat the vault as the source of truth, use SQLite as a rebuildable graph/content/property cache, and add vector search as a second derived index. Prioritize correctness of link semantics, incremental invalidation, and property normalization over feature breadth.

## Decision summary

- **Primary language:** Rust — Best fit for a fast, portable, single-binary CLI with strong text processing and SQLite integration.
- **Core storage:** SQLite — Excellent embedded relational store for cache tables, FTS, metadata, and query planning.
- **Full-text search:** SQLite FTS5 — Good hybrid-search partner; external-content mode avoids duplicating large text bodies.
- **Vector search:** `sqlite-vec` behind an abstraction — Keeps the solution embedded and local while preserving the option to swap backends later.
- **Property model:** Hybrid JSON + relational projections — Preserves loose Obsidian semantics without making query performance or typing unmanageable.
- **Correctness model:** Watcher + periodic reconciliation — File watchers improve freshness but should not be treated as a sufficient source of truth.

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
- Offer an implementation-friendly automation surface for scripts, agents, and shell workflows.

## 3. Non-goals for the initial implementation

Do not aim for full visual or behavioral parity with the Obsidian desktop app in version 1.

- Perfect rendering parity with every community plugin.
- Reproducing the entire interactive Bases UI.
- Supporting arbitrary plugin-defined syntax extensions during initial indexing.
- Using the cache as the authoritative source for note contents.
- Building a distributed service before the local architecture is stable.

Version 1 should be a local, rebuildable, correctness-oriented indexing and query tool.

## 4. Recommended architecture

Use a three-layer architecture.

### Layer 1: Vault source of truth
The filesystem is canonical. Markdown notes, attachments, `.obsidian` configuration, and `.base` files remain authoritative.

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

Every entity should carry enough provenance to support incremental repair: source document id, content hash, parser version, and extraction version where relevant.

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

## 7. Link semantics and move-safe rewrites

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

For the Rust implementation, prefer `pulldown-cmark` as the canonical parser for indexing and rewrite operations, with `Options::ENABLE_WIKILINKS` and `Options::ENABLE_GFM` enabled where appropriate. Current `pulldown-cmark` explicitly supports Obsidian-style wikilinks, supports GFM blockquote tags such as `[!NOTE]`, and exposes `into_offset_iter()` so the parser can return source byte ranges together with events.[13]

Those source ranges are the key reason to prefer it as the core parser for a CLI that needs safe rewrites. The rewrite engine should operate on semantic token spans rather than regex replacement or full document re-rendering.

Recommended approach:

- Parse Markdown into events plus source ranges.
- Run a small Obsidian-specific semantic pass over the parse output to classify callouts, embeds, block references, and any other OFM constructs that matter to indexing or rewriting.
- Persist both raw token text and resolved target identity.
- During a move or rename, query inbound references by resolved target document id, re-parse only the affected source files, and rewrite only the destination segment of each affected link.
- Preserve original style choices such as wikilink vs Markdown-link syntax, embed marker, display text/alias, and heading or block suffix.
- Apply edits from the end of the file toward the start so offsets remain valid while patching.

For callouts specifically, treat them as blockquotes with Obsidian semantics rather than as a wholly separate document form. Obsidian defines a callout by placing `[!type]` on the first line of a blockquote, and callouts can contain ordinary Markdown and internal links.[14]

`comrak` is the main alternative if a richer AST is preferred over a lighter event stream: it supports source positions, front matter, GitHub-style alerts, and wikilinks.[15] However, it is still a secondary recommendation here because span-based patching is the more important requirement for move-safe edits.

Do not use `tree-sitter-markdown` as the canonical source of truth for vault correctness. Its own README states that it is not recommended where correctness is important and positions it primarily as a source of syntactical information for editors.[16]

## 8. Property handling strategy

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

## 9. Full-text search architecture

Use SQLite FTS5 as the exact-search backbone. Prefer an external-content table so the FTS index can reference content stored in ordinary cache tables rather than duplicating it. SQLite documents external-content tables explicitly, but also notes that they require the application to keep the index synchronized with the content table.[7]

Recommended indexed units:

- note title / filename
- aliases
- headings
- body chunks
- selected property text

The FTS query surface should remain simple and predictable. Do not attempt to encode every Obsidian search operator in version 1. Focus on reliable term lookup, phrase search, snippets, and ranking that can be composed with relational filters.

## 10. Native vector search and clustering

Vector search should be implemented as a second derived index. Do not embed vectors directly into the graph tables.

### Recommended approach
- Embed chunks, not whole notes.
- Store vectors keyed by `chunk_id` and embedding model id.
- Keep model metadata: provider, model name, dimensions, normalization, chunking version.
- Re-embed only changed chunks.
- Use hybrid retrieval: FTS candidate generation, vector similarity, optional reranking.

`sqlite-vec` is a strong embedded-first fit for this project because it stores and queries float, int8, and binary vectors in `vec0` virtual tables and supports metadata columns. However, it is explicitly pre-v1, so it should be wrapped behind a small abstraction layer rather than imported deep into the architecture.[8]

Clustering should run in application code, not inside SQLite. Persist results back into the cache only as derived artifacts such as cluster ids, labels, centroids, or neighbor tables.

## 11. Bases support

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

## 12. Language and framework recommendations

### Primary recommendation: Rust

Why Rust:

- Strong fit for a single-binary cross-platform CLI.
- Good control over parsing, indexing, and performance-sensitive text processing.
- Excellent ergonomics for embedding SQLite through `rusqlite`.
- Mature CLI ergonomics through `clap`.
- A reasonable path to file watching and efficient concurrency.

Suggested Rust stack:

- `clap` for CLI structure
- `rusqlite` for SQLite access
- `notify` for file watching
- `pulldown-cmark` with `ENABLE_WIKILINKS` and `ENABLE_GFM`, plus a small Obsidian semantic pass for callouts, embeds, and rewrite-relevant token classification
- `serde` / `serde_yaml` for structured parsing
- `sqlite-vec` loaded as an extension behind a local trait or adapter

### Secondary recommendation: Go
Choose Go only if delivery speed and implementation simplicity matter more than maximum parsing control.

Do not start with TypeScript unless sharing code with an Obsidian plugin ecosystem is a hard requirement; native packaging and high-volume text processing are generally better served by Rust or Go for this tool.

## 13. Operational and schema recommendations

### Schema guidance
- Use stable ids internally; never rely on path text as the sole join key.
- Record `schema_version`, `parser_version`, `embedding_version`, and `extraction_version`.
- Keep enough diagnostics to explain cache state to the user.
- Build explicit repair commands such as `scan`, `reindex`, `verify`, and `doctor`.
- Treat the database as disposable; never require the user to preserve it across incompatible versions.

### CLI UX for humans and agents
The CLI should be designed for both direct human use and reliable agent invocation. These are related but not identical goals: human UX benefits from discoverability and convenience, while agent UX benefits from predictability, machine readability, and strong input validation. This section follows the same core framing argued by Justin Poehnelt: human DX and agent DX are orthogonal enough that the raw, predictable, machine-oriented path must be designed deliberately rather than treated as an afterthought.[12]

Recommended guidance:

- Keep a raw structured-input path as a first-class interface for every complex operation. Convenience flags are useful for humans, but nested actions such as Bases evaluation, structured moves, batch rewrites, or vector indexing options should also accept a single JSON payload that maps directly to the internal request model.
- Make machine-readable output a stable product surface, not a debug mode. Support `--output json` on all commands, and prefer line-delimited JSON for streamed or paginated output so agents can process results incrementally.
- Make the CLI self-describing at runtime. Add a command such as `describe`, `schema`, or `help --json` so an agent can inspect accepted parameters, payload shape, defaults, mutability, and output schema without relying on stale external documentation.
- Keep human-facing defaults pleasant, but make non-interactive behavior deterministic. Avoid prose-heavy output, spinner-only progress, or prompts that appear unexpectedly when stdout is not a TTY.
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

## 14. Performance considerations

Important performance principles:

- Parse only changed files, but make full rebuilds cheap and reliable.
- Chunk lazily but deterministically.
- Use batch transactions for indexing.
- Avoid over-normalizing cold data.
- Materialize only the projections that materially improve query plans.
- Keep link resolution and property coercion deterministic so cache churn stays low.

JSONB can be attractive for canonical property storage in SQLite because current SQLite versions support storing JSON in a binary form that avoids repeated parse/render overhead and usually consumes slightly less space.[9]

Be conservative with generated columns and expression indexes. They are useful for stable scalar derivations, but they are not a substitute for explicit side tables when dealing with array membership, multivalue properties, or other many-to-one indexing problems. SQLite’s JSON functions include table-valued functions such as `json_each` / `json_tree`, but these are not a good foundation for every hot-path property query.[9][10][11]

## 15. Known limitations and design constraints

The implementation agent should explicitly accept these constraints:

- Perfect parity with all Obsidian plugins is not feasible.
- Some vault semantics will remain app-specific or plugin-specific.
- Property types are inconsistent across real vaults and must be handled leniently.
- `sqlite-vec` may change before v1, so backend isolation is mandatory.
- FTS and vector search require careful synchronization; both are derived indexes, not source data.
- File system notifications are imperfect and platform-dependent, so reconciliation scans are not optional.

The product should fail loud, explain clearly, and repair cheaply.

## 16. Recommended phased delivery plan

### Phase 1: Core indexing
- File discovery, document table, frontmatter parsing, headings, block refs, links, aliases, tags.
- SQLite cache, rebuild path, diagnostics, and `doctor` command.

### Phase 2: Safe graph operations
- Backlinks, unresolved-link reporting, alias-aware resolution, move-safe rewrites.

### Phase 3: Search
- FTS5 indexing, snippets, hybrid retrieval scaffolding.

### Phase 4: Properties and Bases
- Canonical property storage, relational projections, typed filtering, read-only subset of Bases evaluation.

### Phase 5: Vectors
- Chunking, embedding pipeline, `sqlite-vec` integration, nearest-neighbor search, duplicate detection, clustering.

### Phase 6: Hardening
- Cross-platform watcher behavior, parser fuzzing, migration testing, performance tuning, CLI polish.

## 17. Hard requirements for the implementation agent

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
