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
| `main` | `https://github.com/tionis/vulcan/releases/download/rolling-main/vulcan-update-channel.json` |

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

Two signature algorithms exist, both over Ed25519 keys:

- `ed25519`: `signature` is the base64 raw 64-byte Ed25519 signature over the payload bytes.
- `sshsig-ed25519`: `signature` is the base64 binary (de-armored) blob that
  `ssh-keygen -Y sign -n vulcan-update@tionis.dev` emits for the payload bytes, as specified by
  OpenSSH's `PROTOCOL.sshsig`. The client requires signature version 1, the exact namespace
  `vulcan-update@tionis.dev`, an empty reserved field, the `sha512` message hash, an `ssh-ed25519`
  key equal to the trusted key, and no trailing bytes. This lets a key that never leaves a
  smartcard sign through `ssh-agent`, and the namespace keeps signatures that key makes for other
  purposes (such as Git commits) from authorizing updates.

Unknown algorithms are ignored, so an envelope can carry signatures that older clients skip.

The decoded version-1 payload is:

```json
{
  "schema_version": 1,
  "product": "vulcan",
  "channel": "stable",
  "version": "0.2.1",
  "source_commit": "<40 hexadecimal characters>",
  "published_at": "2026-08-31T20:00:00Z",
  "prerelease": false,
  "artifacts": [
    {
      "target": "x86_64-unknown-linux-gnu",
      "kind": "archive",
      "format": "tar.gz",
      "url": "https://example.invalid/vulcan-0.2.1-x86_64-unknown-linux-gnu.tar.gz",
      "sha256": "<64 hexadecimal characters>",
      "size": 123456,
      "top_level_directory": "vulcan-0.2.1-x86_64-unknown-linux-gnu"
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
one fixed `rolling-main` prerelease/tag, uploads the replacement before pruning superseded assets,
and does not repeat the complete test suite. The `rolling-main` tag is created once and then left
immutable: the workflow only replaces that release's assets in place, so ordinary Git fetches never
need a forced tag update and mirrors never observe a moved ref. The tag name deliberately differs
from the `main` branch name so it cannot collide with the source branch. Because the fixed tag no
longer names a build commit, the signing workflow anchors the release to the exact source commit
instead. Once the replacement release is published successfully, the workflow removes the historical
`refs/tags/main` ref if it still exists; it never removes that ref before the replacement is
available.

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

The operational private key is stored as the `VULCAN_MAIN_UPDATE_SIGNING_KEY_PEM` secret in the
dedicated GitHub Actions environment `rolling-release-signing`. A protected operator copy remains
at `~/.config/vulcan/release-signing/main-2026-09.pem`, and the Git-canonical recovery copy is the
SOPS-encrypted Grimoire admin secret
`secrets/groups/admin/vulcan-update-main.sops.yaml`. Private material must never enter this
repository, ordinary repository-level secrets, build logs, or workflow artifacts.

Release builds now embed the public identity in a channel-scoped trusted-key ring. A key is eligible
only for its compiled channel, so `main-2026-09` cannot authorize `stable` even when a cryptographic
signature is otherwise valid. Multiple entries and envelope signatures allow bounded overlap during
future rotations.

The rolling build workflow never publishes an unsigned descriptor under the client-facing name. It
stages its checksum-only envelope as `vulcan-update-channel.unsigned.json` beside the previous
build, whose signed `vulcan-update-channel.json` and referenced archives it deliberately keeps:
GitHub can continue resolving a release download name to a replaced asset for minutes, so a client
reading the old descriptor during the handoff must still find its archives. The next build prunes
that retained generation. The gate also treats a staged descriptor for the same commit as already
built, because only the signer (not a rebuild) can complete it. Its successful completion triggers the separate `sign-rolling-release.yml` workflow, whose only signing job uses
the protected `rolling-release-signing` environment. The job checks out the exact source commit,
materializes the environment secret into a mode-restricted ephemeral runner file, invokes
`scripts/release/sign_rolling_release.py --expected-commit <sha>`, and deletes that file when the
step exits. A manual dispatch with an explicit full commit ID provides an idempotent repair path.

The signer fails closed unless both CI and the rolling workflow succeeded for the exact commit
supplied as `--expected-commit` (the triggering rolling workflow's `head_sha`, which equals the
published `source_commit`). Because `rolling-main` is a fixed channel pointer rather than a
per-build tag, the signer no longer requires the tag to name that commit; it ties the descriptor to
the expected commit and the successful workflow runs instead. It downloads the complete published
release and independently checks the release inventory, canonical manifest, exact
six-archive/two-Debian artifact set, sizes, SHA-256 hashes, `SHA256SUMS`, rolling version, source
commit, channel, timestamp, URLs, layouts, and canonical unsigned staged payload. It then rechecks
the release for races, publishes the signed `vulcan-update-channel.json`, reads the uploaded bytes
back through the API, removes the staged envelope, and polls the public
`releases/download/rolling-main/vulcan-update-channel.json` URL that clients use until it serves the
signed bytes. Failing to converge within ten minutes fails the workflow even though the signature
was published, so origin-side download-path lag stays visible. This check observes only the
runner's own CDN edge: clients behind other edges were observed receiving the previous descriptor
for about a minute after the signer had converged. That lag is benign by construction, because
the previous descriptor is signed and its archives are retained until the following build. An already-valid signature is an inexpensive
idempotent no-op that also removes a leftover staged envelope; any other existing signature fails
closed. No developer workstation,
resident process, or systemd timer participates in the normal rolling release path.

The current stable-channel identity was added on 2026-10-04:

- key ID: `stable-2026-10`
- algorithm: `sshsig-ed25519`
- raw Ed25519 public key (base64): `dk91fcu3uqtH5h0q/FWJ/qjlBb65wGojrngIuiusEU4=`
- OpenSSH public key:
  `ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHZPdX3Lt7qrR+YdKvxVif6o5QW+ucBqI654CLorrBFO`
- OpenSSH fingerprint: `SHA256:HYpEy7eKcHkSUuL1y4ADmFk89G0zkkI0jfVXucG/juQ`
- SHA-256 fingerprint of the raw 32-byte public key:
  `d468ccf04842c27b6c1f2234534c51d425f7b6b68ebf7689a1b05a73303806b6`
- custody: the operator's OpenPGP card, exposed to `ssh-agent` by `gpg-agent`; no private key file
  exists. The signer passes the public key file to `ssh-keygen -Y sign -f`, and the card prompts for
  its PIN or touch.
- recovery: the operator keeps an offline copy of the key, so a lost or broken card is replaced
  by loading that copy onto a new card rather than by a trust reset
- authority: `stable` only; it must never authorize `main` metadata

Stable signing is approval-gated rather than scheduled: there is no service, timer, Actions secret,
or unattended stable signer.

After a version-tag workflow succeeds, an operator supplies both the exact immutable tag and its
full commit ID. The signer requires the non-prerelease tag to be exactly `v<version>`, verifies the
successful `release.yml` tag run for that commit, downloads and validates the complete release with
the same canonical artifact checks as the rolling signer, rechecks the release for races, replaces
only the descriptor, and verifies API readback. The version-tag workflow creates the release with
`make_latest: false`, so `releases/latest`, which the stable channel URL resolves through, keeps
pointing at the previous signed release until the signer promotes the new one. Promotion happens
only after the signature is published and never moves `latest` to an older version than the current
one; rerunning the signer on an already-signed release completes a missed promotion. The signer then
polls the client-facing `releases/latest/download/vulcan-update-channel.json` URL until it serves the
signed bytes:

```sh
python scripts/release/sign_stable_release.py \
  --tag v<version> \
  --expected-commit <full-40-character-commit> \
  --ssh-signing-key ~/.config/vulcan/release-signing/stable-2026-10.pub \
  --dry-run
