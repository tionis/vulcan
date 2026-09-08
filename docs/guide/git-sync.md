# Git-backed device synchronization

Vulcan can synchronize the complete canonical vault through a dedicated Git live branch. Each `vulcan sync run` is a finite, direct transaction: it captures the current worktree without touching the normal Git index, reconciles the configured live ref, applies an accepted tree only after preserving local bytes, and refreshes an existing cache. The daemon schedules the same workflow but is not required.

This is device/file-tree synchronization. It replicates the whole vault rather than selecting or translating notes for an external wiki.

## Ordinary Linux and Windows setup

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

Android shared storage is suitable for the Obsidian-visible files but not for Git's private repository machinery. Keep the worktree in shared storage and the detached Git directory inside Termux-private storage.

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
restrictive policy. Android periodic jobs are approximate and have a 15-minute minimum, so this is
a safety net rather than realtime delivery. A Termux shortcut or future Obsidian/native wake bridge
can call `vulcan sync run personal` after save or resume for lower latency; overlapping invocations
still enter Vulcan's ordinary per-repository transaction serialization.

Preview removal before cancelling the Android job and deleting only its managed private wrapper and
manifest:

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

A conflicting sync preserves the immutable candidates and may publish a safe materialization. The accepted remote bytes remain at the original path, the competing local bytes appear below `.sync-conflicts/<conflict-id>/local/`, and clean paths continue synchronizing. The hidden directory is not indexed as notes.

Inspect and resolve through Vulcan rather than editing its refs or conflict-copy structure manually:

```sh
vulcan sync conflicts
vulcan sync conflicts <conflict-id>
vulcan sync resolve <conflict-id> --side local --dry-run
vulcan sync resolve <conflict-id> --side local
```

Complete-file, patch, editor, and reviewed agent-proposal modes are also available. Successful resolution removes the hidden conflict directory atomically while retaining the original Git objects and durable conflict record.

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
