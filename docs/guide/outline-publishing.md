# Outline publishing

Vulcan can package a selected Markdown hierarchy for Outline, publish it into an existing collection, pull an Outline collection or subtree into a contained vault namespace, and compose pull/push into a named conflict-aware route. The Markdown vault remains the inspectable hub; synchronization is never implicit last-writer-wins.

## First-class Outline routes

A route makes a collaboration wiki repeatable by persisting its local root, remote subtree, direction, authority, exact document bindings, removal policy, work limits, and interval. It reuses the named `[publish.outline.profiles.<name>]` profile for the endpoint, collection, credentials, outgoing selection, and rendering transforms:

```toml
[publish.outline.profiles.players]
base_url = "https://outline.example.com"
collection_id = "00000000-0000-0000-0000-000000000000"
collection_title = "Players Wiki"
query = 'from "Players/Campaign"'
token_env = "OUTLINE_API_TOKEN"

[integrations.routes.players]
connector = "outline"
profile = "players"
direction = "mirror" # pull | push | mirror
authority = "review" # local | remote | review
local_root = "Players/Campaign"
remote_roots = ["outline-root-document-id"] # omit for the complete collection
max_depth = 4
excluded_documents = ["private-outline-subtree-id"]
document_bindings = { "existing-outline-id" = "Players/Campaign/Setting.md" }
apply_remote_moves = true
missing_policy = "archive" # retain | archive
missing_archive = "Archive/Outline"
stale_attachment_policy = "retain"
max_documents = 5000
schedule = "every 15m" # also @hourly, @daily, @weekly, every Nh, every Nd
```

Validate and operate it directly, without a daemon:

```sh
vulcan integration list
vulcan integration show players
vulcan integration validate players
OUTLINE_API_TOKEN=... vulcan integration plan players
OUTLINE_API_TOKEN=... vulcan integration run players --interactive
vulcan integration status players
vulcan integration bindings players

# A cron/systemd timer can invoke this frequently; only due routes run.
OUTLINE_API_TOKEN=... vulcan integration run --scheduled
```

`review` pulls first and stops on unresolved local/remote drift before pushing. `remote` pulls with reviewed remote-overwrite authority, then publishes the successfully materialized canonical vault state. `local` publishes first with managed-conflict overwrite authority, then pulls remote-only documents. A live route holds a route lock and writes running/completed/failed checkpoints under `.vulcan/integrations/routes/`; an interrupted run remains visible in `status`. `plan` and `run --dry-run` do not create route locks or state.

Route validation rejects missing profile fields, unsafe/internal paths, invalid selectors and limits, duplicate exact paths, overlapping local pull ownership, overlapping remote scope, and shared push state. Retain is the default removal policy. Route configuration deliberately offers recoverable archive policy but not unattended permanent deletion.

To associate an existing local note with an existing Outline document without rewriting either side:

```sh
OUTLINE_API_TOKEN=... vulcan integration bind players \
  <outline-document-id> Players/Campaign/Setting.md --dry-run
OUTLINE_API_TOKEN=... vulcan integration bind players \
  <outline-document-id> Players/Campaign/Setting.md
vulcan integration unbind players <outline-document-id> --dry-run
vulcan integration unbind players <outline-document-id>
```

Binding uses the immutable Outline ID and records a three-way baseline in durable pull state. Unbinding deletes neither document. `document_bindings` supplies deterministic ID-to-path overrides for route planning; adopt an already divergent local file explicitly before the first local-authority push.

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

# Reuse the same selection and rendering contract as API publication:
vulcan export outline-zip --profile wiki --path wiki.zip
```

As with every query-based export command, omitting both the positional query and `--query-json` selects the full vault (`from notes`). Pass either form when you want a filtered export.

`--profile <name>` instead resolves `[publish.outline.profiles.<name>]` and reuses exactly one configured `query`, `query_json`, or additive `selection`, plus `collection_title`, `content_transforms`, `link_transform`, `block_reference_policy`, `excluded_target_policy`, and `remove_toc`. The ZIP `--path` remains an invocation-local destination. ZIP export does not read or require `base_url`, `collection_id`, `token_env`, retry, timeout, or pagination fields, so an archive-only profile may omit all API settings. Profile mode conflicts with every direct selection, transform, title, TOC, and link-policy flag; define the complete rendering contract in one place rather than partially overriding it. API publication reports `projections` with source paths and rendered content hashes, allowing automation and tests to compare its selected projection directly with the ZIP plan.

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
OUTLINE_API_TOKEN=... vulcan publish outline wiki --interactive
# Explicitly reuse selected documents previously pulled by this profile:
OUTLINE_API_TOKEN=... vulcan publish outline wiki --adopt-pulled --dry-run
OUTLINE_API_TOKEN=... vulcan publish outline wiki --adopt-pulled
```

