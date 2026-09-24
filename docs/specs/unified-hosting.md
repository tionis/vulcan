# Unified Hosting Contract and Migration Inventory

Status: accepted design record for Roadmap 10.7.1  
Scope: implemented long-running surfaces and their migration into a reusable Vulcan host  
Non-goals: new application APIs, Phase 11 auto-commit behavior, Phase 19 app semantics, or a
second MCP implementation

## Purpose

Vulcan has one resident process today, but it does not yet have one reusable hosting abstraction.
The daemon owns synchronization workers and the loopback companion API, while the CLI owns four
standalone listeners: the single-vault API, MCP HTTP, static-site preview, and frontend-bundle
preview. This record fixes the ownership and compatibility contract used to consolidate those
surfaces.

The consolidation preserves two distinct products:

- A **resident host** is enabled only by device-global configuration and started explicitly through
  `vulcan daemon start`, `vulcan daemon install`, or their existing lifecycle adapters. It may
  reconcile registered vaults and configured long-lived services.
- A **temporary foreground host** exists only for one invocation such as `vulcan serve`,
  `vulcan mcp --transport http`, `vulcan mcp remote run`, `vulcan site serve`, or
  `vulcan export profile serve`. It does not register a vault, install or start the daemon, persist
  service enablement, or activate unrelated automation.

Finite CLI commands remain daemon-independent. A host is an adapter over shared finite workflows,
not a replacement source of truth.

## Current implementation inventory

The status column distinguishes code that exists now from later roadmap work. “Migrate” identifies
the first 10.7 slice that changes ownership; it does not claim that slice is implemented.

