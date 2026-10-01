# Device-Key Git Transport, Registration, and Forge Deploy Keys

Status: proposed design for Roadmap 12.21. Slice 12.21.1 (transport binding), the registration
records of 12.21.2 (self-registration, placeholders, revoke, unregister, listing), and the Forgejo
adapter with `forge sync` of 12.21.3 are implemented; the fleet view is not. Two Forgejo questions
remain open for a real run: a key already registered as a user SSH key, and pushing the hidden
`__vulcan-sync` refs under branch protection. This design is independent of the key-management registry in `key-management.md`
(12.17). It builds on `device-identity.md` and `device-key-custody.md` and resolves their deferred
"explicit Git/SSH transport adapter" item.

## Goal

Let a Vulcan installation authenticate its Git sync with its own device key, and let a vault
administrator install device keys as forge deploy keys with little friction:

1. A device-local **transport binding** makes Vulcan's Git engine (direct CLI and daemon) use the
   device key. Optionally it also sets `core.sshCommand` for plain `git`.
2. Each device has a **registration** in the vault's Git remote, published automatically on its first
   mutating sync. An administrator can also create a **placeholder** for a device that has not synced
   yet; the device replaces it on its first sync.
3. A **forge sync** command makes the repository's deploy keys match the registrations. Forgejo ships
   first.
4. **Revocation** marks a registration revoked, and the next forge sync removes its key.
5. A read-only **fleet view** shows a device's state across the vaults it knows.

## Trust model

**The registration list in Git is trusted as written.** This is a deliberate, temporary tradeoff.
Anyone who can push to the repository can add, change, or revoke registrations, and therefore change
which keys the next forge sync installs. A compromised or about-to-be-revoked device could register
extra keys for itself before its own key is removed. The accepted mitigations for now:

- `forge sync --dry-run` shows the full plan, and `devices list` shows every registration, so an
  administrator reviews before applying.
- Forge sync runs only when an administrator runs it with forge credentials; devices never trigger it.
- Only keys with the Vulcan marker are ever added, changed, or removed.

The proper fix is a cryptographic registry that signs administrator decisions (the 12.17 track or its
successor). That, and a deploy-key-management CI job built on it, replace this trust-the-list step
later without changing the registration or adapter shapes. Signing device records now would not close
the gap, because any device is a valid signer.

**Each vault is its own trust plane.** Registration, placeholders, revocation, and forge sync are per
vault. The only cross-vault fact is that a device presents the same key everywhere, observable to
anyone who sees more than one repository's key list. "Managing devices across vaults" means iterating
vaults with a separate result for each.

**The forge decides access.** Only someone with repository administration on the forge can change
deploy keys, and the forge decides whether a key may push. Vulcan keeps no authorization state of its
own beyond the registrations. Revocation takes effect at the forge the moment forge sync removes the
key.

**Administrator keys are not part of this design.** An administrator's authority is their forge
token. They may use any SSH key, including a GPG-agent SSH key, for ordinary Git access; Vulcan does
not store or use it.

## 1. Transport binding (device-local) — implemented

A binding says: "for this vault, Vulcan authenticates Git-over-SSH with the active device key." It is
device-local state (`git-transport.json` under the user state directory, keyed by vault, beside the
sync journals). It is deliberately **not** stored under the vault's `.vulcan/`: plain Git vaults
replicate their whole work tree, so a file there would reach other devices, and a synced binding
would make them fail closed. It is never in the cache and holds only the bound device ID, not a key
path. Both
the direct CLI and the daemon resolve it per vault, so one daemon can serve bound and unbound wikis.

```text
vulcan sync transport bind   [--wiki <name>] [--remote origin] [--git-config] [--dry-run]
vulcan sync transport status [--wiki <name>]
vulcan sync transport unbind [--wiki <name>] [--dry-run]
```

- Optional and per vault. Without a binding Git uses the user's ambient SSH setup, as before. A
  binding never changes global Git or SSH configuration.
