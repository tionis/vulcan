---
name: properties-and-tags
description: Query and refactor structured metadata such as properties and tags.
tools:
  - query
  - ls
  - refactor_rename_property
  - refactor_merge_tags
  - doctor
require_confirmation: false
---

## When to use

Use this skill when the task depends on frontmatter consistency, property queries, or tag cleanup.

## Core patterns

- Inspect first with `query` or `ls --where`.
- Run `doctor` if type mismatches or unsupported syntax might be involved.
- Use refactor commands with `--dry-run` before bulk edits.

## Common mistakes

- Treating free text as if it were indexed structured metadata.
