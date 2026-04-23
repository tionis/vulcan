# JS Tools Namespace

The `tools` namespace is the runtime-facing entrypoint for vault-native custom tools.

Use it when JavaScript running inside Vulcan should inspect or call a registry-backed custom tool
instead of reimplementing logic inline.

Available entrypoints:

- `tools.list()` — list visible custom tools with metadata such as name, description, sandbox, and
  pack membership
- `tools.get(name)` — read one tool definition and documentation
- `tools.call(name, input, opts?)` — invoke one tool with validated input and return the structured
  result

Current runtime surfaces:

- `vulcan run`
- the JS REPL (`vulcan run`)
- `vulcan dataview query-js`
- custom tool entrypoints
- plugin hooks executed through the shared JS runtime

Why this exists:

- skills and plugins should be able to compose the same reusable tool logic
- `vulcan run` scripts should not need to know whether a capability is built-in or vault-defined
- tool-to-tool composition should preserve the current permission ceiling instead of re-expanding it

Runtime behavior:

- `tools.list()` returns visible custom tools plus `callable`
- `tools.get(name)` returns static metadata and the Markdown body from `TOOL.md`
- `tools.call(name, input, opts?)` returns the callee's JSON result
- if the callee returned `{ result, text }`, the same envelope is returned to the caller
- recursive tool-call loops are rejected
- nested calls are capped at depth 8

Custom-tool runtime context:

- `ctx.tool` exposes the running tool's manifest metadata
- `ctx.call` exposes invocation metadata such as the caller surface and timestamp
- `ctx.secrets.list()`, `ctx.secrets.get(name)`, and `ctx.secrets.require(name)` expose manifest
  secret bindings without storing secret values in the vault

Return contract:

- return any JSON-serializable value for a plain structured result
- return `{ result, text }` when you want both machine-readable output and a short human fallback

See also:

- `help tool`
- `help js.host`
- `help js.plugins`
- `help automation-surfaces`
