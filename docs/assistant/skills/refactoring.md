---
name: refactoring
description: Safely rename aliases, headings, block refs, properties, and tags across the vault.
tools:
  - refactor_rename_alias
  - refactor_rename_heading
  - refactor_rename_block_ref
  - refactor_rename_property
  - refactor_merge_tags
  - move
require_confirmation: false
---

## When to use

Use this skill for coordinated vault-wide rewrites where link safety matters.

## Core patterns

- Always start with `--dry-run`.
- Use the most specific refactor command rather than a plain text rewrite.
- Re-scan or inspect follow-up diagnostics if a rename touches many notes.

## Common mistakes

- Using generic search-and-replace for link-aware operations.
