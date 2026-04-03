---
name: note-operations
description: Read, create, append, and patch notes safely.
tools:
  - note_get
  - note_create
  - note_set
  - note_append
  - note_patch
require_confirmation: false
---

## When to use

Use this skill when the task is centered on one note or a small set of notes and precision matters more than breadth.

## Core patterns

- Start with `note get` before changing content.
- Prefer `note append` or `note patch` over `note set` whenever only part of a note should change.
- Use `note patch --check` or dry-run-oriented flows before writing.

## Common mistakes

- Replacing the whole note with `note set` when a targeted append or patch is safer.
- Ignoring multiple-match failures from `note patch`.
