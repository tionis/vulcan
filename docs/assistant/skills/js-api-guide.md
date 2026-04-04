---
name: js-api-guide
description: Orient an external harness around Vulcan's JS runtime and sandbox boundaries.
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
- Start from `vulcan run --sandbox strict` for read-only scripts.
- Escalate to `--sandbox fs` for vault writes and `--sandbox net` for web helpers.
- Prefer `vault.transaction()` when several writes must succeed or roll back together.

## Common mistakes

- Assuming write helpers work without `--sandbox fs` or higher.
- Assuming unrestricted network or shell access from the JS sandbox.