- The sync remote must be SSH (`ssh://` or scp-like); HTTPS is rejected.
- Only providers that serve an unattended non-interactive transport are eligible, initially
  `file_v1`. Others are rejected with a diagnostic. There is no fallback to another key or provider.
- `bind` never initializes the identity; an uninitialized identity fails with `vulcan device init`.

### Command construction

One shared builder produces the SSH options for every Vulcan-spawned Git operation, daemon included:

```text
ssh -i <key> -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes
```

`IdentitiesOnly` and `IdentityAgent=none` stop ambient agents and `IdentityFile` entries from
authenticating as another principal. Host key checking is never weakened; the user's `known_hosts`
and `~/.ssh/config` host aliases still apply. The engine sets `GIT_SSH_COMMAND` (and clears
`GIT_SSH`) on its own Git processes, so the daemon does not depend on repository configuration. The
key path never appears in logs, JSON, or notifications.

### Optional `core.sshCommand`

`--git-config` also writes the repository-local `core.sshCommand` so interactive `git` works. It is
off by default because users may prefer their main key for direct use.

- The value calls a Vulcan wrapper (`vulcan device ssh-command`) rather than embedding the key path,
  so custody migration need not rewrite Git configuration.
- The value is recognized as Vulcan-owned by its wrapper suffix. `bind` writes only if the setting is
  absent or already owned and refuses to overwrite a foreign value. `unbind` removes only an owned
  value. `sync doctor` warns on drift (value missing or replaced).

### Failure behavior

If the bound key is unusable (missing, mismatched after replacement, locked), the engine is given a
transport that fails every SSH connection with a clear message. Sync never retries through another
credential, local capture still runs, and doctor reports an error check. A binding to a device ID that
no longer matches the active key must be re-bound.

## 2. Device registration

A registration is a small public record per device, stored in the vault's Git remote next to the
existing per-device backup heads and independent of them.

```text
refs/heads/__vulcan-sync/registrations/<device-id>
```

The ref lives in the versioned namespace owned by `vulcan-sync`, with builders there rather than at
call sites. One ref per device means concurrent registrations never conflict and never touch the live
branch. The tree holds one bounded, strictly versioned `registration.json`:

```text
version, device_id, public_key (canonical ssh-ed25519), label?, status, created_at, claimed_at?
status: placeholder | registered | revoked
```

Readers reject a record whose device ID is not derived from its public key, any unknown field,
oversize content, an unsupported key type, a ref that names a different device, or a tree that is not
exactly `registration.json`. Writers refuse illegal status transitions (a reader cannot know who
pushed a record): administrators create placeholders and revoke; devices create `registered` records
and claim placeholders; `revoked` is sticky until an administrator unregisters. Rejected records are
ignored with a diagnostic and never affect sync. Labels are sanitized and length-limited, and are
never shown without the device ID.

### Self-registration

The first mutating sync publishes the device's own record when none exists. It is best effort: a
registration failure never fails or delays sync, and doctor, dry-run, and status never create one.
The push is create-only with an exact lease. The decision costs no extra remote round trip: the
sync's own remote observation already lists `refs/heads/__vulcan-sync/registrations/*` in the same
`ls-remote` (cheap sync) or `fetch` (full sync) that reads the live ref. A full sync also mirrors those
refs into `refs/vulcan/registrations/*` and prunes vanished ones. The device compares its own tip with
its local mirror and fetches its record only when the tip moved, so it sees a revocation or deletion
on the very next sync. It pushes only to create a missing record or claim a placeholder. Mirroring is
best effort: if the combined fetch fails, the plain live fetch is retried and the registration step
falls back to the last mirrored copy (or one explicit lookup if the device has never seen its record),
so this namespace can never fail a sync. Afterward the device only updates its own record, to
claim a placeholder or change its label.

A device that finds its own record `revoked` never overwrites it. It reports the state in doctor and
status; if it still authenticates, sync continues as for any other device.

### Placeholders

```text
vulcan sync devices register --public-key <file> [--label <text>] [--dry-run]
```

