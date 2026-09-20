---
name: js-api-guide
description: Orient an external harness around Vulcan's JS runtime and sandbox boundaries.
version: 2
tools:
  - help
  - describe
  - dataview_query
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# JS API Guide

## When to Use This Skill

Use this skill when the workflow genuinely needs scripting or multi-step logic rather than one direct CLI command.

## Recommended Flow

- Read `help js`, `help js.contract`, `help js.vault`, and `help sandbox` before writing runtime code.
- Start from `vulcan run --sandbox strict` for pure computation or read-only inspection.
- Escalate to `--sandbox fs` for vault writes and `--sandbox net` for web helpers.
- Use `--sandbox none` only for a trusted local script that genuinely needs `host.exec()` or
  `host.shell()`; it removes runtime resource limits and still requires explicit execute or
  execute-plus-shell permission.
- Use `vault.transaction()` when several note mutations must succeed or roll back together.
- Use `vault.plan({ dry_run })` for custom tools that need reviewable changed paths, diffs, and dry-run/apply behavior.
- Use `vulcan.permissions()` before optional writes, `tool.result()` for structured returns, and `tools.callChecked()` when composing other tools.
- Check `docs/reference/js-api/contract.json` when a harness needs a stable namespace inventory instead of prose examples.

## Guardrails

- Prefer stable CLI commands when they already solve the task cleanly. The JS runtime is for workflows the CLI does not express well.
- If reusable executable behavior should be callable from CLI, MCP, and other scripts, declare it as a skill command in `metadata.vulcan.commands` with `expose: true`, then call it through `tools.call(...)`.
- For write-capable custom tools, prefer `tool.input(defaults)`, `vault.plan(...)`, and `tool.result()` over ad hoc JSON envelopes.
- Write helpers do not work below `fs`, and web helpers do not work below `net`.
- `host.exec()` and `host.shell()` require `none`; projected skill commands cannot declare that
  sandbox. Prefer `host.exec()` over shell parsing on eligible runtime surfaces.
- In an mdbase collection, each standalone write is an implicit validated commit, while `vault.transaction()` sends the complete proposed change set through one journal batch. A validation failure restores every original and creates no write journal; fix the proposed records instead of bypassing the transaction.
- Treat the sandbox and permission profile as intersecting boundaries. Neither one widens the
  other, and trust is a separate execution gate for vault-owned code.

## Example Moves

- Gather notes with one query, compute a derived table in JS, then write a summary note in one transaction.
- Run a read-only script in `strict` mode to inspect graph or metadata patterns.
- Use `net` only when the workflow truly combines web retrieval with vault processing.
