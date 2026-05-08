---
name: link-curation
description: Review, rank, accept, and reject inferred link suggestions safely.
version: 1
tools:
  - suggest_links
  - graph_communities
  - graph_path
  - backlinks
  - links
require_confirmation: true
---

# Link Curation

## When to Use This Skill

Use this skill when the task is to improve graph connectivity without blindly inserting links.

## Recommended Flow

- Start with `suggest links --note <path>` when a specific note feels isolated.
- Use `graph communities --orphans` to find orphan notes and the closest topic cluster.
- Review the score breakdown before accepting a suggestion. Prefer links with multiple signals.
- Accept only suggestions that make semantic sense in the source vault.
- Reject noisy suggestions so future ranking deprioritizes the same pair.

## Guardrails

- Do not treat inferred links as equivalent to extracted Markdown links.
- Do not accept suggestions just because the score is high.
- Preserve the vault as source of truth: inferred edges are cache-backed unless a user explicitly asks for Markdown edits.

## Example Moves

- Discover ranked suggestions for an orphan note, accept one, then verify the inferred edge appears in `graph path`.
- Find cross-community bridge candidates and reject low-quality pairs after inspecting backlinks.

