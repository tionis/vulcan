# Vulcan

Headless CLI for Obsidian-style vaults and plain Markdown directories.

## Quick start

Build the binary:

```bash
cargo build --release -p vulcan-cli --bin vulcan
```

Initialize and scan a vault:

```bash
./target/release/vulcan --vault ~/wikis/mimir init
./target/release/vulcan --vault ~/wikis/mimir scan
```

Common discovery commands:

```bash
./target/release/vulcan --help
./target/release/vulcan notes --help
./target/release/vulcan search --help
./target/release/vulcan browse --help
./target/release/vulcan edit --help
./target/release/vulcan describe
```

Common day-to-day commands:

```bash
./target/release/vulcan --vault ~/wikis/mimir browse
./target/release/vulcan --vault ~/wikis/mimir search 'dashboard "release notes"'
./target/release/vulcan --vault ~/wikis/mimir edit Projects/Alpha
./target/release/vulcan --vault ~/wikis/mimir inbox "Capture this idea"
```

## Documentation

- `docs/cli.md` — Comprehensive CLI guide: command catalogue, query/filter syntax, search syntax, interactive flows, auto-commit, inbox/templates, JSON/export behavior
- `docs/design_document.md` — Architecture and design decisions
- `docs/ROADMAP.md` — Phased implementation status and remaining work
- `docs/performance.md` — Benchmarking and performance notes

## Workspace

The repository is organized as a Cargo workspace:

- `vulcan-core`: parsing, indexing, cache management, and vault configuration
- `vulcan-embed`: embedding provider and vector store abstractions
- `vulcan-cli`: the `vulcan` binary and command-line interface
