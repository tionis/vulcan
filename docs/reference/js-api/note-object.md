`vault.note(...)` returns a rich `Note` object backed by indexed metadata plus lazily loaded note details.

Current shape:

- identity: `path`, `name`, `aliases`
- content accessors: `content`, `frontmatter`, `headings`, `blocks`, `tasks`, `dataview_fields`
- file metadata: timestamps, tags, aliases, `file.day`
- query/search interop through `vault.query()` and `vault.search()`
- relationship helpers: `links()`, `backlinks()`, `neighbors(depth)`

Guidance:

- Resolve a note once, then operate on its typed fields instead of reparsing raw markdown in JS.
- Prefer structured fields and `vault.query()` when possible.
- Use `vault.set()/append()/patch()/update()/unset()` or `vault.transaction()` for writes when the sandbox is `fs` or higher.

Closest CLI tools:

- `vulcan note get`
- `vulcan query`
- `vulcan note links`
- `vulcan note backlinks`

See also: `help js.vault`, `help note get`, `help query`.
