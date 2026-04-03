JavaScript execution in Vulcan is sandboxed.

Current guarantees:

- Memory, stack, and execution limits are enforced by the embedded QuickJS runtime.
- Dangerous globals like user-visible `eval` and `Function` are removed from the exposed surface.
- Network-style capabilities are gated behind explicit runtime support rather than being implicit.

Operational guidance:

- Keep scripts deterministic and bounded.
- Prefer CLI commands with `--output json` when an external harness can do the orchestration.
- Treat write operations as privileged; use dry runs and explicit confirmation patterns when the runtime eventually exposes mutations.

The standalone scripting runtime in 9.18.5 extends these rules with sandbox tiers for vault writes and web access.

See also: `help scripting`, `help js.vault`, `help web`.
