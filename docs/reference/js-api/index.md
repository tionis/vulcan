The JS runtime surface builds on the DataviewJS-compatible sandbox already embedded in Vulcan.

Important status note:

- DataviewJS and Templater JavaScript are implemented today.
- The broader `vulcan run` runtime is still being completed in Phase 9.18.5.

Planned top-level namespaces:

- `vault` for note lookup, queries, graph access, periodic note helpers, and batched mutations.
- `web` for explicit web search and fetch under a network-enabled sandbox tier.
- `help(obj)` for runtime introspection once the general REPL lands.

See also: `help js.vault`, `help js.vault.graph`, `help js.vault.note`.
