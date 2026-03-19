# Investigation: sqlite-vec Rust Build Story

**Date:** 20 March 2026
**sqlite-vec version:** 0.1.7 (crate), v0.1.7 (upstream, released 17 March 2026)
**Status:** Straightforward to integrate; statically compiled from bundled C source

## Summary

The `sqlite-vec` Rust crate bundles the C source and compiles it statically at build time via the `cc` crate. It exposes a single function that registers the extension with SQLite. Integration with `rusqlite` + `bundled` is well-documented and works cross-platform with no special system dependencies beyond a C compiler.

## Crate details

- **Crate name:** `sqlite-vec` (on crates.io)
- **Current version:** 0.1.7
- **License:** MIT/Apache-2.0
- **Size:** 325KB (mostly the bundled `sqlite-vec.c`)
- **Runtime dependencies:** None
- **Build dependency:** `cc` (for compiling the C source)
- **Dev dependency:** `rusqlite` 0.31
- **Popularity:** ~400K downloads/month, used by 75 crates

## How it works

The crate embeds the `sqlite-vec.c` source file and uses a `build.rs` script with the `cc` crate to compile and statically link it at build time. This means:

- **No dynamic loading.** The extension is compiled into the binary. No `.so`/`.dylib`/`.dll` to distribute.
- **No system dependencies** beyond a C compiler (which `cc` handles via the platform's default: `gcc`/`clang` on Linux/macOS, MSVC on Windows).
- **No minimum SQLite version concern** for the crate itself — it links against whatever SQLite `rusqlite` provides.

## Integration with rusqlite

The crate exposes a single function: `sqlite3_vec_init`. You register it as an auto-extension before opening any connection:

```rust
use sqlite_vec::sqlite3_vec_init;
use rusqlite::{ffi::sqlite3_auto_extension, Connection};

unsafe {
    sqlite3_auto_extension(Some(std::mem::transmute(
        sqlite3_vec_init as *const ()
    )));
}

let db = Connection::open("path/to/db")?;
// vec0 virtual tables are now available
```

This is an `unsafe` block due to the FFI transmute, but it's a one-liner at application startup. After registration, all connections automatically have `sqlite-vec` available.

## Compatibility with rusqlite `bundled`

**No issues.** The `rusqlite` `bundled` feature compiles SQLite from source via `libsqlite3-sys`. The `sqlite-vec` crate compiles its own C code separately and registers via the extension API. These don't conflict — `sqlite-vec` just needs the SQLite header types, which `rusqlite`'s FFI module provides.

The recommended `Cargo.toml` setup:

```toml
[dependencies]
rusqlite = { version = "0.31", features = ["bundled"] }
sqlite-vec = "0.1"
zerocopy = { version = "0.7", features = ["derive"] }  # for efficient Vec<f32> passing
```

## Cross-platform build

| Platform | Build story | Notes |
|---|---|---|
| Linux | Works out of the box | `cc` uses system `gcc` or `clang` |
| macOS | Works out of the box | `cc` uses Xcode `clang` |
| Windows | Works out of the box | `cc` uses MSVC |
| WASM | Supported upstream | Not relevant for CLI, but possible |

No special flags, no vendored libraries, no pkg-config. The `cc` crate handles everything.

## Vector passing

The `zerocopy` crate is recommended for passing `Vec<f32>` to SQLite without copying:

```rust
use zerocopy::AsBytes;
let v: Vec<f32> = vec![0.1, 0.2, 0.3];
stmt.execute(&[v.as_bytes()])?;
```

## Pre-v1 status and abstraction strategy

The crate is explicitly pre-v1 ("expect breaking changes"). For Vulcan this means:

1. **Pin to `0.1.x`** in `Cargo.toml` to avoid surprise breakage.
2. **Wrap behind a `VectorStore` trait** in `vulcan-embed` so that the `vec0` virtual table usage is isolated to one module.
3. The trait surface is small: insert vectors, query nearest neighbors, delete by chunk ID. Swapping to a different backend (e.g., a future stable sqlite-vec, or an external vector DB) would only touch the trait implementation.

## Recommended Vulcan project structure

```
vulcan-embed/
  Cargo.toml          # depends on sqlite-vec, zerocopy, rusqlite (re-exported from vulcan-core)
  src/
    lib.rs
    provider.rs       # EmbeddingProvider trait
    openai_compat.rs  # OpenAI-compatible HTTP provider
    vector_store.rs   # VectorStore trait
    sqlite_vec.rs     # sqlite-vec VectorStore implementation
```

The `sqlite3_auto_extension` registration should happen in `vulcan-core`'s database initialization, since it needs to run before any connection is opened. `vulcan-embed` provides the implementation but `vulcan-core` calls the init.

## Risk assessment

**Low risk.** The build story is simple (bundled C, no system deps), the integration is a single unsafe line, and the pre-v1 concern is fully addressed by the trait abstraction we already planned. The main thing to watch is that `sqlite-vec` updates its crate when upstream `sqlite-vec.c` changes — version 0.1.7 was published just days ago, so the crate appears actively maintained.

## Sources

- [sqlite-vec Rust usage guide](https://alexgarcia.xyz/sqlite-vec/rust.html)
- [sqlite-vec crate on lib.rs](https://lib.rs/crates/sqlite-vec)
- [sqlite-vec GitHub repository](https://github.com/asg017/sqlite-vec)
