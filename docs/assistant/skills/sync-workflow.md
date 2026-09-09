---
name: sync-workflow
description: Synchronize one or more Vulcan wikis safely, configure advertised realtime wake-up endpoints, inspect daemon or direct-mode state, diagnose Git-backed sync, review preserved conflicts, recover detached Android layouts, manage retention, or build semantic history. Use this whenever a user asks about `vulcan sync`, multi-device vault updates, realtime notifications, the Vulcan daemon or Obsidian companion, Termux sync, sync conflicts, hidden live refs, or interrupted synchronization. Do not use it for ordinary human-authored Git commits with no device-sync concern; use git-workflow for that.
version: 18
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

For a new sync checkout, preview `vulcan sync clone <remote> <path> --dry-run` before applying it.
The command derives the wiki ID from the destination, uses native clone defaults on desktop, and
automatically selects a detached private Git directory plus the `android-shared` policy in Termux.
Pass `--id`, `--git-dir`, or `--platform` only when those defaults are not appropriate.

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
   state without mutation. Human output calls this a **sync preview**: “inspected” means the
   read-only check completed, not that synchronization succeeded. The preview inspects the branch
   lane and classifies the file lane as `up_to_date`, `local_changes`, `remote_differs`,
   `local_and_remote_differ`, `local_missing`, `remote_missing`, or `uninitialized` by comparing
   observed refs and the worktree with its last local snapshot. It does not fetch, merge, apply, or
   detect whether differing trees conflict; use `--output json` for `preview.file_state`, observed
   refs, and branch detail.
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
candidate. A successful sync that only failed to refresh the rebuildable cache reports a
`cache_refresh_error` warning instead of failing; the vault is fully synced and the next refresh
heals the cache. Never delete journals,
apply markers, or `refs/vulcan/**` to make a status look clean.
If a retained journal includes an error, daemon and companion status project `error` even though
the journal keeps the exact failed phase for recovery. Do not interpret a stale-looking phase name
inside diagnostics as an operation that is still running. Daemon jobs preserve the sync engine's
typed failure category and retryability through the application layer: network/authentication
failures can project offline, while repository/configuration/invariant failures remain errors with
different repair guidance rather than being flattened into retryable `unknown` failures.

## Branch lane

Every finite cycle also pulls the checked-out branch from its upstream before the hidden live
refs move, following the repository's own pull configuration (`pull.ff`, `pull.rebase`,
`branch.<name>.rebase`) with `--no-edit` and no implicit autostash. Watch the human output or
the JSON `branch` report for `fast-forwarded`, `merged`, `rebased`, `paused` (diverged past
`pull.ff=only`, interactive rebase, or a merge/rebase conflict left for ordinary Git),
`deferred` (dirty worktree, retried next cycle), or `skipped` (no upstream, deleted upstream,
detached HEAD, or bare repository). After a healthy pull lane, the branch tip is published to its
upstream with the observed tracking ref as an exact lease — never force-pushed — before hidden
file reconciliation. Ordinary commits therefore still fetch, pull, and push when the file lane
later preserves a conflict. A moved remote reports for the next cycle; transport or policy
failures record `push_detail` without preventing the file lane, and `pushed` tells whether
publication happened. A daemon job with either branch failure is nevertheless terminally
`failed`, with status `error`; this keeps aggregate status and companion failure notices truthful.
Human output also prints `push_detail` whenever branch publication was rejected or failed; do not
interpret a successful file-lane summary as proof that the checked-out branch was published.
Caveats: `rebase.autostash` is neutralized (automation never stashes implicitly); a
`commit.gpgsign` setup needs a working agent or unattended merges fail loudly; triangular
push remotes (`branch.<name>.pushremote`, `remote.pushdefault`) are not honored — the pull
upstream receives the push; long-lived staged changes keep `pull.rebase` users deferred until
they commit.

## Review preserved conflicts

- List records with `vulcan sync conflicts`; inspect one immutable record with
  `vulcan sync conflicts <conflict-id>`. Keep its base/local/remote refs and device-local artifacts.
  A fully applied resolution prunes its artifact copies automatically (newest 32 retained); the
  immutable refs remain the durable byte archive.
- Never choose a winner implicitly. Preview one explicit side with
  `vulcan sync resolve <id> --side base|local|remote --dry-run`.
  For a `tree_validation` conflict with no individual conflict paths, the selected side replaces
  the complete candidate tree; this is the explicit escape hatch when Git merged cleanly but
  Vulcan's whole-tree link or deletion policy rejected the result.
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
- Git may report synthesized destinations for directory-rename or file-location conflicts that do
  not exist in either candidate tree. Vulcan preserves those structural conflicts without a
  materialized tree and refuses path-side selection with a specific diagnostic; do not reinterpret
  that refusal as a missing-file deletion or edit hidden refs to force a result.
- A sync cycle that fails with "would overwrite untracked worktree files excluded by Git ignore
  rules" refuses to apply the accepted tree over a device-local ignored file that shares a path
  with an incoming file. Move or remove the named ignored files, then rerun; never delete them
  automatically.
- Vulcan verifies the worktree before and after applying an accepted tree. If Obsidian or another
  writer changes a file during that window, the finite cycle recaptures and retries automatically;
  repeated activity exhausts the bounded retry limit and remains a retryable `busy` failure.
