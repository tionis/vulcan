# JS Host Execution

Host execution is intentionally separate from normal vault reads and writes.

Use host execution only when a custom tool or script must call an external program on the local
machine and there is no safer built-in Vulcan API for the job.

Planned APIs:

- `host.exec(argv, opts?)`
  - requires `execute`
  - takes an explicit argument vector
  - does not invoke a shell
- `host.shell(command, opts?)`
  - requires `shell`
  - higher risk because shell parsing and expansion are involved

Security rules:

- execution still requires the current permission profile to allow it
- custom tools remain limited by trusted-vault gating, sandbox ceilings, and normal timeout and
  output limits
- `host.exec()` is the recommended default in examples and bundled docs
- `host.shell()` should stay opt-in for higher-trust profiles only

Practical guidance:

- prefer built-in `vault.*`, `web.*`, and `tools.*` APIs when they exist
- use `host.exec()` for deterministic wrappers like `git`, formatters, or local converters
- avoid `host.shell()` unless argument-vector execution is genuinely insufficient

See also:

- `help tool`
- `help js.tools`
- `help sandbox`
