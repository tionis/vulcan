# JS Tools Namespace

The `tools` namespace is the runtime-facing entrypoint for exposed Agent Skills-compatible command tools.

Use it when JavaScript running inside Vulcan should inspect or call a registry-backed skill command
instead of reimplementing logic inline.

Available entrypoints:

- `tools.list()` — list visible exposed skill command tools with metadata such as name, description, sandbox, and
  pack membership
- `tools.get(name)` — read one tool definition and documentation
- `tools.call(name, input, opts?)` — invoke one tool with validated input and return the structured
  result
- `tools.callChecked(name, input, opts?)` — invoke one tool and attach `expect(path)` for checked
  composition
- `tool.input(defaults?)` — read the validated current custom-tool input merged over defaults
- `tool.result()` / `tool.ok()` / `tool.error()` — build a standard structured tool result
- `tool.progress()` / `tool.notice()` / `tool.audit()` — emit or return agent-friendly runtime events

Current runtime surfaces:

- `vulcan run`
- the JS REPL (`vulcan run`)
- `vulcan dataview query-js`
- skill command entrypoints
- plugin hooks executed through the shared JS runtime

Why this exists:

- skills and plugins should be able to compose the same reusable tool logic
- `vulcan run` scripts should not need to know whether a capability is built-in or vault-defined
- tool-to-tool composition should preserve the current permission ceiling instead of re-expanding it

Runtime behavior:

- `tools.list()` returns visible exposed skill-command tools plus `callable`
- `tools.get(name)` returns static metadata and the Markdown body from the declaring `SKILL.md`
- `tools.call(name, input, opts?)` returns the callee's JSON result
- `tools.callChecked(name, input, opts?)` returns the callee result with non-enumerable `expect(path)`
- recursive tool-call loops are rejected
- nested calls are capped at depth 8

Skill-command runtime context:

- `ctx.skill` exposes the declaring skill metadata
- `ctx.command` exposes the command metadata from `SKILL.md`
- `ctx.call` exposes invocation metadata such as the caller surface and timestamp

Return contract:

- return any JSON-serializable value for a plain structured result
- prefer `tool.result().summary(...).changedPath(...).data(...).ok()` for write-capable tools
- return `tool.error(code, message, details?)` for structured failures

Example:

```js
function main(input) {
  const normalized = tool.input({ dry_run: true });
  const created = tools.callChecked("task-create", { text: input.text, dry_run: normalized.dry_run });
  return tool.result()
    .summary("Created task")
    .data({ task_id: created.expect("task.id") })
    .ok();
}
```

See also:

- `help tool`
- `help js.skills`
- `help js.host`
- `help js.plugins`
- `help automation-surfaces`
