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
- Prefer stable CLI commands when the standalone `vulcan run` runtime has not landed yet.
- Treat DataviewJS and Templater JS as the currently available runtime surfaces.

## Common mistakes

- Assuming the full `vault.transaction()` and write API already exists everywhere.
- Assuming unrestricted network or shell access from the JS sandbox.
