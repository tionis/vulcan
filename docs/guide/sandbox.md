JavaScript execution in Vulcan is sandboxed.

Current sandbox levels:

- `strict`: read-only vault APIs with memory, stack, and execution limits.
- `fs`: adds vault write APIs such as `vault.create()` and `vault.transaction()`.
- `net`: adds `web.search()` and `web.fetch()` on top of `fs`.
- `none`: keeps the full API surface and removes runtime resource limits.

Common guarantees:

- Memory, stack, and execution limits are enforced by the embedded QuickJS runtime for `strict`, `fs`, and `net`.
- Dangerous globals like user-visible `eval` and `Function` are removed from the exposed surface.
- Network-style capabilities are explicit and opt-in rather than implicit.

Operational guidance:

- Keep scripts deterministic and bounded.
- Prefer CLI commands with `--output json` when an external harness can do the orchestration.
- Treat write operations as privileged; use `strict` by default and escalate only when the task requires it.

Configuration defaults live under `[js_runtime]` in `.vulcan/config.toml`, including `default_sandbox` and `default_timeout_seconds`.

See also: `help scripting`, `help js.vault`, `help web`.
