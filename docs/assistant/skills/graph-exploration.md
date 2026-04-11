---
name: graph-exploration
description: Explore backlinks, hubs, paths, and connectivity in the vault graph.
version: 1
tools:
  - backlinks
  - links
  - graph_path
  - graph_hubs
  - graph_components
require_confirmation: false
---

# Graph Exploration

## When to Use This Skill

Use this skill when note relationships matter more than raw content matching.

## Recommended Flow

- Start with `links` or `backlinks` for one note before jumping to vault-wide analytics.
- Use graph path when the task is about how two notes connect.
- Use hubs, dead ends, or components when the task is about structure across the vault.
- Combine graph inspection with `query` when topology alone is not enough.

## Guardrails

- Avoid traversing the whole graph when a small neighborhood answers the question.
- Graph tools describe resolved note relationships, not arbitrary text mentions.
- If the target note is ambiguous, resolve that first or the graph result will be misleading.

## Example Moves

- Start from a project note and inspect its backlinks before looking for hubs.
- Explain how two concepts connect with `graph path`.
- Find dead-end notes, then filter them to one area of the vault with a query.
