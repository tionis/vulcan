# vulcan.nvim

Neovim integration for [Vulcan](../../README.md) vaults: a thin layer over the
`vulcan lsp` language server and `vulcan --output json`. Vault semantics (link
resolution, tasks, templates, periodic notes) stay in Vulcan; this plugin
registers the server, adds commands and pickers, and draws query results.

Requires Neovim 0.10 or newer (0.11+ recommended) and a `vulcan` binary on
`PATH`.

## Install

With lazy.nvim, from a checkout of the Vulcan repository:

```lua
{
  dir = '/path/to/vulcan/integrations/vulcan.nvim',
  ft = 'markdown',
  cmd = 'Vulcan',
  opts = {},
}
```

Or call `require('vulcan').setup()` from your config after adding the folder
to `runtimepath`.

## What you get

From the language server, in any Markdown buffer inside a vault (a folder with
`.vulcan/`, `.vulcan.toml`, or `.obsidian/`):

- go to definition on links, embeds, `#heading` and `#^block` subpaths
- references (backlinks), hover previews, document and workspace symbols
- completion after `[[`, `#`, `^`, and in frontmatter
- diagnostics for unresolved and ambiguous links and frontmatter problems
- link-safe rename of notes, headings, and block ids (`vim.lsp.buf.rename()`)
- code actions: create a missing note, pick an ambiguous link's target,
  convert link style, complete or reopen a task
- semantic highlighting of links, tags, inline fields, and block ids, with
  unresolved links dimmed (override the `@lsp.type.*.markdown` and
  `@lsp.mod.unresolved.markdown` highlight groups to restyle them)
- a backlink count above each note and due-date hints on open tasks

From the plugin:

- query results (inline expressions, Dataview, Tasks, Bases) as virtual text
  beside their source, refreshed on save and whenever the index changes
- `:Vulcan` commands and pickers (below)
- `:checkhealth vulcan`

## Commands

| Command | Does |
| --- | --- |
| `:Vulcan notes` | Pick a note |
| `:Vulcan search [query]` | Full-text search |
| `:Vulcan tags` | Pick a tag, then a note with it |
| `:Vulcan backlinks` | References to the current note |
| `:Vulcan daily [date]`, `weekly`, `monthly`, `periodic {type} [date]` | Open a periodic note, creating it from its template |
| `:Vulcan template [name]` | Insert a template at the cursor |
| `:Vulcan task` | Complete or reopen the task on the cursor line |
| `:Vulcan capture [text]` | Append a quick capture to the configured inbox note |
| `:Vulcan move [path]` | Move the current note, rewriting every link |
| `:Vulcan scan[!]` | Rescan the vault (`!` rebuilds) |
| `:Vulcan render` | Toggle query results for this buffer |
| `:Vulcan doctor` | Load `vulcan doctor` problems into the quickfix list |
| `:Vulcan status` | Show the server's cache and watcher state |
| `:Vulcan browse`, `bases [file]`, `kanban [file]` | Open Vulcan's TUIs in a floating terminal |

Pickers use `vim.ui.select`, so they appear in telescope, fzf-lua, snacks, or
whatever `ui.select` provider you have configured (for example fzf-lua's
`register_ui_select()` or snacks' `picker.ui_select`).

## Configuration

```lua
require('vulcan').setup({
  binary = 'vulcan',           -- executable for CLI calls
  cmd = nil,                   -- server command; defaults to { binary, 'lsp' }
  root_markers = { '.vulcan', '.vulcan.toml', '.obsidian' },
  init_options = {             -- sent to the server
    -- watcher = 'own',        -- 'own' | 'client' | 'off'
    -- dataviewJs = nil,       -- 'off' | 'static' | 'on'; unset follows `vulcan trust`
  },
  codelens = true,             -- backlink-count lens on each note
  inlay_hints = true,          -- due-date hints on open tasks
  render = {
    enabled = true,
    sources = { 'inline', 'dataview', 'dataviewjs', 'tasks', 'base', 'notice' },
    max_lines = 30,
  },
  keymaps = {                  -- buffer-local in vault notes; false disables one
    notes = '<leader>vn', search = '<leader>vs', tags = '<leader>vt',
    daily = '<leader>vd', template = '<leader>vi',
    toggle_task = '<leader>vx', render = '<leader>vr',
  },
})
```

`dataviewjs` blocks always render in the strict sandbox (computation and index
reads; no file access, network, or writes). Vaults you trust with
`vulcan trust` get full evaluation, which still never writes. Set
`dataviewJs = 'off'` to turn them off, or `'static'` to keep even trusted
vaults sandboxed.

Recommended companions: render-markdown.nvim or markview.nvim for live
Markdown rendering, and image.nvim or snacks.image for images.

## Testing

```sh
nvim --headless -u NONE -l test/smoke.lua /path/to/vulcan
```

`cargo test -p vulcan-cli --test nvim_plugin` runs the same smoke test against
the freshly built binary when Neovim 0.10+ is installed.
