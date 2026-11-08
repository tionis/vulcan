# Declarative device configuration

One file describes a device: `daemon.toml` in the Vulcan configuration directory
(`~/.config/vulcan/daemon.toml` on Linux). It lists the device's vaults, its device-local permission
profiles and per-vault settings, named MCP remotes, workers, agents, notifications and bots. Write it
by hand, keep it in your dotfiles, or render it with chezmoi or Ansible, then run:

```sh
vulcan daemon plan      # what would change (writes nothing)
vulcan daemon apply     # make the device match
```

`apply` clones missing vaults, stores identities for new vaults and remotes, installs the native
daemon service when it is missing, and restarts the daemon when it runs an older configuration or
executable. Running it again does nothing. CLI commands (`vault add`, `mcp remote init`,
`daemon config ...`, `config set --target device`) keep editing the same file and keep its comments
and formatting.

## The file

```toml
bind = "127.0.0.1:3210"

[[vault]]
id = "grimoire"
path = "~/wikis/grimoire"                    # `~` expands on every device
source = "git@forge.example:eric/grim"       # lets apply clone a missing vault
sync_backend = "git"

[vault.config.periodic.daily]               # same schema as .vulcan/config.local.toml
template = "Templates/Daily"

[permissions.profiles.grimoire-rw]           # device-local profile, valid as a ceiling
read = "all"
write = "all"
refactor = "all"
config = "read"
index = "allow"
git = "allow"
execute = "deny"
shell = "deny"
network = "deny"

[[mcp_remote]]
version = 1
id = "grimoire"
bind = "0.0.0.0:8765"
public_url = "https://grimoire.proxy.example/mcp"

[mcp_remote.authentication]
type = "indie_auth"
identity = "https://me.example/"

[[mcp_remote.vaults]]
wiki_id = "grimoire"
ceiling_profile = "grimoire-rw"
default_profile = "grimoire-rw"
tool_packs = ["notes-read", "notes-write", "search", "status"]
```

- **Vaults.** `path` is required; `source` is optional. Without `source`, a missing path is an
  error. With it, `apply` clones the vault there, using the device key when the forge accepts it
  (see `vulcan help device-keys`), and an existing path must be a repository whose remote is
  `source`. `vulcan vault clone` records `source` for you.
- **Permission profiles.** Built-ins such as `readonly`, `writer` and `maintainer` (see
  `vulcan help sandbox`) need no definition. `[permissions.profiles.<name>]` defines more for every
  vault on this device. Because the file is never in a vault, a push cannot widen them, so they are
  valid as named-remote ceilings, API token and schedule ceilings, and bot authority. A vault can
  suggest a profile in its shared `config.toml`; review it and copy it here with
  `vulcan config permissions profile pin <name> --target device`.
- **Per-vault settings.** A `[vault.config]` table takes anything `.vulcan/config.local.toml` can
  hold, so a vault needs no local file. It applies to direct CLI runs too. A key that
  `config.local.toml` sets differently is reported and `config.local.toml` wins; a profile defined
  differently in both makes the vault's device ceilings unavailable until it is fixed.
  `vulcan config list` shows such values with source `device_override`.
- **Secrets never go here.** Agents, webhooks and bots name environment variables
  (`api_key_env = "..."`); put the values in `daemon.env` beside the file or the service
  environment.

Identities Vulcan generates (vault registration IDs, remote instance IDs) live in
`daemon.identities.toml` beside the file. API tokens, scan state and OAuth grants refer to them. Do
not edit it or copy it between devices; back it up with the configuration.

## Writing from the CLI

```sh
vulcan config set periodic.daily.template Templates/Daily --target device   # [vault.config]
vulcan config permissions profile create agent --clone readonly --target device
vulcan config permissions profile move grimoire-rw   # from config.local.toml
vulcan config local move                              # all of config.local.toml
```

`--target device` sends `permissions.*` keys to the device-wide `[permissions]` and everything
else to the current vault's `[vault.config]`; the vault must be registered. Device writes are never
auto-committed. Long inline tables may span lines in the shell:

