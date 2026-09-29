---
name: templates-and-capture
description: Create notes from templates, insert template content, use inbox capture, and reason about Templater/QuickAdd compatibility. Use when the user asks about templates, capture, inbox entries, QuickAdd formats, Templater tags, or note scaffolding.
version: 1
tools:
  - template
  - inbox
  - note_create
  - note_append
  - help
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Templates and Capture

## When to Use This Skill

Use this skill for note creation workflows where structure, variables, capture location, or template
compatibility matter.

## Recommended Flow

- Use `vulcan template list` and `vulcan template show` before assuming a template exists.
- Use `vulcan template create` for new notes and `vulcan template insert` for existing notes.
- Use `vulcan inbox` or `note append` for quick additive capture.
- Use `template preview` when variables, Templater tags, or QuickAdd tokens may produce surprising output.
- Import Obsidian template/QuickAdd/Templater settings before expecting compatibility defaults.
- Check `templates.trigger_on_file_creation` and its mode before assuming a newly created note is empty. `note create` applies configured creation triggers automatically; an explicit `--template` takes precedence.
- Keep `vulcan watch` running when creation triggers must also apply to Markdown files created by editors or other filesystem tools.

## Guardrails

- Do not overwrite an existing note when insertion or append is the intended workflow.
- Preview templates that include JS, dates, or user variables.
- If `template list`, `template show`, or `template preview` reports a pending ordinary-write journal, inspect it with `vulcan repair ordinary-write status` before reading further. Direct CLI and MCP template reports refuse a partially published multi-note change until recovery or explicit reconciliation.
- During `note create` (including MCP) with an explicit template or creation trigger, direct `template create`/`insert`, and a creation trigger applied to an existing ordinary note, native and JS `tp.file.create_new` companions publish with the final ordinary note in one recoverable batch. A collision, stale final note, or managed-record side effect leaves those staged files unpublished.
- During `template insert` and creation triggers on existing ordinary notes, native and JS `tp.file.move`/`rename` also stage the move and backlink rewrites in that batch. Repeated moves publish only the final destination and preserve note identity through recovery. `tp.file.exists`/`include` see staged files while rendering. A failed render or late edit publishes none of those staged effects; plugin/system-command effects and generic standalone rendering remain outside this transaction.
- A new-note creation cannot move an existing note encountered at its requested path; use `template insert` or an explicit move workflow for an existing note. Retargeting a genuinely new note remains supported. Ordinary batches are bounded to 32 changed paths; a larger template move fails instead of publishing a partial backlink rewrite.
- Templater `tp.file.move` and `tp.file.rename` refuse a pending ordinary-write journal, including a dry-run move preview. Repair the batch first rather than retrying or removing its journal.
- Keep capture append-only unless the user explicitly asks to reorganize captured material.
- Mutating Templater helpers may require sandbox/permission checks and should not be assumed safe.
- For profile-scoped `note create`, check every path a template may create, move, rename, or rewrite; the connection grant is enforced on those side effects and on the final note path.
- `tp.file.create_new` refuses an existing destination and cannot create a managed mdbase collection record through the ordinary template path; use the collection's validated write workflow instead.
- A creation trigger rejects a note that changed after template rendering; reread the note before retrying instead of forcing the rendered content over the newer edit.
- `template create` refuses a destination created during rendering, and `template insert` rejects a note edited since it was read; inspect the current files before retrying either command.
- Creation triggers are mutations and may execute Templater JS. Keep them disabled unless requested, and inspect folder/regex mappings plus ignored folders before enabling them.

## Example Moves

- Create a project note from a configured template with explicit frontmatter.
- Append a quick inbox item using QuickAdd-style variables.
- Preview a Templater template against a target note before inserting it.
