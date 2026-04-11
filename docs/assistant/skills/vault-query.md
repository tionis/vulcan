---
name: vault-query
description: Choose between search, query, filters, and structured note listing.
version: 1
tools:
  - search
  - query
  - ls
  - help
require_confirmation: false
---

# Vault Query

## When to Use This Skill

Use this skill when the task depends on metadata, frontmatter, tags, paths, or precise selection logic.

## Recommended Flow

- Use `search` when the question is about note text, snippets, or ranked content matches.
- Use `query` when the answer depends on typed metadata, computed fields, or explicit sorting.
- Use `ls --where` for quick path-oriented listings without the full query pipeline.
- Reach for `help filters` and `help query-dsl` when the predicate grammar is unclear.

## Guardrails

- Do not use text search when a property filter would be exact.
- Regex predicates live in the query/filter world as `matches` and `matches_i`; they are not the same as FTS search syntax.
- If the result set is surprising, inspect the filter first before adding more conditions.

## Example Moves

- Find all notes with `status = open` and sort by due date.
- Search for a phrase in note bodies, then switch to `query` once the real discriminator is a property.
- Use `ls --where 'file.path starts_with \"Projects/\"'` when only the matching paths matter.
