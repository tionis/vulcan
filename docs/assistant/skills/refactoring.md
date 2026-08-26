---
name: refactoring
description: Safely rename aliases, headings, block refs, properties, and tags, move notes, split large heading-structured documents into wiki trees, and convert configured folder-note layouts across the vault. Use when a structural change must preserve resolved links, assets, or shared folder-note configuration.
version: 1
tools:
  - refactor_rename_alias
  - refactor_rename_heading
  - refactor_rename_block_ref
  - refactor_rename_property
  - refactor_merge_tags
  - refactor_split_note
  - refactor_folder_notes
  - move
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Refactoring

## When to Use This Skill

Use this skill for coordinated vault-wide rewrites where link safety matters.

## Recommended Flow

- Start with `--dry-run` whenever the command offers it.
- Use the most specific refactor command available instead of a generic text replacement.
- Prefer link-aware operations like `move` and `rename-*` over raw search-and-replace.
- Use `vulcan refactor split-note <source> --from-level <n> --through-level <n> --dry-run` to preview a large document's generated folder-note tree, source-span ownership, and link rewrites before materializing it.
- Review the split plan's generated paths, duplicate-heading diagnostics, preserved HTML page anchors, inbound rewrites, and asset destinations. Repair unresolved links and ambiguous linked headings before applying the plan.
- Use `vulcan refactor folder-notes --dry-run` before changing folder-note placement or naming. Review every planned move, overwrite or case-insensitive collision, and the resulting shared convention.
- Let the folder-note refactor update shared configuration after all moves succeed; do not move the files separately and patch config by hand.
- Inspect follow-up diagnostics or graph fallout after large rewrites.

## Guardrails

- Generic text replacement is the wrong tool for link-aware edits.
- `split-note` replaces the source by default. Use `--keep-source` only when a separate archival copy is intentional; it requires a non-colliding generated folder note.
- `split-note` preserves asset files in place and rewrites relative Markdown destinations. Do not move or duplicate the companion asset directory separately during the refactor.
- Missing source fragments fail closed. Use `--preserve-missing-fragments` only when the conversion already contains absent page anchors and retaining clearly diagnosed broken links is preferable to blocking the split; the flag does not repair those targets.
- Footnotes and reference-style definitions currently fail closed because their definitions are file-local. Convert them to inline links or keep the affected material within one note before splitting.
- Folder-note conversion is an exact configured-layout migration, not automatic convention detection. Supply explicit `--from-*` values when the effective repository config does not describe the source layout.
- A folder-note dry run must not change notes or configuration. Resolve every preflight conflict before applying it.
- Large refactors should be reviewed before commit, especially when many backlinks change.
- If the task is really metadata cleanup, use `update`, `unset`, or `merge-tags` instead of forcing it through a rewrite.

## Example Moves

- Rename one heading and let inbound heading links update safely.
- Move a note into a new folder while preserving inbound links.
- Split a PDF-derived rulebook at chapter and concept heading levels after reviewing the dry-run tree and asset rewrites.
- Split a PDF-derived rulebook with converter page anchors using `--preserve-missing-fragments` only after the dry run proves that the reported fragments are absent from the source.
- Convert `README.md` folder notes to index notes after reviewing the complete collision-safe move plan.
- Merge two tags after confirming that the destination tag is the canonical one.
