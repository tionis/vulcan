# JavaScript Scripting

Vulcan embeds QuickJS for vault automation, computed views, compatibility workflows, reusable
tools, and lifecycle hooks. Prefer a dedicated CLI command when one already expresses the task;
use JavaScript when the workflow needs custom computation or orchestration.

JavaScript-dependent features require the default `js_runtime` Cargo feature.

## Runtime surfaces

Vulcan currently uses the shared runtime in five main ways:

| Surface | Best suited to |
| --- | --- |
| `vulcan run` and the REPL | Ad hoc scripts, named vault scripts, and interactive exploration |
| DataviewJS | Computed views over indexed vault data |
| Templater JavaScript | Dynamic template rendering and compatible `tp.*` helpers |
| Skill command scripts | Reusable, schema-validated commands exposed through CLI, MCP, and JS tools |
| JavaScript plugins | Manual entrypoints and lifecycle hooks around note, scan, refactor, and Git events |

All surfaces use the same core namespaces, but their effective sandbox, permission profile, trust
requirements, input context, and output contract can differ.

## Running scripts

Run a file directly:

```bash
vulcan run ./scripts/report.js
```

Run a named script from the configured scripts folder, `.vulcan/scripts` by default:

```bash
vulcan run weekly-report
```

Use the explicit form in executable shebang scripts:

```text
#!/usr/bin/env -S vulcan run --script
```

Run `vulcan run` without a script to open the REPL. REPL variables persist within the session, and
the interface supports multiline input, completion, `console.log(...)`, `help(obj)`, and history in
`.vulcan/repl_history`.

Useful execution options include:

- `--sandbox strict|fs|net|none` to select the capability tier
- `--timeout <duration>` to override the execution limit for the invocation
- `--permissions <profile>` as a global option to select the caller's authority ceiling

## Choosing a sandbox

Start with `strict` for computation and vault reads. Use `fs` for vault mutations and `net` for web
helpers. `none` removes runtime resource limits and enables permission-gated host process APIs:

```js
host.exec(["git", "status", "--short"]);
host.shell("git status --short");
```

`host.exec()` additionally requires execute permission. `host.shell()` requires both execute and
shell permission. Therefore, scripts are not arbitrary shell environments by default, but a
deliberately configured `none` invocation can run host processes. See the [sandbox guide](sandbox.md)
before using that tier.

## Core namespaces

The versioned JS API currently exposes:

- `vault` for note lookup, queries, graph access, periodic helpers, and mutations
- `vulcan` for runtime metadata, permission introspection, dates, and scratch helpers
- `tool` for custom-tool input, result, progress, confirmation, and audit helpers
- `tools` and `skills` for discovering and invoking exposed skill commands
- `web` for permission-gated search and fetch at `net` or higher
- `host` for permission-gated process execution under `none`
- `help(obj)` for runtime introspection

Use `vulcan help js.contract` or `docs/reference/js-api/contract.json` as the stable namespace
inventory. Use `help(vault)`, `help(web.fetch)`, or another object inside a running script for
contextual documentation.

## Writing vault data

Write helpers require `fs` or higher. Group related mutations in a transaction so they succeed or
roll back together:

```js
const result = vault.transaction(() => {
  vault.create("Reports/Weekly.md", "# Weekly report\n");
  vault.append("Reports/Index.md", "- [[Weekly]]\n");
});

result;
```

Custom tools should prefer `vault.plan({ dry_run })` for reviewable changed paths and diffs, and
`tool.result()` for structured returns.

## Reusable skill commands

Use a skill command when behavior should be discoverable and callable by name across CLI, MCP,
OpenAI tool descriptions, JavaScript `tools.call()`, and assistant workflows. Generated scripts use:

```text
#!/usr/bin/env -S vulcan skill exec
```

`vulcan skill exec` resolves the declaring `SKILL.md`, validates input, builds the execution
context, and calls `main(input, ctx)`. Exposed skill commands must declare a bounded sandbox;
`sandbox = none` is not accepted.

See `vulcan help skill-commands` and `vulcan help custom-tools` for schemas, packaging, dry-run
contracts, and compatibility checks.

## Plugins and compatibility runtimes

Plugins are JavaScript files registered in Vulcan configuration. They may expose `main` for manual
execution and hooks such as `on_note_write`, `on_scan_complete`, or `on_pre_commit`. Plugin
execution requires a trusted vault and remains constrained by the effective sandbox and permission
profile. See `vulcan help js.plugins`.

DataviewJS and Templater-compatible JavaScript share the runtime foundation but provide
surface-specific objects and behavior. Do not assume that an ad hoc `vulcan run` script has the
same context object or lifecycle when copied into a DataviewJS block, template, plugin, or skill
command.

## External automation

An external harness can use the CLI itself instead of embedding JavaScript:

```bash
vulcan describe --format openai-tools
vulcan describe --format mcp
vulcan --output json help query
```

This is usually the better boundary when the harness already provides orchestration, auditing, and
process isolation.

See also `vulcan help sandbox`, `vulcan help js`, `vulcan help automation-surfaces`, and the
[JS API reference](../reference/js-api/index.md).