The profile must select exactly one of `query`, `query_json`, or the shared additive `selection` plan. A selection plan may union query clauses and bounded or recursive graph traversals from multiple seeds; its global exclusions and permission boundaries stop traversal. The profile may also contain the same ordered `[[publish.outline.profiles.wiki.content_transforms]]` rules used by export profiles. Set `remove_toc = true` to enable the optional heading-link TOC cleanup. Set either link policy to `annotated-text` to preserve visible labels and authored destinations, or `plain-text` when labels alone are sufficient. Set a policy to `custom` and configure `link_transform` to use the same callback contract as ZIP export. Publishing uses the same folder-note, callout/frontmatter compatibility, resolved-link, attachment, collision, excluded-target validation, and pure transform runtime as ZIP export. Generated folder-placeholder and fallback warnings are printed in human output and included in the JSON publish report's `diagnostics` array; custom-transform path/hash provenance is also included. Unlike ZIP-relative links, direct API publication rewrites links between managed documents to `/doc/<remote-id>` targets after durable mappings are known.

Vulcan uses Outline's documented `documents.list`, `documents.info`, `documents.create`, `documents.update`, `documents.move`, `documents.archive`, and `attachments.create` POST APIs. Collection listing is paginated. Requests have bounded timeouts and retries; `429 Too Many Requests` responses honor Outline's `Retry-After` delay (including fractional seconds) before consuming a retry, while transient transport and server failures use bounded exponential backoff. Credentials and response bodies that appear credential-bearing are not included in errors. Attachment uploads support Outline's returned POST-form and PUT upload modes. See Outline's [official API documentation](https://docs.getoutline.com/s/guide/doc/api-1rEIXDfLF6) and [OpenAPI specification](https://github.com/outline/openapi/blob/main/spec3.yml).

### Mapping and reconciliation

Durable mappings are stored in `.vulcan/publish/outline/<profile>.json`, outside the rebuildable SQLite cache. Writes use an exclusive lock, a temporary file, `fsync`, and atomic rename. Each entry records Vulcan's own source identity, current source path and cache document ID, remote document ID and parent, the submitted local title/content hash, the title/content hash/parent returned by Outline after a successful mutation, and attachment IDs, URLs, owners, and hashes. Keeping the local projection and observed remote representation separate accounts for Outline's Markdown normalization while preserving remote-drift detection.

Cache document IDs are hints, not durable synchronization identity. Vulcan first matches an existing cache ID (which preserves ordinary indexed moves), then the last path, then a unique prior content hash. Ambiguous recovery fails safe by planning a create/archive pair rather than claiming the wrong remote document.

Reconciliation creates parents before children, updates changed Markdown, moves changed parents, uploads changed attachments, and archives managed documents whose local source is no longer selected. It never permanently deletes documents and never changes remote documents absent from the mapping state. State is saved after each successful remote operation, and new documents use a preselected UUID so an interrupted create can be looked up and adopted on retry.

Large publications build in-memory indexes for durable identities, source paths, planned actions, document destinations, and attachment destinations once per run. Link rewriting then scales with links actually present in each document instead of comparing every selected document with every other selected document. Network behavior remains conservative: reconciliation still obtains the paginated collection view needed for remote-drift detection.

Before any mutation, Vulcan obtains the current managed documents through the paginated collection listing and compares their title, body, and parent with the last representation observed from Outline and the desired local projection. It does not issue a second `documents.info` request for every listed document, and it sends `documents.update` only for a planned content/title overwrite or when newly uploaded attachment URLs change rendered Markdown. A changed remote title, body, or parent is a conflict unless it already equals the desired result of an interrupted prior publication. Conflicts stop the entire mutation pass by default. Each conflict action includes a typed conflict kind, whether the remote object is missing, changed content/title/parent dimensions, and base/local/remote hashes, titles, and parent IDs. Content itself is not copied into reports.

Mapping files created by older Vulcan versions do not contain an observed-remote snapshot. Vulcan deliberately retains the conservative old baseline instead of silently trusting current remote content, because that content could include a genuine remote edit. If an upgrade therefore reports normalization-only conflicts for otherwise unchanged documents, review the dry-run and authorize those documents once with selective `--overwrite-conflict` flags (or `--overwrite-conflicts` only after reviewing every item). The successful publication records Outline's normalized representation, so later unchanged publications remain idempotent.

After reviewing the conflict plan, repeat `--overwrite-conflict <source-path>` to authorize only the named managed conflicts. Any remaining conflict still stops the entire mutation pass, so a selective live run cannot partially mutate the collection by accident. Use `--overwrite-conflicts` only when every reported managed conflict should be replaced by the canonical local projection. These controls update or move remotely edited managed documents, recreate missing managed documents that still have a local source, archive remotely edited managed documents removed from the selection, and adopt already-missing removed documents. They never touch unmanaged remote documents. JSON plans report the number of `overwritten_conflicts`. Both controls work with `--dry-run`, which performs remote reads only and creates neither locks nor mapping directories.

For human terminal use, `--interactive` displays each conflict's path, kind, and changed local/remote dimensions and asks whether the canonical local version may overwrite it. `all` approves the current and remaining conflicts; `no`, Enter, EOF, or quit cancels without mutation. After approval Vulcan fetches and plans the collection again before applying, so changes made while the prompt was open are not overwritten from a stale plan. Interactive mode is intentionally unavailable with `--dry-run`, structured output, redirected input, or the non-interactive overwrite flags.

Human output reports selection, compatibility preparation, remote planning, document hierarchy reconciliation, attachment upload, content update, archival, and completion on stderr, with per-item checkpoints and source paths. `--quiet` suppresses this progress, and JSON/Markdown output remains clean for automation.

### API publisher limitations

- Publishing is one-way. It does not ingest Outline changes, webhooks, or the separate Outline-to-Git backup/audit trail.
- A simultaneous note move and content edit after a complete cache rebuild cannot always be identified without a source marker. Vulcan intentionally does not mutate frontmatter; if cache ID, old path, and prior hash all differ, it treats the file as a new source.
- Replacing a changed attachment uploads a new Outline attachment and rewrites managed document links. Old, now-unreferenced attachment objects are left for Outline's own cleanup because the public API has no archive operation for attachments.
- Compatibility targets Outline 1.9.x and the current official API. Validate against a staging collection before upgrading across a major Outline release.

### Outline pull into the vault

The focused inbound command reuses an Outline profile's endpoint, token environment variable, collection ID, timeout, retry, and pagination settings. The destination is always explicit and contained in the vault:

```sh
OUTLINE_API_TOKEN=... vulcan pull outline wiki --into Imported/Outline --dry-run
OUTLINE_API_TOKEN=... vulcan pull outline wiki --into Imported/Outline
OUTLINE_API_TOKEN=... vulcan pull outline wiki --into Imported/Outline --interactive
OUTLINE_API_TOKEN=... vulcan pull outline wiki --into Imported/Outline --conflict-markers
OUTLINE_API_TOKEN=... vulcan pull outline wiki --into Imported/Outline --overwrite-conflicts
vulcan pull outline wiki --into Imported/Outline --conflict-operation status
OUTLINE_API_TOKEN=... vulcan pull outline wiki --into Imported/Outline --conflict-operation continue
vulcan pull outline wiki --into Imported/Outline --conflict-operation abort --dry-run
vulcan pull outline wiki --into Imported/Outline --conflict-operation abort
OUTLINE_API_TOKEN=... vulcan pull outline wiki --into Imported/Outline --archive-missing Archive/Outline
OUTLINE_API_TOKEN=... vulcan pull outline wiki --into Imported/Outline --delete-missing --confirm-delete-count 2
OUTLINE_API_TOKEN=... vulcan pull outline wiki --into Imported/Outline --root-document <remote-id> --max-depth 2
OUTLINE_API_TOKEN=... vulcan pull outline wiki --into Imported/Outline --exclude-document <private-root-id>
OUTLINE_API_TOKEN=... vulcan pull outline wiki --into Imported/Outline --max-documents 2500
OUTLINE_API_TOKEN=... vulcan pull outline wiki --into Imported/Outline --archive-stale-attachments Archive/OutlineAssets
OUTLINE_API_TOKEN=... vulcan pull outline wiki --into Imported/Outline --delete-stale-attachments --confirm-stale-attachment-delete-count 3
```

The initial pull maps the collection hierarchy to `Parent.md` plus `Parent/Child.md`, rejects unsafe paths and orphaned parents, converts supported Outline callout fences to Obsidian callouts, and rewrites links to other pulled `/doc/<remote-id>` documents as wikilinks. Case-, Unicode-normalization-, and truncation-equivalent sibling names receive deterministic remote-ID-derived suffixes; generated components also avoid Windows reserved names and byte/path overflows. Inline, angle-bracket, balanced-parenthesis, and reference-style Markdown destinations are parsed through the Markdown event stream; code spans and fences are not mistaken for links. Referenced `/api/attachments.redirect` assets are authenticated against the configured Outline origin, downloaded with a 25 MiB per-file limit, written under deterministic `<destination>/_attachments/` paths, and rewritten to relative Markdown links. Dry-run and live reports include structured diagnostics when raw HTML or unsupported directives are preserved verbatim, or when an attachment destination cannot be safely rewritten. Durable state lives under `.vulcan/integrations/outline-pull/`: focused commands use `<profile>.json`, while named routes use `<route>.json` so disjoint routes can safely reuse one API profile. The compact manifest records connector identity, mappings, hashes, revision metadata, and journals, while verified content-addressed files under `sources/` retain exact remote and diff3-base bodies without rewriting every body at each checkpoint. State is locked and atomically replaced outside `cache.db`; changing a binding to another server fails closed instead of silently reusing identities.

Pulls default to a 10,000-document and 256 MiB cumulative-Markdown ceiling, configurable with `--max-documents` and `--max-content-bytes`. Attachment defaults are 10,000 references, 25 MiB per download, and 1 GiB downloaded per invocation; tune them with `--max-attachments`, `--max-attachment-bytes`, and `--max-total-attachment-bytes`. API JSON bodies are independently capped at 64 MiB before parsing. Limits fail before mutation whenever the required size is knowable; a cumulative download overflow leaves the interruption journal available for inspection/retry. Outline's offset pagination does not provide a true collection snapshot cursor, so Vulcan verifies that the advertised total remains stable, rejects duplicate documents, and requires the final item count to match. It also validates required Markdown document fields, collection membership, complete acyclic hierarchy, and consistent collection-wide `revision`/`updatedAt` metadata coverage. Live pulls list the collection a second time immediately before the first mutation and require an identical ID-sorted snapshot. JSON reports expose this as `remote_capabilities`, including whether the pre-apply re-list occurred; retry when either listing or conformance check reports collection drift.

Outline does not expose a stable server-semver endpoint as part of the documented RPC contract, so Vulcan does not infer compatibility from an unreliable version string. The connector instead targets the documented Markdown document API contract and fails closed on malformed success envelopes, missing bodies, unsupported hierarchy, inconsistent optional provenance fields, or pre-apply snapshot drift. Validate a staging collection before major Outline upgrades, especially when the official API contract changes.

A live pull holds Vulcan's shared vault write lock across its fresh plan, filesystem mutations, and final incremental scan. Before writing, it persists a stable operation ID and the pending action set; each successfully completed action updates the mapping and journal together. An interruption, cooperative cancellation, attachment error, or scan error leaves the journal in place, and the next live pull reports `resumed_operation = true`, reuses the operation ID, and reconciles the remaining fresh plan. Human output reports remote listing, planning, document application, attachment downloads, scanning, and completion on stderr; `--quiet` suppresses it and structured stdout remains clean. Ordinary mutation-triggered auto-commit remains opt-in and suppressible with `--no-commit`.

Local-only edits are preserved when Outline has not changed. If both sides changed, the default is a mutation-free conflict. `--overwrite-conflicts` chooses the current Outline projection for every conflict. `--conflict-markers` runs Git's line-oriented three-way merge against the durable base: non-overlapping local and Outline changes are combined automatically and reported as `auto_merged`, while only overlapping hunks receive `LOCAL`, `BASE`, and `OUTLINE` markers. Repeating the pull reconstructs the local side of existing Vulcan diff3 markers instead of nesting them. `--interactive` chooses overwrite or merge per path and re-plans before applying; a resolved marker file that exactly matches the current Outline projection is adopted on the next default pull. Existing unmanaged local files are never claimed unless their content already matches the planned Outline document or the user explicitly resolves the collision.

Conflict-marker lifecycle operations are explicit. `--conflict-operation status` scans managed files and reports complete or malformed Vulcan marker blocks without contacting Outline or requiring a token. After resolving every file, `--conflict-operation continue` refuses to contact or apply Outline while any marker artifact remains, then performs a normal freshly planned pull. `--conflict-operation abort` restores the `LOCAL` side of every complete diff3 block under the vault lock; it validates and authorizes the whole set before writing, rejects malformed markers, supports `--dry-run`, incrementally rescans, and participates in opt-in auto-commit. The operation journal is reported separately because a successful marker-writing pull can correctly finish its journal while leaving human reconciliation work behind.

A managed note that disappears locally while its Outline document still exists is a conflict, not an unchanged result. The default preserves the deletion without changing durable state; a reviewed overwrite restores the remote projection. Interactive mode offers marker choices only for text conflicts with an existing local file. Missing-note and attachment conflicts offer overwrite or cancellation because binary absence/drift cannot be represented by Markdown diff3 markers. JSON actions expose `conflict_markers_available` so non-interactive clients can present the same valid choices.

Existing remote bindings keep stable local paths by default. `--apply-remote-moves` instead maps current Outline titles and parents to destination paths, preflights each move through Vulcan's link-aware refactoring workflow, and reports source, destination, and rewritten backlink files. Occupied destinations fail closed. A remote-only hierarchy change can move a locally edited note without replacing its content; simultaneous remote body and local content changes remain a conflict.

Documents missing from the complete configured remote collection remain local by default. `--archive-missing <vault-directory>` moves each missing managed note to a deterministic recoverable path, rewrites resolvable backlinks, and clears its active mapping so a later remote return materializes normally. `--delete-missing` permanently removes the managed note and its mapped attachments only when `--confirm-delete-count` exactly matches the fresh live plan. `--interactive` offers retain, archive under `<destination>/_removed`, or delete per missing document; those per-item delete choices supply the confirmation. Unmanaged files are never included.

When an Outline document stops referencing a previously materialized attachment, Vulcan keeps both the local file and its durable mapping by default and reports `stale_attachment`; it does not silently turn the file into an unmanaged orphan. `--archive-stale-attachments <vault-directory>` moves each stale managed asset to a deterministic recoverable path and removes its active attachment binding. `--delete-stale-attachments` permanently removes only stale managed assets and requires `--confirm-stale-attachment-delete-count` to exactly match the fresh live plan. Reports separate `stale_attachments`, `archived_stale_attachments`, and `deleted_stale_attachments`.

For a partial collection pull, repeat `--root-document <remote-id>` to select the union of one or more remote subtrees. Add `--max-depth <n>` to bound descendants (`0` means the roots only), and repeat `--exclude-document <remote-id>` to cut out named subtrees. Selectors use durable Outline document IDs rather than mutable titles. Vulcan still enumerates the complete configured collection before planning: active managed documents outside the selector are reported as `out_of_scope` and are never passed to retain/archive/delete handling. Existing mappings outside the scope remain available for link translation, but their files and state are not changed.

Pull and publication remain separate reviewed operations, but `publish outline --adopt-pulled` provides explicit identity continuity between them. It considers only pulled paths selected by the publication profile, verifies that each remote document still matches the content hash, title, parent, and collection recorded by the last successful pull, and rejects duplicate ownership. Successful adoption reuses remote document identities and attachment URLs instead of creating duplicates; local edits made after the pull are then planned as normal publication updates. Always review adoption with `--dry-run`; JSON reports expose `adopted_pull_bindings`.

The focused `pull outline` and `publish outline` commands remain compatible lower-level surfaces. Named routes compose those same planners and durable identities; future generic connectors and daemon-triggered execution will reuse this route contract. See [Local information hub and external wikis](information-hub.md).
