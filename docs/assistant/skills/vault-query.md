---
name: vault-query
description: Choose between search, query, filters, and structured note listing.
version: 3
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
- Use `path_prefix`, `filename_pattern`, explicit `sort`/`desc`, and `limit` for structural navigation. MCP query results default to 50 compact rows and report pagination metadata.
- Treat MCP `offset` as relative to the query's own offset and reuse `next_offset` unchanged for the next page. Embedded query limits bound the whole selection; MCP `limit` is the page size.
- Prefer explicit `file.*` fields for filesystem metadata and `properties.<key>` for frontmatter/inline properties. Unprefixed property keys remain compatible but are less self-documenting.
- Use `ls --where` for quick path-oriented listings without the full query pipeline.
- Reach for `help filters` and `help query-dsl` when the predicate grammar is unclear.

## Guardrails

- Do not use text search when a property filter would be exact.
- Do not use query or search when a domain API such as `daily` or `task_list` already identifies the resource.
- Limits above 200 MCP rows require `allow_large_results: true`; 1000 is the hard per-call maximum.
- DQL accepts only `query`, `engine`, `limit`, and `offset`; use structural query mode for path, filename, field projection, or property inclusion controls.
- Default rows use a compact allowlist and omit properties, frontmatter, links, tasks, and inline-expression payloads unless explicitly projected.
- Regex predicates live in the query/filter world as `matches` and `matches_i`; they are not the same as FTS search syntax.
- If the result set is surprising, inspect the filter first before adding more conditions.

## Example Moves

- Find all notes with `status = open` and sort by due date.
- Search for a phrase in note bodies, then switch to `query` once the real discriminator is a property.
- Use `ls --where 'file.path starts_with \"Projects/\"'` when only the matching paths matter.
