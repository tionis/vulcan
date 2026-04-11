---
name: note-operations
description: Read, create, append, and patch notes safely through Vulcan instead of raw filesystem edits.
version: 1
tools:
  - note_outline
  - note_get
  - note_create
  - note_set
  - note_append
  - note_patch
require_confirmation: false
---

# Note Operations

## When to Use This Skill

Use this skill when the task is centered on one note or a small set of notes and precision matters more than breadth.

## Recommended Flow

1. Start with `note outline` when the note might be long or structurally complex, then use `note get --section <id>` or `note get --heading <name>` to narrow the read.
2. Read the target with `note get` before patching so the agent is editing the right content.
3. Prefer `note append` for additive changes and `note patch` for surgical replacements.
4. Use `note set` only when replacing the whole note is intentional.
5. Switch to a vault-relative path when note names or aliases are ambiguous.

## Guardrails

- `note patch` fails on multiple matches by design. Narrow the selector instead of forcing a broad replacement.
- Prefer section, heading, block-ref, or `--match`-based targeting over whole-note rewrites.
- Keep frontmatter changes structured. If the task is really metadata work, use `update` or `unset` instead of editing YAML by hand.

## Example Moves

- Inspect the outline of a long note, read only the `decisions@42` section, then append a follow-up item under that heading.
- Patch one checklist item or one sentence without disturbing the rest of the note.
- Create a new note at a precise vault-relative path when several notes share the same filename.
