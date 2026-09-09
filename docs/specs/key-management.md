# Cryptographic Key Management and Registries

Status: accepted design for Roadmap 12.17 and the optional 12.18 synced-secrets extension; not yet
implemented. This design round is complete. Exact storage schemas, command details, and conformance
fixtures remain implementation tasks within these boundaries.

This specification builds on `device-identity.md` and `device-key-custody.md`. It defines a small
local key manager and an independently verifiable public registry for single users and small teams.
Existing permissions remain usable during poor connectivity. Registry updates are signed by one
authorized administrator and propagate when devices reconnect. Consumer adapters ship separately;
the existing release updater's trust configuration is unchanged.

## Decisions and scope

This specification defines Vulcan's key-management protocol, authorization rules, and implementation
boundaries. Versioned schemas and conformance tests make these invariants executable before release.

Four boundaries have distinct jobs:

1. `SecretStore` protects bounded opaque bytes, including non-key credentials.
2. `KeyProvider` uses one exact key. `DeviceKeyProvider` is its device-bound adapter, not a second
   custody implementation.
3. `KeyManager` checks local permission and typed usage before invoking a provider.
4. `KeyRegistry` verifies public bindings at one exact accepted Git checkpoint.

The first version supports ordinary Ed25519 SSH keys, one active provider per key, exact usages,
and one administrator signature per registry change. There is no generic signing endpoint, namespace
pattern language, delegated registry policy, certificate authority support, or encryption support.
Security-key algorithms and authentication-only agent handles need separate conformance work.
The optional age-based extension below adds synced secrets independently of the signing registry.

Device, administration, and release keys are separate keys. V1 rejects combining these roles, including
through import of the same public material. Later Git signing and SSH authentication use their own
keys and explicit integrations. Adding a key never grants permission to use it or makes it trusted.

The trusted local OS account can read file-backed keys or invoke an external agent without Vulcan.
Local usage checks constrain Vulcan's managed entrypoints; they cannot constrain that account or a
compromised process. Remote consumers must independently check signatures and authorization.

## Key identity and local inventory

The immutable identifier is:

```text
vkey1_<lowercase-RFC4648-base32-without-padding(SHA-256(ssh-public-key-wire-blob))>
```

Hash the complete canonical SSH wire blob, including its algorithm. Reject malformed framing,
trailing data, algorithm disagreement, and non-canonical IDs. The full digest matches the digest in
an Ed25519 `vdev1_` ID, but these remain separate types: a key need not identify a device.

The immutable public record contains only `version`, `key_id`, `algorithm`, and `public_key`.
V1 accepts `ssh-ed25519`. Labels, origin, provider bindings, and lifecycle are separate mutable metadata;
filenames, comments, principals, and labels never contribute to identity.

The inventory and secret-free operation journals live in device-local storage outside vaults, Git
repositories, and `cache.db`. One key has one authoritative custody binding. Other known private copies
are inactive recovery material, never fallback. General keys use logical names derived from `vkey1_`;
the existing device key retains its manifest and device-specific location. Its inventory entry is a
projection of `identity.json`, never a second mutable provider selector. Device custody commands and
key-manager commands use the same lock and migration workflow.

Enrollment is explicit:

- **generate:** create a key in a selected durable provider, with no replacement;
- **import-public:** add or idempotently confirm an exact public record;
- **import-private:** validate the matching pair and attach custody to an existing public-only record,
  or create a new record; never replace an existing private item;
- **discover:** inspect public candidates without enrollment, trust, or signing; and
- **adopt:** bind an exact external signing key after an explicitly interactive possession test.

Imports accept stdin or an existing owner-only input file, never private bytes in command arguments.
Private export is deferred to a separate encrypted, owner-only, no-clobber recovery format. Normal
output, JSON, logs, journals, notifications, and completion never expose secrets or unsafe locators.

## Typed operations and permission checks

