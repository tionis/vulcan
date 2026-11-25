# Evaluation: the MCP server against current MCP practice (2026-11-25)

Scope: the whole MCP surface (protocol and lifecycle, session state, the HTTP and stdio
transports, the tool/resource/prompt catalog, result and error shapes, authorization) judged
against the MCP 2026-07-28 revision and its extensions (skills, tasks), the 2025-11-25 revision,
Claude Code and Claude connector guidance, and Anthropic's tool-design guidance. The question is
which design decisions still hold and which were made for a protocol or client generation that
has since moved on.

Method: five independent read-only passes (two over the specification and vendor guidance, three
over the code: protocol/transport, tool surface, authorization). The tool-surface pass drove a
debug build over stdio on a copy of `tests/fixtures/vaults/basic` and measured real responses.
Load-bearing claims were then checked in code (marked **verified**). Line references are at
`3f88f651`.

Builds on [2026-11-23-sync-identity-mcp-audit](2026-11-23-sync-identity-mcp-audit.md), whose
MCP coverage gaps (git, `note_move`, hashes, `file_get`, changesets) have since been closed.

## Verdict

- **The core is sound and worth keeping**: a curated catalog with titles, annotations and input
  and output schemas, permission-profile filtering at list and call time, bounded results,
  optimistic concurrency (`expected_hash`, `idempotency_key`), the hosted mutation scheduler, and
  an OAuth server whose security basics (PKCE S256 only, exact redirect match, rotating refresh
  families with replay detection, per-request grant re-resolution) are right.
- **The protocol generation is a year old.** The server speaks only `2025-06-18`, ignores the
  client's requested version, and rejects any other `MCP-Protocol-Version` header with HTTP 400.
  2026-07-28 removed the `initialize` handshake, protocol sessions, the GET stream and
  server-initiated requests. Skills over MCP and Tasks are defined only on 2026-07-28, so none of
  the features in the 2026-11-24 announcement are reachable without a protocol upgrade.
- **Several designs exist to work around limits clients no longer have**, and one conflicts
  with the new spec: adaptive tool packs make `tools/list` vary as a side effect of a tool call,
  which 2026-07-28 forbids, and client-side tool search (on by default in Claude Code) has taken
  over their context-saving job. The session result store, the doubled pretty-text payload, the
  `confirm: true` parameters and the homegrown operation-status endpoint each have a standard
  replacement now.
- **The HTTP transport is the weakest part**: a hand-written HTTP/1.1 server with `Connection:
  close`, one thread per connection, per-session serialization, and an SSE stream that stalls
  while a request runs. The daemon already uses axum for REST, and the roadmap always meant axum
  to replace the hand-rolled listener (ROADMAP §10, "HTTP framework").
- **Bundled skills are written for a shell.** Their frontmatter lists MCP tool names, but their
  bodies teach CLI syntax (`note get --section <id>`). Served to an MCP-only agent (claude.ai, a
  phone, ChatGPT) they teach commands the agent cannot run.

## 1. What changed in MCP that bears on Vulcan

| Change | Revision | Effect on Vulcan |
|---|---|---|
| `initialize`/`initialized` handshake removed; every request carries `_meta` protocol version, client capabilities, client info; `server/discover` is mandatory | 2026-07-28 | No negotiation today; no `server/discover` (falls through to -32601) |
| Protocol sessions and `Mcp-Session-Id` removed; state goes in explicit handles passed as tool arguments | 2026-07-28 | Nine kinds of per-session state (§3) |
| GET stream and `resources/subscribe` replaced by `subscriptions/listen` (a long-lived POST) | 2026-07-28 | GET SSE stream and fingerprint polling |
| Server-to-client requests (sampling, elicitation, roots) replaced by MRTR: `resultType: "input_required"` plus `inputRequests`, client retries with `inputResponses` | 2026-07-28 | No elicitation today; `confirm: true` parameters stand in |
| `resultType` required on results; `ttlMs`/`cacheScope` required on discover, list and read results | 2026-07-28 | Absent |
| `Mcp-Method`/`Mcp-Name` headers; `MCP-Protocol-Version` must match `_meta` (-32020) | 2026-07-28 | Absent |
| SSE resumability (`Last-Event-ID`) removed; closing a request's stream is cancellation on HTTP | 2026-07-28 | Never implemented; cancellation is `notifications/cancelled` |
| Resource not found is -32602 (was -32002) | 2026-07-28 | Uses -32002 |
| New errors partition: -32000..-32019 legacy, no new allocations | 2026-07-28 | Uses -32000 for timeouts and capacity |
| `tools/list` must not vary per connection or as a side effect of other requests; may vary by authorization; deterministic order | 2026-07-28 | Adaptive packs violate this |
| Tool input validation failures are tool execution errors (`isError`), not protocol errors | 2025-11-25 | Built-ins return -32602 |
| Tool/prompt/resource icons; JSON Schema 2020-12 default dialect; URL-mode elicitation | 2025-11-25 | Absent; schemas are fine |
| Dynamic Client Registration deprecated; CIMD preferred; RFC 9207 `iss`; per-issuer credentials | 2025-11-25 / 2026-07-28 | DCR is the main path; no `iss` |
| Logging, sampling, roots deprecated; per-request `_meta` log level | 2026-07-28 | Never implemented; nothing to remove |
| Extensions framework (`capabilities.extensions`, `{vendor}/{name}` ids) | 2026-07-28 | Absent |
| Skills over MCP (`io.modelcontextprotocol/skills`, beta): `skills/list`, `skills/get`, `skill://` resources with digests | 2026-07-28 only | Skills served as plain resources |
| Tasks (`io.modelcontextprotocol/tasks`): `tools/call` may return a task; `tasks/get`/`update`/`cancel` | 2026-07-28 only | Homegrown operation ledger and status endpoint |

