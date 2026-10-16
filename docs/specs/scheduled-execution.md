# Vault-owned schedules and execution trust

Status: planned implementation contract for Roadmap 10.8, 12.17.6, 15.7, and 19.10.
No commands or scheduling configuration in this document are claimed to ship today.

## Scope and ownership

The primary workload is a finite App operation such as refreshing RSS subscriptions and
capturing articles as Markdown. Support a standalone local vault and a vault replicated
between a home server, laptops, and phones with the same execution model.

Canonical vault files own job definitions, enabled state, inputs, trigger policy, and exact
target node IDs. The daemon live-reloads those files. There is no second device-local job
enablement switch or per-update confirmation prompt. One target node is the default;
multiple explicitly selected nodes mean independent executions, not a shared queue.
Stable device identity is local and reuses the device-identity contract, including legacy
identity compatibility; labels are presentation only. Cloning a vault does not clone a node.

Device configuration still owns daemon startup, vault registration, execution permission
ceilings, credentials, and optional signature enforcement/trust roots. Vault configuration
cannot weaken these boundaries. A plain non-Git vault supports repository-trust execution;
signed execution requires Git objects and reports unsupported configuration without them.

Use the shared host and finite synchronous app workflows. The daemon owns one bounded
scheduling service, not one timer thread per App. Preserve domain-specific job ledgers and
recovery behavior. Do not build a coordinator, distributed worker fleet, Git claim files,
leases, election, automatic failover, or a universal replacement job queue for this scope.

## Schedule and execution contract

Freeze a versioned schedule schema before implementation: stable job ID, vault/instance,
typed built-in operation or declared App job, validated non-secret input, package/config
identity, explicit service authority, enabled state, target node IDs, trigger, and limits.
Use vault-visible canonical configuration, not excluded `.vulcan` runtime state. Select its
exact path and integrate with App settings during schema work rather than inventing a second
settings framework. Background authority remains distinct from an interactive session.

Start with fixed intervals and five-field cron plus an explicit IANA timezone. Define interval
anchors, cron day-field semantics, DST gaps/repeated times, and clock-jump behavior. Default
to coalescing downtime into one refresh and no overlapping invocation of a job, with at most
one pending follow-up. Bound retries, queue size, history, run duration, and per-vault work.
One-shot scheduling and elaborate catch-up policies are follow-ons only if needed.

Persist occurrence identity `(job ID, definition revision, scheduled UTC instant)` before
dispatch, with replay-safe handoff to any separate domain ledger. Keep execution evidence,
deduplication watermarks, and accepted trust state outside `cache.db` and outside file sync;
history pruning must not re-enable old occurrences. Retry interrupted work only when the
operation explicitly supports idempotent replay. Do not promise exactly-once effects.
Dropping an async task does not terminate synchronous mutation; cancellation is cooperative.

Configuration reconciliation validates complete bounded snapshots before activation.
Running calls keep their original immutable code/configuration. Disablement, reassignment,
or lost authority stops new claims and requests cooperative cancellation. Recheck authority
at dispatch and mutation boundaries. Malformed/untrusted execution updates block affected
jobs visibly rather than silently continuing stale automation; if scope cannot be determined,
block the vault's scheduled dispatch. Ordinary note edits do not invalidate schedules.

Manual finite workflows remain available without a daemon, subject to the same applicable
execution trust checks. The schedule service requires the daemon and never starts one
implicitly. External cron/native timers can invoke existing finite commands, but are not a
second owner of the same daemon schedule or an escape from execution trust.

## Schema v1 proposal (for review)

Status: proposed, not frozen. This section turns the contract above into the concrete first
slice. Nothing here ships until it is reviewed and frozen.

### Where jobs live

Jobs live in the vault's shared configuration, `.vulcan/config.toml`, under `[[schedules.jobs]]`.
That file is already vault-visible and kept by the default `.vulcan/.gitignore`, so jobs travel
with Git and Vulcan file-tree sync like every other shared setting, and `vulcan config` can show
and edit them. Jobs in `.vulcan/config.local.toml` are rejected with a diagnostic: a device-local
file must not create or enable a job, because enabling is the vault's decision and the device's
only say is its ceilings. A separate committed file was considered and rejected, because it would
need its own gitignore exception, loader, and editor.

```toml
[schedules]
version = 1

[[schedules.jobs]]
id = "nightly-commit"              # stable; 1-64 of [a-z0-9_-]; never reused for another job
enabled = true
target = { builtin = "batch-commit" }
input = {}                         # validated per target; no secrets
nodes = ["<device id>"]            # exact IDs from `vulcan device show`; at least one
trigger = { cron = "30 2 * * *", timezone = "Europe/Berlin" }
# or: trigger = { every = "15m" }  # anchored at the Unix epoch unless `anchor` is given
authority = "unrestricted"         # permission profile the job runs under
limits = { max_runtime_seconds = 600, retries = 0 }
```

