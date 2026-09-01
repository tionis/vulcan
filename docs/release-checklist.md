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
  `SHA256SUMS` verifies every archive and both Debian packages.
- [ ] Decode `vulcan-update-channel.json`, verify its exact payload signature against a documented
  trusted key, and confirm the stable channel, version, source commit, timestamp, five archive URLs,
  sizes, hashes, formats, and top-level directories match the canonical manifest. If no project
  signing identity is configured, leave signatures empty and clearly label the release and
  `--allow-unsigned` requirement instead of implying authenticity.
- [ ] Inspect both Debian packages with `dpkg-deb --info` and `dpkg-deb --contents`. On a clean
  amd64 Debian-family environment, install through `apt`, verify the binary/man page/completions,
  confirm no daemon service was enabled implicitly, upgrade once, and remove the package while
  preserving user and vault state. Inspect the arm64 package metadata even when native hardware is
  unavailable.
- [ ] Confirm native archive smoke tests pass on x86_64 Linux, x86_64 macOS, aarch64 macOS, and
  x86_64 Windows. Manually validate aarch64 Linux on native hardware or a declared emulator before
  describing that artifact as exercised rather than cross-compiled.
- [ ] Test `vulcan --version`, a direct vault command, `sync doctor`, and `daemon install --dry-run`
  from each extracted native archive.
- [ ] From a disposable portable prefix, run `self-update check` and `self-update apply --dry-run`,
  verify signature and downgrade failures are closed by default, then apply an upgrade and refresh
  the daemon service. Never exercise portable replacement against a package-managed path.
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

For the bounded rolling `main` prerelease, confirm the gate selected a new commit with successful
push CI, the embedded version uses the documented `-dev.<date>.<run>.g<commit>` form, the fixed
release was updated only after all target/package smoke checks passed, and obsolete assets were
pruned only after their replacements uploaded successfully. A no-change scheduled run must not
build or publish artifacts. Confirm the machine-local signer reports `signed` or `already_signed`
for the same commit, its readback matches, and `vulcan self-update check --channel main` verifies
`main-2026-09` without `--allow-unsigned` from a post-bootstrap portable build.
