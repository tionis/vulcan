# Cryptographic Device Identity

Status: accepted design for Roadmap 12.15; not yet implemented.

## Purpose

Vulcan needs one stable actor identifier for remote safety heads, synchronization provenance, and
future device-scoped integrations. The identifier should not depend on a hostname, operating-system
installation ID, vault, user account, or rebuildable cache. It should also have a cryptographic
meaning without requiring a trust registry in the first implementation.

The fundamental rule is:

> One device identity is one Ed25519 SSH keypair. A different key is a different device.

Here “device” means a cryptographic installation, not a physical machine or a friendly name. A future
inventory can label successive installations on the same machine without equating their identities or
transferring authority. Device keys are dedicated to device identity; administration and release keys are
separate in the first key-manager version.

There is no key rotation or device-continuity protocol. Replacing or losing a key means setting up a
new device. The old device remains a separate historical identity whose recovery heads are retained
until an operator safely integrates and removes them.

## Scope and non-goals

The first implementation provides:

- generation and durable local storage of one OpenSSH-compatible Ed25519 keypair;
- a versioned, Git-ref-safe device ID derived only from the canonical public key blob;
- read-only inspection and explicit public-key export;
- use of the derived ID in sync safety refs, reports, and commit provenance; and
- a lossless migration from the existing random ULID identifier.

It does not initially provide:

- a device trust registry, ownership, approval, revocation, or authorization;
- key rotation, predecessor/successor statements, or continuity between identities;
- keychain, `ssh-agent`, hardware-token, or passphrase management;
- automatic registration of the public key with a Git forge or SSH server;
- automatic selection of the device key for Git transport authentication;
- commit signing or signed device statements; or
- synchronization of private key material between devices.

Those features may build on the identity later, but none changes the device-equals-key rule.

## Key and identifier format

Version 1 identities use an ordinary `ssh-ed25519` key. The public key is parsed as SSH wire data,
not compared as `.pub` file text. Vulcan must reject an algorithm label that disagrees with the
decoded blob, malformed framing, a non-Ed25519 key, trailing wire data, or a key whose Ed25519
payload is not exactly 32 bytes.

The identifier is:

```text
vdev1_<lowercase-RFC4648-base32-without-padding(SHA-256(ssh-public-key-wire-blob))>
```

The full 256-bit digest is retained; it is not shortened for display. The `vdev1_` prefix makes the
grammar and derivation version explicit, while the lowercase RFC 4648 alphabet (`a-z2-7`) without
padding makes the 52-character digest, and therefore the 58-character complete ID, safe as one Git
ref component. The hashed input includes the SSH algorithm name because it is part of the wire blob.
Comments, whitespace, path names, hostnames, and user-supplied fingerprints do not participate.

Human output may additionally show the conventional OpenSSH `SHA256:` fingerprint and an abbreviated
device ID, but JSON, ref names, comparisons, and mutation commands always use the complete device ID.
Friendly device names are future mutable metadata and never identity.

`GitSyncDeviceId` becomes a versioned parser rather than a ULID wrapper. It accepts both the exact
legacy 26-character lowercase Crockford-Base32 ULID grammar and the exact `vdev1_` grammar so old
recovery heads remain manageable. New identities can only be key-derived.

## Local storage and initialization

The public identity manifest lives once per Vulcan installation under the platform Vulcan user-data
directory, outside all vaults, rebuildable caches, Git repositories, and synchronized trees. The
initial `file_v1` provider uses this layout:

```text
device/
  identity.json
  id_ed25519
  id_ed25519.pub
```

