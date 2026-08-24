# Outline publishing

Vulcan can package a selected Markdown hierarchy for Outline and can publish the same planned hierarchy into an existing Outline collection. Both paths are strictly one-way: the Markdown vault remains canonical and Outline is never used as publication input.

## Outline ZIP export

```sh
vulcan export outline-zip \
  --collection-title "Wiki" \
  --path wiki.zip \
  --block-reference-policy annotated-text \
  --excluded-target-policy annotated-text

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

Planning fails on duplicate folder notes, unsafe or case-insensitive archive collisions, unresolved internal links, links to excluded notes, and missing attachments. Obsidian block-reference targets use `--block-reference-policy error|plain-text|annotated-text|custom`: the backward-compatible `error` default fails closed, `plain-text` preserves only the visible label, and `annotated-text` preserves the label plus the authored destination in an inline-code annotation such as ``remote label (`Target#^block`)``. Query-selected partial exports use the parallel `--excluded-target-policy error|plain-text|annotated-text|custom` option. Its strict default rejects links to notes outside the selection; `annotated-text` avoids publishing a broken cross-collection link while retaining the excluded destination. Embeds retain their intent with a leading `!` in the annotation. `custom` invokes the trusted transform described below. All policies handle Markdown links and wikilinks, produce located diagnostics, and transform only exported content. Missing hierarchy parents are non-fatal warnings backed by generated placeholder documents. `--dry-run` writes no archive and includes the complete deterministic plan and diagnostics in JSON output. Existing output archives are never overwritten.

### Custom link transforms

Both unsupported block references and links outside a partial selection can use one vault-local JavaScript callback:

```sh
vulcan trust add
vulcan export outline-zip \
  'from notes where file.path starts_with "Projects/"' \
  --collection-title Wiki \
  --path wiki.zip \
  --block-reference-policy custom \
  --excluded-target-policy custom \
  --link-transform .vulcan/transforms/outline-links.js
```

The script defines a synchronous global `transform_link(link)` function and returns exactly one `replacement` string:

```js
function transform_link(link) {
  const target = link.is_embed ? "!" + link.authored_target : link.authored_target;
  return {
    replacement: link.label + " (`" + target + "`)"
  };
}
```

The input contains `reason` (`block_reference` or `excluded_target`), `source_path`, `raw_text`, `link_kind`, `is_embed`, `display_text`, `label`, `authored_target`, `resolved_target`, `target_heading`, `target_block`, `line`, `column`, and `byte_offset`. The replacement is inserted as Markdown without additional escaping, allowing annotations, external links, footnotes, or other intentional fallbacks.

Custom transforms require explicit vault trust and a vault-relative `.js` path. They run in a pure QuickJS context with no vault, filesystem, network, shell, plugin, or tool APIs. Wall-clock time and randomness are rejected, asynchronous returns are rejected, each invocation has a 100 ms CPU limit, and the replacement is capped at 64 KiB. The global JavaScript memory and stack settings still apply. A compilation error stops planning; a per-link error becomes a located `transform_failure` diagnostic and prevents export or publication. JSON dry-run and publication reports include the transform path and BLAKE3 content hash, so the exact executable input is auditable. The source vault is never modified.

### ZIP limitations

- Compatibility is based on Outline 1.9.x's upstream `ExportDocumentTreeTask` and `ExportMarkdownZipTask` sibling-file layout and filename encoding.
- Obsidian note embeds become normal Markdown links because Outline has no equivalent transclusion in imported Markdown.
- Block-reference targets are rejected by default. Use `--block-reference-policy annotated-text` to preserve labels and authored targets, or `plain-text` for labels only. Heading targets on supported links remain URL fragments.
- Links to notes outside a partial export are rejected by default. Use `--excluded-target-policy annotated-text` to retain each visible label and authored destination without emitting a broken link, or `plain-text` for labels only.
- Custom link transforms are deliberately limited to link fallback rendering. General publication content transforms remain declarative stripping, metadata filtering, and ordered replacements so audience-safety rules stay inspectable without executing code.
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
block_reference_policy = "error" # error | plain-text | annotated-text | custom
excluded_target_policy = "error" # error | plain-text | annotated-text | custom
# link_transform = ".vulcan/transforms/outline-links.js" # required by either custom policy
```

The token value is not a valid profile field. Put it in the named environment variable; device-specific non-secret overrides such as `base_url` or `token_env` may go in ignored `.vulcan/config.local.toml`. Then preview and apply:

```sh
OUTLINE_API_TOKEN=... vulcan --output json publish outline wiki --dry-run
OUTLINE_API_TOKEN=... vulcan --output json publish outline wiki
# After reviewing a conflict plan, restore selected local projections:
OUTLINE_API_TOKEN=... vulcan --output json publish outline wiki \
  --overwrite-conflict Home.md \
  --overwrite-conflict Projects/Plan.md
