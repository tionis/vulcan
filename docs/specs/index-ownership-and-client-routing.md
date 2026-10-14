# Index Ownership and Daemon Client Routing

Status: proposed scope for Roadmap 10.4 and 10.5  
Scope: one index owner per vault across long-lived processes, and routing of finite read commands to
a resident daemon that already keeps the vault indexed  
Non-goals: the full multi-vault REST API (10.3), per-vault argon2 tokens, remote or multi-user
access (Phase 17), routing writes through the daemon, and making any frontend primary. The CLI,
MCP, LSP, sync, publication, and previews are peer clients of the same substrate.

This record refines the [client routing contract](unified-hosting.md#client-routing-contract) in the
unified hosting record. That record stays authoritative for host, service, execution-context, and
lock semantics.

## Current state

**The daemon already indexes.** Since `d7f236ca` the resident daemon runs an index consumer for
every registered vault whose profile includes `markdown_index`. The consumer shares the
registration's observation hub, scans incrementally under the vault write lock, and keeps a
persisted `VaultScanTracker` with a completed-scan barrier
(`vulcan-daemon/src/runtime.rs`, `scan_runtime.rs`). The
[editor integration contract](editor-integration.md) said the daemon only triggers sync; this
record corrects it.

**Six kinds of process index a vault independently.** Each one watches the whole vault recursively
and scans what changed:

| Process | Watcher | Coordination with other indexers |
| --- | --- | --- |
| Resident daemon, registered knowledge vaults | shared observation hub, index consumer | none |
| `vulcan serve` | `vault_http.rs`, invocation-owned | none |
| `vulcan mcp --transport http` | `mcp.rs` index-watch service | none |
| Site and bundle previews | `preview.rs` rebuild watcher (also scans) | none |
| `vulcan lsp` | `.vulcan/editor-watch.lock` lease | among LSP processes only |
| `vulcan index watch` (also `vulcan watch`) | foreground watch loop with creation triggers, hooks, and auto-commit | none |

All scans serialize on the vault write lock, so concurrent indexers are safe. They are wasteful: on
a vault with N indexers, each external edit costs N recursive watchers (a scarce per-user resource on
Linux), N debounce cycles, and N scans, of which N−1 find nothing to do.

**Finite commands pay a cold start every time.** On the development host at 100K notes
(2026-10-08, Roadmap QRY.9), daemon-less `status` takes about 3 s and `query` about 1.2 s, much
of it loading identities and stored fields that a warm `NoteStoreSession` (QRY.6) retains between
requests. Agents and scripts that run many commands pay this for every call. Warm sessions already
serve the hosted HTTP router (`vault_http.rs`), but a finite command has no way to use a resident
daemon's process. Slice 1 measures how much of each command's time a warm session removes.

## Goals

1. Exactly one process keeps a given vault's index fresh at a time. Others follow it.
2. A finite read command against a vault that a live daemon hosts can be answered from the daemon's
   warm session, with results identical to direct execution and no staler.
3. Direct execution remains the default, needs no daemon, and pays no measurable cost when no daemon
   is running.

## Part A: one index owner per vault

Generalize the editor's lease into a vault-wide index-owner lease:

- **Lease.** An advisory OS lock on `.vulcan/index-owner.lock` (device-local, excluded from sync
  like `write.lock` and `editor-watch.lock`). Because it is an OS lock, a crashed holder never
  leaves a stale lease. The holder records its process kind and PID in the file for diagnostics
  only; the lock, not the contents, is authoritative.
- **Holder.** The process that holds the lease runs the vault's indexing watcher and scans.
- **Followers.** Every other background indexer skips watching and follows the store clock
  (`note_store_clock`), as non-holding LSP processes do today. Followers reacquire the lease
  promptly when the holder exits, so indexing never stops while any interested process remains.
- **Participants.** The daemon index consumer, `serve`, MCP HTTP, previews, and the LSP. The
  previews keep their own rebuild watcher, because rebuilding is their purpose. Only their scanning
  defers to the owner.
- **Explicit watch commands** (`vulcan index watch` and `vulcan watch`) exist to run creation
  triggers, scan hooks, and auto-commit. They always watch. They take the lease when it is free, so background indexers defer
  to them, but they never wait for it.
- **Freshness stays proven, not trusted.** A follower never treats the owner's existence as proof of
  freshness. Requests that need a fresh index use `prove_note_store_fresh` and fall back to an
  incremental scan under the write lock, exactly as direct commands do now.
- **Migration.** `editor-watch.lock` becomes an alias for one release: LSP processes take both
  locks, so mixed-version LSPs still elect one owner, then the old file is retired.

Part A is independent of client routing and removes the duplicate watchers by itself. It also
unblocks the 9.40.2 item "skip the watch lease when a daemon keeps the vault's index fresh". Under
this design that item is simply the LSP becoming a follower.

## Part B: routing finite reads to the daemon

### Modes

`--route direct|daemon|auto`, environment variable `VULCAN_ROUTE`, and device config
`[client] route` (default `direct`):

- **`direct`** (default): today's behavior. It neither probes nor contacts the daemon.
- **`daemon`**: routable commands must be answered by the daemon. Absence, version mismatch, an
  unhosted vault, or a narrower daemon ceiling is an error. There is no fallback.
- **`auto`**: routable read commands go to the daemon when it hosts the vault and are executed
  directly otherwise. Reads have no effects beyond rebuildable cache refreshes, so a failure at any
  point, including after dispatch, may fall back to direct execution. This extends the unified
  hosting record's rule, which permits only pre-dispatch fallback, to read-only operations. Writes
  are never auto-routed.

Commands that are not routable (all writes, interactive TUIs, stdio MCP, the LSP) ignore the route
and run directly. In `daemon` mode they say so instead of silently running directly.

### Discovery and identity

1. Read the daemon runtime record under the user state directory. If it is absent, run directly.
   This check must cost no more than one `stat`.
2. Resolve the vault root on the client (home expansion, the working directory, symlinks) to its
   canonical path, and match it against the runtime's registrations. Only registrations with
   `markdown_index` are routable. An unregistered vault runs directly.
3. Negotiate with `GET /capabilities` on the companion listener. The response lists the routable
   operations and their schema versions. A client caches the result per daemon instance in user
   state, so later commands skip this request.

### Transport and authority

- Requests use the existing loopback companion listener and device bearer credential (protocol v1).
  Only the owning user can read that credential, which is the same trust boundary as direct access
  to the vault files. No new token type is needed for local routing.
- Every request carries the caller's effective permission profile, after `--permissions` and vault
  defaults. The daemon executes under the intersection of that profile and the registration's
  profile. If the registration's profile is narrower than the caller's, `auto` runs directly and
  `daemon` fails with the conflicting profiles named. A routed result is never silently narrower or
  wider than the direct result for the same flags.
- The client resolves every environment-dependent input before sending: relative paths, the
  working directory, `VULCAN_FIXED_NOW`, editor and terminal state, and stdin content. Requests are
  fully resolved typed values, never argument vectors.

### Operations and presentation

- An operation is a versioned typed request (`/v1/vaults/{registration}/ops/{operation}`) whose
  response is the exact report JSON that the command prints with `--output json`. The CLI keeps its
  human presentation and applies it to the returned report, so output is byte-identical in both
  modes.
- Report construction for each routed command lives in `vulcan-app`. The daemon runs the same
  workflow as the CLI's direct path. Moving any remaining CLI-only report logic into `vulcan-app` is
  part of each operation's slice, as the boundary rule already requires.
- **A routed operation answers from retained state.** The baseline (below) shows that a warm
  process alone saves little: `status` and `note info` cost the same in a long-lived MCP process
  as in a fresh CLI process. The large gains come only where the host keeps retained state between
  requests, as `serve` does for `query` and DQL. Each routed operation therefore names the
  retained state it reads (a `NoteStoreSession`, retained graph adjacency, a retained task index),
  and its tests assert, through stage counters, that a warm request rebuilds none of it. An
  operation that would only run the direct workflow in another process is not routed.
- **Slow algorithms are fixed directly, not routed.** An operation that is slow in a warm process
  for algorithmic reasons is fixed in the shared workflow first, which benefits every integration
  including direct mode. It is routed only after that fix, and only if retained state still saves
  a meaningful share of its time.
- The candidate operations are `query` (including DQL), `ls`, `search`, `status`, `note get`,
  `note info`, `note outline`, `note backlinks`, `tasks query`, and `graph stats|path|hubs`. Each
  is added only with a parity test, a retained-state assertion, and a measurement.

### Freshness

The daemon must answer at least as freshly as direct execution would at the moment the client sent
the request:

- The request carries the client's send time. Before executing, the daemon proves the store fresh
  (`prove_note_store_fresh`) or waits for a scan generation that started after the send time, using
  the tracker's barrier. Failing both, it runs an incremental scan under the write lock, as the
  direct auto-refresh does.
- `--refresh` flags keep their meaning. A routed `--refresh off` does not wait for freshness.

### Errors

A routed error carries the same error code and message as the direct path, plus a `route` field
(`daemon`) in JSON output. Transport failures in `daemon` mode use a dedicated
`daemon_unavailable` code that names the cause.

## Acceptance

- **Equivalence.** For every routed operation, a differential test runs the same request directly
  and routed against the shared fixture vaults and a generated 10K vault. The JSON must be
  identical. Tests also run with restricted profiles and after external edits that the daemon has
  not yet observed.
- **Overhead with no daemon.** In `auto` mode, a run with no runtime record must be within
  measurement noise of `direct`, with a budget of 1 ms.
- **Speed.** On a 100K-note vault on the reference host, routed `query`, `status`, and `note get`
  must answer within 100 ms at p95 with a fresh index. The record must include direct and routed
  timings, measured the same way. No performance gate counts as passed without that evidence.
- **One owner.** With the daemon, `serve`, MCP HTTP, and two LSP processes on one vault, an external
  edit causes exactly one scan. The holder's exit hands indexing to a follower within the lease
  poll interval.
- **Safety.** Writes stay direct, and `daemon` mode never falls back. Killing the daemon mid-request
  makes `auto` fall back and `daemon` fail with `daemon_unavailable`.

## Slices

1. **Baseline.** Record direct cold timings for the first operation set at 10K and 100K, warm
   in-process session timings for the same requests, and the cost of the runtime-record check.
   This decides which operations are worth routing first. Recorded in the
   [baseline](#baseline) below.
2. **Index-owner lease (Part A).** Implement the lease, convert the five background indexers to
   holders or followers, migrate the LSP lease, and correct the editor contract. Add tests for
   handover, crash release, and exactly one scan per edit. This ships value on its own.
3. **Routed read protocol.** Add the companion `ops` endpoint, capability advertisement, the
   freshness barrier, authority intersection, and the first three operations (`query`, `status`,
   `note get`), with equivalence tests.
4. **CLI routing modes.** Add `--route`, `VULCAN_ROUTE`, `[client] route`, discovery, the
   capability cache, and fallback rules, with `describe` and `help` entries. Review the agent skills
   so agents running many commands know about `auto`.
5. **Remaining operations.** Add the rest of the first operation set, one slice per operation
   group, each with equivalence tests and measurements.
6. **LSP daemon-backed store (9.40.5).** The LSP reads through the routed protocol when a daemon
   hosts the vault. This is optional and follows the slice 1 measurements.

Routed writes, the full REST surface, and remote access stay out of scope. They need the operation
status and authorization work of 10.3 and Phase 17, and nothing above depends on them.

## Baseline

Slice 1 measured the candidate operations on the public MDB fixture on the designated evaluation
host (Intel Core i7-10700, 31 GiB RAM, Debian 13), release builds, with
`scripts/measure_routing_baseline.py`. Records:
[10K](measurements/routing-baseline-10k-3ac06b5f.json) (20 samples),
[100K](measurements/routing-baseline-100k-1a0186bb.json) (10 samples), and the
[`graph hubs` fix at 10K](measurements/graph-hubs-10k-1a0186bb.json). These are diagnostics, not
gate results. Repeated-request p50 in milliseconds:

| Operation | 10K CLI | 10K warm MCP | 10K `serve` | 100K CLI | 100K warm MCP | 100K `serve` |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `query` | 66 | 75 | 2 | 517 | 829 | 5 |
| `query` (DQL) | 51 | 25 | 1 | 433 | 247 | 6 |
| `search` | 29 | 4 | 4 | 175 | 7 | 6 |
| `status` | 127 | 126 | | 1,475 | 1,465 | |
| `note get` | 21 | 14 | | 225 | 199 | |
| `note outline` | 21 | 14 | | 229 | 201 | |
| `note info` | 137 | 119 | | 1,790 | 1,680 | |
| `note backlinks` | 53 | | | 591 | | |
| `ls` | 88 | | | 806 | | |
| `tasks query` | 767 | 447 | | 8,790 | 4,993 | |
| `graph stats` | 377 | | 353 | 5,426 | | 4,802 |
| `graph path` | 329 | | | 4,486 | | |
| `graph hubs` | 11,152 → 388 | | | 5,019 | | |

The process floor (`vulcan --version`) is about 5 ms, and checking for a missing runtime record
costs about 1.3 µs, so the `auto` budget and the 100 ms routed budget are both reachable.

What it shows:

- **Retained state is where routing pays.** `query`, DQL, and `search` fall from 175-517 ms to
  5-7 ms at 100K in a process that retains state between requests. A warm process without retained
  state saves little: `status`, `note get`, and `note info` cost nearly the same as a fresh CLI
  process, and the MCP `query` path is slower than the CLI's. Hence the retained-state rule under
  [Operations and presentation](#operations-and-presentation).
- **Single-note reads scale with the vault.** `note get` and `note outline` read one note but take
  about ten times longer at 100K than at 10K, `note info` takes 1.8 s, and `note backlinks` 0.6 s.
  These are shared-path costs for every integration and are fixed directly before any routing.
- **Whole-graph operations take about 5 s at 100K**, warm or not: `graph stats` costs nearly the
  same in `serve`. They need retained adjacency or incremental graph state before routing helps.
- **`graph hubs` was quadratic.** It recounted every edge for each note (O(notes × edges)): about
  11 s at 10K and more than 500 s per call at 100K. Fixed in `1a0186bb`; its 10K row shows the
  time before and after, and its 100K row is from the fixed binary.
- **`tasks query` is proportional to its output.** The fixture's 3,333 `type: task` notes are
  TaskNotes task files, and `not done` returns 2,445 of them (3 MB of JSON) at 10K.

## Open questions

- Should `auto` become the default once the equivalence and speed gates pass, or stay opt-in, with
  agents setting `VULCAN_ROUTE=auto`?
- Should the explicit watch commands participate fully, deferring their scans to an owner and
  running hooks when the store clock advances, instead of always watching?
