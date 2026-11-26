# MCP server

Status: target design, decided with Eric 2026-11-25. Replaces the 9.19.15 protocol-native server,
the 9.23 tool packs and the 10.10 transport details; supersedes the parts of
`unified-hosting.md` that describe the hand-written MCP listener. Evidence and alternatives:
[`docs/investigations/2026-11-25-mcp-server-evaluation.md`](../investigations/2026-11-25-mcp-server-evaluation.md).

Decisions taken with this spec:

- **Clean break.** Tool names, arguments and result shapes change without aliases or a
  deprecation window. Named-remote grants that narrowed packs need fresh consent; installed
  skills are refreshed with `vulcan agent install`.
- **The permission profile alone decides what is listed.** Tool packs are removed. A new
  `destructive` profile permission covers overwrite and delete.
- **Three HTTP auth modes:** loopback without auth, a static bearer token, and named remotes
  (local OAuth authorization server with IndieAuth sign-in). Direct local OAuth (approval token,
  static client) and external resource-server mode are removed.
- **Dual-era protocol:** 2025-06-18 and 2025-11-25 through `initialize`, 2026-07-28 stateless.
  ChatGPT, VS Code and Cursor are on 2025-11-25; Claude Code and claude.ai are moving to
  2026-07-28.

## 1. Principles

1. **One stable list per authority.** `tools/list` depends only on the vault, its trust state
   and the caller's resolved permission profile, never on earlier requests. Order is fixed.
2. **Stateless core.** A request carries everything needed to serve it: authority, protocol era,
   client capabilities, client identity, cancellation. State an agent needs across calls is an
   explicit handle passed as an argument (a changeset id, a cursor, an operation id).
3. **One tool per action, honestly annotated.** Reads and writes are separate tools.
   `readOnlyHint`, `destructiveHint`, `idempotentHint` and `openWorldHint` are accurate, because
   ChatGPT confirmations, Claude permissions, Claude Code parallelism and retries depend on them.
4. **Bounded, typed results.** Every built-in tool has a typed `outputSchema` and always returns
   conforming `structuredContent`. Results are bounded by defaults and paging, not by spilling.
5. **Errors the model can act on.** Input problems are tool errors that name the parameter and
   the fix, phrased in MCP terms (tool and argument names), never CLI commands.
6. **Clients do the context work.** No server-side disclosure mechanism; instead good names,
   search-friendly descriptions, a short tool-family map in the instructions, and
   `anthropic/alwaysLoad` on the core tools.
7. **Workflows live in skills**, served to MCP clients in MCP terms.

## 2. Layering

| Layer | Crate and module | Owns |
|---|---|---|
| Core | `vulcan-app::mcp` | Envelope parsing and validation, version and era handling, `server/discover`, method dispatch, catalog and tool handlers, resources, prompts, skills, completion, tasks, input requests, result and error shaping. Synchronous. No transport or session types. |
| Legacy adapter | `vulcan-app::mcp::legacy` | The `initialize` handshake and what a 2025 client expects of a connection: negotiated version, client info, `list_changed` delivery. A thin wrapper over the core. |
| HTTP | `vulcan-daemon::mcp_transport` and `mcp_http_*` | hyper listener and routing for the MCP endpoint (POST; legacy GET and DELETE), `subscriptions/listen`, Origin and Host policy, auth extraction, OAuth and metadata routes for named remotes. Used by the resident daemon and by foreground `vulcan mcp --transport http` (which starts a tokio runtime). Core calls go through `spawn_blocking`. |
| stdio | `vulcan-cli::mcp` | Flag parsing; a reader thread that dispatches each request to a worker, so reads run in parallel and `notifications/cancelled` works. |

The core entry point is one function:

```rust
fn handle(&self, request: McpRequest, ctx: &RequestContext) -> McpOutcome
```

`RequestContext` holds: resolved authority (vault paths, permission guard, grant id, OAuth
client id and subject), protocol era and version, client capabilities (from `_meta` or the
legacy handshake), client info, a cancellation token, and an optional progress sink.
`McpOutcome` is a response, an `input_required` result, a task, or nothing (notifications).

