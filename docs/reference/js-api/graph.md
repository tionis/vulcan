`vault.graph` is the graph-oriented JS surface exposed inside `vulcan run`, DataviewJS, and the shared QuickJS runtime.

Available methods:

- `vault.graph.shortestPath(from, to)`
- `vault.graph.hubs({ limit })`
- `vault.graph.components({ limit })`
- `vault.graph.deadEnds({ limit })`

Use the graph surface when topology matters more than text matching. For simple note lookup, start with `vault.note()` or `vault.query()`.

Equivalent CLI entrypoints:

- `vulcan note backlinks <note>`
- `vulcan note links <note>`
- `vulcan graph path <from> <to>`
- `vulcan graph hubs`

See also: `help js.vault`, `help graph-exploration`, `help graph`.
