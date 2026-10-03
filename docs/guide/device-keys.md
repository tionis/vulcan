# Device keys: authenticating Git over SSH

Every Vulcan installation has one **device key**: an Ed25519 key that identifies this machine to
the Git remotes of your vaults. Vulcan can use it for Git-over-SSH, authorize it with your forge,
bind each vault to it, and retire or replace it later. For an SSH remote this is the default, and
the normal flow needs no setup beyond running the command you already run.

This guide covers what happens by default, how to set up a forge once, how to handle existing
vaults, and how to rotate or retire a key. Everything here is non-interactive except an optional
browser login, and every command previews with `--dry-run`.

## In short

```sh
vulcan vault clone git@forge.example.com:me/wiki.git ~/wiki     # clones, enrolls the key
vulcan vault add notes ~/notes                                   # same enrollment for an existing vault
vulcan sync run                                                  # syncs; no extra auth setup
```

If something is missing, the command still succeeds and says exactly what to do next
(`pending`). Re-running `vulcan vault enroll <id>` continues from where it stopped.

## What the device key is, and is not

- **One key per installation**, created automatically the first time it is needed (or with
  `vulcan device init`). It lives in your user data directory with `0600` permissions and is never
  stored in a vault, so syncing never copies it.
- **Its ID is stable across vaults**: `vdev1_…`, derived from the public key. Inspect it with
  `vulcan device show`; print the public key with `vulcan device public-key`.
- **Each vault is its own trust plane.** Having the key proves *which machine* is acting; the
  remote still decides whether that machine may read or write. The device ID is not an access
  decision, and a registration in one vault says nothing about another.
- **Vulcan never falls back silently.** If a vault is bound to the device key and the key is
  unavailable, Git fails with a clear message instead of trying other credentials.

## What happens when you clone or add a vault

For an SSH remote, `vault clone`, `sync clone`, and `vault add` enroll the key:

1. **Policy.** The transport policy is `device-key` by default. HTTPS remotes, local paths, and
   `ambient` hosts skip enrollment and behave exactly as before.
2. **Identity.** The device key is created if this is the first time.
3. **Probe.** One `git ls-remote` with the device key alone asks the remote whether it accepts it.
4. **Authorize, if needed.** If the remote refuses the key and this machine can act on the forge
   (an OAuth login or an API token), Vulcan adds the key as a deploy key through the forge API and
   probes again.
5. **Bind.** Only after the remote has accepted the key does Vulcan bind the vault to it, so a
   rejected key can never break plain `git` in that vault. Binding also sets a repository-local
   `core.sshCommand`, so plain `git` in the vault uses the same key.
6. **Register.** Your device is recorded in the vault's remote, so an administrator can see it.

During a clone, steps 3 and 4 run *before* the clone so the clone itself uses the device key when
it can. If neither works, the clone uses your own SSH setup (agent, config) exactly as it always
did, and the vault ends as `pending`.

### The three ways you arrive

| You have | What happens |
| --- | --- |
| An SSH agent or key that already reaches the repository | The clone works as always; enrollment then tries to authorize the device key, or reports `pending` with the next step. |
| The device key already added as a deploy key | The probe is accepted, and the clone, bind, and registration all use it. |
| A forge login (OAuth) or API token | Vulcan authorizes the device key through the forge, then clones with it. |

### Reading the result

`vault clone`, `vault add`, and `vault enroll` print one line per step:

- **done**: changed now. **already**: nothing to do. **skipped**: policy or remote kind says it
  does not apply. **pending**: waiting on something outside this machine's authority.
- **failed** is reported but never fails the clone or add.

A `pending` result ends with the exact commands to run. Typical ones:

- *No forge login or token*: run `vulcan sync forge login`, or add the public key
  (`vulcan device public-key`) as a deploy key in the forge's repository settings, then
  `vulcan vault enroll <id>`.
- *Someone else administers the repo*: send them the public key; they run
  `vulcan sync devices register --public-key <file>` and `vulcan sync forge sync`.
- *Network down*: re-run when you are back online.

## Setting up a forge (once per host)

Vulcan ships no forge host and no OAuth client. You tell this device about your forge once, in
the device-local `device.toml`:

```sh
vulcan device config set-forge forge.example.com \
  --kind forgejo \
  --oauth-client-id <public client id of your Vulcan OAuth app>
```

Then log in whenever it asks (the browser opens; Vulcan listens on `127.0.0.1` for the redirect and
stores a refresh token in a private file, never in a vault):

```sh
vulcan sync forge login --wiki <id>
```

Alternatives to OAuth: `--token-env FORGE_TOKEN` names an environment variable that holds an API
token (the token itself is never written to disk). A forge entry's `--login auto` (the default) lets
`vault enroll` start a login on its own only when attached to a terminal, and `--login never`
forbids it; `vault enroll --login` forces one for that run. Never pass `--login` in unattended scripts.

For one vault, `vulcan sync forge init` derives the forge host and `owner/name` from the Git
remote, so you only name the kind and credential. `--publish` shares the non-secret settings
through the remote so other machines can see them; they are only a proposal, and each machine
saves them with `--adopt` after reading them.

## Existing vaults

Vaults added before you had a device key, or ones that finished as `pending`, are enrolled with:

```sh
vulcan vault enroll <id>          # one vault
vulcan vault enroll --all-wikis   # every registered Git vault, each with its own result
vulcan vault enroll <id> --dry-run
```

It is idempotent, never binds a key the remote has not accepted, and stops as `pending` when
authority is missing.

