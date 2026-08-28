# Vulcan companion protocol v1

The companion protocol is a loopback-only projection of Vulcan's typed application and sync
reports. Clients must never implement a second Git state machine or ask the daemon to execute an
arbitrary Git command.

## Connection and authentication

- The server accepts only listeners whose bound address is IPv4 or IPv6 loopback.
- HTTP requests use `Authorization: Bearer <device-token>`.
- Every HTTP operation except `GET /capabilities` uses `Vulcan-Protocol-Version: 1`.
- `POST /{id}/sync` and `POST /{id}/sync/resume` additionally require a bounded
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
| `POST` | `/{id}/sync/pause` | Updated paused registration |
| `POST` | `/{id}/sync/resume` | HTTP 202 with an idempotent retained resume job |
| `GET` | `/{id}/sync/conflicts` | Unresolved preserved conflicts |
| `GET` | `/{id}/sync/conflicts/{conflict}` | Preserved conflict record and resolution state |
| `POST` | `/{id}/sync/conflicts/{conflict}/resolve` | Deterministic reviewed resolution report |
| `POST` | `/{id}/sync/semantic-plans` | Deterministic semantic-history plan report |
| `GET` | `/jobs/{job}` | Retained job status |
| `DELETE` | `/jobs/{job}` | Queued cancellation or cooperative running cancellation report |
| `GET` | `/events` | WebSocket upgrade |

Conflict proposal creation is intentionally absent from v1 capabilities and routing until the
agent proposal workflow is implemented. The future route is
`POST /{id}/sync/conflicts/{conflict}/proposals`.

Conflict resolution JSON contains `side` (`base`, `local`, or `remote`) and may contain `remote`,
`live_ref`, and `dry_run`. Semantic-plan JSON contains `from`, `to`, and `semantic_ref`, and may
contain `remote`, `live_ref`, `agent`, and `dry_run`. The defaults are remote `origin` and live ref
`refs/heads/__vulcan-sync/live`. Capability negotiation reports both agent modes as unavailable
until their complete review and validation pipelines exist.

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
per-wiki `statuses`, and retained `jobs`. This polling projection deliberately reuses durable state
and avoids making an in-memory event bus authoritative.

## Execution boundary

Axum handlers move synchronous registry, supervisor, state-store, and application work through
`spawn_blocking`. Repository operations continue to acquire the same locks and enforce the same
registered permission profiles as direct CLI workflows. Request bodies and WebSocket messages are
bounded, and the server never exposes arbitrary Git execution.
