---
name: mcp-setup
description: Set up, debug, and operate Vulcan's MCP server for ChatGPT or other MCP clients. Use when the user asks about MCP transport, OAuth/IndieAuth, tool packs, remote HTTPS setup, ChatGPT Developer Mode, or MCP tool/resource visibility.
version: 5
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

1. Start local first: `vulcan mcp --transport stdio` or `--transport http`.
2. For single-user ChatGPT access, use HTTPS, `--public-url`, `--oauth-dcr`, `--oauth-indieauth-me <identity>`, and a narrow `--permissions <profile>`. Sign in with the configured identity, then review and explicitly approve Vulcan's consent page. It shows the client, identity, resource, vault, permission profile, and tool packs before issuing a code. OpenAI currently prefers Client ID Metadata Documents where supported but continues to support dynamic client registration; Vulcan uses the latter.
3. For multi-user access, omit process-level `--permissions` and bind each identity with `--oauth-local-user <subject>=<profile>`.
4. Choose tool packs explicitly with repeated `--tool-pack` or comma-separated pack names.
5. Use `vulcan describe --format mcp --tool-pack ...` to inspect the exposed static registry.
6. Use MCP resources to inspect prompts, skills, skill commands, and pack catalogs from the client.
7. Select `--tool-pack sync` only for repository-wide Git synchronization diagnostics. It provides `sync_status`, mutation-free `sync_plan`, `sync_doctor`, and `sync_conflicts`; it does not expose conflict resolution or arbitrary Git commands.
8. Treat adaptive packs as an optimization for clients that demonstrably refresh `tools/list`. Common navigation reads (`note_get`, `note_outline`, `search`, `query`, and `daily`) are startup-pinned on the default surface. Use `capabilities` for a compact active-tool and limit summary.
9. In ChatGPT, enable Developer Mode under Settings → Security and login, add the endpoint from ChatGPT Plugins, review the discovered tools, and use the connection's Refresh action after metadata changes. Developer Mode availability depends on account and workspace policy.

## Guardrails

- Do not expose a no-auth public MCP server for a private vault.
- Keep Vulcan bound to loopback or a private interface behind the HTTPS front door unless you have a deliberate deployment reason.
- For private development, OpenAI Secure MCP Tunnel is an alternative to exposing a public endpoint. Treat it as a separate OpenAI connection path, not as a Vulcan authentication flag.
- Tool packs are not authorization. Permission profiles still decide what is visible and callable.
- `tool_packs` combines list/enable/disable/set operations. Startup-selected packs cannot be removed by adaptive calls. A successful mutation still only changes server state: the client must process `notifications/tools/list_changed`, call `tools/list` again, and replace its callable schemas. Legacy `tool_pack_*` calls remain hidden aliases for cached clients.
- [OpenAI's MCP guide](https://developers.openai.com/api/docs/mcp) documents remote MCP behavior and authentication. Do not rely on mid-conversation dynamic exposure for OpenAI-hosted workflows; start with the required static packs, use the ChatGPT connection's Refresh action after metadata changes, and retest in a new conversation.
- The `sync` pack requires Git permission and full-vault read permission because its safety reports may name paths anywhere in the repository. A path-filtered read profile intentionally sees no sync tools; use scoped note/search tools or a separately reviewed full-read Git profile.
- The optional `graph` pack contains `graph_communities` and `suggest_links`; keep it out of compact retrieval-only deployments.
- If ChatGPT cannot start auth, check issuer metadata, redirect URI, PKCE, allowed principals, and public URL consistency before changing tool permissions.
- Vulcan accepts only its advertised OAuth scopes and requires the authorization `resource` to match the exact public MCP URL. A scope or resource error is an authentication configuration problem, not a reason to widen the permission profile.
- If IndieAuth returns an unauthorized subject, use the subject shown in Vulcan's callback error to correct `--oauth-indieauth-me` or an explicit `--oauth-local-user` binding.
- Reject the consent page if its client, identity, resource, vault, permission profile, or tool packs are unexpected. IndieAuth login authenticates the person; the separate Vulcan consent action authorizes the MCP connection.

## Example Moves

- Build a ChatGPT command line using `--transport http`, `--endpoint /mcp`, `--public-url`, `--oauth-dcr`, and `--oauth-indieauth-me`.
- Debug why `tools/list` does not include a skill command under the selected pack/profile.
- Compare `describe --format mcp` output against the live MCP registry.
