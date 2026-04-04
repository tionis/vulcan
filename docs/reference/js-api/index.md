The JS runtime surface builds on the DataviewJS-compatible sandbox already embedded in Vulcan.

Available today:

- `vulcan run <file.js>` and `vulcan run <script-name>`
- `vulcan run --script <file>` for shebang entrypoints
- `vulcan run` as a simple line-oriented REPL
- `help(obj)` and `console.log(...)` inside the runtime

Current top-level namespaces:

- `vault` for note lookup, queries, graph access, and periodic note helpers.
- `help(obj)` for runtime introspection

Still pending in 9.18.5:

- sandbox selection flags
- persistent REPL state, history, multiline input, and completion
- write-capable `vault.*` APIs and network-gated `web.*`

See also: `help js.vault`, `help js.vault.graph`, `help js.vault.note`.