Clients: Claude Code supports skills (public beta, v2.1.331+, at most 100 skills per server,
100 KB per text file and 400 KB per skill), tasks, and form and URL elicitation, all only on
2026-07-28 connections. Claude Code defers MCP tools behind tool search by default, so only tool
names and server instructions load at start; server instructions and up-front tool descriptions
are cut at 4,096 characters. claude.ai connectors do not support resource subscriptions or
sampling. Legacy clients will be around for at least the 12-month deprecation window, so the
server needs both eras for now.

## 2. Protocol and lifecycle

Facts (**verified** where marked):

- `MCP_PROTOCOL_VERSION = "2025-06-18"` (`vulcan-app/src/mcp_protocol.rs:9`, **verified**).
  `initialize` never reads `params.protocolVersion` or client capabilities; it always answers
  `2025-06-18` (`mcp_dispatch.rs:95-98`, `mcp_session.rs:37-49`).
- HTTP rejects any `MCP-Protocol-Version` other than `2025-06-18` with 400, including on
  `initialize` (`vulcan-daemon/src/mcp_http_codec.rs:75-86`, **verified**). A 2025-11-25 client
  that sends the header it negotiated, or a 2026-07-28 client probing, is refused instead of
  negotiated down.
- All method param structs use `#[serde(deny_unknown_fields)]` after stripping `_meta`
  (`mcp_protocol.rs:323-410`, **verified**). Any field a later revision adds to `tools/call` or
  list params (a 2025-11-25 `task` field, for example) is a -32602.
- The batch-specific error branches are dead: the `as_object()` check runs first and returns a
  generic -32600 (`mcp_dispatch.rs:210` vs `:226`, `:286` vs `:294`, **verified**).
- `jsonrpc: "2.0"` is never checked. stdio does not gate on `initialize`; HTTP does, through the
  session header.
- `serverInfo` has no `title`, `icons` or `websiteUrl`. Instructions are 156 to 390 characters,
  well inside client limits, and computed once at `initialize` from that session's active tools.
- `progressToken` is accepted and ignored. No progress, logging or `resources/updated`
  notifications are sent.

Assessment: a server that always answers its own version is technically allowed by 2025-06-18
negotiation, but the header check turns newer clients away instead of letting them fall back,
and the strict param decoding will break on additive changes. The intended direction is
**dual-era**: select legacy semantics when a client opens with `initialize`, serve 2026-07-28
statelessly when requests carry modern `_meta`, and implement `server/discover`.

## 3. Session state, and what replaces it

2026-07-28 has no sessions; the spec's guidance is explicit handles passed as tool arguments. Every
piece of state in `McpSessionState` (`vulcan-app/src/mcp_session.rs:72-90`) and `McpHttpSession`
(`vulcan-daemon/src/mcp_session.rs:57-66`), with its replacement:

| State | Purpose today | Replacement |
|---|---|---|
| `selected_packs` (adaptive mode) | `tool_packs` changes the visible tools mid-session | **Remove adaptive mode** (§5). Packs stay as configuration and grant data, which may legitimately vary the list by authorization |
| `resources` (`McpToolResourceStore`) | Results over 4 KiB stored as `vulcan://tool-results/N`, unbounded `BTreeMap`, cloned per request | **Remove.** Bound results by design and link large content to stable, re-readable resources (§6) |
| `snapshot` (list fingerprints) | Drives `list_changed` after each request and on each 250 ms SSE poll | `subscriptions/listen` with a file watcher on the prompts and skills folders and the trust store; keep fingerprinting only for legacy sessions |
| `written_paths` | `git_commit scope=session` commits what this session wrote | Changesets are already explicit handles. Commit by `changeset` or `paths`; drop `scope=session` on modern connections |
| `changeset` binding, `parked_written_paths` | `changeset_switch` re-points the session's vault paths | Pass `changeset` as an optional argument on write and read tools; drop `changeset_switch` |
| `client` (name, version, OAuth subject) | Commit trailers | Per-request `_meta` client info plus the token's client id |
| `selection`/`guard`, "never widen after init" | Profile ceiling | Per-request resolution from the token or grant; named remotes already re-resolve the grant on every request |
| HTTP `authority` (credential fingerprint) | Binds a session to one token; a refreshed token gets **403**, not 404, so clients that re-initialize only on 404 do not recover | Gone with sessions |
| `active_requests`, SSE `subscribers`, idle deadline, 256-session registry | Cancellation, push, expiry | Request-scoped cancellation (stream close); `subscriptions/listen` streams |

