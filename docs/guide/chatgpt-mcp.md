# ChatGPT Plugin / MCP Wiki Setup

Use this setup for a private ChatGPT Developer Mode plugin connection to a personal wiki. Developer Mode availability depends on the account and workspace policy.

## Recommended Server Shape

Do not expose `vulcan mcp --auth-token` directly to the public internet. For ChatGPT, initialize a
named remote. It keeps Vulcan on loopback, authenticates the approving person with IndieAuth, and
records the authority selected on Vulcan's consent page as a revocable device-local connection.
The HTTPS reverse proxy is transport, not the authorization boundary.

Recommended setup:

1. Register the vault once, then preview and initialize the named remote:

   ```sh
   vulcan vault add personal /path/to/vault
   vulcan --vault /path/to/vault mcp remote init personal-chatgpt \
     --public-url https://wiki.example.com/mcp \
     --identity https://example.com/ \
     --ceiling-profile daily-wiki-agent \
     --default-profile daily-wiki-agent \
     --tool-pack notes-read,notes-write,notes-manage,search,status,daily,tasks,custom,index \
     --dry-run
   vulcan --vault /path/to/vault mcp remote init personal-chatgpt \
     --public-url https://wiki.example.com/mcp \
     --identity https://example.com/ \
     --ceiling-profile daily-wiki-agent \
     --default-profile daily-wiki-agent \
     --tool-pack notes-read,notes-write,notes-manage,search,status,daily,tasks,custom,index
   vulcan mcp remote run personal-chatgpt
   ```

   For a resident process, use `vulcan daemon start --detach` in place of `remote run`. The
   daemon starts all configured one-vault named remotes together and reports the aggregate
   `listener.mcp-remotes` service in `vulcan daemon status`. Do not run the same instance in
   foreground and resident mode at once; its instance lock rejects the overlap. Restart the
   daemon after `remote init`, `set`, or `remove` to reload listener definitions. Multi-vault
   routing within one named remote is not available in resident mode yet.

2. Publish `https://wiki.example.com/mcp` through an HTTPS reverse proxy to the local Vulcan bind. Also proxy `https://wiki.example.com/.well-known/oauth-protected-resource/mcp`, `https://wiki.example.com/.well-known/oauth-authorization-server/mcp`, and `https://wiki.example.com/oauth/*` to the same Vulcan server.
3. In ChatGPT, open **Settings → Security and login**, enable **Developer mode**, then open **ChatGPT Plugins**, add a connection, and enter the public MCP URL including `/mcp`.
4. Sign in through IndieAuth. On Vulcan's consent page, verify the client, identity, exact MCP URL,
   vault, permission profile, tool packs, scopes, and expiry before approving.
5. Inspect or revoke access later with `vulcan mcp connections list`, `show <id>`, or
   `revoke <id>`. Removing a remote revokes its grants unless `--preserve-grants` is explicit.

`daily-wiki-agent` is the built-in pilot profile for this shape. It allows full vault note/task edits, config reads, and explicit index maintenance, with no shell, host execution, git mutation, refactor, or network access.

Vulcan owns ChatGPT-facing authorization-code + PKCE, Client ID Metadata Document and dynamic
client registration validation, 15-minute access tokens, rotating refresh tokens, replay-driven
token-family revocation, and bearer-token validation. The upstream IndieAuth hop also uses PKCE.
IndieAuth authenticates the human; it does not itself grant vault access.

Named definitions are device-global in Vulcan's user configuration, not in synced
`.vulcan/config.toml`. They reference registered wikis and vault-defined permission profiles. Grant,
refresh-family, last-use, and revocation state is device-local under Vulcan's user state directory;
OAuth secrets are stored per named remote there as owner-only files. `remote set` preserves the
instance identity, and different remotes can run concurrently when their loopback binds and public
URLs do not conflict.

The long-form direct flags remain available for debugging, generic local HTTP clients, external
OIDC, and compatibility. `vulcan mcp` without a management subcommand remains the daemon-independent
stdio default and does not require a named remote or browser login.

For external OIDC resource-server mode, use `--oauth-issuer`, `--oauth-audience`, and an allowed subject or email. This keeps Authentik as the token issuer, but ChatGPT compatibility can vary by provider metadata and token-exchange behavior.

`--auth-token` remains useful for private/internal clients that can set a shared bearer token or `x-vulcan-token`. It is mutually exclusive with direct OAuth mode and is not a ChatGPT-compatible public auth mechanism.

Foreground HTTP MCP starts a background vault watcher; resident mode uses the daemon's vault
observation runtime. Both run incremental scans after filesystem changes. Use `index_scan` when
you want an explicit refresh or a full reindex. Each request is bounded by the configured request
timeout, so long-running tool calls return a structured timeout error instead of leaving the client
waiting indefinitely.

For private development without publishing a public Vulcan endpoint, ChatGPT also supports Secure MCP Tunnel. That is a separate OpenAI-hosted connection path rather than a Vulcan authentication mode; keep the direct HTTPS/OAuth deployment above when you need a conventional independently reachable endpoint.

## ChatGPT Developer Mode

In ChatGPT Developer Mode, add the HTTPS MCP URL exposed by your front door. Prefer a complete static startup selection: OpenAI-hosted workflows may retain imported MCP tool definitions in conversation context, so mid-conversation registry changes are not a portable discovery mechanism. After changing server tool metadata, use the connection's **Refresh** action and start a new conversation before retesting.

Use adaptive mode only when the host demonstrably refreshes `tools/list` after pack mutations. Startup-selected packs remain pinned, and optional packs are changed through `tool_packs`:

```sh
vulcan --permissions daily-wiki-agent mcp \
  --transport http \
  --tool-pack-mode adaptive \
  --tool-pack notes-read,search,status,daily,tasks,index
```

## Tool Selection Guidance

For daily workflow questions, prefer:

- `daily` with `operation: latest|today|show|list|range` before generic reads or search. List/range calls are paginated and omit full event objects unless requested.
- `task_list` or `task_query` for task summaries.
- `task_create`, `task_complete`, and `task_reschedule` for task changes.
- `index_scan` with `full: false` to refresh stale search/query results, or `full: true` to force a full reindex.
- `note_append --periodic daily` as a low-risk log fallback.

Use generic note edits only when the task/daily tools do not model the requested change. MCP `note_set` requires `confirm: true`, and `note_delete` requires either `dry_run: true` or `confirm: true`.

## Vault Preparation

Run:

```sh
vulcan agent install
```

This refreshes skills that retain `metadata.vulcan.managed: true` and preserves unmarked
same-name skills. Use `vulcan agent install --reset <skill>` for a deliberate targeted reset.

Then edit the vault `AGENTS.md` with your routine conventions, inbox paths, daily-note headings, task conventions, and edit rules. Add a Daily Review or Routine skill under `.agents/skills/` when the routine has stable steps.

Current OpenAI references: [connect and test a ChatGPT plugin](https://developers.openai.com/plugins/deploy/connect-chatgpt) and [build an MCP server for plugins](https://developers.openai.com/api/docs/mcp).
