`vault` is the primary JS entrypoint for interacting with indexed vault state.

Available today:

- `vault.note(pathOrName)` to resolve one note.
- `vault.notes(source?)` to iterate or filter note collections.
- `vault.query(dsl, opts?)` to execute structured queries, or `vault.query()` to build one fluently.
- `vault.search(query, opts?)` to run content-oriented search, or `vault.search()` to build one fluently.
- `vault.diffText(before, after, opts?)` to render a small unified diff.
- `vault.plan(opts?)` / `vault.mutation(opts?)` to collect dry-run/apply mutation plans.
- `vault.graph.shortestPath(from, to)`
- `vault.graph.hubs(opts?)`
- `vault.graph.components(opts?)`
- `vault.graph.deadEnds(opts?)`

- `vault.daily.today()`
- `vault.daily.path(date?)`
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
- `vault.withLock(paths, fn)` reserves the concurrency-friendly shape for mutation tools; today it runs the callback in-process.
- `vault.refactor.renameAlias(...)`, `renameHeading(...)`, `renameBlockRef(...)`, `renameProperty(...)`, `mergeTags(...)`, `move(...)`
- `vault.inbox(text)`

Collection results use the shared DataArray pipeline, including `.where()`, `.sortBy()`, `.limit()`, and `.forEach()`.

Mutation plans are useful for custom tools:

```js
const plan = vault.plan({ dry_run: input.dry_run })
  .append("Daily.md", "- [ ] Review")
  .diff("Daily.md", before, after);

return plan.result();
```

Note objects expose `note.section(opts)`, `note.block(id)`, `note.patch(...)`,
`note.append(...)`, and `note.properties.get/set/unset/merge(...)` wrappers over
the same permission-checked vault APIs.

Write operations require `--sandbox fs` or higher. Web helpers live under the separate `web` namespace and require `--sandbox net` or higher.

See also: `help js`, `help js.vault.graph`, `help js.vault.note`.