Initial operations are local key administration, a fresh local possession test, and registry change
signing. Device attestations, Git commit/tag signing, SSH authentication, and release signing are
separate future adapters with fixed payload contracts. Unknown operations are rejected.
V1 usage bindings are `device-identity` scoped to an exact device ID and `registry-admin` scoped
to an exact registry ID. Possession tests are local administration, not a transferable signing usage.
Additional usage kinds require a versioned adapter and verifier contract before enrollment is enabled.

Callers request an operation, not arbitrary bytes plus claimed security metadata. For example,
`sign_registry_change(registry_id, change_id, key_id)` loads the immutable change and constructs
the signed payload internally. A possession test generates a fresh random challenge internally and
uses the fixed namespace `vulcan-key-proof-v1@tionis.dev`; callers cannot supply the challenge bytes.
Registry commits use the standard SSHSIG `git` namespace and dedicated registry-administration keys.
The typed operation validates the registry ID, sole parent, and resulting tree before signing; a
generic Git signing operation cannot use these keys. There is no public `sign(bytes, namespace)` or
wildcard SSHSIG usage that bypasses these checks.

For every managed operation the application service:

1. derives the caller's authority from its trusted execution context, never a supplied subject field;
2. checks local permission for the exact key, operation, and resource, including immediate local denial;
3. loads the exact usage binding and, when required, one explicitly selected registry checkpoint;
4. checks registry eligibility and the separate resource capability grant where applicable;
5. constructs or validates the exact payload and its resource identity; and
6. invokes the one provider with an enforceable prompt policy and bounded input/output.

These checks are intersections, not alternative ways to gain authority. A local allow cannot override
registry denial, and a registry binding cannot grant local custody access or vault permissions. A
request cannot choose a more permissive registry: trusted resource configuration selects it. Principal
identities are `(registry_id, principal_id)` pairs; matching names in another registry confer nothing.
V1 never unions authority across registries.

Local key administration is restricted to the trusted installation owner. MCP, JS, plugins, companion,
and daemon clients receive no key administration or signing capability merely because they have Git,
network, or note-write permission. Exposing a typed operation to them requires an explicit permission
contract and tests. Raw provider and secret-store interfaces stay below that boundary.

An operation pins all relevant policy revisions, then rechecks local denial and accepted-checkpoint
preconditions immediately before dispatch. A signature already issued cannot be recalled; concurrent
revocation affects subsequent operations. Historical signature verification uses the checkpoint named
in its evidence, not whichever local key happens to be active today.

## Device identity and replacement

A device ID identifies a cryptographic installation, not a physical machine or a durable human name.
Keep `vdev1_` key derivation and existing recovery refs. A friendly name can later label installations;
it is never an authorization identifier or proof of continuity.

Moving the same key changes custody only. Replacing it creates a new device ID and requires explicit
re-enrollment wherever the old device had authority. Before any feature requires device-key-backed
access, ship a replacement workflow that captures local bytes, retains the old public identity and
recovery heads, creates the new key without overwriting the old one, and reports every known binding
requiring re-enrollment. Loss of the old private key must not block local replacement. Revoking the old
remote binding requires an authorized registry administrator, not possession of the lost key.

Copying private material still clones the same logical device. Do not present a restored clone as a
new device, or infer authorization from a replacement label. A lost device key does not lose Markdown
bytes; retained recovery objects remain inspectable without accepting that device as trusted.

## Registry v1: administrators and linear history

A registry is a dedicated Git repository of public data with strict versioned schemas:

```text
registry.json              # version, immutable registry_id, admins
principals/<id>.json        # stable registry-local principal IDs
keys/<key-id>.json          # immutable public records
bindings/<id>.json          # principal, key, exact usage/scope, status, reason
```

Its canonical repository may be on local disk or a shared Git remote. Publication needs access to the
configured canonical ref, not necessarily the internet. A local canonical registry works fully offline;
a disconnected replica queues changes. Connectivity failure never promotes a replica to canonical.

