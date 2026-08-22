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

The BLAKE3 digest of the bundled upstream tree is
`9b4c7d477dc914099a5a40092d6543caca9c626d5ca0ff3ed5a4d47646c29e52`.
It is calculated by bytewise sorting every file's `./`-prefixed relative path, then
hashing each path as UTF-8, one NUL byte, and the file's raw bytes in order.
Vulcan's test suite verifies this digest and the source constants.

To upgrade the pin, review the upstream diff, replace all three copied paths,
update the commit and digest here and in `vulcan-core::mdbase`, then run the
upstream artifact checks and Vulcan's full workspace checks.
