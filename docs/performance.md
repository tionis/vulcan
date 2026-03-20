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

Benchmark a full scan and a no-op incremental scan:

```bash
./scripts/benchmark_scan.sh ~/path/to/vault
```

Benchmark keyword and hybrid search latency:

```bash
./scripts/benchmark_search.sh ~/path/to/vault dashboard
```

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

## Concurrency verification

The WAL/read-write serialization path is covered by the concurrent scan/move test:

```bash
cargo test concurrent_scan_and_move_produce_consistent_state --workspace -- --nocapture
```

For watcher throughput, run `watch` against a large vault and save many files in a burst; the watcher should emit a single coalesced update per burst rather than one scan per event.