`registry_id` is a newly generated ULID pinned with the bootstrap checkpoint. V1 principals are people,
services, or devices; groups and delegated namespace ownership are deferred. Each key belongs to one
principal within a registry. Public keys and principals are retained historically; disabling or
retiring a binding changes its status, not the underlying key bytes. A compromised key disables every
binding for that key within the registry; local compromise immediately denies all local use.

`admins` lists registry-local principal IDs. Start with one administrator: the owner. A small team may
add administrators, and each can independently
authorize any registry change, including adding/removing administrators or keys. An administrator may
have several enrolled keys, for example one on each trusted installation and an offline recovery key.
Administration keys are distinct from device keys; copying a device key is not the way to share control.

Every change needs exactly one valid signature from an active administrator key in its accepted parent.
New keys or administrators cannot authorize their own introduction. The resulting state must retain
at least one administrator with an active eligible key. All administrators have the same authority;
adding one is an explicit grant of full registry control. Administration keys have no other usages.

Key replacement uses another already enrolled administrator key, including an owner's recovery key.
If all administrator keys are lost, recovery requires an explicit independently confirmed checkpoint
reset on each consumer. Recovery keys use the same enrollment and signing rules as other administrator
keys; there is no separate approval or recovery-policy mechanism.

Bootstrap pins `(registry_id, git_object_format, checkpoint_oid)` through an independent trust decision.
Fetch never bootstraps trust. Load only exact Git objects using an installed verifier, without executing
candidate hooks, filters, schemas, or policy code. Reject replace refs/grafts, shallow or missing
required history, unsupported fields and algorithms, duplicate IDs/ownership, symlinks, gitlinks, and
ambiguous bindings. Bound objects, signatures, record counts, and traversal work.

After bootstrap, every accepted commit has exactly one parent equal to the previous accepted commit.
Verify every intervening commit and its complete resulting graph, including changes later reverted.
Merges and history rewriting are rejected. Pending changes may be rebased before signing;
a changed base or payload requires a new signature. The immutable registry ID cannot change in a normal
transition. The bootstrap checkpoint itself must pass structural and administrator-eligibility validation.

### Signed changes and publication

Use an ordinary Git SSH commit signature: exactly one `gpgsig` header in SHA-1 repositories or
`gpgsig-sha256` in SHA-256 repositories, with standard continuation-line framing and namespace `git`.
Verify the exact raw commit content with that signature header removed, excluding the Git storage
prefix; preserve every other byte. Reject additional/unknown signature headers, malformed framing,
unsupported algorithms, and signatures that do not resolve to an active administrator key in the parent.
The signature authenticates the commit; Vulcan additionally verifies registry identity, parent-state
authority, and the complete resulting schema. A forge badge or `git verify-commit` alone is insufficient.

Preparation fixes the tree, sole parent, author/committer fields, and message. The administrator reviews
and signs those exact bytes locally, including while offline. One command may prepare and sign a change;
separate review is available but no other administrator's approval is required. Store signed changes
as durable pending work outside accepted refs. Pending grants do not yet confer authority; local denial
can take effect immediately. Byte-level SHA-1/SHA-256 interoperability vectors are a release gate.

When the canonical repository is reachable, read and verify its history before publishing pending changes.
Publication uses an exact lease on the verified parent (local compare-and-swap or remote push lease).
If another administrator published first, retain the pending change, show its diff against the new
accepted state, and require explicit review and re-signing before retrying. Recheck the signer's current
authority; a removed key cannot publish
against an old base. This is routine pending-work reconciliation, not a reset of accepted trust.

Publication uncertainty is resolved by reading back the exact signed object; never overwrite a different
head or duplicate an uncertain publication. Observed remote state is untrusted evidence. The accepted
ref advances by compare-and-swap only after verification; signed local pending work does not advance it
until its canonical publication is confirmed. Readers inspect accepted objects, never a mutable checkout.

