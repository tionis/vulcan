# Release packaging decision

Date: 31 August 2026

## Decision

Keep Vulcan's initial release construction in a small checked-in standard-library Python tool rather
than adopting `cargo-dist` yet.

The canonical contract requires a fixed top-level directory, Vulcan-generated dynamic completions,
a generated man page, install notes and dual-license files, deterministic timestamps, per-artifact
records, a combined JSON manifest, and forge-neutral local execution. The existing release matrix is
small, while package-manager publication and signing identities are not established. A generated
`cargo-dist` configuration would currently add another pinned release tool and generated workflow
policy without removing the custom asset, manifest, or verification logic.

The checked-in `scripts/release/package.py` command accepts an already-built binary and shared asset
directory, produces the same archive locally or in any CI system, and has fixture tests for layout,
permissions, reproducibility, ZIP/TAR selection, and manifest verification. GitHub Actions is only an
orchestrator around that command.

## Revisit criteria

Re-evaluate `cargo-dist` when at least one of these becomes true:

- signing, attestations, Homebrew, and WinGet publication can be expressed without replacing the
  canonical archive layout or making GitHub the release authority;
- the target matrix or installer set grows enough that maintaining the checked-in workflow becomes
  materially riskier than reviewing generated policy;
- `cargo-dist` can consume Vulcan's pre-generated assets and emit the required manifest directly.

Any adoption must pin the tool version, preserve non-GitHub execution, and include a migration test
that compares archive layout and metadata with the existing contract.
