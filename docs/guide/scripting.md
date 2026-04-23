Vulcan has three JavaScript-oriented surfaces today:

- `vulcan run` for ad hoc scripts, named `.vulcan/scripts/*` entrypoints, and a simple REPL.
- DataviewJS evaluation for indexed `dataviewjs` blocks.
- Templater-compatible JavaScript execution for template rendering.

A fourth programmable surface, vault-native custom tools, is planned as a registry-backed direct-call
layer on top of the same runtime. Use `vulcan run` for ad hoc scripts; use custom tools when the
behavior should become discoverable and callable by name across CLI, MCP, and assistant workflows.

These run inside a restricted QuickJS sandbox. They are suitable for note-local automation and computed views, not arbitrary shell access.

External harnesses can also treat the CLI itself as a tool surface:

- `vulcan describe --format openai-tools`
- `vulcan describe --format mcp`
- `vulcan help --output json <topic>`

Current `vulcan run` highlights:

- `vulcan run <file.js>` executes one script file and strips a leading shebang when present.
- `vulcan run <name>` resolves `.vulcan/scripts/<name>` or `.vulcan/scripts/<name>.js`.
- `vulcan run --script <file>` is the shebang-friendly form for executable script files.
- `vulcan run` opens the interactive REPL.
- `--sandbox strict|fs|net|none` selects the runtime capability tier.
- `--timeout <duration>` overrides the JS execution limit for one script run or REPL session.
- `console.log(...)` and `help(obj)` are available inside the runtime.
- REPL variables persist across prompts within the same session, with multiline input, completion, and history in `.vulcan/repl_history`.
- Write-capable helpers such as `vault.transaction()` require `--sandbox fs` or higher.
- Web helpers such as `web.search()` and `web.fetch()` require `--sandbox net` or higher.

See also: `help sandbox`, `help js`, `help describe`, and [automation-surfaces.md](./automation-surfaces.md).