A valid suffix proves authorization relative to its accepted ancestor. It cannot establish that this
is the only authorized branch anywhere. Preserve both sides of a fork and require explicit independent
checkpoint recovery; do not automatically merge competing registry histories.

## Offline trust and revocation

The default policy is to use the last accepted registry state while disconnected, without expiry or an
online check for each operation. New signatures still require an available local provider, and normal
resource permissions still apply. Connectivity failure does not expire otherwise valid permissions or
disable local reading, editing, capture, recovery, or preparation of registry changes.

Revocation is effective locally as soon as a local denial is recorded, and on other devices when they
receive and accept the signed update. Until then a disconnected device may still accept the revoked
key. This propagation delay is an explicit availability tradeoff and has no finite bound in v1. A
withholding remote can prolong it. Signatures prevent unauthorized changes, not hidden newer state.
A fetch timestamp is useful status information, not proof that all revocations have been disclosed.

Reports name the accepted checkpoint, connectivity/last successful check, and any pending changes or
update failures. Local denial always wins over that checkpoint. A newly learned revocation blocks new
uses of the key, without pretending it proves theft time or erases past signatures or existing copies.
Each future consumer adapter must state how it invalidates sessions or other cached authorization.

| Consumer | V1 behavior |
| --- | --- |
| Public inventory, diff, history verification | Works offline against an exact accepted checkpoint |
| Registry editing and signing | Works locally; preserves pending changes until publication can be confirmed |
| Existing registry-backed authority | Future adapters use last accepted state and apply received revocations; no mandatory online freshness gate |
| Local capture and recovery inspection | Independent of registry updates and private custody |
| Bootstrap or loss of accepted state | Explicit independent checkpoint confirmation; never trust a fetch automatically |
| Generated authentication/signing files | Inspection-only plans until the corresponding consumer adapter is implemented |

Invalid signatures, known rollback, and forks are not ordinary offline states. Reject the offered update
and retain evidence and the accepted checkpoint. Pause affected remote ingestion until repaired while
preserving local work and existing local access. Do not accept a known older state as a new checkpoint.
Accepted state is durable security data, not a rebuildable cache; loss or deliberate restoration of an
older backup requires independent confirmation. Normal process restart with intact state does not.

Live consumer adapters still need exact payload, resource-permission, and revocation-application tests.
They do not require expiring trust leases, a trusted clock, or an always-online authority. Stricter
freshness policies can be separate opt-in features later. Existing SSH credentials and release updater
policy remain independent until their explicit integrations ship.

### Projection constraints

A projection plan names its source registry/checkpoint, target consumer, supported restrictions, and
any unrepresentable restrictions. It fails if it would broaden authority. Examples:

| Target | Can represent | Needs an additional verifier |
| --- | --- | --- |
| OpenSSH allowed signers | Principal, key, SSHSIG namespace | Commit versus tag, repository scope, application of accepted revocations |
| OpenSSH authorized keys | Key and supported server-side key options | Application/vault capability grants and application of accepted revocations |
| Vulcan device policy | Exact registry-qualified subject/key bindings | Signed device payload and resource permission |

Do not translate ancestry-based validity into trusted signing times. V1 cannot install these plans.
A future installer must stage complete generations, atomically activate them, record the active source
checkpoint, and deny affected new access if accepted revocation cannot reach the consumer. A consumer
that cannot enforce these requirements is unsupported. Historical evidence remains inspectable.

## Custody, retirement, and recovery

Same-key migration uses `device-key-custody.md`: check dependent operations and their actual execution
contexts before switching, retain the old copy as inactive, and perform cleanup separately. Reusing a
migration journal must not target a new item created at the same logical location.

Removing a usage stops new managed use; retirement records routine withdrawal. Local disablement or
compromise denies local use immediately without waiting for registry publication. Remote withdrawal
requires an approved registry change, and other consumers learn it only through their trust-update
contract. No lifecycle label proves that external backups or signatures have disappeared.

