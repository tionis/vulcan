The JS runtime surface builds on the DataviewJS-compatible sandbox already embedded in Vulcan.

Available today:

- `vulcan run <file.js>` and `vulcan run <script-name>`
- `vulcan run --script <file>` for shebang entrypoints
- `vulcan run` as an interactive REPL with multiline input, completion, and history
- `--sandbox strict|fs|net|none` to control write and network access
- `help(obj)` and `console.log(...)` inside the runtime

Current top-level namespaces:

- `vault` for note lookup, queries, graph access, periodic note helpers, and vault mutations.
- `web` for network-gated search and fetch helpers.
- `help(obj)` for runtime introspection

Current write/network surface:

- `vault.set()`, `vault.create()`, `vault.append()`, `vault.patch()`
- `vault.update()`, `vault.unset()`, `vault.transaction()`, `vault.refactor.*`
- `web.search()` and `web.fetch()` when the sandbox is `net` or `none`

See also: `help js.vault`, `help js.vault.graph`, `help js.vault.note`.
