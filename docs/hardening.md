# Hardening And Fuzzing

This repository keeps fast correctness checks and heavier hardening runs separate on purpose.

## Required On Every PR

These are the required checks for normal pull requests and local feature work:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

That required suite now includes the property-based tests added for:

- relative path normalization idempotency
- query AST JSON and `NoteQuery` round trips
- permission allow/deny precedence
- partial permission-profile override precedence

## Local Hardening Runs

Use these when touching parser-heavy or integration-heavy code:

```bash
# focused integration flows
cargo test -p vulcan-cli --test cli_smoke hardening_vault_cli_flow_covers_scan_query_mutate_refactor_export_and_rerun -- --nocapture
cargo test -p vulcan-cli --test cli_smoke sandboxed_cli_profile_rejects_refactor_git_network_config_execute_and_index_commands -- --nocapture
cargo test -p vulcan-cli serve::tests::serve_applies_permission_filters_and_denies_js_execution -- --nocapture

# larger synthetic regression harnesses
cargo test -p vulcan-core vector_duplicates_benchmark_large_synthetic_scan -- --ignored --nocapture
```

The rest of the hardening coverage already lives in the normal test suite:

- uninitialized and partially initialized vault repair coverage
- watch and serve refresh stability
- rebuild and repair idempotency
- JSON/JSONL/CSV/TSV and human-output contract tests
- MCP permission filtering and rejection paths

## Fuzz Targets

Install `cargo-fuzz` once:

```bash
cargo install cargo-fuzz
```

Run any target for a bounded local pass:

```bash
cargo fuzz run parser -- -max_total_time=30
cargo fuzz run frontmatter -- -max_total_time=30
cargo fuzz run links -- -max_total_time=30
cargo fuzz run chunker -- -max_total_time=30
cargo fuzz run dql -- -max_total_time=30
cargo fuzz run expression -- -max_total_time=30
cargo fuzz run tasks -- -max_total_time=30
cargo fuzz run config -- -max_total_time=30
```

Covered parser and text-ingestion surfaces:

- Markdown document parsing and chunking
- frontmatter extraction
- link and embed parsing
- DQL parsing
- expression parsing
- Tasks query parsing
- `.vulcan/config.toml` ingestion via validation

Other structured imports such as Obsidian plugin JSON settings are not fuzzed separately today because they feed through deterministic serde-based config import paths that already have dedicated fixture tests.

## Promoting Fuzz Findings

Fuzz artifacts are only useful if they become permanent regressions.

When a crash or panic is found:

1. Minimize it with `cargo fuzz tmin <target> <artifact>`.
2. Add a unit test or fixture that reproduces the minimized input.
3. Keep the regression test in-tree before merging the fix.

If a new parser or user-authored text surface is added, either:

- add it to the fuzz harness, or
- document why deterministic tests are enough for that surface.

## CI Layout

- `.github/workflows/ci.yml`: required on push and pull request. Runs fmt, clippy, and the full workspace test suite.
- `.github/workflows/hardening.yml`: scheduled nightly and manual (`workflow_dispatch`). Runs the heavier integration hardening cases, ignored synthetic regression tests, and bounded fuzz passes.
