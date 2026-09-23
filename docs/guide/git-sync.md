# Git-backed device synchronization

Vulcan can synchronize a registered directory's complete canonical file tree through a dedicated Git live branch. Each `vulcan sync run` is a finite, direct transaction: it captures the current worktree without touching the normal Git index, reconciles the configured live ref, and applies an accepted tree only after preserving local bytes. Knowledge-profile sync also refreshes an existing cache. The daemon schedules the same workflow but is not required.

### Managed-directory profiles

Registered directories use a versioned, device-local capability profile. `knowledge` is the backward-compatible default. Choose `files-only` with `vulcan vault add <id> <path> --profile files-only`, `vulcan vault clone <remote> <path> --profile files-only`, `vulcan sync clone <remote> <path> --profile files-only`, or `vulcan vault set <id> --profile files-only`. A registration may also use `vulcan vault add <id> <path> --no-sync` when only device-local management is wanted. Registration and profile changes do not initialize or change the vault's `.vulcan/config.toml`.

The `files-only` profile allows arbitrary files to be managed without initializing or refreshing a Markdown index. It disables knowledge services, scripts, semantic history, agent conflict resolution, and knowledge-specific tree validation for that registered path. Knowledge commands are rejected while the path is registered as files-only; switch back with `vulcan vault set <id> --profile knowledge` when it should act as a knowledge base again. Both profiles currently use full-tree materialization. Capability reports expose the versioned profile and materialization policy; files-only doctoring skips cache checks and reports repository safety. Registration JSON for existing knowledge profiles keeps its previous shape.

Files-only devices retain concurrent automatic merges for review so a local profile cannot accept bytes that a knowledge device would reject. The daemon's unattended files-only preflight rejects detached HEAD, staged changes, multiple linked worktrees, in-progress Git operations, and nested repositories or submodules. It also checks the branch again before applying synchronized files. Vulcan's repository lock does not exclude external Git processes, so do not switch branches or run Git operations while an unattended sync job is active. Active development checkouts are not a supported unattended-management workflow; use explicit manual synchronization only after reviewing repository state.

File synchronization replicates working-tree content; it is not by itself a complete backup. Complete backup needs verified retention and restoration for refs, history, and Git LFS payloads, which the current sync contract does not promise.

