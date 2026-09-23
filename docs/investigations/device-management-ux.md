# Device management UX and security model review

Status: analysis and recommendations, not an accepted implementation contract.

## Scope and evidence

This review covers the current `sync devices` CLI, the planned `device` identity and custody
commands, the planned key manager and accepted registry, and the user journeys that connect them.
It does not treat a remote Git ref, a friendly name, or possession of a public key as proof of
authorization. The main sources are `docs/specs/device-identity.md`,
`docs/specs/device-key-custody.md`, `docs/specs/key-management.md`, Roadmap 12.4 and 12.15–12.17,
`vulcan-app/src/sync_devices.rs`, `vulcan-app/src/sync_state.rs`, `vulcan-app/src/sync.rs`,
`vulcan-sync/src/sync.rs`, `vulcan-sync/src/refs.rs`, and the `vulcan sync devices` CLI.

## Executive assessment

The security design has the right separations: device identity, private-key custody, permitted
usage, accepted authority, and preserved vault bytes are distinct. The current user surface does
not yet make those separations easy to understand. It primarily exposes per-vault recovery heads;
the first-class local identity, custody, fleet, replacement, and registry workflows remain planned.
The next UX work should present one coherent device story while retaining separate operations and
evidence for each layer.

The highest-risk UX mistake would be a single “trusted/active device” badge derived from a name,
ref, key-derived ID, or successful local key proof. Those facts answer different questions. A
remote safety ref is an unauthenticated observation; a key-derived ID proves only a deterministic
public-key name; private-key custody proves local control at the time of a challenge; accepted
registry history and resource grants establish scoped authority. Even accepted revocation may not
have reached an offline device.

## What exists today

| Surface | Current behavior | Important limit |
| --- | --- | --- |
| Local sync ID | First mutating sync creates a device-local random ULID in `_device.json`; read-only doctor does not create one. | No cryptographic device key, provider, or first-class local `device show` command yet. |
| `sync devices list` | Queries one remote/profile; shows safety heads, local recovery freshness, retained recovery refs, and names without backups. | A remote failure prevents the command from showing even local-only evidence; “current” means the fetched ref matches one observed remote OID, not that its contents are integrated. |
| Names | `set-name`/`clear-name` edit display-only JSON under the selected vault's `.vulcan/device-names/`. | The installation identity is global but labels are per vault, writable as ordinary shared vault data, and synced only when tracked by Git. A label can exist without a device or backup. |
| Recovery | `fetch` anchors the selected head and live in local refs; `remove` requires matching fetched refs, checks integration, and uses an exact lease. | `remove` deletes one remote backup ref. It does not retire a device, revoke Git/registry access, delete a key, or establish that the machine is gone. |
| Diagnostics | `sync doctor` reports the ULID as a stable device identity or notes that the first mutation will create it. | It cannot yet distinguish key identity validity, provider availability, unattended usability, and authorization. |

The current `vulcan sync devices` help describes “remote per-device safety backups,” which is
accurate, but its `remove` verb and device-oriented naming can still be read as fleet
deprovisioning. The human output should explicitly say “backup ref removed; access unchanged”
when key-backed access arrives. An alias may preserve the existing command while a clearer
`sync backups` surface takes over recovery operations.

## Model the user actually needs

One row in a fleet inventory should be a *projection*, not a mutable “device object” that owns
all state. Keep these records and authorities separate:

| Dimension | Identity and source | What the UI may conclude |
| --- | --- | --- |
| Installation identity | Current local public manifest; later `vdev1_` derived from exact SSH public-key bytes. Legacy ULIDs remain historical identifiers. | Exact ID and kind; local manifest validity. Same private key on two installations is the same identity, not two devices. |
| Human label | Local preference or vault-authored shared metadata with visible provenance. | A convenient name only. Duplicate names are allowed; renaming never changes ID, authority, or history. |
| Key custody | One selected provider plus secret-free local operation journal. | Available/locked/prompt-required/unavailable/invalid and capability for the *specific* requested context. Interactive success does not imply daemon suitability. |
| Usage and authority | Local denial plus an exact accepted registry checkpoint, scoped binding, and resource capability. | Which operation is allowed at which checkpoint and what is pending or unavailable. A fetched registry tip is not accepted authority. |
| Vault participation | Per-vault remote safety ref, local recovery ref, and local sync journal. | Observed backup/recovery state for that vault and remote. No proof of ownership or online presence. |
| Transport access | Explicit Git/SSH adapter and the remote provider's own access policy. | Whether this configured transport can authenticate now. Registry enrollment does not silently configure a forge or SSH client. |

