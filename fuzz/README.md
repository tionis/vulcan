# Fuzzing

Use [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) from the repository root:

```bash
cargo install cargo-fuzz
cargo fuzz run parser
cargo fuzz run frontmatter
cargo fuzz run links
cargo fuzz run chunker
cargo fuzz run dql
cargo fuzz run expression
cargo fuzz run tasks
cargo fuzz run config
```

Each target exercises a user-authored text surface through the public API:

- `parser`, `frontmatter`, `links`, `chunker`: Markdown document ingestion
- `dql`: Dataview query tokenization and parsing
- `expression`: expression parser
- `tasks`: Tasks query parser
- `config`: `.vulcan/config.toml` validation

When a target finds a crash or panic:

1. Minimize it with `cargo fuzz tmin <target> <artifact>`.
2. Reproduce it locally.
3. Promote the minimized input into a unit test or fixture regression before closing the issue.

See [`docs/hardening.md`](../docs/hardening.md) for the full local and CI hardening workflow.
