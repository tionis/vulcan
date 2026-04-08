# Fuzzing

Use [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) from the repository root:

```bash
cargo install cargo-fuzz
rustup toolchain install nightly
cargo +nightly fuzz run parser
cargo +nightly fuzz run frontmatter
cargo +nightly fuzz run links
cargo +nightly fuzz run chunker
cargo +nightly fuzz run dql
cargo +nightly fuzz run expression
cargo +nightly fuzz run tasks
cargo +nightly fuzz run config
```

`cargo-fuzz` uses nightly-only sanitizer flags (`-Zsanitizer=address`). If you run it from the repo root on the stable toolchain, it will fail with the exact `the option 'Z' is only accepted on the nightly compiler` error you saw.

If you prefer not to spell out `+nightly`, the `fuzz/` subdirectory pins nightly via `fuzz/rust-toolchain.toml`, so this also works:

```bash
cd fuzz
cargo fuzz run parser
```

Each target exercises a user-authored text surface through the public API:

- `parser`, `frontmatter`, `links`, `chunker`: Markdown document ingestion
- `dql`: Dataview query tokenization and parsing
- `expression`: expression parser
- `tasks`: Tasks query parser
- `config`: `.vulcan/config.toml` validation

When a target finds a crash or panic:

1. Minimize it with `cargo +nightly fuzz tmin <target> <artifact>` from the repo root, or `cargo fuzz tmin <target> <artifact>` from inside `fuzz/`.
2. Reproduce it locally.
3. Promote the minimized input into a unit test or fixture regression before closing the issue.

See [`docs/hardening.md`](../docs/hardening.md) for the full local and CI hardening workflow.