# Or explicitly restore every reported managed conflict:
OUTLINE_API_TOKEN=... vulcan --output json publish outline wiki --overwrite-conflicts
```

The profile must select exactly one of `query`, `query_json`, or the shared additive `selection` plan. A selection plan may union query clauses and bounded or recursive graph traversals from multiple seeds; its global exclusions and permission boundaries stop traversal. The profile may also contain the same ordered `[[publish.outline.profiles.wiki.content_transforms]]` rules used by export profiles. Set `remove_toc = true` to enable the optional heading-link TOC cleanup. Set either link policy to `annotated-text` to preserve visible labels and authored destinations, or `plain-text` when labels alone are sufficient. Set a policy to `custom` and configure `link_transform` to use the same callback contract as ZIP export. Publishing uses the same folder-note, callout/frontmatter compatibility, resolved-link, attachment, collision, excluded-target validation, and pure transform runtime as ZIP export. Generated folder-placeholder and fallback warnings are printed in human output and included in the JSON publish report's `diagnostics` array; custom-transform path/hash provenance is also included. Unlike ZIP-relative links, direct API publication rewrites links between managed documents to `/doc/<remote-id>` targets after durable mappings are known.

Vulcan uses Outline's documented `documents.list`, `documents.info`, `documents.create`, `documents.update`, `documents.move`, `documents.archive`, and `attachments.create` POST APIs. Collection listing is paginated. Requests have bounded timeouts and retries; `429 Too Many Requests` responses honor Outline's `Retry-After` delay (including fractional seconds) before consuming a retry, while transient transport and server failures use bounded exponential backoff. Credentials and response bodies that appear credential-bearing are not included in errors. Attachment uploads support Outline's returned POST-form and PUT upload modes. See Outline's [official API documentation](https://docs.getoutline.com/s/guide/doc/api-1rEIXDfLF6) and [OpenAPI specification](https://github.com/outline/openapi/blob/main/spec3.yml).

### Mapping and reconciliation

Durable mappings are stored in `.vulcan/publish/outline/<profile>.json`, outside the rebuildable SQLite cache. Writes use an exclusive lock, a temporary file, `fsync`, and atomic rename. Each entry records Vulcan's own source identity, current source path and cache document ID, remote document ID and parent, last published title/content hash, and attachment IDs, URLs, owners, and hashes.

Cache document IDs are hints, not durable synchronization identity. Vulcan first matches an existing cache ID (which preserves ordinary indexed moves), then the last path, then a unique prior content hash. Ambiguous recovery fails safe by planning a create/archive pair rather than claiming the wrong remote document.

Reconciliation creates parents before children, updates changed Markdown, moves changed parents, uploads changed attachments, and archives managed documents whose local source is no longer selected. It never permanently deletes documents and never changes remote documents absent from the mapping state. State is saved after each successful remote operation, and new documents use a preselected UUID so an interrupted create can be looked up and adopted on retry.

Before any mutation, Vulcan obtains the current managed documents through the paginated collection listing and compares their title, body, and parent with durable last-published hashes and the desired local projection. It does not issue a second `documents.info` request for every listed document, and it sends `documents.update` only for a planned content/title overwrite or when newly uploaded attachment URLs change rendered Markdown. A changed remote title, body, or parent is a conflict unless it already equals the desired result of an interrupted prior publication. Conflicts stop the entire mutation pass by default. Each conflict action includes a typed conflict kind, whether the remote object is missing, changed content/title/parent dimensions, and base/local/remote hashes, titles, and parent IDs. Content itself is not copied into reports.

After reviewing the conflict plan, repeat `--overwrite-conflict <source-path>` to authorize only the named managed conflicts. Any remaining conflict still stops the entire mutation pass, so a selective live run cannot partially mutate the collection by accident. Use `--overwrite-conflicts` only when every reported managed conflict should be replaced by the canonical local projection. These controls update or move remotely edited managed documents, recreate missing managed documents that still have a local source, archive remotely edited managed documents removed from the selection, and adopt already-missing removed documents. They never touch unmanaged remote documents. JSON plans report the number of `overwritten_conflicts`. Both controls work with `--dry-run`, which performs remote reads only and creates neither locks nor mapping directories.

Human output reports selection, compatibility preparation, remote planning, document hierarchy reconciliation, attachment upload, content update, archival, and completion on stderr, with per-item checkpoints and source paths. `--quiet` suppresses this progress, and JSON/Markdown output remains clean for automation.

### API publisher limitations

- Publishing is one-way. It does not ingest Outline changes, webhooks, or the separate Outline-to-Git backup/audit trail.
- A simultaneous note move and content edit after a complete cache rebuild cannot always be identified without a source marker. Vulcan intentionally does not mutate frontmatter; if cache ID, old path, and prior hash all differ, it treats the file as a new source.
- Replacing a changed attachment uploads a new Outline attachment and rewrites managed document links. Old, now-unreferenced attachment objects are left for Outline's own cleanup because the public API has no archive operation for attachments.
- Compatibility targets Outline 1.9.x and the current official API. Validate against a staging collection before upgrading across a major Outline release.

### Planned inbound route

A separate scoped `Outline -> Vulcan -> local Markdown` route is now planned as part of Phase 15's external knowledge-hub architecture. The pure inbound Markdown compatibility primitives already convert supported Outline callout fences into Obsidian callouts and mapped `/doc/<remote-id>` links into wikilinks. The route still needs to supply durable ID-to-path bindings and perform hierarchy and attachment materialization. The concrete use case is a canonical local wiki that can ingest selected external knowledge and then publish independently selected local content to Outline or other systems.

This does not change the command described above: `vulcan publish outline` remains one-way and the current publisher never consumes Outline as source data. The future inbound route will have its own remote scope, local destination namespace, authority and deletion policy, durable revision state, hierarchy/attachment materialization, pagination and rate limits, and local/remote drift handling. Pull and push remain separately planned and journaled operations rather than implicit bidirectional synchronization. See [Local information hub and external wikis](information-hub.md).