- **`target`** names a typed operation. The first built-ins are finite workflows that already
  exist: `scan` (incremental, or `{ full = true }` in `input`) and `batch-commit` (the
  `vulcan git batch-commit` pass). `outline-publish` and `site-build` follow once their inputs
  are classified as execution configuration. App jobs (`{ app = ..., job = ... }`) come with
  Phase 19.10 and use the same fields.
- **`nodes`** selects devices by their key-derived device ID. A device runs a job only when its own
  ID is listed. Several IDs mean independent runs on each, not a shared queue.
- **`authority`** is a permission profile, capped by the registration's ceiling. A synced change
  can choose a narrower profile, never a wider one than the device allows.
- **`limits`**: `max_runtime_seconds` (1 to 86,400, default 3,600) is enforced by cooperative
  cancellation. `retries` (0 to 5, default 0) applies only to targets that declare themselves
  idempotent; `scan` and `batch-commit` do.
- **Definition revision** is the BLAKE3 digest of the job's canonical JSON. A changed definition is
  a new revision, so its occurrences never collide with the old one's.

### Time

- **Intervals** (`every`, 1 minute to 31 days) fire at `anchor + k × every`.
- **Cron** is the classic five fields (minute, hour, day of month, month, day of week). Lists,
  ranges, steps, and `*` are supported; names and `@` shortcuts are not in v1. When both day
  fields are restricted, either may match, as in Vixie cron. `timezone` is a required IANA name.
- **DST.** A time inside a spring-forward gap fires at the first valid instant after the gap. A
  time that repeats in the fall fires once, at its first occurrence.
- **Missed runs coalesce.** After downtime, suspend, or a forward clock jump, only the most recent
  missed occurrence runs. A backward jump never repeats an occurrence: a per-job watermark records
  the latest dispatched instant.
- **No overlap.** While a job runs, a due occurrence waits as the single pending follow-up; later
  ones coalesce into it.
- The scheduler is one supervised daemon service with an injected clock, so all of this is tested
  without real time.

### State

Occurrences live in `daemon/schedules.sqlite` in the device state directory, outside `cache.db`
and outside sync:

- `occurrences(job_id, revision, scheduled_utc, state, started_ms, finished_ms, outcome)` with
  `(job_id, revision, scheduled_utc)` unique, written as `dispatched` before the workflow starts;
- `watermarks(job_id, revision, last_scheduled_utc)`, never pruned while the job exists;
- history pruned to the newest 200 occurrences per job, or 30 days.

An occurrence found `dispatched` after a crash is retried only if its target is idempotent, and
otherwise reported as `interrupted`.

### Commands

`vulcan schedule list`, `show <id>`, `next <id> [--count N]` (the next local and UTC times),
`history <id>`, `run <id> [--dry-run]` (once, now, through the same authorizer; works without the
daemon), and `enable <id>` / `disable <id>` (`--dry-run`), which edit the shared configuration.
All have JSON output. Status shows each job's active and pending revision, whether this device is
a target, the trust decision, next run, last success or error, and a running job's cancellation.

### Trust staging

v1 ships the scheduler with `repository-trust` and the shared synchronous execution-authorizer
boundary, which returns the verified execution identity. `trusted-commit` (static
`allowed_signers`, exact-commit resolution, rollback and fork checks) is the next slice. It
plugs into the same authorizer without changing scheduler types.

## Two trust policies, one execution path

`repository-trust` accepts incoming execution definitions as configured, including local
uncommitted edits, while preserving package validation, sandbox limits, permissions, and
secret/network policy. Synced changes cannot raise the device permission ceiling.

`trusted-commit` requires an execution configuration authorized by a trusted admin signature.
The locally configured source ref/selection rule resolves to an exact commit. Verify native
Git SSH signatures against a protected static `allowed_signers` source first. Enforce signer
principal, namespace, repository/vault scope, and execution purpose in the Vulcan verifier;
an allowed-signers file alone cannot express all those constraints. Do not invoke
candidate-supplied hooks, filters, verifier commands, or schemas during verification.

Authorize the complete execution definition at that commit, not each file's last modifying
commit. Bind App package digests, entrypoints, execution settings, capture rules, and authority
requests to immutable bytes. Resolve external package blobs by their authorized digest and
run the normal complete package validation before use. Signature verification does not grant
new capabilities or replace package-publisher signature policy. Working-tree code/config,
device overrides, dynamic imports, and nested invocations cannot substitute unsigned bytes.
An invocation's captured identity must cover its executable/configuration dependency closure.

Ordinary current wiki content and fetched articles remain mutable input, never implicit code.
Classify settings that select network sources, destinations, capture behavior, or execution
authority as execution configuration; operational secrets are separately resolved local handles.
Avoid per-record signature checks for ordinary data or signing every generated RSS note.

Unsigned content commits may follow an authorized execution revision without requiring every
note edit to be admin-signed. The resolver must prove that the selected execution definition
still matches the accepted signed snapshot; changed execution dependencies require a newly
authorized snapshot. Never walk backward to silently run an old job after an unsigned
disablement or update. Conflicting definitions block dispatch until resolved.

