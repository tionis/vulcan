# Committed-history-only sync

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

**Status:** Normative and current. Committed-history-only sync is the only sync behavior. It was
selected with `sync.worktree = "none"` while the whole-worktree live mode (`"all"`) and path-scoped
mode (`"paths"`) existed; both were removed on 2026-11-24. The detailed mechanics live in the
[branch lane](branch-lane.md); this page is the short contract a vault, an agent, or a reviewer can
rely on.

## Configuration

- `[sync] worktree = "none"` is still accepted, in either layer, and does nothing. `"all"` and
  `"paths"`, and the removed keys `sync.paths`, `sync.merge_policy`, `sync.merge_automation`,
  `sync.agent_auto_accept`, and `sync.tree_validation`, are ignored with a config diagnostic.
- `sync.branch_pull`, `sync.branch_push`, `sync.publish_device_backup`, `sync.mirrors`,
  `sync.poll_min_seconds`, and `sync.poll_max_seconds` shape the cycle described below.

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
5. **Mirror** the branch, when `sync.mirrors` names remotes of the clone and the cycle neither
   failed nor paused: each mirror receives the tip the primary remote has (the pushed head, or else
   the upstream tracking ref) on the same branch, as a plain fast-forward push with hooks, never
   forced. The tip is recorded in `refs/remotes/<mirror>/<branch>` after a successful push, and a
   mirror already at the tip costs no round trip. Each mirror reports `pushed`, `up_to_date`,
   `rejected` (it has commits the primary lacks), or `failed` in `branch.mirrors`; none of them fails
   the cycle, and the daemon alerts `sync_mirror_behind` once some mirror has not been current for
   30 minutes. Naming the upstream's own remote is reported as a failed mirror. Dry runs (and so
   `sync status`) contact no mirror: they report `planned` when the mirror's recorded tip differs
   from the upstream tracking ref and `up_to_date` otherwise. `sync doctor` reports `git.mirrors`,
   an error when a listed name is not a remote of the clone or is the primary remote.

Device-registration refs are observed in the same remote query as the device backup and upstream
tips, so an unchanged cycle costs one remote round trip.

Outcomes are `pulled` (the branch moved), `pushed` (it published), `up_to_date`, `paused`, or
`planned` for a dry run. A branch that cannot be reconciled pauses the cycle with `branch_paused`
and a typed reason (`fast_forward_only`, `interactive_rebase`, `shared_commits`, `merge_commits`,
`rebase_conflict`, `merge_conflict`) whose alert text gives the fix; only a conflict whose abort
failed pauses on the in-progress operation. A pull deferred by overlapping uncommitted edits is a
detail of a successful cycle and alerts (`sync_branch_deferred`) only after 30 minutes; so is a
rejected or failed push. A failed fetch or pull is a retryable failure. A missing checked-out
branch, upstream, or upstream ref on the remote fails the cycle as unsupported configuration
instead of silently doing nothing.

## Invariants

- Sync never commits worktree edits. It reads and writes no hidden live ref, so there are no live
  snapshots, merges, conflict records, checkpoints, epochs, or semantic proposals. Remote live refs
  and conflict refs left by the removed mode are ignored; nothing deletes them.
- Uncommitted text does not leave the device unless `publish_device_backup` is set.
- A mirror only ever receives commits the primary remote already has, and only by fast-forward.
- Sync rewrites commits only under the `rebase` and `guarded-rebase` policies, and never commits
  another branch or worktree builds on.
- A dry run takes no lock, writes nothing, and previews the remote refs and the branch lane.

## Who commits

Because sync moves only commits, capture belongs to whoever commits:

- People and agents with a shell commit themselves (`vulcan git commit`, `git commit`).
- Writers that cannot run Git (MCP clients, the bot, the REST API) rely on Vulcan's auto-commit,
  which always runs for synced vaults. With `[git] auto_commit = true` and `strategy = "batched"`,
  the daemon commits once the vault has been quiet, and the ref watch below then syncs the commit.
  Every MCP write reports a `commit` object; `uncommitted` means the change stays on this device
  until something commits it.
- The MCP `git` pack offers `git_commit` to clients that may commit directly. It commits only the
  files that session's writes changed, so one agent never commits another's half-finished edits,
  and names the client in `Vulcan-MCP-*` trailers. Auto-commit picks up whatever no agent
  committed; a batched commit carries the same trailers for every MCP client whose writes it
  takes (recorded per path in `.vulcan/write-attribution.json`, device state that prunes itself).
- An MCP session may stage work in a changeset: a `vulcan/changeset/<id>` branch checked out in a
  worktree under the user state directory. Sync never sees the branch or the worktree; landing
  fast-forwards the vault's branch under the vault write lock (rebasing first if it moved), and
  sync then publishes the result like any other commit. A changeset is not itself trusted; a skill
  command runs there under its vault's trust and grants only while its skill directory is
  byte-identical to the vault's and its effective permission profile grants no more there.

## Triggers

- The daemon cannot learn of new commits from worktree events, since committing only changes the
  Git directory, which the worktree watcher ignores. It natively watches the Git directory's
  `HEAD`, `packed-refs`, and `refs/heads/` (ignoring lock files and other private state) and treats
  changes there as sync triggers with the usual debounce, so commits publish promptly. A branch
  move made by sync itself costs one further, unchanged cycle. Saved files alone do not start a
  cycle; an unavailable ref watch only delays commits until the next poll.
- Remote changes arrive through adaptive polling.
- `vulcan sync run`, `vulcan sync refresh`, and the MCP `sync_refresh` and `sync_resume` tools
  start a cycle on demand.
