# Declarative Device Configuration

Status: implemented (Roadmap 10.12). User guide: `docs/guide/device-config.md`
(`vulcan help device-config`). Two points differ from the original design and are marked
"Implemented as" below: a running daemon that needs new configuration is restarted as a whole, and
`apply --file` only checks.

## Problem

Setting up a device is a sequence of imperative commands whose results land in several places:

- `vulcan vault clone`/`add` registers vaults in `~/.config/vulcan/daemon.toml` and generates a
  `registration_id` ULID for each.
- `vulcan mcp remote init` writes a named remote with a generated `instance_id`.
- A remote's ceiling profile must be defined in each vault's device-local
  `.vulcan/config.local.toml`, a second device-local file per vault.
- Notifications, agents, workers, bots and sync policy have their own `vulcan daemon config ...`
  commands.

Because `daemon.toml` mixes what the owner chose (Git URL, path, profiles, packs, bind) with what
Vulcan generated (ULIDs), it cannot be copied to another machine, kept in dotfiles, or rendered by
configuration management. `save_config` also rewrites the whole file with `toml::to_string_pretty`,
so comments and hand formatting are lost on the next CLI mutation. Moving one named remote to a
server takes a clone, a file copy for one profile, a retyped remote definition, and re-consent.

## Decisions

Stated by the owner (2026-11-07):

1. **`daemon.toml` is the declarative file.** No second manifest format. It holds only owner
   intent, can be written by hand or rendered by chezmoi or Ansible (Gandalf), and CLI commands
   keep editing the same file.
2. **Undeclared state is drift, removed only with `--prune`.** State that exists on the device but
   is no longer declared is reported by `plan` and left alone by `apply`, like `picomq-tokens`.
   `apply --prune` removes it. Vault files are never deleted either way.
3. **`apply` also manages the daemon service.** It installs the native per-user service when it is
   missing and restarts it when the executable changed, so applying the file is the whole setup.
4. **Everything in `daemon.toml` is covered**, not only vaults and remotes: notifications, agents,
   workers, the chat bot, `scan_dirs` and `automatic_sync` are planned, validated, and reloaded the
   same way.
5. **A rendered file names its owner** with `managed_by`. Mutating commands then refuse and print
   the snippet to add to that owner's source.
6. **Per-vault device-local settings are declarable too.** Anything a vault's
   `.vulcan/config.local.toml` can hold can be declared on its `[[vault]]` entry, so a vault needs no
   local file at all.

## Goals

- Copying one file (plus device-local secrets) to a new device and running one command reproduces
  its vaults, device-local permission profiles, named remotes, workers, agents, notifications and
  bots, and leaves the daemon service installed and running.
- One place for device-local authority: ceiling profiles can be defined next to the remotes,
  tokens and schedules that use them.
- `plan` shows exactly what `apply` would do; `apply` is idempotent.
- Comments and formatting in `daemon.toml` survive CLI mutations.
- Existing installations migrate automatically without losing any identity that API tokens, scan
  state, or OAuth grants reference.

## Non-goals

- Moving OAuth issuer keys, grants or refresh-token families between devices. Authority stays per
  device; a remote moved to a new device needs fresh consent once.
- Templating, includes or per-host conditionals inside Vulcan. Host differences are the job of
  chezmoi or Ansible, which already render per-host files.
- Secrets in `daemon.toml`. It continues to name environment variables or `SecretStore` entries.
- Declaring vault content or shared vault configuration. Synced `.vulcan/config.toml` stays the
  vault's own; this file is device-local.

## File model

### Desired state: `~/.config/vulcan/daemon.toml`

Everything in it is owner intent. Generated identifiers are no longer written to it.

```toml
bind = "127.0.0.1:3210"

[[vault]]
id = "grimoire"
path = "~/wikis/grimoire"
source = "git@forge.tionis.dev:eric/grim"   # new: lets apply clone a missing vault
sync_backend = "git"

[permissions.profiles.grimoire-rw]           # new: device-global device-local profiles
read = "all"
write = "all"
# ... same schema as a vault profile

[[mcp_remote]]
id = "grimoire"
bind = "100.102.174.182:8765"
public_url = "https://grimoire.proxy.tionis.dev/mcp"
authentication = { type = "indie_auth", identity = "https://eric.wendland.dev/" }

[[mcp_remote.vaults]]
wiki_id = "grimoire"
ceiling_profile = "grimoire-rw"
default_profile = "grimoire-rw"
tool_packs = ["notes-read", "search", "status"]
```

- `~` and `$HOME` expand in paths, so one file fits devices with different home directories.
- `source` is optional. Without it, a missing path is an error, as today.
- `[mcp_remote]` `bind` is required in the file. `mcp remote init` still picks a free port and
  writes it, so the interactive path stays as easy as now.

### Identity and state: `daemon.identities.toml`

Vulcan-owned, never hand-edited, never copied between devices. It sits beside `daemon.toml` rather
than in the state directory: it must be backed up together with the configuration (losing it orphans
API tokens and grants), and the many tests and setups that isolate only the configuration directory
must not leak identities into the real state directory. Configuration management renders only
`daemon.toml` and leaves this file alone.

