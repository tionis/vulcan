# Git-backed device synchronization

Vulcan synchronizes a registered directory's committed Git history between devices. Each `vulcan sync run` is a finite, direct transaction: it snapshots the current worktree into a device-local safety ref without touching the normal Git index, fetches the checked-out branch, fast-forwards or reconciles it with its upstream, and pushes local commits. Sync never commits your edits; people, agents, and Vulcan's auto-commit do. Knowledge-profile sync also refreshes an existing cache. The daemon schedules the same workflow but is not required.

Registrations live in the device-global `daemon.toml` (`~/.config/vulcan/daemon.toml` on Linux), which you may also edit by hand: CLI commands keep its comments and formatting. The identities Vulcan generates for registrations and named remotes are kept in `daemon.identities.toml` beside it, because API tokens, scan state, and OAuth grants refer to them. Do not edit that file or copy it to another device. A `daemon.toml` from an older version that still carries `registration_id` or `instance_id` lines is migrated on first use: the identities move to `daemon.identities.toml` and only those lines are removed. Going back to an older version afterwards would generate new identities and break existing API tokens and grants. To set up a device from one file, add `source` to each `[[vault]]` and run `vulcan daemon apply`, which clones missing vaults; see [Declarative device configuration](device-config.md).

### Managed-directory profiles

Registered directories use a versioned, device-local capability profile. `knowledge` is the backward-compatible default. Choose `files-only` with `vulcan vault add <id> <path> --profile files-only`, `vulcan vault clone <remote> <path> --profile files-only`, `vulcan sync clone <remote> <path> --profile files-only`, or `vulcan vault set <id> --profile files-only`. A registration may also use `vulcan vault add <id> <path> --no-sync` when only device-local management is wanted; a directory that is not a Git repository is registered that way unless `--sync-backend git` or `--git-init` asks for Git. `vulcan vault set <id> --sync-backend git` or `--no-sync` switches an existing registration without removing it, which would drop its API tokens and `--serve-remote` opt-in. Registration and profile changes do not initialize or change the vault's `.vulcan/config.toml`.

A Git-sync registration needs a Git repository. `vulcan vault add` on a plain folder still registers it but says it is not a repository yet and skips device-key enrollment; `vulcan vault add <id> <path> --git-init` creates the repository first when the path is not inside one. It makes no commit and adds no remote, and if the registration is refused (a taken ID or an already registered path) no repository is created. Add the `origin` remote yourself, then run `vulcan vault enroll <id>`. `vault add` records an existing `origin` as the entry's `source` in `daemon.toml`, which lets `vulcan daemon apply` clone the vault on another device; for a remote added after registration, add `source` to the entry yourself.

To find vaults you have not registered yet, `vulcan vault discover <dir>` looks at the immediate subdirectories of `<dir>` and reports the ones holding `.obsidian/`, `.vulcan/`, or a `.vulcan.toml` pointer, each with a free suggested ID and the `vulcan vault add` command to run. Already registered vaults are listed with their ID. Remember a parent folder with `vulcan vault scan-dir add <dir>` (stored as `scan_dirs` in `~/.config/vulcan/daemon.toml`) so that a bare `vulcan vault discover` scans it; `vault scan-dir list` and `vault scan-dir remove` manage the list. Discovery never registers anything itself.

The `files-only` profile allows arbitrary files to be managed without initializing or refreshing a Markdown index. It disables knowledge services, scripts, and agent workers for that registered path. Knowledge commands are rejected while the path is registered as files-only; switch back with `vulcan vault set <id> --profile knowledge` when it should act as a knowledge base again. Both profiles currently use full-tree materialization. Capability reports expose the versioned profile and materialization policy; files-only doctoring skips cache checks and reports repository safety. Registration JSON for existing knowledge profiles keeps its previous shape.

The daemon's unattended files-only preflight rejects detached HEAD, staged changes, multiple linked worktrees, in-progress Git operations, and nested repositories or submodules. It also checks the branch again before applying synchronized files. Vulcan's repository lock does not exclude external Git processes, so do not switch branches or run Git operations while an unattended sync job is active. Active development checkouts are not a supported unattended-management workflow; use explicit manual synchronization only after reviewing repository state.

Sync replicates committed history; it is not by itself a complete backup. Complete backup needs verified retention and restoration for refs, history, and Git LFS payloads, which the current sync contract does not promise.

