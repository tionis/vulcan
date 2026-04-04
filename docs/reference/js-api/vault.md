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
- `vault.set(path, content, opts?)`
- `vault.create(path, opts?)`
- `vault.append(path, text, opts?)`
- `vault.patch(path, find, replace, opts?)`
- `vault.update(path, key, value)`
- `vault.unset(path, key)`
- `vault.transaction(fn)`
- `vault.refactor.renameAlias(...)`, `renameHeading(...)`, `renameBlockRef(...)`, `renameProperty(...)`, `mergeTags(...)`, `move(...)`
- `vault.inbox(text)`

Collection results use the shared DataArray pipeline, including `.where()`, `.sortBy()`, `.limit()`, and `.forEach()`.

Write operations require `--sandbox fs` or higher. Web helpers live under the separate `web` namespace and require `--sandbox net` or higher.

See also: `help js`, `help js.vault.graph`, `help js.vault.note`.
