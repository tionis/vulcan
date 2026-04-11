---
name: properties-and-tags
description: Query and refactor structured metadata such as properties and tags.
version: 1
tools:
  - query
  - ls
  - refactor_rename_property
  - refactor_merge_tags
  - doctor
require_confirmation: false
---

# Properties And Tags

## When to Use This Skill

Use this skill when the task depends on frontmatter consistency, property queries, or tag cleanup.

## Recommended Flow

- Inspect first with `query` or `ls --where` so the write scope is explicit.
- Use `update` and `unset` for bulk property changes instead of editing YAML by hand.
- Use `merge-tags` for tag normalization when the tag appears in bodies and frontmatter.
- Run `doctor` when type mismatches or parser edge cases may be involved.

## Guardrails

- Do not treat free text as if it were indexed structured metadata.
- Bulk metadata changes should be tested with `--dry-run` when available.
- Ambiguous note selection is a data-quality problem. Resolve that before mutating properties.

## Example Moves

- Set one property across a filtered project set with `update`.
- Remove a stale property with `unset` after verifying the candidate notes.
- Merge an old tag into a canonical tag and follow with a query to confirm the result.
