# Markdown Wiki Package v1

Markdown Wiki Package is an extractor-neutral, immutable snapshot of a Markdown wiki. A package is either a directory ending in `.wikibundle` or a ZIP file ending in `.wikipack`.

## Layout

```text
wiki.json
content/
  Home.md
  Notes/Topic.md
  assets/image.png
```

`wiki.json` is UTF-8 JSON conforming to `wiki.schema.json`. Every regular file below `content/` is declared exactly once in `members`; undeclared, missing, duplicate, case-fold-colliding, non-NFC, absolute, traversing, symbolic, and special-file members are invalid. ZIP readers must enforce bounded entry counts, expanded sizes, individual sizes, and compression ratios.

Member paths include the `content/` prefix. A `note` member has a `.md` path and UTF-8 Markdown bytes. Every other member is an `asset`. `digest` is `blake3:<lowercase-hex>` over the exact member bytes. `document_id` is optional durable producer identity and must not be inferred from a path.

## Logical identity

The logical identity is independent of directory/ZIP serialization and ZIP metadata. Sort members by path and hash this UTF-8 JSON Lines projection with BLAKE3:

```json
{"path":"content/Home.md","role":"note","size":7,"digest":"blake3:..."}
```

Each object has exactly the keys above in that order, followed by `\n`. The identity is reported as `blake3:<lowercase-hex>`. Metadata, producer details, extensions, and lineage do not alter content identity.

## Semantics

`format` is `dev.tionis.markdown-wiki-package`, `version` is `1`, and `producer.name` identifies the producing tool. Unknown top-level fields are preserved by readers. `lineage` may carry prior package identities, but does not imply synchronization or mutable ancestry. Import materializes ordinary Markdown and assets into a new destination; those files then become canonical. Cache databases, Git data, credentials, device state, and application workspaces do not belong in a package.

SQLite may later serialize this same logical model, but it is not a writable Vulcan vault backend in v1.
