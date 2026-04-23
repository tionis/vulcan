# JS Tools Namespace

The `tools` namespace is the runtime-facing entrypoint for vault-native custom tools.

Use it when JavaScript running inside Vulcan should inspect or call a registry-backed custom tool
instead of reimplementing logic inline.

Planned core entrypoints:

- `tools.list()` — list visible custom tools with metadata such as name, description, sandbox, and
  pack membership
- `tools.get(name)` — read one tool definition and documentation
- `tools.call(name, input, opts?)` — invoke one tool with validated input and return the structured
  result

Why this exists:

- skills and plugins should be able to compose the same reusable tool logic
- `vulcan run` scripts should not need to know whether a capability is built-in or vault-defined
- tool-to-tool composition should preserve the current permission ceiling instead of re-expanding it

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