```toml
[vault.grimoire]
registration_id = "01M1..."
[mcp_remote.grimoire]
instance_id = "01M6..."
```

- Identity is keyed by the declared id. A declared vault or remote without an entry gets a fresh
  ULID on first reconcile.
- Removing an entry with `vulcan vault remove` or `vulcan mcp remote remove` deletes its identity, so
  registering the same id again never revives old API tokens or grants. Removing it from the file
  by hand keeps the identity as drift, so declaring it again restores it until `--prune`.
- Renaming an id in the file is a new identity (old one becomes drift). `vulcan vault set --id`
  and `vulcan mcp remote set --id` rename both the declaration and the identity entry, which keeps
  API tokens and grants.
- The file is written atomically under the existing `daemon.toml.lock`.

### Device-local permission profiles

`[permissions.profiles.<name>]` in `daemon.toml` defines profiles that are device-local by
construction (the file is never in a vault). They are valid wherever a device-local profile is
required today: named remote ceilings, API tokens, scheduled jobs, chat bot authority.

Resolution (`resolve_device_permission_profile`) gains this source. `vulcan-core` reads
`daemon.toml` from the user configuration directory itself, as it already reads
`trusted_vaults.json`, so every code path that loads profiles or vault configuration sees the same
layers whether or not the CLI or daemon wired anything up. It reads only `[permissions]` and the
`path` and `config` of `[[vault]]` entries and stays synchronous. Order:

1. Built-in profiles.
2. `daemon.toml` profiles, then the vault entry's `config.permissions`.
3. The vault's `.vulcan/config.local.toml` profiles (kept for compatibility).

A profile both device files define differently is reported with both file names, and because any
diagnostic makes device-profile resolution fail, every device ceiling for that vault is unavailable
until it is resolved: authority never depends on a silent precedence. A profile only in the synced
vault config remains refused as a ceiling, with the existing `profile pin` path. The general
profile list (`--permissions <name>`) sees the same layers between the shared and local config.

### Per-vault device-local settings

A `[[vault]]` entry may carry a `config` table with the same schema as `.vulcan/config.local.toml`
(any `VaultConfig` override, including vault-scoped permission profiles):

```toml
[[vault]]
id = "grimoire"
path = "~/wikis/grimoire"

[vault.config.web.search]
backend = "brave"

[vault.config.permissions.profiles.agent-local]
read = "all"
```

Effective vault configuration, lowest to highest: built-in defaults, synced `.vulcan/config.toml`,
the device-global `[permissions]`, the `[[vault]]` `config` table, `.vulcan/config.local.toml`. The
two device-local sources are merged key by key. A setting both set to different values is reported
with both file names and `config.local.toml` wins; failing every command over a setting would be
worse than a visible warning. Permission profiles fail closed as described above. A vault entry
matches when its `path` (after `~` expansion) and the vault root canonicalize to the same
directory; `[folder_notes]` stays shared-only, as in `config.local.toml`. `config.local.toml` keeps working and stays the place for settings on an
unregistered vault.

This holds for direct CLI use without the daemon, because `vulcan-core` reads the file itself (see
above). Unit tests of `vulcan-core` never read the developer's real `daemon.toml`; they set a device
file per test. The daemon stores `[permissions]` and `config` tables opaquely and writes them back
unchanged. `vulcan config show` reports which
source set each key, and `vulcan config set --target device` writes to the `[[vault]]` entry.
`vulcan config local move` (run in the vault) moves an existing `config.local.toml` into the entry.

## Commands

```text
vulcan daemon plan  [--output json] [--file <path>]
vulcan daemon apply [--dry-run] [--prune] [--output json] [--file <path>]
```

`--file` checks another file without installing it (for review before copying). Implemented as:
`apply --file` behaves like `apply --dry-run` on that file, because applying anything but the
installed file would leave the device running a configuration no file describes.

`plan` covers every section of the file and the daemon service itself (installed, running,
executable current). It compares the file with the device and reports, per item, one of `ok`, `create`, `update`,
`clone`, `drift` (undeclared state), or `error`, with the reason. Exit code 0 when nothing would
change, 2 when changes are pending, 1 on an error (same convention as `picomq-tokens plan`).

`apply` performs, in order:

1. Validate the whole file. Any error stops before any change.
2. Allocate identities for new declarations.
3. Clone each declared vault whose path is missing and that has a `source`, with the same code
   path as `vulcan vault clone`. A path that exists must be a repository whose remote matches
   `source`, or apply reports an error for that vault and continues with the others.
4. Make sure the daemon service is installed and running, the same work as `vulcan daemon install`
   (`--no-service` skips this, for containers or manual supervision). A running daemon reloads its
   configuration: changed remotes, vault runtimes, workers, agents, notification sinks and bots
   restart; unchanged ones keep running. A daemon started from an older executable is restarted.
   Implemented as: vault declarations and permission profiles are picked up while the daemon runs
   (it polls the registry, and `vulcan-core` rereads `daemon.toml`); any other change, detected by
   a digest of those sections in the daemon's runtime record, or a replaced executable restarts the
   whole daemon through a graceful stop (final syncs run) and the service manager.
