# Git synchronization and daemon architecture

**Status:** Implemented baseline (Roadmap 12.1–12.14 and the daemon pieces of Phase 10). Since
2026-11-17, replication, device identity, key custody and large-collection storage move to geth
(the Data Platform), so this specification is maintained but no longer extended. The designs for
key custody and management, synced secrets (12.16–12.18), Git-level artifact mergers (12.19), and
selective materialization (12.20) are superseded or on hold; check the roadmap before relying on
them.

**Committed-history-only (2026-11-24).** Sync moves only commits: it fetches, reconciles, and pushes
the checked-out branch, and keeps a device safety snapshot of uncommitted work. The whole-worktree
live mode (`sync.worktree = "all"`) and path-scoped mode (`"paths"`) were removed, together with
the hidden live ref, merge policy, structured resolution, whole-tree validation, conflict
resolution and proposals, checkpoints, retention, epochs, and semantic history. `sync.worktree =
"none"` is accepted as a no-op; the other values and the removed keys are ignored with a config
diagnostic. Conflict records retained from the live mode stay readable and archivable.

This document holds the detailed synchronization, daemon, and packaging contracts that were previously
inlined in the crate-boundary section of `docs/design_document.md`. The design document keeps the
architectural summary and ownership rules; this specification carries the operational detail. When the
two disagree, fix the disagreement rather than choosing one silently.

Related specifications: [unified hosting](unified-hosting.md), [device identity](device-identity.md),
[device key custody](device-key-custody.md), [key management](key-management.md),
[update channels](update-channels.md), and the [companion protocol](../companion_protocol.md).

## Sections

- [Committed-history-only sync](git-sync/none-mode.md): the short contract for sync
- [Daemon runtime, services, packaging, and companion transports](git-sync/daemon-runtime.md)
- [Git engine, ref namespace, and device safety state](git-sync/engine-and-refs.md)
- [Branch lane, safety gates, status, identity, and repository checks](git-sync/branch-lane.md)
- [Legacy conflict records](git-sync/conflicts.md) (read and archive only)
- [Optional daemon workers](git-sync/daemon-workers.md)
