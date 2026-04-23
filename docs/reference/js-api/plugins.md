# JS Plugins

Vulcan plugins are plain JavaScript files, typically stored in `.vulcan/plugins/`, and
registered in `.vulcan/config.toml` under `[plugins.<name>]`.

Example registration:

```toml
[plugins.lint]
enabled = true
events = ["on_note_write", "on_pre_commit"]
sandbox = "strict"
permission_profile = "readonly"
```

Default file resolution:

- `plugins.lint` without an explicit `path` resolves to `.vulcan/plugins/lint.js`
- `vulcan plugin list` also discovers unregistered `*.js` files in `.vulcan/plugins/`

## When a plugin is the right tool

Plugins are not the same thing as skills or custom tools.

- A **plugin** reacts to a Vulcan lifecycle event such as note write, pre-commit, scan completion,
  or refactor.
- A **custom tool** is a directly callable request/response function that a human, LLM, CLI command,
  MCP client, or JS script invokes by name.
- A **skill** is Markdown guidance that teaches a workflow; it is not an event hook and not a typed
  callable function.

Use a plugin when the behavior should happen because an event occurred. Use a custom tool when the
behavior should happen because someone explicitly asked for it. Use a skill when the asset is
guidance rather than code.

Examples:

- "Reject writes that do not end in a newline" is a plugin.
- "Summarize a meeting note into structured JSON" is a custom tool.
- "Teach the agent how to do a daily review" is a skill.

See also: [automation-surfaces.md](../../guide/automation-surfaces.md)

## Entrypoints

Plugins expose global functions. Vulcan looks up handlers by name:

- `on_note_write(event, ctx)`
- `on_note_create(event, ctx)`
- `on_note_delete(event, ctx)`
- `on_pre_commit(event, ctx)`
- `on_post_commit(event, ctx)`
- `on_scan_complete(event, ctx)`
- `on_refactor(event, ctx)`
- `main(event, ctx)` for `vulcan plugin run <name>`

## Blocking vs post hooks

Blocking hooks:

- `on_note_write`
- `on_pre_commit`

These run before the action completes. Throw an error to abort the operation.

Post hooks:

- `on_note_create`
- `on_note_delete`
- `on_post_commit`
- `on_scan_complete`
- `on_refactor`

These run after the action succeeds. Errors are reported as warnings and do not roll back the
underlying operation.

## Event payloads

Every handler receives:

- `event`: the event payload for the current invocation
- `ctx.plugin`: metadata about the running plugin (name, path, events, sandbox, permission profile)

Current payload shapes:

- `on_note_write`: `{ kind, path, operation, existed_before, previous_content, content }`
- `on_note_create`: `{ kind, path, content }`
- `on_note_delete`: `{ kind, path }`
- `on_pre_commit`: `{ kind, action, files }`
- `on_post_commit`: `{ kind, action, files, sha, message }`
- `on_scan_complete`: `{ kind, mode, summary, paths? }`
- `on_refactor`: `{ kind, action, paths }`
- `main`: `{ kind: "manual", plugin }`

## Trust and permissions

- Plugin execution requires a trusted vault: `vulcan trust add`
- `sandbox` uses the same `strict|fs|net|none` levels as `vulcan run`
- `permission_profile` reuses `[permissions.profiles.*]`
- If the CLI is already running under `--permissions <profile>`, plugins inherit that profile
  unless they request a narrower profile; broader plugin profiles are rejected

## Example

```js
function on_note_write(event) {
  if (!event.content.endsWith("\n")) {
    throw new Error(`note ${event.path} must end with a newline`);
  }
}

function main(event, ctx) {
  return { plugin: ctx.plugin.name, kind: event.kind };
}
```