5. With `--prune`: remove drift. That is identity entries, scan state and API tokens of undeclared
   vaults, and OAuth state (signing keys, clients, grants) of undeclared remotes. Each removal is
   listed in `plan` first. Working trees and Git directories are never deleted.

`apply --dry-run` prints the plan and changes nothing.

### Daemon start

`vulcan daemon start` runs steps 1, 2 and 4 of apply on its own configuration. It does not clone
(network, unbounded time) and does not prune. A declared vault that is missing is reported in
`vulcan daemon status` with the hint to run `vulcan daemon apply`. So the shortest server setup is:
render `daemon.toml` and run `vulcan daemon apply`.

### CLI mutations

Existing commands (`vault add/clone/set/remove`, `mcp remote init/set/remove`,
`daemon config ...`, `config permissions profile ...` with a new `--target device`) keep working
and edit `daemon.toml` in place with a format-preserving TOML editor, keeping comments, order and
unrelated sections.

A file rendered by configuration management names its owner at the top level, for example
`managed_by = "gandalf"` or `managed_by = "chezmoi"`. Mutating commands then refuse with
"daemon.toml is managed by gandalf; add this to its source instead" and print the exact TOML snippet,
so the next render cannot silently undo a CLI change that would otherwise be lost (Ansible overwrites
without a diff). `--force-local-edit` performs the edit anyway, for an emergency fix on a server,
and warns that the next render reverts it. Without `managed_by`, CLI edits apply in place. `plan`
reports the owner so it is visible which tool to change.

## Migration

On the first load by a new version:

1. Read `registration_id` and `instance_id` from the existing `daemon.toml`.
2. Write them to `daemon.identities.toml` (atomic, under the lock).
3. Remove only those lines from `daemon.toml`; nothing else in the file changes.

If step 3 fails, the old fields are still read on the next start and the migration repeats; an
identity in both places must match or loading fails with both values named. The old fields remain
accepted for reading for at least one release. A downgrade after migration would generate new IDs
and orphan tokens and grants, which the release notes must state.

Vault `config.local.toml` profiles are not moved automatically. `vulcan daemon plan` suggests the
move for profiles used as ceilings, and `vulcan config permissions profile move <name>`
does it.

## Security

- The file never carries secrets, so it is safe in dotfiles and in Ansible inventories (as today's
  `daemon.toml` already is, apart from IDs).
- Device-local profiles in `daemon.toml` keep the existing rule: a vault push cannot widen a
  ceiling, because the file is not in any vault.
- `apply` clones only from the declared `source`, with the device's normal Git transport
  (including the device-key binding). It never runs vault hooks or scripts.
- `--prune` is the only destructive step, it only removes Vulcan state, and `plan` lists every
  removal before it happens.

## Phasing

1. Identity split and migration, format-preserving writes. No new behavior; all existing commands
   keep working. This is the prerequisite for everything else.
2. Device-global permission profiles and resolver input, per-vault `config` tables as a device
   overlay for the loader (direct CLI included), `--target device`, `profile move`, `config local
   move`.
3. `vulcan daemon plan` and `apply` without cloning or pruning, daemon reload.
4. `source` and cloning in apply, missing-vault status at daemon start.
5. `--prune`, `managed_by`, and service install and restart in `apply`.
6. Guides, help topics, the configuration reference, the `mcp-setup` and configuration skills, an
   example rendered by chezmoi or Ansible.

## Test plan

- Migration: IDs move intact; API tokens, scan state and OAuth grants keep working; an interrupted
  migration resumes; conflicting IDs fail closed.
- Format preservation: comments, ordering and unknown-to-the-command sections survive every
  mutating command.
- Vault overlay: a `[[vault]]` `config` key applies to direct CLI and daemon runs; conflicting keys
  in the overlay and `config.local.toml` fail; `config show` names the source; unregistered vaults
  are unaffected.
- Profiles: a ceiling resolves from `daemon.toml`; conflicting duplicate names fail; a synced-only
  profile is still refused.
- Plan/apply: idempotence (apply twice, second plan is empty), exit codes, `--dry-run` writes
  nothing, `--file` applies nothing to the live config.
- Clone: missing path cloned from `source`; existing path with another remote is an error; one
  failing vault does not stop the others.
- Service: apply installs a missing service, restarts on a changed executable, and `--no-service`
  leaves it alone.
- `managed_by`: mutating commands refuse with the owner and snippet; `--force-local-edit` edits and
  warns.
- Prune: drift listed by plan, untouched by apply, removed only with `--prune`; vault files never
  deleted.
- Non-regression: an existing installation with no new fields behaves exactly as before; direct
  CLI works without the daemon; the `--no-default-features` build lints and tests clean.
