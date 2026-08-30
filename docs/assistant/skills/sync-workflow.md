---
name: sync-workflow
description: Synchronize one or more Vulcan wikis safely, inspect daemon or direct-mode state, diagnose Git-backed sync, review preserved conflicts, recover detached Android layouts, manage retention, or build semantic history. Use this whenever a user asks about `vulcan sync`, multi-device vault updates, the Vulcan daemon or Obsidian companion, Termux sync, sync conflicts, hidden live refs, or interrupted synchronization. Do not use it for ordinary human-authored Git commits with no device-sync concern; use git-workflow for that.
version: 1
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

Interpret `paused` as preserved work, not failure: staged normal-index changes, an in-progress Git
operation, or unexplained HEAD movement were captured before reconciliation stopped. Resolve that
ordinary Git state and rerun. `offline` likewise retains the local candidate. Never delete journals,
apply markers, or `refs/vulcan/**` to make a status look clean.

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

- Start the daemon explicitly with `vulcan daemon start` or `--detach`. Watcher, startup, poll, and
  companion triggers coalesce through one per-wiki supervisor.
- Provision a companion only from a running daemon. `vulcan daemon companion --output json` is
  non-secret; `--reveal-token` transfers bearer authority and must never be copied into a note,
  synchronized plugin settings, logs, or source control.
- The reference Obsidian companion requests editor save, debounces completed writes, displays
  authenticated state, and previews conflicts. It is not a second Git engine. Do not run it beside
  another independent Git-sync plugin against the same worktree.
- Provider endpoints/models are daemon configuration, not companion request fields. Use
  `vulcan daemon config set-agent resolution|semantic ... --dry-run`, keep key values in the named
  environment variable, apply deliberately, and restart the daemon.

## Android and detached Git data

- Under Termux, keep the worktree in shared storage and the detached Git directory in Termux-private
  storage. Clone with both `--git-dir <private-path>` and `--platform android-shared`.
- One-shot `sync status`, `sync doctor`, and `sync run` are the supported baseline and require no
  daemon. Android shared storage cannot faithfully represent executable bits or symlinks.
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
  apply explicitly. Reject declined plans with `sync semantic-reject <plan-id> --dry-run` followed
  by the reviewed mutation. Never edit proposal refs or retained plan JSON manually.

## Non-negotiable safety properties

- Current local bytes must be captured before remote application or publication.
- A remote update uses an exact lease; never replace a rejected push with unconditional force.
- The user's normal index, staged state, and semantic branch are not sync scratch space.
- Scan only after the complete accepted tree has been applied and verified.
- Treat policy, platform, link-validation, deletion-limit, stale-input, and worktree-drift failures as
  reasons to preserve and stop. Do not bypass them to make synchronization appear seamless.
