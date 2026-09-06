# Bundled mdbase v0.3 artifacts

Vulcan pins its mdbase compatibility work to an immutable upstream revision so
that specification upgrades remain explicit, reviewable code changes.

- Upstream repository: <https://github.com/mdbase-dev/mdbase-spec>
- Upstream commit: `68b9a97969bf9472f0d42b8faf8a2e349553f4ea`
- Commit date: 2026-08-07
- Declared specification version: `0.3.0`
- Retrieved: 2026-08-22
- License: MIT; the unmodified upstream `LICENSE` is bundled beside the assets

The following paths were copied byte-for-byte from that commit:

- `LICENSE` to `upstream/LICENSE`
- `schemas/v0.3/` to `upstream/schemas/`
- `tests/v0.3/` to `upstream/tests/`
- `examples/v0.3/tasknotes-migration/v0.3/_contracts/tasknotes.task.md` to the
  same path under `upstream/`
- `examples/v0.3/tasknotes-migration/v0.3/_types/task.md` to the same path
  under `upstream/`

The BLAKE3 digest of the bundled upstream tree is
`91c7c26271d1b1069eb89940492b6e9e7583ce01884925ef3b9302c96031c276`.
It is calculated by bytewise sorting every file's `./`-prefixed relative path, then
hashing each path as UTF-8, one NUL byte, and the file's raw bytes in order.
Vulcan's test suite verifies this digest and the source constants.

To upgrade the pin, review the upstream diff, replace all copied paths,
update the commit and digest here and in `vulcan-core::mdbase`, then run the
upstream artifact checks and Vulcan's full workspace checks.
