Vulcan uses a shared typed filter grammar across `notes`, `search --where`, `ls`, report definitions, and several mutation commands.

Common operators:

- Equality: `status = done`
- Inequality: `priority != low`
- Ordering: `rating >= 4`
- Text predicates: `contains`, `starts_with`, `ends_with`, `matches`, `matches_i`
- Collection predicates: `in`, `not in`
- Null checks: `is null`, `is not null`

Examples:

- `status = active and due >= date("2026-04-01")`
- `tags contains "#project"`
- `file.path starts_with "Projects/"`
- `owner matches_i "^eric|sam$"`

Notes:

- Property typing is lenient, but mismatches surface through `doctor`.
- `matches` is regex and `matches_i` is case-insensitive regex.
- Use `search` for free text and `query` or `notes --where` for metadata precision.

See also: `help query-dsl`, `help query`, `help notes`.
