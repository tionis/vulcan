---
name: refactoring
description: Safely rename aliases, headings, block refs, properties, and tags across the vault.
version: 1
tools:
  - refactor_rename_alias
  - refactor_rename_heading
  - refactor_rename_block_ref
  - refactor_rename_property
  - refactor_merge_tags
  - move
require_confirmation: false
---

# Refactoring

## When to Use This Skill

Use this skill for coordinated vault-wide rewrites where link safety matters.

## Recommended Flow

- Start with `--dry-run` whenever the command offers it.
- Use the most specific refactor command available instead of a generic text replacement.
- Prefer link-aware operations like `move` and `rename-*` over raw search-and-replace.
- Inspect follow-up diagnostics or graph fallout after large rewrites.

## Guardrails

- Generic text replacement is the wrong tool for link-aware edits.
- Large refactors should be reviewed before commit, especially when many backlinks change.
- If the task is really metadata cleanup, use `update`, `unset`, or `merge-tags` instead of forcing it through a rewrite.

## Example Moves

- Rename one heading and let inbound heading links update safely.
- Move a note into a new folder while preserving inbound links.
- Merge two tags after confirming that the destination tag is the canonical one.
