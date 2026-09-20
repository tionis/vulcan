# ChatGPT Plugin / MCP Wiki Setup

Use this setup for a private ChatGPT Developer Mode plugin connection to a personal wiki. Developer Mode availability depends on the account and workspace policy.

## Recommended Server Shape

Do not expose `vulcan mcp --auth-token` directly to the public internet. For ChatGPT, use Vulcan's built-in MCP OAuth issuer over HTTPS, keep Vulcan bound to loopback or private networking, and put only the HTTPS reverse proxy on the public internet. Human login can be delegated to IndieAuth with `--oauth-indieauth-me`.

Preferred direct HTTPS shape:

1. Run Vulcan on a private bind:

   ```sh
   vulcan --permissions daily-wiki-agent mcp \
     --transport http \
     --bind 127.0.0.1:8765 \
     --endpoint /mcp \
     --request-timeout 120s \
     --public-url https://wiki.example.com/mcp \
     --oauth-dcr \
     --oauth-indieauth-me https://example.com/ \
     --tool-pack notes-read,notes-write,notes-manage,search,status,daily,tasks,custom,index
   ```

2. Publish `https://wiki.example.com/mcp` through an HTTPS reverse proxy to the local Vulcan bind. Also proxy `https://wiki.example.com/.well-known/oauth-protected-resource/mcp`, `https://wiki.example.com/.well-known/oauth-authorization-server/mcp`, and `https://wiki.example.com/oauth/*` to the same Vulcan server.
3. In ChatGPT, open **Settings → Security and login**, enable **Developer mode**, then open **ChatGPT Plugins**, add a connection, and enter the public MCP URL including `/mcp`.
4. Keep shell, host execution, git mutation, unrestricted network, broad refactor, and config writes out of the selected permission profile.

`daily-wiki-agent` is the built-in pilot profile for this shape. It allows full vault note/task edits, config reads, and explicit index maintenance, with no shell, host execution, git mutation, refactor, or network access.

The recommended Vulcan path is its built-in MCP OAuth issuer. Vulcan owns ChatGPT-facing authorization-code + PKCE, dynamic client registration, short-lived MCP access tokens, and bearer-token validation. Current OpenAI guidance prefers Client ID Metadata Documents when an authorization server supports them, while dynamic client registration remains supported; Vulcan currently uses the latter. Human login can be delegated to an upstream IndieAuth server by setting `--oauth-indieauth-me` to your identity URL; Vulcan discovers `indieauth-metadata` from that profile URL and falls back to legacy `authorization_endpoint` / `token_endpoint` links. The upstream IndieAuth hop also uses PKCE.

For the common single-user setup, `--oauth-indieauth-me <identity>` together with `--permissions <profile>` is sufficient: Vulcan automatically allows that identity under the process-wide profile. For multi-user or per-user permissions, omit the process-level `--permissions` flag and bind each allowed subject with `--oauth-local-user <subject>=<profile>[,<email>]`. Explicit user bindings disable the single-user default. URL subjects are matched canonically, so `https://example.com` and `https://example.com/` are equivalent. If the IndieAuth provider returns an unexpected subject, the callback error reports that value and gives the exact configuration forms needed to authorize it.

When `--oauth-dcr` is enabled, ChatGPT can register dynamically instead of being configured with static client credentials. Vulcan generates and stores the local issuer signing secret in `.vulcan/mcp-oauth-issuer-secret` unless `--oauth-local-client-secret` is provided as an explicit override. `--oauth-local-approval-token` remains available as a simple fallback when IndieAuth is not configured.

For external OIDC resource-server mode, use `--oauth-issuer`, `--oauth-audience`, and an allowed subject or email. This keeps Authentik as the token issuer, but ChatGPT compatibility can vary by provider metadata and token-exchange behavior.

`--auth-token` remains useful for private/internal clients that can set a shared bearer token or `x-vulcan-token`. It is mutually exclusive with direct OAuth mode and is not a ChatGPT-compatible public auth mechanism.

HTTP MCP starts a background vault watcher and runs incremental scans after filesystem changes. Use `index_scan` when you want an explicit refresh or a full reindex. Each request is bounded by `--request-timeout`, so long-running tool calls return a structured timeout error instead of leaving the client waiting indefinitely.

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
