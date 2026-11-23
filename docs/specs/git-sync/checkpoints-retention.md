# Checkpoints, retention, epochs, and publication retries

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

> **Frozen (2026-11-23).** This part belongs to the whole-worktree live mode (`sync.worktree = "all"`/`"paths"`). It stays maintained as shipped but is not extended; once no wiki uses the live mode it is deprecated and removed, and geth takes over replication. New work targets [committed-history-only sync](none-mode.md).

- `vulcan sync checkpoint [<wiki>] --kind recovery|semantic` anchors the currently accepted live
  commit under a unique local `refs/vulcan/checkpoints/<kind>/<ulid>` ref. It requires the profile's
  local, fetched, and pending refs to agree with the exact remote live ref, serializes creation with
  other repository mutations, never advances the semantic branch, and creates no new Git objects.
  Recovery and semantic classes are retention intent; both remain immutable object reachability
  anchors until a later explicit retention workflow removes them.
- `vulcan sync retention-plan [<wiki>]` is the read-only boundary for that later workflow. It first
  requires the same exact accepted-ref and remote agreement as checkpoint creation, then uses typed
  ref enumeration and bounded first-parent traversal to report active-epoch pressure, retain the
  newest configured recovery checkpoints, classify only older recovery refs as expirable, and list
  semantic checkpoints as permanent. Planning never deletes refs or creates objects. In particular,
  it does not claim that deleting checkpoint refs prunes snapshot commits which remain ancestors of
  the canonical live tip; rollover and archive expiry are separately reviewed decisions.
- `vulcan sync retention-apply [<wiki>]` always recomputes under the repository lock and deletes
  each expirable recovery ref only through its exact planned object lease. Its default remains
  checkpoint cleanup only. Explicit `--rollover` additionally requires an over-limit epoch, exact
  accepted/remote agreement, an unchanged captured worktree, no staged state, and no in-progress Git
  operation. It deterministically anchors the old live tip under both
  `refs/vulcan/epochs/live/<profile>/<epoch>` and the fallback remote branch
  `refs/heads/__vulcan-sync/epochs/<profile>/<epoch>`, creates a reproducible parentless commit with
  the identical tree and protocol/archive trailers, adds the reproducible same-tree branch link
  commit on top of it when a published branch tip exists, then changes live only through
  compare-and-swap. Repetition accepts only the same archive/root identities; interruption before
  accepted-ref updates is repaired by ordinary finite sync. Semantic refs are never changed.
- Epoch archive expiry is a separate explicit reduction of the offline reconciliation horizon.
  `--epoch-archives-keep <n>` is at least one and plans against the verified trailer-linked chain,
  newest first; missing local state while a remote archive still exists marks the chain incomplete
  and prevents expiry classification. `--expire-epoch-archives` deletes only planned older entries,
  remote first and local second, with exact object leases. That order leaves a local recovery anchor
  if remote deletion succeeds immediately before interruption, and both completed halves plus
  repeated application converge safely. A root whose local and remote predecessor refs are both
  absent is the intentional truncated boundary. Live and semantic refs are never rewritten, while a
  device older than the chosen horizon receives an explicit error and retains its captured worktree
  rather than being merged as unrelated history.
- A device that edited from the retired epoch does not force old ancestry back into live history. On
  divergence, finite sync scans the bounded first-parent chain for the active epoch root (reading
  commits in batches, and caching the answer for the last live tip under the Git directory so a
  later scan reads only first-parent commits added since; an unrelated or missing cached tip falls
  back to the full scan), fetches the named archive into its matching local durable ref without
  overwriting a different object, and verifies that the archived predecessor and root have the same
  tree. It constructs a private bridge whose parent is the archived predecessor and whose tree is
  the current remote live tip, merges the offline capture against that bridge, and publishes the
  validated result as a one-parent child of the current new-epoch tip. Sequentially reconnecting
  offline devices repeat this operation even after earlier devices have advanced the epoch;
  unresolved content still enters the ordinary preserved-conflict workflow.
- A rejected live-ref compare-and-swap does not immediately fail or busy-spin. The finite Git cycle
  recaptures the worktree, re-queries and fetches the live ref, recomputes ancestry or a merge, and
  retries up to the configured bound with deterministic exponential waits from 25 ms through a 400
  ms cap. Cancellation is observed before and after every wait. Repository serialization is shared
  with resolution and checkpoints; daemon trigger coalescing remains a scheduler concern.
