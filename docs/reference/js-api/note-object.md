The planned `Note` JS object wraps indexed note metadata and selected content accessors.

Expected shape:

- identity: `path`, `name`, `aliases`
- file metadata: timestamps, tags, headings, frontmatter, `file.day`
- graph data: backlinks and outbound links
- helper methods for content extraction and lightweight mutation

Guidance:

- Resolve a note once, then operate on its typed fields instead of reparsing raw markdown in JS.
- Prefer structured fields and `vault.query()` when possible.
- Use transaction-style APIs for multi-note writes once those land in the general runtime.

Today, the closest CLI tools are `note get`, `query`, `links`, and `backlinks`.

See also: `help js.vault`, `help note get`, `help query`.
