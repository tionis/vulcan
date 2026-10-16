# Daemon REST API

Status: accepted design for Roadmap 10.2 (tokens) and 10.3 (REST API), plus the 11.2 history
endpoints that build on it.
Scope: per-vault API tokens, request principals, and the HTTP routes the daemon serves for
knowledge reads, history, and writes.
Non-goals: Phase 17 identities and delegable grants, non-loopback listeners and TLS, and a second
implementation of any CLI workflow.

## One listener, one namespace

The REST API extends the companion protocol on the daemon's existing loopback listener. The
companion protocol already defines `GET /capabilities`, `GET /vaults`, `/{id}/sync/...`, the routed
`/{id}/ops/...` reads, jobs, and events. The REST routes live in the same `/{id}/...` namespace
beside them, and they reuse the same transport policy:

- the listener refuses non-loopback addresses;
- every request except `GET /capabilities` and the event upgrade carries
  `Vulcan-Protocol-Version: 1`;
- an `Origin`, when present, must be one the companion credential allows;
- request bodies are limited to 1 MiB, and requests have a 30-second deadline.

Responses use the CLI's `--output json` shapes. Errors use the companion error object
(`{"version": 1, "kind": ..., "detail": ...}`) with an HTTP status: 400 invalid request, 401 missing or invalid
credential, 403 denied by the principal or its permission profile, 404 unknown vault or path, 409
conflict, 503 the vault is not served right now (for example, its index consumer is not running).

## Principals

A bearer credential resolves to exactly one principal:

| Credential | Principal | Reach | Permission profile |
| --- | --- | --- | --- |
| Companion credential (`vulcan daemon companion`) | Device | Every route and every registered vault | The registration's `permissions_profile` (default `unrestricted`) |
| API token (`vulcan vault token create`) | Vault | The REST routes of its own vault, `GET /capabilities`, `GET /health`, and `GET /vaults` (listing only its vault) | The registration's `permissions_profile` |

An API token never reaches sync administration (pause, resume, conflict resolution, semantic
plans), jobs, events, routed `ops`, or shutdown. Those stay device-only. A vault principal that
names another vault's ID gets 404, so a token cannot probe other registrations.

Authorization and result filtering reuse `PermissionGuard` and `PermissionFilter`. Reads apply the
profile's read filter, so a restricted caller only sees permitted notes. Writes, Git, and
configuration checks fail with 403 and the guard's message. The daemon adds no policy of its own.

## API tokens

- **Format.** `vat_<token-id>_<secret>`. The token ID is a ULID and is not secret; it names the
  token in listings and revocation. The secret is 32 bytes from the OS generator, base64url
  without padding.
- **Storage.** Tokens live outside vault content, in `daemon/api-tokens.json` under the
  device-local state directory, written atomically with the same file checks as the companion
  credential (no symlinks, bounded size, not group- or world-readable on Unix). Each record holds
  the token ID, the registration ID it belongs to, an optional label, its creation time, and the
  BLAKE3 hash of the secret. The secret itself is shown once, at creation.
- **Why BLAKE3 rather than argon2.** The roadmap sketch named argon2, which is a password hash:
  it slows guessing of low-entropy secrets. These secrets are 256 random bits, so guessing is
  already infeasible and a slow hash would only add per-request cost and a new dependency.
  Verification compares the hash in constant time. Phase 17 can replace the scheme without changing
  the token format.
- **Binding.** A token binds to the registration ID, not the wiki ID, so removing a registration
  and adding another with the same ID does not revive old tokens. Removing a registration makes
  its tokens fail; `vault token list` reports them as orphaned and `vault token revoke` deletes
  them.
- **Commands.** `vulcan vault token create <wiki> [--label <text>] [--dry-run]`,
  `vulcan vault token list [<wiki>]`, and `vulcan vault token revoke <token-id> [--dry-run]`, all
  with JSON output. Listings never include secrets or hashes.

## Routes

