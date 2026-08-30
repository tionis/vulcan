# Vulcan companion protocol v1

The companion protocol is a loopback-only projection of Vulcan's typed application and sync
reports. Clients must never implement a second Git state machine or ask the daemon to execute an
arbitrary Git command.

## Connection and authentication

- The server accepts only listeners whose bound address is IPv4 or IPv6 loopback.
- HTTP requests use `Authorization: Bearer <device-token>`.
- Every HTTP operation except `GET /capabilities` uses `Vulcan-Protocol-Version: 1`.
- `POST /{id}/sync`, `POST /sync`, and `POST /{id}/sync/resume` additionally require a bounded
  `Idempotency-Key`. Keys are scoped to the non-secret device credential ID and survive daemon
  restart while the retained job exists.
- If an `Origin` header is present, it must exactly match an origin in the device credential.
  Supported origins are bounded `app://`, `capacitor://`, or HTTP loopback origins. CORS preflight
  uses the same Origin policy but does not carry the bearer secret.
- Error bodies are versioned JSON objects with `version`, `kind`, and `detail`. Stable kinds are
  `invalid_request`, `not_found`, `permission_denied`, `conflict`, and `internal`.

The response header `Vulcan-Protocol-Version: 1` is present on transport-generated responses.
Clients should call `GET /capabilities` before relying on optional operations.

## HTTP endpoints

| Method | Path | Result |
|---|---|---|
| `GET` | `/capabilities` | Protocol, sync-contract, operation, backend, transport, and agent-mode capabilities |
| `GET` | `/vaults?group=<group>` | Registered wiki reports, optionally filtered by group |
| `GET` | `/{id}/sync/status` | Reconstructed per-wiki sync status |
| `POST` | `/{id}/sync` | HTTP 202 with an idempotent retained manual-sync job |
| `POST` | `/sync` | HTTP 202 with an idempotent aggregate job for exactly one `wiki`, `group`, or `all` selection |
| `POST` | `/{id}/sync/pause` | Updated paused registration |
| `POST` | `/{id}/sync/resume` | HTTP 202 with an idempotent retained resume job |
| `GET` | `/{id}/sync/conflicts` | Unresolved preserved conflicts |
| `GET` | `/{id}/sync/conflicts/{conflict}` | Preserved conflict record and resolution state |
| `POST` | `/{id}/sync/conflicts/{conflict}/resolve` | Deterministic reviewed resolution report |
| `POST` | `/{id}/sync/conflicts/{conflict}/proposals` | Provider-backed resolution proposal when advertised |
| `POST` | `/{id}/sync/conflicts/{conflict}/proposals/approve` | Explicit stale-checked proposal approval |
| `POST` | `/{id}/sync/conflicts/{conflict}/proposals/reject` | Explicit proposal rejection |
| `POST` | `/{id}/sync/semantic-plans` | Deterministic or advertised provider-backed semantic-history plan report |
| `GET` | `/jobs/{job}` | Retained job status |
| `DELETE` | `/jobs/{job}` | Queued cancellation or cooperative running cancellation report |
| `GET` | `/aggregate-jobs/{job}` | Aggregate selection status, independent child reports, and outcome counts |
| `DELETE` | `/aggregate-jobs/{job}` | Cancel the parent and each child not shared by another active aggregate request |
| `GET` | `/events` | WebSocket upgrade |
| `POST` | `/shutdown` | Gracefully stop a process-owned daemon when advertised |

Conflict proposal creation and explicit approval/rejection are advertised only when the daemon has
a server-configured resolution provider. Provider endpoints or credentials never come from a
companion request. When proposal creation is available, capabilities report
`agent_conflict_proposal_limit_per_conflict: 1` and
`agent_conflict_proposal_claim_scope: daemon_process`. The daemon claims the repository/conflict
pair before invoking the provider and returns `conflict` immediately for a concurrent request.
The scoped claim is released after success or failure. It prevents duplicate token spend inside
one daemon, but it is not a cross-device coordination protocol.

Semantic agent planning is likewise available only when `agent_semantic_plans` is true. The
daemon owns that provider's endpoint, model, and credential; companion JSON only opts into the
configured provider with `agent: true`. The registered wiki permission profile gates the reported
network endpoint before any patches are sent. Deterministic planning remains available when no
semantic provider is configured, while an explicit agent request then returns `not_found`.

Aggregate selection JSON contains exactly one of `wiki` (a registered wiki ID), `group` (a
registered group name), or `all: true`. The retained parent identifies every normalized child job;
its counts and state are derived from those children. A failed or conflicted child does not roll
back a successful child.

Conflict resolution JSON contains `side` (`base`, `local`, or `remote`) and may contain `remote`,
`live_ref`, and `dry_run`. Semantic-plan JSON contains `from`, `to`, and `semantic_ref`, and may
contain `remote`, `live_ref`, `grouping`, `agent`, and `dry_run`. The defaults are remote `origin`,
live ref `refs/heads/__vulcan-sync/live`, and deterministic `top_level` grouping. Capability
negotiation is authoritative for both optional agent modes.

## WebSocket events

Browser WebSocket constructors cannot set `Authorization` or arbitrary version headers. A client
therefore offers two subprotocols:

```text
vulcan.v1, vulcan.bearer.<device-token>
```

The server validates both and selects only `vulcan.v1`; the token is never put in the URL or echoed
as the selected protocol. The same exact Origin allowlist applies to the upgrade.

The stream sends an initial `state_snapshot` JSON object and then sends another only when its
serialized state changes. A snapshot contains `version`, registered `vaults`, reconstructed
per-wiki `statuses`, retained child `jobs`, and retained `aggregates`. This polling projection deliberately reuses durable state
and avoids making an in-memory event bus authoritative.

## Execution boundary

Axum handlers move synchronous registry, supervisor, state-store, and application work through
`spawn_blocking`. Repository operations continue to acquire the same locks and enforce the same
registered permission profiles as direct CLI workflows. Request bodies and WebSocket messages are
bounded, and the server never exposes arbitrary Git execution.

`daemon_shutdown` appears in capabilities only for a process-owned service. `POST /shutdown`
requires the same bearer, version, and Origin checks as other mutating companion requests and sets
the cooperative process stop flag; embedded routers may omit the operation entirely.
