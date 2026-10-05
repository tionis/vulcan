---
name: vault-query
description: Choose between search, native queries, canonical mdbase collection queries, filters, and structured note listing.
version: 6
tools:
  - search
  - query
  - ls
  - help
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Vault Query

## When to Use This Skill

Use this skill when the task depends on metadata, frontmatter, tags, paths, or precise selection logic.

## Recommended Flow

- Use `search` when the question is about note text, snippets, or ranked content matches.
- Use `query` when the answer depends on typed metadata, computed fields, or explicit sorting.
- For a collection governed by `mdbase.yaml`, use `vulcan mdbase query --file query.yaml --output json` for canonical mdbase queries. Native `query` and DQL are not interchangeable with that query format. Inspect `vulcan mdbase query --help` for input options.
- Use `path_prefix`, `filename_pattern`, explicit `sort`/`desc`, and `limit` for structural navigation. MCP query results default to 50 compact rows and report pagination metadata.
- Treat MCP `offset` as relative to the query's own offset and reuse `next_offset` unchanged for the next page. Embedded query limits bound the whole selection; MCP `limit` is the page size.
- Prefer explicit `file.*` fields for filesystem metadata and `properties.<key>` for frontmatter/inline properties. Unprefixed property keys remain compatible but are less self-documenting.
- Use `ls --where` for quick path-oriented listings without the full query pipeline.
- Reach for `help filters` and `help query-dsl` when the predicate grammar is unclear.
- Native query DSL starts with `from notes`, combines predicates with `and`, and uses `order by`;
  tag and folder selection are predicates, not alternate native sources.

## Guardrails

- Do not use text search when a property filter would be exact.
- Do not use query or search when a domain API such as `daily` or `task_list` already identifies the resource.
- Limits above 200 MCP rows require `allow_large_results: true`; 1000 is the hard per-call maximum.
- DQL accepts only `query`, `engine`, `limit`, and `offset`; use structural query mode for path, filename, field projection, or property inclusion controls.
- Default rows use a compact allowlist and omit properties, frontmatter, links, tasks, and inline-expression payloads unless explicitly projected.
- Regex predicates live in the query/filter world as `matches` and `matches_i`; they are not the same as FTS search syntax.
- A single `--where` value is one predicate. Repeat `--where` for `AND`; it does not accept `OR`,
  parentheses, `!=`, `in`, or `is null` (`field = null` is supported).
- If the result set is surprising, inspect the filter first before adding more conditions.
- MDB queries never return results from records that changed on disk. A cached record is reused only when its file identity and change time match the bytes that were indexed; otherwise readable sources are reconciled before returning. In an initialized vault, unrestricted queries may populate or repair the rebuildable cache without changing notes. Uninitialized collections remain source-only and are not initialized as a query side effect. Restricted queries reuse matching cached records or read authorized sources without publishing a partial collection. A `stale_state` error means records changed during preparation: retry after the edit finishes, rather than trusting the previous cache. `--refresh off` does not bypass this MDB check.
- Simple metadata queries run from the index without loading note bodies: type selection, `where` conjunctions (`&&`) of field or `file.path` comparisons and `startsWith` against literals, field selections, ordering, and limits. Expression selections, projections, grouping, summaries, `include_body`, `||`, and other filter forms still evaluate every visible record. Prefer the simple shape for frequent list queries rather than assuming a small response is cheap.
- Cache reuse additionally needs read authority for `mdbase.lock.yaml`, even when absent. Without that grant, queries retain the existing authorized source-only path without probing the lockfile; do not widen a user's permissions merely to enable caching.
- If direct CLI search, query, `ls`, tags, properties, backlinks, or links reports a pending ordinary-write journal, inspect it with `vulcan repair ordinary-write status`; do not use another read path to bypass the recovery check.

## Example Moves

- Find all notes with `status = open` and sort by due date.
- Search for a phrase in note bodies, then switch to `query` once the real discriminator is a property.
- Use `ls --where 'file.path starts_with \"Projects/\"'` when only the matching paths matter.