None of this needs to vanish for legacy clients at once: the legacy path can keep a session
wrapper around the same stateless core.

## 4. Transports

### Streamable HTTP

Facts:

- Hand-written HTTP/1.1 over `std::net::TcpListener`: one thread per connection, every response
  `Connection: close`, no keep-alive, chunked requests refused, 5 s read deadline, 5 s write
  timeout that also applies to SSE writes, 64 connections, 1 MiB bodies
  (`vulcan-daemon/src/mcp_transport.rs`, `mcp_http_codec.rs`). The daemon's REST API uses axum
  (`vulcan-daemon/src/http.rs`, `rest.rs`).
- POST responses are always a single `application/json` body, never SSE, so no progress can be
  streamed.
- Requests in one session are serialized by the core mutex (`mcp_http_driver.rs:247-258`), and
  the GET SSE stream polls under the same mutex, so it stalls with no keepalives for the duration
  of any long request on that session. `list_changed` raised by a POST is lost if no GET stream
  is open, because the snapshot has already advanced.
- `Origin` is validated only when present, and only localhost or the bind IP are accepted
  (`vulcan-daemon/src/http_policy.rs:73-97`; the port is deliberately not compared, for local
  harnesses). **No `Host` header is validated anywhere.** Behind the usual HTTPS reverse proxy,
  server-side clients (claude.ai, ChatGPT) send no `Origin` and pass; a browser-based client on
  the public domain would be refused, because the public URL's origin is not on the list.
- Transport-level failures all map to -32600 with id null; client notifications the server does
  not know (`notifications/roots/list_changed`, `notifications/progress`) get HTTP 400.
- `McpServerCore` in `vulcan-cli/src/mcp.rs:1280-1401` duplicates the `McpRequestCore` impl in
  `mcp_execution.rs:38-57`, and `mcp.rs` carries 63 `#[cfg(test…)]` forwarding shims.

Assessment: the hand-rolled codec was reasonable when MCP HTTP was "one POST, one JSON reply". The
2026-07-28 transport needs per-request SSE (progress, MRTR, task notifications), long-lived
`subscriptions/listen` streams with keepalives, header routing (`Mcp-Method`, `Mcp-Name`), and
request-stream close as cancellation. Building that on a `Connection: close` thread-per-socket
codec is the wrong investment. **Move the MCP endpoint onto axum** in `vulcan-daemon`, mounted
both by the resident daemon and by foreground `vulcan mcp --transport http` (the CLI already
depends on tokio; the core stays synchronous behind `spawn_blocking`, as the constraints
require). Validate `Host` and accept as `Origin` either a loopback origin or the configured public
URL's origin, so the proxy deployment and DNS-rebinding protection both hold. With no sessions, per-session
serialization disappears and the existing per-vault mutation scheduler remains the
write-ordering point.

### stdio

Facts: strictly serial. Each line runs to completion before the next is read, on a fresh
16 MiB-stack worker over a clone of the whole core (`vulcan-cli/src/mcp.rs:796-831`, **verified**;
`mcp_execution.rs:238-285`). `notifications/cancelled` is therefore a no-op: it can only arrive
after its target has finished.

**Bug:** on timeout, stdio returns a timeout error and discards the clone, but the worker thread
keeps running with no cancellation token (`mcp_execution.rs:259` passes `None`). A write that
the client was told timed out can still land afterwards, and its session-state changes are lost.
HTTP named remotes have the indeterminate-outcome and operation-status machinery for this; stdio
has nothing.

Fix: a reader thread that dispatches requests to workers (reads run in parallel, writes go through
the vault write gate), honors `notifications/cancelled`, and either passes a cancellation token
into timed-out workers or reports a write timeout as indeterminate with an operation id.

## 5. Tool catalog

Measured (`tools/list` on the fixture): 8 tools and 11.2 KB by default; 49 tools and 43.2 KB with
all packs (about 10.8k tokens). Descriptions total only 5.2 KB; input schemas take 23.9 KB, mostly
per-parameter descriptions. Descriptions are short and imperative, which is good.

### Tool packs and adaptive mode: outdated

Packs were built to keep context small on clients that load every tool up front. Since then:

- Claude Code defers MCP tools behind tool search by default; the platform tool search tool does
  the same for API clients. Only names load at start.
- 2026-07-28: `tools/list` "MUST NOT vary per-connection or as a side effect of other requests".
  Adaptive mode is exactly that.
- Legacy clients that load everything up front still exist, and 49 tools is above the 30 to 50
  where selection accuracy degrades, so the catalog should also shrink (below).

Recommendation: **remove adaptive mode, the `tool_packs` tool and its hidden `tool_pack_*`
aliases**, and expose the whole consolidated catalog by default. §5a sets out how other servers
and clients handle this and why. Move the non-standard top-level `toolPacks` field on each tool
into `_meta`.

### Consolidation