| Surface | Current command/module | Current lifecycle and shutdown | Authority, credentials, and durable state | Intended owner | Migrate |
| --- | --- | --- | --- | --- | --- |
| Resident process and companion listener | `vulcan daemon start`; `vulcan-daemon/src/process.rs`, `http.rs` | One device process lock; Tokio loopback listener; Ctrl-C, service termination, or authenticated `/shutdown`; final-sync drain before worker cancellation and runtime-record removal | Device companion bearer credential and origin allowlist under user state; runtime record under user state; device registry/config under user config | Reusable resident `Host`; companion is a required global endpoint | 10.7.2 |
| Sync trigger coordinator | `runtime.rs`, `watch.rs` | One coordinator thread; registry reconciliation; one watcher thread per active Git-capable registration; stop signal joins all watchers | Registered wiki and permission-profile reference from device config; apply markers and journals under user state | Supervised global worker plus shared per-vault observation consumers | 10.7.2, 10.7.3 |
| Sync executor | `process.rs`, `supervisor.rs`, `sync.rs` | One worker thread claims durable jobs and cooperatively cancels finite app transactions | Durable bounded job ledger and sync repository state under user state; registration profile revalidated before execution | Required supervised global worker using per-vault mutation coordination | 10.7.2, 10.7.4 |
| Remote notification runtime | `notifications.rs` via `process.rs` | Tokio task; discovers one listener per eligible wiki; reconnects until host cancellation | Advertisement from Git discovery ref; optional named secret environment values; no vault token copy | Optional supervised global worker with vault-scoped children | 10.7.2, 10.7.3 |
| Alert delivery | `alert_delivery.rs` via `process.rs` | Bounded worker; drains or cancels during daemon shutdown | Device notification config; durable delivery ledger and named environment credentials under user state/environment | Optional supervised global worker | 10.7.2 |
| Conflict proposal worker | `conflict_worker.rs` via `process.rs` | Optional thread; polls retained conflicts; joins on stop | Device agent endpoint/model and named environment credential; proposal/claim state under user sync state; vault permission profile | Optional supervised global worker; explicit service authority | 10.7.2, 10.7.4, 10.7.6 |
| Semantic-plan worker | `semantic_worker.rs` via `process.rs` | Optional thread; polls eligible work; joins on stop | Device agent endpoint/model and named environment credential; durable plans under user sync state; vault permission profile | Optional supervised global worker; explicit service authority | 10.7.2, 10.7.4, 10.7.6 |
| Single-vault HTTP API | `vulcan serve`; `vulcan-daemon/src/vault_http.rs` with the CLI lifecycle shim in `vulcan-cli/src/serve.rs` | Reusable axum router on an invocation-owned Tokio listener, optional index-watch thread, graceful foreground shutdown | Invocation-local bind/vault/profile; vault cache and config; no device service registration | Reusable daemon router mounted by a temporary or resident host | 10.7.4, 10.7.5 |
| MCP stdio | `vulcan mcp`; `vulcan-cli/src/mcp.rs` | Client-owned stdin/stdout loop; ends on EOF/process exit | Invocation-local permission profile and packs; session state in memory; vault config/cache | Permanent transport-specific exception in CLI using the shared dispatcher; never resident-owned | 10.7.6 |
| Direct MCP HTTP | `vulcan mcp --transport http`; `vulcan-cli/src/mcp.rs` | Blocking listener and connection threads; invocation-owned watcher; ends on signal/process exit | Invocation flags, optional static token or OAuth settings; OAuth/session state in memory; selected durable local-issuer material under user state | Reusable MCP router mounted by a temporary host | 10.7.4, 10.7.6 |
| Named remote MCP HTTP | `vulcan mcp remote run`; `mcp.rs`, `commands/mcp_remote.rs` | Same foreground HTTP adapter plus a per-instance ownership lock; distinct instances may run concurrently | Named definition in device registry; per-instance OAuth clients/secrets and connection grants under user state; exact audience and vault/profile/pack ceilings | Same MCP router mounted by temporary or resident host from the same named definition | 10.7.4, 10.7.6, Roadmap 10.10 |
| Static-site preview | `vulcan site serve`; `vulcan-cli/src/site_server.rs` | Blocking loopback listener, connection threads, optional rebuild watcher; invocation shutdown joins both | Invocation-local profile/output/bind; generated output and vault cache/config | Reusable preview service under a temporary or resident host | 10.7.3, 10.7.7 |
| Frontend-bundle preview | `vulcan export profile serve`; `vulcan-cli/src/bundle_server.rs` | Blocking loopback listener, connection threads, optional rebuild watcher; invocation shutdown joins both | Invocation-local bundle/output/bind; generated output and vault cache/config | Reusable preview service under a temporary or resident host | 10.7.3, 10.7.7 |
| Per-command auto-commit | mutation orchestration in `vulcan-app` and CLI adapters | Finite post-mutation action; no resident periodic loop exists | Vault Git repository and opt-in vault config; suppressed by `--no-commit` | Remains finite now; a Phase 11 supervised consumer may call the same workflow | 10.7.4, 10.7.6, Phase 11 |
| Vulcan App HTTP/runtime endpoints | Phase 19 | Not implemented | Not applicable | Host services only after each Phase 19 contract exists | 10.7.7, Phase 19 |

Child processes are limited to the already bounded Git engine, configured agent/network clients,
platform notification helpers, service-manager adapters, and preview/browser conveniences. Hosting
does not make an arbitrary child process a service. Each existing child retains its no-shell,
timeout, permission, and credential-redaction contract.

## Host and service model

### Stable service identity

A service definition has these transport-neutral fields:

- `id`: a stable, lower-case identifier with a kind prefix. Built-ins use
  `endpoint.companion`, `worker.sync-trigger`, `worker.sync-executor`,
  `worker.remote-notifications`, `worker.alert-delivery`, `worker.conflict`,
  `worker.semantic`, `observation.vault/<registration-id>`, `endpoint.mcp/<instance-id>`,
  `endpoint.rest/<listener-id>`, `preview.site/<session-id>`, or
  `preview.bundle/<session-id>`.
