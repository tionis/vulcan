---
name: dataview-and-bases
description: Work with Dataview, DataviewJS, Bases, and .base files. Use when the user asks about Dataview DQL, inline fields, DataviewJS blocks, Bases views, formulas, saved task views, or .base editing/evaluation.
version: 1
tools:
  - dataview
  - bases
  - query
  - help
require_confirmation: false
---

# Dataview and Bases

## When to Use This Skill

Use this skill for Obsidian Dataview compatibility, `.base` files, formula/view troubleshooting,
and translating view logic into Vulcan queries.

## Recommended Flow

- Use `vulcan dataview query` for DQL strings and `vulcan dataview eval` for indexed Dataview blocks.
- Use `vulcan dataview query-js` only when JavaScript behavior matters.
- Use `vulcan bases eval <file>` to inspect `.base` output and diagnostics.
- Use shared `query` when the workflow does not require Dataview-specific syntax.
- Read diagnostics before editing view definitions; unsupported syntax should surface as diagnostics.

## Guardrails

- Prefer canonical `query` for agent workflows unless the user specifically needs Dataview/Bases compatibility.
- DataviewJS runs inside Vulcan's JS sandbox; write/network helpers depend on sandbox and permissions.
- `.base` edits should preserve view structure and formulas; avoid broad text rewrites.

## Example Moves

- Explain why a Dataview query returns different rows than a property query.
- Evaluate a `.base` file and patch one view filter.
- Convert a working DQL query into a reusable Vulcan query or saved report.
