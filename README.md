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
./target/release/vulcan describe
```

## Documentation

- `docs/cli.md` — User-facing CLI guide, query/filter syntax, search syntax, JSON/export behavior, and interactive features
- `docs/design_document.md` — Architecture and design decisions
- `docs/ROADMAP.md` — Phased implementation status and remaining work
- `docs/performance.md` — Benchmarking and performance notes

## Workspace

The repository is organized as a Cargo workspace:

- `vulcan-core`: parsing, indexing, cache management, and vault configuration
- `vulcan-embed`: embedding provider and vector store abstractions
- `vulcan-cli`: the `vulcan` binary and command-line interface
