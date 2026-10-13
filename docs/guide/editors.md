# Editing a vault in Neovim, Vim, and other editors

Vulcan ships a language server, `vulcan lsp`, so any editor with an LSP client
gets vault-aware editing: go to definition on links, backlinks, hover
previews, completion of links, headings, block ids, tags, and frontmatter keys,
diagnostics for broken links, link-safe renames, and code actions. Start it
from the editor; it speaks JSON-RPC on stdin/stdout.

The server finds the vault from the editor's workspace folder (or
`--vault <dir>`), and never initializes one: run `vulcan index init` in the
vault first. The editor contract, including the custom `vulcan/` methods, is in
[the editor integration spec](../specs/editor-integration.md).

## Neovim

The recommended setup is the bundled plugin,
[`integrations/vulcan.nvim`](../../integrations/vulcan.nvim/README.md). It
registers the server and adds pickers, `:Vulcan` commands (daily notes,
templates, task toggling, moves, doctor), query results as virtual text, and
`:checkhealth vulcan`.

```sh
vulcan editor nvim install   # into Neovim's package directory; re-run after updates
```

Then call `require('vulcan').setup()`, or with lazy.nvim (which does not load
the package directory) point a spec at the installed copy:

```lua
{ dir = vim.fn.stdpath('data') .. '/site/pack/vulcan/start/vulcan.nvim', ft = 'markdown', cmd = 'Vulcan', opts = {} }
```

On AstroNvim, add `dependencies = { 'AstroNvim/astrolsp' }` to that spec so
AstroLSP's keymaps attach to the client the plugin starts, and do not list
`vulcan` in AstroLSP's `servers`; the plugin README has the full spec.

Without the plugin, Neovim 0.11+ needs only:

```lua
vim.lsp.config('vulcan', {
  cmd = { 'vulcan', 'lsp' },
  filetypes = { 'markdown' },
  root_markers = { '.vulcan', '.vulcan.toml', '.obsidian' },
})
vim.lsp.enable('vulcan')
```

For live Markdown rendering add render-markdown.nvim or markview.nvim, and
image.nvim or snacks.image for images; Vulcan does not draw Markdown itself.

## Vim

The recommended setup is the bundled plugin,
[`integrations/vulcan.vim`](../../integrations/vulcan.vim/README.md), on top
of [yegappan/lsp](https://github.com/yegappan/lsp) (Vim 9). It registers the
server itself and adds pickers, `:Vulcan` commands, query results as virtual
text, buffers that follow moved notes, and `:Vulcan health`.

```sh
vulcan editor vim install   # into ~/.vim/pack/vulcan/start; re-run after updates
```

One Vim session serves one vault, since yegappan/lsp starts one server per
registration.

Without the plugin, with yegappan/lsp:

```vim
call LspAddServer([#{
      \ name: 'vulcan',
      \ filetype: ['markdown'],
      \ path: 'vulcan',
      \ args: ['lsp'],
      \ rootSearch: ['.vulcan/', '.vulcan.toml', '.obsidian/'],
      \ runIfSearch: ['.vulcan/', '.vulcan.toml', '.obsidian/'],
      \ }])
```

(yegappan/lsp wants a trailing `/` on directory markers.) Without the plugin,
moving an open note leaves its buffer on the old path; reopen it from its new
one.

With [vim-lsp](https://github.com/prabirshrestha/vim-lsp):

```vim
autocmd User lsp_setup call lsp#register_server({
      \ 'name': 'vulcan',
      \ 'cmd': {server_info -> ['vulcan', 'lsp']},
      \ 'allowlist': ['markdown'],
      \ 'root_uri': {server_info -> lsp#utils#path_to_uri(
      \     lsp#utils#find_nearest_parent_directory(lsp#utils#get_buffer_path(), '.vulcan/'))},
      \ })
```

Pickers work through the CLI, for example with fzf.vim (paths are vault-relative, so run Vim from the vault root):

```vim
command! VulcanNotes call fzf#run(fzf#wrap({'source': 'vulcan ls --format paths', 'sink': 'edit'}))
" Creates today's daily note from its template when missing, then opens it.
command! VulcanDaily execute 'edit' trim(system('vulcan --output json periodic daily --no-edit 2>/dev/null | jq -r .path'))
```

## Helix

Helix has no plugins, so its integration,
[`integrations/helix`](../../integrations/helix/README.md), is configuration
the binary merges into `~/.config/helix/`, keeping your own settings:

```sh
vulcan editor helix install --dry-run   # preview as a diff, then without --dry-run
```

Its `languages.toml` registers the server for Markdown and starts it only in
vaults; its `config.toml` adds keybindings under `space v` for daily notes,
task toggling, section extraction, capture, and rescans, through
`:lsp-workspace-command`. Start Helix in the vault. Query results are not
drawn, since Helix has no virtual text for servers.

## Emacs

With Eglot:

```elisp
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs '(markdown-mode . ("vulcan" "lsp"))))
```

## Several vaults in one workspace

Editors that open several workspace folders in one session (VS Code
multi-root workspaces, Helix, Zed) can mix vaults: the server serves each
folder's vault separately, so links, diagnostics, and renames stay inside
the vault a note belongs to, while workspace symbols and diagnostics cover
all of them. Neovim starts one server per vault root instead.

## Several editors on one vault

Each editor process runs its own server. The first to start holds
`.vulcan/editor-watch.lock` and watches the file system; the others follow the
cache. Set `initializationOptions.watcher` to `"client"` to rely on the
editor's file watching instead, or `"off"` to index only saved files.

## Safety

- Unsaved buffer text never reaches the cache; other Vulcan commands see a
  note's changes once it is saved.
- Renames and moves write closed files as one journaled batch and edit open
  buffers, including unsaved text, through the editor.
- `dataviewjs` blocks run in the strict sandbox (computation and index reads;
  no file access, network, or writes) in every vault. Vaults you trust with
  `vulcan trust` get full evaluation, which still never writes.
- `vulcan --permissions <profile> lsp` hides notes the profile cannot read and
  checks every planned change against the profile.