## 3. Protocol

### Versions and eras

Supported: `2025-06-18`, `2025-11-25` (legacy) and `2026-07-28` (modern).

- A request carrying `_meta["io.modelcontextprotocol/protocolVersion"]` is modern and served
  statelessly. A modern request without the required `_meta` fields is rejected with -32602
  (HTTP 400).
- `initialize` selects legacy semantics for the stdio process or the HTTP legacy session. The
  server answers with the client's requested version when supported, otherwise `2025-11-25`.
- An unsupported version gets `UnsupportedProtocolVersionError` (-32022) listing the supported
  versions. On HTTP, `MCP-Protocol-Version` must name a supported version and, on modern
  requests, match `_meta` (-32020 otherwise).
- `server/discover` is implemented in both eras.

### Envelope

- Exactly one JSON-RPC message per stdio line or POST body; `jsonrpc` must be `"2.0"`; arrays are
  invalid requests.
- Method params tolerate unknown fields (forward compatibility); tool *arguments* do not
  (`additionalProperties: false`), and an unknown argument is a tool error that lists the valid
  ones.
- Modern results carry `resultType` and `_meta["io.modelcontextprotocol/serverInfo"]`.
  `server/discover`, `tools/list`, `prompts/list`, `resources/list`, `resources/templates/list`
  and `resources/read` carry `ttlMs` and `cacheScope` (`private`: every list depends on the
  caller's authority).
- On HTTP, modern requests send `Mcp-Method`, and `Mcp-Name` for `tools/call`, `resources/read`
  and `prompts/get`. The server uses them only for routing and logging; the body is
  authoritative and a mismatch is -32020.

### Capabilities

Server: `tools` (`listChanged`), `resources` (`listChanged`), `prompts` (`listChanged`),
`completions`, and extensions `io.modelcontextprotocol/skills` (`directoryRead: true`) and
`io.modelcontextprotocol/tasks`. No `logging`, no resource subscriptions, no sampling or roots.

`serverInfo`: `name: "vulcan"`, `title: "Vulcan"`, `version`, `websiteUrl`, `icons`.

### Instructions

Generated from the visible catalog, at most 2,000 characters, with the first 512 characters
self-contained (ChatGPT reads only those reliably): what the server is, the tool families present
(notes, files, search and query, daily notes, tasks, graph, web, git and changesets, sync,
configuration), when to reach for each, and that workflows are available as skills.

### Errors

| Condition | Response |
|---|---|
| Malformed JSON | -32700 |
| Invalid envelope | -32600 |
| Unknown method | -32601 (HTTP 404 on modern requests) |
| Bad method params; unknown tool; unknown resource or prompt | -32602 (legacy clients may also see -32002 for resources until removed) |
| Unsupported version / header mismatch / missing client capability | -32022 / -32020 / -32021 |
| Internal failure | -32603 |
| Cancelled before completion | -32800 |
| Tool input invalid, domain failure, permission denied, timeout | Tool result with `isError: true` |

A tool error result has text content with the message and `structuredContent`
`{"error": {"code": "<stable_code>", "message": "...", "parameter": "<name>?", "hint": "...?"}}`.
Stable codes: `invalid_argument`, `not_found`, `ambiguous`, `conflict` (hash mismatch),
`permission_denied`, `index_missing`, `timeout`, `indeterminate`, `failed`.

## 4. State model

No protocol sessions in the core. Every former piece of session state has an explicit
replacement:

| Former session state | Replacement |
|---|---|
| Selected and adaptive tool packs | Removed; the profile decides |
| Large-result store (`vulcan://tool-results/N`) | Removed; bounded results, stable resource links (§6) |
| Changeset attachment (`changeset_switch`) | Optional `changeset` argument on every note, file, search, query, task and git tool |
| Uncommitted written paths (`git_commit scope=session`) | `git_commit` commits a `changeset`, explicit `paths`, or everything (`all: true`) |
| Client name and version | Per request: modern `_meta` client info, or the legacy handshake |
| Profile ceiling ("never widen after init") | Per request: the token's grant and the remote's ceiling, as named remotes already re-resolve |
| `list_changed` fingerprints | A server-level change watcher (§8) |

The legacy adapter keeps only the negotiated version, client info and the connection's
notification stream.

## 5. Visibility and permissions

A tool is listed and callable exactly when the resolved profile grants its requirement:

| Requirement | Profile setting |
|---|---|
| read | `read` (path-scoped; results filtered per path) |
| write | `write` (path-scoped) |
| destructive | `write` **and** `destructive = allow` (new) |
| refactor | `refactor` (path-scoped) |
| index | `index` |
| network | `network` (origin-scoped) |
| config read / write | `config` |
| git read / git write | `git` (plus full-vault read for history tools) |
| skill command | vault trust, `execute`, and the command's declared requirements |

**`destructive`** (new, `PermissionMode`; `deny` when unset and in the built-in `readonly`,
`researcher` and `daily-wiki-agent` profiles; `allow` in `writer`, `maintainer` and
`unrestricted`): covers `note_set`, `note_delete`, `file_put` with
`overwrite: true`, `changeset_abandon`, `task_reminder_remove`, and `config_set`. Path-scoped
`write` still applies. `note_create` with an existing target and `note_patch` are not
destructive (they fail on conflict or edit in place).

Skill command tools are listed only for trusted vaults, as today. A vault exposing more than 64
skill command tools is listed with the first 64 by name and a diagnostic.

## 6. Tool catalog

### Conventions

- **Names:** `family_action`, lowercase, unique within the server, at most 40 characters.
- **Note references:** `note` resolves a reference (path, title or alias) to an existing note;
  `path` is a literal vault-relative path (creation, files). An ambiguous `note` is an
  `ambiguous` tool error listing the candidates, or, on modern clients that support form
  elicitation, an `input_required` choice.
- **Mutations:** `dry_run` on every mutation; `expected_hash` (from the `content_hash` that every
  note and file read returns) on every edit of existing content; `idempotency_key` on
  `note_append`, `note_create` and `task_create`; `no_commit` where auto-commit applies;
  `check` (run diagnostics on the result) on note writes. `confirm` is removed.
- **Paging:** `limit` and an opaque `cursor`; results carry `page: {next_cursor, has_more}` and,
  where cheap, `total`. Every list-shaped tool pages and has a documented default and maximum.
- **Detail:** `detail: "concise" | "full"` (default `concise`) on `search`, `task_query`,
  `git_log`, `git_blame` and `sync_status`. Concise rows keep identifiers needed for follow-up
  calls and drop duplicated or diagnostic fields.
- **Sorting:** `sort` (enum per tool) and `order` (`asc` | `desc`).
- **Output keys:** snake_case everywhere.
- **Descriptions:** the first sentence says what the tool does in search-friendly words; at most
  400 characters; no instructions about agent behavior.
- `_meta["anthropic/alwaysLoad"]: true` on `search`, `query`, `note_get`, `note_outline` and
  `note_append`.

### Tools

| Tool | Requirement | Annotations | Notes |
|---|---|---|---|
| `note_get` | read | ro, idem | Selectors (`section_id`, `heading`, `block_ref`, `lines`, `match`), `evaluate`; returns `content_hash`. Over the size cap: the head, `truncated`, and a `resource_link` to `vulcan://note/{path}` |
| `note_outline` | read | ro, idem | Sections with ids and line spans |
| `note_info` | read | ro, idem | Metadata, links, backlink counts |
| `note_create` | write | idem with key | Fails if the path exists |
| `note_append` | write | idem with key | Heading, periodic target |
| `note_patch` | write | – | Find/replace scoped by selector; `expected_hash` |
| `note_set` | destructive | destr | Replace the whole note; `expected_hash` |
| `note_delete` | destructive | destr | `expected_hash` |
| `note_move` | refactor | – | Rewrites links |
| `file_get` | read | ro, idem | Non-Markdown and raw reads, byte paging |
| `file_put` | write (destructive with `overwrite`) | destr | Text or base64 |
| `search` | read | ro, idem | Full-text and semantic; paging; `detail` |
| `query` | read | ro, idem | Query DSL, DQL or structured filters; paging |
| `daily` | read | ro, idem | `operation`: `latest`, `today`, `show`, `list`, `range` |
| `task_query` | read | ro, idem | Tasks DSL or structured filters (replaces `task_list` and `task_query`); paging; `detail` |
| `task_create` | write | idem with key | |
| `task_complete` | write | idem | |
| `task_reschedule` | write | idem | |
| `task_reminder_add` | write | – | |
| `task_reminder_remove` | destructive | destr, idem | |
| `graph_communities` | read | ro, idem | Computes without persisting; persisting stays an index job |
| `link_suggestions` | read | ro, idem | Ranked suggestions; paging |
| `link_suggestion_resolve` | write | – | Accept or reject one suggestion (split out of `suggest_links`, which wrote behind a read-only annotation) |
| `status` | read | ro, idem | Vault, index and server state; the caller's effective permissions; replaces `capabilities` |
| `index_scan` | index | – | Task-capable |
| `web_search` | network | ro, open | |
| `web_fetch` | network | ro, open | Task-capable |
| `config_get` | config read | ro, idem | Renamed from `config_show`; paths relative to the vault |
| `config_set` | config write + destructive | destr | |
| `sync_status` | git read | ro, idem, open | Replaces `sync_status` and `sync_plan`; `detail` |
| `sync_doctor` | git read | ro, idem, open | |
| `sync_conflicts` | git read | ro, idem | |
| `sync_refresh` | git write | open | Task-capable |
| `sync_resume` | git write | idem, open | |
| `git_status` | git read | ro, idem | |
| `git_diff` | git read | ro, idem | Paging |
| `git_log` | git read | ro, idem | Paging; `detail` |
| `git_blame` | git read | ro, idem | Paging; `detail` |
| `git_commit` | git write | – | `changeset`, `paths` or `all` |
| `changeset_begin` | git write | – | Returns the changeset id |
| `changeset_list` | git read | ro, idem | |
| `changeset_land` | git write | – | Task-capable |
| `changeset_abandon` | git write + destructive | destr | |
| `operation_get` | none (caller-bound) | ro, idem | Status and outcome of an operation id from an indeterminate or task-backed call; the legacy-era counterpart of `tasks/get` |
| `skill_<skill>_<command>` | per command | from sandbox tier | Description from the command, not the skill; `outputSchema` omitted unless declared |

Removed: `capabilities`, `daily_show`, `daily_list`, `task_list`, `sync_plan`, `suggest_links`,
`config_show`, `changeset_switch`, `tool_packs` and its `tool_pack_*` aliases.

44 built-in tools; 12 under a read-only profile.

## 7. Results

- `structuredContent` always present and conforming to the tool's `outputSchema`.
- One text content block with the compact JSON serialization of `structuredContent` (the spec's
  compatibility form). Tools with a natural text form (`note_get` content, `web_fetch` Markdown)
  put that text first and the JSON second.
- No result-size spill. Each tool has a default and maximum page size; `note_get` and
  `file_get` cap inline content (default 64 KiB) and link the rest. `_meta`
  `anthropic/maxResultSizeChars` is set on `note_get` and `file_get` to their cap.
- `content_hash` on every note and file read and write.

## 8. Resources, prompts, completion, change notification

Resources:

| URI | Content |
|---|---|
| `vulcan://note/{path}` (template) | `text/markdown`, the note source; permission-filtered |
| `vulcan://file/{path}` (template) | The file with its MIME type (text, or blob for binary) |
| `vulcan://help/{topic}` (template), `vulcan://help/index` | `text/markdown` help written in MCP terms |
| `vulcan://agents` | The vault's `AGENTS.md`, when present |
| `skill://{name}/…` | Skill files (§9) |

`resources/list` returns the help index, `vulcan://agents` and the visible skills' `SKILL.md`
resources (legacy clients find skills this way); notes are reached through the templates, with
completion for `{path}` and `{topic}`. Removed: `vulcan://assistant/*` indexes and config
summary, `vulcan://tool-results/*`.

Prompts: Markdown files in `assistant.prompts_folder`, with standard `arguments` (no extra
fields) and completion for arguments typed as notes.

Change notification: list fingerprints (tool catalog, prompt files, resource files) are compared
every 2 seconds by a watcher: one thread per HTTP listener for the legacy sessions with a GET
stream, one per stdio process, and each `subscriptions/listen` stream for its own authority. A
change is delivered to modern `subscriptions/listen` streams that opted in (first message
`notifications/subscriptions/acknowledged`, every notification carrying
`_meta["io.modelcontextprotocol/subscriptionId"]`), legacy HTTP GET streams and the legacy stdio
connection. Resource update subscriptions are not offered. `listChanged` is advertised to modern
clients only where the transport serves `subscriptions/listen`.

## 9. Skills

**Sources:** bundled skills compiled into the binary, and vault skills in
`assistant.skills_folder`; a vault skill overrides a bundled one with the same name.

**Visibility:** a bundled skill is served over MCP only when it has an MCP rendering (an
`<!-- vulcan:mcp -->` block). Any skill is listed only if it names no built-in MCP tool in its
`tools:` frontmatter or at least one the caller may call (names that are CLI commands or custom
tools do not count), so a read-only connection still gets the note skill for its reading half.

**Modern clients:** the `io.modelcontextprotocol/skills` extension. `skills/list` and
`skills/get` return entries with `uri` (`skill://{name}/SKILL.md`), the frontmatter verbatim,
and a manifest of every file with `sha256:` digest and size; `resources/read` serves files;
`resources/directory/read` lists directories. `ttlMs` 300000, `cacheScope` `private`.

**Legacy clients:** the same `skill://` files through `resources/list` and `resources/read`.

**MCP rendering:** bundled skills are written once with target blocks:

```markdown
<!-- vulcan:cli -->Run `vulcan note outline <note>`, then `vulcan note get <note> --section <id>`.<!-- /vulcan:cli -->
<!-- vulcan:mcp -->Call `note_outline`, then `note_get` with `section_id`.<!-- /vulcan:mcp -->
```

`vulcan agent install` renders the CLI form; the MCP server renders the MCP form. Text outside
blocks is shared. Vault skills are served as written. Digests are computed over the rendered
bytes.

Skill command tools stay tools (§6).

## 10. Long-running operations, input and progress

**Operation ledger:** every mutation that can outlive its request (all writes on HTTP, the
task-capable tools everywhere) is registered durably before dispatch with an operation id bound
to the caller, as named remotes do today. On stdio every tool call is recorded in memory for the
life of the process, which is as long as its one client lives; `operation_get` reads those
records and is listed where a transport keeps them: stdio, and HTTP, where each listener records the calls its hosted ledger does not keep, owned by the caller (connection grant, else credential), and `operation_get` reads the ledger first.

**Tasks** (modern clients that declare `io.modelcontextprotocol/tasks`; the extension is
declared in `server/discover` where the transport keeps operation records): a call of a
task-capable tool (`index_scan`, `sync_refresh`, `changeset_land`, `web_fetch`, `query`, `search`)
still running after 5 seconds answers `resultType: "task"`, and keeps running uncancelled. The
task id is the call's operation id: `tasks/get` reports `working`, then `completed` with the
`tools/call` result (tool errors included), `failed` with the JSON-RPC error, or `cancelled`;
`operation_get` reads the same record. `tasks/cancel` asks the call to stop (as do `notifications/cancelled` and,
on HTTP, a modern client leaving); a call that has started stops where its tool can stop safely:
`index_scan` between files, rolling its whole index change back; `changeset_land` before its
commit, rebase or fast-forward (after the fast-forward it only skips the index refresh);
`sync_refresh` at the sync engine's next safe point when it runs the cycle itself. Other tools run
to completion. Vulcan's tasks do
not ask for input, so `tasks/update` is acknowledged and changes nothing. Records last an hour
after the call finishes (`ttlMs`), `pollIntervalMs` is 2000; an unknown task is -32602, and a
client that has not declared the extension gets -32021. On a named remote, a hosted mutation
that may become a task is also recorded in memory under its durable ledger id, so the task
reports the call's result; the ledger stays the durable record (`operation_get`,
`status_path`). After a restart `tasks/get` answers from the ledger record: `completed` with a
result naming its commit state (the call's own result is not stored), or `failed` for a failed,
interrupted or unknown outcome; `tasks/cancel` records the request there. Hosted mutations from clients that do
not declare the extension answer as before, with their operation id and `status_path`.

**Timeouts without tasks:** a call that exceeds `--request-timeout` returns an `indeterminate`
tool error carrying the operation id; the work either finishes (and `operation_get` reports it)
or is cancelled through its token. A timed-out worker is never left running unobserved.

**Input requests** (modern clients that declare form elicitation): a `tools/call` whose note
argument matches several notes the caller may read answers `input_required` with one form
request, keyed `note:<argument>`, offering the candidate paths (at most 25) as a single choice.
The retry carries the arguments, so there is no `requestState`: an accepted answer replaces that
argument; a declined or cancelled one, or a client without form elicitation, gets the `ambiguous`
tool error. Destructive operations do not elicit by default (client approval prompts already
cover them). With `mcp.confirm_destructive = true` in the vault's config, a destructive call
(a tool needing the `destructive` permission, or `file_put` with `overwrite`; never a dry run)
from a modern client that declares form elicitation answers `input_required` with one form
request, keyed `confirm:destructive`, holding a boolean `confirm`; only a retry that accepts with
`confirm: true` runs. A decline, an unchecked box, a client without form elicitation and a 2025
client get a tool error (`confirmation_declined`, or `confirmation_required` when the client
cannot ask) and nothing changes. The setting is read per call.

**Progress:** a tool call whose `_meta` carries a `progressToken` (either era) gets
`notifications/progress` for it while it runs, at most every 250 ms, always increasing, and
never after its response (nor once it has become a task). `index_scan` reports files scanned of
files found, then its finishing phases, ending at the total; `changeset_land` reports each step
(commit, rebase, fast-forward, index refresh) of four; `sync_refresh` reports each phase of a
cycle it runs itself (fetching, merging, pushing and so on), while one queued on the daemon
answers at once after a start notification. On stdio the notifications are lines before the response; on HTTP the first one turns the
response into an event stream whose last event is the response, and a call that reports nothing
is answered with plain JSON.

## 11. Transports

### HTTP

- One endpoint path per listener; POST for every request and notification. Responses are
  `application/json`, or `text/event-stream` when the request has progress or input to stream.
- Modern: no session header. `subscriptions/listen` is a long-lived POST stream with keepalive
  comments every 15 seconds and `X-Accel-Buffering: no`. Closing a request's stream cancels it:
  a modern `tools/call` from a client that accepts `text/event-stream` and still runs after one
  second is answered with an event stream carrying a keepalive comment every two seconds (and its
  progress, if asked), its response the last event. When a write fails because the client has
  gone, the request's token is cancelled. A plain end of input is not taken as leaving, since the
  listener allows half-close; a call that became a task is not cancelled by the end of its
  response.
- Legacy: `initialize` creates a legacy session (`Mcp-Session-Id`, idle expiry 30 minutes, at
  most 256), GET opens its notification stream, DELETE ends it, `notifications/cancelled`
  cancels by request id. A refreshed token on the same session is accepted when it resolves to
  the same grant.
- **Origin:** when present, must be a loopback origin on a loopback listener or the configured
  public URL's origin; otherwise 403. **Host:** must be an IP address, `localhost` on a loopback
  or wildcard listener, or the public URL's host; a wildcard listener without a public URL
  accepts any. Otherwise 421. (IP addresses cannot be used for DNS rebinding, and proxies often
  rewrite Host to the backend address.)
- Limits: 4 MiB request bodies, 64 concurrent requests per listener, `--request-timeout`
  (default 120 s) per request, keep-alive allowed.

### stdio

Newline-delimited JSON; nothing but MCP messages on stdout; diagnostics on stderr. A reader
thread dispatches to a bounded worker pool (default 4); writes are serialized by the vault write
gate; responses are written as they complete. `notifications/cancelled` trips the request's
token. The process exits on stdin EOF after in-flight requests finish or 5 seconds pass.

## 12. Authorization

Modes:

| Mode | When | Credential |
|---|---|---|
| None | Loopback listener without other auth | — |
| Static bearer | `--auth-token`; scripts and automation | `Authorization: Bearer` only (`X-Vulcan-Token` removed) |
| Named remote | `vulcan mcp remote …` | OAuth 2.1 from Vulcan's own authorization server |

Named remotes:

- **Metadata:** Protected Resource Metadata at `/.well-known/oauth-protected-resource{path}` and
  the root, with `resource`, `authorization_servers` (the remote's issuer), `scopes_supported`
  `["mcp"]`, `bearer_methods_supported` `["header"]`, `resource_name`. Authorization server
  metadata at `/.well-known/oauth-authorization-server{path}` and
  `/.well-known/openid-configuration{path}` (clients probe both), with
  `client_id_metadata_document_supported: true`, `registration_endpoint`,
  `code_challenge_methods_supported: ["S256"]`,
  `authorization_response_iss_parameter_supported: true`.
- **Client registration:** CIMD (preferred) and DCR (deprecated by the spec, kept for clients
  that need it). CIMD documents are fetched with a streaming 64 KiB cap, DNS pinning, no
  redirects, IPv4-mapped and other special-purpose addresses refused, and cached per HTTP caching
  headers for at most one hour. Public clients and `client_secret_basic`/`post` confidential DCR
  clients.
- **Authorization:** IndieAuth sign-in, consent page choosing vault, profile (within the
  remote's ceiling) and expiry. PKCE S256 required; exact redirect match with loopback and app
  scheme policy; `state` passed through; `iss` added to the redirect (RFC 9207).
- **Tokens:** short-lived JWT access tokens (`aud` = the resource URL, `scope` space-delimited),
  rotating refresh tokens with replay revocation, durable grants re-resolved on every request.
  `resource` validated at authorize, token and refresh.
- **Challenges:** 401 `WWW-Authenticate: Bearer resource_metadata="…", scope="mcp"` (no `error`
  when no token was sent); `error="invalid_token"` for bad or expired tokens.
- Secrets and approval-style values compared in constant time.
- Scopes: one scope, `mcp`. Authority comes from the grant's profile, not from scopes.

Grants store vault, profile, expiry, client, audience and scope. The packs field is dropped.

## 13. Migration

- A grant approved under packs keeps them until its next token use or refresh. Then, if its packs
  covered every pack its profile could use, it keeps working and forgets them; otherwise it is
  revoked, the client is told to reconnect, and `vulcan mcp connections list` shows it revoked.
- `--tool-pack` and `--tool-pack-mode` are removed; `vulcan mcp` and `vulcan describe` reject them
  with a pointer to permission profiles. Named-remote definitions and vault `[mcp_remote]` settings
  drop `tool_packs` (old values load and are ignored), as does skill-command frontmatter; `--auth` modes other than the
  three above are rejected with a pointer to named remotes.
- `vulcan describe --format mcp|openai-tools` exports the same catalog the server lists for the
  given profile.
- Bundled skills, the AGENTS template, `docs/guide/mcp*.md`, `docs/guide/chatgpt-mcp.md` and the
  `mcp-setup` skill are rewritten for the new catalog.

## 14. Testing

- Core: unit tests per handler; golden `tools/list` per built-in profile (stable order and
  bytes); schema conformance of every result against its `outputSchema`; error shapes.
- Eras: the same scenario suite over a legacy stdio connection, a modern stdio connection, legacy
  HTTP and modern HTTP.
- Transports: Origin and Host policy, cancellation by stream close and by notification,
  concurrency, `subscriptions/listen` delivery, timeout to `indeterminate` to `operation_get`.
- Auth: metadata documents, challenges, CIMD fetch limits and caching, PKCE, `iss`, refresh
  rotation and replay, grant migration.
- Skills: rendering of target blocks, digests, visibility by profile, legacy resource path.
