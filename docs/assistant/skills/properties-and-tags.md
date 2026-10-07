---
name: properties-and-tags
description: Query and refactor structured metadata such as properties and tags.
version: 2
tools:
  - query
  - ls
  - refactor_rename_property
  - refactor_merge_tags
  - doctor
metadata:
  vulcan:
    managed: true
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
- Before setting properties on records of an mdbase collection, read the type's shape with `vulcan mdbase schema <type>... --output json`: it lists each field with its declaring types, whether persisted frontmatter requires it (a read default does not satisfy that), its read default, and whether it is `editable`. Do not write fields with `editable: false`; the write pipeline generates them or the schema marks them read-only. A non-empty `conflicts` list means that type composition cannot be written until the type files are fixed.
- `update` and `unset` preflight every selected mdbase record and commit managed records through one validated journal batch. Treat collection validation errors as blockers; an ordinary metadata request never implies raw repair or direct YAML/filesystem bypass.
- A managed `concurrent_modification` means the source changed after selection. Rerun the query, review the current values, and form a new mutation instead of forcing the stale batch.
- Ordinary `update`, `unset`, property rename, and tag merge workflows refuse a pending ordinary-write journal before preview or apply. Inspect it with `vulcan repair ordinary-write status`; do not bypass the check with raw YAML edits.

## Example Moves

- Set one property across a filtered project set with `update`.
- Remove a stale property with `unset` after verifying the candidate notes.
- Merge an old tag into a canonical tag and follow with a query to confirm the result.
