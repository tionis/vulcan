---
name: skill-creator
description: Create, modify, and improve Agent Skills-compatible skills for Vulcan vaults. Use this whenever the user wants to create a new skill, turn a repeated workflow into a skill, add callable skill commands, write or review skill command scripts, validate skills, or improve when a skill should trigger.
version: 1
tools:
  - skill_list
  - skill_get
  - skill_commands
  - skill_validate
  - tool_list
  - tool_show
  - tool_run
require_confirmation: false
---

# Skill Creator

Use this skill to design and maintain Agent Skills-compatible skill packages in a Vulcan vault.

Vulcan skills live under `.agents/skills/<skill-name>/` by default. A skill always has `SKILL.md`
and may include `scripts/`, `references/`, `assets/`, and `schemas/`.

## Recommended Flow

1. Capture the intended workflow before writing files.
2. Decide whether the skill is only guidance or also needs callable commands.
3. Scaffold with `vulcan skill init <name>` when starting fresh.
4. Add or edit `SKILL.md` frontmatter and instructions.
5. Put deterministic executable behavior in skill command scripts.
6. Validate with `vulcan skill validate`.
7. If commands are exposed, verify them with `vulcan tool list`, `vulcan tool show`, and `vulcan tool run`.

## Frontmatter

Use the standard Agent Skills fields:

```yaml
---
name: daily-review
description: Guide daily review and planning in this vault. Use when the user asks about today's routine, planning, inbox processing, daily notes, or reviewing open work.
version: 1
tools:
  - note_get
  - search
require_confirmation: false
---
```

Make `description` specific and trigger-oriented. Include both what the skill does and when to use
it. Keep detailed workflow instructions in the body.

## Vulcan Skill Commands

Declare callable commands under `metadata.vulcan.commands` when part of the skill should be invoked
directly by CLI, MCP, `describe`, JS `tools.call()`, or another skill command.

```yaml
metadata:
  vulcan:
    commands:
      - id: prepare-day
        description: Build a structured daily briefing for one date.
        script: scripts/prepare-day.js
        sandbox: fs
        permission_profile: daily-wiki-agent
        packs: [custom, daily]
        expose: true
        cli:
          aliases: [prepare-day]
          args:
            - flag: date
              action: string
              field: date
              description: Daily note date in YYYY-MM-DD form.
        input_schema:
          type: object
          additionalProperties: false
          properties:
            date:
              type: string
          required: [date]
        output_schema:
          type: object
```

Use `expose: true` only for stable commands that should appear in `vulcan tool list`, MCP, and
machine-readable descriptions.

Add a `cli` block when humans should run the command frequently from a shell.
CLI aliases and flags only build the input JSON object; the script, MCP tool, permissions,
and schema validation remain the same. Supported flag actions are `string`, `json`,
`string_file`, `json_file`, and `append_message` for chat-style repeated turns.

## Script Rules

Generated command scripts should be directly executable:

```javascript
#!/usr/bin/env -S vulcan skill exec

function main(input, ctx) {
  return {
    date: input.date,
    skill: ctx.skill.name,
    command: ctx.command.id,
  };
}
```

Rules for scripts:

- Export or define `main(input, ctx)`.
- Return a JSON-serializable value.
- Use `#!/usr/bin/env -S vulcan skill exec` for skill command scripts.
- Design input schemas so the command is usable both as a structured tool and from the shell. Direct scripts and `vulcan skill run` accept `--arg key=value` for string fields, `--arg-json key=json` for typed values, and `--arg-file key=path` or `--arg-json-file key=path` for larger fields. Use `-` as the path to read one field from stdin.
- For polished shell UX, declare `metadata.vulcan.commands[].cli` and test `vulcan tool run <alias> --flag value`.
- Prefer Vulcan JS APIs such as `vault.*`, `tools.*`, `skills.*`, `web.*`, and `host.*` over raw filesystem or shell work.
- Set the narrowest useful `sandbox`: `strict`, `fs`, or `net`. Do not use `none` for exposed skill commands.
- Add `permission_profile` when the command should run under a narrower authority ceiling.
- Use `host.exec(argv, opts)` instead of `host.shell(command, opts)` unless shell parsing is genuinely required.

## When Not to Add a Command

Keep the skill as Markdown guidance when:

- the workflow is judgment-heavy and not a stable request/response function
- one existing Vulcan CLI or MCP tool already handles the action
- the script would just wrap a single command without adding schema, validation, or reuse

Use `vulcan run` for exploratory one-off scripts. Promote to a skill command only when discovery,
schemas, permissions, or cross-harness execution matter.

## Validation Checklist

- `vulcan skill validate` succeeds.
- `vulcan skill commands <skill>` shows expected command metadata.
- Direct script execution works: `.agents/skills/<skill>/scripts/<command>.js --arg name=value`, `--arg-json-file messages=-`, or `--input-json '{}'`.
- `vulcan tool list` shows exposed commands after the vault is trusted.
- `vulcan tool run <tool-name> --input-json '<json>'` returns the expected JSON.
- `vulcan tool run <alias> --flag value` returns the same shape when CLI metadata is declared.
- Any write, network, or host execution behavior is covered by sandbox and permission-profile choices.

## Review Checklist

- The skill body is concise enough to load into context.
- Large details are moved into `references/` and linked from `SKILL.md`.
- Command schemas are strict enough to catch bad input.
- Script output is stable and documented.
- The skill explains when a plugin or plain `vulcan run` script would be a better fit.
