# mdbase CEL engine selection

Vulcan uses [`cel-interpreter` 0.10.0](https://crates.io/crates/cel-interpreter/0.10.0)
with [`cel-parser` 0.10.1](https://crates.io/crates/cel-parser/0.10.1) behind a
Vulcan-owned adapter. Both releases use Rust 2021 and avoid the `LazyLock`
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
