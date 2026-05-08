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
```

Projected skill commands may also appear as normal tools in `vulcan describe --format mcp` and in the MCP server.

Projected tool names are normalized as `skill_<skill_name>_<command_id>`, for example `skill_daily_review_prepare_day`.

JavaScript can call skill commands through either `tools.call("skill_daily_review_prepare_day", input)` or `skills.run("daily-review", "prepare-day", input)`.

Nested calls preserve the current effective permission ceiling. A script cannot use `skills.run()` to escape its own sandbox or permission profile.