Selective materialization remains planned in [Roadmap 12.20](../ROADMAP.md#1220-selective-materialization-and-large-repository-synchronization). Sparse checkout, partial clone, shallow-history reconciliation, and selective LFS hydration are not currently supported sync combinations. Existing Git/LFS filter checks do not establish those guarantees. The future model preserves excluded paths and reports metadata synchronization separately from local payload availability.

This is device/file-tree synchronization. It replicates the whole vault rather than selecting or translating notes for an external wiki.

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

The default colocated layout keeps `.git/` beside the vault. `sync run` does not start the daemon. It also works against an unregistered repository selected with `--vault <path>`.

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
Configure unattended Git authentication in Termux itself and verify it with a manual
`vulcan sync run personal` before relying on the scheduler. Do not put credentials in the wrapper,
vault, or command line.

The `android-shared` profile records the filesystem limitations instead of pretending it behaves like native Linux storage:

- executable bits are not representable;
- symlinks are checked out as link files;
- case-folding, Unicode-normalization, Windows-reserved-name, and path-length hazards are diagnosed before unsafe publication or application;
- case-only renames may require an intermediate path.

Do not place the detached Git directory in shared storage. Uninstalling Termux can remove its private objects and refs while leaving the visible vault behind. If that happens, preserve the vault and use `vulcan vault recover-git personal <remote> --dry-run` before applying recovery. Vulcan captures the surviving worktree first, but unpushed objects that existed only in the lost private directory cannot be reconstructed.

## Conflicts and recovery

A conflicting sync preserves the immutable candidates and may publish a safe projection. The accepted remote bytes remain provisionally at the original path, clean paths continue synchronizing, and no conflict artifacts are written into the vault. The immutable conflict-record ref is also published to the remote so the record and both candidate histories remain Git-reachable.

Inspect and resolve through Vulcan rather than editing its refs or conflict-copy structure manually:

```sh
vulcan sync conflicts
vulcan sync conflicts <conflict-id>
vulcan sync resolve <conflict-id> --side local --dry-run
vulcan sync resolve <conflict-id> --side local
```

Complete-file, patch, editor, and reviewed agent-proposal modes are also available. Use repeatable
`--file '<conflict-path>=<reviewed-source>'` arguments for mixed conflicts containing `.obsidian`
state, binary content, or Git-synthesized rename destinations: complete-file resolution accepts
those locally reviewed bytes without exposing them to an LLM. Agent proposals intentionally reject
the entire conflict when any input is ineligible. Successful resolution closes the conflict state
by publishing the reviewed tree and a remote resolution ref while retaining the original Git
objects and durable conflict record.

If a finite cycle is interrupted or the network is unavailable, rerun `vulcan sync run`. Device-local journals and Git refs retain the captured state; do not delete them or replace the vault with a fresh clone as a recovery shortcut.

## Background failure notifications

The daemon always writes one structured warning or error record when a registered wiki first
enters a conflicted, paused, or failed sync state. Repeated automatic attempts in the same state do
not flood the service log; a successful cycle resets the notification. Full error detail remains in
`vulcan sync status <wiki>` rather than logs or desktop messages.

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

## Debounced semantic commits

`sync semantic-auto` is a finite scheduler entrypoint for cron, a systemd timer, or Forgejo Actions.
Each invocation reads the accepted live revision and a small device-local observation record. It
exits as `deferred` until that revision has been stable for the quiet interval (or the maximum
batching interval is reached), then creates a semantic plan, applies it to the configured semantic
branch, and publishes it with an exact remote lease. An already-current tree exits as `up_to_date`.

```sh
vulcan sync semantic-auto personal \
  --quiet-seconds 900 \
  --maximum-wait-seconds 21600
```

Use `--agent --model <model> --base-url <url> --api-key-env <name>` for LLM-organized whole-file
groups and commit messages. The model cannot change vault bytes: the generated history must still
reproduce the exact accepted live tree. `--dry-run` neither advances the debounce record nor creates
Git objects or refs. `--no-publish` keeps a completed semantic history local. Schedule only one
writer per semantic branch; cross-run races are still rejected by the local compare-and-swap and
remote exact lease.

To run the same workflow inside Vulcan's installed daemon, configure the provider and an explicit
wiki allowlist, then restart the daemon (configuration is loaded at startup):

```sh
vulcan daemon config set-agent semantic \
  --base-url <openai-compatible-url> \
  --model <model> \
  --api-key-env VULCAN_SEMANTIC_KEY
vulcan daemon config set-semantic-worker \
  --wiki personal \
  --semantic-ref refs/heads/main \
  --quiet-seconds 900 \
  --maximum-wait-seconds 21600 \
  --poll-seconds 30
vulcan daemon semantic-status
```

The worker only visits listed wikis, skips paused wikis and wikis with queued/running file-tree
sync jobs, applies the registration's Git and network permission profile, and records the latest
per-wiki outcome outside the vault. Disable it with
`vulcan daemon config clear-semantic-worker`. Provider keys remain environment-only; foreground
and installed Linux/macOS services can read them from `$XDG_CONFIG_HOME/vulcan/daemon.env`
(normally `~/.config/vulcan/daemon.env`). Keep that file mode `0600` on Unix and use literal
`NAME=value` records; inherited environment variables take precedence.

## Unattended conflict resolution

A designated daemon can resolve conservative high-confidence conflicts and publish the accepted
tree for other devices without an operator in the loop. Pairing it with the semantic worker keeps
the byte-exact sync history separate from a readable commit history on `main`. The following is a
complete setup for an already registered `personal` wiki; replace the paths, provider details, and
environment-variable names, and make both named secrets available to the daemon process:

```sh
vulcan daemon config set-agent resolution \
  --base-url <openai-compatible-url> \
  --model <model> \
  --api-key-env VULCAN_RESOLUTION_KEY
vulcan --vault /path/to/vault config set sync.agent_auto_accept true --target local
vulcan daemon config set-conflict-worker \
  --wiki personal \
  --max-groups-per-run 128 \
  --poll-seconds 30 \
  --dry-run

# Review the non-secret preview, then apply the same allowlist.
vulcan daemon config set-conflict-worker \
  --wiki personal \
  --max-groups-per-run 128 \
  --poll-seconds 30

vulcan daemon config set-agent semantic \
  --base-url <openai-compatible-url> \
  --model <model> \
  --api-key-env VULCAN_SEMANTIC_KEY
vulcan daemon config set-semantic-worker \
  --wiki personal \
  --semantic-ref refs/heads/main \
  --quiet-seconds 900 \
  --maximum-wait-seconds 21600 \
  --poll-seconds 30 \
  --dry-run

# Review the preview, apply it without --dry-run, and reload daemon configuration.
vulcan daemon config set-semantic-worker \
  --wiki personal \
  --semantic-ref refs/heads/main \
  --quiet-seconds 900 \
  --maximum-wait-seconds 21600 \
  --poll-seconds 30
vulcan daemon stop
vulcan daemon start --detach
vulcan daemon status
vulcan daemon conflict-status
vulcan daemon semantic-status
```

The worker skips paused or actively syncing wikis and only sends pending singleton Markdown or text
overlaps with complete regular-file evidence under strict byte ceilings. It never enables broad
context. Binary, structural, missing-side, device-state, oversized, and other ineligible conflicts
remain available for ordinary review. Provider output still passes the normal exact-path, syntax,
link, deletion, tree, recovery, stale-input, and remote-lease checks before it can be accepted. A
provider failure is reported in durable status and backs off for five minutes across daemon restarts.
Disable unattended resolution with `vulcan daemon config clear-conflict-worker`; disabling the local
`sync.agent_auto_accept` switch is an additional per-vault stop. Run only one configured resolver
daemon for a vault: claims are shared inside one daemon process, while Git leases provide safety—not
token-spend coordination—across devices.

Accepted resolutions immediately advance the canonical sync history consumed by other devices. To
also maintain readable commits on `main`, configure the semantic worker above; it can organize and
message the accepted bytes but cannot modify them.

For automation and detailed diagnosis, request JSON status:

```sh
vulcan --output json daemon conflict-status
```

After a successful pass it has this shape (optional outcome fields are omitted when absent):

```json
{
  "version": 1,
  "checked_unix_ms": 1789900000000,
  "entries": [
    {
      "wiki_id": "personal",
      "unresolved_conflicts": 3,
      "eligible_groups": 2,
      "conflict_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "proposal_id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "resolution_commit": "0123456789abcdef0123456789abcdef01234567"
    }
  ]
}
```

`checked_unix_ms` is the durable pass time. Each entry reports the wiki, unresolved-conflict count,
and number of eligible groups selected in that pass. `conflict_id`, `proposal_id`, and
`resolution_commit` identify an accepted result. A non-action outcome instead uses `skipped`; a
failure uses `error` plus `retry_after_unix_ms`, the earliest epoch-millisecond retry time.

### Troubleshooting the conflict worker

- If status says that the worker has not completed a pass, confirm `vulcan daemon status`, inspect
  `vulcan daemon config show`, verify the named provider key exists in the daemon environment, and
  restart after configuration changes. An older installed `vulcan` binary may not contain these
  commands; rebuild or update it before diagnosing daemon state.
- `waiting for provider error backoff` means the preceding provider or approval attempt failed.
  Read `error` and `retry_after_unix_ms` from JSON status, repair credentials, connectivity, or the
  reported safety failure, and wait until that time. The five-minute backoff is durable across
  restarts, so restarting is not a bypass.
- `no pending conflict groups met the high-confidence policy` is an expected safe outcome for
  binary, structural, missing-side, device-state, oversized, non-text, or otherwise ineligible
  groups. Inspect them with `vulcan sync conflicts` and resolve them through the ordinary reviewed
  workflow.
- If proposal creation succeeded but automatic approval failed, the error retains the proposal ID.
  Inspect the conflict, then preview explicit approval with
  `vulcan sync resolve <conflict-id> --approve-proposal <proposal-id> --dry-run`, or preview rejection
  with `vulcan sync reject <conflict-id> <proposal-id> --dry-run`. Do not edit proposal refs or state
  files manually.
- If local auto-accept is disabled, enable it only in device-local config with
  `vulcan --vault <path> config set sync.agent_auto_accept true --target local`; do not put this
  trust decision in shared vault config.
- A backlog drains deliberately: each wiki processes at most one conflict per poll, with no more
  than `max_groups_per_run` complete groups from that conflict. Monitor successive status passes
  instead of raising the bound past 128.
- Operate one resolver daemon per vault. Remote leases prevent unsafe publication races across
  devices, but they do not prevent duplicate provider requests and token spend.
