# Branch lane, safety gates, status, identity, and worktree application

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

- Normal-index and Git-operation safety gates protect reconciliation and application without
  sacrificing recoverability. Staged changes sync as ordinary worktree bytes through the alternate
  index while the normal index is never staged, reset, or rewritten; only
  merge/rebase/cherry-pick/revert/bisect state or an unexpected HEAD commit or symbolic-ref change
  pauses the cycle. When such a state is present, Vulcan first captures the worktree to its local
  candidate ref, publishes its per-device safety head, and fetches an existing remote live tip, then
  emits a structured pause reason and retains the paused transaction journal. It does not push
  canonical live, apply, reset the normal index, or reinterpret a branch switch. A safety change
  detected after remote acceptance anchors that accepted commit under the pending ref before pausing
  application.
- Each finite cycle publishes its pre-operation worktree to the remote device safety head, then
  fetches and, when possible, pulls the checked-out branch from its upstream before the canonical
  hidden live ref moves, using the strategy fixed by the vault's `sync.branch_pull` (`ff-only`,
  `merge`, `rebase`, or `guarded-rebase`, which first asks the engine for a rebase hazard: a
  replayed commit reachable from another branch, tag, remote-tracking ref, or worktree HEAD,
  ignoring Vulcan's own `refs/vulcan/*` and `__vulcan-sync` namespaces, or a merge commit in the
  range; a hazard pauses the lane with the ref named), or, when it is `git` (the default), the
  user's own pull configuration (`pull.ff`, `pull.rebase`, `branch.<name>.rebase`), always with
  `--no-edit` and no implicit autostash. A fixed policy is applied without reading the Git pull
  settings, so a malformed device setting cannot fail a lane the vault already decided. If the pull
  changes the checkout, the resulting bytes are captured and published again; a divergent
  same-device history is bridged, keeping the pre-pull state reachable. A merge or rebase that the
  branch lane starts and that conflicts is aborted at once (`git merge --abort` / `git rebase
  --abort`), restoring the pre-pull branch, index, and worktree, which the device backup already
  holds: an unattended checkout is never left mid-operation, where the next commit by an agent or
  hook would record conflict markers. Only if the abort fails does the conflict stay for the
  in-progress-operation pause. A daemon whose conflict worker lists the wiki, under a `rebase` or
  `guarded-rebase` policy, may afterwards replay the paused branch with the resolution agent:
  `GitEngine::prepare_branch_replay` cherry-picks the local commits onto the upstream in a scratch
  worktree (removed by a drop guard, then `git worktree prune`) with hooks disabled, signing off
  (reported as `signatures_dropped` when `commit.gpgSign` is set), and LFS smudging skipped; commits
  the upstream already contains are dropped, as `git rebase` does; each conflicted commit's text
  sides go to the agent, and the answer must cover exactly the conflicted paths without conflict
  markers. Preparing holds neither the vault's mutation gate nor the repository lock, so agent calls
  never block writers or synchronization. `GitEngine::apply_branch_replay` then runs under both: the
  branch moves only by a two-way checkout that refuses to overwrite uncommitted bytes followed by a
  compare-and-swap of the ref, and a lost compare-and-swap restores the files only while HEAD still
  names the replay's starting commit. Any hazard, deletion, non-regular or non-UTF-8 file, or path
  with a Git `filter` attribute declines with no change, and the declined `head..upstream` is not
  retried. Divergence past `pull.ff=only` or `sync.branch_pull = "ff-only"`, an interactive rebase
  request, or an aborted merge/rebase conflict pauses the branch lane with a reason, and the daemon
  raises an attention alert with `reason: "branch_paused"` (event `sync_branch_paused` when the file
  lane succeeded) whose desktop, Telegram, and webhook text gives the fix; a later cycle that finds
  the branch reconciled clears it; a dirty worktree the pull must not overwrite defers to the next
  cycle. Repositories without an upstream, detached HEADs, and bare repositories skip with a reason.
  Immediately after a healthy branch pull lane, the branch tip is published to its upstream with the
  tracking ref observed during the attempt as an exact lease — never force-pushed — before
  conflict-prone canonical file reconciliation begins. Ordinary committed history therefore
  continues to fetch, pull, and push when the file lane later preserves an unresolved conflict. A
  moved remote reports for the next cycle, while transport or policy failures remain explicit
  branch-lane failures without preventing the file lane from proceeding.
