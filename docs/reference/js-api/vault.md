`vault` is the primary JS entrypoint for interacting with indexed vault state.

Available today:

- `vault.note(pathOrName)` to resolve one note.
- `vault.notes(source?)` to iterate or filter note collections.
- `vault.query(dsl, opts?)` to execute structured queries.
- `vault.search(query, opts?)` to run content-oriented search.
- `vault.graph.shortestPath(from, to)`
- `vault.graph.hubs(opts?)`
- `vault.graph.components(opts?)`
- `vault.graph.deadEnds(opts?)`

- `vault.daily.today()`
- `vault.daily.get(date)`
- `vault.daily.range(from, to)`
- `vault.events({ from, to })`

Collection results use the shared DataArray pipeline, including `.where()`, `.sortBy()`, `.limit()`, and `.forEach()`.

Still pending are write-capable methods such as transactional note mutation and network-gated helpers.

See also: `help js`, `help js.vault.graph`, `help js.vault.note`.
