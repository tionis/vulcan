# Git engine, ref namespace, and device safety state

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

- `vulcan-sync` owns synchronous device/file-tree backend contracts and typed repository-engine
  operations. Its versioned, serializable plan/report/status/conflict/job/error schemas and explicit
  capability declarations are shared by direct, daemon, companion, test, and JSON surfaces; the
  finite `SyncBackend::sync_once` contract deliberately excludes scheduling and retained lifecycle
  state. Its initial `GitCliEngine` uses fixed Git CLI operations and reports installation plus
  colocated, detached, and bare repository layouts, while `GitSyncBackend` adapts the implemented
  finite Git cycle to the backend-neutral contracts without overstating conflict preservation. Every
  engine command uses one captured-I/O spawn boundary: Unix starts a process group for bounded
  timeout cleanup, and Windows applies `CREATE_NO_WINDOW` so background daemon cycles and
  Git-spawned helpers cannot flash console windows. Daemon scheduling and complete vault
  transactions stay outside this crate. Finite cycles expose typed progress observations and a
  cloneable cancellation token, checking cancellation only between safe Git operations after
  preserving any captured candidate.
- Agent-facing Git synchronization remains narrower than the human CLI and companion transaction
  surface. The MCP `sync` pack contains only mutation-free status, finite-cycle preview, doctor, and
  immutable conflict list/detail tools with closed JSON schemas; it never exposes arbitrary Git
  arguments or conflict resolution. Because these reports can name any path in a repository-wide
  tree, visibility and execution require both the profile's Git capability and unrestricted vault
  read access. Profiles with path-filtered reads must use narrower note/query tools instead of
  receiving a partially redacted sync report that could obscure a safety invariant.
- The Git ref layout is a formal versioned contract owned by `vulcan-sync`, not a collection of
  call-site strings. Version 1 centralizes validated builders for device-local
  candidate/fetched/pending refs, epoch archives, conflicts and their recovery/decisions,
  checkpoints, semantic proposals, detached-loss recovery, and remote per-device safety heads. Sync
  reports expose `refs.namespace_version`, and created live and epoch commits carry
  `Vulcan-Ref-Namespace` trailers so recovery tooling can interpret reachable state independently of
  the rebuildable cache. Local `refs/vulcan/**` are never published. Every mutating finite cycle
  separately pushes the captured snapshot object to
  `refs/heads/__vulcan-sync/devices/<profile>/<device-id>` before pull or canonical reconciliation;
  each attempt first observes the device head, the branch upstream when it shares the sync remote,
  and (unless the cycle was triggered to fetch live first) the live and registration refs in one
  `ls-remote`. A device head that already covers the capture is not pushed, an upstream tip already
  held by its tracking ref is not fetched, and the observed live tip feeds reconciliation, so an
  unchanged cycle costs one remote round trip, opens no receive-pack connection, and runs no
  pre-push hooks. When that observation already shows the cycle would only fast-forward live to the
  capture (live is the local copy, the branch lane cannot move the checkout, and no Git operation is
  in progress), the device head and live are published by one non-atomic push with a lease per ref.
  This saves a connection and a run of pre-push hooks, and the backup still lands if live moved
  first. Every remote write still carries an exact lease, so a stale observation can only cause a
  rejected push and a retry. One moving exact-lease head per device bounds remote-ref growth. A
  fast-forward retains prior history normally; reuse of one device identity by divergent restored
  checkouts creates a reproducible two-parent same-tree bridge so neither history is discarded.
  Current writers emit namespace version 3 with key-derived `vdev1_...` device IDs; key-shaped refs
  remain untrusted recovery evidence. Legacy ULID heads remain readable recovery state. Readers
  reject live-tip namespace versions above the one they understand before canonical reconciliation,
  after preserving a captured local safety backup. Generic Git transport conformance proves custom
  refs can use fetch and leased mutation, but the interoperable remote default remains the hidden
  branch `refs/heads/__vulcan-sync/live` with its epoch and device subtrees until a deployed Forgejo
  passes explicit permission, advertisement, webhook, maintenance, and Actions checks.
- Friendly device labels are optional, mutable display metadata in shared
  `.vulcan/device-names/<device-id>.json` files. They travel through ordinary vault sync when
  tracked by Git, stay outside the rebuildable cache and note frontmatter, and do not affect ref
  names, identity comparisons, or recovery commands. Unknown devices remain identifiable by their
  full IDs.
- The read-only device inventory reports remote safety heads, whether each has a matching local
  recovery ref, retained local recovery refs with no remote head, and names without any backup. A
  local recovery copy marked current only matches the observed remote revision; it does not
  establish integration with live or permission to delete the backup. The installation-wide
  projection keeps local identity and legacy sync actor distinct, groups recovery evidence by
  registered wiki, and records each wiki's independent remote observation; an unavailable remote
  does not erase local evidence or imply a backup is absent. Explicit offline mode skips remote Git
  observation and reports `not_requested` while retaining local recovery and labels.
- Detached-directory recovery is explicit rather than an automatic repair. `vulcan vault
  recover-git` requires a device-local registration, an intact materialized worktree, an absent
  registered Git directory, and a regular `.git` pointer that still names exactly that directory. It
  recreates an empty detached repository with the recorded platform policy, captures the complete
  worktree under a unique recovery ref, and only then configures and fetches the supplied `origin`;
  it performs no checkout or reset. Sync maintenance later releases that recovery ref (and any other
  `refs/vulcan/recovery/detached-git-loss/*` ref) once the accepted live revision contains its whole
  history, with an exact-tip delete; until then it keeps the only copy of those commits. A
  mismatched pointer or existing Git directory fails closed. The report names hidden Vulcan ref
  namespaces that may have held unpushed objects which cannot be reconstructed from the shared
  worktree, and warns that uninstalling Termux can remove private Git data while leaving an
  Obsidian-visible shared-storage vault.
