`vault` is the primary JS entrypoint for interacting with indexed vault state.

Planned core methods:

- `vault.note(pathOrName)` to resolve one note.
- `vault.notes()` to iterate or filter note collections.
- `vault.query(dsl)` to execute structured queries.
- `vault.search(query)` to run content-oriented search.

Available now through the shared runtime foundations:

- `vault.daily.today()`
- `vault.daily.get(date)`
- `vault.daily.range(from, to)`
- `vault.events({ from, to })`

Longer-term additions include graph traversal, transactional writes, and richer collection pipelines.

See also: `help js`, `help js.vault.graph`, `help js.vault.note`.
