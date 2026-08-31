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

The format, signer, verifier, and key-rotation envelope are implemented, but no project release
identity is configured yet. Consequently current stable and rolling descriptors are unsigned and
the client refuses them unless the user explicitly supplies `--allow-unsigned`. Production signing
requires an offline-managed Ed25519 identity, an embedded trusted public key and key ID in release
builds, protected CI signing access, documented rotation/revocation, and release-checklist evidence.