| Today | Proposal |
|---|---|
| `sync_status` and `sync_plan` run the same function, `sync_preview` (`mcp_tool_exec.rs:149`, **verified**) | Keep one |
| `daily`, `daily_show`, `daily_list` (the latter "legacy MCP pagination contract", `mcp_read_tools.rs:258`) | Keep `daily` |
| `task_list` (whose `filter` already accepts the Tasks DSL) and `task_query` | One `task_query`/`task_list` with `query`, structured filters and paging |
| `capabilities`, `status`, `tool_packs list`; the active-tool list appears three times in `capabilities`, which spills to a resource link even by default | Fold what an agent needs into `status`; the rest belongs in `server/discover` and instructions |
| `vulcan://assistant/skill-commands/index` and `…/tools/index` | One index, or none once skills are served natively |

That takes the catalog from 49 to about 43 without losing capability. Further merges (the
`note_info`/`note_outline`/`note_get` cluster) are worth evaluating with real agent transcripts
rather than by inspection.

### Consistency and annotations

- Annotations: `note_create` and `note_append` are marked destructive (they add content);
  `git_commit`, `changeset_land` and `index_scan` are not; `task_list`/`task_query` are read-only
  but non-idempotent; `task_reminder_add` has every hint false. Claude Code parallelizes on
  `readOnlyHint` and retries on `idempotentHint`, and connector review requires accurate hints, so
  these have behavioral effect.
- Parameter names: the note is `note` in most tools but `path` in `note_create`, `file_get`,
  `file_put`; mutation safety is spread over `dry_run`, `check`, `confirm`, `no_commit`, and
  `note_create`/`note_set` have no `dry_run` (the project convention says every mutation does);
  sorting is `desc`, `order`, `sort` or `sort_by` depending on the tool.
- Output keys mix snake_case and camelCase (`capabilities`, `tool_packs`, task rows).
- Tool names mix `noun_verb` with bare names (`search`, `query`, `status`, `daily`) and one
  verb-first name (`suggest_links`). Tool search and namespacing guidance favor a consistent
  prefix scheme; not urgent.
- Custom skill-command tools take the skill's description rather than the command's, and send
  `"outputSchema": null` (**verified**), which a strict client rejects. Omit it.

## 5a. Progressive disclosure: what others do

Two further research passes covered how prominent servers and the main clients handle large
tool sets as of 2026-11 (sources at the end).

### Servers

