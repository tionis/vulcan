---
name: mcp-setup
description: Set up, debug, and operate Vulcan's MCP server for ChatGPT or other MCP clients. Use when the user asks about MCP transport, OAuth/IndieAuth, tool packs, remote HTTPS setup, ChatGPT Developer Mode, or MCP tool/resource visibility.
version: 6
tools:
  - mcp
  - describe
  - help
  - config_show
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# MCP Setup

## When to Use This Skill

Use this skill for MCP server configuration, ChatGPT remote connector setup, OAuth/IndieAuth
debugging, tool pack selection, and permission-profile questions.

## Recommended Flow

1. For a local harness, use `vulcan mcp --transport stdio` (the default) or direct loopback HTTP. No daemon, browser, or named remote is required.
2. For ChatGPT or another hosted client, register the vault and preview `vulcan mcp remote init <name> --public-url <https-url> --identity <indieauth-url> --dry-run`. Apply it after reviewing the vault, loopback bind, ceiling/default profiles, and eligible packs.
3. Start it with `vulcan mcp remote run <name>` and give the client the single public MCP URL printed by init/show. Proxy the MCP path, OAuth metadata paths, and `/oauth/*` to that loopback listener.
4. Sign in with IndieAuth, then review and explicitly approve Vulcan's separate consent page. Select a profile bounded by the remote ceiling, eligible packs, and expiry. IndieAuth login alone grants no vault authority.
5. Inspect or revoke durable approvals with `vulcan mcp connections list|show|revoke`. Use `vulcan mcp remote set` for deployment changes and `remote remove` to revoke its grants and remove only the device-global definition.
6. Use `vulcan describe --format mcp --tool-pack ...` to inspect the exposed static registry and MCP resources to inspect prompts, skills, skill commands, and pack catalogs from the client.
7. Select `--tool-pack sync` only for repository-wide Git synchronization diagnostics. It provides `sync_status`, mutation-free `sync_plan`, `sync_doctor`, and `sync_conflicts`; it does not expose conflict resolution or arbitrary Git commands.
8. Treat adaptive packs as an optimization for clients that demonstrably refresh `tools/list`. Common navigation reads (`note_get`, `note_outline`, `search`, `query`, and `daily`) are startup-pinned on the default surface. Use `capabilities` for a compact active-tool and limit summary.
9. In ChatGPT, enable Developer Mode under Settings → Security and login, add the endpoint from ChatGPT Plugins, review the discovered tools, and use the connection's Refresh action after metadata changes. Developer Mode availability depends on account and workspace policy.

## Guardrails

- Do not expose a no-auth public MCP server for a private vault.
- Keep Vulcan bound to loopback or a private interface behind the HTTPS front door unless you have a deliberate deployment reason.
- For private development, OpenAI Secure MCP Tunnel is an alternative to exposing a public endpoint. Treat it as a separate OpenAI connection path, not as a Vulcan authentication flag.
- Tool packs are not authorization. Permission profiles still decide what is visible and callable.
- Custom-tool resource details (`vulcan://assistant/tools/{name}`) require the selected `custom` pack even when a client guesses a tool URI; review the profile and pack together when debugging missing resources.
- `tool_packs` combines list/enable/disable/set operations. Startup-selected packs cannot be removed by adaptive calls. A successful mutation still only changes server state: the client must process `notifications/tools/list_changed`, call `tools/list` again, and replace its callable schemas. Legacy `tool_pack_*` calls remain hidden aliases for cached clients.
- [OpenAI's MCP guide](https://developers.openai.com/api/docs/mcp) documents remote MCP behavior and authentication. Do not rely on mid-conversation dynamic exposure for OpenAI-hosted workflows; start with the required static packs, use the ChatGPT connection's Refresh action after metadata changes, and retest in a new conversation.
- The `sync` pack requires Git permission and full-vault read permission because its safety reports may name paths anywhere in the repository. A path-filtered read profile intentionally sees no sync tools; use scoped note/search tools or a separately reviewed full-read Git profile.
- The optional `graph` pack contains `graph_communities` and `suggest_links`; keep it out of compact retrieval-only deployments.
- If ChatGPT cannot start auth, check issuer metadata, redirect URI, PKCE, allowed principals, and public URL consistency before changing tool permissions.
- Vulcan accepts only its advertised OAuth scopes and requires the authorization `resource` to match the exact public MCP URL. A scope or resource error is an authentication configuration problem, not a reason to widen the permission profile.
- If IndieAuth returns an unauthorized subject, use the subject shown in Vulcan's callback error to correct `--oauth-indieauth-me` or an explicit `--oauth-local-user` binding.
- Reject the consent page if its client, identity, resource, vault, permission profile, or tool packs are unexpected. IndieAuth login authenticates the person; the separate Vulcan consent action authorizes the MCP connection.
- Named remote definitions are device-global and never copied through vault sync. Vault permission profiles remain in `.vulcan/config.toml`; grants, refresh-token families, revocations, and per-remote OAuth secrets remain device-local outside the rebuildable cache.
- A named remote ceiling cannot be `unrestricted`. Changing a profile may narrow an existing grant, but a broader current profile is rejected until the user grants fresh consent.
- Client ID Metadata Documents are accepted only as public HTTPS clients with exact IDs and allowlisted redirect hosts. Dynamic registration remains available; do not work around failed client validation by weakening redirect checks.

## Example Moves

- Initialize `personal-chatgpt`, run it, inspect the resulting connection, and revoke it without touching the vault.
- Use the long-form direct `--oauth-*` flags only for debugging, external OIDC, or compatibility.
- Debug why `tools/list` does not include a skill command under the selected pack/profile.
- Compare `describe --format mcp` output against the live MCP registry.
