# Daemon REST API

The daemon serves an HTTP API on its loopback listener, the same one the Obsidian companion uses.
It answers reads from the daemon's retained index, so a script, a dashboard, or another program can
query a vault without starting a CLI process per request. Responses are the same JSON the CLI prints
with `--output json`.

The API listens on loopback only (`127.0.0.1:3210` by default; see `vulcan daemon config show`).
It is not meant to be exposed to a network directly.

## Get a token

Register the vault, start the daemon, and create a token for the vault:

```bash
vulcan vault add notes ~/vaults/notes --no-sync   # or keep Git sync enabled
vulcan daemon start --detach
vulcan vault token create notes --label dashboard
```

The token (`vat_…`) is printed once. Store it where the client keeps secrets; Vulcan keeps only a
hash. `vulcan vault token list` shows token IDs and labels, and
`vulcan vault token revoke <token-id>` revokes one immediately.

A token reaches only its own vault. Every request runs under the vault's registered permission
profile (`vulcan vault set notes --permissions-profile <profile>`): reads return only notes the
profile may read, and operations it does not allow fail with 403. A token cannot reach Git sync
administration, the job and event streams, or other vaults; those answer 404.

## Make a request

Every request carries the token and the protocol version header:

```bash
TOKEN=vat_...
curl -s -H "Authorization: Bearer $TOKEN" -H "Vulcan-Protocol-Version: 1" \
  "http://127.0.0.1:3210/notes/search?q=dashboard&limit=5"
```

## Routes

`{id}` is the registered wiki ID. Note paths are vault-relative and may contain `/`.

| Route | Answers like |
| --- | --- |
| `GET /{id}/search?q=...` | `vulcan search` (keyword) |
| `GET /{id}/notes?where=...&sort=...&limit=...` | `vulcan query` with property filters |
| `GET /{id}/query?dsl=...` | `vulcan query` with the query DSL |
| `GET /{id}/dataview/query?dql=...` | `vulcan dataview query` |
| `GET /{id}/bases/eval?file=...` | `vulcan bases eval` |
| `GET /{id}/notes/{path}` | `vulcan note get` (supports `heading`, `section_id`, `block_ref`, `lines`, `match`, `context`, `no_frontmatter`, `raw`) |
| `GET /{id}/info/{path}` | `vulcan note info` |
| `GET /{id}/links/{path}`, `GET /{id}/backlinks/{path}` | `vulcan links`, `vulcan backlinks` |
| `GET /{id}/graph/stats`, `/graph/hubs`, `/graph/path?from=...&to=...` | `vulcan graph stats|hubs|path` |
| `GET /{id}/mdbase/...` | the `vulcan serve` mdbase routes |
| `GET /{id}/history?limit=...` | `vulcan git log` |
| `GET /{id}/history/{path}` | `vulcan note history`; with `?at=<commit>`, the file's content at that commit |
| `GET /{id}/diff?path=...` | `vulcan git diff` (uncommitted changes) |
| `GET /{id}/diff/{path}?from=<commit>&to=<commit>` | one file between two commits, or from a commit to the working tree when `to` is left out |
| `GET /health` | which vaults the daemon serves, and their cache state |
| `GET /vaults` | registered vaults visible to the token |
| `GET /openapi.json` | an OpenAPI 3.1 description of these routes |

Errors are JSON objects with `kind` and `detail`: 400 for an invalid request, 401 for a missing or
revoked token, 403 when the profile denies the operation, 404 for an unknown vault or note, and 503
when the daemon does not index the vault (a files-only directory, or while it is starting).

## Writes

Writes take a JSON body and an `Idempotency-Key` header. Pick a fresh key for each new write and
reuse it only to retry that same request: a retry returns the original response instead of writing
twice. If the daemon stopped before it recorded a write's outcome, the key is refused with 409;
check the vault before writing again with a new key.

```bash
curl -s -X POST -H "Authorization: Bearer $TOKEN" -H "Vulcan-Protocol-Version: 1" \
  -H "Idempotency-Key: $(uuidgen)" -H "Content-Type: application/json" \
  -d '{"path": "Inbox/Idea", "body": "A fresh idea."}' \
  http://127.0.0.1:3210/notes/notes
```

| Route | Answers like | Body |
| --- | --- | --- |
| `POST /{id}/notes` | `vulcan note create` | `path`, `body`, `template`, `frontmatter` |
| `PUT /{id}/notes/{path}` | `vulcan note set` | `content`, `preserve_frontmatter` |
| `PATCH /{id}/notes/{path}` | `vulcan note patch` | `find`, `replace`, `all`, and the `note get` selectors |
| `DELETE /{id}/notes/{path}` | `vulcan note delete` | optional `dry_run` |
| `POST /{id}/append/{path}` | `vulcan note append` | `text`, `mode`, `heading` |
| `POST /{id}/inbox` | `vulcan inbox` | `text` |
| `POST /{id}/move` | `vulcan refactor move` | `source`, `destination`, `dry_run` |
| `POST /{id}/scan` | `vulcan scan` | `full` |

Every write body also accepts `no_commit`. Writes run under the profile like reads, so a read-only
profile cannot write, and a move needs permission to refactor the note and to write every file whose
links it rewrites. Writes wait their turn with the daemon's other writers to the same vault, then
refresh the index and auto-commit as the CLI would.

## History

History and diffs read Git directly, so they also work for vaults the daemon does not index. They
need a profile that allows Git. Whole-vault history and uncommitted diffs name every changed file,
so they also need a profile that reads every note; a restricted profile can still ask for one file
it may read. Commits are named by their hexadecimal ID only, not by branch names or `HEAD~1`.
Commit messages are shown as written and can mention other files.

Reads wait until the daemon has indexed every change made before the request, so a note written a
moment ago is found. `GET /{id}/related` and `search` with `mode=hybrid` use the vault's own
embedding configuration, as `vulcan serve` does.