- Live history links the checked-out branch. The published branch tip is the merge base of `HEAD`
  and the branch's upstream tracking ref, so it never includes unpushed commits; it is absent for
  detached, bare, or upstream-less checkouts. Whenever that tip is not already an ancestor of the
  snapshot base, capture creates a live snapshot even for an unchanged tree, with the previous
  snapshot as first parent and the published branch tip as second parent, plus a
  `Vulcan-Sync-Branch-Link` trailer naming it. First-parent traversal, epoch discovery, and
  retention therefore see only live snapshots, while ordinary ancestry makes the branch an ancestor
  of every later accepted live tip. When the branch lane pushes or moves the branch within a cycle,
  the cycle recaptures so the same attempt publishes the new link. Epoch roots remain parentless;
  rollover publishes a reproducible same-tree link commit on top of the root when a published branch
  tip exists, and offline epoch bridging selects the newest archive whose own epoch root (rather
  than any shared branch commit) is contained in the offline candidate.
- An upstream branch tip that the synchronized worktree already contains is adopted rather than
  pulled. When the fetched upstream tip is a fast-forward of `HEAD` and an ancestor of the captured
  snapshot (before reconciliation) or of the applied accepted live tip (after application), the
  branch lane moves the branch with a two-way index-only merge (`read-tree -i -m HEAD upstream`)
  followed by a compare-and-swap ref update, and records `adopted`. Worktree bytes are never
  rewritten by adoption, because the worktree already descends from the upstream content through
  live; ordinary pull strategies would instead resurrect upstream bytes that later live history
  changed. Index entries equal to the old `HEAD` move to the upstream entry, other staged entries
  are preserved, and a staged entry on a path the upstream also changes refuses adoption with a
  deferred reason. This is the only normal-index rewrite performed by sync, and it is exactly the
  index transition of a fast-forward checkout. It resolves devices that received another device's
  edits through live before that device's commits reached the branch, which previously left the
  receiving checkout with a stale branch and a large apparent uncommitted diff.
- Remote device safety heads are operator-visible recovery state, not automatic merge inputs.
  `vulcan sync devices list` enumerates exact heads, `devices fetch <id>` anchors a selected head
  and current live under local recovery refs and reports `same`, `integrated`, `contains_live`, or
  `diverged`, plus changed paths and isolated-worktree guidance. `devices prune-backup <id>` (with
  `remove` retained as a compatibility alias) cannot prune the current device backup, requires those
  exact fetched recovery refs, rechecks the remote objects, and deletes with an exact lease only
  when the candidate is an ancestor of live or has the same tree. Pruning a backup does not revoke
  device, Git transport, or vault access. Divergent or live-ahead device information fails closed;
  remote deletion deliberately retains the local recovery ref. Sync maintenance additionally sweeps
  at most once a day per repository: one listing of `refs/heads/__vulcan-sync/*` identifies backups
  in the sync profile whose device has no registration ref and is not this device; only those are
  fetched (into temporary local refs, released afterwards), and those the accepted live revision
  contains, or whose tree equals live's, are deleted with one push carrying an exact lease per ref.
  Backups holding content live lacks are kept and recorded for a `sync.device-backups` doctor
  warning. The sweep then releases local device recovery refs whose remote backup no longer exists
  and whose history live contains (and the profile's live review anchor once no device anchors
  remain), adds `^refs/heads/__vulcan-sync/*` to `remote.<remote>.fetch` when the remote has
  configured fetch refspecs, so plain `git fetch` and `git pull` (and Git-CLI clients such as the
  desktop Obsidian Git plugin) no longer download device backups or conflict objects, and drops
  every remote-tracking copy under `refs/remotes/<remote>/__vulcan-sync/`. Nothing reads those
  copies; Git ignores negative refspecs both when a push records its tracking refs and when pruning,
  so the sweep removes them instead. Vulcan's own fetches name every source and destination and pass
  `--refmap=`, so they create no tracking copies and are unaffected by the exclusion. Clients
  without negative-refspec support, such as isomorphic-git, cannot fetch from a remote configured
  this way; Vulcan's Android layout keeps the detached Git directory in Termux-private storage, out
  of their reach. Registered devices' backups are never swept. This gives another node the tools to
  resolve a lost device's conflicts without silently mixing its files into canonical live.