python scripts/release/sign_stable_release.py \
  --tag v<version> \
  --expected-commit <full-40-character-commit> \
  --ssh-signing-key ~/.config/vulcan/release-signing/stable-2026-10.pub
```

The dry run also signs, so the card is asked once per run. `ssh-agent` (here `gpg-agent`) must be
able to prompt for the PIN on the operator's terminal.

The `v0.1.0` release predates this descriptor contract, so there is no older stable descriptor to
retrofit or sign.

Rotation uses an overlap release whose envelope carries signatures from both the retiring and new
stable keys while clients embed both public keys. A later out-of-band release removes the retiring
key.

Stable trust history:

- `stable-2026-09` (raw Ed25519 `sOrBt76ruZ2kSR+4glX9k/ZjSoS1YSvmK9yMSVCiWpE=`, created 2026-09-02,
  file-held) was the first stable identity. `v0.2.1` was its trust bootstrap; older binaries needed
  one checksummed manual installation.
- `v0.3.0` embedded both keys and its descriptor carried both signatures, so every `v0.2.1` binary
  could self-update into trusting `stable-2026-10`.
- The release after `v0.3.0` stops embedding `stable-2026-09` and is signed only with
  `stable-2026-10`. Binaries older than `v0.3.0` can no longer verify the stable channel and need one
  checksummed manual installation. The retired private key and its SOPS recovery copy are then
  destroyed.

If a stable private key is lost before an overlap, or suspected compromised, stop signing with it,
remove its trust in a manually verified release, install that release through checksums/packages,
and resume with a new identity. A compromised key cannot securely authorize its own revocation.