- The daemon gives an initial retryable repository-lock (`busy`) failure one durable recovery
  cycle before notifying. If that recovery is also busy, it records and delivers the failure
  normally instead of retrying forever.

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
- Publish or rotate the advertisement by piping the capability without exposing it in process
  arguments or shell history:
  `secret-manager read notification-subscribe-url | vulcan sync advertise --subscribe-url-file - [--remote origin] [--expected <rev>]`.
  On Unix, a mode-`0600` regular file with no symlinked path component may be named instead of
  `-` when piping is impractical. Other platforms must use piped stdin.
  Notification advertisement publication and discovery currently require the `origin` remote;
  alternate sync remotes are rejected so CLI status and the daemon cannot observe different refs.
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
  configured remote (the same device-local fetch the daemon performs, never a publish), but only
  after the effective profile allows Git. It then validates the advertisement, applies the
  endpoint network permission check, and reports stable
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
- Daemon sync watchers use native filesystem notifications when available. Content-comparing
  polling runs only if native watcher setup fails, with a 30-second interval; it can be costly
  on large worktrees. Periodic sync reconciliation (every five minutes by default) repairs
  missed notifications. To request reconciliation immediately, use `vulcan sync run <wiki>`.
- The daemon is quiet by default. Run `vulcan --verbose daemon start` (or with `--detach`,
  which carries the flag to the background child and its `daemon.log`) for operational stderr
  lines: one per completed sync job with wiki, triggers, state/outcome, watch-trigger detail,
  and branch-lane action, plus notification advertisement discovery and wake-up enqueueing
  identified by endpoint origin and fingerprint only. At the default quiet level, failed jobs
  print one secret-minimal line per wiki/category state with retryability and a direction to inspect
  retained status; error text remains on the authenticated status surface. Conflicted and paused
  jobs emit warning records, failed jobs emit error records, and branch pull-strategy and push
  failures also keep their specific diagnostic. Attention states deduplicate per wiki until recovery,
  so installed services make persistent trouble visible without flooding their logs. To add native
  desktop delivery, preview and apply
  `vulcan daemon config set-notifications --desktop true --dry-run` and then without `--dry-run`,
  then restart the daemon. Desktop delivery is best-effort, timeout-bounded, and never changes the
  retained sync result; helper or desktop-session failures become warning log records. Its durable
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
- The reference Obsidian companion requests editor save, debounces completed writes, displays
  authenticated state, and previews conflicts. By default it shows one bounded notice per failed
  daemon job or retained failed transaction, deduplicated by durable job/transaction identity
  across its live event stream and 30-second polling fallback. A temporary companion connection
  failure changes the status bar to offline but is not labeled as a sync failure. The setting can
  disable notices without changing authoritative status. It is not a second Git engine. Do not run
  it beside another independent Git-sync plugin against the same worktree.
- Provider endpoints/models are daemon configuration, not companion request fields. Use
  `vulcan daemon config set-agent resolution|semantic ... --dry-run`, keep key values in the named
  environment variable, apply deliberately, and restart the daemon.
- For daemon-owned LLM semantic commits, configure the semantic agent first, then preview and apply
  `vulcan daemon config set-semantic-worker --wiki <id> --quiet-seconds <n> --maximum-wait-seconds <n> --poll-seconds <n>`.
  The allowlist is explicit; paused/busy wikis are skipped. Restart after changing configuration,
  inspect `vulcan daemon semantic-status`, and disable with `daemon config clear-semantic-worker`.

## Android and detached Git data

- Install a checksummed, full-feature `aarch64-linux-android` release through the version-matched
  POSIX installer; never substitute the `aarch64-unknown-linux-gnu` archive, which targets a
  different runtime. A Termux source build needs Clang, `rquickjs/bindgen`, and the documented
  AArch64 SLP-vectorizer workaround; do not disable QuickJS, OAuth, vectors, or web merely to make
  the Android build compile.
- Under Termux, keep the worktree in shared storage and the detached Git directory in Termux-private
  storage. `vulcan sync clone <remote> <shared-path> --dry-run` selects both defaults automatically.
- After a successful clone, an ownership rejection from Git automatically adds only the canonical
  new worktree path to the user's global `safe.directory` entries and retries discovery. This also
  applies to `vault clone`; dry runs do not change Git configuration. Existing repositories are
  never automatically trusted by discovery, status, or sync.
- One-shot `sync status`, `sync doctor`, and `sync run` are the supported baseline and require no
  daemon. Android shared storage cannot faithfully represent executable bits or symlinks.
- For an Android-managed periodic safety net, preview `vulcan sync termux-install <wiki>
  --period-minutes 60 --dry-run`, then apply only after checking the job ID, wrapper path, and
  network/battery policy. It requires Termux:API plus `pkg install termux-api`, defaults to
  battery-not-low and storage-not-low, and never starts the daemon. Use `--network unmetered` or
  `--charging` when requested. Preview `sync termux-uninstall <wiki> --dry-run` before removal.
- Inspect saved settings with `vulcan sync schedule show <wiki>`. Change only the interval with
  `vulcan sync schedule set <wiki> --period-minutes 30 --dry-run`, then apply without `--dry-run`.
  Updates retain the same Android job ID and all unspecified settings. Use `--network unmetered`,
  `--charging true|false`, `--battery-not-low true|false`, or `--persisted true|false` for explicit
  changes. The minimum interval is 15 minutes. These commands manage an existing job; create it
  with `sync termux-install` first. Saved settings do not prove Android still has the job queued;
  use `termux-job-scheduler --pending` to inspect Android's actual queue.
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
