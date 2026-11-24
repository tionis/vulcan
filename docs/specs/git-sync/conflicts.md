# Legacy conflict records

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

> **Legacy (2026-11-24).** Conflict records were written only by the removed whole-worktree live
> mode (`sync.worktree = "all"`/`"paths"`). Committed-history-only sync never creates them: a
> branch merge or rebase that conflicts is aborted and pauses the cycle (see the
> [branch lane](branch-lane.md)). Retained records stay readable and archivable but can no longer
> be resolved; side, file, patch, editor, proposal, format-proposal, and agent resolution were
> removed with the live mode.

- A record describes one preserved divergence of the old live ref: a deterministic conflict ID, the
  merge base and both candidate tips, the conflicted paths with their stable classification
  (overlapping text or binary, delete/modify, rename/rename, directory/file, case collision,
  device-local state, unsupported object, ambiguous), bounded diagnostics, and any batch,
  resolution, supersession, or proposal metadata written while the mode existed. Records live
  beneath the device-local sync state directory, never in the vault or `cache.db`; the side bytes
  stay in Git under the local `refs/vulcan/conflicts/<id>/` refs.
- `vulcan sync conflicts [<id>]` is a read-only projection over that state for the selected path or
  `--wiki <id>`. Listings report the remaining records, an archived count, and an explicitly
  complete-or-truncated 16-path preview; detail exposes the bounded path pages and groups, and
  `--path-offset` plus `--path-limit` request an explicitly bounded page. The companion capability
  contract advertises the same 256-path page ceiling, and the MCP `sync` pack exposes the same
  immutable list and detail. Registered `sync status` and the daemon's reconstructed status count
  unresolved records.
- `vulcan sync conflicts-archive [--older-than-days <n>] [--dry-run]` moves records closed (resolved
  or superseded) at least that long ago, 30 days by default, into a per-repository archive under the
  vault and repository locks; a dry run takes no locks and changes nothing. Sync no longer archives
  automatically. The local ref `refs/vulcan/conflict-archive` names a parentless commit whose root
  lists immutable segments `sNNNNNN` plus a `count` blob; each run writes its batch as one new
  segment holding each archived record under `<first two hex digits>/<id>/`, with its conflicted
  base/local/remote versions as entries pointing at the original Git blobs under `sides/`, so
  contents are kept without copies. Each run compare-and-swaps the ref before removing the records'
  directories, markers, and local conflict refs, so an interrupted run is simply repeated, then
  deletes the archived records' remote refs (`refs/heads/__vulcan-sync/conflicts/<id>/record` and
  published resolution refs) with one listing and one push carrying an exact lease per ref; an
  unreachable remote only defers that cleanup. Detail reads fall back to the archive.
- A device-local index beside `conflicts/` holds one empty marker per possibly-open record and a
  closed marker per resolved or superseded one, so listings and status cost follows the open
  records rather than the repository's conflict history; the read-only listing repairs the index
  from the states it reads.
