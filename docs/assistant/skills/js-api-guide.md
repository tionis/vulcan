---
name: js-api-guide
description: Orient an external harness around Vulcan's JS runtime and current sandbox boundaries.
tools:
  - help
  - describe
  - dataview_query
require_confirmation: false
---

## When to use

Use this skill when a workflow needs JavaScript-oriented guidance rather than direct CLI mutations.

## Core patterns

- Read `help js`, `help js.vault`, and `help sandbox` first.
- `vulcan run <file.js|script-name>` is available for read-oriented runtime workflows.
- Prefer stable CLI commands for write operations that the JS runtime does not expose yet.

## Common mistakes

- Assuming the full `vault.transaction()` and write API already exists everywhere.
- Assuming unrestricted network or shell access from the JS sandbox.
