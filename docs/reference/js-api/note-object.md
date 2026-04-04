`vault.note(...)` currently returns the Dataview-style page object for one note, which wraps indexed note metadata and selected content accessors.

Current shape is centered on page/file metadata:

- identity: `path`, `name`, `aliases`
- file metadata: timestamps, tags, headings, frontmatter, `file.day`
- query/search interop through `vault.query()` and `vault.search()`
- graph traversal through `vault.graph.*`

Guidance:

- Resolve a note once, then operate on its typed fields instead of reparsing raw markdown in JS.
- Prefer structured fields and `vault.query()` when possible.
- Use CLI mutations for writes until transaction-style JS APIs land in the general runtime.

Closest CLI tools:

- `vulcan note get`
- `vulcan query`
- `vulcan note links`
- `vulcan note backlinks`

See also: `help js.vault`, `help note get`, `help query`.
