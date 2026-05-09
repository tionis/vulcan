---
name: mcp-setup
description: Set up, debug, and operate Vulcan's MCP server for ChatGPT or other MCP clients. Use when the user asks about MCP transport, OAuth/IndieAuth, tool packs, remote HTTPS setup, ChatGPT Developer Mode, or MCP tool/resource visibility.
version: 1
tools:
  - mcp
  - describe
  - help
  - config_show
require_confirmation: false
---

# MCP Setup

## When to Use This Skill

Use this skill for MCP server configuration, ChatGPT remote connector setup, OAuth/IndieAuth
debugging, tool pack selection, and permission-profile questions.

## Recommended Flow

1. Start local first: `vulcan mcp --transport stdio` or `--transport http`.
2. For remote ChatGPT access, use HTTPS, `--public-url`, embedded OAuth/IndieAuth, DCR when useful, and a narrow permission profile.
3. Choose tool packs explicitly with repeated `--tool-pack` or comma-separated pack names.
4. Use `vulcan describe --format mcp --tool-pack ...` to inspect the exposed static registry.
5. Use MCP resources to inspect prompts, skills, skill commands, and pack catalogs from the client.

## Guardrails

- Do not expose a no-auth public MCP server for a private vault.
- Keep Vulcan bound to loopback or a private interface behind the HTTPS front door unless you have a deliberate deployment reason.
- Tool packs are not authorization. Permission profiles still decide what is visible and callable.
- If ChatGPT cannot start auth, check issuer metadata, redirect URI, PKCE, allowed principals, and public URL consistency before changing tool permissions.

## Example Moves

- Build a ChatGPT command line using `--transport http`, `--endpoint /mcp`, `--public-url`, `--oauth-dcr`, and `--oauth-indieauth-me`.
- Debug why `tools/list` does not include a skill command under the selected pack/profile.
- Compare `describe --format mcp` output against the live MCP registry.
