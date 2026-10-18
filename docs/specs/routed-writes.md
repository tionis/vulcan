# Routed Writes

Status: not pursued (2026-10-18). Slice 1 found no gain: a daemon-side write costs the same as a
direct one, because the cost is the shared write path's post-write cache refresh
([measurement](measurements/routed-writes-baseline.json)). That path is fixed directly instead.  
Scope proposed for Roadmap 10.5 (the two open routing items)  
Scope: sending finite CLI write commands to a resident daemon that hosts the vault, without ever
applying a write twice or silently falling back after it may have run  
Non-goals: remote or multi-user access (Phase 17), interactive commands (TUIs, `$EDITOR` flows,
template prompts), making `auto` the default, and writes that already have a daemon owner (sync
jobs, scans)

This record extends [routing finite reads](index-ownership-and-client-routing.md#part-b-routing-finite-reads-to-the-daemon)
to writes. The [client routing contract](unified-hosting.md#client-routing-contract) stays
authoritative: fallback is allowed only before dispatch, and a write that may have reached the
daemon is resolved by asking for its status, never by running it again directly. The REST
[idempotency rules](daemon-rest-api.md#writes) already implement the daemon side of that contract
for REST writes; routed CLI writes reuse them.

## Why route writes at all

A direct write today proves the cache fresh by walking the vault (about 100 ms at 100K notes),
takes the cross-process write lock, writes, refreshes the cache incrementally, and may auto-commit.
When a daemon hosts the vault it already knows the cache is fresh through its watcher, holds the
host's per-vault mutation permit that MCP and REST writes queue on, and keeps a warm session. A
routed write could therefore skip the freshness walk and serialize in one queue with every hosted
writer instead of contending for the file lock from outside.

These gains are expected, not measured. Slice 1 measures them; if a routed write saves little over
a direct one, writes stay direct and this record closes without further slices, as the read
record's retained-state rule would also require.

## Modes

Writes route only when explicitly enabled, by `--route-writes` or `[client] route_writes = true`.
`--route`, `VULCAN_ROUTE`, and `[client] route` keep meaning reads only: the bundled `vault-query`
skill tells agents to set `VULCAN_ROUTE=auto` for read-heavy work, and that must not quietly expose
them to indeterminate write outcomes they were never told about. Once enabled, writes take the
read route's mode with write-specific rules:

- **`direct`** (default): unchanged.
- **`daemon`**: a routable write must be answered by the daemon. Absence, version mismatch, an
  unhosted vault, or a narrower daemon ceiling is an error before anything is sent.
- **`auto`**: a routable write goes to the daemon when it hosts the vault. It falls back to direct
  execution **only** when the failure is known to precede dispatch: no runtime record, connection
  refused, capability or version mismatch, or an unhosted or narrower registration. Once the
  request may have been sent, the write is never run directly (see [Outcomes](#outcomes)).

A dry run (`--dry-run`) is read-only, so it follows the read rules and may fall back at any point.

Folding the switch into `route` is a later decision, taken only after the fault-injection gate and
the slice 1 measurement pass. Starting separate keeps that open; starting merged and splitting
later would break setups that relied on it.

## Outcomes

Every routed write carries an idempotency key the client generates per invocation (a ULID). The
daemon records the key as pending before the workflow runs and completed with the response after,
exactly as for REST writes, so the same request under the same key returns the stored response and
never runs twice. The client classifies what happened:

| Outcome | When | Client behavior |
| --- | --- | --- |
| Not dispatched | No connection, capability/version mismatch, or refusal before the request was accepted | `auto`: run directly. `daemon`: fail with `daemon_unavailable`. |
| Completed | The daemon answered with the report or a workflow error | Print it as the direct path would, with the same exit code. |
| Indeterminate | The connection failed or timed out after the request may have been sent | Ask for the key's status (below) for a bounded time. If it resolves, print that outcome. Otherwise fail with error code `write_indeterminate` and exit code 3, naming the key and how to inspect the vault. Never run directly. |

Exit code 3 is reserved for this outcome. The CLI uses 1 for failures and 2 for completed checks
with issues; an indeterminate write is neither, and a script must not treat it like a refusal (safe
to retry) or a success. A temporary-failure code is deliberately avoided, since it reads as "retry",
the one thing a caller must not do.

**Operation status.** `GET /{id}/ops/write-status/{key}`, scoped to the principal, returns `absent`
(never received: safe to retry), `pending` (still running: keep waiting), `completed` (with the
stored response), or `unknown` (the daemon stopped while the write was pending). Only `absent`
permits a retry, and the retry reuses the same key, so even a race returns the original outcome.
`unknown` is surfaced to the user as indeterminate: the write may have partly applied, and the
message points at `vulcan git diff` (or the note) to check. This is the durable unknown-write
recovery the unified hosting record calls for.

## Requests

- A routed write is a versioned typed request, `POST /{id}/ops/<operation>`, built by
  `vulcan_app::routed` from fully resolved values: paths relative to the vault, content already read
  from files or stdin, the resolved time (`VULCAN_FIXED_NOW`), and the caller's flags including
  `no_commit`. Requests are never argument vectors.
- The response is the exact report JSON the command prints with `--output json`, so the CLI renders
  human output from it and both modes print the same bytes.
- The daemon runs the same `vulcan-app` workflow as the direct path, behind the mutation permit, so
  profile checks, cache refresh, plugin hooks, and auto-commit behave identically. Moving any
  CLI-only write logic into `vulcan-app` is part of each operation's slice.
- **Authority.** The request carries the caller's selected profile, which must be this device's own
  ([device-local limits](daemon-rest-api.md)); the daemon runs under it intersected with the
  registration's profile. If the registration is narrower, `auto` runs directly before dispatch and
  `daemon` fails naming both profiles. A routed write is never silently narrower or wider than the
  direct one.
- **Freshness.** Before running, the daemon passes the same index barrier routed reads use, so a
  write sees every change made before the request, as a direct write does after its freshness proof.

## Which writes

Candidates, in the order slices take them: `note append`, `inbox`, `note create`, `note set`,
`note patch`, `note delete`, `tasks complete|create|reschedule`, `note update`/`unset`, and
`refactor move`. Each needs a parity test, a fault-injection test, and a measurement.

Not routed: `scan` and `index` commands (the daemon owns indexing), `sync` (already daemon jobs),
`config` and `vault` registry edits (device-local and small), `git` commands, anything that opens an
editor or a TUI, template rendering that needs prompts, and plugin-run commands whose environment
the client cannot fully resolve.

## Acceptance

1. **Parity.** For each routed write, JSON and human output, exit codes, the resulting files, the
   cache, and the auto-commit are identical to direct execution, in an end-to-end test like the
   read routing test.
2. **Never twice.** Fault injection on a non-idempotent write (`note append`): dropping the
   connection after send, killing the daemon before and after the workflow finishes, and a
   response timeout. The text is appended exactly once or not at all, the client reports completed
   or indeterminate, and `auto` never re-runs it directly.
3. **Pre-dispatch fallback only.** `auto` falls back on each pre-dispatch failure and on none
   after dispatch.
4. **No cost when unused.** `direct` and `auto` without a daemon stay within the read record's
   budget (one `stat` for the runtime record).
5. **Measured benefit.** Slice 1's measurement shows a routed write saves time over direct at 10K
   and 100K, or routing writes stops there.

## Slices

1. Measure direct versus daemon-side cost of `note append` and `note set` at 10K and 100K (the
   REST write routes already run the daemon side), and decide whether to continue.
2. The write-status route and client outcome classification, with the fault-injection harness, on
   `note append`.
3. The remaining candidates, one operation per slice.

## Outcome

Slice 1 (2026-10-18, [record](measurements/routed-writes-baseline.json)): at 100K notes a direct
`note append` takes about 1,350 ms and the same append through the daemon's REST route about
1,460 ms; `note set` 1,010 against 1,440 ms; at 10K both sides take 230 to 290 ms. Skipping the
client's freshness proof saves nothing. The time goes to the write workflow's post-write refresh, a
full incremental scan that discovers every file (about 500 ms at 100K with nothing changed) and then
refreshes vault-wide derived state, which a daemon would run just the same. By this record's own
rule, routed writes stop here; the refresh is fixed in the shared write path, where every
integration gains. The design below stays as the reference should a later measurement change the
answer.

## Decisions

- **Indeterminate writes** fail with error code `write_indeterminate` and exit code 3 (2026-10-18).
- **A separate opt-in.** Writes route only under `--route-writes` or `[client] route_writes`;
  `VULCAN_ROUTE` and `route` stay read-only (2026-10-18).
- **Slice 1 is a go/no-go.** If routing `note append` and `note set` saves little at 10K and 100K,
  this record is marked not pursued; direct writes are already correct.
