---
name: sync-workflow
description: Synchronize one or more Vulcan wikis safely, configure advertised realtime wake-up endpoints, inspect daemon or direct-mode state, diagnose Git-backed sync, review preserved conflicts, recover detached Android layouts, manage retention, or build semantic history. Use this whenever a user asks about `vulcan sync`, multi-device vault updates, realtime notifications, the Vulcan daemon or Obsidian companion, Termux sync, sync conflicts, hidden live refs, or interrupted synchronization. Do not use it for ordinary human-authored Git commits with no device-sync concern; use git-workflow for that.
version: 5
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Sync Workflow

Vulcan synchronizes canonical vault files through finite, recoverable transactions. Live snapshots
are deliberately non-semantic and do not advance the user's checked-out branch. Use the direct CLI
for one-shot work and the daemon for scheduling, watching, multiple wikis, or a companion client;
both execute the same application workflow.

## Select the execution mode

- For an unregistered path or a daemon-independent operation, use `vulcan --vault <path> sync ...`.
  Direct commands never start a daemon implicitly.
- For registered wikis, select one ID, `--group <name>`, or `--all`. Aggregate results are independent
  per-wiki transactions, not a cross-repository atomic commit.
- Use `vulcan daemon status` before diagnosing automatic work. A stopped daemon does not prevent
  direct `sync status`, `sync doctor`, or `sync run`.
- Use `vulcan sync pause [<wiki>] --dry-run` and then the same command without `--dry-run` only when
  the user wants to stop future automatic triggers. Manual direct operations remain available.

## Inspect before mutation

1. Run `vulcan sync status [<wiki>]` to inspect layout, safety state, candidate refs, and remote live
   state without mutation.
2. Run `vulcan sync doctor [<wiki>]` when installation, detached storage, hidden refs, filters/LFS,
   platform compatibility, locks, journals, apply markers, or cache coherence may be involved.
3. Preview a finite transaction with `vulcan sync run [<wiki>] --dry-run`; apply it by omitting
   `--dry-run` only after the target remote/live ref and diagnostics are understood.
4. Every Git and Git LFS subprocess is bounded to 300 seconds by default. For a known slower
   transport, set `--git-timeout-seconds <seconds>` on `sync run`; do not raise it merely to hide a
   stuck credential helper, filter, or remote. A timeout preserves the captured snapshot and
   recovery journal, so diagnose the named phase and rerun safely.

Interactive human `sync run` uses one transient phase line and clears it before printing the compact
result. Add `--verbose` for durable phase-by-phase diagnostics, including retries. `--quiet`,
structured output, and redirected runs suppress progress chatter; JSON remains isolated on stdout.
Every Git subprocess still has the configured timeout, so a stuck phase fails with a named,
recoverable diagnostic instead of waiting forever.

Interpret `paused` as preserved work, not failure: an in-progress Git
operation or unexplained HEAD movement was captured before reconciliation stopped. Resolve that
ordinary Git state and rerun. Staged changes are not a pause condition: they sync as ordinary
worktree bytes while the normal index is left untouched. `offline` likewise retains the local
candidate. Never delete journals,
apply markers, or `refs/vulcan/**` to make a status look clean.

## Branch lane

Every finite cycle also pulls the checked-out branch from its upstream before the hidden live
refs move, following the repository's own pull configuration (`pull.ff`, `pull.rebase`,
`branch.<name>.rebase`) with `--no-edit` and no implicit autostash. Watch the human output or
the JSON `branch` report for `fast-forwarded`, `merged`, `rebased`, `paused` (diverged past
`pull.ff=only`, interactive rebase, or a merge/rebase conflict left for ordinary Git),
`deferred` (dirty worktree, retried next cycle), or `skipped` (no upstream, detached HEAD, or
bare repository). The branch is never pushed; publication stays in the semantic lane.

## Review preserved conflicts

- List records with `vulcan sync conflicts`; inspect one immutable record with
  `vulcan sync conflicts <conflict-id>`. Keep its base/local/remote refs and device-local artifacts.
- Never choose a winner implicitly. Preview one explicit side with
  `vulcan sync resolve <id> --side base|local|remote --dry-run`.
- For reviewed content, use complete `--file '<conflict-path>=<source>'` inputs, a reviewed
  `--patch <file>`, or `--editor`; preview every mode first. The editor writes markers only in a
  private temporary directory.
