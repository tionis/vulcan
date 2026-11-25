---
name: sync-workflow
description: Synchronize one or more Vulcan wikis safely, tune how often the daemon checks remotes, inspect daemon or direct-mode state, diagnose Git-backed sync, inspect retained conflict records, recover detached Android layouts, or recover another device's backup. Use this whenever a user asks about `vulcan sync`, multi-device vault updates, how quickly other devices' changes arrive, the Vulcan daemon or Obsidian companion, Termux sync, sync conflicts, paused branches, or interrupted synchronization. Do not use it for ordinary human-authored Git commits with no device-sync concern; use git-workflow for that.
version: 41
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Sync Workflow

Vulcan synchronizes a vault by moving committed Git history through finite, recoverable
transactions: each cycle fetches, integrates the checked-out branch with its upstream, and pushes.
Uncommitted edits never travel. Use the direct CLI for one-shot work and the daemon for scheduling,
watching, multiple wikis, or a companion client; both execute the same application workflow.

## Inspect local device identity

- Use `vulcan device show --output json` for a vault-independent, state-free view of the local
  identity. `device_id` is the key-derived ID that sync uses as its actor; it proves no trust,
  remote access, or enrollment. The first mutating sync initializes the identity automatically.
- Preview `vulcan device init --dry-run` before initializing a missing local Ed25519 identity.
  Initialization never replaces existing key material. `vulcan device public-key` explicitly
  exports the public key; never request or print the private key for inventory work. If `device show`
  reports `invalid` because storage is readable by other accounts, preview
  `vulcan device repair-permissions --dry-run`; it only tightens access and never rewrites keys.
- `vulcan device key status --output json` reports the key's custody `provider` (`file_v1` today),
  its `state` (`available`, `missing`, `unavailable`, `invalid`, `unsupported`, or `unknown`), and
  per-capability support. It never signs, unlocks, or prompts. `unsupported` means the manifest
  names a provider this build cannot use: report it, and do not edit the manifest or copy key files
  to work around it. `vulcan device key verify` proves possession by signing a fresh challenge
  locally; use it when a device-key operation fails, before suggesting re-enrollment or replacement.
- Device IDs are key-derived (`vdev1_...`). A safety branch named with an older 26-character ULID
  is refused by `vulcan sync devices list`; report it to the user rather than deleting it. A stale
  legacy `_device.json` is ignored.
- `vulcan sync transport bind|status|unbind` makes one vault's Git sync (CLI and daemon) authenticate
  over SSH with the device key. Preview with `--dry-run`; it needs an SSH remote and an initialized
  identity, never initializes one, and never falls back to another key. By default `bind` also writes
  a Vulcan-owned repository-local `core.sshCommand` so plain `git` in that repository uses the device
  key too; the value falls back to ordinary `ssh` if the `vulcan` executable it names disappears. A
  `core.sshCommand` owned by another tool is left alone and reported (`git_config_skipped`).
  `--no-git-config` opts out and `--git-config` insists. After installing a new `vulcan`, re-run
  `bind` so the value names the installed path (`doctor` reports a `stale` value). Binding does not
  authorize the key on the remote; that remains a separate step, and `sync doctor` cannot verify it
  offline. Do not bind on the user's behalf unless asked, since their own SSH key may be intended.
- Use `vulcan devices list --output json` for an installation-wide view across registered wikis.
  Each wiki has its own local recovery and remote observation state; a failed remote observation
  means remote backup status is unknown, while locally retained recovery and names remain visible.
  Use `vulcan devices list --offline` for a guaranteed local-only inventory; its remote state is
  `not_requested`, which does not mean no remote backups exist. The same flag is available for a
  selected wiki through `vulcan sync devices list --offline`.
  The current inventory uses the default `origin` remote for Git registrations.
  The backup list is an observed recovery inventory, not an enrollment, possession, trust, or online
  roster. Device registrations are listed separately under `registrations` in the same output.
- Each device publishes a registration record (key-derived ID, public key, status) to the remote on
  its first successful non-dry-run sync, reported once as `registration.outcome`. Registering never
  fails a sync. `vulcan sync devices register --public-key <file>` creates a key-bearing
  `placeholder` that the device claims on its first sync; `revoke <full-id>` writes a tombstone the
  device never overwrites; `unregister <full-id>` deletes the record. All accept `--dry-run`. Always
  pass the full `vdev1_` ID and confirm it against `vulcan device show` on that device. The list is
  trusted as written by anyone who can push, it grants no remote access, and revoking it does not
  remove any forge deploy key. Never register, revoke, or unregister on the user's behalf unprompted.
- `vulcan sync forge init` is the generic setup. The forge host and `owner/name` are derived from
  the Git remote (`git@host:owner/name` gives `https://host` and `owner/name`), so only the kind and a
  credential are stated: `--kind forgejo --oauth-client-id <ID>` and/or `--token-env <VAR>`. Add
  `--publish` to share the non-secret settings (kind, OAuth client ID, and a URL only if it differs)
  on the remote. On another machine, `sync forge init` with no `--kind` only *shows* what the remote
  proposes; `--adopt` saves it locally. Anyone who can push can change the published settings, so never
  adopt on the user's behalf without showing them the proposal, and never pass `--allow-other-host`
  unless the user asks: the API URL must be on the Git remote's host, since that is where the
  credential is sent. Adoption keeps device-specific values (the token variable) local.
