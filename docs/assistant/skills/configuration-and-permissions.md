---
name: configuration-and-permissions
description: Configure Vulcan safely, manage device-local wiki registrations and groups, inspect settings, manage permission profiles, and understand trust boundaries. Use when the user asks about registered vaults, config, permissions, profiles, access control, sandboxing, trust, setup, or why a command/tool is denied.
version: 3
tools:
  - config_show
  - config_get
  - config_set
  - config_list
  - trust
  - help
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Configuration and Permissions

## When to Use This Skill

Use this skill when a task changes Vulcan settings, explains effective configuration, adjusts
permission profiles, or diagnoses permission and trust failures.

## Recommended Flow

1. Inspect before editing: `vulcan config show`, `vulcan config get <key>`, or `vulcan config list`.
2. Prefer dedicated config subcommands over manual TOML edits.
3. Use `--target local` for machine-specific secrets, paths, or credentials.
4. Use permission profiles to narrow assistant/MCP authority instead of relying on prompt text.
5. Check trust separately from permissions when JS, plugins, or skill command tools fail to run.
6. Use `vulcan config import folder-notes --preview` to inspect an Obsidian Folder Notes convention before applying it. Folder-note structure is shared repository state, so the importer rejects a local target.
7. Use `vulcan vault add/list/show/set/remove` for device-local wiki registration. Registration is optional and does not initialize, modify, synchronize, or delete the materialized vault.

## Guardrails

- Do not put private credentials in shared `.vulcan/config.toml`; use local config or environment variables.
- Keep assistant-facing profiles narrow. Add only the read/write/network/execute capabilities required by the workflow.
- A skill command can narrow authority with `permission_profile`; it cannot widen the caller's profile.
- Trust is an execution gate, not a permission profile. A trusted vault can still be denied by a profile.
- Importing folder-note settings configures the convention; it does not auto-detect or move existing folder notes. Use `vulcan refactor folder-notes --dry-run` for a layout conversion.
- `vulcan sync status` and `vulcan sync run` both inspect repository and remote state and therefore require the selected profile's Git permission; `--dry-run` prevents mutation but does not bypass that permission boundary.
- Preview registration mutations with `vulcan vault add ... --dry-run`, `vault set ... --dry-run`, or `vault remove ... --dry-run`. Removing a registration must never be treated as permission to delete its worktree or Git directory.

## Example Moves

- Explain why an MCP tool is hidden under `--permissions readonly`.
- Add a local web search backend key without changing shared vault config.
- Preview and import a shared folder-note convention, then separately plan any required layout conversion.
- Create a profile for a daily wiki agent with notes/tasks/search access but no shell or git mutation.
- Register personal and work wikis in separate local groups, then inspect their availability with `vulcan vault list`.
