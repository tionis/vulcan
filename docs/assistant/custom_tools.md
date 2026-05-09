# Skill Command Tools

Vulcan's preferred model for user-defined callable automation is now
Agent Skills-compatible skill commands.

Define tools as commands under `.agents/skills/<skill-name>/SKILL.md` in
`metadata.vulcan.commands` with `expose: true`. `vulcan tool list/show/run`,
MCP, `describe`, and JS `tools.call()` all use that same exposed command registry.

See [skill_commands.md](skill_commands.md).
