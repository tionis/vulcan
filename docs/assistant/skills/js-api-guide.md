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
- Use `vault.plan({ dry_run })` for custom tools that need reviewable changed paths, diffs, and dry-run/apply behavior.
- Use `vulcan.permissions()` before optional writes, `tool.result()` for structured returns, and `tools.callChecked()` when composing other tools.

## Guardrails

- Prefer stable CLI commands when they already solve the task cleanly. The JS runtime is for workflows the CLI does not express well.
- If reusable executable behavior should be callable from CLI, MCP, and other scripts, declare it as a skill command in `metadata.vulcan.commands` with `expose: true`, then call it through `tools.call(...)`.
- For write-capable custom tools, prefer `tool.input(defaults)`, `vault.plan(...)`, and `tool.result()` over ad hoc JSON envelopes.
- Write helpers do not work below `fs`, and web helpers do not work below `net`.
- Treat the sandbox boundary as real. Do not assume unrestricted shell or network access.

## Example Moves

- Gather notes with one query, compute a derived table in JS, then write a summary note in one transaction.
- Run a read-only script in `strict` mode to inspect graph or metadata patterns.
- Use `net` only when the workflow truly combines web retrieval with vault processing.