- Generate model help only when requested with `vulcan sync propose <id> --model <model> ...`.
  Provider output is an untrusted retained proposal, not an accepted merge. Review it, then preview
  exact approval with `sync resolve --approve-proposal <proposal-id> --dry-run`, or reject it with
  `sync reject <conflict-id> <proposal-id> --dry-run`.
- A published conflict materialization keeps accepted remote bytes at the original path and local
  copies under `.sync-conflicts/<id>/local/`. Do not edit or push that managed tree manually; a
  reviewed Vulcan resolution removes it atomically.

## Daemon and Obsidian companion

### Realtime ref-change notifications

- Realtime configuration is discovered automatically from the exact Git ref
  `refs/vulcan/notifications`; there is no subscription-bundle import or separate credential store.
  The ref points to a commit containing `notification.json` with version `1`, transport
  `http_long_poll`, and one HTTPS `subscribe_url`.
- Treat the complete subscribe URL as confidential repository capability data. Never print it,
  copy it into a note, or expose it in logs. Repository administrators configure the separate
  publish-only URL or secret directly in the forge webhook; that value never belongs in the Git
  advertisement. `sync advertise`/`unadvertise` reports carry only the endpoint origin and
  fingerprint for this reason.
- Publish or rotate the advertisement with
  `vulcan sync advertise --subscribe-url <https-url> [--remote origin] [--expected <rev>]`.
  Preview with `--dry-run` first. Without `--expected`, the current remote revision is leased
  opportunistically; with it, a diverged ref fails instead of overwriting. Remove it with
  `vulcan sync unadvertise [--expected <rev>]`. Publication builds a parentless commit with
  object-store plumbing only, so the worktree and user index are never touched. The commit
  attributes the publisher's Git identity (repository configuration over global), so rotation
  history shows who advertised each endpoint. Add `--sign` (or `--signing-key <keyid>`) for a
  GPG/SSH signature from the publisher's own configuration; this needs a working agent or
  cached credentials and fails loudly otherwise. Signatures are not verified by discovery.
- Check whether Vulcan would use a notification server with
  `vulcan sync notifications [--wiki <id>]`: it fetches the advertisement through the
  configured remote (the same device-local fetch the daemon performs, never a publish),
  validates it, applies the effective Git and network permission checks, and reports stable
  reason codes (`missing-advertisement`, `invalid-advertisement`, `git-denied`,
  `network-denied`, `paused`, `non-git-backend`, `daemon-stopped`) plus the `would_listen`
  verdict. Endpoint identity stays origin plus fingerprint.
- The daemon starts one listener per active advertised Git wiki. A stopped daemon, missing ref,
  malformed advertisement, or unavailable endpoint only increases latency; direct and periodic
  synchronization continue normally.
- The wiki's permission profile must allow Git plus network access to the endpoint origin. Do not
  weaken a profile to hide a notification diagnostic; periodic polling is the safe fallback.
- Notifications are untrusted hints. They may enqueue only the ordinary finite synchronization
  transaction for the registered wiki; response bytes never select a ref, object, remote, or local
  path. Git remains authoritative and periodic polling remains the repair path.

- Prefer `vulcan daemon install --dry-run` followed by `vulcan daemon install` for a persistent
  per-user service on Linux, macOS, or Windows. Linux installs a restartable `systemd --user` unit,
  macOS installs a restartable per-user LaunchAgent, and Windows installs a limited per-user logon
  task. Uninstall with `vulcan daemon uninstall --dry-run` and then the reviewed mutation.
  Uninstalling the service does not remove registrations, credentials, journals, conflicts, or
  vault data.
- Services and direct foreground startup read optional provider credentials from the device-local
  `$XDG_CONFIG_HOME/vulcan/daemon.env` file (normally `~/.config/vulcan/daemon.env`). Keep it mode
  `0600` on Unix, use literal `NAME=value` entries, and never place it in a vault. Existing process
  environment variables take precedence, and the file does not execute or expand shell syntax.
- Start the daemon explicitly with `vulcan daemon start` or `--detach`. On Linux, macOS, and
  Windows the running daemon owns advertised long-poll listeners; watcher, notification, startup,
  poll, and companion triggers coalesce through one per-wiki supervisor.
- The daemon is quiet by default. Run `vulcan --verbose daemon start` (or with `--detach`,
  which carries the flag to the background child and its `daemon.log`) for operational stderr
  lines: one per completed sync job with wiki, triggers, and state/outcome, plus notification
  advertisement discovery and wake-up enqueueing identified by endpoint origin and fingerprint
  only. Installed services run at the default quiet level.