`id_ed25519` is present only for `file_v1`; later providers keep private material in the provider named
by the manifest. `identity.json` is bounded, closed, versioned metadata with exactly `version`,
`scheme`, `device_id`, `public_key`, and `key_provider` fields. Version 1 uses scheme
`ssh-ed25519-sha256-v1`, canonical
two-field OpenSSH public-key text without a comment, and initially the `file_v1` provider. The provider
is a closed versioned discriminator, not an arbitrary path or command; `device-key-custody.md` defines
the later `system_secret_v1` and `ssh_agent_v1` alternatives. The manifest contains no friendly name,
trust decision, predecessor, vault list, timestamp, or absolute path. On every load Vulcan parses the
public key and recomputes the ID. An operation needing the private key resolves the exact provider and
proves that it matches the public key. Stored IDs and `.pub` comments are never derivation inputs.

Key generation uses the operating system CSPRNG and a reviewed SSH key parser/serializer rather than
hand-written OpenSSH private-key encoding. It writes an unencrypted OpenSSH private key so the current
unattended daemon can use it. This is explicitly a filesystem-protected credential until later keychain
or agent support exists: on Unix the directory/private file are owner-only, and loads reject symlinks
and group/other-accessible private keys; Windows uses a private per-user directory, inherited ACLs, and
reparse-point rejection. The private key and its contents never appear in normal output, JSON, logs,
notifications, crash diagnostics, or vault configuration.

Initialization is serialized, no-clobber, and crash-recoverable. Files are prepared in the destination
filesystem, permissions and the matching public key are verified, durable files are replaced atomically,
and the manifest becomes authoritative last. A complete matching keypair left before the manifest by an
interruption may be adopted; partial, mismatched, over-permissive, or malformed material fails closed and
is never overwritten automatically.

Read-only operations never initialize identity. The first mutating operation that requires an identity
may initialize it when no identity artifacts exist, preserving today's frictionless first-sync behavior.
An explicit `vulcan device init` provides the same operation, and `--dry-run` reports the algorithm and
destination without generating a key or pretending to know the resulting ID.

## First-class command and report surface

The singular `vulcan device` group describes the local cryptographic identity; the existing plural
`vulcan sync devices` group continues to manage per-repository remote recovery heads.

- `vulcan device show` is read-only and reports `uninitialized`, `ready`, `degraded`, `legacy`, or
  `invalid`, the complete ID when available, scheme, public fingerprint, provider, provider
  availability, and migration guidance. It does not reveal private material, unsafe provider locators,
  or create directories.
- `vulcan device init [--dry-run]` creates the identity only when no current key identity exists. It
  never overwrites or silently replaces a key.
- `vulcan device public-key` explicitly emits the canonical OpenSSH public key for later manual
  registration. Structured output keeps it in a named field so scripts need not scrape prose.
- `sync status`, `sync doctor`, `sync devices list`, and per-vault daemon status expose the current
  device ID and identity state. Remote device lists distinguish `ssh_key_v1` from `legacy_ulid` and do
  not imply that either is trusted.

Creating a replacement identity on an already initialized installation is deliberately outside this
slice. Until a reviewed lifecycle command exists, Vulcan refuses to overwrite the current identity.
A fresh installation or explicitly relocated old identity directory can initialize a new device; this
does not assert continuity with the old key.

Replacement and independent re-enrollment are prerequisites for any later feature that requires device
keys for access. That workflow must work after loss of the old private key, preserve old public identity
and recovery refs, capture current bytes, and list known remote bindings to revoke/re-enroll. It must
not silently restore an old private key onto a new installation or require the lost key to authorize
its replacement. Remote re-enrollment uses the relevant independent administrator authority.

## Synchronization integration and migration

The identity is global to the local Vulcan installation and shared by all its vaults. It remains an
actor/provenance input, not vault content. The device ID continues to name:

```text
refs/heads/__vulcan-sync/devices/<profile>/<device-id>
```

and continues to appear in `Vulcan-Sync-Device` trailers and typed reports. The complete captured tree
must still reach the leased device safety head before conflict-prone canonical reconciliation. Identity
work must not weaken capture-before-apply, compare-and-swap, conflict preservation, or recovery-ref
retention.

