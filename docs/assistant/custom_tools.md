# Skill Command Tools

Vulcan's preferred model for user-defined callable automation is now
Agent Skills-compatible skill commands.

Define tools as commands under `.agents/skills/<skill-name>/SKILL.md` in
`metadata.vulcan.commands` with `expose: true`. `vulcan tool list/show/run`,
MCP, `describe`, and JS `tools.call()` all use that same exposed command registry.

Use `vulcan tool init <alias>` for the starter scaffold, `vulcan tool lint` for
authoring checks, and `vulcan tool test` for declared examples. Examples may use
inline `input`, `cli_args`, or fixture files with `input_file` and
`expected_output_file`; expected-output mismatches report JSON path diffs.

See [skill_commands.md](skill_commands.md).