- `vulcan help device-keys` is the complete user walkthrough of everything below (default flow,
  forge setup, replacing and revoking keys, troubleshooting); point users to it rather than
  re-deriving the steps.
- `vulcan vault enroll <wiki>` takes one vault to "bound to this device's key", or says exactly what is
  missing. An HTTPS remote is skipped unless `--switch-to-ssh` is given, which switches it to the
  forge's SSH address only after the key is proven there (preview with `--dry-run`). For a directory
  of arbitrary files, such as a media library in Git LFS, register it with
  `vulcan vault add <id> <path> --profile files-only` (Git sync without a Markdown index) first. It checks the transport policy (the default is the device key; `device config set-transport
  ambient` or `--no-device-key` skips it), ensures the device identity, probes whether the remote
  accepts the device key, authorizes it through the forge when this machine has the authority (an OAuth
  login or the token variable), then binds the transport (including `core.sshCommand`) and publishes
  this device's registration. It never binds a key the remote has not accepted, never asks a question,
  and is safe to re-run; when authority or a login is missing it reports `pending` with `next_steps`
  and still exits successfully. `--dry-run` probes read-only and reports what it would do;
  `--all-wikis` gives each vault its own result. Interactive login only starts with `--login` (or on a
  terminal when the forge entry allows it), so never pass `--login` unattended.
- `vulcan vault clone`, `vulcan sync clone`, and `vulcan vault add` now run that enrollment for you
  when the transport policy is `device-key` (the default) and the remote is SSH. A clone first probes
  the URL: if the device key is accepted, or this machine can authorize it through the forge, the clone
  itself uses the device key; otherwise it uses the user's own credentials exactly as before and
  enrollment finishes afterwards as `pending`. The clone or add never fails because of enrollment; the
  result is in the `transport` and `enroll` fields (or `enroll_error`). Pass `--no-device-key` for the
  old behaviour, and never pass `--login` unattended. A vault added or cloned before this existed is
  enrolled with `vulcan vault enroll <wiki>`.
- `vulcan vault add` records the checkout's `origin` URL as the entry's `source` in `daemon.toml`,
  as `vault clone` does, so the entry can clone the vault on another device.
- `vulcan vault add` on a folder that is not a Git repository still registers it, but reports
  `not_a_git_repository: true` and skips enrollment. `vault add <id> <path> --git-init` creates the
  repository first (no commit, no remote) and leaves an existing repository alone; a refused
  registration creates nothing. Adding the `origin` remote is still the user's step; afterwards run
  `vulcan vault enroll <wiki>`.
- `vulcan sync transport bind --all-wikis [--dry-run]` binds every registered Git vault whose remote already accepts the device key (it probes first and skips, with the fix, any vault that refuses it); it authorizes and registers nothing, so use `vault enroll` for that. `sync devices list` shows whether each registration is signed by its device: a device signs every record it writes, an unsigned `registered` record is an older one that is signed on that device's next sync, and a signature proves key possession, not approval.
- `vulcan device replace [--revoke-old] [--activate-anyway] [--dry-run]` swaps this installation's
  device key safely: it stages a new key, authorizes and proves it in every vault bound to the old
  key, and only then activates it and rebinds each vault. If a vault cannot be proven yet it changes
  nothing and prints the pending steps; re-run it later. Preview with `--dry-run` first, and never
  pass `--activate-anyway` or `--revoke-old` unless the user asked. `vulcan devices revoke <device-id>`
  retires a (lost or old) device in every registered Git vault: it tombstones the registration and
  removes only that device's Vulcan-marked deploy key; it reaches only vaults registered on this
  machine, so say so. Use `vulcan sync devices revoke` for a single vault.
- `vulcan device config show|set-transport|set-forge|remove-forge` edits this device's non-secret
  policy in `device.toml` beside `daemon.toml`: the default transport (`device-key`, or `ambient` to
  leave SSH to the user), and per-host forge entries (kind, public OAuth client ID, a token variable
  *name*, an optional per-host transport override, and `login auto|never`). It never holds a secret
  and is never read from a vault or remote. `vulcan sync forge authorize-self` installs this device's
  key as a Vulcan-managed deploy key through the forge API using the user's OAuth login or token,
  with no registration needed; it is idempotent and only ever touches its own key.
- `vulcan sync forge login` authenticates with the forge's OAuth app instead of a pasted token: it
  needs the OAuth client ID from `sync forge init --oauth-client-id`, prints an authorization URL,
  tries to open a browser, and waits for the redirect to `127.0.0.1`. It is interactive (the user
  approves in their browser), so never run it unattended. The login is stored device-locally with
  owner-only permissions, refreshes automatically, and is shown (never the token) by `sync forge show`;
  `sync forge logout` removes it, but only revoking the app in the forge's settings invalidates the
  grant. `forge sync` uses the OAuth login first and the `--token-env` variable as a fallback.
- `vulcan sync forge set --url <https-url> --repo <owner/name> --token-env <VAR>` saves device-local
  forge settings (only the variable *name*; never ask for or print the token). `vulcan sync forge sync
  --dry-run` shows the plan, then `vulcan sync forge sync` installs a write-capable deploy key for
  each placeholder or registered device and removes the key only for an explicitly `revoked` one.
  Vulcan-marked keys without a registration are reported as orphans and never removed; keys without
  the `vulcan-device:` title marker are never touched. It changes nothing if the registrations cannot
  be read from the remote. Always run the dry run first and show it to the user; because the
  registration list is trusted as written by anyone who can push, an unexpected `add` is worth
  questioning. It needs network permission for the forge URL and the token variable set.
  `vulcan sync forge sync --all-wikis` repeats this for every registered Git vault that has forge
  settings; each vault uses its own settings, token, permissions, and result, one failure never
  affects the others (the command still exits non-zero), and vaults without settings are skipped.
