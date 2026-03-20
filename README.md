# Vulcan

Headless CLI for Obsidian-style vaults and plain Markdown directories.

The repository is organized as a Cargo workspace:

- `vulcan-core`: parsing, indexing, cache management, and vault configuration
- `vulcan-embed`: embedding provider and vector store abstractions
- `vulcan-cli`: the `vulcan` binary and command-line interface

Primary design and implementation references live in `docs/design_document.md` and `docs/ROADMAP.md`.
