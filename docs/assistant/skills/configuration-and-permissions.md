---
name: configuration-and-permissions
description: Configure Vulcan safely, inspect settings, manage permission profiles, and understand trust boundaries. Use when the user asks about config, permissions, profiles, access control, sandboxing, trust, setup, or why a command/tool is denied.
version: 1
tools:
  - config_show
  - config_get
  - config_set
  - config_list
  - trust
  - help
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

## Guardrails

- Do not put private credentials in shared `.vulcan/config.toml`; use local config or environment variables.
- Keep assistant-facing profiles narrow. Add only the read/write/network/execute capabilities required by the workflow.
- A skill command can narrow authority with `permission_profile`; it cannot widen the caller's profile.
- Trust is an execution gate, not a permission profile. A trusted vault can still be denied by a profile.

## Example Moves

- Explain why an MCP tool is hidden under `--permissions readonly`.
- Add a local web search backend key without changing shared vault config.
- Create a profile for a daily wiki agent with notes/tasks/search access but no shell or git mutation.
