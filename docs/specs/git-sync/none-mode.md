# Committed-history-only sync (`sync.worktree = "none"`)

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

**Status:** Normative and current. This is the sync mode new work targets; the whole-worktree live
mode is frozen. The detailed mechanics live in [worktree modes](worktree-modes.md) and the
[branch lane](branch-lane.md); this page is the short contract a vault, an agent, or a reviewer
can rely on.

## Selection

- Shared `.vulcan/config.toml` sets `[sync] worktree = "none"` for every device. A device may opt
  itself out of worktree sync with the same value in `.vulcan/config.local.toml`; it cannot turn a
  shared `"none"` back into another mode.
- Reports carry `worktree: "none"`, and `sync doctor` reports `sync.worktree` instead of live-ref
  checks.

## What a cycle does

1. **Capture locally.** The worktree, including uncommitted edits, is snapshotted into this
   device's local safety ref so the branch lane can move the checkout without losing bytes. The
   snapshot is never merged or applied anywhere. It leaves the device only when
   `sync.publish_device_backup = true`, which pushes it to this device's private remote branch
   under exact leases.
2. **Fetch and fast-forward** the checked-out branch. A fast-forward tolerates uncommitted files
   that the incoming commits do not touch; one that they do touch defers the pull to a later cycle.
3. **Reconcile divergence** by `sync.branch_pull`: `git` (each clone's `pull.rebase`/`pull.ff`),
   `ff-only`, `merge`, `rebase`, or `guarded-rebase`, which replays local commits only when no other
   ref or worktree contains them, none is a merge, and the replay applies cleanly. A merge or rebase
   that sync starts and that conflicts is always aborted, leaving the branch as it was.
4. **Push** local commits when the branch is strictly ahead of its upstream, with the observed
   tracking ref as an exact lease and never with force. `sync.branch_push = false` (either layer)
   leaves pushing to the user.

Outcomes are `pulled`, `pushed`, or `up_to_date`. A branch that cannot be reconciled pauses the cycle
with `branch_paused` and a typed reason (`fast_forward_only`, `interactive_rebase`,
`shared_commits`, `merge_commits`, `rebase_conflict`, `merge_conflict`) whose alert text gives the
fix. A pull deferred by overlapping uncommitted edits is a detail of a successful cycle and alerts
(`sync_branch_deferred`) only after 30 minutes. A failed fetch or pull is a retryable failure; a
missing branch, upstream, or remote branch is unsupported configuration.

## Invariants

- Sync never commits worktree edits and never creates, reads, or publishes the live ref, so there
  are no live snapshots, merges, conflict records, checkpoints, epochs, or semantic proposals.
  Workflows that need live state refuse with an error naming the mode; live state retained from an
  earlier mode is left untouched.
- Uncommitted text does not leave the device unless `publish_device_backup` is set.
- Sync rewrites commits only under the `rebase` and `guarded-rebase` policies, and never commits
  another branch or worktree builds on.

## Who commits

Because sync moves only commits, capture belongs to whoever commits:

- People and agents with a shell commit themselves (`vulcan git commit`, `git commit`).
- Writers that cannot run Git (MCP clients, the bot, the REST API) rely on Vulcan's auto-commit.
  With `[git] auto_commit = true` and `strategy = "batched"`, the daemon commits once the vault has
  been quiet, and the ref watch below then syncs the commit. Every MCP write reports a `commit`
  object; `uncommitted` means the change stays on this device until something commits it.
- The MCP `git` pack offers `git_commit` to clients that may commit directly. It commits only the
  files that session's writes changed, so one agent never commits another's half-finished edits,
  and names the client in `Vulcan-MCP-*` trailers. Auto-commit picks up whatever no agent
  committed.

## Triggers

- The daemon watches the Git directory's `HEAD`, `packed-refs`, and `refs/heads/`, so a commit
  starts a cycle after the usual debounce. Saved files alone do not start one.
- Remote changes arrive through adaptive polling.
- `vulcan sync run`, `vulcan sync refresh`, and the MCP `sync_refresh` and `sync_resume` tools
  start a cycle on demand.