- `vulcan devices list` also reports, per vault, the device-local Git `transport` state and
  `this_device_registration` (`not_registered`, `placeholder`, `registered`, `revoked`, or `unknown`
  when the remote was not observed, as with `--offline`). They are separate per vault: a registration
  or binding in one vault says nothing about another, and neither proves remote access.

For a new sync checkout, preview `vulcan sync clone <remote> <path> --dry-run` before applying it.
The command derives the wiki ID from the destination, uses native clone defaults on desktop, and
automatically selects a detached private Git directory plus the `android-shared` policy in Termux.
Pass `--id`, `--git-dir`, or `--platform` only when those defaults are not appropriate. Add
`--profile files-only` when the registration should not expose knowledge services.

## Select the execution mode

- A registered directory uses the versioned device-local `knowledge` profile by default. Choose
  `--profile files-only` on `vault add`, `vault clone`, `sync clone`, or `vault set` for a full
  working tree that should not initialize or refresh a Markdown index. This profile also disables
  knowledge-specific validation, scripts, and agent services while keeping the same Git sync.
  Profile state is local registration metadata, not synchronized vault config. Existing
  registrations keep the knowledge default and their prior JSON representation.
  `vault add --no-sync` registers a directory without a file-tree sync backend, which is also the
  default for a directory that is not a Git repository (for example a Syncthing folder). `vault set <id>
  --sync-backend git` or `--no-sync` changes it later without re-registering, so tokens and remotes
  are kept.
  Never run Git sync and Syncthing on the same tree: `vault add` warns and `sync doctor` reports
  `sync.file-synchronizer` when a `.stfolder` sits at or above the vault, since the two race
  and Syncthing copies `.git`. The exception is a folder whose `.stignore` ignores `.git` with one
  committing device running Git sync (the others use `--no-sync`); that is reported only as a note.
  `vulcan doctor` lists Syncthing's leftover
  `*.sync-conflict-*` copies under `sync_conflict_copies`: merge each into its original, then
  delete the copy; until then it is indexed and searched like any note.
- Use `vulcan vault show <id>` or `vulcan vault list` to inspect registration. Human output calls
  out a nondefault files-only profile. Each entry's `health.state` names what most needs attention
  (`missing`, `unindexed`, `conflicted`, `recovery_required`, `paused`, `failing`, `offline`,
  `syncing`, or `healthy`) with the conflict count and last sync attempt; it works without a
  daemon (`daemon_running` says whether one answered). Start from it before choosing
  `vulcan sync conflicts`, `vulcan scan`, or `vulcan sync resume`, and leave resuming a paused vault
  to the user unless asked.

- For an unregistered path or a daemon-independent operation, use `vulcan --vault <path> sync ...`.
  Direct commands never start a daemon implicitly. For an unregistered files-only path, choose
  `vulcan --vault <path> sync run --profile files-only` (or `sync status --profile files-only`);
  registered directories use their stored profile.
- Files-only registrations cannot run knowledge commands. Change the stored profile back to
  `knowledge` before using indexing, search, scripts, or other knowledge services.
- The Vulcan lock does not exclude external Git processes; do not switch branches or run Git while
  a daemon job is active. Active development checkouts need a separate contract and are not an
  unattended sync target. Sync does not provide a complete refs/history/LFS backup.
- For registered wikis, select one ID, `--group <name>`, or `--all`. Aggregate results are independent
  per-wiki transactions, not a cross-repository atomic commit. `--all` and `--group` leave out
  wikis registered with `--no-sync`.
- When the daemon runs, `vulcan sync run --group <name> --queue` (or one ID, `--all`, or the
  selected registered vault) asks it to queue one retained job per wiki and returns at once with
  the aggregate and child job IDs; follow them with `vulcan daemon status`. Queued jobs use each
  registration's stored profile, so `--permissions` and the run-tuning flags are refused. Without a
  daemon `--queue` fails rather than syncing here; drop it to run the cycles directly. After a
  transport error the jobs may already be queued, so check `daemon status` before retrying.
- Direct and daemon sync of an initialized vault use the same vault-write and repository locks as
  ordinary Vulcan mutations and Git/auto-commit commands. A competing writer may therefore wait or
  report bounded repository contention; do not bypass the lock with raw Git while sync is active.
  Sync holds the vault-write lock only while it touches the work tree, not while it fetches or
  pushes, so note and task writes proceed during a slow or unreachable remote; there is no need to
  pause sync before writing.
  On Android, Vulcan keeps its vault-write lock and rebuildable SQLite cache in private device
  state because Android shared storage does not support the required file locking and SQLite WAL
  behavior. The detached Git directory and sync journal must also remain in private storage.
