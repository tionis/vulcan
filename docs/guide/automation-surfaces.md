# Automation Surfaces

Vulcan has several related but different automation surfaces. They should be documented together so
users do not have to infer architecture from implementation details.

## Short version

- Use a **skill** to teach a workflow.
- Use a **custom tool** to expose a reusable typed callable function.
- Use a **plugin** to react to Vulcan lifecycle events.
- Use **`vulcan run`** for one-off or local scripting that does not need to become a first-class
  discoverable tool.

## Comparison

| Surface | Primary purpose | Trigger | Discoverable as tool | Best for |
|---|---|---|---|---|
| Skill | Guidance and examples | Read by human/LLM | No | Teaching workflows |
| Custom tool | Direct request/response callable | Explicit invocation | Yes | Reusable typed automation |
| Plugin | Reactive hook | Lifecycle event | No | Policy, linting, post-action automation |
| `vulcan run` script | Ad hoc script execution | Manual script run | No | One-off or local automation |

## Skills

Skills are Markdown guidance files, usually under `.agents/skills/<name>/SKILL.md`.

Use a skill when:

- the main value is instructions, heuristics, examples, or sequencing advice
- a human or LLM needs help choosing commands or avoiding common mistakes
- the workflow can be expressed as "when to use this" and "how to approach it"

Do not use a skill when:

- you need a typed callable interface with structured input and output
- the behavior should be exported through MCP or `describe --format openai-tools`
- the primary asset is executable logic rather than guidance

Example:

- "daily-review" is a skill because it teaches how to inspect the day's notes, search results, and
  tasks, but does not need to be a single callable function.

## Custom tools

Custom tools are vault-defined callable functions, planned under `.agents/tools/<name>/`.

Use a custom tool when:

- the behavior should be callable by name from CLI, MCP, external runtimes, and JS
- the function benefits from an explicit input schema and output schema
- you want one reusable request/response surface rather than repeating script snippets

Do not use a custom tool when:

- the code should run automatically on events
- the asset is mostly explanatory or instructional
- the logic is purely one-off and does not justify registry/discovery overhead

Examples:

- `summarize_meeting`: read a note, return structured decisions and action items
- `calendar_lookup`: call an external API and normalize the response into a stable JSON shape
- `bulk_tag_cleanup`: take typed input and apply a constrained vault mutation

## Plugins

Plugins are event-driven JS hooks, usually registered under `[plugins.<name>]` and stored in
`.vulcan/plugins/`.

Use a plugin when:

- the code should run because something happened in Vulcan
- success or failure should block or annotate an existing operation
- the right mental model is "hook into write/scan/commit/refactor"

Do not use a plugin when:

- a human or LLM should call the behavior directly by name
- the behavior needs a typed request/response contract exposed to MCP or `describe`
- the asset is primarily instructions or examples

Examples:

- reject note writes that violate formatting rules
- run a linter before commit
- emit a warning after scan completion if diagnostics crossed a threshold

## `vulcan run` scripts

`vulcan run` is the general JS execution path for ad hoc or local scripts.

Use it when:

- the automation is one-off or experimental
- you are prototyping logic before deciding whether it deserves promotion into a custom tool
- the code does not need registry-backed discovery or typed schemas

Promote a script into a custom tool when:

- other people or agents should discover and call it by name
- you want stable schemas and documentation
- the same behavior is being copied into skills, wrappers, or external harness glue

## Choosing the right surface

If you are unsure, ask these questions in order:

1. Is the main asset instructions rather than code?
   Then use a skill.
2. Should it run automatically in response to Vulcan events?
   Then use a plugin.
3. Should it be directly callable by name with typed input and output?
   Then use a custom tool.
4. Is it still exploratory or personal automation?
   Start with `vulcan run`.

## Examples by scenario

Scenario: "Teach the assistant how to do a weekly review."

- Best fit: skill
- Why: the main value is process guidance and examples

Scenario: "Given a note path, return a structured meeting summary."

- Best fit: custom tool
- Why: direct invocation with typed input/output

Scenario: "Prevent commits if generated notes are malformed."

- Best fit: plugin
- Why: event-driven pre-commit policy hook

Scenario: "Try a one-off migration over a subset of notes."

- Best fit: `vulcan run` script first
- Why: low ceremony until the behavior stabilizes

## Documentation expectations

The integrated help system and in-repo docs should describe these surfaces consistently. In
particular, plugin docs should not assume users already understand how plugins differ from tools and
skills, and custom tool docs should explain when a plugin or plain script would be the better fit.

See also:

- [custom_tools.md](../assistant/custom_tools.md)
- [plugins.md](../reference/js-api/plugins.md)
- [scripting.md](./scripting.md)
