# Vulcan update channels

Status: version 1, implemented for portable Vulcan archives.

This contract separates *which release stream a user follows* from the forge, package registry, or
installer used to deliver it. The canonical streams are:

- `stable`: immutable, version-tagged releases. This is the default channel for ordinary builds
  and the future default for package registries.
- `main`: one replace-in-place development prerelease built from the newest eligible `main`
  commit. It is opt-in, may disappear or be replaced, and is not a supported rollback boundary.

Additional channels require a new client release. Remote metadata cannot invent a channel or lower
the client's local trust policy.

## Discovery

The canonical descriptors are named `vulcan-update-channel.json`:

| Channel | Descriptor |
| --- | --- |
| `stable` | `https://github.com/tionis/vulcan/releases/latest/download/vulcan-update-channel.json` |
| `main` | `https://github.com/tionis/vulcan/releases/download/main/vulcan-update-channel.json` |

A client may use an explicit HTTPS descriptor URL for mirrors and tests, but still supplies the
expected channel independently. Redirects remain HTTPS and are bounded.

## Signed envelope

The descriptor is a strict JSON object. Unknown fields are rejected by the reference client.

```json
{
  "schema_version": 1,
  "payload": "<base64 of the exact UTF-8 payload bytes>",
  "signatures": [
    {
      "algorithm": "ed25519",
      "key_id": "release-2026",
      "signature": "<base64 Ed25519 signature over the decoded payload bytes>"
    }
  ]
}
```

Signatures cover the decoded payload bytes directly. Consumers must not parse, reserialize, or
otherwise canonicalize those bytes before verification. Publishers currently emit compact JSON
with lexicographically sorted keys, but that encoding is a publisher rule rather than part of
signature verification. Multiple signatures permit an overlap window during key rotation. A
signature is trusted only when both its `key_id` and Ed25519 public key match a key compiled into or
otherwise configured by the client.

The decoded version-1 payload is:

```json
{
  "schema_version": 1,
  "product": "vulcan",
  "channel": "stable",
  "version": "0.2.0",
  "source_commit": "<40 hexadecimal characters>",
  "published_at": "2026-08-31T20:00:00Z",
  "prerelease": false,
  "artifacts": [
    {
      "target": "x86_64-unknown-linux-gnu",
      "kind": "archive",
      "format": "tar.gz",
      "url": "https://example.invalid/vulcan-0.2.0-x86_64-unknown-linux-gnu.tar.gz",
      "sha256": "<64 hexadecimal characters>",
      "size": 123456,
      "top_level_directory": "vulcan-0.2.0-x86_64-unknown-linux-gnu"
    }
  ]
}
```

The payload contains exactly one portable `archive` record for each supported target. Version 1
supports `tar.gz` and ZIP archives. Artifact URLs use HTTPS, and the declared size, SHA-256 digest,
top-level directory, target, and exact executable member are verified before replacement. Metadata,
archives, and extracted executables have independent size bounds.

## Client policy

`vulcan self-update` is only for a manually installed portable executable. Package-managed installs
must use their package manager so its database and packaged files remain coherent.

The client performs these checks in order:

1. fetch a bounded envelope over HTTPS;
2. verify an Ed25519 signature against local trusted keys when signatures are required;
3. validate product, schema, expected channel, semantic version, source commit, and exact target;
4. require the available semantic version to be newer than the running build;
5. download the bounded archive and verify its exact size and SHA-256 digest;
6. extract only the expected regular-file executable within a bounded expanded tar stream; and
7. acquire a same-directory update lock, stage and sync a new file, preserve executable
   permissions, and replace the running path with rollback on installation failure.

An explicit `--allow-downgrade` is required to reinstall the same version or move backwards. This
also makes replayed older signed metadata non-mutating by default. `--dry-run` downloads and verifies
the complete artifact but does not change the executable. A daemon using the old process image must
be restarted after a successful update.

