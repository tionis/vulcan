# JS API Contract

Contract: `vulcan-js-api`
Version: `1`
Status: experimental

This is the versioned reference point for Vulcan's JavaScript runtime surface. The prose topics under `help js.*` explain behavior and examples; `contract.json` is the compact machine-readable inventory for harnesses, skills, and docs checks.

The contract is checked in with the reference docs and should be updated in the same commit as runtime namespace changes. Treat a removal, rename, argument shape change, or stricter permission requirement as a contract change.

## Stability Rules

- `active`: implemented and expected to remain source-compatible inside this contract version.
- `planned`: documented design that should not be used as a hard dependency yet.
- `experimental`: available, but callers should keep usage narrow and prefer CLI commands when possible.

## Current Surfaces

| Namespace | Stability | Minimum sandbox | Help topic | Purpose |
| --- | --- | --- | --- | --- |
| `vault` | active | `fs` for writes, `strict` for read helpers backed by indexed data | `js.vault` | Vault reads, search/query helpers, periodic helpers, graph helpers, and mutation plans. |
| `tools` | active | `strict` | `js.tools` | Discovery and invocation of exposed skill command tools. |
| `skills` | active | `strict` | `js.skills` | Agent Skills-compatible command discovery and invocation. |
| `tool` | active | `strict` | `custom-tools` | Custom tool input, result, progress, confirmation, and audit helpers. |
| `web` | active | `net` | `web` | Permission-gated search and fetch helpers. |
| `host` | active | `none` plus execute permission | `js.host` | Permission-gated process execution helpers. |
| `vulcan` | active | `strict` | `js` | Runtime metadata, permissions, dates, and scratch helpers. |
| `help` | active | `strict` | `js` | Runtime introspection for objects and namespaces. |

## Update Checklist

1. Update runtime implementation and tests.
2. Update the relevant `docs/reference/js-api/*.md` topic.
3. Update `docs/reference/js-api/contract.json` and this page if the namespace inventory or stability changes.
4. Update `docs/assistant/skills/js-api-guide.md` when agent-facing guidance changes.
5. Run `vulcan help js.contract` and the CLI help tests.
