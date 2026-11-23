# Optional daemon workers

Part of the [Git synchronization and daemon architecture](../git-sync-architecture.md).

Semantic automation remains a finite synchronous application workflow. `semantic-auto` records when
an accepted live revision was first observed, exits while its quiet-period debounce is pending,
enforces a maximum batching delay, and composes plan, apply, and optional exact-lease publication
only when due. Cron, Forgejo CI, and the daemon may all invoke this boundary; the app layer does not
own a resident async loop, and dry-run does not advance scheduling state.

The daemon semantic worker is an opt-in low-frequency adapter around that workflow, not another
synchronization engine. Its startup-loaded configuration names an explicit wiki allowlist and
bounded quiet, maximum-wait, and polling intervals; it requires the daemon-owned semantic provider,
skips paused registrations and repositories with active file-tree jobs, reapplies each
registration's Git and provider-network permission profile, and persists only the latest operational
report outside the vault/cache. Blocking provider clients are owned across the async runtime
boundary so their final destruction happens after Tokio shuts down.

Semantic scheduling separates local eligibility from remote validation. Finite idle/deferred calls
use local accepted refs, avoid remote requests, and retain identical durable debounce state. A
changed target resets quiet time but retains the original batching start, including across restart.
Due work validates remote agreement before entering the existing plan/apply/publication guards. The
daemon observes native Git-ref/config hints, sync-job changes, and actual debounce/retry deadlines;
registration and job changes are checked within one second (registry caching may add up to 500 ms).
Error retries use `poll_seconds`. Remote-only changes and missing native hints reconcile every
`max(300, poll_seconds)` seconds, plus in-flight work; unavailable native watches do not disable
reconciliation. The persisted `checked_unix_ms` reports the last persisted evaluation, not every
idle wake. Identical reports are not replaced except for a heartbeat at reconciliation, so
operational freshness remains bounded without idle writes.

Registry runtime clones share a content-validated cache. Runtime reads may reuse a snapshot for at
most 500 ms; each consumer's polling interval adds to external-edit observation latency (at most 1.5
seconds with the default one-second intervals). Reconciliation reads actual bytes, detecting atomic
replacements and same-size, timestamp-preserved edits. Direct/API reads bypass the time window, and
successful API mutations invalidate it. Malformed configuration fails closed and clears the
validated snapshot; startup failure and runtime stop/restart behavior remain unchanged, and repaired
bytes are accepted on the next read/restart. Watcher and notification-listener lifecycle decisions
remain independent.

The daemon conflict worker is the corresponding opt-in resolver adapter for a deliberately
designated node. It requires both a daemon-local wiki allowlist and each vault's device-local
`sync.agent_auto_accept` switch, so configuring a provider alone never authorizes unattended
mutation. “High confidence” is an objective eligibility tier rather than a model-supplied score:
only pending singleton regular-file Markdown or UTF-8 text overlaps with complete base/local/remote
objects and tight per-path and aggregate byte limits are submitted, without broad vault context.
Structural, binary, missing-side, device-state, oversized, non-text, stale, or otherwise ambiguous
groups remain preserved for review. The worker shares the companion's process-local conflict claim,
reapplies Git and provider-network permissions, and enters the existing proposal retention and
automatic approval transaction unchanged; exact output coverage, syntax, links, deletion ceilings,
tree identity, worktree recovery, and remote leases therefore remain mandatory. It skips paused or
actively syncing repositories, processes at most one conflict per wiki pass, persists content-free
outcomes outside the vault/cache, and carries provider-error backoff across restart. Resolution
commits advance canonical live history for other devices; the separate semantic worker may
subsequently organize the already accepted bytes into human-facing branch commits but can never
change them.
