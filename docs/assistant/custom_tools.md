# Skill Command Tools

Vulcan's preferred model for user-defined callable automation is now
Agent Skills-compatible skill commands.

Define tools as commands under `.agents/skills/<skill-name>/SKILL.md` in
`metadata.vulcan.commands` with `expose: true`. `vulcan tool list/show/run`,
MCP, `describe`, and JS `tools.call()` all use that same exposed command registry.

Use `vulcan tool init <alias>` for the starter scaffold. Add `--template
reader`, `mutation`, `exporter`, or `wrapper` when a more specific starting
point is useful. Use `vulcan tool lint` for authoring checks, and `vulcan tool
test` for declared examples. `tool lint --fix`
only applies safe packaging repairs such as shebang normalization and executable
bits; mutation-capable tools should expose a boolean dry-run input and at least
one dry-run example. Examples may use inline `input`, `cli_args`, or fixture
files with `input_file` and `expected_output_file`; expected-output mismatches
report JSON path diffs. Use `vulcan tool test --all` to run every exposed
tool's examples in a vault, and use `--update-expected` to refresh
`expected_output_file` snapshots from the current output.

Use `vulcan tool test <alias> --profile <permission-profile>` to check the
examples under the same permission profile exposed to an MCP or external agent
caller. Use `vulcan tool compat <alias> --surface cli,mcp,openai-tools,js` to
check surface-specific schema, CLI, sandbox, and callability requirements.

Inside JavaScript tool scripts, prefer `tool.input(defaults)` for normalized
input, `vault.plan({ dry_run })` for reviewable mutation plans, `tool.result()`
for structured output, `vulcan.permissions()` for adaptive permission checks, and
`tools.callChecked()` when composing other tools.

See [skill_commands.md](skill_commands.md).