- `scope`: `global`, `vault`, or `instance`, with the stable registration/instance identity stored
  separately from a display name or path.
- `required`: whether failure prevents host readiness. The process lock, configured required
  listeners, sync executor, and trigger coordinator are required for the resident configuration
  that enables them. Disabled and best-effort delivery/agent services are not.
- `dependencies`: an acyclic set of service IDs. A service can start only after every required
  dependency is ready. Shutdown uses reverse dependency order.
- `restart`: `never`, `on_failure`, or `bounded_on_failure`. Built-in workers default to bounded
  restart only after their finite work has either completed or retained recovery evidence. An
  endpoint bind failure is a startup/configuration error, not an in-place retry loop.

Service IDs identify lifecycle projections, not authorization principals. Authority is always an
explicit execution-context field.

### Lifecycle states and health

Every configured service is visible in a bounded, secret-free status projection, including
disabled and degraded services:

```text
disabled -> starting -> ready -> stopping -> stopped
                    \-> degraded -> restarting -> ready
                    \-> failed
```

The report contains service ID, kind, scope, lifecycle state, readiness, start/restart counts,
last transition time, and a sanitized last failure category/detail. It excludes bearer values,
OAuth material, environment values, private request bodies, filesystem event payloads, and model
prompts. Existing per-wiki sync status stays a separate domain projection.

Host readiness is published only after all required configured services are ready. A required
service startup failure unwinds already-started services. An optional failure leaves the host
running in a visible degraded state. Unexpected exit or panic is observed by the supervisor;
bounded restart uses capped backoff and exposes exhaustion. Restart never replays an uncommitted
mutation merely because its worker exited.

### Shutdown order

The resident host shuts down in this order:

1. Stop ordinary ingress and producers of new external work.
2. Quiesce mutation-producing workers and reject new queued mutations.
3. Enqueue and drain the existing internal final-sync work within its current deadline.
4. Cooperatively cancel remaining finite work; a response timeout or dropped async task does not
   assert that a synchronous write stopped.
5. Stop vault consumers, observation owners, endpoints, and support workers in reverse dependency
   order, joining every owned task/thread.
6. Remove the runtime record only if it is still owned by this process, then release locks.

Temporary hosts omit final sync and unrelated resident workers. They stop only services created by
that invocation.

## Configuration and reconciliation

Resident service enablement, listener binds, public origins, instance identities, credentials,
execution trust ceilings, and named remote definitions are device-global configuration. Vault
configuration owns portable application settings, permission profiles, schedules, and enabled
state that are meaningful as vault content. Secrets and non-rebuildable runtime state remain in the
user state/secret store, never in synced vault configuration or `cache.db`.

Settings are classified before implementation:

- Listener bind/origin, credential-store location, process-wide limits, and service graph changes
  are **restart required** for the initial host.
- Registry membership, registration path metadata, pause state, and vault-owned schedules are
  **live reconciled**. Reconciliation validates a complete candidate before replacing the active
  definition.
- A live update that changes execution authority is revalidated at dispatch. Invalid execution
  configuration disables/degrades the affected service and blocks new dispatch; it does not keep
  stale broader authority active.

Temporary configuration is invocation-local and never written unless the command is an explicit
configuration mutation such as `mcp remote init/set`.

## Per-vault identity and ownership

Canonicalized materialized path plus durable registration identity determines a resident
per-vault runtime. Aliases may resolve to that runtime but cannot create another automatic owner.
Plain Markdown and non-Git registrations may have indexing/preview consumers without acquiring a
sync worker.

Within one host, a per-vault scheduler serializes conflicting mutations and permits bounded reads
and work on different vaults. It supplements, and never replaces, the same cross-process vault,
repository, and operation locks used by standalone commands. The lock order is:

1. resolve canonical vault and repository identity without a mutation lock;
2. acquire the vault application-write lock;
3. acquire the repository/sync transaction lock when Git state is involved;
4. acquire short-lived cache/database write transactions only while needed;
5. release in reverse order.

