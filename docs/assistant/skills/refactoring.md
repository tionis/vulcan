---
name: refactoring
description: Safely rename aliases, headings, block refs, properties, and tags, move notes, and convert configured folder-note layouts across the vault. Use when a structural change must preserve resolved links or coordinate note moves with shared folder-note configuration.
version: 1
tools:
  - refactor_rename_alias
  - refactor_rename_heading
  - refactor_rename_block_ref
  - refactor_rename_property
  - refactor_merge_tags
  - refactor_folder_notes
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
- Use `vulcan refactor folder-notes --dry-run` before changing folder-note placement or naming. Review every planned move, overwrite or case-insensitive collision, and the resulting shared convention.
- Let the folder-note refactor update shared configuration after all moves succeed; do not move the files separately and patch config by hand.
- Inspect follow-up diagnostics or graph fallout after large rewrites.

## Guardrails

- Generic text replacement is the wrong tool for link-aware edits.
- Folder-note conversion is an exact configured-layout migration, not automatic convention detection. Supply explicit `--from-*` values when the effective repository config does not describe the source layout.
- A folder-note dry run must not change notes or configuration. Resolve every preflight conflict before applying it.
- Large refactors should be reviewed before commit, especially when many backlinks change.
- If the task is really metadata cleanup, use `update`, `unset`, or `merge-tags` instead of forcing it through a rewrite.

## Example Moves

- Rename one heading and let inbound heading links update safely.
- Move a note into a new folder while preserving inbound links.
- Convert `README.md` folder notes to index notes after reviewing the complete collision-safe move plan.
- Merge two tags after confirming that the destination tag is the canonical one.
