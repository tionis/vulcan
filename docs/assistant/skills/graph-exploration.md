---
name: graph-exploration
description: Explore backlinks, hubs, paths, and connectivity in the vault graph.
tools:
  - backlinks
  - links
  - graph_path
  - graph_hubs
  - graph_components
require_confirmation: false
---

## When to use

Use this skill when note relationships matter more than raw content matching.

## Core patterns

- Start with backlinks or links for one note.
- Use graph path or hubs when the task depends on topology.
- Combine graph inspection with `query` for metadata constraints.

## Common mistakes

- Traversing the whole graph when a small neighborhood would answer the question.