Never delete public records or accepted history as private-copy cleanup. Last-managed-copy destruction
is separate from cleanup and deferred until a recovery format exists. It must require the complete key
ID, an impact plan, explicit non-interactive confirmation, and an honest statement that unmanaged copies
may remain. Losing every enrolled administrator key requires out-of-band recovery; losing a device key
does not grant that recovery authority.

## Optional synced secrets with age

If synced secrets are implemented (Roadmap 12.18), start with one native age X25519 recipient per
secret. Several secrets may use the same recipient. Its canonical public `age1...` string is the
encryption-key reference; it is not an SSH `vkey1_` ID. Generate a dedicated age identity rather than
converting or sharing a device, administrator, or release signing key.

Store each secret as a versioned record containing a stable secret ULID, its recipient, and a standard
age ciphertext. Encrypt the secret ID and value together and check the ID on read to catch accidental
record substitution. Use a maintained age implementation for encryption and complete authenticated
decryption; do not invent a cipher, reuse age's per-file randomness, or release partially verified values.
The record ID and recipient are public metadata; sensitive labels belong inside the ciphertext.

Sync these records as opaque canonical files. Private age identities and decrypted values stay outside
the synchronized tree, cache, search indexes, ordinary output, and logs. The owner distributes the private
age identity through a trusted out-of-band channel and imports it explicitly into local custody on each
device. Sharing is intentional; Vulcan does not distribute keys automatically. A protected local identity
file is a sufficient baseline, with optional `SecretStore` custody; SSH signing agents are not decryption
providers. Explicit key transfer must use protected input/output, never command-line secret arguments.

An imported identity enables offline decryption subject to local caller/resource permission. A missing
key leaves the record locked and still synchronizable; it never triggers replacement or plaintext
fallback. Sync does not decrypt records. Preserve conflicting versions for explicit resolution without
text-merging ciphertext. Encryption does not authenticate the author or make an old value current;
ordinary sync authorization and conflict handling still apply. Apps receive only explicitly authorized
secret access through opaque handles, never the shared decryption identity.

Possession of the private identity permits decryption of every accessible version addressed to it.
Local denial constrains Vulcan use but cannot revoke a copied key. Removing a user from a registry cannot
erase previously obtained keys, ciphertext, or plaintext. Excluding a former holder from future versions
requires a new encryption key; an exposed underlying password or API token must also change at its source.

No secret-access registry, automatic key distribution, per-user recipient management, or rotation
workflow is required for this first extension. Versioned records and explicit key references leave room
for those later. It depends on local custody and file sync, not public-registry enrollment or availability.
It does not automatically migrate existing credentials or encrypt Phase 17.4 Markdown secret callouts.

Before shipping, test age interoperability, offline import/read, shared-key and distinct-key records,
missing/wrong keys, malformed/truncated ciphertext, secret-ID mismatch, conflicts, interrupted writes,
and absence of plaintext/private keys from sync, caches, logs, and dry runs. Pin the record layout and
bounded parsing contract during implementation. Bundled skills change only when the feature ships.

## Planned command surface and delivery

These commands are design targets, not currently available commands:

```text
vulcan keys list|show|export-public
vulcan keys generate|import-public|import-private|discover|adopt|verify
vulcan keys usage add|remove
vulcan keys disable|retire|mark-compromised
vulcan keys custody status|migrate|cleanup
vulcan keys registry trust init|show|reset
vulcan keys registry verify
vulcan keys registry change prepare|review|sign|status|publish|discard
vulcan keys registry project --at <checkpoint> --target <consumer>
```