Avoid a single lifecycle enum. Present separate fields such as `identity_state`,
`custody_state`, `authority_state`, `transport_state`, and `backup_state`, each with its source,
scope, last observation, and any uncertainty. Reserve “verified” for a named verification
procedure and exact evidence. Do not infer “online” from a Git commit timestamp or a ref's
presence; timestamps and remote ads are not liveness proofs.

The UI should consistently distinguish: **this installation**, **known identity**, **observed
remote ref**, **retained recovery copy**, **accepted binding**, and **locally denied key**.
Unknown is a first-class state, not a green check or an empty field.

### Security and failure boundaries for the UI

| Situation | Safe presentation and behavior |
| --- | --- |
| Vault writer changes a shared label | Show it as a vault-authored alias, never as verified ownership or an exact selector for a privileged action. |
| Remote writer advertises a device ref or key-shaped ID | Treat it as observed, unauthenticated recovery data until a separate signed statement and accepted binding are verified. Preserve the bytes for review. |
| Private key is copied | Both copies claim one device ID. Warn on divergent use; do not invent a new identity or automatic continuity. |
| Configured provider is locked or unavailable | Keep public-only inventory, capture, and recovery available; block only operations that actually require private-key use. Do not silently select a fallback. |
| Registry remote withholds a revocation | Show the exact last accepted checkpoint and unknown freshness. Do not claim global revocation or silently accept an unverified fetched tip. |
| Local OS account is compromised | State the protected-file provider's real boundary: the account can read an unencrypted file key. A green custody badge is not a defense against that account. |

Status, doctor, inventory, and dry-run must remain read-only and non-prompting. Mutations use exact
IDs, typed plans, explicit permissions, non-interactive failure on unknown prompt capability,
atomic durable state, and readback where external state changes. Never print private material or
unsafe provider locators, including in JSON, errors, logs, or crash diagnostics.

## User journeys and recommended UX

### See my installation and the fleet

`vulcan device show` should work without a vault, remote, daemon, prompt, or state mutation. It
should show exact ID/kind, a short display fingerprint, label/source, manifest health, provider
state, unattended suitability for configured uses, and one next action. `vulcan devices list`
should project known identities across registered vaults, with an optional per-vault view. The
inventory must show local facts while offline and attach an observation error to remote fields;
it must not discard local recovery evidence because `ls-remote` failed. A stored observation is
explicitly dated and never called current authority.

In a terminal, lead with label and state, put full IDs and refs in a detail block, and show the
source of any trust or label. In JSON, retain complete IDs, exact OIDs, enum states, timestamps,
and source/checkpoint fields. Use a separate detail command rather than truncating IDs in
machine output. Do not let a global `--limit` or `--fields` option appear to filter a command
unless that command actually implements it.

### Add another installation

After cloning or registering a vault, explain that a new installation gets a new identity. Show
the new ID before any enrollment request and never suggest copying the old device private key.
Offer an optional display label. If Git transport is already configured, report that separately.
If a registry is configured, prepare an enrollment request that an existing administrator can
review against the exact ID, public key, scope, and permissions. “Request prepared,” “published,”
and “accepted at checkpoint X” are distinct outcomes. No name or remote ref grants access.

### Move custody or replace a lost key

The primary prompt should ask whether the user is moving the *same key* or replacing it with a
*new identity*. Same-key migration lists dependent daemon/SSH/signing contexts, unknown checks,
the source copy to be retained, and the exact post-switch proof. It never silently falls back or
deletes the source. Replacement must work without the old private key, capture local bytes first,
retain the old public identity and recovery refs, then list each known binding that needs
re-enrollment or revocation. It must not imply continuity or transfer of authority.

### Recover, retire, or respond to compromise

Recovery starts from a named vault and observed backup: fetch into durable local refs, compare
against accepted live, resolve any device-only work, then consider pruning the backup. The
inventory should report the removal preconditions, but the destructive command must recheck them
against exact OIDs and a lease. The action should be worded **prune remote backup**, not
**remove device**. The retained local recovery copy stays visible.

