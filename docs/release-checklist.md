# Release checklist

Use this checklist before creating a version tag. A tag triggers publication only after the locked
workspace test gate and every archive build succeed.

- [ ] Update the workspace version and changelog/release notes; confirm the tag is exactly `v<version>`.
- [ ] Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and `python3 -m unittest discover -s scripts/release/tests -v`.
- [ ] Build release assets and one local archive with `scripts/release/generate_assets.py` and
  `scripts/release/package.py`; inspect the stable top-level directory, binary mode, completions,
  man page, install notes, README, and both license files.
- [ ] Confirm the release manifest contains exactly the five advertised targets and that
  `SHA256SUMS` verifies every archive.
- [ ] Confirm native archive smoke tests pass on x86_64 Linux, x86_64 macOS, aarch64 macOS, and
  x86_64 Windows. Manually validate aarch64 Linux on native hardware or a declared emulator before
  describing that artifact as exercised rather than cross-compiled.
- [ ] Test `vulcan --version`, a direct vault command, `sync doctor`, and `daemon install --dry-run`
  from each extracted native archive.
- [ ] Upgrade from the prior supported version in the same prefix, refresh the daemon service, check
  authenticated status, then roll back and repeat. Confirm registry, credential, journal, conflict,
  and vault state are preserved.
- [ ] Check download links and both installers in dry-run and applied modes. Verify PATH behavior and
  removal on a clean Windows runner.
- [ ] Validate the LaunchAgent on macOS: install, bootstrap, authenticated status, graceful stop,
  restart after failure, reinstall, upgrade-path preservation, bootout, and uninstall.
- [ ] Apply notarization, Developer-ID, Authenticode, SBOM, provenance, or release signatures only
  when their configured identities and verification steps are available. Clearly label unsigned
  builds.
