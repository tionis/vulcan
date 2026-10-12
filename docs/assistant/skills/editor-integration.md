---
name: editor-integration
description: Set up and troubleshoot Vulcan's language server for Neovim, Vim, Helix, Emacs, Zed, or another LSP client. Use when the user wants link navigation, backlinks, completion, or diagnostics in a terminal editor, or asks why their editor reports an unresolved or ambiguous link.
version: 2
tools:
  - describe
  - help
  - doctor
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Editor Integration

## When to Use This Skill

Use this skill when someone edits the vault in a terminal or desktop editor other than Obsidian
and wants vault-aware features, or when editor diagnostics need explaining.

## Recommended Flow

1. Confirm the vault is initialized (`.vulcan/` exists). `vulcan lsp` never initializes a vault;
   without one it serves buffer-local features and shows a warning. Run `vulcan index init` first
   if needed.
2. Configure the editor to start `vulcan lsp` for Markdown files, with root markers `.vulcan`,
   `.vulcan.toml`, and `.obsidian`. For Neovim 0.11+:

   ```lua
   vim.lsp.config("vulcan", {
     cmd = { "vulcan", "lsp" },
     filetypes = { "markdown" },
     root_markers = { ".vulcan", ".vulcan.toml", ".obsidian" },
   })
   vim.lsp.enable("vulcan")
   ```

   Other clients take the same command. Pass `--vault <dir>` only when the editor's workspace
   folder is not inside the vault; otherwise the server discovers it like any other command.
3. Explain what the server offers: go-to-definition on links, embeds, `#heading`, and `#^block`
   subpaths; references (backlinks); hover previews; completion after `[[`, `#`, and in
   frontmatter keys; document and workspace symbols; diagnostics for open buffers.
4. For a whole-vault view of broken links, run `vulcan doctor`. Editor diagnostics use the same
   categories (`unresolved-link`, `ambiguous-link`, `outside-vault-link`, `malformed-frontmatter`)
   but only cover open buffers.
5. Point to `vulcan lsp --help` and `docs/specs/editor-integration.md` for supported methods and
   the custom `vulcan/` protocol (`vulcan/status`, versioned by `protocolVersion`).

## Several Editors on One Vault

Each editor process runs its own `vulcan lsp`. One of them holds `.vulcan/editor-watch.lock` and
watches the file system; the others follow the cache. `vulcan/status` shows which (`watcher`:
`lease` or `polling`). If the editor already reports file changes, `initializationOptions =
{ watcher = "client" }` uses those instead; `"off"` indexes saved files only. Never delete the
lease file to change which process watches: it is an OS lock that is released when the process
exits.

## Guardrails

- The language server never writes unsaved buffer text to the cache. A link to a note that exists
  only as an unsaved buffer resolves in the editor, but `doctor`, queries, and other commands see
  it only after the file is saved.
- Agent edits made with `vulcan note`, `move`, or `refactor` while the vault is open in an editor
  stay safe: they take the vault write lock and check file contents first. The editor reloads the
  changed files. Prefer `move` and `refactor` over ad hoc edits so link rewrites stay journaled.
- Do not start `vulcan lsp` interactively; it speaks JSON-RPC on stdout and waits for a client.
