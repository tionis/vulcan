# Connecting MCP Clients

Vulcan serves one vault to MCP clients (Claude Code, claude.ai, ChatGPT, Claude Desktop, MCP
Inspector and others) in one of three ways:

| Way | Use it for | Authentication |
| --- | --- | --- |
| stdio (`vulcan mcp`) | a client on the same machine that starts Vulcan itself | none: the client owns the process |
| local HTTP (`vulcan mcp --transport http`) | a client on this machine or network that connects to a running server | loopback, or a static bearer token |
| named remote (`vulcan mcp remote`) | hosted clients over HTTPS: claude.ai, ChatGPT, n8n | IndieAuth sign-in and Vulcan's consent page (OAuth) |

In each case the connection acts under a permission profile, which decides both which tools the
client sees and what they may do. Pick the narrowest profile that does the job: `readonly`,
`researcher`, `daily-wiki-agent`, `writer`, `maintainer` (see `vulcan help sandbox`).

## Claude Code on this machine (stdio)

```sh
claude mcp add vulcan -- vulcan --vault ~/notes mcp --permissions writer
```

Claude Code starts `vulcan mcp` when a session begins and talks to it over stdin and stdout.
Nothing listens on the network. The same command line works in any client that launches stdio
servers; in a JSON configuration it is
`{"command": "vulcan", "args": ["--vault", "/home/me/notes", "mcp", "--permissions", "writer"]}`.

`--request-timeout` (default 2 minutes) bounds how long a tool call may run before the client is
told its outcome is not known yet; see [Long-running calls](#long-running-calls).

## Local HTTP with a static token

```sh
vulcan --vault ~/notes mcp --transport http --bind 127.0.0.1:8765 --auth-token "$TOKEN" --permissions writer
claude mcp add --transport http vulcan http://127.0.0.1:8765/mcp --header "Authorization: Bearer $TOKEN"
```

The token is sent only as `Authorization: Bearer` (the old `X-Vulcan-Token` header is not read for
MCP) and compared in constant time. Without `--auth-token` the listener accepts only loopback
clients. Binding beyond loopback needs a token. Do not expose a static-token listener to the
internet: hosted clients go through a named remote instead.

## claude.ai, ChatGPT and other hosted clients (named remote)

A hosted client reaches your machine over HTTPS through a reverse proxy, and you sign it in with
your IndieAuth identity. Set up the remote once (see `vulcan help mcp-remotes`):

```sh
vulcan vault add personal ~/notes
vulcan mcp remote init personal --wiki personal \
  --public-url https://wiki.example.com/mcp --identity https://example.com/ \
  --ceiling-profile writer --default-profile readonly
vulcan mcp remote run personal        # or: vulcan daemon start --detach
```

Then add the connector in the client with the public MCP URL, path included
(`https://wiki.example.com/mcp`):

- **claude.ai:** Settings, then Connectors, then Add custom connector. Paste the URL and connect.
  claude.ai registers itself, sends you to your IndieAuth provider, then to Vulcan's consent page,
  where you pick the vault, the permission profile (up to the ceiling) and how long the
  connection lasts.
- **ChatGPT:** Developer Mode, see `vulcan help chatgpt-mcp`.
- **Claude Code against the remote:** `claude mcp add --transport http vulcan https://wiki.example.com/mcp`,
  then `/mcp` in a session to sign in.

`vulcan mcp connections list` shows each approved connection, and `revoke` ends one at once.

## What a connection sees

- **Tools:** every tool the profile allows, in a stable order, and nothing else. A missing tool
  means the profile does not grant it. The core reading tools (`note_get`, `note_outline`,
  `search`, `query`) load up front in Claude Code; the rest load when needed.
- **Resources:** `vulcan://note/{path}` and `vulcan://file/{path}` (with path completion),
  `vulcan://help/tools` (the caller's tools and their arguments), `vulcan://agents` (the vault's
  `AGENTS.md`) and the skills.
- **Skills:** Vulcan's workflow skills, written for MCP tool names, and the vault's own
  `.agents/skills`, a vault skill replacing a bundled one of the same name. A skill is offered
  only when the connection may call a tool it names. Clients that speak MCP 2026-07-28 and support
  skills (Claude Code in an interactive session, for example) load them as skills; others find them
  as `skill://` resources.
- **Prompts:** the vault's prompt files in `assistant.prompts_folder`.
- **List changes:** when tools, prompts or resources change, the client is told (2025 clients on
  their session's event stream, 2026-07-28 clients through `subscriptions/listen`).

Vulcan speaks MCP 2025-06-18 and 2025-11-25 through `initialize`, and 2026-07-28 without a
handshake or session. Clients choose; nothing needs configuring.

## Long-running calls

- **Progress:** a client that sends a progress token on a call (Claude Code does) gets progress
  for `index_scan` (files scanned, then the finishing phases) and a start notice for
  `sync_refresh` and `changeset_land`.
- **Tasks:** a 2026-07-28 client that supports tasks gets `index_scan`, `sync_refresh`,
  `changeset_land`, `web_fetch`, `query` and `search` back as a task when they run longer than 5
  seconds, and follows the task to its result instead of waiting.
- **Timeouts:** otherwise a call still running at `--request-timeout` keeps running, and the
  client gets an `indeterminate` error with an `operation_id`. `operation_get` with that id says
  whether it is still running, succeeded (with the result) or failed. Check it before repeating a
  write. On a named remote, writes are also kept in a durable ledger for 30 days (unknown outcomes
  for 90) and can be checked at their `status_path`.

## When a note name is ambiguous

A note name that matches several notes (`Topic` for both `Archive/Topic.md` and
`Projects/Topic.md`) is an `ambiguous` error listing the matches. A 2026-07-28 client that can
show forms asks you which note you meant instead, and continues with your answer.

## Confirming overwrites and deletes

Clients usually ask before a tool marked destructive runs. To have Vulcan ask too, set
`confirm_destructive = true` under `[mcp]` in the vault's `.vulcan/config.toml`. A 2026-07-28
client that can show forms (claude.ai does) then asks you to allow each overwrite or delete before
it happens. Clients that cannot ask are refused those calls, and dry runs always go through.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| 401 with `WWW-Authenticate: Bearer ... resource_metadata=` | the client has no valid token; for a named remote it should start the OAuth flow |
| 403 `MCP session authority does not match` | a 2025 session was used with a token for another connection; reconnect |
| 421 `this listener does not serve the host` | the proxy passes a host name the listener does not know; pass the public host or rewrite `Host` to the listener's address |
| 404 at the bare origin | the URL lacks the MCP path; use `https://host/mcp` |
| -32020 | a 2026-07-28 request's `MCP-Protocol-Version`, `Mcp-Method` or `Mcp-Name` header contradicts its body |
| -32022 | the client asked for a protocol version Vulcan does not speak; the error lists the supported ones |
| a tool is missing | the connection's profile does not grant it; approve the connection again with a wider profile, within the ceiling |
| `--oauth-*` or `--public-url` on `vulcan mcp` is refused | direct OAuth was removed; use `vulcan mcp remote init` |
| `--tool-pack` is refused | tool packs were removed; choose a profile instead |

`vulcan --permissions <profile> describe --format mcp` prints the tools a profile would see,
without starting a server.