```sh
vulcan config set permissions.profiles.notes-rw --target device '{
  read = "all", write = "all", refactor = "all", config = "read",
  index = "allow", git = "allow", execute = "deny", shell = "deny", network = "deny" }'
```

## Plan and apply

`vulcan daemon plan` reports each item as `ok`, `create`, `update`, `clone`, `drift`, `skip` or
`error`, with the reason. It exits 0 when `apply` would change nothing, 2 when changes are pending,
and 1 on an error, so scripts and configuration management can use it as a check.

`vulcan daemon apply`:

1. Validates the whole file. Any error in the configuration or a remote's ceiling stops it before
   any change. A vault that cannot be cloned or whose remote does not match is reported and the
   other vaults continue.
2. Stores identities for new vaults and remotes.
3. Clones vaults whose path is missing and that have a `source`.
4. Installs the native daemon service when it is missing or outdated, and restarts the daemon
   when it runs an older configuration or executable (for example after `cargo install` or
   `self-update`), or starts it. Vault changes need no restart: the daemon picks them up while
   running. `apply` succeeds only once a new daemon runs the current executable and configuration;
   if the old one is still finishing its final syncs, it stops and asks you to run it again.
   `--no-service` leaves the service and process alone, for containers or manual supervision.

Options: `--dry-run` prints the plan only. `--file <path>` plans another file, for review before
copying it into place; `apply --file` only checks. `--output json` gives the plan and each step.

`vulcan daemon start` validates the file and stores identities but never clones; a declared vault
that is missing shows in `vulcan daemon status` with the hint to run `vulcan daemon apply`.

## Drift and `--prune`

State that exists on the device but is no longer declared is drift: identities of vaults and
remotes removed from the file by hand, scan state and API tokens of undeclared vaults, and OAuth
keys, grants and runtime state of undeclared remotes. `plan` lists it and `apply` leaves it alone,
so declaring an entry again restores it with its tokens and grants. `vulcan daemon apply --prune`
removes it. Working trees and Git directories are never deleted. `vault remove` and
`mcp remote remove` remove the identity right away, so a later registration with the same id never
revives old tokens or grants.

## Files rendered by configuration management

A rendered file names its owner at the top:

```toml
managed_by = "chezmoi"
```

CLI commands that would edit the file then refuse and print the TOML to add to the owner's source,
so the next render does not silently undo a change. `--force-local-edit` edits anyway, for an
emergency fix, and warns that the next render reverts it. `plan` shows the owner.

A chezmoi template that differs per host
(`~/.local/share/chezmoi/dot_config/vulcan/daemon.toml.tmpl`):

```toml
managed_by = "chezmoi"
bind = "127.0.0.1:3210"

[[vault]]
id = "grimoire"
path = "~/wikis/grimoire"
source = "git@forge.example:eric/grim"
sync_backend = "git"

{{ if eq .chezmoi.hostname "server" -}}
[[mcp_remote]]
version = 1
id = "grimoire"
bind = "0.0.0.0:8765"
public_url = "https://grimoire.proxy.example/mcp"

[mcp_remote.authentication]
type = "indie_auth"
identity = "https://me.example/"

[[mcp_remote.vaults]]
wiki_id = "grimoire"
ceiling_profile = "grimoire-rw"
default_profile = "grimoire-rw"
tool_packs = ["notes-read", "search", "status"]
{{- end }}

[permissions.profiles.grimoire-rw]
read = "all"
write = "all"
```

With a `run_onchange_` script that runs `vulcan daemon apply`, `chezmoi apply` sets up the whole
device. In Ansible, render the same file with the `template` module and run
`vulcan daemon plan` with `changed_when: result.rc == 2` and `failed_when: result.rc == 1`, then
`vulcan daemon apply` when it changed.

## Moving a remote to another device

Copy its `[[mcp_remote]]` entry, its vault (with `source`) and the ceiling profile into the other
device's `daemon.toml` and run `vulcan daemon apply` there. OAuth state stays per device, so the
client approves the connection once more. On the old device, remove the entry and run
`vulcan daemon apply --prune` to drop its keys and grants.
