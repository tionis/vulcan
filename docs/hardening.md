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

## Feature Boundary Matrix

Run this matrix when changing feature flags, optional dependencies, MCP/server
boundaries, or app/core crate ownership:

```bash
cargo check --workspace --no-default-features
cargo test -p vulcan-core --no-default-features --test minimal_non_ai
cargo check -p vulcan-core --no-default-features --features oauth,vectors,web
cargo check -p vulcan-app --no-default-features --features oauth,vectors,web
cargo check -p vulcan-cli --no-default-features --features oauth,vectors,web
scripts/compare_feature_matrix.sh
```

The `oauth,vectors,web` checks intentionally omit `js_runtime`; they verify the
full non-JS backend combination still compiles. `scripts/compare_feature_matrix.sh`
writes comparable `cargo tree` outputs and a short optional-dependency summary
under `target/feature-matrix/`.

The rest of the hardening coverage already lives in the normal test suite:

- uninitialized and partially initialized vault repair coverage
- watch and serve refresh stability
- rebuild and repair idempotency
- JSON/JSONL/CSV/TSV and human-output contract tests
- MCP permission filtering and rejection paths

## Fuzz Targets

Install `cargo-fuzz` once:

```bash
rustup toolchain install nightly
cargo +nightly install cargo-fuzz --locked --version 0.13.2
```

Run any target for a bounded local pass from the repository root with nightly enabled:

```bash
cargo +nightly fuzz run parser -- -max_total_time=30
cargo +nightly fuzz run frontmatter -- -max_total_time=30
cargo +nightly fuzz run links -- -max_total_time=30
cargo +nightly fuzz run chunker -- -max_total_time=30
cargo +nightly fuzz run dql -- -max_total_time=30
cargo +nightly fuzz run expression -- -max_total_time=30
cargo +nightly fuzz run tasks -- -max_total_time=30
cargo +nightly fuzz run config -- -max_total_time=30
```

`cargo-fuzz` relies on nightly-only sanitizer flags, so plain `cargo fuzz ...` from the stable repo root will fail. If you want to avoid `+nightly`, `cd fuzz` first; that subtree pins nightly in `fuzz/rust-toolchain.toml`.

Covered parser and text-ingestion surfaces:

- Markdown document parsing and chunking
- frontmatter extraction
- link and embed parsing
- DQL parsing
- expression parsing
- Tasks query parsing
- `.vulcan/config.toml` ingestion via validation

Other structured imports such as Obsidian plugin JSON settings are not fuzzed separately today because they feed through deterministic serde-based config import paths that already have dedicated fixture tests.

## Persistent Overnight Fuzzing

The manual CI hardening workflow uses short fuzz passes to verify that every harness still builds
and runs. Sustained bug discovery belongs on a development machine or dedicated runner where the
corpora in `fuzz/corpus/` and failures in `fuzz/artifacts/` persist between runs.

The following sequential pass gives each target one hour, for an approximately eight-hour run:

```bash
set -e
for target in parser frontmatter links chunker dql expression tasks config; do
  cargo +nightly fuzz run "$target" -- -max_total_time=3600
done
```

Run this through the host's scheduler, preserve the `fuzz/` directory between executions, and
report a non-zero exit so crashes are noticed. Do not treat a retained corpus as a regression
suite: minimize each failure and promote it into a deterministic test or fixture as described
below.

## Promoting Fuzz Findings

Fuzz artifacts are only useful if they become permanent regressions.

When a crash or panic is found:

1. Minimize it with `cargo +nightly fuzz tmin <target> <artifact>` from the repo root, or `cargo fuzz tmin <target> <artifact>` from inside `fuzz/`.
2. Add a unit test or fixture that reproduces the minimized input.
3. Keep the regression test in-tree before merging the fix.

If a new parser or user-authored text surface is added, either:

- add it to the fuzz harness, or
- document why deterministic tests are enough for that surface.

## CI Layout

- `.github/workflows/ci.yml`: required on push and pull request. Runs fmt, clippy, and the full workspace test suite.
- `.github/workflows/hardening.yml`: manual (`workflow_dispatch`). Runs the heavier integration hardening cases, ignored synthetic regression tests, and short fuzz harness smoke passes. Use it before releases or after risky parser, permission, refactoring, vector, or sync changes; use a persistent external runner for scheduled fuzzing.
