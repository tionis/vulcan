The query DSL is Vulcan's structured language for selecting notes, tasks, and Dataview-like results.

Shape:

1. Start with a source such as `from notes`, `from #tag`, or a saved source.
2. Add `where` clauses for typed filters.
3. Add `sort by`, `limit`, or output formatting when needed.

Examples:

- `from notes where status = active sort by updated desc`
- `from #project/alpha where owner = "eric"`
- `from notes where file.path starts_with "Daily/" limit 10`

Use `query` when you care about fields, tags, links, or file metadata. Use `search` when you care about note text and ranking.

Related command patterns:

- `vulcan query --format table '<dsl>'`
- `vulcan ls --where 'status = active'`
- `vulcan search release --where 'team = platform'`

See also: `help filters`, `help examples`, `help query`.
