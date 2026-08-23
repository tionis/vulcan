# Outline publishing

Vulcan can package a selected Markdown hierarchy for Outline and can publish the same planned hierarchy into an existing Outline collection. Both paths are strictly one-way: the Markdown vault remains canonical and Outline is never used as publication input.

## Outline ZIP export

```sh
vulcan export outline-zip \
  --collection-title "Wiki" \
  --path wiki.zip

vulcan --output json export outline-zip \
  --collection-title "Wiki" \
  --path wiki.zip \
  --dry-run
```

As with every query-based export command, omitting both the positional query and `--query-json` selects the full vault (`from notes`). Pass either form when you want a filtered export.

The archive layout follows Outline 1.9.x Markdown exports. An Outline document with children is represented by a Markdown file and a sibling directory with the same name:

```text
Wiki/
  Projects.md
  Projects/
    Child.md
```

Vulcan converts the single convention configured for the repository into that layout. Runtime and export planning do not auto-detect conventions:

```toml
[folder_notes]
placement = "inside"       # inside | outside
name = "{{folder_name}}"   # exact stem/template: index, README, readme, ...
```

This represents `Projects/Projects.md`. Use `name = "index"`, `"README"`, or `"readme"` for those inside-folder forms. Use `placement = "outside"` with `name = "{{folder_name}}"` for `Projects.md` beside `Projects/`. Matching is exact and case-sensitive. When an included hierarchy level has no selected configured folder note, Vulcan emits one warning for that folder and adds a minimal export-only placeholder (`# Folder name`) so Outline can preserve the hierarchy. The source vault is not changed. `vulcan config import folder-notes` can import the Obsidian Folder Notes plugin setting during setup.

The exporter uses the publication query and content-transform pipeline, then applies the shared Outline compatibility pass. YAML frontmatter is stripped from the published body, Obsidian callouts are converted to Outline `:::info`, `:::tip`, `:::success`, or `:::warning` fences, and resolved note and attachment references become Markdown links suitable for Outline import. Pass `--remove-toc` to also strip Obsidian-style lists made entirely of current-note heading links. The transformed content is reparsed before packaging, so removed metadata and sections cannot retain links or copy otherwise-unused assets. Referenced attachments are copied below a deterministic `uploads/<source-path-hash>/` path. The source vault is never modified.

Planning fails on duplicate folder notes, unsafe or case-insensitive archive collisions, unresolved internal links, links to excluded notes, missing attachments, and Obsidian block-reference targets. Missing hierarchy parents are non-fatal warnings backed by generated placeholder documents. `--dry-run` writes no archive and includes the complete deterministic plan and diagnostics in JSON output. Existing output archives are never overwritten.

### ZIP limitations

- Compatibility is based on Outline 1.9.x's upstream `ExportDocumentTreeTask` and `ExportMarkdownZipTask` sibling-file layout and filename encoding.
- Obsidian note embeds become normal Markdown links because Outline has no equivalent transclusion in imported Markdown.
- Block-reference targets are rejected. Heading targets are retained as URL fragments.
- Generated folder placeholders contain only a heading and exist only in the ZIP or remote Outline collection. Add a real note matching `[folder_notes]` when the hierarchy needs authored landing-page content.

## API publishing

Configure a named target in shared `.vulcan/config.toml`:

```toml
[publish.outline.profiles.wiki]
base_url = "https://outline.example.com"
collection_id = "00000000-0000-0000-0000-000000000000"
collection_title = "Wiki"
query = "from notes"
token_env = "OUTLINE_API_TOKEN"
timeout_seconds = 30
max_retries = 3
page_size = 100
remove_toc = false
```

The token value is not a valid profile field. Put it in the named environment variable; device-specific non-secret overrides such as `base_url` or `token_env` may go in ignored `.vulcan/config.local.toml`. Then preview and apply:

```sh
OUTLINE_API_TOKEN=... vulcan --output json publish outline wiki --dry-run
OUTLINE_API_TOKEN=... vulcan --output json publish outline wiki
```

