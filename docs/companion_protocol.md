# Vulcan companion protocol v1

The companion protocol is a loopback-only projection of Vulcan's typed application and sync
reports. Clients must never implement a second Git state machine or ask the daemon to execute an
arbitrary Git command.

## Connection and authentication

Start the daemon, then run `vulcan daemon companion --output json` to inspect its loopback endpoint
and non-secret credential identity. For an explicit client setup, run
`vulcan daemon companion --reveal-token --output json` and transfer the returned token directly
into device-local client storage. The reveal command refuses to provision a stopped or
credential-mismatched daemon. Never put its output in a vault note, `.obsidian/plugins/*/data.json`,
shared configuration, shell history, logs, or source control; the token is bearer authority for
the local companion API.

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
| `GET` | `/capabilities` | Protocol, sync-contract, operation, backend, transport, conflict-page, and REST capabilities |
| `GET` | `/vaults?group=<group>` | Registered wiki reports, optionally filtered by group |
| `GET` | `/{id}/sync/status` | Reconstructed per-wiki sync status |
| `POST` | `/{id}/sync` | HTTP 202 with an idempotent retained manual-sync job |
| `POST` | `/sync` | HTTP 202 with an idempotent aggregate job for exactly one `wiki`, `group`, or `all` selection |
| `POST` | `/{id}/sync/pause` | Updated paused registration |
| `POST` | `/{id}/sync/resume` | HTTP 202 with an idempotent retained resume job |
| `POST` | `/{id}/sync/activity` | Activity signal: queue a sync unless one is pending or fresh |
| `GET` | `/{id}/sync/conflicts` | Unresolved legacy conflict records (read-only) |
| `GET` | `/{id}/sync/conflicts/{conflict}` | Legacy conflict record detail (read-only) |
| `GET` | `/jobs/{job}` | Retained job status |
| `DELETE` | `/jobs/{job}` | Queued cancellation or cooperative running cancellation report |
| `GET` | `/aggregate-jobs/{job}` | Aggregate selection status, independent child reports, and outcome counts |
| `DELETE` | `/aggregate-jobs/{job}` | Cancel the parent and each child not shared by another active aggregate request |
| `GET` | `/events` | WebSocket upgrade |
| `POST` | `/shutdown` | Gracefully stop a process-owned daemon when advertised |

Conflict records were written only by the removed whole-worktree sync mode. They stay readable but
cannot be resolved; the former `resolve`, `proposals`, and `semantic-plans` endpoints and their
agent capabilities were removed.

Aggregate selection JSON contains exactly one of `wiki` (a registered wiki ID), `group` (a
registered group name), or `all: true`. The retained parent identifies every normalized child job;
its counts and state are derived from those children. A failed or conflicted child does not roll
back a successful child.

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
requires the same bearer, version, and Origin checks as other mutating companion requests. The
daemon first stops accepting new companion requests, then runs its bounded final-sync policy,
stops and joins hosted services in reverse dependency order, and finally removes its owned runtime
record. Embedded routers may omit the operation entirely.

## Reference Obsidian companion

`integrations/obsidian-vulcan` contains the reference desktop/mobile client. It requires Obsidian
1.11.4 so bearer authority can live in native `SecretStorage`; ordinary plugin `data.json` retains
only allowlisted non-secret connection and trigger preferences. The release bundle contains no Git
implementation. It requests the editor save command before manual sync, debounces completed vault
writes, and displays authenticated status/event snapshots. Its mock-daemon suite verifies
authorization, protocol versioning, idempotency, path encoding, typed errors, status, and sync.