Signatures authorize inherited tree content, so a signed merge is an explicit approval of its
execution definition. Automatic sync-device signatures and signed capture/semantic history
are not admin authorization. This consumer needs a scoped execution-authorizer role; Phase
12.17's registry-administration keys currently have a distinct, incompatible role. Specify an
explicit role extension or separate execution keys, never silently reuse registry-admin custody.

Retain the accepted execution checkpoint and policy generation durably. Define ancestry and
configuration-digest checks to reject known rollback/forks, including an old definition copied
under a new unsigned descendant. A deliberate rollback needs fresh authorization or an explicit
local trust reset. First bootstrap, missing/restored security state, key removal, and policy
changes need inspectable handling. Unreachable remotes do not expire accepted authority;
known revocation blocks new use and cancels affected calls cooperatively. Report unavailable,
untrusted, stale, conflicted, and blocked states distinctly. No clock-based trust lease is required.

## Sigchain evolution

Extend the existing [key registry](key-management.md), not a separate identity or trust engine.
Start with a small synchronous execution-authorizer interface returning the exact verified
execution identity, policy generation, signer, and evidence. Static allowed signers and a future
accepted-registry adapter use that same interface; scheduling and App invocation do not change.

The current registry v1 uses a dedicated Git repository and linear signed history. A future
versioned transport profile may embed several isolated registries/sigchains in any repository
or vault. Each keeps its own immutable registry ID, authenticated transition history, pinned
root/checkpoint, scope, and accepted-state journal. Define canonical paths/ref or immutable
record encoding and ensure required evidence travels through both ordinary Git and Vulcan
file-tree sync. A mutable folder of keys or an unrelated outer commit signature is insufficient.
Do not impose the registry's single-parent rule on ordinary wiki history or text-merge chains.

Local bootstrap pins which exact registry may authorize which vault/execution scope. Discovery
does not establish trust; adding a chain cannot authorize itself. Never union chains implicitly
or match principals by unqualified names. Preserve parent-admin eligibility, signed rotation,
revocation, retained evidence, pending-versus-accepted changes, and fork/rollback rejection.
Apply received revocations atomically; deny affected execution if projection/activation fails.
Disconnected devices retain last accepted authority with explicitly unbounded revocation delay.
This adapter and transport profile do not block static-key scheduling.

## RSS ingestion and node handoff

Portable subscriptions and capture rules retain stable IDs in canonical vault records. Captured
article bindings needed for deduplication and reconciliation must travel with the vault, using
the external-document binding model outside `cache.db`; local ledgers may index them but are
not their only copy. Do not add implementation-only sync markers to note frontmatter. User-facing
source provenance is separate. ETags, fetched bodies, and indexes remain discardable; read/star
state may remain device-local.

Fetch and prepare outside mutation locks, then recheck source identity, bindings, permissions,
and note revisions under the shared journaled mutation workflow. Persist content and canonical
bindings recoverably before advancing import checkpoints. Default capture is import-once;
source refresh of captured notes requires an explicit policy preserving user edits. Feed removal
does not delete notes. Reused IDs and uncertain duplicates produce diagnostics/review.

Reassign a runner by changing the vault definition. Quiesce the old node when reachable and
converge notes/bindings before the new node refreshes. An offline former owner may keep running
its last accepted assignment; synced configuration cannot revoke it instantly. Duplicate-aware
imports and ordinary preserved sync conflicts handle overlap without claiming global exclusion.

## Delivery and verification

1. Freeze schema, settings classification, trust selection, and lifecycle contracts. Reuse host
   supervision/mutation coordination and the authorizer boundary from the first implementation.
2. Ship vault-owned scheduling with repository trust and static-key enforcement, bounded retained
   execution, live reload, status/history, and preview of upcoming local/UTC occurrences.
3. Integrate declared App jobs and prove RSS-to-Markdown refresh plus node handoff. No duplicate
   Feed Reader timer or private-only import mapping should need replacing later.
4. Add scoped execution authorization to accepted registries and the embedded multi-chain transport
   as independently gated extensions of Phase 12.17, with the same scheduler contract.

Use injected clocks for intervals, DST, resume, and clock jumps. Cover crash-before/after dispatch,
interrupted import, duplicate triggers, cross-process writes, malformed reload, reassignment,
two offline runners, portable bindings, uncommitted code, signed/unsigned merges, dependency
substitution, rollback, key removal, and permission changes while queued/running. Sigchain gates
add cross-chain substitution, malicious bootstrap, invalid intermediate transitions, missing
history, concurrent signed changes, delayed revocation, and projection failure.

Document standalone/non-Git, home-server plus Git-sync, signed-admin, and offline handoff examples.
Expose active/pending revision, target/local identity, trust reason, last success/error, next run,
and cancellation state without secrets. CLI mutation previews and JSON match daemon reports.
Update bundled configuration/sync/Git/diagnostic skills when behavior ships, not while it is planned.
