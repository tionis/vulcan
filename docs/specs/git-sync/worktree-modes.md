# Worktree sync modes

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

Everything above describes the default mode, in which every cycle snapshots the whole worktree into
live. That suits notes edited where nobody commits. A repository that also holds code, or whose
authors commit their own work, may instead synchronize only committed history.

- Shared `.vulcan/config.toml` selects the mode with `sync.worktree = "all" | "none" | "paths"`
  (default `"all"`). `"none"` means committed-history-only synchronization for every device;
  `"paths"` is path-scoped synchronization (below). Device-local `.vulcan/config.local.toml` may set
  `"none"` to opt one device out of worktree synchronization, but any other local value is diagnosed
  and ignored, because a device that snapshots alone, or snapshots different paths, would publish
  uncommitted work the repository asked to keep local.
- `sync.branch_push` (default `true`, either layer, local wins) controls whether a cycle publishes
  local commits to the branch upstream. With `false`, cycles still fetch and fast-forward, and
  pushing stays a deliberate `git push`. It applies in both modes.
- A committed-history-only cycle never queries, fetches, pushes, or applies the live ref, so it
  creates no live snapshots, merges, conflicts, epochs, or semantic proposals. It still captures the
  whole worktree into the device-local candidate ref, so uncommitted work stays recoverable while
  the branch lane moves the checkout, but by default it does not publish the per-device safety head:
  uncommitted text never leaves the device, and no remote bridge chain accumulates.
  `sync.publish_device_backup = true` (either layer) publishes it under the same leases as in the
  worktree modes, as an off-device recovery copy that is never merged or applied anywhere. It then
  runs the branch lane exactly as described above: fetch, fast-forward (`merge --ff-only` tolerates
  dirty files the incoming commits do not touch), the user's pull strategy when diverged, and the
  leased push. Device-registration refs are observed in the same remote query as before.
- Because the branch lane is the whole cycle in this mode, its results become the cycle's outcome:
  `pulled` when the branch moved, otherwise `pushed` when it published, otherwise `up_to_date`. A
  paused branch lane (divergence past `pull.ff=only`, an interactive-rebase request, or a conflicted
  pull that was aborted) pauses the cycle with reason `branch_paused`; only a conflict whose abort
  failed pauses on the in-progress operation. A deferred pull (a dirty file the incoming commits
  change) remains a branch-lane detail of an otherwise successful cycle, and so does a rejected or
  failed push. A failed fetch or pull fails the cycle as a retryable error, since there is no file
  lane to report success for. Without a checked-out branch, an upstream, or the upstream ref on the
  remote, the cycle fails as unsupported configuration instead of silently doing nothing. Reports
  carry `worktree: "none"` so consumers can tell the modes apart.
- Workflows that create or publish live-ref state (conflict resolution and proposal approval,
  checkpoints, retention planning and application, semantic planning and automation, and therefore
  the daemon's semantic and conflict workers) refuse with an error naming the mode instead of acting
  on stale or absent live state. Read-only conflict listing still shows records retained from an
  earlier `"all"` period. Sync maintenance leaves those records, live, and device refs untouched;
  switching back resumes from them. A dry run skips the live query and file-lane preview, and `sync
  doctor` reports `sync.worktree` instead of live ref checks.
- The daemon cannot learn of new commits from worktree events, since committing only changes the Git
  directory, which the watcher ignores. Under the observation hub, the sync consumer of a
  committed-history-only wiki ignores worktree hints altogether (a cycle per saved file only
  re-snapshotted uncommitted edits). For every Git-synced wiki, in either mode, the daemon also
  natively watches the Git directory's `HEAD`, `packed-refs`, and `refs/heads/` (ignoring lock files
  and other private state) and treats changes there as sync triggers with the same debounce, so
  commits publish promptly. A branch move made by sync itself costs one further, unchanged cycle.
  Remote changes still arrive through adaptive polling; an unavailable ref watch only delays commits
  until the next poll or a worktree change.

## Path-scoped synchronization