Creates a `placeholder` record for a device that has not synced yet. The administrator takes the
public key from `vulcan device public-key` on the device and checks the displayed device ID against
`vulcan device show`. The key is mandatory: a keyless placeholder could not become a deploy key. On
its first successful sync the device replaces the placeholder with a `registered` record. The device
ID is derived from the key, so a device with a different key cannot claim it; it simply registers
separately. Registration reaches the remote with whatever credentials the administrator already has.

### Listing and revoking

```text
vulcan sync devices list     [--wiki <name>]
vulcan sync devices revoke   [--wiki <name>] <device-id> [--dry-run]
vulcan sync devices unregister [--wiki <name>] <device-id> [--dry-run]
```

- `devices list` extends the existing per-device inventory: registration status, public key
  fingerprint, backup head, and (when configured) forge key state, each labeled with its source.
- `revoke` sets the record's status to `revoked`, keeping the tombstone so an honest device does not
  re-register. The next forge sync removes that device's deploy key.
- `unregister` deletes a record entirely, for a stale placeholder or a device that will never
  return. It does not remove any forge key; run forge sync and review the orphan report.
- Existing backup-head commands (`fetch`, `prune-backup`) keep their meaning and are unaffected.

## 3. Forge adapters and forge sync

Adapter configuration is **device-local, per vault**, in operational state like the binding: forge
kind, API base URL, `owner/repo`, and the *name* of the environment variable (or secret reference)
holding the API token. It is never read from vault files or synced configuration, because a synced
value could redirect an administrator's token to another host. Token values are never persisted or
reported. Network use is gated by the vault's network permission.

```text
vulcan sync forge set    [--wiki <name>] --kind forgejo --url <base> --repo <owner/repo> --token-env <VAR>
vulcan sync forge show   [--wiki <name>]
vulcan sync forge clear  [--wiki <name>]
vulcan sync forge sync   [--wiki <name>] [--dry-run]
```

A synchronous `ForgeDeployKeyAdapter` trait in `vulcan-app`, with all forge specifics inside
implementations:

```text
list_deploy_keys(repo)               -> [{ id, title, key, read_only }]
add_deploy_key(repo, key, title)     // always read_only = false
remove_deploy_key(repo, id)
```

### Reconciliation

`forge sync` fetches the registrations, reads the forge's deploy keys, and makes them agree:

| Registration status | Forge key | Action |
| --- | --- | --- |
| `placeholder` or `registered` | missing | add |
| `placeholder` or `registered` | present | none |
| `revoked` | present | remove |
| `revoked` | missing | none |
| none | Vulcan-marked key present | report as orphan; do not remove |
| any | foreign key (no Vulcan marker) | untouched |

- Keys are titled `vulcan-device:<device-id>` plus a sanitized label, always write-capable (sync pushes
  a per-device safety ref, so a read-only key cannot synchronize).
- Matching is by public key, not title, so retitled keys are still recognized.
- Removal comes only from an explicit `revoked` record, never from absence. A failed or partial fetch,
  or an empty list, therefore cannot delete anything. Orphans are reported for the administrator to
  revoke or unregister deliberately.
- A managed key that exists but is read-only is removed and re-added as write-capable; the old key
  must go first because the same public key cannot be added twice. A failure between the two steps is
  reported, and re-running converges.
- `--dry-run` is mutation-free and prints every add, remove, orphan, and conflict. JSON output matches.
- Forge errors are reported per key, with the forge's own message, and never change registrations or
  sync state.

### Forgejo (first adapter)

Repository keys API: `GET`/`POST /api/v1/repos/{owner}/{repo}/keys` and
`DELETE /api/v1/repos/{owner}/{repo}/keys/{id}`, body `{ key, title, read_only }`. The token needs
write/administration scope on that repository only.

Open compatibility questions, to settle by testing against the deployed Forgejo, not by assumption:

- **Same key as a deploy key on several repositories.** Verified on a deployed Forgejo: one key can be
  a deploy key on several repositories, so one device key can serve several vaults there. GitHub does
  not allow it, so a future GitHub adapter must surface that conflict (per-device keys are out of
  scope because a device is one key).
- Whether a key already registered as a user SSH key can also be a deploy key. Expectation: no, so
  the device key must never be uploaded as a user key.
- Whether deploy keys can push `refs/heads/__vulcan-sync/**` under branch protection, which is the
  existing Forgejo hidden-ref conformance gate in `git-sync-architecture.md`.

Other forges (GitHub, GitLab, plain `authorized_keys`) implement the same trait later. Several of
them forbid one key as a deploy key on many repositories; the adapter must surface that clearly.

## 4. Fleet view

`vulcan device show` is vault-independent. Extend the installation inventory with a per-vault
projection of transport state (not bound, usable, key unavailable) and this device's registration
status, kept separate from backup and recovery state. For administrators, `devices list --all-wikis`
and `forge sync --all-wikis` iterate configured vaults with independent forge credentials and results;
one vault's failure never affects another. Neither view treats one vault's registrations as meaning
anything in another, and offline mode contacts no remote.

## Replacement and retirement

The key-management gate that requires a replacement workflow before key-backed access becomes
*required* does not apply: a binding is optional per vault, the user's other SSH access remains, and
an administrator can always revoke the registration and run forge sync (or delete the key at the forge) without the lost private key. Replacing a key is: initialize a new identity, let it register, revoke the old registration. A first-class
`device replace` workflow (capture bytes, new key, list bound vaults) remains a worthwhile follow-on
tracked under 12.15.5, but this feature does not depend on it.

## Extension points (not built)

- **Cryptographic registry.** Replaces trust-the-list with signed administrator decisions, and makes a
  deploy-key-management CI job possible. Registration records and the adapter trait should survive
  unchanged; only the source of the desired set changes.
- **Registration signing.** Signing a device's own record would make it tamper-evident against other
  devices. It needs a typed signing operation on the device key and does not close the trust gap
  alone, so it is deferred.
- **Multiple users.** The forge's collaborator model scopes who may run forge sync today. A richer
  model belongs with whatever registry design follows.

## Non-goals

- A Vulcan-run authorization registry, admin signing keys, or signed roster in this feature.
- Cross-vault trust, a shared roster, or installation-wide approval.
- Per-vault device keys.
- Replacing the user's main Git SSH identity or changing global Git/SSH configuration.
- Installing a key on a forge from anything but an explicit administrator command.
- User-level forge SSH keys; only repository deploy keys.

## Delivery order

1. **Transport binding** — done.
2. **Registration records**: ref contract, strict reader, self-registration on first mutating sync,
   `devices register` placeholders, claim, `revoke`, `unregister`, and the extended `devices list`.
   Useful on its own as an inventory before any forge exists.
3. **Forge adapter and `forge sync`**, with device-local configuration and Forgejo. Verify the
   Forgejo open questions first.
4. **Fleet view** and doctor integration.

## Tests

- Binding (done): argv construction, agent/config suppression, HTTPS and provider rejection, no
  implicit init, key-path redaction, dry runs, `core.sshCommand` ownership and drift, fail-closed
  engine, CLI JSON round trip.
- Registration: ID-versus-key mismatch, unknown fields and oversize input, illegal status transitions,
  create-only leased self-registration, concurrent first syncs from many devices, placeholder claim,
  revoked tombstone not overwritten, registration failure not failing sync, doctor and dry-run never
  creating records, label sanitization.
- Adapter: recorded-API contract tests for Forgejo; fake-forge tests for every row of the
  reconciliation table, marker-only management, match-by-key, idempotent resume after partial failure,
  empty or failed fetch deleting nothing, and per-key conflict reporting. A real Forgejo run covers
  the open questions above; mock-only evidence does not close them.
- Config safety: adapter settings and tokens never read from vault files, token values never in output,
  network permission denial.
- Fleet: per-vault isolation, partial failure, offline mode without remote contact.