`{id}` is a registered wiki ID. Paths after `notes/`, `links/`, `backlinks/`, and `history/` are
vault-relative and may contain `/`.

### Daemon

- `GET /capabilities`: protocol version and feature flags, extended with `rest` route groups.
- `GET /health`: daemon status and, per visible vault, whether it is served and its cache state.
- `GET /vaults`: registered vaults visible to the principal.
- `GET /openapi.json`: an OpenAPI 3.1 document for the REST routes, with an HTTP bearer security
  scheme and a note that each request is constrained by the principal's permission profile.

### Reads

Reads run on a blocking thread after an index barrier, so changes made before the request are
indexed, under the registration's profile. Their responses are the CLI's JSON reports.

- **Mounted `vulcan serve` routes.** The single-vault serve router answers these per vault, with its
  query parameters and the retained note-store session: `GET /{id}/search`, `/{id}/notes` (property
  query), `/{id}/query` (query DSL or `QueryAst` JSON), `/{id}/related`, `/{id}/bases/eval`,
  `/{id}/dataview/{inline|query|query-js|eval}`, and `/{id}/mdbase/{query|read|types|contracts|schema|views|view}`.
  The serve envelope's `result` is returned as the body; its failures become companion errors with
  the same status.
- `GET /{id}/notes/{path}`: one note, as `vulcan note get` reads it, with its section, heading,
  block, line, and match selectors.
- `GET /{id}/info/{path}`: a note's metadata, as `vulcan note info` reports it.
- `GET /{id}/links/{path}` and `GET /{id}/backlinks/{path}`: outgoing and incoming links, filtered
  by the profile.
- `GET /{id}/graph/stats`, `/{id}/graph/hubs`, and `/{id}/graph/path?from=...&to=...`: from the
  daemon's retained graph, the same one routed CLI graph reports use.

A registered vault the daemon does not index (files-only, or while the daemon starts) answers 503.

### History (11.2)

History reads Git, not the cache, so it works for any registered vault with a repository. It needs
the profile's Git permission and applies the read filter to the path it names. Whole-vault history
and uncommitted whole-vault diffs need a profile that reads every note, because they name every
changed file. Commit messages are returned as written and may mention other files; a restricted
profile that must not learn other paths should not be given Git access. Revisions are hexadecimal
commit IDs only (4 to 64 digits), so no ref expression or option-like text reaches Git.

- `GET /{id}/history?limit=...`: recent commits (`vulcan git log`).
- `GET /{id}/history/{path}?limit=...`: commits that touched one file.
- `GET /{id}/history/{path}?at=<sha>`: the file's content at that commit.
- `GET /{id}/diff?path=...`: uncommitted changes (`vulcan git diff`).
- `GET /{id}/diff/{path}?from=<sha>&to=<sha>`: one file's diff between two commits.

### Writes

Writes enter the host's mutation gate for the vault and repository, exactly as hosted MCP writes
do, and participate in auto-commit like the CLI commands they mirror. Every write takes an
`Idempotency-Key` header scoped to the principal. Replaying a key returns the original result, and a
key reused with a different request is rejected with 409. After a timeout the outcome is reported as
unknown, never retried by the daemon.

- `POST /{id}/notes`: create a note (`{"path": ..., "content": ...}`).
- `PATCH /{id}/notes/{path}`: replace content or set properties.
- `DELETE /{id}/notes/{path}`: delete a note.
- `POST /{id}/move`: move or rename with link rewriting (`{"source": ..., "destination": ...}`).
- `POST /{id}/inbox`: quick capture (`{"text": ...}`).
- `POST /{id}/scan`: trigger an incremental scan.

## Delivery

1. Tokens and principals: the token store, `vault token` commands, and middleware that resolves
   either credential to a principal and enforces its reach.
2. Daemon and read routes, plus `openapi.json`.
3. History routes.
4. Write routes with idempotency.

Each slice ships with tests and its roadmap update.