Network calls, agent generation, and other preparatory work stay outside mutation locks where
possible. The apply step revalidates permissions, configuration revision, source hashes/frontiers,
and operation identity after acquiring locks.

A temporary host that overlaps a resident runtime must do one of two explicit things:

- attach through a versioned resident API and reuse the resident per-vault runtime; or
- run standalone and acquire the applicable cross-process ownership/mutation locks.

Failure to acquire ownership is actionable and must name the conflicting resident or foreground
instance. No temporary invocation silently starts, stops, reconfigures, or bypasses the daemon.

## Execution context

Hosted adapters pass a transport-neutral context into shared dispatch/workflow code:

- canonical vault and optional repository identity;
- caller identity plus effective `PermissionGrant`/filter ceiling;
- service/instance and request/operation IDs;
- audience/session binding where applicable;
- cancellation token and absolute deadline;
- explicit background service authority for automation.

Background work never borrows a browser, MCP, or companion session credential. Queued work
revalidates current authority immediately before execution. Each operation class declares whether
it is read-only, idempotently retryable, recoverable from a durable ledger/journal, or potentially
indeterminate after a timeout. An in-memory queue does not claim exactly-once delivery.

The implemented contract lives in `vulcan_app::execution`. `ExecutionVaultIdentity::resolve`
canonicalizes aliases before scheduling, and an optional `ExecutionRepositoryIdentity` carries the
repository layer's stable key and canonical Git directory. `ExecutionAuthority` is deliberately a
closed choice between a caller principal and a configured background service authority; both own a
permission ceiling, and construction rejects any broader effective grant. `ExecutionIdentity`
carries independently generated request and operation ULIDs plus the hosting service-instance ID.
The cancellation token is a synchronous atomic flag and the deadline is an absolute Unix-epoch
millisecond value, so ordinary app entrypoints can call `checkpoint()` without depending on Tokio.
Adapters remain responsible for checking at workflow boundaries and again immediately before an
apply step. `ExecutionRetryClass` records whether a disconnected or timed-out operation is a read,
idempotent, durably recoverable, or potentially indeterminate after dispatch.

The concrete lock inventory, known migration gaps, canonical identities, ordering, and contention
semantics are maintained in [mutation coordination and lock audit](mutation-coordination.md).

## Client routing contract

Routing is explicit and happens before dispatch:

- **Direct** remains the default for ordinary finite CLI commands and all stdio MCP use. It neither
  probes nor starts the daemon.
- **Explicit daemon** is selected by a daemon-specific command/flag or a client already configured
  for the companion/resident protocol. Version and capability negotiation occurs before dispatch.
- **Automatic** routing is not enabled by this record. A later implementation may fall back to
  direct execution only after a known pre-dispatch failure. If a write may have reached the daemon,
  the client must query operation status or return an indeterminate result; it must not retry the
  write directly.

