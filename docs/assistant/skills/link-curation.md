---
name: link-curation
description: Review, rank, accept, and reject inferred link suggestions safely.
version: 3
tools:
  - link_suggestions
  - link_suggestion_resolve
  - graph_communities
  - graph_path
  - backlinks
  - links
metadata:
  vulcan:
    managed: true
require_confirmation: true
---

# Link Curation

## When to Use This Skill

Use this skill when the task is to improve graph connectivity without blindly inserting links.

## Recommended Flow

<!-- vulcan:cli -->
- Start with `suggest links --note <path>` when a specific note feels isolated.
- Use `graph communities --orphans` to find orphan notes and the closest topic cluster.
- Review the score breakdown before accepting a suggestion. Prefer links with multiple signals.
- Accept only suggestions that make semantic sense in the source vault.
- Reject noisy suggestions so future ranking deprioritizes the same pair. Accept and reject decisions persist on this device across cache rebuilds and note renames, so there is no need to repeat them after a reindex.
<!-- /vulcan:cli -->
<!-- vulcan:mcp -->
- Start with `link_suggestions` with `note` when a specific note feels isolated.
- Use `graph_communities` with `orphans: true` to find orphan notes and the closest topic cluster.
- Review the score breakdown before accepting a suggestion; prefer links with several signals.
- Accept or reject with `link_suggestion_resolve` (`id`, `action`). Accept only suggestions that make sense in the vault, and reject noisy ones so ranking learns; decisions persist on this device across reindexes and renames.
- Accepting records the decision; write the link into the note with `note_patch` or `note_append` when it should appear in the text.
<!-- /vulcan:mcp -->

## Guardrails

- Do not treat inferred links as equivalent to extracted Markdown links.
- Do not accept suggestions just because the score is high.
- Preserve the vault as source of truth: inferred edges are cache-backed unless a user explicitly asks for Markdown edits.

## Example Moves

- Discover ranked suggestions for an orphan note, accept one, then verify the inferred edge appears in `graph path`.
- Find cross-community bridge candidates and reject low-quality pairs after inspecting backlinks.
