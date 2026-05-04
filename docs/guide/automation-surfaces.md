# Automation Surfaces

Vulcan has several related but different automation surfaces. They are documented together so users can choose the right abstraction without inferring architecture from implementation details.

## Short version

- Use a **skill** to package workflow instructions, examples, references, assets, and optional commands.
- Use a **skill command** when part of a skill should be directly callable with typed input and output.
- Use a **plugin** to react to Vulcan lifecycle events.
- Use **`vulcan run`** for one-off or local scripting that does not need to become a first-class skill command.

## Comparison

| Surface | Primary purpose | Trigger | Discoverable as MCP/describe tool | Best for |
|---|---|---|---|---|
| Skill | Portable workflow package | Activated by human/LLM | Catalog/resource, not executable by itself | Teaching workflows and bundling resources |
| Skill command | Typed callable inside a skill | Explicit invocation | Yes, when projected | Reusable automation with schemas/permissions |
| Plugin | Reactive hook | Lifecycle event | No | Policy, linting, post-action automation |
| `vulcan run` script | Ad hoc script execution | Manual script run | No | One-off or experimental automation |

## Skills

Skills are Agent Skills-compatible directories, usually under `.agents/skills/<name>/`.

A skill contains a required `SKILL.md` file and may contain supporting directories such as `scripts/`, `references/`, and `assets/`.

Use a skill when:

- the main value is instructions, heuristics, examples, or sequencing advice
- a human or LLM needs help choosing commands or avoiding common mistakes
- you want a portable package that other Agent Skills-compatible harnesses can inspect
- the workflow needs supporting references, templates, or scripts

Do not use a skill alone when:

- behavior must be callable by name with typed input and output
- you need permission, secret, timeout, and output-schema metadata for a specific action

Example:

- `daily-review` is a skill because it teaches how to inspect the day's notes, tasks, and context. It may also export commands such as `prepare-day` and `process-inbox`.

## Skill commands

Skill commands are executable entrypoints declared by a skill. They usually live under the skill's `scripts/` directory and are described under `metadata.vulcan.commands` in `SKILL.md`.

Use a skill command when:

- behavior should be callable by name from CLI, MCP, external runtimes, or JS
- the function benefits from an explicit input schema and output schema
- the command needs scoped permissions, sandboxing, or secret bindings
- the command belongs with skill instructions and examples rather than as an unrelated standalone script

Examples:

- `daily-review.prepare-day`: read calendar/tasks/recent notes and propose a daily briefing
- `gmail.triage`: read recent email and propose inbox items or tasks
- `forgejo.project-status`: read issues/PRs and update a project status note

Projected command names may be normalized for tool surfaces, for example `daily_review_prepare_day`.

## Plugins

Plugins are event-driven JS hooks, usually registered under `[plugins.<name>]` and stored in `.vulcan/plugins/`.

Use a plugin when:

- code should run because something happened in Vulcan
- success or failure should block or annotate an existing operation
- the right mental model is "hook into write/scan/commit/refactor"

Do not use a plugin when:

- a human or LLM should call the behavior directly by name
- behavior needs a typed request/response contract exposed to MCP or `describe`
- the asset is primarily instructions, examples, or workflow guidance

Examples:

- reject note writes that violate formatting rules
- run a linter before commit
- emit a warning after scan completion if diagnostics cross a threshold

## `vulcan run` scripts

`vulcan run` is the general JS execution path for ad hoc or local scripts.

Use it when:

- automation is one-off or experimental
- you are prototyping logic before promoting it into a skill command
- code does not need registry-backed discovery or typed schemas

Promote a script into a skill command when:

- other people or agents should discover and call it by name
- you want stable schemas and documentation
- the same behavior is being copied into skills, wrappers, or external harness glue

## Choosing the right surface

If unsure, ask these questions in order:

1. Is this a workflow package with instructions, references, assets, or examples?
   Use a skill.
2. Is part of that skill directly callable with typed input/output?
   Add a skill command.
3. Should code run automatically in response to Vulcan events?
   Use a plugin.
4. Is it still exploratory or personal automation?
   Start with `vulcan run`.

## Examples by scenario

Scenario: "Teach the assistant how to do a weekly review."

- Best fit: skill
- Why: the main value is process guidance and examples

Scenario: "Given a date, prepare a daily briefing with proposed note edits."

- Best fit: skill command inside a `daily-review` skill
- Why: typed invocation plus skill-local guidance and templates

Scenario: "Prevent commits if generated notes are malformed."

- Best fit: plugin
- Why: event-driven pre-commit policy hook

Scenario: "Try a one-off migration over a subset of notes."

- Best fit: `vulcan run` first
- Why: low ceremony until behavior stabilizes

## Documentation expectations

The integrated help system and in-repo docs should describe these surfaces consistently. Plugin docs should not assume users already understand how plugins differ from skills and skill commands. Skill-command docs should explain when a plugin or plain script would be the better fit.

See also:

- [skill_commands.md](../assistant/skill_commands.md)
- [plugins.md](../reference/js-api/plugins.md)
- [scripting.md](./scripting.md)
