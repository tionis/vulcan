---
name: artifact-import
description: Inspect, validate, and import extractor- and source-format-neutral MDAF packages into a Markdown wiki tree. Use when Markdown, assets, source selectors, alternative outline evidence, and conversion provenance arrive together as one artifact; use refactoring instead for an existing canonical vault note.
version: 1
tools:
  - artifact_inspect
  - artifact_validate
  - artifact_import
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Artifact Import

Use this workflow for a Markdown Artifact Format (MDAF) directory or ZIP that is still external to the vault.

## Workflow

- Start with `vulcan --output json artifact inspect <artifact>` to review identity, producer, declared members, capabilities, sources, and diagnostics.
- Run `vulcan artifact validate <artifact>` before planning an import. Validation checks the extractor-neutral contract and hashes but does not interpret native renditions or extensions.
- Plan with `vulcan --output json artifact import <artifact> --destination <new-folder> --from-level <n> --through-level <n> --dry-run`.
- Review all generated note and asset paths, diagnostics, rewritten links, and source spans. Apply the same command without `--dry-run` only after the destination and hierarchy are correct.
- For an approved book outline with major sections at level two, preview `--hierarchy outline --from-level 2` for chapter notes, or add `--through-level 3` for nested topic notes. A `large_root_remainder` diagnostic means the selected levels leave substantial content in the root; review the authority and level range before applying.
- Use `--hierarchy outline` only when the user explicitly prefers the aligned alternative outline. Markdown headings are authoritative by default.

## Guardrails

- The destination is mandatory and must not already exist. Do not choose a destination by guessing from a private source filename.
- Treat the MDAF package as immutable evidence. A successful import makes the generated Markdown tree canonical; it does not move or rewrite the package.
- Do not infer extraction behavior from a PDF, image, audiovisual, tabular, web, or other source media type—or from Marker, Mistral, DeepSeek, Docling, provider namespaces, asset names, or opaque rendition contents.
- Preserve `vulcan.source` frontmatter. It carries artifact identity, original Markdown byte spans, and available normalized interval, spatial, grid, text, fragment, or extension selectors for later citation or reinterpretation.
- Keep the original MDAF accessible: importing notes/assets does not copy native renditions or the full provenance graph. Root title metadata comes from the manifest, with existing frontmatter preserved. A misleading manifest title needs an explicit producer-side derivative.
- Plain-text source references may become links only when their authored placement and destination note are unique. Coarse source ranges spanning several notes, code spans, and repeated text remain unchanged with diagnostics; do not force them to the first matching note.
- An outline import must fail closed when its hierarchy cannot be represented exactly. Do not silently merge Markdown and outline authority.
- Resolve validation, collision, ambiguous-source-reference, and unsupported-Markdown diagnostics before applying. Do not manually copy assets around the planned tree.
- Use `vulcan refactor split-note` instead when the large Markdown file and its assets are already canonical vault content rather than an MDAF package.
