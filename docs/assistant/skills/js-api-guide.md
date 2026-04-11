---
name: js-api-guide
description: Orient an external harness around Vulcan's JS runtime and sandbox boundaries.
version: 1
tools:
  - help
  - describe
  - dataview_query
require_confirmation: false
---

# JS API Guide

## When to Use This Skill

Use this skill when the workflow genuinely needs scripting or multi-step logic rather than one direct CLI command.

## Recommended Flow

- Read `help js`, `help js.vault`, and `help sandbox` before writing runtime code.
- Start from `vulcan run --sandbox strict` for pure computation or read-only inspection.
- Escalate to `--sandbox fs` for vault writes and `--sandbox net` for web helpers.
- Use `vault.transaction()` when several note mutations must succeed or roll back together.

## Guardrails

- Prefer stable CLI commands when they already solve the task cleanly. The JS runtime is for workflows the CLI does not express well.
- Write helpers do not work below `fs`, and web helpers do not work below `net`.
- Treat the sandbox boundary as real. Do not assume unrestricted shell or network access.

## Example Moves

- Gather notes with one query, compute a derived table in JS, then write a summary note in one transaction.
- Run a read-only script in `strict` mode to inspect graph or metadata patterns.
- Use `net` only when the workflow truly combines web retrieval with vault processing.