Retirement is a separate checklist of authority, transport, scheduling, and data obligations.
Disable managed local usage immediately when requested; publish an administrator-authorized
registry withdrawal; show its accepted checkpoint and propagation uncertainty; remove or rotate
external transport access through its own adapter; and only then offer optional recovery-head
pruning. Compromise must not wait on remote availability to apply local denial. Offline peers may
continue using their last accepted checkpoint until they receive a valid update; the UI must say
so rather than promising immediate global revocation.

## Concrete shortcomings and priorities

| Priority | Gap | Recommended next work |
| --- | --- | --- |
| P0 | The implemented ref namespace constant is already `2`, while Roadmap 12.15 and the identity spec describe introducing namespace version 2 for key-derived IDs. `GitSyncDeviceId::parse` still accepts only a 26-character ULID and lowercases input. | Reconcile the protocol contract *before* the dual-reader/key-writer rollout. Either assign a new capability/version signal or revise the staged-version plan; test old readers, mixed IDs, trailers, and fail-closed behavior. Reject noncanonical key-ID spellings. Do not reuse an already emitted version as a migration signal. |
| P0 | The UI can conflate recovery-head deletion with device deprovisioning. | Rename the primary recovery action to backup pruning, keep the old command as a compatibility alias, and make access/revocation status explicit in its plan and completion report. |
| P0 | Key-backed access could strand a lost-key device if replacement and independent re-enrollment are missing. | Make the replacement/re-enrollment journey a release gate before any device-key-backed authorization becomes required, as the accepted design already requires. |
| P1 | No local-first global device overview; remote failure hides current local recovery inventory. | Build a read-only projection from the local manifest, local recovery refs, labels, registered vaults, and optional remote observations; report source and freshness per field. |
| P1 | Labels are per-vault shared files while identity is installation-global. A shared label is untrusted presentation data, and `.vulcan/` may be ignored. | Decide between local preferred label and explicit shared vault alias, show provenance, handle collisions, and offer a deliberate publish/track path. Never use a label for authorization or exact selection. |
| P1 | Provider state and unattended capability are planned but not yet shown in one device detail view. | Ship state-free `device show` and context-specific `device key status` before migration and signing flows. Use actionable degraded/invalid states without triggering unlocks. |
| P1 | Accepted registry status, local denial, pending changes, and remote observation can look like one “trusted” state. | Display the exact accepted checkpoint and scope, plus pending/local-denial/offline-update states separately. Keep transport authorization separate. |
| P2 | Names and OIDs provide no trustworthy “last online” or device ownership evidence. | Offer “last observed by this installation at …” only as local telemetry, or add a separately specified signed heartbeat later. Never infer liveness from commit author/date. |

## Implementation order and acceptance gates

1. Fix the namespace/version contract and specify a typed inventory projection with source,
   scope, and freshness. Make offline local inventory available before adding remote controls.
2. Implement the state-free local `device show/init/public-key` slice and the staged legacy/key
   reader migration. Show `legacy_ulid` versus `ssh_key_v1` without a trust badge.
3. Add custody status and same-key migration behind capability checks. Test daemon/SSH contexts,
   prompt suppression, retained-source repair, redaction, and crash recovery.
4. Add public key inventory and accepted registry history. Build enrollment, local denial,
   revocation, replacement, and explicit re-enrollment flows before requiring key-backed access.
5. Add any WebUI only over the same typed plans and reports as the direct CLI; no UI-only authority
   path, hidden side effect, private-key rendering, or silent provider fallback.

Release tests should cover a new installation, two independent devices with the same name,
unavailable remote with retained local recovery, stale and divergent backup refs, copied key,
lost private key, locked provider in unattended sync, failed custody migration, offline
revocation propagation, exact-lease races, mixed-version readers, and accessibility/non-TTY
command use. Test the *wording* of high-risk status/action reports as well as their JSON fields.

## Decisions to settle before implementation

1. Should `vulcan devices list` include only locally observed identities, or also every identity
   in an accepted registry? The UI can show both, but must mark the source and avoid silently
   treating registry members as synchronized vault participants.
2. Which label is primary when local and shared aliases differ? A local display preference is
   useful, but its source must remain visible and shared label edits cannot imply verification.
3. What exact capability/version identifies key-ID-aware sync readers, given that namespace
   version 2 is already emitted by the current ULID writer?
4. Which external Git/SSH/forge bindings can Vulcan inspect or revoke directly, and which must
   remain an explicit operator checklist with “unknown” status?
