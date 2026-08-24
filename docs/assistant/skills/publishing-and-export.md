---
name: publishing-and-export
description: Build static sites, export or package vault content, operate one-way Outline publications, and pull an explicitly scoped Outline collection into the vault. Use when the user asks about site builds, export profiles, EPUB/ZIP/SQLite/JSON/CSV output, Outline ZIP, API publishing or pull, graph-based publication scope, reconciliation conflicts, render diagnostics, publish filters, content transforms, or route/link policy.
version: 1
tools:
  - site
  - export
  - publish
  - render
  - query
  - graph
  - config_show
  - help
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Publishing and Export

## When to Use This Skill

Use this skill when the user wants vault content rendered, packaged, published, or diagnosed for a
static output target or a configured one-way Outline publication.

## Recommended Flow

- Use `vulcan render` for one Markdown file or stdin.
- Use `vulcan export ...` for one-off artifacts such as Markdown, JSON, CSV, graph, EPUB, ZIP, SQLite, search index, or frontend bundle outputs.
- Use export profiles for repeatable export settings.
- Use `vulcan site build --profile <name>` for static sites and `vulcan site doctor` for publish diagnostics.
- Omitting a query exports the full vault. Use a query for one selection rule or `--selection-json` for an additive plan that unions query clauses and bounded or recursive graph traversals. Review global exclusions and permission boundaries because they also stop graph traversal.
- Use `vulcan export outline-zip ... --dry-run` to inspect the complete Outline-compatible hierarchy and diagnostics before writing an archive.
- For API publication, inspect the configured profile and run `vulcan publish outline <profile> --dry-run` first. A dry run performs remote reads but does not mutate Outline or create mapping state.
- Review each structured remote-drift conflict's kind, changed dimensions, and base/local/remote metadata before a live publication. Prefer repeating `--overwrite-conflict <source-path>` for only the reviewed managed documents. Use `--overwrite-conflicts` only when every reported conflict should be replaced; check `overwritten_conflicts` in JSON output.
- When a person is present at a terminal, `--interactive` can review and approve each push conflict. Cancellation remains mutation-free, and Vulcan re-plans after approval before applying.
- For inbound Outline content, always start with `vulcan pull outline <profile> --into <vault-directory> --dry-run`. Keep the destination stable across runs. Default conflicts preserve local files; choose `--interactive`, `--conflict-markers`, or `--overwrite-conflicts` only after reviewing the plan. Conflict-marker mode uses Git diff3, reports clean non-overlapping merges as `auto_merged`, and writes localized markers only for overlapping hunks.
- Scope large inbound collections with repeatable `--root-document <remote-id>`, optional `--max-depth <n>`, and repeatable `--exclude-document <remote-id>`. Use IDs from Outline, not titles. Confirm that intentionally unselected managed documents appear as `out_of_scope`; missing-document policies never apply to them.
- Keep `--max-documents` at a deliberate bound (10,000 by default). A changing, duplicate, or incomplete paginated listing fails closed; retry it rather than increasing the bound. Pull state is tied to the normalized connector server and records exact remote source plus available revision metadata, so do not copy a profile state file between servers.
- Treat portable-path, Unicode-collision, or missing-parent errors as remote hierarchy issues to resolve in Outline; do not hand-edit durable mappings to bypass them. Reference-style and complex Markdown attachment/document destinations are supported and code spans are ignored.
- To publish pulled notes back to the same Outline objects, first review `vulcan publish outline <profile> --adopt-pulled --dry-run`, then apply with `--adopt-pulled`. Adoption is explicit, includes only paths selected by the publication profile, reuses pulled attachment URLs, and fails if the remote binding drifted or is already owned. Confirm the report's `adopted_pull_bindings` count before applying.
- Remote title and hierarchy changes preserve existing local paths by default. Use `--apply-remote-moves` only after reviewing the dry-run source, destination, and rewritten backlink paths; the move workflow preserves local-only note edits and updates resolvable local links.
- Missing remote documents are retained by default. Prefer `--archive-missing <vault-directory>` for recoverable removal. Permanent `--delete-missing` requires `--confirm-delete-count <exact-live-count>`; obtain the count from a dry run. Interactive mode can retain, archive to `<destination>/_removed`, or delete each missing document.
- A live pull reports phase/item progress in human mode and journals its `operation_id` before mutation. If it is interrupted or cancelled, rerun the same command and confirm `resumed_operation`; Vulcan re-plans under the shared vault write lock and clears the journal only after the incremental scan succeeds.
- Attachments removed from remote Markdown remain managed and retained by default and are counted as `stale_attachments`. Prefer `--archive-stale-attachments <vault-directory>` for cleanup. Permanent `--delete-stale-attachments` requires the fresh dry-run count through `--confirm-stale-attachment-delete-count`; never infer this count from an older run.
- After upgrading an older mapping, normalization-only conflicts may require a one-time reviewed overwrite to seed Outline's observed representation. Do not delete or hand-edit state, and do not assume every content-only conflict is normalization rather than a real remote edit.
- After a successful or interrupted live publication, rerun the same profile rather than editing reconciliation state. Durable mappings allow Vulcan to adopt completed work and continue safely.
- Inspect link policy, route collisions, asset policy, publish filters, and hidden-content transforms before changing output.

## Guardrails

- Exports and sites should be reproducible from vault source plus config.
- Outline publication is one-way: the local vault remains canonical. Do not treat remote edits as input. Remote drift fails closed unless the user explicitly authorizes the named source with `--overwrite-conflict` or authorizes all reported conflicts with `--overwrite-conflicts` after a dry run. Neither control affects unmanaged documents, and unresolved conflicts prevent all mutations.
- Outline mappings under `.vulcan/publish/outline/` are durable state, not rebuildable cache. Do not delete or hand-edit them as a routine repair step.
- Outline pull state under `.vulcan/integrations/outline-pull/` is also durable. Pull and publish state are deliberately separate and do not imply bidirectional synchronization. Missing remote documents remain local unless archive or exact-count-confirmed delete is selected. Referenced Outline attachments are downloaded to deterministic `_attachments/` paths; local attachment drift is preserved unless reviewed overwrite is authorized.
- Custom link transforms require an explicitly configured `custom` policy and a trusted vault-local script. Keep the transform deterministic and free of I/O.
- Generated folder placeholders exist only in the output or remote collection. Add a configured folder note when authored landing-page content is required.
- Do not silently publish private or hidden sections; check include/exclude filters and content transforms.
- Broken links should be handled by configured link policy, not by ad hoc deletion.
- For site work, prefer profile edits over command lines that cannot be repeated.

## Example Moves

- Build a public static site profile and diagnose unpublished links.
- Export a selected set of notes to EPUB while excluding private callouts.
- Build a selection plan from several graph seeds, preview its provenance, and reuse it in an export or publication profile.
- Dry-run an Outline publication, inspect structured drift evidence, selectively authorize reviewed source paths, then apply the same profile while monitoring stderr progress.
- Dry-run an Outline pull into an explicit namespace, then preserve, overwrite, or materialize markers for reviewed local/remote conflicts.
- Render one note to HTML to inspect markdown/parser behavior.
