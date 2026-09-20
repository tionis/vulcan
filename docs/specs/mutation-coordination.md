# Mutation coordination and lock audit

This document records the Phase 10.7.4 lock audit. It is both the migration baseline and the
contract for hosted execution. An in-process scheduler may reduce contention, but filesystem locks
remain authoritative because direct CLI commands and multiple processes must remain safe.

## Canonical identities

- A vault lock is identified by the canonical materialized vault root and stored at
  `<vault>/.vulcan/write.lock`. Callers must canonicalize before constructing `VaultPaths`; two path
  aliases must not produce two lock identities. The lock file is device-local coordination state,
  excluded from sync snapshots and worktree-equivalence checks, and acquiring it does not scaffold
  unrelated `.vulcan` files that could invalidate a prepared frontier.
- A repository lock is identified by the canonical Git directory returned by repository discovery
  and stored at `<git-dir>/vulcan-sync/sync.lock`. Linked worktrees that share Git metadata therefore
  share the repository mutation boundary even when their materialized roots differ.
- A durable registration ID is routing metadata, not a substitute for either filesystem identity.
  Re-registering or aliasing a path must not create a second lock domain.

`vulcan_app::execution::ExecutionVaultIdentity` and `ExecutionRepositoryIdentity` carry these
resolved identities into hosted dispatch.

## Required ordering

Every operation that needs more than one lock uses this order:

1. resolve canonical vault and repository identity without holding a mutation lock;
2. enter the bounded in-process vault/repository scheduler;
3. acquire the vault application-write lock;
4. acquire the repository mutation lock if Git metadata or the worktree is involved;
5. open short-lived SQLite/cache write transactions only while needed;
6. release in reverse order.

Preparation that may block on a network, credential helper, model, or human review occurs before
step 2 where possible. The apply phase reacquires the scheduler/filesystem locks and revalidates the
current permission grant, configuration revision, source hashes or Git frontier, and durable
operation identity. Existing sync transaction scope is not narrowed merely to increase throughput.

## Inventory

| Surface | Current cross-process boundary | Scope and contention | Hosted requirement |
| --- | --- | --- | --- |
| Core/app note, property, Bases, refactor, import, maintenance, vector, and scan writes | `vulcan_core::write_lock` | Exclusive/shared advisory lock at `.vulcan/write.lock`; acquisition currently waits in the OS | Use canonical vault identity and enter the scheduler first; retain the same file lock |
| mdbase managed writes | Vault write lock plus durable mdbase journal | Plan/apply revalidates revisions while holding the vault lock; cooperative reads use the shared side | Preserve journal and stale-plan checks; do not treat queue admission as authorization |
| Finite sync | Vault write lock outside `vulcan_sync::RepositoryLock` for initialized vaults | Exclusive vault coordination plus bounded repository wait across capture, fetch, merge, publish, apply, and verification; uninitialized plain Git vaults have no `.vulcan` application lock yet | Retain the full repository transaction scope and the vault-before-repository order |
| Conflict apply and proposal approval | Vault write lock outside the repository lock when `.vulcan/` exists | Serializes recovery capture, frontier validation, publication, worktree apply, and the innermost cache refresh | Retain stale frontier/tree validation and the full transaction scope |
| Semantic apply/publish, checkpoints, retention, devices, advertisements | Repository lock | Serializes metadata/ref changes that do not apply a new tree to the materialized vault | Share the repository scheduler/lock across worktrees using the same Git directory |
| CLI/MCP auto-commit and explicit Git commit | Vulcan repository lock outside Git's index/ref locks | Runs after the originating vault mutation lock has normally been released; serializes with finite sync and other Vulcan Git writers | Enter the repository scheduler and revalidate the candidate path set after acquisition |
| Daemon sync, conflict, and semantic workers | Existing `SyncSupervisor` coalescing plus the same app/repository locks | Supervisor serializes its own sync jobs but is not a general mutation lock | Keep supervisor semantics; all hosted writers additionally use the shared scheduler |
| HTTP, MCP, companion, and future REST adapters | Workflow-dependent app locks | Transport does not itself establish a new lock identity | Construct one execution context and dispatch through the same scheduler/workflow as direct mode |
| SQLite/cache transactions | SQLite locking and, for rebuild/update workflows, vault write lock | Cache is rebuildable and transactions should be short | Always innermost; never wait for network, repository, or scheduler work while a transaction is open |
| Outline publication/pull and integration state | Connector-specific state lock; pull also uses the vault lock | Remote preparation is bounded; local apply is journaled and stale checked | Keep remote preparation outside mutation locks and revalidate the local/remote plan at apply |

## Audit findings carried into implementation

The audit identified three coordination gaps. Finite sync, conflict application, and core
Git/auto-commit entrypoints now use the ordered cross-process locks; hosted adapters resolve
canonical identity before entering the scheduler:

1. ordinary vault writes and finite sync formerly used different cross-process locks, allowing a
   direct write to overlap a sync worktree transaction;
2. auto-commit and explicit Git commit formerly relied only on Git's low-level locks and did not
   participate in the Vulcan repository mutation lock;
3. `VaultPaths::new` does not canonicalize by itself, so adapters resolve identity before lock
   acquisition rather than assuming spelling-equivalent paths coordinate.

These are not reasons to weaken existing locks. The shared mutation guard and hosted scheduler must
bridge the lock domains in the required order, and direct entrypoints must retain equivalent
cross-process protection.

## Hosted scheduler

`vulcan_daemon::mutation_scheduler::MutationScheduler` is the process-wide admission layer owned by
`HostSupervisor`. It has fixed bounds for queued operations, total in-flight operations, and reads
per vault. A vault `RwLock` permits bounded concurrent reads but excludes mutations; canonical
repository keys add a shared mutation lane across distinct worktrees/vault registrations that use
the same Git metadata. Different vaults without a shared repository lane continue independently.

Admission checks cancellation/deadline while waiting. Once the vault and optional repository lanes
are held, the adapter-provided revalidation callback runs before dispatch, so queued permission or
configuration changes fail closed. A returned permit represents only in-process admission; the
workflow must still acquire the filesystem locks listed above in the same order.

## Contention and failure semantics

- Scheduler queue admission is bounded and deadline-aware. Rejection before dispatch is known not
  to have mutated state.
- A filesystem-lock contention error is retryable only when the operation has not begun mutation.
- Cancellation is cooperative. Once synchronous apply has started, response timeout, caller
  disconnect, or dropped async task does not prove the write stopped.
- A write whose completion cannot be established is reported as indeterminate. Clients query the
  durable operation/job status or use the workflow's journal recovery; they never silently retry it
  through direct mode.
- No in-memory queue provides exactly-once execution. Recoverable operations use durable identities
  and journals outside `cache.db`; other mutations use stale-input/idempotency checks where their
  contract provides them.
