`vault.graph` is the planned graph-oriented JS surface.

Its role is to expose relationships already indexed by Vulcan:

- outbound links
- backlinks
- shortest paths
- dead ends
- hubs
- connected components

Use the graph surface when topology matters more than text matching. For simple note lookup, start with `vault.note()` or `vault.query()`.

Until the standalone runtime lands, the equivalent CLI entrypoints are:

- `vulcan backlinks <note>`
- `vulcan links <note>`
- `vulcan graph path <from> <to>`
- `vulcan graph hubs`

See also: `help js.vault`, `help graph-exploration`, `help graph`.
