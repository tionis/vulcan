# Performance

Vulcan's performance model is built around cheap incremental rescans and rebuildable derived indexes.

## Current tuning

- Incremental `scan` skips unchanged files by `mtime` + size and verifies with `blake3` only when needed.
- No-op incremental scans skip link re-resolution and FTS rewrites entirely.
- SQLite runs in WAL mode with a configured `busy_timeout`.
- Vector indexing batches work by provider `max_batch_size` and keeps cluster state derived.
- The watcher coalesces filesystem events before triggering a new incremental scan.

## Benchmarking

Build a release binary first:

```bash
cargo build --release -p vulcan-cli --bin vulcan
```

## Binary size

Phase 9.19.14 measured the Linux x86_64 release binary using `stat`, `size`,
`strip`, `cargo tree`, and the built static archives. `cargo-bloat` was not
installed in the local environment, so dependency and archive inspection were
used instead of symbol-level breakdowns.

Current measurements:

- Default release build (`cargo build --release -p vulcan-cli`):
  `31,856,664` bytes on disk, `26,568,376` bytes after `strip`
- No-JS release build (`cargo build --release -p vulcan-cli --no-default-features`):
  `29,682,952` bytes on disk, `24,670,480` bytes after `strip`
- Stripping alone saves about `5.3 MB` on the default binary
- Disabling `js_runtime` saves about `2.17 MB` unstripped and `1.90 MB`
  stripped once the feature propagation is wired correctly

Largest contributors identified during the investigation:

- Bundled SQLite from `rusqlite` (`libsqlite3.a`): about `3.2 MB`
- QuickJS when `js_runtime` is enabled (`libquickjs.a`): about `2.0 MB`
- `zstd` pulled transitively by `zip` default features (`libzstd.a`): about
  `1.6 MB`
- `reqwest` + `rustls` remain a substantial intentional dependency, but are
  already fairly constrained (`default-features = false`, `blocking`, `json`,
  `rustls-tls`)

Findings:

- `vulcan-cli` must depend on `vulcan-core` with `default-features = false` so
  `cargo build -p vulcan-cli --no-default-features` actually removes
  `js_runtime` from the final binary.
- The main remaining low-risk size follow-up is narrowing `zip` features.
  Vulcan currently writes only stored and deflated archives, but the default
  `zip` feature set still brings in AES, `bzip2`, and `zstd`.
- `ratatui`, `rustyline`, bundled SQLite, and the network stack appear to be
  deliberate product tradeoffs rather than accidental binary bloat.

Benchmark a full scan and a no-op incremental scan:

```bash
./scripts/benchmark_scan.sh ~/path/to/vault
```

Benchmark keyword and hybrid search latency:

```bash
./scripts/benchmark_search.sh ~/path/to/vault dashboard
```

Benchmark vector queue, repair, and rebuild maintenance flows:

```bash
./scripts/benchmark_vectors.sh ~/path/to/vault
```

To include mutating vector operations in the benchmark run:

```bash
RUN_MUTATING=1 ./scripts/benchmark_vectors.sh ~/path/to/vault
```

For model migration benchmarks on a large vault:

1. Run `vectors index` once on the current model so the queue is empty.
2. Change `[embedding].model`, dimensions, or provider settings in `.vulcan/config.toml` or `.vulcan/config.local.toml`.
3. Re-run `./scripts/benchmark_vectors.sh ~/path/to/vault` and compare `vectors queue status`, `vectors repair --dry-run`, and `vectors rebuild --dry-run`.
4. If you want end-to-end migration timings, rerun with `RUN_MUTATING=1`.

## Profiling

Linux `perf` example:

```bash
perf record --call-graph dwarf ./target/release/vulcan --vault ~/path/to/vault scan --full
perf report
```

The main hot paths to watch are:

- Markdown parsing and chunk construction
- Link resolution when the graph changes
- FTS backfill or repair work after schema changes
- Embedding request batching and vector row writes during `vectors index`
- Large vector maintenance passes after provider/model changes
- Canvas JSON parsing and text node FTS indexing (Phase 18) — canvas files can be large; text node extraction and chunk creation should be profiled alongside Markdown parsing
- Post-FTS filter pipeline (Roadmap 9.6) — scope operators (`section:`, `line:`, `block:`), case-sensitive matching, and regex filtering run after FTS hits are collected; for broad queries with many hits, this post-filter pass may become the bottleneck

## Concurrency verification

The WAL/read-write serialization path is covered by the concurrent scan/move test:

```bash
cargo test concurrent_scan_and_move_produce_consistent_state --workspace -- --nocapture
```

For watcher throughput, run `watch` against a large vault and save many files in a burst; the watcher should emit a single coalesced update per burst rather than one scan per event.
