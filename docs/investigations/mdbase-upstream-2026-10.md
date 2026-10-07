# mdbase upstream, MSRV, and crate-boundary review (2026-10-08)

MDB.9 asks for this review before each profile expansion. It was made after MDB.8 added saved
views, schema reports, and the `view_records` / `writable_view_sources` optional features.

## Specification stability

- Vulcan pins `mdbase-spec` at `68b9a979` (v0.3.0 release-candidate chapters, promoted to the
  repository root on 2026-07-16).
- Upstream `main` is 33 commits ahead (head `2be70152`, "v0.3.0-rc.5 errata from the first
  implementation", 2026-10-04). The new work includes concurrent-edit rules (`12a`), a regex
  profile, match determinism, merge and body-edit suites, YAML-document records, optional
  membership, rename references, validation tiers, and watch/move detection, plus changes to the
  config, type-file, type-pack, conformance-claim, and view schemas.
- Assessment: the specification is still a moving release candidate. Re-pinning is an explicit,
  reviewed change that re-runs every claimed suite against the new artifact digest; it is not
  forced by any claimed profile today. Re-evaluate at the next profile expansion or when upstream
  tags a final 0.3.0.

## `mdbase-rs`

- `callumalpass/mdbase-rs` (`mdbase` 0.4.0-rc.6) declares `rust-version = "1.94"` and pins the
  1.94 toolchain. Vulcan's MSRV is 1.88, so depending on it would raise the MSRV.
- It owns a SQLite store (`rusqlite` 0.32; Vulcan uses 0.37), a collection-mutation layer (a
  `legacy-collection-mutation` default feature slated for removal in 0.5), and separate runtime,
  command, and testbed crates. Using it would introduce a second authoritative cache and mutation
  engine beside Vulcan's record cache, journal, and watcher.
- Decision: no direct dependency. Keep Vulcan's single engine and use upstream suites (and
  `mdbase-rs` behavior, when useful) as conformance evidence only.

## CEL engine (affects MDB.5)

- `mdbase-rs` uses `cel` 0.14.5. The `cel` line (0.14.x and 0.15.0) declares Rust 1.86, within
  Vulcan's 1.88 MSRV, and depends on `pastey` (the maintained fork) instead of `paste`.
- The 2025 decision to stay on `cel-interpreter` 0.10 targeted a Rust 1.77 compatibility goal that
  the repository no longer has. Migrating the private `vulcan-core::mdbase::cel` adapter to `cel`
  removes `RUSTSEC-2024-0436` from `deny.toml`, so that MDB.5 item is actionable rather than
  blocked upstream.

## Crate boundaries

mdbase semantics stay in `vulcan-core::mdbase` (synchronous, no async or HTTP), workflows in
`vulcan-app::mdbase`, and transports in `vulcan-cli` / `vulcan-daemon`. The MDB.8 additions
follow that split: view resolution and schema reports in core, the `.base` adapter, source
operations, and change feed in the app crate, and only routing in the CLI and daemon.
