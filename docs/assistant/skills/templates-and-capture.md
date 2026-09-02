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
- Keep capture append-only unless the user explicitly asks to reorganize captured material.
- Mutating Templater helpers may require sandbox/permission checks and should not be assumed safe.
- Creation triggers are mutations and may execute Templater JS. Keep them disabled unless requested, and inspect folder/regex mappings plus ignored folders before enabling them.

## Example Moves

- Create a project note from a configured template with explicit frontmatter.
- Append a quick inbox item using QuickAdd-style variables.
- Preview a Templater template against a target note before inserting it.
