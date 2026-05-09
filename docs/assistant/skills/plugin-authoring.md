---
name: plugin-authoring
description: Create, inspect, and troubleshoot Vulcan JavaScript lifecycle plugins. Use when the user asks about plugins, event hooks, on_note_write, on_pre_commit, plugin permissions, plugin trust, or when to use a plugin instead of a skill command.
version: 1
tools:
  - plugin
  - trust
  - config_show
  - help
require_confirmation: false
---

# Plugin Authoring

## When to Use This Skill

Use this skill when behavior should run because a Vulcan lifecycle event happened, not because a
human or LLM explicitly invoked a request/response command.

## Recommended Flow

- Use `vulcan plugin list` to inspect registered and discovered plugins.
- Use `vulcan plugin enable`, `set`, or `disable` to manage registrations.
- Use `vulcan plugin run <name>` to test a plugin manually.
- Keep plugin code in `.vulcan/plugins/` unless the config points elsewhere.
- Use `help js.plugins`, `help js.host`, and `help sandbox` for runtime boundaries.

## Guardrails

- Use a skill command, not a plugin, for directly callable automation.
- Plugins require vault trust before execution.
- Blocking hooks such as pre-commit/write hooks should fail with clear, actionable messages.
- Prefer `host.exec(argv)` over `host.shell(command)` when host execution is genuinely needed.
- Keep permissions narrow; plugins should not run with broad shell/network access by default.

## Example Moves

- Add a pre-commit plugin that rejects malformed generated notes.
- Debug why an `on_note_write` hook did not run.
- Convert a manually-invoked plugin idea into a skill command when direct invocation is the better model.
