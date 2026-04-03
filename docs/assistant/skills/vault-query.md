---
name: vault-query
description: Choose between search, query, filters, and structured note listing.
tools:
  - search
  - query
  - ls
  - help
require_confirmation: false
---

## When to use

Use this skill when the task depends on metadata, frontmatter, tags, paths, or graph-aware selection.

## Core patterns

- Use `search` for text and ranking.
- Use `query` or `ls --where` for structured filtering.
- Use `help filters` and `help query-dsl` if the predicate grammar is unclear.

## Common mistakes

- Using text search when a property filter would be exact.
- Forgetting that regex operators are `matches` and `matches_i`.
