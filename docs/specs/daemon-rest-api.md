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

The registration's `permissions_profile` is the device's limit for the vault, so it resolves from
built-in profiles and the vault's `config.local.toml` only (`resolve_device_permission_profile`).
A name only the synced `config.toml` defines is refused, as is a synced definition wider than the
device's or with a different policy hook; workflows that later resolve the same name with the full
configuration therefore never exceed the device's definition. The same rule applies to named MCP
remote ceilings and scheduled-job ceilings.

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

- `GET /capabilities`: protocol version and feature flags, extended with `rest`: the daemon-level
  routes (`daemon_routes`), the first path segment of every per-vault route group
  (`vault_route_groups`, such as `doctor` or `graph`), and the `openapi` document's path. A client
  checks a group there before calling it. REST responses are the CLI's JSON reports; reports that
  carry their own `version` field keep it, and the protocol version covers the rest.
- `GET /health`: per visible vault, whether it is served, its cache freshness and last scan error,
  and each observer and consumer (`observer`, `index`, `sync`, `auto-commit`) with whether it runs,
  its restart count, and its last exit error. `healthy` is true when every wanted component runs and
  no cache error is recorded. `GET /{id}/health` reports one vault. The trigger runtime records
  exits when it reaps them and publishes which components run after each reconcile pass.
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
- `GET /{id}/graph/dead-ends` and `/{id}/graph/components`: from the cache, filtered by the profile.
- `GET /{id}/vectors/models`: the embedding models in the vault's vector index.
- `GET /{id}/vectors/neighbors?q=...` or `?note=...` (`limit` up to 200, optional `provider`): the
  nearest chunks, filtered by the profile. Either form embeds text with the vault's provider, called
  from the daemon with the device-local provider configuration and the key from the daemon's
  environment, so the profile must allow the provider's endpoint; it is checked before any request.
  Related notes are `GET /{id}/related?note=...`, a mounted serve route.
- `POST /{id}/vectors/index` (body: optional `dry_run`, `provider`): embeds new and changed chunks
  behind the mutation gate with an `Idempotency-Key`; the profile must allow indexing and the
  provider's endpoint. A daemon built without the `vectors` feature answers these routes with 501.
- `GET /{id}/doctor`: the vault's diagnostics, as `vulcan doctor` reports them, never repairs. The
  report names every note it flags, so a profile that does not read every note, or that has a policy
  hook, is refused with 403.

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

Writes run the shared workflows the CLI and MCP use (`vulcan_app::mcp_notes`, `inbox`, `browse`
moves, `mcp_scan`), so they check the profile, refresh the cache, and auto-commit like the CLI.
Each waits for the host's mutation permit for its vault and repository (`HostedWorkerGate`'s file
write gate, which unlike the Git workers' gate does not require Git permission), so REST writes
serialize with hosted MCP writes and daemon workers.

Every write needs an `Idempotency-Key` header (1 to 128 visible ASCII characters), scoped to the
principal and vault. The key's record is written as pending before the workflow runs and completed
with the response after; a replay of the same method, path, query, and body returns the stored
response, a different request under the same key is refused with 409, and so is a key whose
outcome was never recorded (the daemon stopped mid-write), so an ambiguous write is never run
twice. Records live under `daemon/rest-idempotency/` in the state directory, one file per key, and
are pruned after a day. A response timeout ends only the caller's wait; the write still records
its outcome for a later replay.

- `POST /{id}/notes`: create a note (`path`, `body`, `template`, `frontmatter`).
- `PUT /{id}/notes/{path}`: replace content (`content`, `preserve_frontmatter`). A PUT confirms
  the replacement by itself.
- `PATCH /{id}/notes/{path}`: find and replace (`find`, `replace`, `all`, selectors).
- `DELETE /{id}/notes/{path}`: delete a note; confirms by itself, `dry_run` previews.
- `POST /{id}/append/{path}`: append (`text`, `mode`, `heading`).
- `POST /{id}/inbox`: quick capture (`text`), checked against write access to the inbox note.
- `POST /{id}/move`: move or rename with link rewriting (`source`, `destination`, `dry_run`). The
  profile must allow refactoring the note and writing every file the move changes; a dry run
  plans the move first and its changed files are checked before anything is written.
- `POST /{id}/scan`: an incremental scan, or a full one with `full`.
- `POST /{id}/update`: `where` filters (at least one), `set` properties, and `unset` properties,
  through `vulcan_app::properties::update_properties_where`. The notes are selected once, before
  any change, and the changes are applied in order and auto-committed together.

Every body accepts `no_commit`. Route-given fields (`note`, `confirm`) are refused in the body.

## Delivery

1. Tokens and principals: the token store, `vault token` commands, and middleware that resolves
   either credential to a principal and enforces its reach.
2. Daemon and read routes, plus `openapi.json`.
3. History routes.
4. Write routes with idempotency.

Each slice ships with tests and its roadmap update.