`--allow-unsigned` is a local, explicit checksum-only exception. It does not become part of channel
metadata and cannot be requested by a server. HTTPS and SHA-256 protect transport and detect damaged
bytes, but without a trusted signature they do not authenticate a compromised forge or publisher.

## Publication and package registries

Stable release versions use the workspace semantic version. A rolling build increments the next
patch position and appends `-dev.<UTC date>.<workflow run>.g<commit prefix>`, for example
`0.1.1-dev.20260831.412.g0123abcd`. Its binary, archives, Debian version, manifest, descriptor, and
release title derive from that one version.

The scheduled rolling workflow runs at most once per day, does nothing when `main` has not advanced,
and publishes only a commit whose required push CI succeeded. It reuses the canonical builders and
one fixed `main` prerelease/tag, uploads the replacement before pruning superseded assets, and does
not repeat the complete test suite.

Future Homebrew, WinGet, APT, or other registries should map their stable/default stream to `stable`
and expose `main` only through an explicit development opt-in. Registries consume the same artifact
manifest and channel meaning, but their native package manager remains responsible for update and
rollback. They must not invoke portable self-replacement behind the package manager.

## Current signing state

The first dedicated rolling-channel identity was created on 2026-09-01. Its public identity is:

- key ID: `main-2026-09`
- raw Ed25519 public key (base64): `6gbtjy5nGZoT8kFAfYELB5x73S34kjv+/tPn8XEjrg0=`
- SHA-256 fingerprint of the raw 32-byte public key:
  `5486dec9f64d452becdcf091dca0e51ade004baf089cc31ece7ba180d8c7b7f3`
- authority: `main` only; it must never authorize `stable` metadata

The operational private key remains machine-local at
`~/.config/vulcan/release-signing/main-2026-09.pem`. Its recovery copy is stored only as the
SOPS-encrypted Grimoire admin secret
`secrets/groups/admin/vulcan-update-main.sops.yaml`. Neither private representation belongs in this
repository, build logs, workflow artifacts, or GitHub Actions secrets.

Release builds now embed the public identity in a channel-scoped trusted-key ring. A key is eligible
only for its compiled channel, so `main-2026-09` cannot authorize `stable` even when a cryptographic
signature is otherwise valid. Multiple entries and envelope signatures allow bounded overlap during
future rotations.

GitHub Actions deliberately publishes a checksum-only rolling descriptor and has no access to the
private key. The key-holding machine runs `scripts/release/sign_rolling_release.py`, which fails
closed unless both CI and the rolling workflow succeeded for the exact commit named by the `main`
tag. It downloads the complete release and independently checks the release inventory, canonical
manifest, exact five-archive/two-Debian artifact set, sizes, SHA-256 hashes, `SHA256SUMS`, rolling
version, source commit, channel, timestamp, URLs, layouts, and canonical unsigned payload. It then
rechecks the release for races, replaces only `vulcan-update-channel.json`, and reads the uploaded
bytes back. An already-valid signature is an inexpensive idempotent no-op; any other existing
signature fails closed.

On the signing machine, preview and install the hourly systemd user timer with:

```sh
python scripts/release/sign_rolling_release.py \
  --signing-key ~/.config/vulcan/release-signing/main-2026-09.pem --dry-run
python scripts/release/install_rolling_signer.py install \
  --signing-key ~/.config/vulcan/release-signing/main-2026-09.pem --dry-run
python scripts/release/install_rolling_signer.py install \
  --signing-key ~/.config/vulcan/release-signing/main-2026-09.pem
```

The installer copies the two required signer scripts into a private user libexec location, writes a
hardened oneshot service and hourly timer, enables the timer, and immediately invokes the service.
It stores only the key path in the unit, never key contents. The service uses the machine's existing
GitHub CLI authentication. Inspect failures with
`journalctl --user -u vulcan-rolling-signer.service`; they are visible and leave the descriptor
unchanged. Uninstalling the timer preserves the signing key.

No stable signing identity is configured yet. Stable descriptors therefore remain unsigned and
require the explicit checksum-only exception until that separate identity and bootstrap exist.