> **Frozen (2026-11-23).** Path-scoped synchronization belongs to the whole-worktree live mode (`sync.worktree = "all"`/`"paths"`). It stays maintained as shipped but is not extended; once no wiki uses the live mode it is deprecated and removed, and geth takes over replication. New work targets [committed-history-only sync](none-mode.md).

`sync.worktree = "paths"` with a shared `sync.paths` list snapshots only part of the worktree. Every
other path travels only through real commits.

- Each `sync.paths` entry is a literal repository-relative file or directory (with everything below
  it) using `/` separators. Entries are normalized (a trailing `/` dropped, duplicates and entries
  below another entry removed); empty, absolute, `.`/`..`, `.git`, and `.vulcan` entries are
  rejected, because configuration must change through commits. `.vulcan/device-names` is always in
  scope. `sync.paths` is shared only; a local value is diagnosed and ignored. The normalized scope
  has a stable hash.
- The scope lives on the discovered `GitRepository`, so every engine worktree operation honours it,
  whether a sync cycle, conflict resolution, proposal approval, or retention calls it. A capture
  stages only scoped paths and then takes every unscoped path from the checked-out branch's
  published tip (the merge base of `HEAD` and its upstream tracking ref), so uncommitted unscoped
  files never enter live. Without a checked-out branch with a published upstream, path-scoped
  capture fails as unsupported configuration. Worktree comparison, the application precondition, and
  post-application verification compare only scoped paths, and application materializes only scoped
  paths through a two-way `read-tree -m -u` from the expected tree, so unscoped files (committed or
  not) are never written from live. Unscoped files therefore come only from the branch lane.
- Every live commit a path-scoped repository writes carries `Vulcan-Ref-Namespace: 4`, which readers
  that predate path scopes refuse, plus `Vulcan-Sync-Scope: <scope hash>` and
  `Vulcan-Sync-Scope-Branch: <published tip>`. That covers snapshots, merges, scope alignments,
  preserved conflict records, epoch reconciliations, epoch roots and their branch links, and side,
  batch, and proposal resolutions; the reproducible rollover commits record the branch tip they
  link. Whole-worktree commits keep namespace 3 and their historical trailers.
- Before reconciling with a different live tip, a cycle compares that tip's scope trailer with its
  own scope (none in `"all"` mode). A tip without a scope trailer (whole-worktree history, or a
  commit written by an older Vulcan) may always be adopted: switching a repository to path scopes
  starts from it, and the next path-scoped snapshot replaces its unscoped content with the published
  branch. A tip with a different scope is adopted only when this device's published branch is
  strictly newer than the tip's recorded branch. Otherwise the cycle fails with the retryable
  `ScopeMismatch` error until this device's branch (and with it the configuration) catches up. A
  device with stale configuration therefore never reverts paths another device still synchronizes
  from its worktree, while the device holding the newer committed configuration may change the
  scope.
- Merges, including offline epoch reconciliation against its bridge, compare only scoped paths:
  before merging, the local candidate takes every unscoped path from the remote tip in a `vulcan
  live scope alignment` commit, so the two sides never differ outside the scope and the existing
  merge, structured resolution, whole-tree validation, and conflict preservation run unchanged.
  Live's unscoped content can lag the newest published branch until the next snapshot restores this
  device's published tip; it is never applied to a worktree.
- The branch lane fast-forwards path by path instead of using `merge --ff-only` or index-only
  adoption, because scoped paths routinely hold uncommitted bytes that arrived through live. Each
  changed path is classified first. A changed unscoped path with uncommitted bytes defers the whole
  advance, a staged conflict defers as before, and a changed scoped path with other local bytes
  keeps them (the lane reports it as a detail). Git then performs the write itself: a two-way
  `read-tree -m -u` from the old commit to the new commit's tree, with kept paths restored to their
  old entries, updates every other changed path and refuses, inside one process, any file that
  changed after classification. Such a refusal defers to the next cycle rather than overwriting the
  edit. The branch and normal index then move as in adoption, which takes kept paths to the new
  commit without touching their bytes. Divergence still uses the user's pull strategy.
- Semantic planning refuses a target that differs from its source outside the scope: that difference
  is branch history live has not caught up with, which semantic commits must never revert. Doctor
  reports the scope (`sync.worktree`).
