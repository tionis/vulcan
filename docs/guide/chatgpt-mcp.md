# ChatGPT MCP Wiki Setup

Use this setup for a private ChatGPT Developer Mode connection to a personal wiki.

## Recommended Server Shape

Do not expose `vulcan mcp --auth-token` directly to the public internet. For ChatGPT, use MCP OAuth mode with an external OIDC provider such as Authentik, or keep Vulcan on loopback/private networking behind an OAuth-aware front door.

Preferred direct ChatGPT shape:

1. Run Vulcan on a private bind:

   ```sh
   vulcan mcp \
     --transport http \
     --bind 127.0.0.1:8765 \
     --endpoint /mcp \
     --public-url https://wiki.example.com/mcp \
     --oauth-local-client-secret "$VULCAN_MCP_OAUTH_CLIENT_SECRET" \
     --oauth-dcr \
     --oauth-indieauth-me https://example.com/ \
     --oauth-local-subject fallback \
     --oauth-local-user https://example.com/=daily-wiki-agent,you@example.com \
     --tool-pack notes-read,notes-write,notes-manage,search,status,daily,tasks,custom
   ```

2. Publish `https://wiki.example.com/mcp` through an HTTPS reverse proxy to the local Vulcan bind. Also proxy `https://wiki.example.com/.well-known/oauth-protected-resource/mcp`, `https://wiki.example.com/.well-known/oauth-authorization-server/mcp`, and `https://wiki.example.com/oauth/*` to the same Vulcan server.
3. Configure ChatGPT Developer Mode with the public MCP URL.
4. Keep shell, host execution, git mutation, unrestricted network, broad refactor, and config writes out of the selected permission profile.

`daily-wiki-agent` is the built-in pilot profile for this shape. It allows full vault note/task edits, config reads, and no shell, host execution, git mutation, refactor, network, or explicit index maintenance.

The recommended ChatGPT path is Vulcan's built-in MCP OAuth issuer. Vulcan owns ChatGPT-facing authorization-code + PKCE, dynamic client registration, short-lived MCP access tokens, and bearer-token validation. Human login can be delegated to an upstream IndieAuth server by setting `--oauth-indieauth-me` to your identity URL; Vulcan discovers `indieauth-metadata` from that profile URL and falls back to legacy `authorization_endpoint` / `token_endpoint` links. Vulcan maps the returned IndieAuth subject to a permission profile with `--oauth-local-user <subject>=<profile>[,<email>]`.

For per-user permission binding, omit the process-level `--permissions` flag and bind each allowed IndieAuth subject with `--oauth-local-user`. If `--permissions` is provided, it remains the process-wide profile for all MCP sessions.

`--oauth-local-client-secret` is the local issuer signing secret and should be high entropy. When `--oauth-dcr` is enabled, ChatGPT can register dynamically instead of being configured with static client credentials. `--oauth-local-approval-token` remains available as a simple fallback when IndieAuth is not configured.

For external OIDC resource-server mode, use `--oauth-issuer`, `--oauth-audience`, and an allowed subject or email. This keeps Authentik as the token issuer, but ChatGPT compatibility can vary by provider metadata and token-exchange behavior.

`--auth-token` remains useful for private/internal clients that can set a shared bearer token or `x-vulcan-token`. It is mutually exclusive with direct OAuth mode and is not a ChatGPT-compatible public auth mechanism.

## ChatGPT Developer Mode

In ChatGPT Developer Mode, add the HTTPS MCP URL exposed by your front door. Refresh tools after changing Vulcan's `--tool-pack` flags or after enabling packs in adaptive mode.

Use static packs for hosts that do not react to `notifications/tools/list_changed`. Use adaptive mode only when the host reliably refreshes `tools/list` after pack mutations:

```sh
vulcan --permissions daily-wiki-agent mcp \
  --transport http \
  --tool-pack-mode adaptive \
  --tool-pack notes-read,search,status,daily,tasks
```

## Tool Selection Guidance

For daily workflow questions, prefer:

- `daily_show` before `note_get` for today's daily note.
- `daily_list` for week or month summaries.
- `task_list` or `task_query` for task summaries.
- `task_create`, `task_complete`, and `task_reschedule` for task changes.
- `note_append --periodic daily` as a low-risk log fallback.

Use generic note edits only when the task/daily tools do not model the requested change. MCP `note_set` requires `confirm: true`, and `note_delete` requires either `dry_run: true` or `confirm: true`.

## Vault Preparation

Run:

```sh
vulcan agent install --overwrite
```

Then edit the vault `AGENTS.md` with your routine conventions, inbox paths, daily-note headings, task conventions, and edit rules. Add a Daily Review or Routine skill under `.agents/skills/` when the routine has stable steps.
