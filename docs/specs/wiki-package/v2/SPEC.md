# Markdown Wiki Package version 2

A Markdown Wiki Package is an immutable snapshot of a Markdown wiki. Version 2 is a [Container Core v1](../../container-core/v1/SPEC.md) format, and it records how the wiki was built and which sources it came from. A producer (a rendering pipeline, an analysis recipe, or Vulcan's own export) decides the note tree. The consumer imports that tree as it is; it does not split a document into notes itself.

Use [MDAF](../../mdaf/v1/SPEC.md) for one document extracted from source media, and Wiki Package v2 for the wiki built from it. A Wiki Package may cite an MDAF artifact, a game-script database, or any other source by declaring it and addressing it with locators.

The package is a `.wikibundle` directory or a `.wikipack` ZIP file. Both serializations have the same logical identity. Version 1 packages remain valid (see [v1](../v1/SPEC.md)). A reader dispatches on `wiki.json` `version`.

## Layout

```text
wiki.json           required manifest
provenance.json     required activity graph
content/            the wiki: notes (*.md) and assets
source-map.jsonl    optional note-to-source mappings and references
knowledge.jsonl     optional Knowledge v1 snapshot
sources/            optional embedded source documents
environments/       optional locks, inventories, or SBOMs
extensions/<ns>/    optional producer data, namespaced by reverse domain
```

## Manifest

`wiki.json` conforms to [`wiki.schema.json`](wiki.schema.json) and rejects unknown fields.

| Field | Meaning |
|---|---|
| `format` | `dev.tionis.markdown-wiki-package` |
| `version` | `2` |
| `title` | Optional display title. |
| `root` | Optional declared note that serves as the wiki's entry page. |
| `producer` | `name` and `version`, plus an optional `revision`. |
| `members` | Every regular file except `wiki.json`, declared exactly once. |
| `sources` | The Container Core source table. It may be empty. |
| `derived_from` | Logical identities of parent packages. |

### Member roles

| Role | Path | Rules |
|---|---|---|
| `note` | `content/**/*.md` (the extension is case-insensitive) | `media_type` is `text/markdown`, and the bytes are UTF-8. An optional `document_id` is the producer's durable identity and is unique across notes. |
| `asset` | any other path below `content/` | |
| `provenance` | `provenance.json` | Required. |
| `source-map` | `source-map.jsonl` | Optional. |
| `knowledge` | `knowledge.jsonl` | Optional, a Knowledge v1 document. |
| `source` | `sources/**` | Referenced by a source's `embedded_path`. |
| `environment` | `environments/**` | Opaque to consumers. |
| `extension` | `extensions/<namespace>/**` | `namespace` is required and equals the path segment. Opaque to consumers. |

Every member's `created_by` names a provenance activity that lists the member in its outputs. That includes `provenance.json` itself. A producer typically has an extraction activity that emits embedded sources, an analysis activity that emits `knowledge.jsonl` with its models recorded, and a rendering activity that emits the notes, assets, source map, and provenance.

## Source map

`source-map.jsonl` ties spans of note text to source segments. It is UTF-8 JSON Lines with LF-terminated lines and no blank lines. Each record conforms to [`source-map-record.schema.json`](source-map-record.schema.json).

1. The first and only header is `{"record":"header","format":"dev.tionis.wiki-source-map","version":1}`.
2. `mapping` records have `note`, `document` (a byte span), `source` (a locator), and optional `confidence` and namespaced `method`. They state that the note text in the span was derived from the located source segment.
3. `reference` records have `note`, `document`, `target` (a locator), and an optional `kind`. They state that the note text in the span refers to the target, for example a character name that points at a script label.

Records after the header are sorted by note path bytes, then `document.start`, then `document.end`. Mappings and references may interleave. Mappings may overlap, may be partial, and may repeat a span for several sources.

Each span addresses the declared bytes of a declared note. The manifest digest binds those bytes, so no per-record digest is needed. Each locator names a declared source.

## Knowledge

`knowledge.jsonl` is a [Knowledge v1](../../knowledge/v1/SPEC.md) snapshot hosted by the package. Its evidence locators resolve against the package's `sources`. Entity `note` and claim `notes` references must name declared notes, and their spans must be valid in those notes. Producers render entity and claim pages themselves. The snapshot makes those pages citable and machine-readable; it does not replace them.

## Validation

Readers apply the Container Core container, path, archive, digest, and identity rules. They also check:

- the manifest schema, and the role, path, and namespace table;
- `document_id` uniqueness;
- that `root` names a declared note;
- embedded-source digests;
- the provenance schema, emission of every member, parameter digests, and acyclic dependencies;
- that notes are UTF-8;
- the source-map header, order, notes, spans, and locators;
- the Knowledge v1 rules.

## Import

Vulcan imports a valid package into a new, explicit vault destination:

- Each `content/` member is materialized at `<destination>/<path below content/>`.
- Notes without source-map mappings are copied byte for byte. For a note with mappings, Vulcan adds a `vulcan.source` frontmatter entry:

  ```yaml
  vulcan:
    source:
      artifact: blake3:<package identity>
      member: content/Characters/Alice.md
      spans:
        - start: 30
          end: 75
          locators:
            - source_id: script
              selectors: [{type: interval, unit: line, start: 2, end: 3, origin: 1}]
              confidence: 0.9
              method: dev.tionis.renwiki/dialogue
  ```

  Spans address the package member's bytes, not the annotated note. This is the same shape that MDAF import writes. An existing `vulcan.source` entry makes the import fail before anything is written.
- Provenance, the source map, knowledge, sources, environments, and extensions are validated and summarized. They are not materialized, and the package stays external evidence.
- Vulcan refreshes the cache after writing and removes the destination if writing or reindexing fails.

Import never merges into an existing tree. Importing a later snapshot goes into a new destination.

## Export

`vulcan exchange wiki export` writes version 2. The package contains the vault's regular files below `content/`, and a `provenance.json` with one `vulcan.wiki-export` activity. Its source table is empty, and it has no source map or knowledge.

## Test vector

[`identity-test-vector.json`](identity-test-vector.json) gives the canonical records and identity of [`examples/sourced.wikibundle`](examples/sourced.wikibundle). That synthetic example exercises the following:

- nested notes, an asset, and a root note;
- an embedded source;
- a three-activity provenance graph with a pinned model;
- source-map mappings and a reference;
- a knowledge snapshot with two entities and an attributed, scoped claim;
- a namespaced extension.

## Changes from version 1

- Identity follows Container Core, so it includes `wiki.json` and every sidecar. Version 1 hashed content members only.
- Provenance is required, and every member declares `media_type` and `created_by`.
- A source table, a source map, and a Knowledge v1 snapshot are added.
- Unknown manifest fields are rejected. Producer data moves to `extensions/<namespace>/`.
- `lineage` is renamed `derived_from`.
