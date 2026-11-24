# Branch lane, safety gates, status, identity, and repository checks

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

The branch lane is the whole sync cycle; [committed-history-only sync](none-mode.md) gives the short
contract.

- Normal-index and Git-operation safety gates protect reconciliation without sacrificing
  recoverability. Capture stages the worktree, including staged and uncommitted changes, through an
  alternate index into the device-local snapshot ref; the normal index is never staged, reset, or
  rewritten except by upstream adoption (below). Merge/rebase/cherry-pick/revert/bisect state or an
  unexpected HEAD commit or symbolic-ref change pauses the cycle. Vulcan first captures the
  worktree and, when `sync.publish_device_backup = true`, publishes its per-device safety head, then
  emits a structured pause reason and retains the paused transaction journal. It does not reset the
  normal index or reinterpret a branch switch.
- Each finite cycle captures its pre-operation worktree, then fetches and, when possible, pulls the
  checked-out branch from its upstream, using the strategy fixed by the vault's `sync.branch_pull`
  (`ff-only`, `merge`, `rebase`, or `guarded-rebase`, which first asks the engine for a rebase
  hazard: a replayed commit reachable from another branch, tag, remote-tracking ref, or worktree
  HEAD, ignoring Vulcan's own `refs/vulcan/*` and `__vulcan-sync` namespaces, or a merge commit in
  the range; a hazard pauses the lane with the ref named), or, when it is `git` (the default), the
  user's own pull configuration (`pull.ff`, `pull.rebase`, `branch.<name>.rebase`), always with
  `--no-edit` and no implicit autostash. A fixed policy is applied without reading the Git pull
  settings, so a malformed device setting cannot fail a lane the vault already decided. If the pull
  changes the checkout, the resulting bytes are captured again; a divergent same-device backup
  history is bridged, keeping the pre-pull state reachable. A merge or rebase that the branch lane
  starts and that conflicts is aborted at once (`git merge --abort` / `git rebase --abort`),
  restoring the pre-pull branch, index, and worktree, which the safety snapshot already holds: an
  unattended checkout is never left mid-operation, where the next commit by an agent or hook would
  record conflict markers. Only if the abort fails does the conflict stay for the
  in-progress-operation pause.
- A daemon whose conflict worker lists the wiki, under a `rebase` or `guarded-rebase` policy, may
  afterwards replay the paused branch with the resolution agent: `GitEngine::prepare_branch_replay`
  cherry-picks the local commits onto the upstream in a scratch worktree (removed by a drop guard,
  then `git worktree prune`) with hooks disabled, signing off (reported as `signatures_dropped`
  when `commit.gpgSign` is set), and LFS smudging skipped; commits the upstream already contains are
  dropped, as `git rebase` does; each conflicted commit's text sides go to the agent, and the answer
  must cover exactly the conflicted paths without conflict markers. Preparing holds neither the
  vault's mutation gate nor the repository lock, so agent calls never block writers or
  synchronization. `GitEngine::apply_branch_replay` then runs under both: the branch moves only by
  a two-way checkout that refuses to overwrite uncommitted bytes followed by a compare-and-swap of
  the ref, and a lost compare-and-swap restores the files only while HEAD still names the replay's
  starting commit. Any hazard, deletion, non-regular or non-UTF-8 file, or path with a Git `filter`
  attribute declines with no change, and the declined `head..upstream` is not retried.
- Divergence past `pull.ff=only` or `sync.branch_pull = "ff-only"`, an interactive rebase request,
  or an aborted merge/rebase conflict pauses the cycle with reason `branch_paused`, and the daemon
  raises an attention alert whose desktop, Telegram, and webhook text gives the fix; a later cycle
  that finds the branch reconciled clears it. A dirty worktree the pull must not overwrite defers to
  the next cycle. A cycle without a checked-out branch, an upstream, or the upstream ref on the
  remote fails as unsupported configuration. After a healthy pull, a branch strictly ahead of its
  upstream is published with the tracking ref observed during the attempt as an exact lease, never
  force-pushed. A moved remote reports for the next cycle, while transport or policy failures stay
  explicit branch-lane details.
- The local snapshot links the checked-out branch. The published branch tip is the merge base of
  `HEAD` and the branch's upstream tracking ref, so it never includes unpushed commits. Whenever
  that tip is not already an ancestor of the snapshot base, capture creates a snapshot even for an
  unchanged tree, with the previous snapshot as first parent and the published branch tip as second
  parent, plus a `Vulcan-Sync-Branch-Link` trailer naming it.
