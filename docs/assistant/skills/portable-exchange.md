---
name: portable-exchange
description: Inspect, validate, import, and export portable Markdown exchange packages. Use TextBundle or TextPack for one Markdown document and its assets; use artifact-import when source evidence, extraction provenance, or source selectors must be retained.
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

## Guardrails

- Import requires a new explicit vault-relative destination and never merges into an existing tree.
- Treat TextBundle extension metadata as opaque application data. Preserve it by keeping the original package when another application may need it.
- TextBundle is an editable single-document interchange format. It does not retain source coordinates, conversion provenance, alternative extraction evidence, or the original source media.
- Use the `artifact-import` workflow for MDAF when evidence and traceability matter. Import the artifact into a wiki tree only after validation.
- Export includes only local assets referenced by the note. Remote URLs, Markdown note links, and vault-external paths are not copied.