MCP stdio remains client-owned. Foreground and resident MCP HTTP must use one shared dispatcher,
router, OAuth implementation, and named-remote definition. A resident instance cannot reinterpret
its audience, grants, tool packs, vault ceiling, session keys, or revocation state.
The initial extraction places transport-neutral request types and protocol constants in
`vulcan-app::mcp_protocol`, tool input/output JSON Schemas in `vulcan-app::mcp_schemas`, and
built-in tool metadata, pack selection, and permission visibility in `vulcan-app::mcp_catalog`.
`vulcan-app::mcp_dispatch` routes JSON-RPC requests through a method-handler boundary with the
existing stdio and HTTP notification, error, and timeout shapes. CLI stdio and foreground HTTP
consume these shared contracts. `vulcan-app::mcp_assistant` provides permission-filtered prompt
and skill discovery, prompt rendering, resource discovery/templates, vault-owned assistant reads, and pack-filtered custom-tool resource reads to both transports.
`vulcan-app::mcp_help` owns built-in help topics, their report types, and help-resource response
shaping, with command-specific help supplied by the host.
`vulcan-app::mcp_completion` owns permission-filtered completion and response shaping with the
host's help-topic catalog injected. `vulcan-app::mcp_read_tools` owns search and query workflows,
including read-filtered results and bounded query projection, plus the note-source read boundary,
guarded daily-content access, bounded daily-list response shaping, and permission-filtered task-query reports. MCP task list/query and create/complete/reschedule invoke app task workflows directly, preserving their write preflight and cache refresh. `vulcan-app::periodic`
owns date/target resolution and daily list/show reports shared by CLI and MCP. MCP status uses the existing
`vulcan-app::browse` report directly.
`vulcan-app::notes` resolves vault and direct Markdown targets and owns `note_outline`,
`note_get`, and `note_info` reports for CLI and MCP. Core graph queries provide
permission-filtered note-link confidence, so MCP note-info counts exclude unreadable backlink
sources.
`vulcan-app::web` provides permission-checked search/fetch workflows to CLI and MCP, including
network and optional save-path preflight; MCP no longer calls CLI web handlers.
Remaining tool workflows, CLI-derived command-help catalog sharing, and HTTP/OAuth hosting
remain migration work under 10.7.6.

## Preserved compatibility contracts

Migration must retain these shipped contracts until an explicit version boundary is documented:

- Companion protocol v1 routes, authentication, Origin checks, WebSocket subprotocols, status/job
  JSON, idempotency rules, and graceful shutdown behavior in `docs/companion_protocol.md`.
- `vulcan serve` paths, request/response JSON, bind defaults/options, permission filtering, and
  daemon-independent foreground behavior.
- MCP protocol version, tools/resources/prompts/completions schemas, JSON-RPC errors, pagination,
  large-result resources, notifications/cancellation, stdio cleanliness, Streamable HTTP session
  rules, static-token and OAuth combinations, and named-remote consent/grant behavior.
- Site and bundle preview routes, live reload/rebuild semantics, output layout, and foreground
  convenience commands.
- Existing CLI human/JSON reports, dry-run guarantees, direct sync semantics, final-sync policy,
  and durable state locations.

Capability documents and schemas are generated from actually mounted routes. Consolidation does
not advertise unimplemented Phase 10 REST, Phase 11 automation, or Phase 19 app capabilities.

The migrated single-vault router preserves `/`, `/health`, `/search`, `/notes`, `/graph/stats`,
`/related`, and the four `/dataview/*` paths and delegates their JSON reports to
`vulcan_app::serve`. `vulcan serve` starts that listener and its optional watcher as invocation-scoped
services in an ephemeral `HostSupervisor`, with dependency-ordered readiness and reverse shutdown.
It still owns only its temporary listener, generated or supplied token, selected permission profile,
and optional watcher; it does not read or mutate the resident registry, persisted host status, or
service installation. Host and Origin validation, constant-time token comparison,
declared body limits, and request deadlines now live in the reusable daemon adapter. Feature flags
for vectors and the JS runtime are forwarded through the daemon crate so moving the transport does
not silently remove endpoints from the default CLI build.

Shared HTTP policy primitives now enforce declared body limits, bounded async response waits,
exact Origin parsing, constant-time secret-header checks, bearer extraction, CORS response headers,
and secret-minimal access records that omit query strings and every request header. The companion
and single-vault adapters use those primitives but retain different authenticators, error schemas,
allowed origins, body ceilings, and audiences. Companion transport remains loopback-only; its
bearer cannot authorize a vault request, and a vault token cannot authorize companion routes.
Browser preflight for the single-vault API validates Host and Origin without treating preflight as
an authenticated operation; the subsequent request still requires its vault token.

