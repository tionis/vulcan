Vulcan has three JavaScript-oriented surfaces today:

- `vulcan run` for ad hoc scripts, named `.vulcan/scripts/*` entrypoints, and a simple REPL.
- DataviewJS evaluation for indexed `dataviewjs` blocks.
- Templater-compatible JavaScript execution for template rendering.

These run inside a restricted QuickJS sandbox. They are suitable for note-local automation and computed views, not arbitrary shell access.

External harnesses can also treat the CLI itself as a tool surface:

- `vulcan describe --format openai-tools`
- `vulcan describe --format mcp`
- `vulcan help --output json <topic>`

Current `vulcan run` highlights:

- `vulcan run <file.js>` executes one script file and strips a leading shebang when present.
- `vulcan run <name>` resolves `.vulcan/scripts/<name>` or `.vulcan/scripts/<name>.js`.
- `vulcan run --script <file>` is the shebang-friendly form for executable script files.
- `vulcan run` opens the current line-oriented REPL.
- `console.log(...)` and `help(obj)` are available inside the runtime.

Current limitations:

- The REPL does not yet preserve JS variables between prompts.
- Runtime sandbox selection flags and write-capable JS APIs are not available yet.

See also: `help sandbox`, `help js`, `help describe`.
