# Vault-owned schedules and execution trust

Status: implementation contract for Roadmap 10.8, 12.17.6, 15.7, and 19.10. Schema v1 below is
frozen and shipped with repository trust (`vulcan_core::schedules`, `vulcan_app::schedules`, the
daemon's `worker.schedules`, and `vulcan schedule`); trusted-commit, App targets, and the registry
extensions remain planned. User guide: `docs/guide/schedules.md`.

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

## Schema v1

Status: frozen 2026-10-16 after review, and implemented. This section turns the contract above into
the concrete first slice.

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
- **`nodes`** selects devices by their key-derived device ID (`vdev1_...`; letters, digits, `-`,
  and `_`). A device runs a job only when its own
  ID is listed. Several IDs mean independent runs on each, not a shared queue.
- **`authority`** is a permission profile, capped by the registration's ceiling. A synced change
  can choose a narrower profile, never a wider one than the device allows. The ceiling's profile
  must be a built-in or defined in the device-local `.vulcan/config.local.toml`; profiles in the
  synced `.vulcan/config.toml`, including redefinitions of built-in names, never apply to it.
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

An occurrence found `dispatched` after a crash is retried only if its target is idempotent and the
job has retries left, and otherwise reported as `interrupted`. A failed run is retried after 30
seconds within the same budget. A revision is activated (its watermark set) when the scheduler
first sees it, so a new or changed job never runs occurrences from before then. Manual runs are
recorded with `manual = 1` and leave the watermark alone. The scheduler rereads registrations and
vault configuration at least every 30 seconds; a run whose job is disabled, removed, reassigned,
or changed, or that exceeds `max_runtime_seconds`, is asked to stop through its cancellation
token. `scan` cannot stop mid-scan and `batch-commit` stops at its next check, so both finish
their current step. The same token cancels a run's wait for the host's per-vault mutation permit,
so a cancelled run (or shutdown) never waits behind a hosted write, and a run cancelled before it
holds the permit stops without starting its target.

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
plugs into the same authorizer without changing scheduler types; see
[Trusted-commit v1](#trusted-commit-v1).

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

## Trusted-commit v1

Status: frozen 2026-10-17 after review, and implemented (`vulcan_app::schedule_trust`, the
scheduler's trust targets, and `vulcan schedule trust`). Signatures are SSH commit signatures only;
OpenPGP signatures are not accepted. This section makes the policy above
concrete for static keys; the sigchain adapter below replaces only where the keys come from.

### Keys and device configuration

An administrator enables the policy per registered vault in the device configuration, which no
synced file can change:

```toml
[[vault]]
id = "mimir"
# ...
[vault.execution_trust]
policy = "trusted-commit"                  # default "repository-trust"
allowed_signers = "/home/me/.config/vulcan/execution-signers"
principals = ["admin"]                     # which entries of that file may authorize
source = "HEAD"                            # ref whose tip is resolved; default HEAD
```

- **A dedicated role.** The allowed-signers file lists execution signers only. It is a separate
  file from Git's own `gpg.ssh.allowedSignersFile`, and from the 12.17 registry administrators,
  so trusting a key for one purpose never trusts it for another. Vulcan refuses the configuration
  when an entry's key is this device's identity key or a device key the vault's device registry
  knows, because automatic sync commits must never count as authorization.
- **Protected and outside the vault.** The file must be outside the vault and its Git work tree,
  owned by the current user, and not writable by group or others (on Unix). A file that fails
  these checks blocks every job of the vault, rather than being ignored.
- **Format.** OpenSSH `allowed_signers` lines: principals, optional `namespaces="git"`, key type,
  and key. Lines naming `cert-authority`, `valid-after`, or `valid-before` are rejected with a
  diagnostic in v1 instead of being half-supported. Only entries whose principal is listed in
  `principals` count.
- **Signatures are verified in-process** (the `ssh-key` crate's SSHSIG verification with namespace
  `git`), against the commit object's `gpgsig` header and payload. Vulcan runs neither
  `ssh-keygen` nor `gpg` nor any program Git configuration names, so neither the repository nor
  the candidate commits can choose the verifier. Supported key types match the crate features:
  Ed25519 first, including keys held on hardware tokens; `sk-` (FIDO) keys and RSA follow if
  needed. OpenPGP-signed commits are not accepted: an administrator who signs with OpenPGP today
  sets `gpg.format = ssh` for the vault repository.

A plain (non-Git) vault configured for `trusted-commit` blocks every job as unsupported.

### What is authorized

Each job is authorized separately, so an unsigned change blocks only the jobs it touches. A job's
**execution definition** is the canonical JSON of:

- its `[[schedules.jobs]]` entry;
- the shared definition of the permission profile its `authority` names (device-local overrides
  still apply at run time, and the device ceiling still caps the result);
- the configuration its target declares as execution configuration: none for `scan`, and the
  whole `[git]` table for `batch-commit`, because that table chooses the commit strategy and the
  message agent, which reaches the network. Later targets declare their own sections when they
  are added.

The definition digest is the BLAKE3 hash of that JSON. A job absent from a commit has the digest
of "absent", so removing or disabling a job is a definition change like any other.

Definitions are read from Git objects, never the working tree: `git show <commit>:<prefix>.vulcan/config.toml`,
where `<prefix>` is the vault's path inside the repository (`docs/` for a vault in a
subdirectory). Uncommitted edits are reported as pending and have no effect until committed and
signed. The run receives the authorized snapshot, so `batch-commit` uses the signed `[git]` table
even if the working tree changed it. The device ceiling is resolved as under repository trust, from
built-ins and local configuration only, so not even a signed edit can widen it. Targets never run working-tree
code under trusted-commit: `scan` runs the scan alone, without scan-triggered auto-commit or plugin
events, and `batch-commit` runs without commit plugins. Each run resolves again at dispatch.

### Resolution

On each scheduler reload, the verifier resolves `source` to its tip `T` and the vault's accepted
checkpoint `C` from device state:

1. **History must contain the checkpoint.** If `C` is not an ancestor of `T` or equal to it (a
   force-push, a reset, or another fork), every job of the vault is blocked as `rolled-back`. Only
   an explicit `trust accept` moves past this.
2. **Find the authorizing frontier.** Among the commits reachable from `T` but not from `C`, take
   those signed by an allowed signer. The frontier is the set of those that are not ancestors of
   another one (`git merge-base --independent`). With none, the frontier is `C` itself.
3. **Match every job.** A job is authorized when its digest at `T` equals its digest at every
   frontier commit. An unsigned commit that changes a job after the last signature blocks that job
   as `unsigned-change`. An unsigned merge of two differently signed definitions blocks it as
   `conflicted`. An old definition copied back in an unsigned commit differs from the frontier's,
   so it is blocked too, which is the rollback case the policy names.
4. **Advance the checkpoint.** When the frontier is a single commit that descends from `C`, it
   becomes the new checkpoint, even if some jobs are blocked: it is an authorized state, and
   accepting it closes off rollback to anything older.

Unsigned commits that leave execution definitions alone (note edits, auto-commits, sync
snapshots) never block anything. A signed merge authorizes the definition in its tree. Every
reload resolves again, but the walk covers only the commits since the checkpoint, which keeps
advancing, so an unchanged tip costs a few plumbing calls.

A signer that loses trust (removed from the file or from `principals`) no longer signs anything.
If the checkpoint itself was theirs, the jobs whose definitions it authorized stay blocked until a
remaining signer signs a descendant. Falling back to an older signed commit would be a rollback.
Each reload rechecks, and a run whose job loses authorization is cancelled cooperatively, as with
disablement.

### State, bootstrap, and recovery

The checkpoint lives in `daemon/schedules.sqlite`, beside the occurrence ledger:
`trust_checkpoints(vault, policy_generation, commit, signer, accepted_ms, how)`, where `how` is
`enabled`, `advanced`, or `accepted`. The policy generation is the digest of the trust
configuration and the allowed-signers file's contents, so editing either is visible in status.

- **Bootstrap is explicit.** `vulcan schedule trust enable` writes the device configuration and
  pins the current frontier as the first checkpoint, printing the commit and signer. The
  administrator who runs it is the one who would otherwise approve that state. With no signed
  commit yet, jobs stay blocked as `unsigned` until one arrives.
- **Missing state blocks.** If the policy is configured but no checkpoint exists (the state
  directory was lost, or another device's configuration was copied), every job is blocked as
  `no-checkpoint`. Vulcan does not re-bootstrap silently, because that would accept any old signed
  commit an attacker presents.
- **Restored state** from an older backup can only pin an older checkpoint. Rollback to states
  between it and the newest one is then possible until the next advance. Status shows the
  checkpoint's age, and the guide tells administrators to rerun `trust accept` after a restore.
- **Unreachable remotes** change nothing: authority is resolved from local objects and lasts until
  the next resolution.

### Commands

- `vulcan schedule trust status`: the policy, signers and principals, checkpoint, frontier, and
  each job's state (`authorized`, `unsigned-change`, `conflicted`, `rolled-back`,
  `no-checkpoint`, `unsupported`, or `pending` for uncommitted edits), with the commits involved.
- `vulcan schedule trust enable --allowed-signers <file> --principal <name>… [--source <ref>]`
  and `trust disable` (back to `repository-trust`), with `--dry-run`.
- `vulcan schedule trust accept [<commit>]`: pins a signed commit as the checkpoint after
  rollback, a fork, or restored state. The commit must be signed by an allowed signer, and the
  command shows what changes for each job before accepting. `--dry-run` is supported.

Administrators sign with plain Git (`git commit -S`, or `git merge -S` for a merge); Vulcan needs no
signing command. `schedule list`, `show`, and `run` report the trust decision they already carry,
with the job's state in place of `repository-trust`.

### Tests

Signed, unsigned, and mixed histories built in temporary repositories with fixture keys: an
unsigned note edit after a signed definition; an unsigned job change, removal, and re-addition of
an old definition; signed and unsigned merges of conflicting definitions; force-push below the
checkpoint; signer removal; a device key listed as a signer; a writable or in-vault signers file;
missing and restored checkpoints; a vault in a repository subdirectory; uncommitted edits;
`batch-commit` reading the signed `[git]` table. Malformed signature headers and unsupported key
types fail closed.

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