If the device key is already authorized everywhere (for example you added it as a deploy key by
hand) and you only want to switch the vaults over, `vulcan sync transport bind --all-wikis` binds
every registered Git vault whose remote already accepts the key. It probes each vault first, skips
(with the reason and the fix) any vault the remote refuses, an `ambient` host, or a non-SSH remote,
and reports each vault on its own; `--dry-run` previews it. It authorizes nothing and registers
nothing; use `vault enroll` for that. `vulcan sync doctor` points at enrollment when a vault is not bound.

## Staying on your own SSH setup

If you prefer ssh-agent, a credential helper, or a forge that cannot use one key on several
repositories (GitHub deploy keys are one-repo-only), choose `ambient`:

```sh
vulcan device config set-transport ambient                              # everywhere
vulcan device config set-forge github.com --transport ambient           # one host
vulcan vault add notes ~/notes --no-device-key                          # one command
vulcan sync transport unbind                                            # undo for an enrolled vault
```

Unbinding removes only Vulcan's own `core.sshCommand`; another tool's setting is never touched.

## Seeing the state

- `vulcan device show`: this installation's identity and status.
- `vulcan devices list`: per vault, whether it is bound, and whether this device is registered.
- `vulcan sync transport status`: one vault's binding and whether plain `git` is configured.
- `vulcan sync devices list`: every device registered in a vault's remote, and whether each record is signed by its device.
- `vulcan sync doctor`: reports binding problems and suggests the fix.

## Adding another machine

Install Vulcan on the new machine and clone or add the vault: it creates its own key and enrolls
it as above. If that machine cannot authorize itself, an administrator registers its public key
once and reconciles the forge:

```sh
vulcan sync devices register --public-key new-machine.pub --label "Laptop"
vulcan sync forge sync --dry-run      # review what would be added
vulcan sync forge sync
```

A registration the administrator creates is a placeholder and is unsigned; on its first sync the
device claims it and signs it with its own key. From then on every record a device writes is signed,
so nobody else can claim a placeholder or forge a registration for it. (A signature proves the
device holds the key, not that anyone approved it: approval is still your `sync forge sync`.)

`sync forge sync` only adds keys for registered devices and removes only a key whose registration
was explicitly revoked. Keys without Vulcan's `vulcan-device:` title marker are never touched, and
nothing changes if the registration list cannot be read.

## Replacing this machine's key

Rotate the key without breaking any vault:

```sh
vulcan device replace --dry-run    # shows which vaults are affected
vulcan device replace              # stage, authorize, prove, activate, rebind
```

Vulcan stages a new key beside the current one, authorizes and proves it in every vault bound to
the old key, and only then makes it the active identity and rebinds each vault. If any vault
cannot be proven yet (for example, no forge login), **nothing changes** and the pending steps are
listed; fix them and run the command again, and it reuses the staged key. Details:

- `--activate-anyway` switches now; the unproven vaults fail closed until you run
  `vulcan vault enroll <id>` for them.
- The old key is archived under the device state directory, never deleted. Its recovery backups
  stay on the remote.
- The old device stays registered, and its deploy key stays, until you retire it. Pass
  `--revoke-old` to do so as part of the replace. It skips any vault where the new key is not yet
  proven, so a vault is never left with neither key.
- A lost old key does not block replacement: it only needs the old public identity.

## Retiring a lost or old device

```sh
vulcan devices revoke vdev1_… --dry-run
vulcan devices revoke vdev1_…
```

In every Git vault registered on this machine it tombstones the device's registration and removes
only the deploy key Vulcan installed for that device. It never adds keys and never touches other
devices or foreign keys. Each vault reports its own result, and one failing vault does not affect
the others. It can only reach vaults registered **on this machine**: if the lost device was enrolled
in vaults you do not have here, run `vulcan sync devices revoke vdev1_…` inside them, or remove the
deploy key in the forge.

## Troubleshooting

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| `pending`: "no forge is configured for this host" | No `set-forge` entry for the host. | `vulcan device config set-forge …`, then `vulcan vault enroll <id>`. |
| `pending`: "a forge login is required" | OAuth configured, not logged in. | `vulcan sync forge login --wiki <id>`, then enroll again. |
| Probe says the remote refused the key | Key not authorized, or a read-only deploy key. | Authorize it (`vulcan sync forge authorize-self`), or ask an administrator. |
| Git says the device key is unavailable | The identity changed or its files are unreadable. | `vulcan device show`; re-run `vulcan vault enroll <id>`; `vulcan device repair-permissions` for loose permissions. |
| Only one process (such as the daemon) says it uses a different device than the bound one | That process reads another identity directory, usually a different `HOME` or `XDG_DATA_HOME`. | Fix that process's environment; do not re-bind, which would lock the installation key out. |
| Plain `git` ignores the key | `core.sshCommand` is owned by another tool, or Vulcan moved. | `vulcan sync transport status`; `vulcan sync transport bind` refreshes it. |
| A daemon sync fails for one vault only | That vault is not authorized for this key. | `vulcan sync doctor <id>` and `vulcan vault enroll <id>`. |
| Hardware or card-backed key never works unattended | Signing needs a touch or PIN. | Use `ambient` for that host; the device key is file-backed by design. |

## Safety properties

- The private key never leaves its file, is never printed, logged, synced, or put in a vault.
- Binding only follows a successful probe; a refused key is never bound.
- A bound vault never falls back to other credentials, and unbinding is always available.
- Nothing is ever revoked or removed because something was *absent*; only an explicit revocation
  removes a key, and only one carrying Vulcan's marker.
- A device signs every registration it writes with its own key, and readers verify it; this proves key possession, not approval.
- Forge settings are device-local; settings shared through a remote are proposals you must adopt.
- Each vault is separate; one vault's failure never changes another.

See also: [Git synchronization](git-sync.md), and the design specifications in
`docs/specs/device-transport-auth.md` and `docs/specs/device-key-enrollment.md`.