- Provision a companion only from a running daemon. `vulcan daemon companion --output json` is
  non-secret; `--reveal-token` transfers bearer authority and must never be copied into a note,
  synchronized plugin settings, logs, or source control.
- The reference Obsidian companion requests editor save, debounces completed writes, displays
  authenticated state, and previews conflicts. It is not a second Git engine. Do not run it beside
  another independent Git-sync plugin against the same worktree.
- Provider endpoints/models are daemon configuration, not companion request fields. Use
  `vulcan daemon config set-agent resolution|semantic ... --dry-run`, keep key values in the named
  environment variable, apply deliberately, and restart the daemon.
- For daemon-owned LLM semantic commits, configure the semantic agent first, then preview and apply
  `vulcan daemon config set-semantic-worker --wiki <id> --quiet-seconds <n> --maximum-wait-seconds <n> --poll-seconds <n>`.
  The allowlist is explicit; paused/busy wikis are skipped. Restart after changing configuration,
  inspect `vulcan daemon semantic-status`, and disable with `daemon config clear-semantic-worker`.

## Android and detached Git data

- Under Termux, keep the worktree in shared storage and the detached Git directory in Termux-private
  storage. Clone with both `--git-dir <private-path>` and `--platform android-shared`.
- One-shot `sync status`, `sync doctor`, and `sync run` are the supported baseline and require no
  daemon. Android shared storage cannot faithfully represent executable bits or symlinks.
- For an Android-managed periodic safety net, preview `vulcan sync termux-install <wiki>
  --period-minutes 60 --dry-run`, then apply only after checking the job ID, wrapper path, and
  network/battery policy. It requires Termux:API plus `pkg install termux-api`, defaults to
  battery-not-low and storage-not-low, and never starts the daemon. Use `--network unmetered` or
  `--charging` when requested. Preview `sync termux-uninstall <wiki> --dry-run` before removal.
- Treat Android JobScheduler timing as approximate. Use the periodic job as an energy-efficient
  safety net; a shortcut or future save/resume bridge may invoke the same finite `sync run` for
  lower latency. A foreground or persistently supervised Termux daemon may use the same advertised
  listener as an optional latency optimization, but it never replaces JobScheduler reconciliation.
  Verify unattended Git credentials manually and never embed secrets in the managed wrapper or
  vault.
- If private Git data is lost, preview `vulcan vault recover-git <wiki> <remote> --dry-run`.
  Recovery captures the untouched materialized vault before fetching, but cannot reconstruct
  unpushed objects that existed only in the deleted Git directory.

## Retention and semantic history

- Use `sync retention-plan` before any retention action. Preview `sync retention-apply --dry-run`;
  add `--rollover` or epoch-archive expiry only after the user accepts the offline-recovery impact.
- Use `sync semantic-plan --from <rev> --to <accepted-live-rev> --dry-run` to propose human-facing
  history without rewriting live snapshots. `top-level` is default; `file`, `change`, `hunk`, and
  `all` are deterministic alternatives. Hunk grouping splits only safe separated text changes.
- Materialize the plan only after review, preview `sync semantic-apply <plan-id> --dry-run`, and then
  apply explicitly. Preview `sync semantic-publish <plan-id> --dry-run` before publication; the
  real command uses the recorded source as an exact remote lease and refuses divergence instead
  of force-pushing. Reject declined plans with `sync semantic-reject <plan-id> --dry-run` followed
  by the reviewed mutation. Never edit proposal refs or retained plan JSON manually.
- For cron, timers, or Forgejo CI, use `sync semantic-auto [<wiki>]`. It runs one bounded cycle and
  exits: `deferred` means the accepted live revision has not passed `--quiet-seconds`, `up_to_date`
  means no semantic tree change exists, and `completed` includes application plus leased
  publication. Use `--maximum-wait-seconds` to cap batching and `--dry-run` for a state-free preview.

## Non-negotiable safety properties

- Current local bytes must be captured before remote application or publication.
- A remote update uses an exact lease; never replace a rejected push with unconditional force.
- The user's normal index and semantic branch are not sync scratch space; staged entries sync
  as worktree bytes without pausing and are never staged, reset, or rewritten by sync.
- Scan only after the complete accepted tree has been applied and verified.
- Treat policy, platform, link-validation, deletion-limit, stale-input, and worktree-drift failures as
  reasons to preserve and stop. Do not bypass them to make synchronization appear seamless.
