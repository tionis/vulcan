---
name: summarize_note
title: Summarize Note
description: Return a lightweight local summary for one note.
runtime: quickjs
entrypoint: main.js
sandbox: fs
read_only: true
input_schema:
  type: object
  additionalProperties: false
  properties:
    note:
      type: string
      description: Vault-relative note path or resolvable note name.
  required:
    - note
output_schema:
  type: object
  additionalProperties: false
  properties:
    note:
      type: string
    title:
      type: string
    preview:
      type: string
    tool:
      type: string
  required:
    - note
    - title
    - preview
    - tool
---

## What This Example Shows

Use this example as a starting point for a read-only custom tool that works across CLI, MCP, and
`tools.call()`.

- Read a note through `vault.note(...)` instead of touching files directly.
- Return structured JSON in `result` plus a short human-readable `text` fallback.
- Keep reusable executable behavior in `.agents/tools` when it should be discoverable outside one
  skill or script.

## Suggested Next Steps

- Rename the tool to match the workflow it should automate.
- Tighten `input_schema` and `output_schema` once the interface stabilizes.
- Add `permission_profile`, `packs`, or `secrets` when the tool needs narrower access or remote
  credentials.
