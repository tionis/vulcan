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

```lua
-- lazy.nvim
{ dir = '/path/to/vulcan/integrations/vulcan.nvim', ft = 'markdown', cmd = 'Vulcan', opts = {} }
```

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

With [yegappan/lsp](https://github.com/yegappan/lsp):

```vim
call LspAddServer([#{
      \ name: 'vulcan',
      \ filetype: ['markdown'],
      \ path: 'vulcan',
      \ args: ['lsp'],
      \ rootSearch: ['.vulcan', '.vulcan.toml', '.obsidian'],
      \ }])
```

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

In `languages.toml`:

```toml
[language-server.vulcan]
command = "vulcan"
args = ["lsp"]

[[language]]
name = "markdown"
language-servers = ["vulcan"]
roots = [".vulcan", ".vulcan.toml", ".obsidian"]
```

## Emacs

With Eglot:

```elisp
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs '(markdown-mode . ("vulcan" "lsp"))))
```

## Several editors on one vault

Each editor process runs its own server. The first to start holds
`.vulcan/editor-watch.lock` and watches the file system; the others follow the
cache. Set `initializationOptions.watcher` to `"client"` to rely on the
editor's file watching instead, or `"off"` to index only saved files.

## Safety

- Unsaved buffer text never reaches the cache; other Vulcan commands see a
  note's changes once it is saved.
- Renames and moves write closed files as one journaled batch and edit open
  buffers through the editor; a buffer with unsaved changes blocks them.
- `dataviewjs` blocks run in the strict sandbox (computation and index reads;
  no file access, network, or writes) in every vault. Vaults you trust with
  `vulcan trust` get full evaluation, which still never writes.
- `vulcan --permissions <profile> lsp` hides notes the profile cannot read and
  checks every planned change against the profile.
