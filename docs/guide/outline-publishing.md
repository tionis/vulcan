# Outline publishing

Vulcan can package a selected Markdown hierarchy for Outline and can publish the same planned hierarchy into an existing Outline collection. Both paths are strictly one-way: the Markdown vault remains canonical and Outline is never used as publication input.

## Outline ZIP export

```sh
vulcan export outline-zip "from notes" \
  --collection-title "Wiki" \
  --path wiki.zip

vulcan --output json export outline-zip "from notes" \
  --collection-title "Wiki" \
  --path wiki.zip \
  --dry-run
```

The archive layout follows Outline 1.9.x Markdown exports. An Outline document with children is represented by a Markdown file and a sibling directory with the same name:

```text
Wiki/
  Projects.md
  Projects/
    Child.md
```

Vulcan converts both common Obsidian folder-note conventions into that layout:

```text
Projects/Projects.md
Projects/index.md
```

Only one convention may be present for a folder. Nested folders must have an included folder note at every hierarchy level; Vulcan does not invent synthetic remote documents for missing folder notes.

The exporter uses the publication query and content-transform pipeline, then reparses transformed Markdown and uses resolved link data to rewrite note links and attachment references. Referenced attachments are copied below a deterministic `uploads/<source-path-hash>/` path. The source vault is never modified.

Planning fails on duplicate folder notes, unsafe or case-insensitive archive collisions, unresolved internal links, links to excluded notes, missing hierarchy parents, missing attachments, and Obsidian block-reference targets. `--dry-run` writes no archive and includes the complete deterministic plan and diagnostics in JSON output. Existing output archives are never overwritten.

### ZIP limitations

- Compatibility is based on Outline 1.9.x's upstream `ExportDocumentTreeTask` and `ExportMarkdownZipTask` sibling-file layout and filename encoding.
- Obsidian note embeds become normal Markdown links because Outline has no equivalent transclusion in imported Markdown.
- Block-reference targets are rejected. Heading targets are retained as URL fragments.
- A directory without an included `index.md` or same-name folder note cannot be represented as a document parent and is rejected.

## API publishing

The `publish outline` profile, durable mapping state, reconciliation behavior, and attachment upload configuration are documented below as those milestones are implemented.
