---
name: note-operations
description: Read, create, append, and patch notes safely through Vulcan instead of raw filesystem edits.
version: 1
tools:
  - note_outline
  - note_get
  - note_info
  - note_create
  - note_set
  - note_append
  - note_patch
  - note_delete
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Note Operations

## When to Use This Skill

Use this skill when the task is centered on one note or a small set of notes and precision matters more than breadth.

## Recommended Flow

1. Start with `note outline` when the note might be long or structurally complex, then use `note get --section <id>` or `note get --heading <name>` to narrow the read.
2. Use `note info` for a quick metadata and link-count summary, or read the target with `note get` before patching so the agent is editing the right content.
3. Prefer `note append` for additive changes and `note patch --section|--heading|--block-ref|--lines` for surgical replacements inside one resolved scope.
4. Use `note set` only when replacing the whole note is intentional.
5. Switch to a vault-relative path when note names or aliases are ambiguous.

## Guardrails

- `note patch` fails on multiple matches by design. Narrow the scope with `--section`, `--heading`, `--block-ref`, or `--lines` instead of forcing a broad replacement.
- MCP `note_info` backlink and link-confidence counts include only readable source notes under the connection's permission profile; do not treat scoped counts as vault-wide totals.
- MCP `note_delete` previews list only backlinks from source notes the connection can currently read. A scoped preview is not proof that deleting the note leaves no other backlinks; inspect with broader authorized access when that matters.
- Prefer section, heading, block-ref, or `--match`-based targeting over whole-note rewrites.
- Keep frontmatter changes structured. If the task is really metadata work, use `update` or `unset` instead of editing YAML by hand.
- Note creates, replacements, appends, patches, and deletes targeting an mdbase record path use the collection's validated, journaled write boundary. Treat validation errors as blockers and do not bypass them with raw filesystem edits; explicit repair is a separate workflow that must be intentionally requested.
- On Android, mdbase journals, staging, receipts, and outbox live in per-vault Termux-private state. A write returns `unsupported_storage` before changing records when shared storage cannot sync canonical directories; keep the worktree canonical and use a supported filesystem for that write workflow.
- If a managed write reports `concurrent_modification`, reread the current note and rebuild the intended edit from its new revision. Never replay a stale whole-note replacement blindly.

## Example Moves

- Inspect the outline of a long note, read only the `decisions@42` section, then append a follow-up item under that heading.
- Patch one checklist item or one sentence without disturbing the rest of the note.
- Create a new note at a precise vault-relative path when several notes share the same filename.