- `sync status` is a read-only preview, not a retained daemon-job status or a completed finite
  cycle. It queries remote refs, previews the checked-out branch, compares the worktree with its
  last local snapshot, and classifies the hidden file lane as up to date, locally changed, remotely
  different, changed on both sides, missing on one side, or uninitialized. Registered status also
  reads durable conflict records and reports unresolved retained conflicts separately from the
  current file-lane comparison; an accepted/materialized lane can be current while historical
  conflict choices still require review. Failure to inspect retained conflict state is explicit and
  makes the overall inspection incomplete rather than silently assuming zero conflicts. It does not
  fetch remote objects, merge, apply, or claim that differing trees are conflict-free. Human output
  labels the operation and its successful items as a preview/inspection, states that no changes were
  applied, and must not render the internal `planned` outcome as though synchronization itself
  succeeded.
- The first mutating sync initializes the installation-wide cryptographic device identity and reuses
  its key-derived ID as the Git actor identity across vaults; dry-run and doctor only read it and
  never initialize state. The contract in `docs/specs/device-identity.md` is: one locally generated
  Ed25519 SSH keypair, a device ID derived from its canonical public-key blob, no continuity between
  keys, dual-reading of legacy IDs, and no destructive relabelling of existing refs or provenance.
  Roadmap 12.16 and `docs/specs/device-key-custody.md` then separate the public identity from its
  active private-key provider: protected file and native system secret are concrete no-fallback
  choices; the generic exact-key SSH agent is interactive-only. Same-key migration checks dependent
  operations, retains the old copy for separate cleanup, and does not change identity.
  Vulcan-created live snapshot, live merge, recovery snapshot, and conflict-resolution commits use
  concise non-semantic subjects plus parseable trailers for sync protocol version, device, profile
  where applicable, policy version/hash, immutable source revisions, conflict identity where
  applicable, and `Vulcan-Sync-Semantic: false`. This provenance is Git-reachable and never depends
  on `cache.db` or note frontmatter. Key derivation and custody alone do not authenticate a remote
  claim; signing, trust, ownership, revocation, and SSH transport authorization remain later
  explicit layers.
- Worktree application has two durable witnesses with deliberate ordering. Vulcan first persists the
  device-local transaction journal, then writes a versioned `vulcan-sync/apply.json` marker inside
  the private Git directory before the Git engine mutates files. The marker identifies the
  transaction, repository, pre-apply snapshot revision, and accepted revision; it survives failed or
  interrupted application, gives future watchers a stable self-generated event identity, and is
  cleared only after post-application tree verification. `sync doctor` treats a retained or
  malformed marker as an error and directs the next cycle to recapture and verify rather than
  trusting partially applied bytes.
- Before application, the Git engine derives a typed plan from the immutable expected and accepted
  commits. Each changed path is classified as add, update, delete, or type change and retains its
  expected/target object ID, mode, and kind without loading blob contents. Immediately before Git's
  alternate-index worktree update, Vulcan stages the complete current tree through normal Git clean
  filters and requires its tree ID to equal the expected revision exactly; this is the
  delete/overwrite precondition and also detects untracked or filtered drift outside the planned
  path set. Git performs the materialization without touching the user's normal index, after which
  Vulcan verifies the complete result against an isolated temporary index seeded from the accepted
  tree before scanning the cache; consulting the normal index here would misclassify target-added
  files as untracked. The returned application plan makes destructive scope visible in JSON reports
  and remains independent of an installed-Git versus future embedded engine implementation. The
  versioned public Git-engine conformance harness builds its seed and bare remote with ordinary Git,
  gives the selected engine exclusive ownership after clone, and checks stable capture, normal-index
  isolation, local and remote ref leases, live and custom-ref transport, divergent-conflict
  classification, safe application and drift rejection, plus ordinary-Git object/ref readability.
  Journal interruption and repository locking stay in the shared application transaction tests
  rather than becoming engine-specific behavior.