The single-vault adapter installs routes from the transport-neutral `vulcan_app::serve` catalog.
The root document retains its original ordered `endpoints` array and adds a `routes` capability
array generated from that same catalog. Each entry names the installed path and method, publishes
its JSON query/response schemas, and reports feature-dependent availability. Thus a build without
vectors or the JavaScript runtime still reports `/related` or `/dataview/query-js` explicitly as an
installed but unavailable compatibility route instead of silently advertising working behavior.

The listener registration accepts either invocation-instance or registered-vault ownership while
using the identical router. Acceptance tests run both ownership modes without a daemon process and
compare complete HTTP responses. The temporary watcher now reports service readiness only after
its startup observation and scan succeed; a pre-readiness watcher failure rolls back the dependent
listener and releases its port. Coverage also exercises permission filtering, independent token,
Host, and Origin rejection, malformed connections, declared oversized bodies, bind conflicts,
watch-backed refresh, and clean shutdown.

### Compatibility test inventory

Migration extends these existing suites instead of replacing them with host-only tests:

| Surface | Existing evidence | Required migration comparison |
| --- | --- | --- |
| Daemon lifecycle and companion | `vulcan-daemon/src/process.rs`, `runtime.rs`, `watch.rs`, `supervisor.rs`, and `http.rs` unit/integration tests; `daemon_cli_detaches_reports_status_and_stops_gracefully` | Partial-start rollback, required/optional service failure, restart exhaustion, final-sync ordering, runtime/port cleanup, and unchanged companion v1 responses |
| Single-vault HTTP | `vulcan-cli/src/serve.rs` tests for repeated queries, watch refresh, token auth, and permission filtering | Byte/JSON-equivalent temporary versus resident routes, limits, malformed requests, bind conflicts, and daemon-stopped foreground operation |
| MCP | `vulcan-cli/src/mcp/tests.rs` plus `mcp_server_*` and `mcp_http_transport_*` CLI integration tests | One dispatcher conformance across stdio, foreground HTTP, and resident HTTP; two isolated authorities/sessions; OAuth combinations; disconnect/cancellation cleanup |
| Named MCP remotes | registry/state tests and `named_mcp_remote_cli_lifecycle_is_device_global_and_dry_run_safe` | Same instance definition and consent behavior in foreground/resident modes; multiple instances; ownership conflict; revocation without unrelated restart |
| Site preview | `vulcan-cli/src/site_server.rs` serve/watch/live-reload/prefix tests and `site_serve_json_reports_a_ready_bound_url` | Foreground/resident route and rebuild parity, irrelevant-event filtering, cleanup, and last-good-output behavior |
| Bundle preview | `vulcan-cli/src/bundle_server.rs` contract and watch/rebuild tests | Foreground/resident route and live-reload parity, cleanup, and independent concurrent previews |
| Sync/conflict/semantic workers | daemon runtime, supervisor, sync, conflict-worker, semantic-worker, and CLI smoke tests | Same durable jobs/results under the reusable supervisor, visible degradation, cancellation races, and no duplicate automatic owner |

Every migration slice also retains the workspace boundary guard, full CLI snapshots where output
changes, and feature-disabled checks for affected adapters.

## Migration sequence and removal rule

1. Introduce the reusable host/supervisor and adapt current daemon workers without changing domain
   behavior (10.7.2).
2. Add transport-neutral execution and mutation coordination, then move the single-vault HTTP
   router behind the host while preserving `vulcan serve` (10.7.4–10.7.5).
3. Reconcile one shared per-vault observation owner with independent bounded consumers (10.7.3).
4. Extract the MCP dispatcher/protocol and mount its HTTP router in temporary and resident hosts;
   keep stdio as a client-owned adapter (10.7.6 and 10.10).
5. Move preview listeners/rebuild consumers and only implemented app surfaces (10.7.7).

An old listener, watcher, or worker loop is removed only after parity tests cover temporary and
resident behavior, startup rollback, authorization, contention, cancellation, cleanup, and the
feature-disabled builds it affects. Until then there is one selected owner per invocation—never two
automatic owners for the same service or vault.