The profile must select exactly one of `query` or `query_json`. It may also contain the same ordered `[[publish.outline.profiles.wiki.content_transforms]]` rules used by export profiles. Set `remove_toc = true` to enable the optional heading-link TOC cleanup. Publishing uses the same folder-note, callout/frontmatter compatibility, resolved-link, attachment, collision, and excluded-target validation as ZIP export. Generated folder-placeholder warnings are printed in human output and included in the JSON publish report's `diagnostics` array. Unlike ZIP-relative links, direct API publication rewrites links between managed documents to `/doc/<remote-id>` targets after durable mappings are known.

Vulcan uses Outline's documented `documents.list`, `documents.info`, `documents.create`, `documents.update`, `documents.move`, `documents.archive`, and `attachments.create` POST APIs. Collection listing is paginated. Requests have bounded timeouts and retries; credentials and response bodies that appear credential-bearing are not included in errors. Attachment uploads support Outline's returned POST-form and PUT upload modes. See Outline's [official API documentation](https://docs.getoutline.com/s/guide/doc/api-1rEIXDfLF6) and [OpenAPI specification](https://github.com/outline/openapi/blob/main/spec3.yml).

### Mapping and reconciliation

Durable mappings are stored in `.vulcan/publish/outline/<profile>.json`, outside the rebuildable SQLite cache. Writes use an exclusive lock, a temporary file, `fsync`, and atomic rename. Each entry records Vulcan's own source identity, current source path and cache document ID, remote document ID and parent, last published title/content hash, and attachment IDs, URLs, owners, and hashes.

Cache document IDs are hints, not durable synchronization identity. Vulcan first matches an existing cache ID (which preserves ordinary indexed moves), then the last path, then a unique prior content hash. Ambiguous recovery fails safe by planning a create/archive pair rather than claiming the wrong remote document.

Reconciliation creates parents before children, updates changed Markdown, moves changed parents, uploads changed attachments, and archives managed documents whose local source is no longer selected. It never permanently deletes documents and never changes remote documents absent from the mapping state. State is saved after each successful remote operation, and new documents use a preselected UUID so an interrupted create can be looked up and adopted on retry.

Before any mutation, Vulcan fetches every managed remote document. A changed remote title, body, or parent is a conflict unless it already equals the desired result of an interrupted prior publication. Conflicts stop the entire mutation pass by default and are included in the structured report. `--dry-run` performs remote reads only and creates neither locks nor mapping directories.

### API publisher limitations

- Publishing is one-way. It does not ingest Outline changes, webhooks, or the separate Outline-to-Git backup/audit trail.
- A simultaneous note move and content edit after a complete cache rebuild cannot always be identified without a source marker. Vulcan intentionally does not mutate frontmatter; if cache ID, old path, and prior hash all differ, it treats the file as a new source.
- Replacing a changed attachment uploads a new Outline attachment and rewrites managed document links. Old, now-unreferenced attachment objects are left for Outline's own cleanup because the public API has no archive operation for attachments.
- Compatibility targets Outline 1.9.x and the current official API. Validate against a staging collection before upgrading across a major Outline release.

### Planned inbound route

A separate scoped `Outline -> Vulcan -> local Markdown` route is now planned as part of Phase 15's external knowledge-hub architecture. The pure inbound Markdown compatibility primitives already convert supported Outline callout fences into Obsidian callouts and mapped `/doc/<remote-id>` links into wikilinks. The route still needs to supply durable ID-to-path bindings and perform hierarchy and attachment materialization. The concrete use case is a canonical local wiki that can ingest selected external knowledge and then publish independently selected local content to Outline or other systems.

This does not change the command described above: `vulcan publish outline` remains one-way and the current publisher never consumes Outline as source data. The future inbound route will have its own remote scope, local destination namespace, authority and deletion policy, durable revision state, hierarchy/attachment materialization, pagination and rate limits, and local/remote drift handling. Pull and push remain separately planned and journaled operations rather than implicit bidirectional synchronization. See [Local information hub and external wikis](information-hub.md).
