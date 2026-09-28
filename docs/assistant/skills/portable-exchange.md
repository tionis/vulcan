---
name: portable-exchange
description: Inspect, validate, import, and export portable Markdown exchange packages. Use TextBundle or TextPack for one Markdown document and its assets, or Wiki Bundle and Wiki Pack for a complete wiki snapshot, including version 2 packages that carry provenance, source maps, and cited knowledge; use artifact-import for a single extracted MDAF document that Vulcan should split into notes.
version: 1
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Portable Exchange

Use this workflow to move one editable Markdown document and its linked assets between compatible applications.

## Workflow

- Export a canonical vault note with `vulcan --output json exchange textbundle export <note> --package <package.textpack> --dry-run` and review the planned package before applying it without `--dry-run`.
- Use a `.textbundle` output for a directory package or `.textpack` for a ZIP package.
- Inspect an incoming package with `vulcan --output json exchange textbundle inspect <package>`.
- Validate it with `vulcan exchange textbundle validate <package>` before import.
- Plan import with `vulcan --output json exchange textbundle import <package> --destination <new-folder> --dry-run`, then apply the same command after reviewing the destination and assets.
- Export a complete vault snapshot with `vulcan --output json exchange wiki export --package <wiki.wikipack> --dry-run`. Use `.wikibundle` for a directory or `.wikipack` for a ZIP.
- Inspect and validate incoming wiki packages with `vulcan exchange wiki inspect <package>` and `vulcan exchange wiki validate <package>`, then plan materialization with `vulcan --output json exchange wiki import <package> --destination <new-folder> --dry-run`.
- For a version 2 wiki package, read the inspection `summary` before importing. It gives the counts of sources, provenance activities, source-map mappings, knowledge entities and claims, and accepted records. The import report's `annotated_notes` lists the notes that will receive `vulcan.source` frontmatter. It names the package and member by default; pass `--source-locators full` only when the vault must carry every span and locator itself, because full locators can outweigh the notes.

## Guardrails

- Import requires a new explicit vault-relative destination and never merges into an existing tree.
- Treat TextBundle extension metadata as opaque application data. Preserve it by keeping the original package when another application may need it.
- TextBundle is an editable single-document interchange format. It does not retain source coordinates, conversion provenance, alternative extraction evidence, or the original source media.
- Use the `artifact-import` workflow for MDAF when Vulcan should split one extracted document into notes. A version 2 wiki package already has its note tree and keeps its source evidence through `vulcan.source` locators on import.
- Export includes only local assets referenced by the note. Remote URLs, Markdown note links, and vault-external paths are not copied.
- Knowledge records in a wiki package carry their own review status. Do not describe `unreviewed` claims as accepted facts, and do not treat successful validation as proof that a claim is true.
- Wiki import fails if a note that would be annotated already has `vulcan.source` frontmatter. Report the conflict; do not strip the existing entry.
- Wiki export preserves regular vault content while excluding Vulcan, Git, Obsidian, trash, synchronization, and common operating-system metadata state; it rejects symbolic or special files. A wiki package is an immutable snapshot, not a synchronized or writable vault.
