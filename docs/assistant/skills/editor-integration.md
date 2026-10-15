---
name: editor-integration
description: Set up and troubleshoot Vulcan's language server for Neovim, Vim, Helix, Emacs, Zed, or another LSP client. Use when the user wants link navigation, backlinks, completion, or diagnostics in a terminal editor, or asks why their editor reports an unresolved or ambiguous link.
version: 10
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

   For Neovim, prefer the bundled `integrations/vulcan.nvim` plugin (pickers, `:Vulcan` commands,
   query results as virtual text, `:checkhealth vulcan`). `vulcan editor nvim install` (with
   `--dry-run` to preview) installs the copy matching the binary into Neovim's package directory;
   `vulcan editor nvim status --output json` reports whether it is current, and copies with local
   changes need the user's `--force`. With lazy.nvim, point a `dir` spec at
   `vulcan editor nvim path`; on AstroNvim add `dependencies = { "AstroNvim/astrolsp" }` so
   AstroLSP's keymaps attach to the plugin's client, and do not also list `vulcan` in AstroLSP's
   `servers`. For Vim 9 with yegappan/lsp, `vulcan editor vim install`
   installs the matching `vulcan.vim` the same way (it registers the server itself; one vault per
   Vim session); for Helix, `vulcan editor helix install` merges the `languages.toml` entries
   and `space v` keybindings into `~/.config/helix/` without overwriting the user's settings (run
   it with `--dry-run` first and show the diff; `--force` replaces bindings the user set, so only
   pass it when they ask), and `print-config` prints them for merging by hand; `docs/guide/editors.md` covers Vim,
   Helix, and Emacs. Other clients take the same command. Pass `--vault <dir>` only when the editor's workspace
   folder is not inside the vault; otherwise the server discovers it like any other command.
3. Explain what the server offers: go-to-definition on links, embeds, `#heading`, and `#^block`
   subpaths; references (backlinks); hover previews; completion after `[[`, `#`, and in
   frontmatter keys; document and workspace symbols; diagnostics for open buffers; and renames
   that rewrite every link (rename a link's note target to move the note, or a heading or block
   id). The `vulcan.note.move` command moves any note, with `dryRun` to preview. Code actions
   create a missing note from an unresolved link, pick the target of an ambiguous link, convert
   between wikilink and Markdown style, and toggle the task on the cursor line
   (`vulcan.task.toggle`; completing a recurring task inserts its next occurrence). On a heading,
   a code action extracts its section into a new note (`vulcan.section.extract`), retargeting
   links into the section. Commands also
   open or create periodic notes (`vulcan.periodic.open`), insert templates
   (`vulcan.template.insert`, honoring `tp.file.cursor()`), capture to the inbox
   (`vulcan.capture`), and rescan (`vulcan.scan`); `vulcan/render` returns query results for
   display beside their blocks.
4. For a whole-vault view of broken links, run `vulcan doctor`. Editor diagnostics use the same
   categories (`unresolved-link`, `ambiguous-link`, `outside-vault-link`, `malformed-frontmatter`);
   they are pushed for open buffers, and clients that pull `workspace/diagnostic` also see closed
   notes the index reports problems in.
5. Point to `vulcan lsp --help` and `docs/specs/editor-integration.md` for supported methods and
   the custom `vulcan/` protocol (`vulcan/status`, versioned by `protocolVersion`).

## Several Editors on One Vault

Each editor process runs its own `vulcan lsp`. One process per vault, an editor or another Vulcan
process such as the daemon or `vulcan serve`, holds `.vulcan/index-owner.lock` and indexes file
system changes; the others follow it and take over when it exits. `vulcan/status` shows which
(`watcher`: `lease` or `following`). If the editor already reports file changes, `initializationOptions =
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
- Editors render `dataviewjs` blocks in the strict sandbox for every vault; full evaluation
  follows `vulcan trust`. Only suggest `vulcan trust` for a vault the user knows, since trust also
  enables its startup scripts and plugins.
- Editor renames work with unsaved buffers: each one is rewritten from its own text. Only moving a
  note that itself has unsaved changes is refused; save it first.
- Do not start `vulcan lsp` interactively; it speaks JSON-RPC on stdout and waits for a client.
