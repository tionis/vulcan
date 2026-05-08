# JS Skills Namespace

The `skills` namespace is a convenience layer over the shared tool registry for Agent Skills-compatible command packages.

Skill commands declared in `.agents/skills/<name>/SKILL.md` under `metadata.vulcan.commands` are projected as tool names such as `skill_daily_review_prepare_day`. They are also available from JavaScript through `skills`.

Available entrypoints:

- `skills.list()` lists visible projected skill-command tools.
- `skills.get(toolName)` reads one projected command definition and skill documentation.
- `skills.commands(skillName?)` lists projected commands, optionally scoped to one skill.
- `skills.run(skillName, commandId, input, opts?)` invokes one command through the same permission and sandbox ceiling used by `tools.call()`.

Use `skills.run()` when code already knows the skill and command IDs. Use `tools.call()` when code is operating on normalized registry tool names returned by `tools.list()` or MCP.

See also:

- `help skill-commands`
- `help js.tools`
- `help automation-surfaces`