The ref contract moves to namespace version 2 because the accepted device-ID grammar changes. Rollout
is deliberately two-stage:

1. A compatibility release reads legacy ULIDs, key IDs, and namespace versions 1 and 2 while still
   creating the old identity form. It must tolerate mixed legacy/key device heads.
2. Only after that reader is deployed does a later release generate key identities and write namespace
   version 2 provenance.

An existing installation keeps its legacy `_device.json` intact during migration. The first explicit
initialization or identity-requiring mutation creates a new key identity and therefore a new remote
device head. Existing commits, trailers, conflict records, journals, recovery refs, and legacy remote
heads are never rewritten, relabelled, or automatically deleted. The legacy head remains visible as a
different historical device and uses the ordinary fetch, compare, integrate, and exact-lease removal
workflow. Local migration metadata may report the legacy ID, but it is not a predecessor claim and is
not published as cryptographic continuity.

Old binaries that do not understand namespace version 2 must fail closed and require upgrade. The
two-stage rollout prevents that failure during an intentionally supported rolling upgrade; it does not
promise indefinite write compatibility with unupgraded clients.

## Security and failure semantics

A key-derived ID proves only that identical public-key bytes produce identical names. Until signed
statements are implemented and verified, a remote ref or commit trailer containing an ID is an
unauthenticated claim: another remote writer can spell that ID. User interfaces must say
`key-derived`, not `verified` or `trusted`.

Copying the private key copies the device identity. Two installations using the same private key are
the same logical device, not related devices. This is unsupported as an ordinary topology; Vulcan should
diagnose divergent use prominently. The existing same-device bridge remains a loss-prevention fallback
and must not be presented as identity continuity or proof of legitimate cloning.

If the manifest exists but its configured private-key provider is locked, missing, unsafe, or
unreadable, Vulcan does not generate a replacement. The public manifest remains sufficient for ordinary
unsigned ref naming and provenance, so byte-preserving sync may continue when the active transport does
not use the device key. Any operation that actually requires device signing or key-backed SSH
authentication fails closed after preserving local recovery state. A provider that returns a different
key makes the identity invalid rather than degraded. If the entire identity directory is intentionally
absent, the installation is uninitialized and explicit initialization creates a different device.

Protocol integrations require typed payloads, local permission, and separate remote authorization.
The initial key manager uses separate device, administration, release, SSH-authentication, and Git-signing
keys; it provides no generic signing endpoint or implicit default key. Any later reviewed sharing of
usages must preserve that separation of authority. Vulcan never silently replaces the user's existing
Git SSH identity or uploads a public key to a forge.

Generated internal synchronization commits remain deterministic and unsigned. A future signing layer
should publish domain-separated signed attestations over exact snapshot/ref/profile/protocol inputs,
rather than changing deterministic commit IDs or making local capture depend on signing availability.
Human/semantic Git commit signing is a separate opt-in integration.

## Deferred layers

The accepted custody design in `device-key-custody.md` adds explicit system-secret and SSH-agent
providers plus same-key storage migration without changing identity. The accepted higher-level design
in `key-management.md` adds typed key inventory, usages, principals, lifecycle, and accepted public
registries. Later designs may add device naming and fleet administration, hardware-backed storage,
transport configuration, signed device statements, semantic commit signing, notifications addressed
to devices, and encryption. They must preserve these boundaries:

- key replacement creates a new device, without a succession or continuity assertion;
- authentication and signing usages are authorized independently; key reuse is not an implicit default;
- trust is derived from an explicit local or accepted registry state, never from a ref name, comment,
  commit author, forge badge, or self-asserted principal; and
- loss of trust metadata cannot make preserved vault bytes unreachable.

Roadmap 12.17 defines a typed key manager and linear, administrator-signed registry in
`key-management.md`. Any existing administrator can authorize a change. Devices can keep using the
last accepted state offline; revocations propagate when updates arrive. Registry support remains
separate from and is not a dependency of the initial device identity implementation.
