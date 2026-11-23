---
name: note-operations
description: Read, create, append, and patch notes safely through Vulcan instead of raw filesystem edits.
version: 10
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
   - `note get` returns the note source, which is what patches must match. Add `--evaluate` only to see dynamic content (Dataview, Tasks, Bases embeds) as results; never patch against evaluated output.
3. Prefer `note append` for additive changes and `note patch --section|--heading|--block-ref|--lines` for surgical replacements inside one resolved scope. `note append --dry-run` (MCP `dry_run: true`) returns the resulting note as `content` without writing it or creating a missing periodic note; combine it with `--check` to see diagnostics first.
4. Use `note set` only when replacing the whole note is intentional. To move or rename a note, use `note rename`/`refactor move` (MCP `note_move` with `note` and `destination`, `dry_run` first): links and embeds pointing at it are rewritten in the same operation, which a delete-and-create would break.
   - Guard it with the `content_hash` that `note get` returned: MCP `expected_hash` (CLI `--expected-hash`) on `note_set`, `note_patch`, `note_append`, or `note_delete` refuses with `stale_content` if anyone changed the note since your read, instead of overwriting their edit. On `stale_content`, reread and rebuild the edit.
   - A hosted client that may retry a lost response should give `note_append` an `idempotency_key`; repeating the same append with the same key returns the first result (`replayed: true`) without appending twice. Reusing a key for a different append is an error.
5. Switch to a vault-relative path when note names or aliases are ambiguous.
6. For attachments and other non-Markdown files over MCP, use `file_get` (bounded slices as UTF-8 or base64, with the whole file's `content_hash`; follow `next_offset`) and `file_put` (text or base64, at most 10 MiB; refuses Markdown, which belongs to the note tools, and refuses to replace an existing file unless `overwrite` is set, ideally with `expected_hash`). Embed a stored attachment in a note with `![[path]]`.

## Guardrails

- `note patch` fails on multiple matches by design. Narrow the scope with `--section`, `--heading`, `--block-ref`, or `--lines` instead of forcing a broad replacement.
- MCP `note_append.text` is literal, including `"-"`. The local CLI alone treats a `-` text argument as a request to read stdin.
- MCP `note_info` backlink and link-confidence counts include only readable source notes under the connection's permission profile; do not treat scoped counts as vault-wide totals.
- MCP `note_delete` previews list only backlinks from source notes the connection can currently read. A scoped preview is not proof that deleting the note leaves no other backlinks; inspect with broader authorized access when that matters.
- Prefer section, heading, block-ref, or `--match`-based targeting over whole-note rewrites.
- Keep frontmatter changes structured. If the task is really metadata work, use `update` or `unset` instead of editing YAML by hand.
- Note creates, replacements, appends, patches, and deletes targeting an mdbase record path use the collection's validated, journaled write boundary. Treat validation errors as blockers and do not bypass them with raw filesystem edits; explicit repair is a separate workflow that must be intentionally requested.
- Routing vault note writes requires read authority for `mdbase.yaml`, even when that file is absent or the target is an ordinary note. A denied routing control is not evidence that a collection exists. Request an authorized profile change if needed; do not substitute the default profile or use a raw write to bypass the check.
- Managed mdbase batches validate the proposed final state: duplicate values and broken declared incoming links can reject the entire edit before any file changes. A valid unique-value swap must be submitted together in one supported batch, not as sequential writes. `stale_state` means a validation dependency changed; reread and rebuild the preview instead of reusing it.
- Managed mdbase create/update policies may generate IDs, timestamps, or other fields before validation. Read the resulting note rather than assuming the submitted frontmatter was persisted unchanged. Previews retain the generated values for apply/replay; a new preview may generate new values. A no-op policy preserves exact source; changed fields may reserialize frontmatter while preserving the body. `lifecycle_event_unsupported` blocks an optional rename/delete hook; it is not permission to bypass the managed write boundary.
- To move many notes, pipe `source<TAB>destination` lines into `vulcan refactor move --stdin` instead of looping over `vulcan move`: the whole batch is checked first (malformed lines, repeated sources or destinations), moves run in order with one auto-commit, and the run stops at the first failed move, reporting which pair failed and that the earlier moves stayed applied. A `--dry-run` batch previews each pair against the current vault, so it cannot preview a pair that depends on an earlier one.
- Refactors, `refactor split`, `refactor folder-notes`, Kanban card edits, language-server edits to closed notes, `web fetch --save`, and bot attachments use the same validated boundary whenever they touch a record, and commit ordinary notes they rewrite in the same transaction. A rejection means nothing was written: fix the input or the record rather than editing files directly. Outline pull and artifact, TextBundle, or wiki-package imports refuse with `mdbase_collection` instead of writing a record; pick a destination outside the collection, or ask the owner to add it to `settings.exclude`.
- Moving or renaming an mdbase collection record (`vulcan move`, `note rename`, `refactor move`) commits the move and every rewritten reference as one validated mdbase rename under the selected profile. A validation or permission failure leaves every file unchanged and is not retried as an ordinary move; fix the cause instead of moving the file by hand.
- On Android, mdbase journals, staging, receipts, and outbox live in per-vault Termux-private state. A write returns `unsupported_storage` before changing records when shared storage cannot sync canonical directories; keep the worktree canonical and use a supported filesystem for that write workflow.
- If a managed write reports `concurrent_modification`, reread the current note and rebuild the intended edit from its new revision. Never replay a stale whole-note replacement blindly.
- If an ordinary `note set`, `note append`, `note patch`, or `note delete` reports that the note changed during editing, reread it and rebuild the edit; Vulcan leaves the newer content untouched. A create or periodic append also refuses to replace a file created concurrently.
- If `note get`, `note outline`, or `note info` reports a pending ordinary-write journal through the CLI or MCP, stop reading that vault until `vulcan repair ordinary-write status` shows the transaction has recovered or been explicitly reconciled. These reads are blocked to avoid showing only one side of an interrupted task conversion or archive move.
- The vault may be open in an editor running `vulcan lsp`. A note's file reflects its last save, not unsaved buffer text. Vulcan note writes stay safe (vault lock, content checks) and the editor reloads the file, but an editor holding unsaved changes to the same note will conflict when the user saves. Do not edit a note the user is actively editing; ask them to save first.
- Ordinary note create, replacement, append, patch, and delete also refuse a pending ordinary-write journal, even for an unrelated note. Repair the journal before retrying the edit; do not bypass the guard with a raw file write.

## Example Moves

- Inspect the outline of a long note, read only the `decisions@42` section, then append a follow-up item under that heading.
- Patch one checklist item or one sentence without disturbing the rest of the note.
- Create a new note at a precise vault-relative path when several notes share the same filename.