Selective materialization remains planned in [Roadmap 12.20](../ROADMAP.md#1220-selective-materialization-and-large-repository-synchronization). Sparse checkout, partial clone, shallow-history reconciliation, and selective LFS hydration are not currently supported sync combinations. Existing Git/LFS filter checks do not establish those guarantees. The future model preserves excluded paths and reports metadata synchronization separately from local payload availability.

Authentication over SSH uses this installation's device key by default and is set up automatically when you clone or add a vault: see [Device keys](device-keys.md) (`vulcan help device-keys`).

This is device synchronization. It replicates the repository's committed history rather than selecting or translating notes for an external wiki.

On graceful daemon shutdown, Vulcan queues a final retained sync for every active Git-backed wiki
before stopping its workers. This covers `vulcan daemon stop`, foreground Ctrl-C, and normal
service-manager termination, including the termination signal normally sent during an orderly OS
shutdown. Final jobs have a 30-second drain window and use the ordinary recoverable transaction,
permissions, leases, and safety checks; Vulcan never force-pushes or discards a conflict merely to
finish shutdown. Paused and non-Git registrations are not overridden. If the platform suspends
without a reliable pre-sleep notification, clock-gap detection queues a resume sync immediately
after wake. Abrupt power loss, hard process kills, and offline shutdown cannot guarantee that bytes
reach the remote, so the normal watcher remains the primary path and final sync is a last-chance
safety net.

The daemon checks each Git wiki's remote on its own adaptive schedule, one `ls-remote`-sized round
trip when nothing changed. After any activity (a local edit or commit, a manual sync, or a check that
found another device's commits) it checks again after `poll_min_seconds` (default 15), then doubles
the wait after each quiet check up to `poll_max_seconds` (default 900). A busy wiki is therefore
current within seconds, and an idle one costs a few checks an hour. Both settings may sit in the
shared `.vulcan/config.toml` or a device's `.vulcan/config.local.toml`:

```toml
[sync]
poll_min_seconds = 15   # never below 5
poll_max_seconds = 3600 # an archive wiki rarely needs checking
```

An idle wiki may be up to `poll_max_seconds` behind when someone starts working in it, so whatever
starts work should say so first: `vulcan sync refresh` (in the vault, or with a wiki ID, `--group`,
or `--all`) tells the daemon. The daemon syncs right away unless a sync is queued, running, or
started in the last 30 seconds, polling returns to the shortest interval, and the command waits up
to `--wait-seconds` (default 15) to report each wiki, including a paused branch and how to continue.
Without a running daemon it syncs directly. It never fails because of a sync outcome, so agent
session hooks can run it unconditionally.

For installation, service management, and upgrade behavior on each supported platform, see [Installation](../installation.md). The daemon is optional: install its native per-user service with `vulcan daemon install --dry-run` followed by `vulcan daemon install` only when background synchronization is wanted.

## Ordinary Linux, macOS, and Windows setup

Preview a clone and device-local registration first:

```sh
vulcan --output json vault clone <remote> <vault-path> --id personal --dry-run
```

Then create it and run a finite sync:

```sh
vulcan vault clone <remote> <vault-path> --id personal
vulcan sync doctor personal
vulcan sync run personal
```

For an SSH remote, the clone also enrolls this device's key (authorize, bind, register) and prints what it did or exactly what is still `pending`; see [Device keys](device-keys.md). Use `--no-device-key` to keep your own SSH setup.

The default colocated layout keeps `.git/` beside the vault. `sync run` does not start the daemon. It also works against an unregistered repository selected with `--vault <path>`.

To have a running daemon do the work instead, add `--queue`: `vulcan sync run --group work --queue` queues one retained sync job per wiki in the group (also for one wiki ID, `--all`, or the registered vault at the selected path) and returns immediately with the jobs' IDs. `vulcan daemon status` shows their progress. Queued jobs run with each registration's stored permission profile and sync settings, so `--queue` refuses `--permissions` and the run-tuning flags, and it fails rather than syncing directly when no daemon is running.

### Network and power conditions

Each way of syncing has its own default:

- **Finite runs** (`vulcan sync run`, also with `--queue`) always run: you asked for them.
- **The daemon's own cycles** (adaptive polls, file changes, resume after sleep) run under no conditions by default, because the daemon is what moves edits off a device and holding it back on a low battery risks losing the last ones. A device can opt in: `vulcan daemon config set-automatic-sync --network unmetered --power battery-not-low` (add `--defer-on-unknown` to also wait when a reading is unavailable; run it without flags to remove the conditions). A held-back cycle shows as paused with the reason in `vulcan daemon status` and is tried again at the next poll or change; since every cycle captures the whole working tree, nothing is lost. Recovery and the final sync at shutdown are never held back.
- **Android/Termux jobs** use the conditions given to `sync termux-install` (battery not low and storage not low by default, optionally unmetered or charging), which Android itself enforces.

Network cost is read from NetworkManager on Linux; battery state from the system on Linux, macOS, and Windows. Where a reading is unavailable it counts as satisfied unless `--defer-on-unknown` is set.

## Android and Termux

Android shared storage is suitable for the Obsidian-visible files but not for Git's private repository machinery or SQLite WAL. Keep the worktree in shared storage; Vulcan places the detached Git directory, rebuildable cache, vault write lock, mdbase transaction state, and integration/Outline state in Termux-private storage. Existing route and Outline state is copied there on the first write, with the older files left intact. An existing mdbase transaction is copied under the vault lock before recovery. After upgrading from a build that kept `.vulcan/cache.db` in shared storage, run `vulcan scan` to build the private cache. The old shared cache is not used and can be removed after the rebuild succeeds. Mdbase canonical file writes require directory sync on the shared worktree; if the Android filesystem rejects it, Vulcan refuses the write before creating a transaction.

After granting Termux storage access and installing Git, preview the layout. In Termux,
`sync clone` automatically keeps Git metadata in the private Vulcan data directory and selects the
`android-shared` policy:

```sh
termux-setup-storage
pkg install git
vulcan --output json sync clone <remote> /storage/emulated/0/Documents/Personal --dry-run
```

The wiki ID defaults to `personal` from the destination name. `--id`, `--group`, `--git-dir`, and
`--platform` remain available when an override is needed. Apply the same command without
`--dry-run`, then use one-shot commands whenever synchronization is wanted:

```sh
vulcan sync status personal
vulcan sync doctor personal
vulcan sync run personal
```

These commands work from an interactive Termux shell, a Termux shortcut, or an explicitly configured scheduler. They neither require nor implicitly start the Vulcan daemon. Android lifecycle and battery integration are packaging concerns layered over this same finite command.

For a low-frequency, energy-aware safety net, install Termux:API from the same source/signing family
as Termux and install its command package with `pkg install termux-api`. Then preview the managed
Android JobScheduler entry:

```sh
vulcan --output json sync termux-install personal \
  --period-minutes 60 \
  --network any \
  --dry-run
```

Apply it by omitting `--dry-run`. The default job survives reboot and runs only with a usable
network, non-low battery, and non-low storage; add `--network unmetered` or `--charging` for a more
restrictive policy. Network sync failures can be ignored with
`--network-notification-mode ignore`, or reported once after consecutive failures with
`--network-notification-mode count --network-failure-count 3`, or after an observed duration with
`--network-notification-mode duration --network-failure-minutes 15`. Non-network errors still
notify immediately. Change the saved policy later with `vulcan sync schedule set personal`; the
private failure counter resets on recovery or a non-network error. Duration is checked on each
scheduled run, so Android may deliver the alert after the configured time. Android periodic jobs
are approximate and have a 15-minute minimum, so this is
a safety net rather than realtime delivery. A Termux shortcut or future Obsidian/native wake bridge
can call `vulcan sync run personal` after save or resume for lower latency; overlapping invocations
still enter Vulcan's ordinary per-repository transaction serialization.

Preview removal before cancelling the Android job and deleting only its managed private wrapper and
manifest and network failure state:

```sh
vulcan sync termux-uninstall personal --dry-run
vulcan sync termux-uninstall personal
```

The scheduled process inherits Termux's normal account environment, not an interactive SSH agent.
The device key is a file-backed key, so `vulcan vault enroll personal` (or the automatic enrollment
on `sync clone`) gives the scheduler working unattended authentication without an agent. Otherwise
configure unattended Git authentication in Termux itself. Verify it with a manual
`vulcan sync run personal` before relying on the scheduler. Do not put credentials in the wrapper,
vault, or command line.

The `android-shared` profile records the filesystem limitations instead of pretending it behaves like native Linux storage:

- executable bits are not representable;
- symlinks are checked out as link files;
- case-folding, Unicode-normalization, Windows-reserved-name, and path-length hazards are diagnosed before unsafe publication or application;
- case-only renames may require an intermediate path.

Do not place the detached Git directory in shared storage. Uninstalling Termux can remove its private objects and refs while leaving the visible vault behind. If that happens, preserve the vault and use `vulcan vault recover-git personal <remote> --dry-run` before applying recovery. Vulcan captures the surviving worktree first, but unpushed objects that existed only in the lost private directory cannot be reconstructed.

## Syncthing and other file synchronizers

Use one synchronizer per tree. Git sync and Syncthing on the same directory race each other, and
Syncthing also copies the `.git` directory, which corrupts repositories that change on two
devices at once. `vulcan vault add` warns, and `vulcan sync doctor` reports
`sync.file-synchronizer`, when a `.stfolder` marker sits at or above the vault. A folder that
only Syncthing replicates can still be a Vulcan vault: register it with `--no-sync` (the default
for a directory that is not a Git repository). `vulcan doctor` lists Syncthing's
`*.sync-conflict-*` copies under `sync_conflict_copies`; merge each into its original and delete
it.

## Per-device files

Obsidian stores each device's open panes in `.obsidian/workspace.json` and
`.obsidian/workspace-mobile.json`. Syncing them turns every pane change into a commit and spreads
one device's layout to the others.

Setting a vault up for Git sync with `vulcan vault add`, `vulcan vault clone`, or
`vulcan sync clone` adds `.gitignore` entries for these files when the vault has a `.obsidian`
directory, and says so in its output. Pass `--keep-device-files` to go on syncing them. Sync itself
never edits the `.gitignore`: while a vault still shares the files, `vulcan sync run` and
`vulcan sync status` print a hint, and `vulcan sync doctor` warns (`sync.device-files`). Repair an
existing vault with:

```sh
vulcan sync doctor --fix --dry-run
vulcan sync doctor --fix
```

`vulcan sync ignore-device-files [--dry-run]` does the same as a single step. Either adds entries
only for the files no shared `.gitignore` covers yet, and running it on one device is enough: the
`.gitignore` reaches the others once it is committed. Files that are already tracked stay tracked
until you remove them from the index (`git rm --cached .obsidian/workspace.json
.obsidian/workspace-mobile.json`) and commit; devices that pull that commit may lose their copy,
which Obsidian recreates. Only rules in committed `.gitignore` files count, not `info/exclude` or a
global excludes file.

## How a cycle works

Sync moves only commits. Before 2026-11-24 Vulcan could also snapshot the whole worktree into a
hidden live branch (`sync.worktree = "all"`, the former default) or only some paths (`"paths"`);
both modes were removed. `worktree = "none"` is still accepted and does nothing; `"all"`, `"paths"`,
and the old `sync.paths`, `sync.merge_policy`, `sync.merge_automation`, `sync.agent_auto_accept`,
and `sync.tree_validation` keys are ignored with a config warning, so delete them. Leftover
`__vulcan-sync/live` refs on a remote are ignored; nothing deletes them.

```toml
# .vulcan/config.toml (shared, committed)
[sync]
branch_pull = "ff-only" # optional: never merge or rebase for you; default "git"
# branch_push = false   # optional: fetch and fast-forward, but push yourself
```

Edits become history only when something commits them. People and agents with a shell commit
themselves; for edits made through MCP, the bot, or other tools that cannot run Git, enable batched
auto-commit, which always runs for synced vaults:

```toml
[git]
auto_commit = true
strategy = "batched"  # the daemon commits after the vault has been quiet
```

Each `vulcan sync run` snapshots this device's worktree into a local safety ref (never applied
anywhere), fetches the checked-out branch, fast-forwards it when the incoming commits do not touch
files you are editing, falls back to `sync.branch_pull` when it diverged, and pushes your commits.
The branch needs an upstream that exists on the remote; without one the cycle fails as unsupported
configuration. The snapshot stays in this repository, so half-finished edits never leave the device.
Set `publish_device_backup = true` in `[sync]` to also push the snapshot to this device's private
remote branch as an off-device backup. The daemon starts a cycle when a commit moves `HEAD` or a
branch, not for every saved file, and polls the remote as usual. `branch_push` may be set in either
file.

When sync cannot fast-forward because the branch diverged, it follows `sync.branch_pull`. The
default, `git`, uses each device's own `pull.rebase` and `pull.ff`, so a global `pull.rebase = true`
makes the daemon rebase your commits in the background. `ff-only`, `merge`, `rebase`, or
`guarded-rebase` fixes the strategy for every device, whatever their Git configuration says.

For repositories where agents commit, `guarded-rebase` resolves most divergences without anyone:
sync replays the local commits onto the upstream and publishes them, but only when no other branch,
tag, remote-tracking ref, or worktree contains them (so an agent's worktree never loses its base),
none is a merge, and the replay applies cleanly. Uncommitted edits hold it back only when the
upstream changes the same files: the commits are then replayed in a scratch worktree, so unrelated
edits stay as they are. While overlapping edits block it, the branch waits; after 30 minutes the
daemon alerts (`sync_branch_deferred`) so someone commits or discards them. Anything else pauses
the branch, with an alert that says to run `git pull --rebase` and push. `ff-only` is the strictest choice: sync never rewrites or
creates a commit and every divergence pauses. A merge or rebase that sync starts and that conflicts
is always aborted, leaving the branch as it was.

A conflicted rebase can also be settled by the resolution agent, on a device you choose: list the
wiki in the daemon's conflict worker (see [Unattended branch replay](#unattended-branch-replay))
and set `branch_pull` to `guarded-rebase` or `rebase`. When the branch is still diverged after a
sync, the worker replays the local commits onto the upstream in a scratch worktree and asks the
agent to resolve each conflicted commit. The branch moves only if every commit replayed, and a
sync then publishes it at once. The worker declines without changing anything when another ref or
worktree builds on the commits, the range has a merge, a side deletes a file or holds a non-text
file, the agent's answer still has conflict markers, or uncommitted edits would be overwritten. A
declined replay is not sent to the agent again until either side moves, and the branch stays
paused with its alert.

## Conflicts and recovery

Sync no longer records conflicts: a diverged branch it cannot reconcile pauses with an alert that
says how to fix it. Conflict records kept from the removed whole-worktree mode stay readable and
can be archived, but no longer resolved:

```sh
vulcan sync conflicts
vulcan sync conflicts <conflict-id>
vulcan sync conflicts-archive --dry-run
```

`sync conflicts-archive` moves records closed at least `--older-than-days` (default 30) ago into a
Git-reachable archive. Unresolved records keep `vulcan sync status` reporting the vault as
conflicted; read them, restore anything you still need from their preserved sides with ordinary
Git, and then archive them with `vulcan sync conflicts-archive --include-open`. Archived records
stay readable with `vulcan sync conflicts <id>`.

If a finite cycle is interrupted or the network is unavailable, rerun `vulcan sync run`. Device-local journals and Git refs retain the captured state; do not delete them or replace the vault with a fresh clone as a recovery shortcut.

## Background failure notifications

The daemon always writes one structured warning or error record when a registered wiki first
enters a paused or failed sync state. Repeated automatic attempts in the same state do
not flood the service log; a successful cycle resets the notification. Full error detail remains in
`vulcan sync status <wiki>` rather than logs or desktop messages.

A paused branch is announced too: when local commits could not be combined with the upstream, the
alert carries `reason: "branch_paused"` and `branch_pause`, which says why:
`fast_forward_only` (diverged under fast-forward-only pulls), `interactive_rebase`
(`pull.rebase=interactive`), `shared_commits` or `merge_commits` (a guarded rebase would strand
another branch or flatten merges), or `rebase_conflict` / `merge_conflict` (the attempt conflicted
and was undone). The message gives the matching fix (rebasing, merging instead, or resolving the
conflict by hand), then `vulcan sync refresh <wiki>`. The next cycle after that push clears it.

Jobs held back on purpose raise nothing: a paused registration (`vulcan sync pause`) or this device's
background policy (battery, metered network) needs nobody. A device backup that keeps receiving
history another checkout wrote raises `reason: "shared_device_identity"` once it has happened three
times in a daemon run: an extra checkout or a copied device key syncs under this device's ID.

Native desktop notifications are opt-in and require a daemon restart after configuration:

```sh
vulcan daemon config set-notifications --desktop true --dry-run
vulcan daemon config set-notifications --desktop true
vulcan daemon stop
vulcan daemon start --detach
```

Vulcan uses `notify-send` on Linux, `osascript` on macOS, and PowerShell on Windows without a shell.
Delivery runs on a bounded worker with a five-second helper timeout. If the helper or graphical
session is unavailable, the daemon logs that delivery failure and leaves the authoritative sync
result unchanged. Desktop attempts use the durable alert ledger too: enabling the setting after a
retained failure delivers that current state once, while later daemon restarts do not repeat an
already delivered alert. Disable it with `set-notifications --desktop false`.

For ntfy, configure the full non-secret topic endpoint and keep an optional access token in
`daemon.env`:

```sh
vulcan daemon config set-notification-webhook phone \
  --url https://ntfy.example.com/vulcan-alerts \
  --format ntfy \
  --token-env VULCAN_NTFY_TOKEN \
  --dry-run
```

The default `json` format POSTs the versioned event to a generic webhook, which can route to an
email service or another notification system. URLs must use HTTPS (or loopback HTTP), may not embed
credentials/query/fragment data, and redirects are not followed. The affected wiki's permission
profile must allow network access to the endpoint.

Local adapters receive the same JSON on stdin. For example, the NATS CLI reads a publication body
from stdin when no message argument is supplied:

```sh
vulcan daemon config set-notification-command nats \
  --program /usr/local/bin/nats \
  --arg pub \
  --arg vulcan.sync.alerts \
  --dry-run
```

Use an equivalent absolute, locally reviewed adapter for an email gateway. Vulcan never invokes a
shell, command arguments must not contain credentials, and each command delivery requires the
wiki profile's execute permission. After applying any sink change, restart the daemon. Inspect
configured sink names, pending jobs, attempt counts, and next retry times without exposing endpoint,
program, or token details:

```sh
vulcan daemon alert-status
```

Remote deliveries are recorded under the device state directory before dispatch, retried with
bounded exponential backoff across restarts, and identified by the durable sync job ID. Receivers
should deduplicate on the `Idempotency-Key` header or JSON `job_id`. Remove a sink with
`vulcan daemon config remove-notification-sink <name> --dry-run` and then apply the reviewed change.

## Unattended branch replay

A designated daemon can settle a branch whose `guarded-rebase` (or `rebase`) pull stopped on a
conflict, as described in [How a cycle works](#how-a-cycle-works). The following is a complete
setup for an already registered `personal` wiki; replace the provider details and the
environment-variable name, and make the named secret available to the daemon process:

```sh
vulcan daemon config set-agent resolution \
  --base-url <openai-compatible-url> \
  --model <model> \
  --api-key-env VULCAN_RESOLUTION_KEY
vulcan daemon config set-conflict-worker --wiki personal --poll-seconds 30 --dry-run

# Review the non-secret preview, apply it without --dry-run, and restart the daemon.
vulcan daemon config set-conflict-worker --wiki personal --poll-seconds 30
vulcan daemon stop
vulcan daemon start --detach
vulcan daemon status
vulcan daemon conflict-status
```

If the named credential variable is not set in the daemon's environment or `daemon.env`, the daemon
still starts and synchronizes. The agent is unavailable and the worker is listed as `disabled` in
`vulcan daemon status` with the reason (for example `resolution agent disabled:
VULCAN_RESOLUTION_KEY is not set`). This lets one shared `daemon.toml` enable branch replay only on
the device that holds the key. Provider keys remain environment-only; foreground and installed
Linux/macOS services can read them from `$XDG_CONFIG_HOME/vulcan/daemon.env` (normally
`~/.config/vulcan/daemon.env`). Keep that file mode `0600` on Unix and use literal `NAME=value`
records; inherited environment variables take precedence.

The worker skips paused or actively syncing wikis and applies the registration's Git and network
permission profile. A provider failure is reported in durable status and backs off for five minutes
across daemon restarts. Disable it with `vulcan daemon config clear-conflict-worker`. Run only one
configured worker per vault.

`vulcan --output json daemon conflict-status` reports, per wiki, `replayed_branch` (the commit a
paused branch was replayed to), `declined_replay` (a `head..upstream` range that will not be retried
until either side moves), `skipped`, or `error` plus `retry_after_unix_ms`, the earliest
epoch-millisecond retry time. `checked_unix_ms` is the durable pass time.