| Pattern | Who | 2026-07-28 | Without client tool search |
|---|---|---|---|
| Static groups chosen by config, URL path, header or credential scope | GitHub (`--toolsets`, `/x/{toolset}`, `/readonly`, scope filtering), Supabase (`--features`), Grafana, Playwright (`--caps`), Notion (admin picks per connection) | Allowed: a separate endpoint, or variation by authorization | Every enabled tool costs context on every turn |
| Meta tool enables a group, then `list_changed` | GitHub (**removed** in PR #2512, 2026-05: local-only, complex, unused), Pipedream full-config (dropped in `/v3`), Vulcan adaptive mode | **Forbidden**: the list varies as a side effect of a request | Works only if the client refreshes, and most do not |
| Fixed gateway: `discover`/`search`, `describe`, `execute_read`/`execute_write` | Atlassian Rovo v2, Stripe `stripe_api_*`, AWS `call_aws`, Azure `--mode single`, Composio, Zapier | Allowed | Cheapest up front; an extra round trip per action; hides per-tool annotations and schemas |
| Code mode: one or two tools that run model-written code against a typed API | Cloudflare (two tools over 2,500+ endpoints, about 1k tokens against 1.17M) | Allowed | Cheap; needs a sandbox; approval becomes "run this code?" |
| Consolidation into workflow-shaped tools | GitHub Projects (merged into three tools, about 50% fewer tokens), Azure `consolidated` mode, Anthropic guidance | Allowed | Fewer, larger tools |
| Skills as the workflow layer | Shopify, Stripe, HubSpot, Zapier; MCP skills extension | Allowed | Ignored if unsupported |

The gateway and code-mode servers are all API surfaces with hundreds or thousands of operations.
None is a 45-tool server.

### Clients

| Client | Defers tools behind search? | Hard cap | Honors `tools/list_changed`? | Protocol |
|---|---|---|---|---|
| Claude Code | Yes, by default; `_meta["anthropic/alwaysLoad"]` exempts a tool | None | Yes | 2026-07-28 (v2 runtime) |
| claude.ai, Desktop, mobile | Yes: per-chat "Tool access" Auto (default) / On demand / Always | None published | Unverified | 2026-07-28 rolling out |
| Codex CLI, Copilot CLI, Cursor | Yes (Copilot CLI from about 30 tools) | Cursor's old 40 appears lifted (unverified) | Codex: off by default; Cursor: no | 2025-11-25 or unpublished |
| OpenAI Responses / Agents API | Opt-in `tool_search`, gpt-5.4+; Agents API automatic | None | n/a | Unpublished |
| ChatGPT (developer mode, apps) | **No** documented deferral | None documented | **No**; tool updates need a manual refresh | 2025-11-25 in practice |
| VS Code chat | "Virtual tools" grouping past a threshold | 128 per request | Yes | 2025-11-25 |
| Windsurf/Devin, Gemini CLI, Goose, local-model hosts | No | Windsurf 100; Goose reportedly best under 25 | Gemini CLI yes; others unverified | Mixed |

Two client behaviors make per-tool annotations load-bearing: ChatGPT treats every tool without
`readOnlyHint` as a write that needs confirmation, and Claude's connector review rejects tools
that mix reads and writes and requires a `title` plus a read-only or destructive hint on every tool.
Most clients also let users switch individual tools off: Claude per tool or group, ChatGPT in
developer mode, Codex `enabled_tools`, Gemini `includeTools`, the VS Code picker.

Evidence on count: Anthropic's tool-search docs say selection degrades past 30 to 50 tools; tool
search raised MCP-eval accuracy from 49% to 74% (Opus 4) while cutting definition tokens by 85%;
GitHub Copilot gained 2 to 5 points going from 40 to 13 core tools. No published benchmark
compares a server-side gateway against client-side tool search.

### Recommendation for Vulcan

1. **One flat, stable list per endpoint: the consolidated catalog, filtered by the permission
   profile and grant.** About 43 tools after consolidation. The clients that matter most (Claude
   Code, claude.ai, Codex, Copilot CLI, Cursor) defer and search, so the count costs little
   there; it is under every hard cap; and one tool per action keeps the annotations that drive
   ChatGPT confirmations and Claude permissions. Return tools in a deterministic order.
2. **Packs stay, as authorization and configuration, never as runtime state.** A named remote's
   consent already picks packs into the durable grant, which is exactly the "vary by
   authorization" the spec allows. For stdio and direct HTTP, `--tool-pack` stays as a static
   selection, with its default changed to everything the profile allows. A small set for small
   local models or a read-only connector is a second named remote with fewer packs or a
   narrower profile, which works already and needs no new mechanism.
3. **Help deferral do its job.** Mark three to five core tools with
   `_meta["anthropic/alwaysLoad"]` (candidates: `search`, `query`, `note_get`, `note_outline`,
   `note_append`). Put search terms at the start of each description. Rewrite the server
   instructions as a short map of tool families and when to search for each, computed from the
   whole visible catalog rather than from the "active" packs: the first 512 characters
   self-contained for ChatGPT, the whole under 4,096 for Claude Code.
4. **No gateway and no code mode.** Both answer a scale problem Vulcan does not have, and a
   gateway would hide the per-tool annotations that ChatGPT and Claude rely on. The one place it
   could become relevant is vault skill commands, the only set that can grow without bound;
   revisit if a vault ever exposes dozens of them.
5. **Workflows go in skills**, not in more tools or longer descriptions (§7).
6. **Watch** the Progressive Disclosure working group (renamed from Primitive Grouping in
   2026-09; only a proposed grouping convention so far) and `experimental-ext-variants`
   (SEP-2053, a server offering variants of itself). A standard for server-declared groups would
   be the legitimate successor to tool packs; nothing is ready.

## 6. Results and errors

### Result shape

Facts (`McpToolResourceStore::success_response`, `mcp_protocol.rs:98-115`, and
`tool_success_content`, `:209-222`, **verified**):

- Up to 4 KiB: `content` is the **pretty-printed** JSON and `structuredContent` the same object,
  so the payload goes over twice (pretty text is 1.1 to 1.56 times the compact size).
- 4 KiB to 64 KiB: a one-line summary plus a `resource_link` to the session store, **and** full
  `structuredContent` anyway, so the link saves nothing.
- Over 64 KiB: summary and link only, **no `structuredContent`**, which violates the rule that a
  tool with an `outputSchema` must return conforming structured content.
- 34 of 49 tools declare the generic `{"type":"object","additionalProperties":true}` output
  schema, which carries no information.

Assessment and direction:

- Text content: compact JSON (the spec's compatibility SHOULD), or a short human rendering for the
  few tools where that reads better. Never pretty-printed.
- Always return `structuredContent` when an output schema is declared; type the schemas that are
  generic or drop them.
- Bound results at the source instead of spilling: concise defaults, an optional
  `response_format: "concise" | "detailed"` on the heavy readers (`search`, task listing,
  `git_blame`, `sync_status`), and paging everywhere. Search hits are about 320 bytes and task rows
  about 470 bytes with duplicated fields (`link`/`section`, `text`/`visual`,
  `checked`/`completed`/`fullyCompleted`); `note_get` echoes every request selector, mostly null.
- For genuinely large content (a 119 KB note), link to a **stable resource** rather than a
  session-scoped one: expose notes and files as resource templates (`vulcan://note/{path}`,
  `vulcan://file/{path}`, with the existing completion for `{path}`). That also gives Claude Code
  `@vulcan:` mentions of vault notes, which do not exist today.
- Claude Code saves text results over 50,000 characters to a file and warns above 10k tokens;
  `_meta["anthropic/maxResultSizeChars"]` raises the limit per tool where a large result is the
  point.

### Paging and limits

`search` has a default of 20 but no maximum (`limit: 1000` accepted), no offset and no `has_more`.
`task_list` has no limit at all. `git_log` clamps silently to 200. `query` and `daily` page well
(`page{has_more, next_offset, total_count…}`); use that shape everywhere.

### Errors

- Bad arguments, unknown arguments, wrong types, missing required fields and domain validation
  (`daily.operation=explode`, `query.limit` over 200, missing `confirm`) are JSON-RPC -32602.
  2025-11-25 moved input validation to tool execution errors so the model sees and corrects them.
  Return `isError: true` with a message that names the parameter (raw serde text such as
  "invalid type: integer `5`, expected a string" does not). Custom tools already do this, so
  built-ins and custom tools disagree today.
- Messages that point at the CLI: "cache is missing; run `vulcan scan`" should name `index_scan`
  (or the server should scan on first use); `status` says "before repairing indexes". `web_fetch`
  on a bad URL returns "builder error". Query DSL errors should point at `vulcan://help/query-dsl`.
- Help resources document CLI flags (`--mode`, `--section`) rather than MCP argument names, and
  are Markdown wrapped in a JSON string with mimeType `application/json`; serve `text/markdown`
  written for the MCP argument names. `describe --format mcp` examples are CLI lines too.
- Use -32602 for resource-not-found on modern connections, accept that legacy clients may see
  -32002, and stop emitting -32000 for new conditions.

## 7. Resources, prompts and skills

- Resources are all `vulcan://help/*` and `vulcan://assistant/*`; vault content is not exposed as
  resources (see §6). `vulcan://assistant/config` includes absolute filesystem paths, which a
  remote client has no use for.
- Prompts come from `AI/Prompts`; argument entries carry a non-standard `completion` field (the
  completion endpoint works, so drop the field).
- **Skills** are served as resources from the vault's `.agents/skills` only, so bundled skills
  reach an MCP client only after `vulcan agent install` in that vault, and a host treats them as
  resources the model may or may not read, not as skills.

Direction:

- On 2026-07-28 connections, implement `io.modelcontextprotocol/skills`: `skills/list` and
  `skills/get` over `skill://<name>/…` with sha256 digests and sizes; serve bundled skills
  straight from the binary, vault skills overriding by name; filter by permission profile and by
  which tools the skill's `tools:` frontmatter needs (a skill whose tools are not available under
  this grant is noise). Keep the resource form for legacy clients until the deprecation window
  closes. Bundled skills total about 258 KB over 27 skills, inside Claude Code's limits.
- **Make the skills transport-aware.** Their bodies teach CLI invocations. Either write each
  bundled skill in terms of operations with both forms, or render the MCP variant at serve time
  (the `tools:` list already names the MCP tools). This matters more than the transport work: a
  skill that teaches `note get --section` to an agent with only `note_get` makes it worse.
- Skill commands remain custom tools; give each tool the command's description.

## 8. Long-running operations and confirmations

- **Tasks.** Named remotes already have most of a task system: mutations are durably registered
  before dispatch, carry an `operation_id`, and an indeterminate outcome returns
  `status_path` and `retry: "check_operation_status_first"` against a non-standard
  `{endpoint}/operations/{id}` route (`mcp_hosted.rs`, `mcp_execution.rs:166-192`). On 2026-07-28
  connections that declare the extension, return `resultType: "task"` from `tools/call` for
  `index_scan`, `sync_refresh`, `changeset_land`, `web_fetch` and long queries, backed by the same
  operation ledger, with `tasks/get`/`tasks/cancel`. Claude Code moves a call still running after
  two minutes to the background anyway; a task makes that explicit and survives reconnects. Keep
  the operation-status route for legacy clients.
- **Elicitation via MRTR.** `note_set` and `note_delete` require `confirm: true`, which the agent
  sets itself, so it confirms nothing to the human. On modern clients that declare elicitation,
  return `input_required` with a form request for destructive operations (delete, overwrite,
  `changeset_abandon`, `config_set`); fall back to the parameter on legacy clients and on
  claude.ai connectors (form mode only). Permission profiles remain the actual boundary.
- **Progress.** Once POST responses can stream, report progress for `index_scan` and sync.

## 9. Authorization

Four modes (`vulcan-cli/src/mcp.rs:1409-1568`): none (loopback), a static bearer token, an
external resource server that validates an upstream IdP's JWTs, and a local authorization server
used either directly (static client and/or DCR, approval token or IndieAuth) or by named remotes
(DCR or CIMD, IndieAuth sign-in, consent page, durable grants, refresh tokens). About 7,500
non-test lines.

### Fix (security and conformance)

1. **Approval token travels in a GET URL.** The direct local mode's approval form submits by GET,
   so the token lands in browser history and any access log (`mcp_oauth_authorize.rs:89-124`,
   `:301-330`). POST it, or remove the mode (below).
2. **Non-constant-time secret comparison** for the static client secret, DCR client secrets and
   the approval token (`vulcan-core/src/oauth.rs:489-496`, `mcp_oauth_policy.rs:210-216`).
3. **CIMD fetch hardening.** No caching: the document is fetched on every `/authorize` and every
   `/token` (`mcp_oauth_authorize.rs:219-232`, `mcp_oauth_token.rs:194-199`). A response without
   `Content-Length` is read in full before the 64 KiB cap applies. The non-public address filter
   (`oauth.rs:166-188`) misses IPv4-mapped and IPv4-compatible IPv6, NAT64, 6to4, 198.18.0.0/15
   and 240.0.0.0/4. Stream with a hard cap, cache by HTTP caching headers with a bounded TTL, and
   normalize mapped addresses before the check.
4. **External mode is a metadata shim, not a resource server pointer.** Protected Resource
   Metadata names the MCP server itself as the authorization server and serves a copy of the
   upstream discovery document with `issuer` rewritten, while tokens carry the upstream `iss`
   (`oauth.rs:254-264`, `:1052-1069`). Spec-conformant: PRM lists the real issuer in
   `authorization_servers`, and the MCP server serves no AS metadata. JWKS is fetched once and
   never refreshed, so upstream key rotation needs a restart; refresh on unknown `kid` with a rate
   limit.
5. **Challenges.** A missing token gets `error="invalid_token"` (RFC 6750 says omit the error);
   401 carries no `scope` hint, which the spec says challenges should include. Add RFC 9207 `iss`
   to authorization responses.
6. `scopes_supported` advertises `openid`, `email`, `profile`, which do nothing (no ID token, no
   userinfo); the spec also advises against advertising every scope. Drop them.

### Outdated or over-built

- **DCR is deprecated** (2026-07-28). CIMD already works and is accepted even with DCR off. Keep
  DCR for clients that need it (Claude's connector docs still list `oauth_dcr`, and the
  ChatGPT path used it), but make CIMD the documented path.
- **The direct local authorization server mode** (static client, approval token, multi-user
  `--oauth-local-user`, no refresh tokens, unrevocable stateless tokens) overlaps named remotes,
  which do the same job properly. The named-remote static client has no redirect URIs, so it is
  a dead path. Recommendation: retire direct local OAuth and keep three modes: none (loopback),
  static bearer (scripts and automation), and named remotes. Keep external RS mode only if it is
  used; otherwise retire it too.
- **Scopes are protocol namespaces** (`mcp:tools`, `mcp:resources`, `mcp:prompts`) checked by
  method prefix; real authority is the profile, packs and grant. Namespace scopes give step-up
  nothing to work with. If incremental consent is wanted, scopes along the axis people actually
  grant (`vault:read`, `vault:write`, `vault:git`) would map onto profiles; otherwise leave them
  and do not invest in step-up.
- Session-authority binding goes away with sessions (§3).

### Keep

PKCE S256 only; exact redirect matching with the loopback/app-scheme policy; CIMD DNS pinning, no
redirects and size bounds; rotating refresh families with replay revocation; per-request durable
grant re-resolution, which makes revocation immediate; consent page CSRF, CSP and frame
protection; monotonic grant attenuation; no token passthrough anywhere.

## 10. Keep, change, remove

| Area | Decision |
|---|---|
| Curated catalog, annotations, schemas, profile filtering, `additionalProperties: false` | Keep; fix annotations and naming |
| `expected_hash`, `idempotency_key`, hosted mutation scheduler, operation ledger | Keep; the ledger backs Tasks |
| Changesets | Keep; become the explicit state handle |
| Hand-written HTTP/1.1 codec, transport, SSE poller | Replace with axum |
| `initialize`-only lifecycle, single protocol version | Dual-era with `server/discover` |
| Adaptive packs, `tool_packs`, `tool_pack_*` aliases | Remove |
| Session result store, pretty-text duplicate, 4 KiB/64 KiB spill | Remove; bounded results plus stable resources |
| `sync_plan`, `daily_show`, `daily_list`, `capabilities` | Remove or fold |
| `git_commit scope=session`, `changeset_switch` | Replace with explicit `changeset`/`paths` arguments |
| `confirm: true` | Keep as legacy fallback; MRTR elicitation on modern clients |
| `{endpoint}/operations/{id}` | Keep for legacy; Tasks on modern |
| Skills as resources | Keep for legacy; skills extension on modern; transport-aware bodies |
| Direct local OAuth mode, `openid`/`email`/`profile` scopes, dead static client | Remove |
| External RS mode | Fix (real PRM, JWKS refresh) or remove if unused |
| Dead batch branches, `McpServerCore` duplication, CLI test shims | Remove |

## 11. Suggested order

1. **Fixes that stand alone** (small, no design change): stdio orphaned-write timeout; approval
   token by POST; constant-time compares; CIMD streaming cap, cache and address normalization;
   `Host` validation and the public URL's origin in the `Origin` check; `structuredContent` always present; compact text
   instead of pretty; `outputSchema: null`; annotation corrections; input validation as `isError`
   with parameter names; error messages that name MCP tools.
2. **Catalog cleanup and disclosure** (§5a): remove adaptive packs and aliases, default to the
   whole profile-filtered catalog, fold the duplicate tools, `alwaysLoad` on the core tools,
   instructions as a tool-family map, make paging and parameter names consistent, add
   `response_format` and slimmer rows, expose notes and files as resource templates. Update
   `mcp-setup` and the AGENTS template.
3. **Transport-aware skills**: rewrite or render bundled skill bodies for MCP tool names. Useful
   immediately through the existing resources and through `vulcan agent install`.
4. **axum transport**: move the HTTP endpoint onto axum in `vulcan-daemon` for both hosts, with
   per-request SSE responses; keep the legacy session wrapper.
5. **Dual-era protocol**: `server/discover`, per-request `_meta`, `resultType`, `ttlMs`/
   `cacheScope`, `Mcp-Method`/`Mcp-Name`, version errors, -32602 not found, stateless core with
   changesets as handles, `subscriptions/listen`.
6. **Extensions on modern connections**: skills (bundled and vault, digests, profile filtering),
   then tasks over the operation ledger, then MRTR elicitation for destructive tools, then
   progress.
7. **Auth consolidation**: retire direct local OAuth; fix or retire external mode; CIMD as the
   documented registration path; RFC 9207 `iss`.

Steps 1 to 3 are worth doing even if the protocol upgrade waits; 4 is the prerequisite for 5 and
6 over HTTP, while stdio can take 5 and 6 first.

## 12. Decisions and remaining questions

Answered by Eric (2026-11-25):

- **Clients:** mostly claude.ai and others; ChatGPT remains a target. DCR and the CIMD
  auth-method negotiation stay; CIMD becomes the documented path alongside them.
- **Deployment:** most use OAuth through IndieAuth behind an HTTPS proxy, that is, named
  remotes. That path is the one to optimize; the proxy shapes the `Host`/`Origin` fix (§4).
- **Default exposure:** settled by §5a: the whole profile- and grant-filtered catalog, packs as
  authorization and configuration only.
- **Skills over MCP:** whenever it fits best. It needs the 2026-07-28 protocol in Claude Code, so
  it lands in step 6, after the transport-aware skill rewrite (step 3) has proved the content
  through the existing resources and `vulcan agent install`.

Still open:

- Does any device use direct local OAuth (approval token or static client, not a named remote)
  or external RS mode? "Most" use named remotes; retirement of the other two needs a yes or no.
- ChatGPT reportedly gives Plus and Pro accounts read-only connectors; check whether that keys on
  `readOnlyHint` alone or on specific tool names before relying on it.

## Sources

- MCP 2026-07-28: <https://modelcontextprotocol.io/specification/2026-07-28> and its changelog,
  versioning, Streamable HTTP, stdio, authorization (discovery, client registration, security
  considerations), MRTR, subscriptions, tools, resources, deprecated features; 2025-11-25
  changelog; security best practices.
- Extensions: <https://modelcontextprotocol.io/extensions/overview>,
  <https://modelcontextprotocol.io/extensions/skills/overview> and SEP-2640,
  <https://github.com/modelcontextprotocol/ext-skills>,
  <https://github.com/modelcontextprotocol/ext-tasks>,
  <https://github.com/modelcontextprotocol/ext-apps>.
- Claude: <https://claude.com/resources/articles/skills-over-mcp-tasks-and-more>,
  <https://claude.com/resources/articles/bringing-mcp-2026-07-28-to-claude>,
  <https://code.claude.com/docs/en/mcp>, <https://claude.com/docs/connectors/building> (review
  criteria, authentication),
  <https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool>.
