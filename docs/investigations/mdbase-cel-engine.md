# mdbase CEL engine selection

Vulcan now uses [`cel` 0.15.0](https://crates.io/crates/cel/0.15.0) behind the
Vulcan-owned adapter (see the 2026-10 update below). The rest of this section records the
original selection of [`cel-interpreter` 0.10.0](https://crates.io/crates/cel-interpreter/0.10.0)
with [`cel-parser` 0.10.1](https://crates.io/crates/cel-parser/0.10.1). Both releases use Rust 2021 and avoid the `LazyLock`
dependency introduced by the renamed `cel` 0.11 line, so they remain compatible
with the repository's conservative Rust 1.77 compatibility requirement. An
isolated build of the selected engine, parser, and `antlr4rust` 0.3.0-rc2
resolution passes with Rust 1.77.2.

The current `cel` 0.14 series is not suitable for that compatibility target: its
published package metadata requires Rust 1.86. The older engine is kept private
to `vulcan-core::mdbase::cel`, allowing a future engine upgrade or replacement
without changing Vulcan's mdbase API.

The adapter supports the portable minimums of 64 KiB of source, AST depth 100,
and link traversal depth 10. It rejects work before evaluation when any
configured bound is exceeded. Additional bounds cover lexical complexity,
parsed AST node count, estimated comprehension work, aggregate input/output
bytes and value nodes, and individual list/map iteration width. CEL has no I/O
facilities in this integration. Host bindings and link resolution are added only
by Vulcan and must consume the adapter's explicit traversal budget.

## 2026-10 update

The repository's MSRV is now Rust 1.88, so the reason above no longer applies: the `cel` 0.14/0.15
line declares Rust 1.86 and replaces `paste` with the maintained `pastey`. See
[the upstream review](mdbase-upstream-2026-10.md).

The adapter now uses `cel` 0.15.0 (with `antlr4rust` 0.6 and `pastey`; `uuid` is held at 1.26.1,
the newest release that supports Rust 1.88), and `deny.toml` no longer excepts
`RUSTSEC-2024-0436`. Behavioral notes:

- Every program compiles in one shared environment built once per process. Constructing the
  standard environment per evaluation, as `Context::default()` does, is avoided.
- The standard library's `duration(string)` parses Go-style strings (`1h30m`), and new
  declarations may not shadow standard overloads. A parse-time `duration` macro rewrites calls to
  Vulcan's ISO 8601 implementation, registered in every context, so `duration('P1D')` keeps the
  mdbase meaning everywhere and Go-style strings are rejected.
- Every pinned upstream suite (`cel`, `cel_match`, `cel_query`, `links`, saved views) and the
  native feature gates pass unchanged.
