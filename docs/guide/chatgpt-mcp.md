# ChatGPT MCP Wiki Setup

Use this setup for a private ChatGPT Developer Mode connection to a personal wiki.

## Recommended Server Shape

Do not expose `vulcan mcp --auth-token` directly to the public internet. For ChatGPT, use Vulcan's built-in MCP OAuth issuer over HTTPS, keep Vulcan bound to loopback or private networking, and put only the HTTPS reverse proxy on the public internet. Human login can be delegated to IndieAuth with `--oauth-indieauth-me`.

Preferred direct ChatGPT shape:

1. Run Vulcan on a private bind:

   ```sh
   vulcan mcp \
     --transport http \
     --bind 127.0.0.1:8765 \
     --endpoint /mcp \
     --request-timeout 120s \
     --public-url https://wiki.example.com/mcp \
     --oauth-dcr \
     --oauth-indieauth-me https://example.com/ \
     --oauth-local-subject fallback \
     --oauth-local-user https://example.com/=daily-wiki-agent,you@example.com \
     --tool-pack notes-read,notes-write,notes-manage,search,status,daily,tasks,custom,index
   ```

2. Publish `https://wiki.example.com/mcp` through an HTTPS reverse proxy to the local Vulcan bind. Also proxy `https://wiki.example.com/.well-known/oauth-protected-resource/mcp`, `https://wiki.example.com/.well-known/oauth-authorization-server/mcp`, and `https://wiki.example.com/oauth/*` to the same Vulcan server.
3. Configure ChatGPT Developer Mode with the public MCP URL.
4. Keep shell, host execution, git mutation, unrestricted network, broad refactor, and config writes out of the selected permission profile.

`daily-wiki-agent` is the built-in pilot profile for this shape. It allows full vault note/task edits, config reads, and explicit index maintenance, with no shell, host execution, git mutation, refactor, or network access.

The recommended ChatGPT path is Vulcan's built-in MCP OAuth issuer. Vulcan owns ChatGPT-facing authorization-code + PKCE, dynamic client registration, short-lived MCP access tokens, and bearer-token validation. Human login can be delegated to an upstream IndieAuth server by setting `--oauth-indieauth-me` to your identity URL; Vulcan discovers `indieauth-metadata` from that profile URL and falls back to legacy `authorization_endpoint` / `token_endpoint` links. The upstream IndieAuth hop also uses PKCE. Vulcan maps the returned IndieAuth subject to a permission profile with `--oauth-local-user <subject>=<profile>[,<email>]`; URL subjects are matched canonically, so `https://example.com` and `https://example.com/` are equivalent.

For per-user permission binding, omit the process-level `--permissions` flag and bind each allowed IndieAuth subject with `--oauth-local-user`. If `--permissions` is provided, it remains the process-wide profile for all MCP sessions.

When `--oauth-dcr` is enabled, ChatGPT can register dynamically instead of being configured with static client credentials. Vulcan generates and stores the local issuer signing secret in `.vulcan/mcp-oauth-issuer-secret` unless `--oauth-local-client-secret` is provided as an explicit override. `--oauth-local-approval-token` remains available as a simple fallback when IndieAuth is not configured.

For external OIDC resource-server mode, use `--oauth-issuer`, `--oauth-audience`, and an allowed subject or email. This keeps Authentik as the token issuer, but ChatGPT compatibility can vary by provider metadata and token-exchange behavior.

`--auth-token` remains useful for private/internal clients that can set a shared bearer token or `x-vulcan-token`. It is mutually exclusive with direct OAuth mode and is not a ChatGPT-compatible public auth mechanism.

HTTP MCP starts a background vault watcher and runs incremental scans after filesystem changes. Use `index_scan` when you want an explicit refresh or a full reindex. Each request is bounded by `--request-timeout`, so long-running tool calls return a structured timeout error instead of leaving the client waiting indefinitely.

## ChatGPT Developer Mode

In ChatGPT Developer Mode, add the HTTPS MCP URL exposed by your front door. Refresh tools after changing Vulcan's `--tool-pack` flags or after enabling packs in adaptive mode.

Use static packs for hosts that do not react to `notifications/tools/list_changed`. Use adaptive mode only when the host reliably refreshes `tools/list` after pack mutations:

```sh
vulcan --permissions daily-wiki-agent mcp \
  --transport http \
  --tool-pack-mode adaptive \
  --tool-pack notes-read,search,status,daily,tasks,index
```

## Tool Selection Guidance

For daily workflow questions, prefer:

- `daily_show` before `note_get` for today's daily note.
- `daily_list` for week or month summaries.
- `task_list` or `task_query` for task summaries.
- `task_create`, `task_complete`, and `task_reschedule` for task changes.
- `index_scan` with `full: false` to refresh stale search/query results, or `full: true` to force a full reindex.
- `note_append --periodic daily` as a low-risk log fallback.

Use generic note edits only when the task/daily tools do not model the requested change. MCP `note_set` requires `confirm: true`, and `note_delete` requires either `dry_run: true` or `confirm: true`.

## Vault Preparation

Run:

```sh
vulcan agent install --overwrite
```

Then edit the vault `AGENTS.md` with your routine conventions, inbox paths, daily-note headings, task conventions, and edit rules. Add a Daily Review or Routine skill under `.agents/skills/` when the routine has stable steps.