- Use `vulcan daemon status` before diagnosing automatic work. It reports each registered wiki's
  reconstructed daemon state, latest daemon-supervised attempt, and path. It also lists hosted
  sync, trigger, alert, conflict, semantic, and companion-listener services with
  lifecycle state and restart counts; disabled services remain visible, and a sanitized last
  failure explains degraded or exhausted workers. It is an offline inspection and does not fetch. A stopped daemon does not prevent
  direct `sync status`, `sync doctor`, or `sync run`; direct runs do not enter the daemon job
  ledger, so their time is not the daemon's `last_attempt_unix_ms`.
  If runtime metadata exists but the authenticated capability probe fails, treat the daemon as
  unresponsive and inspect the reported probe error and service logs; do not describe it as stopped
  or start a duplicate process.
- Pausing a registered wiki disables only its sync observation consumer and periodic sync work.
  The shared filesystem observer and incremental cache-index consumer remain active, so local
  search and other cache readers can stay current without publishing device-sync state.
- Use `vulcan sync pause [<wiki>] --dry-run` and then the same command without `--dry-run` only when
  the user wants to stop future automatic triggers. Manual direct operations remain available.
- Treat `vulcan daemon stop` as a final syncing shutdown. It queues one retained `shutdown` job per
  active Git-backed wiki, waits up to 30 seconds, and cooperatively cancels unfinished work before
  exiting. Ctrl-C and graceful service-manager termination use the same path; a detected suspend
  gap queues a `resume` sync after wake. Paused and non-Git registrations stay excluded. Never claim
  this guarantees publication through abrupt power loss, forced termination, or an offline remote.

## Inspect before mutation

1. Run `vulcan sync status [<wiki>]` to inspect layout, safety state, the checked-out branch and
   its upstream, and observed refs without mutation. Human output calls this a **sync preview**:
   “inspected” means the read-only check completed, not that synchronization succeeded. It does not
   fetch or merge; use `--output json` for observed refs and branch detail. Registered previews also
   count retained conflict records; an unavailable conflict count makes the inspection incomplete
   instead of implying zero.
