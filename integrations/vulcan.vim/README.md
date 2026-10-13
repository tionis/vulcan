# vulcan.vim

Vim integration for [Vulcan](../../README.md) vaults: a thin Vim9script layer
over the `vulcan lsp` language server, through
[yegappan/lsp](https://github.com/yegappan/lsp), and `vulcan --output json`.
Vault semantics (link resolution, tasks, templates, periodic notes) stay in
Vulcan; this plugin registers the server, adds commands and pickers, and
draws query results.

Requires Vim 9.0.0067 or newer, yegappan/lsp, and a `vulcan` binary on
`PATH`. The Neovim counterpart is
[vulcan.nvim](../vulcan.nvim/README.md).

## Install

Install yegappan/lsp the way you install plugins, then the copy of this
plugin bundled with your `vulcan` binary:

```sh
vulcan editor vim install
```

This writes it into Vim's package directory
(`~/.vim/pack/vulcan/start/vulcan.vim`, or under `~/.config/vim` when Vim
uses that), where Vim loads it at startup; nothing else is needed in your
vimrc. `vulcan editor vim status` tells you whether the copy is current,
`self-update` refreshes it, and `:Vulcan health` warns when it is stale.
Edited copies are left alone unless you pass `--force`;
`vulcan editor vim install --dir <path>` installs anywhere else.

The plugin registers the server with yegappan/lsp itself. If you already
call `g:LspAddServer()` for `vulcan lsp`, set
`g:vulcan_register_server = false`.

## What you get

From the language server, in any Markdown buffer inside a vault (a folder with
`.vulcan/`, `.vulcan.toml`, or `.obsidian/`), through yegappan/lsp's commands:

- go to definition on links, embeds, `#heading` and `#^block` subpaths
  (`:LspGotoDefinition`), references (`:LspShowReferences`), hover previews
- completion after `[[`, `#`, `^`, and in frontmatter
- diagnostics for unresolved and ambiguous links and frontmatter problems
- link-safe rename of notes, headings, and block ids (`:LspRename`)
- code actions (`:LspCodeAction`): create a missing note, pick an ambiguous
  link's target, convert link style, complete or reopen a task, extract a
  heading's section into a new note
- a backlink count (`:LspCodeLens`) and due-date inlay hints
  (`:LspInlayHints enable`, or yegappan/lsp's `showInlayHints` option)

From the plugin:

- query results (inline expressions, Dataview, Tasks, Bases) as virtual text
  beside their source, refreshed on save and whenever the index changes
- `:Vulcan` commands and pickers (below)
- buffers that follow notes moved through Vulcan
- `:Vulcan health`

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
| `:Vulcan extract[!]` | Extract the section of the heading on the cursor line into a new note, leaving a link (`!` opens the new note) |
| `:Vulcan scan[!]` | Rescan the vault (`!` rebuilds) |
| `:Vulcan render` | Toggle query results for this buffer |
| `:Vulcan doctor` (or `problems`) | Load `vulcan doctor` problems into the quickfix list |
| `:Vulcan status` | Show the server's cache and watcher state |
| `:Vulcan health` | Check the setup |
| `:Vulcan browse`, `bases [file]`, `kanban [file]` | Open Vulcan's TUIs in a terminal popup |

Pickers use fzf.vim when it is installed (set `g:vulcan_picker = 'popup'` to
use Vim's popup menu instead).

## Configuration

Set `g:vulcan` before the server starts (any key can be left out):

```vim
vim9script
g:vulcan = {
  binary: 'vulcan',
  root_markers: ['.vulcan/', '.vulcan.toml', '.obsidian/'],
  init_options: {},      # watcher: 'own'|'client'|'off'; dataviewJs: 'off'|'static'|'on'
  render: {enabled: true, max_lines: 30,
           sources: ['inline', 'dataview', 'dataviewjs', 'tasks', 'base', 'notice']},
  keymaps: {notes: '<Leader>vn', search: '<Leader>vs', tags: '<Leader>vt',
            daily: '<Leader>vd', template: '<Leader>vi', task: '<Leader>vx',
            render: '<Leader>vr'},   # '' disables one
}
```

`dataviewjs` blocks render in the strict sandbox, with full evaluation in
vaults trusted with `vulcan trust`.

## Limitations

- One vault per Vim session: yegappan/lsp starts one server per
  registration, so notes from a second vault reach the first vault's server,
  which ignores them. The plugin warns when that happens.
- Semantic highlighting is not drawn: yegappan/lsp only maps standard token
  types, and Vulcan's are custom.
- Workspace-wide diagnostics are not available in yegappan/lsp;
  `:Vulcan problems` loads `vulcan doctor` instead.

## Testing

```sh
VULCAN_BIN=/path/to/vulcan VULCAN_VIM_LSP=/path/to/yegappan/lsp \
VULCAN_SMOKE_LOG=/tmp/vulcan-vim.log vim -Nu NONE -i NONE -es -S test/smoke.vim
```

`cargo test -p vulcan-cli --test vim_plugin` runs it against the freshly built
binary when Vim 9 is installed and `VULCAN_VIM_LSP` points at yegappan/lsp.