- An upstream branch tip that the worktree already contains is adopted rather than pulled. When the
  fetched upstream tip is a fast-forward of `HEAD` and an ancestor of the captured snapshot, the
  branch lane moves the branch with a two-way index-only merge (`read-tree -i -m HEAD upstream`)
  followed by a compare-and-swap ref update, and records `adopted`. Worktree bytes are not rewritten.
  Index entries equal to the old `HEAD` move to the upstream entry, other staged entries are
  preserved, and a staged entry on a path the upstream also changes refuses adoption with a deferred
  reason. This is the only normal-index rewrite performed by sync, and it is exactly the index
  transition of a fast-forward checkout.
- Remote device safety heads are operator-visible recovery state, not merge inputs. `vulcan sync
  devices list` enumerates exact heads; `devices fetch <id>` anchors a selected head under a local
  recovery ref and, while the remote still holds a live ref left by the removed whole-worktree mode,
  compares the head with it (`same`, `integrated`, `contains_live`, or `diverged`, plus changed
  paths). `devices prune-backup <id>` (with `remove` retained as a compatibility alias) cannot prune
  the current device backup, requires the exact fetched recovery refs, rechecks the remote objects,
  and deletes with an exact lease only when the candidate is an ancestor of that legacy live ref or
  has the same tree; without one it refuses. Pruning a backup does not revoke device, Git transport,
  or vault access, and remote deletion deliberately retains the local recovery ref. Nothing sweeps
  device backups or legacy live and conflict refs automatically.
- `sync status` is a read-only preview (a dry-run cycle), not a retained daemon-job status. It
  queries remote refs and previews the checked-out branch without fetching objects. Registered
  status also counts retained legacy conflict records. Human output labels the operation as a
  preview and must not render the internal `planned` outcome as though synchronization succeeded.
- The first mutating sync initializes the installation-wide cryptographic device identity and reuses
  its key-derived ID as the Git actor identity across vaults; dry-run and doctor only read it and
  never initialize state. The contract in `docs/specs/device-identity.md` is: one locally generated
  Ed25519 SSH keypair, a device ID derived from its canonical public-key blob, no continuity between
  keys, dual-reading of legacy IDs, and no destructive relabelling of existing refs or provenance.
  Vulcan-created snapshot commits use concise non-semantic subjects plus parseable trailers for sync
  protocol version, device, and source revisions. This provenance is Git-reachable and never depends
  on `cache.db` or note frontmatter.
- `vulcan-app` persists interruption-sensitive sync transaction journals under the platform Vulcan
  user-state directory (`XDG_STATE_HOME` or the platform/home fallback), keyed by the canonical
  worktree. Journals are versioned, bounded, and atomically replaced; they never live in
  `.vulcan/cache.db` or the synchronized tree. Sync no longer writes the `vulcan-sync/apply.json`
  application marker; `sync doctor` still reports one retained from the removed mode.
- The direct sync doctor composes read-only `GitEngine` diagnostics with `vulcan-app` journal and
  cache inspection. It reports the resolved Git CLI/version, layout and operation safety, device
  identity, transport, remote reachability, advisory lock ownership, required filters/LFS
  availability, ignored cache files, retained recovery state, Syncthing replication of the same
  vault, and cache/file inventory coherence without creating a repository, cache, ref, or state
  directory. Finite cycles perform the same tracked-path filter inspection before capture or remote
  access: each declared filter must have a process driver or both clean and smudge drivers, and Git
  LFS must also be executable; an unavailable driver fails as unsupported repository configuration.
  A typed immutable-tree preflight of the local snapshot uses the registered platform profile (or
  the native profile for an unregistered path): case-fold and canonical-Unicode aliases plus
  Windows-reserved components are error-level incompatibilities, while non-representable executable
  modes, link-file symlinks, and filesystem-dependent long paths remain bounded warnings. Reports
  carry it as `local_platform_preflight`.
- `vulcan sync ignore-device-files [--dry-run]` appends anchored entries for Obsidian's per-device
  layout files (`.obsidian/workspace.json`, `.obsidian/workspace-mobile.json`) to the vault's
  `.gitignore` when no shared `.gitignore` already ignores them, and `sync doctor` reports
  `sync.device-files` as a warning while such a file is tracked; `sync doctor --fix` applies the
  same entries. Registering a vault with a `.obsidian` directory for Git sync (`vault add`, `vault
  clone`, `sync clone`) adds them as part of setup unless `--keep-device-files` is given.
- A rejected compare-and-swap or leased push does not fail at once: the finite cycle retries up to
  the configured bound with deterministic exponential waits from 25 ms through a 400 ms cap,
  observing cancellation before and after every wait.
