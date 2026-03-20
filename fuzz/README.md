# Fuzzing

Use [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) from the repository root:

```bash
cargo install cargo-fuzz
cargo fuzz run parser
cargo fuzz run frontmatter
cargo fuzz run links
cargo fuzz run chunker
```

Each target exercises the public parser pipeline with a different shape of input so parser, frontmatter extraction, link handling, and chunk generation are all covered by libFuzzer.