- Anthropic engineering: "Writing effective tools for agents" (2025-09), "Code execution with
  MCP" (2025-11), "Advanced tool use" (2025-11).
- Progressive disclosure, servers: github/github-mcp-server (`docs/remote-server.md`, discussion
  #1182, PR #2512, changelog 2026-01-28 and 2026-07-23); <https://blog.cloudflare.com/code-mode/>,
  <https://blog.cloudflare.com/code-mode-mcp/>;
  <https://developer.atlassian.com/cloud/rovo-mcp/guides/supported-tools/>;
  <https://docs.stripe.com/mcp>; <https://supabase.com/docs/guides/ai-tools/mcp>;
  <https://github.com/getsentry/sentry-mcp>;
  <https://learn.microsoft.com/azure/developer/azure-mcp-server/concepts>;
  <https://docs.zapier.com/mcp/overview/how-tools-work>;
  <https://docs.composio.dev/toolkits/meta-tools>;
  <https://modelcontextprotocol.io/community/working-groups/progressive-disclosure>.
- Progressive disclosure, clients:
  <https://support.claude.com/en/articles/13730515-manage-claude-s-tool-access>,
  <https://claude.com/docs/connectors/building/review-criteria>,
  <https://developers.openai.com/api/docs/guides/developer-mode>,
  <https://developers.openai.com/apps-sdk/changelog/>,
  <https://developers.openai.com/api/docs/guides/tools-tool-search>,
  <https://docs.github.com/en/copilot/concepts/agents/copilot-cli/tool-search>,
  <https://code.visualstudio.com/docs/agents/run/tools>,
  <https://cursor.com/blog/dynamic-context-discovery>,
  <https://geminicli.com/docs/tools/mcp-server/>,
  <https://modelcontextprotocol.io/extensions/client-matrix>; third-party compatibility notes at
  <https://zuplo.com/learn/mcp/compatibility/clients/>.
- Evidence: RAG-MCP (arXiv 2505.03275), LongFuncEval (arXiv 2505.10570), "How we're making GitHub
  Copilot smarter with fewer tools" (2025-11).

Caveats: the skills spec's tail and Claude Code's tasks section were read partly through
summaries; the absence of a pre-2026-07-28 path for skills and tasks is inferred from the specs
not offering one, consistent with Claude Code's documented requirement of 2026-07-28. Token
figures are bytes divided by four.