2. Run `vulcan sync doctor [<wiki>]` when installation, detached storage, hidden refs, filters/LFS,
   platform compatibility, locks, journals, apply markers, or cache coherence may be involved.
   A `sync.device-files` warning (or the matching `hint:` from `sync run`/`sync status`) means
   Obsidian's per-device layout files are synced; preview `vulcan sync doctor [<wiki>] --fix
   --dry-run`, then apply it with `--fix`. The fix adds `.gitignore` entries and removes the files
   from the index (keeping them on disk); commit that change, since sync carries only commits.
   Devices that pull it lose their copy, which Obsidian recreates. Sync never edits `.gitignore` on its own; setting a
   vault up with `vault add`/`vault clone`/`sync clone` adds the entries unless
   `--keep-device-files` is passed.
3. Preview a finite transaction with `vulcan sync run [<wiki>] --dry-run`; apply it by omitting
   `--dry-run` only after the target remote, branch, and diagnostics are understood.
4. Every Git and Git LFS subprocess is bounded to 300 seconds by default. For a known slower
   transport, set `--git-timeout-seconds <seconds>` on `sync run`; do not raise it merely to hide a
   stuck credential helper, filter, or remote. A timeout preserves the captured snapshot and
   recovery journal, so diagnose the named phase and rerun safely.

Interactive human `sync run` uses one transient phase line and clears it before printing the compact
result. Add `--verbose` for durable phase-by-phase diagnostics, including retries. `--quiet`,
structured output, and redirected runs suppress progress chatter; JSON remains isolated on stdout.
Every Git subprocess still has the configured timeout, so a stuck phase fails with a named,
recoverable diagnostic instead of waiting forever.

Interpret `paused` as preserved work, not failure: an in-progress Git operation, unexplained HEAD
movement, or a branch that sync could not integrate on its own stopped the cycle with nothing lost.
Resolve that ordinary Git state and rerun. `offline` likewise keeps local commits for the next
cycle. A successful sync that only failed to refresh the rebuildable cache reports a
`cache_refresh_error` warning instead of failing; the vault is fully synced and the next refresh
heals the cache. Never delete journals, apply markers, or `refs/vulcan/**` to make a status look
clean.
If a retained journal includes an error, daemon and companion status project `error` even though
the journal keeps the exact failed phase for recovery. Do not interpret a stale-looking phase name
inside diagnostics as an operation that is still running. Daemon jobs preserve the sync engine's
typed failure category and retryability through the application layer: network/authentication
failures can project offline, while repository/configuration/invariant failures remain errors with
different repair guidance rather than being flattened into retryable `unknown` failures.
Sync journals and apply markers from older installations are read as recovery witnesses by current
Vulcan; rerun sync to recover them rather than deleting the files.

## What sync moves

Sync moves only committed history. Each non-dry-run cycle snapshots this device's worktree into its
local safety ref, fetches, integrates the checked-out branch with its upstream, and pushes it. The
checked-out branch needs an upstream on the remote: a cycle without a checked-out branch or upstream
fails as unsupported configuration, so push the branch once with `git push -u`. Uncommitted edits
never travel; commit them (auto-commit, `vulcan git commit`, plain `git commit`, or MCP
`git_commit`) for them to reach other devices. The daemon syncs a vault when a commit moves a
branch, not when a file is saved. `sync.worktree` is obsolete: `"none"` is accepted as a no-op, and
settings from the removed whole-worktree mode are ignored with a config diagnostic. Hidden live refs
left on a remote by older versions are ignored harmlessly.

- Integration follows the vault's `sync.branch_pull` (`ff-only`, `merge`, `rebase`, or
  `guarded-rebase`) or, when that is `git` (the default), the repository's own pull configuration
  (`pull.ff`, `pull.rebase`, `branch.<name>.rebase`) with `--no-edit` and no implicit autostash. In
  a repository where agents commit, prefer `sync.branch_pull = "guarded-rebase"` (or the stricter
  `"ff-only"`) in the shared config: sync replays only commits that no other branch or worktree
  builds on and never leaves a conflict behind.
- Watch the human output or the JSON `branch` report for `fast-forwarded`, `merged`, `rebased`,
  `paused`, or `deferred` (an uncommitted or staged change in a file the incoming commits touch,
  retried next cycle; dirty files the incoming commits do not touch are fine).
- A paused cycle with reason `branch_paused` means the branch diverged under fast-forward-only
  pulls (`sync.branch_pull = "ff-only"` or `pull.ff=only`), a conflicted pull was aborted (the
  checkout is left as it was), or an interactive rebase was requested. The report's `branch.pause`
  (and an alert's `branch_pause`) names which; `shared_commits` and `merge_commits` call for a merge
  (`git pull --no-rebase`), the others for `git pull --rebase` with any conflict resolved, then push.
  If pushes or fetches fail only for Vulcan or only for plain `git`, run `vulcan sync transport test`.
- After a healthy pull the branch is published to its upstream (unless `sync.branch_push = false`,
  in which case push deliberately) with the observed tracking ref as an exact lease, never
  force-pushed. A moved remote reports for the next cycle; transport or policy failures record
  `push_detail`, and `pushed` tells whether publication happened. A daemon job with either branch
  failure is terminally `failed`, with status `error`, which keeps aggregate status and companion
  failure notices truthful. Human output prints `push_detail` whenever branch publication was
  rejected or failed.
- `sync.mirrors` (a list of Git remote names, set in the local config) keeps backup remotes up to
  date: after a healthy cycle each receives the tip the primary remote has, fast-forward only and
  never forced. `branch.mirrors` reports `pushed`, `up_to_date`, `rejected` (the mirror has commits
  of its own; reconcile it by hand), or `failed` per mirror without failing the cycle, and the
  daemon alerts `sync_mirror_behind` after 30 minutes. In a Git LFS clone, sync first fetches from the primary the LFS
  objects the mirror's new commits need; a `failed` detail starting "cannot fetch the Git LFS
  objects" means the primary lacks them or is unreachable, not that the mirror refused the push.
- Caveats: `rebase.autostash` is neutralized (automation never stashes implicitly); a
  `commit.gpgsign` setup needs a working agent or unattended merges fail loudly; triangular push
  remotes (`branch.<name>.pushremote`, `remote.pushdefault`) are not honored — the pull upstream
  receives the push; long-lived staged changes keep `pull.rebase` users deferred until they commit.

## Recover another device's work

Every non-dry-run cycle snapshots the complete worktree, uncommitted edits included, into a local
per-device safety ref before pulling, merging, or rebasing. It is pushed to the device's remote
safety head only with `sync.publish_device_backup = true`. The snapshot protects local bytes; it
does not carry them to other devices. The moving head retains earlier device history; if restored
checkouts accidentally reuse one device ID and diverge, Vulcan publishes a bridge that keeps both
histories reachable and warns. A single bridge after a re-clone or Git directory recovery is
expected; if the warning repeats, two checkouts on one device or two installations with a copied
device key share the ID. Do not delete either history: remove the extra checkout, or run
`vulcan device replace` on the other installation. Sync JSON (`device_id`) and daemon wiki status
name the device.

- Run `vulcan sync devices list [--wiki <id>]` to inventory remote safety heads, whether their
  local recovery copies are missing, current, or stale, retained local copies without remote heads,
  and named devices without backups. If `remote_observation.state` is `unavailable`, remote backup
  presence is unknown; use the displayed local recovery refs and labels as local evidence only. A
  fetched local copy is not proof that a backup is integrated or safe to prune. The list shows
  friendly names when set and exact IDs and revisions. Use
  `vulcan sync devices set-name <device-id> <name>` to save a
  shared vault label, or `clear-name <device-id>` to remove it; both support `--dry-run`. Labels
  travel with the vault when `.vulcan/device-names/` is tracked by Git, while refs and recovery
  commands always use the full device ID. These refs are recovery inputs, never merged
  automatically merely because they exist.
- Treat `identity_kind: ssh_key_v1` as the grammar of a key-shaped backup ID, not proof that the
  listed device possesses that key, is trusted, has access, or is online.
- Run `vulcan sync devices fetch <device-id> [--wiki <id>] --dry-run`, then omit `--dry-run` to
  anchor that device's backup under a durable local recovery ref. Review the reported paths and
  diff the recovery ref against your branch. Use the printed detached `git worktree add` command to
  recover a lost or inaccessible device's work in isolation, and publish what you keep as an
  ordinary commit.
- Prune a remote safety backup only after its work is integrated. First fetch it again, then run
  `vulcan sync devices prune-backup <device-id> --dry-run`. The `remove` spelling remains a
  compatibility alias. Pruning refuses the current device, stale recovery refs, and any backup the
  published branch (or a legacy live ref, where one remains) does not already contain. Applying the prune uses an exact lease and
  keeps the local recovery ref. It deletes only the remote backup ref;
  device, Git, and vault access are unchanged.
- When the backup is published, a safety-head publication failure stops the cycle. Do not delete
  the head or bypass this gate just to make sync green; restore remote write access or select the
  correct remote, then rerun.

## Vaults nested in a larger repository

A vault can be one directory of a bigger Git repository, such as the `docs/` directory of an
MkDocs site; `vulcan status` shows `git_repository_root` and `git_vault_prefix`. Sync pulls and
pushes the enclosing repository's checked-out branch, so commits anywhere in the repository travel,
and the safety snapshot covers the whole work tree. It reads sync configuration and takes the write
lock from the vault, keys its journal and conflict records on the repository root, and refreshes the
vault cache after pulling. Before syncing a code repository, confirm that the user wants its branch
pulled and pushed automatically.

Registration and cloning follow the repository's `.vulcan.toml` pointer: `vulcan vault add <id>
<repo>` or `vulcan vault clone <url> <dir>` registers the pointed vault directory as the wiki
`path` and records the repository root as `work_tree`. Commands then work from anywhere in the
checkout. Use `vulcan init --repository-pointer` inside the vault to create the pointer and commit
it with the repository. Never hand-edit a pointer to name a directory outside the repository.

## Retained conflict records

- Sync no longer creates file-level conflict records; a branch it cannot integrate pauses instead.
  `vulcan sync conflicts` lists records retained from the removed whole-worktree mode and
  `vulcan sync conflicts <conflict-id>` shows one; page a large record with `--path-offset <n>
  --path-limit <1-256>`, reading `path_page.total` and `path_page.next_offset`. These records stay
  readable and archivable but can no longer be resolved: recover any bytes worth keeping from the
  record's base/local/remote refs with ordinary Git and commit them. Never delete those refs or
  `refs/vulcan/conflict-archive` by hand. Archived conflicts stay readable with
  `vulcan sync conflicts <id>` (listings show `archived_count`). To archive sooner, preview
  `vulcan sync conflicts-archive --older-than-days <n> --dry-run`, then rerun without `--dry-run`.
  Unresolved records keep `sync status` reporting the vault as conflicted; once their bytes are
  recovered or not needed, `vulcan sync conflicts-archive --include-open` archives them too. Ask
  the user before archiving open records.
- Use JSON `operational_stats` (`git_subprocesses`, `elapsed_ms`, `backend_cycle_ms`,
  `cache_refresh_ms`), or the concise `--verbose` human line, to assess one cycle's cost. These are
  content-free observations for that cycle.
- The daemon gives an initial retryable repository-lock (`busy`) failure one durable recovery
  cycle before notifying. If that recovery is also busy, it records and delivers the failure
  normally instead of retrying forever.

## Daemon and Obsidian companion

### How quickly other devices' changes arrive

- The daemon checks each Git wiki's remote on an adaptive schedule; there is no forge webhook,
  notification relay, or advertisement ref to set up. After activity it checks again after
  `sync.poll_min_seconds` (default 15) and backs off to `sync.poll_max_seconds` (default 900),
  as described under the daemon watcher notes below. Only committed work arrives this way.
- MCP sessions and `vulcan lsp` already ask the daemon to sync the wiki when they start. Otherwise,
  before working in a Git-synced wiki, run `vulcan sync refresh` (inside the vault, or name the
  wiki; `--all` for every registered wiki). It asks the daemon for an immediate sync unless one
  ran in the last 30 seconds, waits up to `--wait-seconds` (default 15), and prints each wiki's
  state. A `sync paused` line with diverged local commits means: run `git pull --rebase && git push`
  in the wiki. Without a daemon it syncs directly; it never exits non-zero for a sync outcome.
  `vulcan agent print-config --runtime <harness>` prints a session-start hook that runs it.
- An agent working only through MCP uses the `sync` pack: `sync_refresh` starts a cycle now (to
  publish just-committed work or pick up others' changes), and `sync_resume` clears a paused
  registration. Check `sync_status` afterwards; a paused branch still needs the fix its alert names.
- Repositories that still carry a `refs/vulcan/notifications` ref from the retired wake-up
  advertisement can drop it with ordinary Git (`git push origin :refs/vulcan/notifications`);
  Vulcan no longer reads it.

- Prefer `vulcan daemon install --dry-run` followed by `vulcan daemon install` for a persistent
  per-user service on Linux, macOS, or Windows. Linux installs a restartable `systemd --user` unit,
  macOS installs a restartable per-user LaunchAgent, and Windows installs a logon task whose trigger
  and least-privilege interactive-token principal are bound to the current user SID. Windows
  installation needs no Administrator terminal or stored password, runs the registered task
  immediately, and uses detached start. On supported Windows 11 24H2 or newer releases, the single
  `vulcan.exe` embeds the detached console-allocation policy, so the task creates no transient or
  long-running terminal window. Background synchronization also starts every Git subprocess in
  Windows no-window mode, including inherited helpers, so a sync cycle does not flash Git terminal
  windows; no separate launcher binary is needed. Uninstall with
  `vulcan daemon uninstall --dry-run` and then the reviewed mutation.
  Uninstalling the service does not remove registrations, credentials, journals, conflicts, or
  vault data.
- Services and direct foreground startup read optional provider credentials from the device-local
  `$XDG_CONFIG_HOME/vulcan/daemon.env` file (normally `~/.config/vulcan/daemon.env`). Keep it mode
  `0600` on Unix, use literal `NAME=value` entries, and never place it in a vault. Existing process
  environment variables take precedence, and the file does not execute or expand shell syntax.
- Start the daemon explicitly with `vulcan daemon start` or `--detach`. On Linux, macOS, and
  Windows the running daemon owns watchers and adaptive remote polling; watcher, startup, poll,
  and companion triggers coalesce through one per-wiki supervisor.
- Daemon sync watchers use native filesystem notifications when available. Content-comparing
  polling runs only if native watcher setup fails, with a 30-second interval; it can be costly
  on large worktrees. Remote checks are adaptive per wiki: after activity (a local edit or
  commit, a manual sync, or a check that found new remote revisions) the daemon checks again
  after `sync.poll_min_seconds` (default 15), doubling the wait after each quiet check up to
  `sync.poll_max_seconds` (default 900). Raise the maximum for rarely changing wikis rather than
  disabling sync. To request reconciliation immediately, use `vulcan sync run <wiki>`, for
  example before starting work that depends on another device's latest commits.
- Companion connections share one status stream. Daemon jobs and successful companion mutations trigger updates; changes made by external processes are reconciled every 30 seconds while a companion is connected. Use `vulcan sync status <wiki>` for an immediate status read. With no companion connected, the stream performs no background snapshot work.
- The daemon is quiet by default. Run `vulcan -v daemon start` (or with `--detach`,
  which carries the level and `--color` to the background child and its `daemon.log`; an
  installed service reads `VULCAN_VERBOSITY=<level>` from `daemon.env` instead) for operational stderr
  lines: one per completed sync job with wiki, triggers, state/outcome, watch-trigger detail,
  and branch-lane action. At the default quiet level, failed jobs
  print one secret-minimal line per wiki/category state with retryability and a direction to inspect
  retained status; error text remains on the authenticated status surface. Paused
  jobs emit warning records, failed jobs emit error records, and branch pull-strategy and push
  failures also keep their specific diagnostic. Attention states deduplicate per wiki until recovery,
  so installed services make persistent trouble visible without flooding their logs. To add native
  desktop delivery, preview and apply
  `vulcan daemon config set-notifications --desktop true --dry-run` and then without `--dry-run`,
  then restart the daemon. Native desktop/Android daemon notices include the wiki, error category,
  retryability, job ID, and a status command; the full error remains on authenticated status.
  To quiet transient network failures, add `--network-mode ignore`, or choose
  `--network-mode count --network-count 3` or
  `--network-mode duration --network-minutes 15`. These settings affect only native daemon
  delivery; logs and remote sinks retain their existing event policy. Count and duration show one
  native alert per observed network outage and reset after a successful or non-network result.
  Duration starts when the daemon first observes the failure, so a restart resets its timer;
  its delivery worker checks deadlines every 30 seconds. `vulcan daemon alert-status` shows the
  effective policy. The independently installed `sync termux-install` periodic wrapper still
  posts on any failed direct run; daemon notification settings do not control that wrapper.
  Desktop delivery is best-effort, timeout-bounded, and never changes the retained sync result;
  helper or desktop-session failures become warning log records. Its durable
  delivery identity surfaces a failure that already exists when desktop alerts are first enabled,
  but suppresses replay after successful delivery on later restarts.
  For remote delivery, configure a named JSON webhook or ntfy topic with
  `daemon config set-notification-webhook <name> --url <https-url> --format json|ntfy
  [--token-env <name>] --dry-run`. Apply and restart only after the preview is correct. The URL
  cannot contain credentials, a query, or fragment; keep bearer values in protected `daemon.env`.
  A shell-free `set-notification-command` adapter can feed the event JSON on stdin to an absolute
  local bridge such as the NATS CLI or an email gateway. It requires the wiki profile's execute
  capability; webhooks require network access to their endpoint. Inspect retry attempts and next
  delivery times with `vulcan daemon alert-status`. Remote events are durably retained and retried
  across daemon restarts, but delivery failure never changes the authoritative sync result.
- Provision a companion only from a running daemon. `vulcan daemon companion --output json` is
  non-secret; `--reveal-token` transfers bearer authority and must never be copied into a note,
  synchronized plugin settings, logs, or source control.
- Install or update the embedded Obsidian companion with `vulcan daemon companion install
  [<wiki>] --dry-run`, review the managed files and target, then rerun without `--dry-run`. Omit the
  wiki only when global `--vault` resolves to the intended registration. The installer seeds the
  non-secret endpoint/wiki settings only on first install and preserves later `data.json` choices;
  it never pairs the bearer token. Enable the plugin, then perform the explicit `--reveal-token`
  handoff into Obsidian `SecretStorage`.
- The reference Obsidian companion requests editor save, debounces completed writes, displays
  authenticated state, and previews conflicts. By default it shows one bounded notice per failed
  daemon job or retained failed transaction, deduplicated by durable job/transaction identity
  across its live event stream and 30-second polling fallback. Network failure notices can be
  ignored, delayed until a configured number of distinct failed jobs, or delayed until a failure
  remains observed for a configured duration. Those delayed policies notify once per continuous
  network outage; recovery resets them. The status dialog retains job ID, category, retryability,
  and full error text even when a notice is suppressed. A temporary companion connection failure
  changes the status bar to offline but is not labeled as a sync failure. The master setting can
  disable notices without changing authoritative status. It is not a second Git engine. Do not run
  it beside another independent Git-sync plugin against the same worktree.
- Provider endpoints/models are daemon configuration, not companion request fields. Use
  `vulcan daemon config set-agent resolution|semantic ... --dry-run`, keep key values in the named
  environment variable, apply deliberately, and restart the daemon.
- For a designated unattended resolver, configure `vulcan daemon config set-agent resolution`, then preview and apply `vulcan daemon config set-conflict-worker --wiki <id> --poll-seconds <n> --dry-run`. Restart and inspect `vulcan daemon conflict-status`. When the vault's `sync.branch_pull` is `guarded-rebase` or `rebase`, the worker replays a branch whose rebase stopped on a conflict, asking the agent per conflicted commit in a scratch worktree, and leaves publication to the next sync; `conflict-status` shows `replayed_branch` or `declined_replay` with the reason. It never touches shared commits, merges, deletions, non-text files, or uncommitted edits, so a declined replay still needs `git pull --rebase` by a person. Configure only one resolver node per vault.

## Android and detached Git data

- Install a checksummed, full-feature `aarch64-linux-android` release through the version-matched
  POSIX installer; never substitute the `aarch64-unknown-linux-gnu` archive, which targets a
  different runtime. A Termux source build needs Clang, `rquickjs/bindgen`, and the documented
  AArch64 SLP-vectorizer workaround; do not disable QuickJS, OAuth, vectors, or web merely to make
  the Android build compile.
- Under Termux, keep the worktree in shared storage and the detached Git directory in Termux-private
  storage. `vulcan sync clone <remote> <shared-path> --dry-run` selects both defaults automatically.
  The SQLite cache is rebuilt in Termux state after `vulcan init` or `vulcan scan`; an older
  `.vulcan/cache.db` in shared storage is left untouched but no longer used on Android.
- After a successful clone, an ownership rejection from Git automatically adds only the canonical
  new worktree path to the user's global `safe.directory` entries and retries discovery. This also
  applies to `vault clone`; dry runs do not change Git configuration. Existing repositories are
  never automatically trusted by discovery, status, or sync.
- One-shot `sync status`, `sync doctor`, and `sync run` are the supported baseline and require no
  daemon. Android shared storage cannot faithfully represent executable bits or symlinks.
- The daemon's own sync cycles (poll, change, resume) run unconditionally
  by default; `vulcan daemon config set-automatic-sync --network unmetered --power battery-not-low
  --dry-run` previews holding them back on metered networks or low battery. A held-back cycle is
  reported as paused with "deferred by this device's background policy" and retried at the next
  poll; it is not a failure and needs no repair. `sync run`, `--queue`, recovery, and shutdown syncs
  are never held back. Change these conditions only when the user asks.
- For an Android-managed periodic safety net, preview `vulcan sync termux-install <wiki>
  --period-minutes 60 --dry-run`, then apply only after checking the job ID, wrapper path, and
  network/battery policy. It requires Termux:API plus `pkg install termux-api`, defaults to
  battery-not-low and storage-not-low, and never starts the daemon. Use `--network unmetered` or
  `--charging` when requested. Set `--network-notification-mode ignore`, or use
  `count --network-failure-count 3` or `duration --network-failure-minutes 15` to delay
  network-only notices. Preview `sync termux-uninstall <wiki> --dry-run` before removal.
- Inspect saved settings with `vulcan sync schedule show <wiki>`. Change only the interval with
  `vulcan sync schedule set <wiki> --period-minutes 30 --dry-run`, then apply without `--dry-run`.
  Updates retain the same Android job ID and all unspecified settings. Use `--network unmetered`,
  `--charging true|false`, `--battery-not-low true|false`, `--persisted true|false`, or
  `--network-notification-mode` and its threshold flags for explicit changes. The minimum
  interval is 15 minutes. These commands manage an existing job; create it
  with `sync termux-install` first. Saved settings do not prove Android still has the job queued;
  use `termux-job-scheduler --pending` to inspect Android's actual queue.
- Prefer scheduled, finite sync runs over a long-running daemon on Termux. When a daemon is
  installed through `sync termux-install`, a non-network error posts a high-priority
  `termux-notification` immediately. Network errors follow the wrapper policy and a later
  successful run removes the notice and resets the private failure counter. When a daemon is
  deliberately kept running, `vulcan daemon config set-notifications --desktop true` also uses
  `termux-notification` for sync-attention alerts and requires the Termux:API companion app plus
  the `termux-api` package. Notification failure does not change retained sync state.
- Treat Android JobScheduler timing as approximate. Use the periodic job as an energy-efficient
  safety net; a shortcut or future save/resume bridge may invoke the same finite `sync run` for
  lower latency. A foreground or persistently supervised Termux daemon polls adaptively like any
  other daemon, but it never replaces JobScheduler reconciliation.
  Verify unattended Git credentials manually and never embed secrets in the managed wrapper or
  vault.
- If private Git data is lost, preview `vulcan vault recover-git <wiki> <remote> --dry-run`.
  Recovery captures the untouched materialized vault before fetching, but cannot reconstruct
  unpushed objects that existed only in the deleted Git directory.

## Non-negotiable safety properties

- Current local bytes are snapshotted to the device safety ref before the branch is pulled,
  merged, or rebased.
- A remote update uses an exact lease; never replace a rejected push with unconditional force.
- The user's normal index is not sync scratch space: sync never stages, resets, or rewrites staged
  entries, and defers a pull that would touch them.
- Uncommitted edits never leave the device through sync; do not tell a user their unsaved or
  uncommitted work has synced.
- Treat policy, platform, stale-input, and worktree-drift failures as reasons to preserve and stop.
  Do not bypass them to make synchronization appear seamless.
