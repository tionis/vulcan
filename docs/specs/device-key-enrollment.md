# Integrated Device-Key Enrollment

Status: proposed design for Roadmap 12.22. Builds on `device-transport-auth.md` (transport binding,
registrations, forge adapters, OAuth login). Nothing here is implemented yet.

## Goal

Make the device key the normal way a Vulcan installation reaches its vaults' Git remotes, without a
manual five-command setup per vault, and without ever leaving a repository in a state where plain
`git` or the daemon cannot authenticate. A clone or an add should end with the vault bound to the
device key when that is possible, and otherwise in a clearly reported, resumable `pending` state.

## Decisions

- **Default policy is `device-key`.** Absent any configuration, an SSH remote is enrolled with the
  device key. Hosts whose forges cannot use it (a forge that forbids one key on several repositories)
  are listed as `ambient`.
- **Three entry paths, all supported.** A user normally arrives with one of:
  1. *Ambient access*: an SSH agent holding a key that can reach the repository (or a credential
     helper such as git-credential-oauth for HTTPS remotes). Cloning works as it always did.
  2. *A pre-authorized device key*: the device key was added as a deploy key before cloning. The
     clone itself then uses the device key.
  3. *Dynamic OAuth authority*: the user can log in to the forge (OAuth, or an API token) and Vulcan
     authorizes the device key through the forge API *before* cloning, so the clone uses it.
- **No question is ever asked.** Every step is non-interactive. Where authority is missing the
  command reports `pending` with the exact next steps and exits successfully; re-running continues.
- **Lifecycle work comes after enrollment**: `device replace` and `devices revoke --everywhere` are
  the next slice (12.22.5), because relying on managed keys raises their importance.

## Policy: device-level configuration

Non-secret, device-local configuration in `device.toml` beside `daemon.toml` in the user config
directory. Vulcan ships no forge or client ID; this file is where they come from.

```toml
version = 1

[transport]
default = "device-key"          # device-key | ambient (the default when absent: device-key)

[[forge]]
host = "forge.example.com"
kind = "forgejo"
oauth_client_id = "…"           # public client ID for `sync forge login`
token_env = "FORGE_TOKEN"       # optional fallback credential
transport = "device-key"        # optional per-host override
login = "auto"                  # auto: log in interactively when attached to a terminal | never
```

Resolution for one vault: a per-host `transport` wins, then `[transport] default`, then `device-key`.
A per-command `--no-device-key` forces `ambient` for that command only. The file is validated
strictly (unknown fields refused, bounded size, HTTPS-only forge hosts are hostnames, never URLs) and
is never read from a vault or a remote. Auto-adopting a remote's published settings
(`trust_shared_settings`) is deliberately **not** part of this slice; an administrator still runs
`sync forge init --adopt` until the trust and consent story is designed.

## States

```text
ambient ──▶ registered ──▶ authorized ──▶ bound
   │            │               │
   └────────────┴───────────────┴──▶ pending (reason, next steps)
```

- `registered`: this device's registration record exists on the remote.
- `authorized`: a probe, `git ls-remote` authenticated with the device key alone, succeeded.
- `bound`: the transport binding (and `core.sshCommand`) is in place.
- `pending`: something outside this machine's authority is missing; the report says what.

**Probe before bind** is the central safety rule. Binding sets `core.sshCommand` and makes sync fail
closed, so binding a key the forge does not accept would immediately break plain `git`. Binding
therefore happens only after a successful probe, never before. A dry run never probes the network
unless asked, and never binds.

## The pipeline: `vault enroll`

One idempotent command, safe to re-run, each step reporting `done`, `already`, `skipped`, `pending`,
or `failed`:

1. **Policy.** Resolve the policy. `ambient` or a non-SSH remote ends here as `skipped` (an HTTPS
   remote cannot use SSH key authentication; Vulcan does not rewrite the user's remote).
2. **Identity.** Ensure the device identity exists (initializing it is what a first sync already does).
3. **Probe.** If the device key already authenticates, skip to step 6.
4. **Authority.** Resolve forge settings (the vault's own, else the device-level entry for the host
   combined with the host and `owner/name` derived from the remote). Resolve a credential (a valid
   OAuth login, refreshing it; else the token variable). With no valid login, run the login only if
   the forge's `login` is `auto` and the command is attached to a terminal or `--login` was given;
   otherwise stop as `pending: needs_login`. With a credential, **authorize this device** through the
   forge API: add a write-capable deploy key titled `vulcan-device:<id>` if no key with this public key
   exists. Without any authority, stop as `pending: not authorized` and print what an administrator
   runs (`sync devices register --public-key … ` then `sync forge sync`) and what to re-run here.
5. **Re-probe.** A few short retries, because forges apply new keys slightly later. Still denied is
   `pending`, not failure.
6. **Register.** Publish this device's registration (a placeholder if it has not synced) using whatever
   credential works now: the device key after step 5, otherwise ambient access. Best effort and never
   fatal; it also makes a `pending` device visible to an administrator.
7. **Bind.** Only after a successful probe: bind the transport and configure plain `git` (the
   `bind` defaults).

The report lists the steps, the final state, and `next_steps` as exact commands. Exit status is
success for `bound`, `pending`, and `skipped`; failures are real errors.

## Integration points

- **`vault clone` and `sync clone`.** With policy `device-key` and an SSH remote, before cloning:
  probe the device key against the URL; if it is not accepted and the host has an authority path,
  authorize the device through the forge API and re-probe. Then clone with the device key if the probe
  succeeds, otherwise with ambient credentials (path 1). After cloning and registering the wiki, run
  the pipeline. `--no-device-key` skips all of this; `--login` permits an interactive login.
- **`vault add`.** After registering an existing vault, run the pipeline. `--no-enroll` skips it.
- **`vault enroll <wiki>`.** The same pipeline for an already registered vault, for resuming a
  `pending` one and for repairing a stale binding.
- **First sync.** Unchanged: it still self-registers.
- **Observability.** `devices list` and `sync doctor` report the state and, for `pending`, the next
  steps. `vault enroll --all-wikis` iterates the registered vaults with independent results.

## Security considerations

- Probing is one unauthenticated-to-the-forge SSH attempt with the device key. It cannot lock a
  repository out and never sends another credential.
- Authorizing a device through the forge API uses the user's own OAuth login or token and adds only a
  `vulcan-device:`-marked deploy key; it never touches other keys.
- The device-level file holds no secret. A forge entry only names where to send a credential the user
  already holds; the same-host rule between the Git remote and the forge URL still applies.
- One unencrypted key authorizes every enrolled repository. That blast radius is why `device replace`
  and `revoke --everywhere` follow this slice.

## Non-goals

- Rewriting HTTPS remotes to SSH, or managing a user's own SSH keys.
- Enrolling vaults on forges without an adapter, or on forges that forbid shared deploy keys.
- Automatically adopting a remote's published settings (deferred).
- Interactive prompts of any kind.

## Tests

Policy resolution (default, per-host override, `--no-device-key`); strict device config parsing; the
probe against a real local SSH-less remote and against a denying one; authorize-self idempotence
against the fake forge; every pipeline branch (ambient, non-SSH, already bound, probe-ok, needs login,
no authority, authorized after retry, still denied); bind never happening before a successful probe;
clone using the device key, falling back to ambient, and authorizing first; `vault add` and
`--no-enroll`; and a CLI end-to-end for the three entry paths.
