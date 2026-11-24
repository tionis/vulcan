# Optional daemon workers

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

Registry runtime clones share a content-validated cache. Runtime reads may reuse a snapshot for at
most 500 ms; each consumer's polling interval adds to external-edit observation latency (at most 1.5
seconds with the default one-second intervals). Reconciliation reads actual bytes, detecting atomic
replacements and same-size, timestamp-preserved edits. Direct/API reads bypass the time window, and
successful API mutations invalidate it. Malformed configuration fails closed and clears the
validated snapshot; startup failure and runtime stop/restart behavior remain unchanged, and repaired
bytes are accepted on the next read/restart. Watcher and notification-listener lifecycle decisions
remain independent.

The daemon conflict worker is an opt-in adapter for a deliberately designated node. Its
daemon-local configuration names an explicit wiki allowlist and a polling interval (default 30
seconds), and it requires the daemon-owned resolution agent, so configuring a provider alone never
authorizes unattended mutation. Its only job is the branch replay described in the
[branch lane](branch-lane.md): when a `rebase` or `guarded-rebase` pull paused on a conflict, it
replays the local commits onto the upstream with the agent's help, and an ordinary sync then
publishes the result. It skips paused or actively syncing repositories, reapplies the registration's
Git and provider-network permissions, and persists content-free outcomes outside the vault and
cache (`replayed_branch` for a completed replay, `declined_replay` for a `head..upstream` range it
will not retry until either side moves), carrying provider-error backoff across restart. Legacy
conflict records are never touched.
