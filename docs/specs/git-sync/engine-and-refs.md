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
  call-site strings. Validated builders cover the device-local snapshot and device-tracking refs,
  device recovery refs, detached-loss recovery, the legacy conflict refs and conflict archive, and
  remote per-device safety heads. Sync reports expose `refs.namespace_version` and `refs.local`,
  `refs.device`, and `refs.device_tracking`, and snapshot commits carry `Vulcan-Ref-Namespace`
  trailers so recovery tooling can interpret reachable state independently of the rebuildable
  cache. Local `refs/vulcan/**` are never published. The sync profile key in these names is still
  derived from the remote and the former default live ref name, so snapshot and backup refs keep
  their names across the removal of the live mode.
- Every mutating finite cycle captures the worktree into the local snapshot ref. With
  `sync.publish_device_backup = true` it also pushes that snapshot to
  `refs/heads/__vulcan-sync/devices/<profile>/<device-id>` before the branch lane pulls. Each
  attempt first observes the device head, the branch upstream when it shares the sync remote, and
  the device-registration refs in one `ls-remote`. A device head that already covers the capture is
  not pushed and an upstream tip already held by its tracking ref is not fetched, so an unchanged
  cycle costs one remote round trip and opens no receive-pack connection. Every remote write carries
  an exact lease, so a stale observation can only cause a rejected push and a retry. One moving
  exact-lease head per device bounds remote-ref growth. A fast-forward retains prior history
  normally; reuse of one device identity by divergent restored checkouts creates a reproducible
  two-parent same-tree bridge so neither history is discarded. Current writers use key-derived
  `vdev1_...` device IDs; key-shaped refs remain untrusted recovery evidence, and legacy ULID heads
  remain readable recovery state. The hidden branch `refs/heads/__vulcan-sync/live` and its epoch
  subtree are no longer read or written; refs left on a remote are ignored and nothing deletes
  them.
- Friendly device labels are optional, mutable display metadata in shared
  `.vulcan/device-names/<device-id>.json` files. They travel through ordinary vault sync when
  tracked by Git, stay outside the rebuildable cache and note frontmatter, and do not affect ref
  names, identity comparisons, or recovery commands. Unknown devices remain identifiable by their
  full IDs.
- The read-only device inventory reports remote safety heads, whether each has a matching local
  recovery ref, retained local recovery refs with no remote head, and names without any backup. A
  local recovery copy marked current only matches the observed remote revision; it does not
  establish integration or permission to delete the backup. The installation-wide
  projection keeps local identity and legacy sync actor distinct, groups recovery evidence by
  registered wiki, and records each wiki's independent remote observation; an unavailable remote
  does not erase local evidence or imply a backup is absent. Explicit offline mode skips remote Git
  observation and reports `not_requested` while retaining local recovery and labels.
- Detached-directory recovery is explicit rather than an automatic repair. `vulcan vault
  recover-git` requires a device-local registration, an intact materialized worktree, an absent
  registered Git directory, and a regular `.git` pointer that still names exactly that directory. It
  recreates an empty detached repository with the recorded platform policy, captures the complete
  worktree under a unique recovery ref, and only then configures and fetches the supplied `origin`;
  it performs no checkout or reset. The recovery ref is kept until an operator deletes it, since it
  may hold the only copy of those commits. A
  mismatched pointer or existing Git directory fails closed. The report names hidden Vulcan ref
  namespaces that may have held unpushed objects which cannot be reconstructed from the shared
  worktree, and warns that uninstalling Termux can remove private Git data while leaving an
  Obsidian-visible shared-storage vault.
