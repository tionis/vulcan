---
name: index-maintenance
description: Maintain the cache, scanner, search index, vector index, and derived repairable state. Use when the user asks about scan, reindex, cache, repair, watch, vectors, embeddings, stale search results, or index diagnostics.
version: 1
tools:
  - index_scan
  - index_rebuild
  - index_status
  - cache_verify
  - repair
  - vectors
  - help
require_confirmation: false
---

# Index Maintenance

## When to Use This Skill

Use this skill when derived state may be stale, broken, slow, or incomplete.

## Recommended Flow

- Use `vulcan index scan` for normal incremental refresh.
- Use `vulcan index rebuild` when schema/content drift or cache corruption is suspected.
- Use `vulcan cache verify` and `vulcan doctor` to distinguish cache problems from source-note problems.
- Use `vulcan repair` for derived index repair paths before manually deleting cache files.
- Use `vulcan vectors ...` for embedding queue, neighbors, duplicates, clusters, and vector repair.

## Guardrails

- The vault is the source of truth. `.vulcan/cache.db` and search/vector indexes are rebuildable.
- Prefer repair/rebuild commands over manual SQLite edits.
- Do not treat stale cache output as note truth; rescan before making write decisions.
- Vector search depends on provider/config/model state, so inspect queue/status before assuming semantic search is broken.

## Example Moves

- Reindex after external file edits before answering a vault-wide query.
- Repair FTS/vector drift after a failed run.
- Explain why a note exists on disk but does not appear in search results.
