# Git synchronization and daemon architecture

**Status:** Implemented baseline (Roadmap 12.1–12.14 and the daemon pieces of Phase 10). Since
2026-11-17, replication, device identity, key custody and large-collection storage move to geth
(the Data Platform), so this specification is maintained but no longer extended. The designs for
key custody and management, synced secrets (12.16–12.18), Git-level artifact mergers (12.19), and
selective materialization (12.20) are superseded or on hold; check the roadmap before relying on
them.

**Freeze (2026-11-23).** Committed-history-only sync (`sync.worktree = "none"`) is the current
mode. The whole-worktree live mode (`"all"`) and path-scoped mode (`"paths"`) are frozen: their
merge policy, structured resolution, conflict records, checkpoints, retention, epochs, and semantic
history stay maintained as shipped, are not extended, and are to be deprecated and removed once no
wiki uses them (`"paths"`, which has no user, first). Frozen parts are marked.

This document holds the detailed synchronization, daemon, and packaging contracts that were previously
inlined in the crate-boundary section of `docs/design_document.md`. The design document keeps the
architectural summary and ownership rules; this specification carries the operational detail. When the
two disagree, fix the disagreement rather than choosing one silently.

Related specifications: [unified hosting](unified-hosting.md), [device identity](device-identity.md),
[device key custody](device-key-custody.md), [key management](key-management.md),
[update channels](update-channels.md), and the [companion protocol](../companion_protocol.md).

## Sections

- [Committed-history-only sync](git-sync/none-mode.md): the short contract for the current mode
- [Daemon runtime, services, packaging, and companion transports](git-sync/daemon-runtime.md)
- [Git engine, ref namespace, and device safety state](git-sync/engine-and-refs.md)
- [Merge policy, structured resolution, and whole-tree validation](git-sync/merge-policy.md) (frozen)
- [Conflict preservation and resolution](git-sync/conflicts.md) (frozen)
- [Checkpoints, retention, epochs, and publication retries](git-sync/checkpoints-retention.md) (frozen)
- [Branch lane, safety gates, status, identity, and worktree application](git-sync/branch-lane.md)
- [Worktree sync modes](git-sync/worktree-modes.md)
- [Semantic history](git-sync/semantic-history.md) (frozen)
- [Optional daemon workers](git-sync/daemon-workers.md)
