# Skill commands

Skill commands are executable entrypoints declared by Agent Skills-compatible skill packages.

A skill lives under `.agents/skills/<name>/` and contains a required `SKILL.md`. The skill may include scripts, references, assets, schemas, and examples. Vulcan-specific command metadata lives under `metadata.vulcan.commands` in the `SKILL.md` frontmatter.

Use a skill command when an action should be directly callable with typed input and output from CLI, MCP, `describe`, internal JS, schedulers, or future assistant runtimes.

Use a plugin instead when code should run automatically because a Vulcan event happened.

Use `vulcan run` instead when the script is still exploratory.

Common commands:

```bash
vulcan skill list
vulcan skill show daily-review
vulcan skill commands daily-review
vulcan skill run daily-review prepare-day --input-json '{"date":"2026-05-05"}'
vulcan skill run daily-review prepare-day --arg date=2026-05-05 --arg-json dryRun=true
jq '.messages' chat.json | vulcan skill run conversation-export export --arg title=Chat --arg-json-file messages=-
```

`--arg key=value` adds a string field to the input object. `--arg-json key=json`
adds a typed JSON value. `--arg-file key=path` adds a string field from a file,
and `--arg-json-file key=path` adds a typed JSON field from a file. Use `-` as
the path to read one field from stdin. These flags merge into any object supplied
by `--input-json`, `--input-file`, or stdin, then the final object is validated
against the skill command input schema.

Projected skill commands may also appear as normal tools in `vulcan describe --format mcp` and in the MCP server.

Projected tool names are normalized as `skill_<skill_name>_<command_id>`, for example `skill_daily_review_prepare_day`.

Skill commands may declare `metadata.vulcan.commands[].cli` aliases and flags for a
more natural shell interface:

```bash
vulcan tool init conversation-export --description "Export chat transcripts"
vulcan tool run conversation-export --title Chat --user Hello --assistant "Some message"
```

These custom flags only build the same JSON input object used by MCP and `tools.call()`;
the normal schema validation and permission checks still run.

Bash, Fish, and Zsh completions use the same `cli` metadata. `vulcan complete
custom-tool <prefix>` lists projected tool names and aliases, while `vulcan
complete custom-tool-flag:<tool-or-alias> --<prefix>` lists declared custom
flags for one tool. `vulcan complete custom-tool-value:<tool-or-alias>:<flag>
<prefix>` lists declared choices or delegates to the flag's `completion` context.

Custom CLI actions include typed scalar flags, repeatable array flags, file/stdin
flags, `choice` flags, and `append_message`. Dotted `field` values create nested
input objects before schema validation.

Declare `examples` beside the command metadata to make the tool self-testing and
documented. `vulcan tool test <tool-or-alias>` runs every example, and
`--example <name>` selects one. Examples can provide raw `input`, `input_file`
relative to the skill directory, or shell-style `cli_args`; optional
`expected_output` or `expected_output_file` is compared exactly. Mismatches are
reported with JSON path diffs. Add `--profile <permission-profile>` to verify
the tool under the same permission ceiling an MCP or external agent caller will use.

Use `vulcan tool lint [<tool-or-alias>] --strict` before exposing a custom tool to
external harnesses. It checks for schemas, CLI aliases, examples, required-field
coverage, relative entrypoints, Vulcan shebangs, executable scripts, mutation
dry-run conventions, and risky sandbox choices. Add `--fix` for safe packaging
repairs such as shebang normalization and executable-bit fixes.

Use `vulcan tool compat <tool-or-alias> --surface cli,mcp,openai-tools,js` before
sharing a tool across harnesses. It reports surface-specific schema, CLI, sandbox,
and callability issues that may not show up when only running a local example.

JavaScript can call skill commands through either `tools.call("skill_daily_review_prepare_day", input)` or `skills.run("daily-review", "prepare-day", input)`.

Nested calls preserve the current effective permission ceiling. A script cannot use `skills.run()` to escape its own sandbox or permission profile.
