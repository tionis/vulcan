---
name: publishing-and-export
description: Build static sites, export or package vault content, and operate one-way Outline publications. Use when the user asks about site builds, export profiles, EPUB/ZIP/SQLite/JSON/CSV output, Outline ZIP or API publishing, graph-based publication scope, reconciliation conflicts, render diagnostics, publish filters, content transforms, or route/link policy.
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
- After upgrading an older mapping, normalization-only conflicts may require a one-time reviewed overwrite to seed Outline's observed representation. Do not delete or hand-edit state, and do not assume every content-only conflict is normalization rather than a real remote edit.
- After a successful or interrupted live publication, rerun the same profile rather than editing reconciliation state. Durable mappings allow Vulcan to adopt completed work and continue safely.
- Inspect link policy, route collisions, asset policy, publish filters, and hidden-content transforms before changing output.

## Guardrails

- Exports and sites should be reproducible from vault source plus config.
- Outline publication is one-way: the local vault remains canonical. Do not treat remote edits as input. Remote drift fails closed unless the user explicitly authorizes the named source with `--overwrite-conflict` or authorizes all reported conflicts with `--overwrite-conflicts` after a dry run. Neither control affects unmanaged documents, and unresolved conflicts prevent all mutations.
- Outline mappings under `.vulcan/publish/outline/` are durable state, not rebuildable cache. Do not delete or hand-edit them as a routine repair step.
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
- Render one note to HTML to inspect markdown/parser behavior.
