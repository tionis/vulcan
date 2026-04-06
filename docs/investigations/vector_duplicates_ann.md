# Investigation: ANN for `vector_duplicates`

**Date:** 6 April 2026  
**Status:** Deferred for now; keep the exact scan

## Question

Should `vulcan vectors duplicates` switch from the current exact all-pairs scan to an approximate nearest-neighbor index such as HNSW?

## Current implementation

- `vulcan-core/src/vector.rs` computes duplicate candidates with an exact similarity scan over all stored vectors.
- The hot path now keeps a bounded top-N heap, raises the similarity floor as better pairs arrive, and pre-resolves chunk metadata before entering the inner loop.
- The vector backend remains isolated behind the `VectorStore` trait in `vulcan-embed/src/store.rs`.
- The only backend today is `sqlite-vec` 0.1.7 via `vulcan-embed/src/sqlite_vec.rs`.

## Measured result

The ignored benchmark `vector_duplicates_benchmark_large_vault` was run locally against a synthetic vault with 1,200 notes.

Observed timings:

- Full scan: about `322ms`
- Vector indexing: about `10.18s`
- Duplicate scan: about `926ms`
- Retained pairs: `25`

For this benchmark, duplicate detection is not the dominant cost. Embedding/indexing is much slower than the duplicate scan itself.

## Constraint analysis

Why ANN is not the right change for Phase 9.19.1:

1. `vector_duplicates` is an exact all-pairs problem, not just a top-k query problem.
2. The current `VectorStore` trait exposes insert/delete/load/query operations, but no index-construction or ANN-tuning surface.
3. `sqlite-vec` is already wrapped precisely so backend-specific indexing strategies can change later without leaking into `vulcan-core`.
4. Adding HNSW now would either:
   - break the current backend abstraction, or
   - require a second backend and a broader design change that is larger than a bug-fix slice.

## Decision

Do not add ANN/HNSW in Phase 9.19.1.

Keep the exact duplicate scan for now because:

- the measured 1,200-note benchmark completes the duplicate phase in under 1 second,
- the recent pruning fixes materially improve the exact scan,
- the backend abstraction is not yet designed for ANN-specific capabilities.

## Revisit trigger

Revisit ANN when at least one of these becomes true:

- duplicate scans on real vaults with a few thousand embedded notes become multi-second after the current pruning optimizations,
- vector search itself needs ANN support for user-facing latency,
- the `VectorStore` trait is expanded to support backend capabilities or a second backend is introduced.