Mutations support `--dry-run`, structured JSON, exact IDs, and non-interactive operation where provider
capabilities permit. Applying `adopt` and signing through generic external agents require `--interactive`
and a TTY. Dry-run neither generates keys nor signs, unlocks providers, or installs projections. Trust reset
requires an independently confirmed exact checkpoint and expected old checkpoint, preserves old trust
and fork evidence, and never means "trust whatever the server currently returns."

Ship local inventory and permission checks first, then the offline registry verifier and signed-change
workflow. Native providers can ship independently after their platform tests; registry verification
needs public data only and must not depend on completing all platform providers. Runtime registry
consumers, external trust-file installation, generic SSHSIG, SSH CAs, hardware algorithms, encryption,
and delegated administration remain separate follow-ons.

## Acceptance scenarios

Implementation tests must exercise these outcomes, not just successful signatures:

| Scenario | Required result |
| --- | --- |
| Same key imported with different comments/labels | One immutable key ID; no provider overwrite |
| Private import after public-only import | Attach verified custody without changing the public record |
| Caller supplies another subject or disguises an approval as a possession test | Denied; trusted context and internally constructed payloads control signing |
| Device key enrolled as an administrator or release key | Rejected in v1 |
| Local allow conflicts with registry denial or missing resource grant | Denied; no registry union or local override |
| Two registries contain the same principal name | Distinct identities, no authority transfer |
| Agent confirms interactively or refuses a constrained challenge | No unattended probe; report unsupported/unknown capability honestly |
| Migration destination signs but cannot serve configured daemon/SSH use | Switch blocked; source retained |
| Crash, repeated migration, or stale cleanup journal | One active selector; cleanup cannot remove active/replaced/last durable copy |
| One existing administrator signs a valid change while all others are offline | Authorized; no other approvals required |
| New administrator/key signs its own addition; last active administrator is removed | Rejected by parent-state/resulting-state checks |
| Connection is lost for an extended period | Existing accepted permissions remain usable; signing needs only its local provider |
| Two administrators queue changes against the same base | Publish one; preserve and explicitly review/re-sign the other against the verified new base |
| Pending change grants access to a new key | No new authority until canonical publication is confirmed |
| A key is revoked while another device is offline | Local denial applies immediately; the other device enforces revocation when it accepts the update |
| Invalid intermediate change later reverted; merge; malformed signature framing | History rejected without advancing accepted state |
| Remote hides revocation | Last accepted authority continues under the explicit eventual-revocation policy; no false freshness claim |
| Remote serves rollback, invalid history, or an accepted-history fork | Reject update, pause affected remote ingestion, retain checkpoint and local access |
| Projection cannot preserve scope or an accepted revocation | No deployable output that broadens authority |
| Device key lost or cloned; all administrator keys lost | Explicit replacement or checkpoint recovery; vault bytes and old evidence retained |

Use one-owner and small-team fixtures with independent publisher/consumer clones and a trusted verifier.
Add byte-level signature vectors, bounds/redaction checks, exact-lease races, crash injection, downgrade,
and platform prompt tests before enabling the corresponding implementation. Design-only edits do not
advertise unimplemented commands in bundled agent skills.

## References

- [SSH agent protocol, RFC 9987](https://www.rfc-editor.org/rfc/rfc9987.html): enumeration and signing do
  not provide a standard no-prompt contract or complete per-key constraint introspection.
- [OpenSSH agent implementation](https://github.com/openssh/openssh-portable/blob/master/ssh-agent.c):
  destination-constrained authentication keys cannot be tested with arbitrary signing payloads.
- [OpenSSH allowed signers](https://man.openbsd.org/ssh-keygen#ALLOWED_SIGNERS): projections have a
  narrower policy vocabulary than Vulcan's typed usages.
- [Git signature format](https://git-scm.com/docs/gitformat-signature): standard SSH commit signatures
  carry the signed changes; Vulcan adds registry-specific authorization and schema checks.
- [age format](https://age-encryption.org/v1): the optional synced-secrets extension uses native X25519
  recipients and standard authenticated file encryption through an existing implementation.
