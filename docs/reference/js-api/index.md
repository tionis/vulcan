The JS runtime surface builds on the DataviewJS-compatible sandbox already embedded in Vulcan.

Available today:

- `vulcan run <file.js>` and `vulcan run <script-name>`
- `vulcan run --script <file>` for ad hoc script shebang entrypoints
- `vulcan skill exec <script>` for generated skill command shebang entrypoints
- `vulcan run` as an interactive REPL with multiline input, completion, and history
- `--sandbox strict|fs|net|none` to control write and network access
- `help(obj)` and `console.log(...)` inside the runtime

Current top-level namespaces:

- `vault` for note lookup, queries, graph access, periodic note helpers, and vault mutations.
- `vulcan` for permission introspection, vault-aware date helpers, and scratch data.
- `tool` for custom-tool input, result, progress, confirmation, and audit helpers.
- `web` for network-gated search and fetch helpers.
- `tools` for registry-backed skill command tool discovery and invocation.
- `skills` for Agent Skills-compatible command discovery and invocation.
- `host` for permission-gated local process execution.
- `help(obj)` for runtime introspection

Current write/network surface:

- `vault.set()`, `vault.create()`, `vault.append()`, `vault.patch()`
- `vault.update()`, `vault.unset()`, `vault.transaction()`, `vault.refactor.*`
- `vault.plan()` / `vault.mutation()` for dry-run/apply mutation plans with changed paths and diffs
- `vault.note(path).properties.*` for frontmatter get/set/unset/merge helpers
- `web.search()` and `web.fetch()` when the sandbox is `net` or `none`

See also: `help js.vault`, `help js.tools`, `help js.skills`, `help js.host`, `help js.vault.graph`, `help js.vault.note`.
