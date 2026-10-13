vim9script
# vulcan.vim: a thin Vim layer over `vulcan lsp` (through yegappan/lsp) and
# `vulcan --output json`.
#
# Vault semantics (links, tasks, templates, periodic dates, frontmatter)
# live in Vulcan. This plugin only registers the language server, wires
# commands to it or to the CLI, and draws what they return.

import autoload 'lsp/buffer.vim' as lspbuf
import autoload 'lsp/util.vim' as lsputil
import autoload 'vulcan/cli.vim'
import autoload 'vulcan/health.vim'
import autoload 'vulcan/pickers.vim'
import autoload 'vulcan/render.vim'

export const SERVER = 'vulcan'

# Options; override any of them in `g:vulcan` before the server starts.
export const defaults = {
  binary: 'vulcan',
  root_markers: ['.vulcan/', '.vulcan.toml', '.obsidian/'],
  filetypes: ['markdown'],
  # Sent as `initializationOptions`: `watcher` ('own', 'client', 'off') and
  # `dataviewJs` ('off', 'static', 'on'; unset follows `vulcan trust`).
  init_options: {},
  render: {
    enabled: true,
    sources: ['inline', 'dataview', 'dataviewjs', 'tasks', 'base', 'notice'],
    max_lines: 30,
  },
  popup: {width: 0.9, height: 0.85},
  # Buffer-local normal-mode mappings in vault notes; '' disables one.
  keymaps: {
    notes: '<Leader>vn',
    search: '<Leader>vs',
    tags: '<Leader>vt',
    daily: '<Leader>vd',
    template: '<Leader>vi',
    task: '<Leader>vx',
    render: '<Leader>vr',
  },
}

export var config: dict<any> = deepcopy(defaults)
var registered = false
var warned_roots: dict<bool> = {}

def Merge(base: dict<any>, overrides: dict<any>): dict<any>
  var merged = deepcopy(base)
  for [key, value] in items(overrides)
    if type(value) == v:t_dict && type(get(merged, key, 0)) == v:t_dict
      merged[key] = Merge(merged[key], value)
    else
      merged[key] = value
    endif
  endfor
  return merged
enddef

export def Warn(message: string)
  echohl WarningMsg
  echomsg $'vulcan: {message}'
  echohl None
enddef

export def Error(message: string)
  echohl ErrorMsg
  echomsg $'vulcan: {message}'
  echohl None
enddef

# Register `vulcan lsp` with yegappan/lsp. Runs on its `User LspSetup`
# event; `g:vulcan_register_server = false` leaves registration to you.
export def Register()
  config = Merge(defaults, get(g:, 'vulcan', {}))
  if registered || !get(g:, 'vulcan_register_server', true)
    return
  endif
  if !exists('*g:LspAddServer')
    Warn('yegappan/lsp is not installed; see :help vulcan-setup')
    return
  endif
  g:LspAddServer([{
    name: SERVER,
    filetype: config.filetypes,
    path: config.binary,
    args: ['lsp'],
    rootSearch: config.root_markers,
    # Only start inside a vault.
    runIfSearch: config.root_markers,
    initializationOptions: config.init_options,
    customNotificationHandlers: {
      'vulcan/indexChanged': (_, notification) => IndexChanged(notification.params),
    },
  }])
  # The backlink-count code lens runs this client-side command.
  g:LspRegisterCmdHandler('vulcan.showBacklinks', (_) => {
    execute 'LspShowReferences'
  })
  registered = true
enddef

# The Vulcan server for the current buffer, or {} when none is attached.
export def Server(): dict<any>
  if !exists('*g:LspAddServer')
    return {}
  endif
  return lspbuf.CurbufGetServerByName(SERVER)
enddef

# The vault root the current buffer's server serves, else the nearest root
# marker, else the working directory. No trailing separator.
export def VaultRoot(): string
  var server = Server()
  var root = ''
  if !server->empty() && !get(server, 'workspaceFolders', [])->empty()
    root = server.workspaceFolders[0]
  else
    root = FindRoot(expand('%:p:h'))
  endif
  return root->substitute('[/\\]$', '', '')
enddef

def FindRoot(start: string): string
  var directory = start->empty() ? getcwd() : start
  while true
    for marker in config.root_markers
      if !glob(directory .. '/' .. marker->substitute('[/\\]$', '', ''), true)->empty()
        return directory
      endif
    endfor
    var parent = fnamemodify(directory, ':h')
    if parent == directory
      return getcwd()
    endif
    directory = parent
  endwhile
  return getcwd()
enddef

export def Uri(bnr: number = bufnr()): string
  return lsputil.LspBufnrToUri(bnr)
enddef

export def JoinPath(root: string, path: string): string
  return root .. '/' .. path
enddef

# Send a request to the current buffer's server. `Callback` receives the
# result and the error (empty on success).
export def Request(method: string, params: any, Callback: func(any, dict<any>)): bool
  var server = Server()
  if server->empty()
    Warn('no language server attached to this buffer')
    return false
  endif
  # yegappan/lsp reports buffer changes from a listener that runs on the
  # next redraw; send pending ones first so the server sees this text.
  listener_flush()
  server.rpc_a(method, params, (_, result, error) => Callback(result, error))
  return true
enddef

# Run a `vulcan.*` command through `workspace/executeCommand`.
export def Execute(command: string, argument: dict<any>, Callback: func(any) = null_function)
  Request('workspace/executeCommand', {command: command, arguments: [argument]}, (result, error) => {
    if !error->empty()
      Error(get(error, 'message', string(error)))
    elseif Callback != null_function
      Callback(result)
    endif
  })
enddef

# `vulcan/indexChanged`: re-point buffers of notes the server moved, then
# redraw query results.
def IndexChanged(params: dict<any>)
  var renames = get(params, 'renames', [])
  if !renames->empty()
    Repoint(renames)
  endif
  render.RefreshVisible()
enddef

# yegappan/lsp applies a `RenameFile` by renaming the file on disk only, so
# the buffer of a moved note still names the old path. Write it to the new
# path (the server never moves a note with unsaved changes, so it holds only
# the server's link rewrites), edit it there, and wipe the old buffer, so
# yegappan/lsp closes the old document and opens the new one.
def Repoint(renames: list<dict<any>>)
  var root = VaultRoot()
  for move in renames
    var from = fnamemodify(JoinPath(root, move.from), ':p')
    var to = fnamemodify(JoinPath(root, move.to), ':p')
    for info in getbufinfo()
      if info.name->empty() || fnamemodify(info.name, ':p') != from || filereadable(from)
        continue
      endif
      var origin = win_getid()
      var winid = bufwinid(info.bufnr)
      var previous = bufnr()
      if winid != -1
        win_gotoid(winid)
      else
        execute $'silent keepalt keepjumps buffer {info.bufnr}'
      endif
      var view = winsaveview()
      execute $'silent write! {fnameescape(to)}'
      execute $'silent keepalt edit {fnameescape(to)}'
      winrestview(view)
      execute $'silent bwipeout {info.bufnr}'
      if winid == -1 && previous != info.bufnr && bufexists(previous)
        execute $'silent keepalt keepjumps buffer {previous}'
      endif
      win_gotoid(origin)
    endfor
  endfor
enddef

# `User LspAttached`: set up a vault note.
export def Attached()
  var context = get(g:, 'LspAttachedContext', {})
  if index(get(context, 'servers', []), SERVER) < 0
    return
  endif
  var bnr: number = context.bufnr
  if bnr == bufnr()
    Adopt(bnr)
  else
    execute $'autocmd BufEnter <buffer={bnr}> ++once vulcan#Adopt({bnr})'
  endif
enddef

export def Adopt(bnr: number)
  if getbufvar(bnr, 'vulcan_adopted', false)
    return
  endif
  setbufvar(bnr, 'vulcan_adopted', true)
  for [name, lhs] in items(config.keymaps)
    if !empty(lhs) && has_key(MAPPED, name)
      execute $'nnoremap <buffer> <silent> {lhs} <Cmd>Vulcan {MAPPED[name]}<CR>'
    endif
  endfor
  render.Attach(bnr)
  # yegappan/lsp starts one server per registration: notes from a second
  # vault reach the first vault's server, which ignores them.
  var root = FindRoot(expand('%:p:h'))
  var served = VaultRoot()
  if fnamemodify(root, ':p') != fnamemodify(served, ':p') && !has_key(warned_roots, root)
    warned_roots[root] = true
    Warn($'{root} is a different vault from {served}; restart Vim in it to edit it with Vulcan')
  endif
enddef

const MAPPED = {
  notes: 'notes',
  search: 'search',
  tags: 'tags',
  daily: 'daily',
  template: 'template',
  task: 'task',
  render: 'render',
}

# Open a vault-relative path, optionally at a 1-based line.
export def Open(root: string, path: string, line: number = 0)
  # `:hide` keeps an unsaved buffer (such as the source of an extracted
  # section) instead of refusing when 'hidden' is off.
  execute $'hide edit {fnameescape(JoinPath(root, path))}'
  if line > 0
    cursor(line, 1)
  endif
enddef

# ---------------------------------------------------------------------------
# Subcommands

def Periodic(period: string, date: string = '')
  var root = VaultRoot()
  var argument: dict<any> = {period: period, uri: Uri()}
  if !date->empty()
    argument.date = date
  endif
  Execute('vulcan.periodic.open', argument, (result) => {
    var path = get(get(result, 'report', {}), 'path', '')
    if !path->empty()
      Open(root, path)
    endif
  })
enddef

export def InsertTemplate(name: string)
  var server = Server()
  if server->empty()
    Warn('no language server attached to this buffer')
    return
  endif
  var bnr = bufnr()
  Execute('vulcan.template.insert', {
    path: Uri(),
    template: name,
    position: server.getPosition(false),
  }, (result) => {
    # Place the cursor where the template called `tp.file.cursor()`.
    var cursor = get(get(result, 'report', {}), 'cursor', v:null)
    if type(cursor) == v:t_dict && bufnr() == bnr
      var position = copy(cursor)
      server.decodePosition(bnr, position)
      setcursorcharpos(position.line + 1, position.character + 1)
    endif
  })
enddef

def ToggleTask()
  Execute('vulcan.task.toggle', {path: Uri(), line: line('.') - 1})
enddef

def Extract(open: bool)
  var root = VaultRoot()
  Execute('vulcan.section.extract', {
    path: Uri(),
    position: {line: line('.') - 1, character: 0},
  }, (result) => {
    var destination = get(get(result, 'report', {}), 'destination', '')
    if destination->empty()
      return
    endif
    if open
      Open(root, destination)
    else
      echomsg $'vulcan: extracted to {destination}'
    endif
  })
enddef

def Capture(text: string)
  var value = text->empty() ? input('Capture: ') : text
  if value->empty()
    return
  endif
  Execute('vulcan.capture', {text: value, uri: Uri()}, (result) => {
    var path = get(get(result, 'report', {}), 'path', '')
    if !path->empty()
      echomsg $'vulcan: captured to {path}'
    endif
  })
enddef

def Move(destination: string)
  var to = destination
  if to->empty()
    var root = VaultRoot()
    var current = expand('%:p')
    var relative = current->stridx(root .. '/') == 0 ? current[len(root) + 1 :] : current
    to = input('Move note to: ', relative)
  endif
  if !to->empty()
    Execute('vulcan.note.move', {from: Uri(), to: to})
  endif
enddef

def Scan(full: bool)
  Execute('vulcan.scan', {full: full}, (result) => {
    var summary = get(result, 'summary', {})
    if type(summary) == v:t_dict && !summary->empty()
      echomsg printf('vulcan: %d added, %d updated, %d deleted', summary.added, summary.updated, summary.deleted)
    endif
  })
enddef

export def Doctor()
  var root = VaultRoot()
  cli.JsonOrWarn(['doctor'], (reports) => {
    var entries = []
    var report = get(reports, 0, {})
    for [kind, issues] in items(type(report) == v:t_dict ? report : {})
      if kind == 'summary' || type(issues) != v:t_list
        continue
      endif
      for issue in issues
        if type(issue) == v:t_dict && type(get(issue, 'document_path', v:null)) == v:t_string
          var line = get(issue, 'line', v:null)
          var text = $'[{kind}] ' .. get(issue, 'message', '')
          var lnum = type(line) == v:t_number ? line : 1
          entries->add({filename: JoinPath(root, issue.document_path), lnum: lnum, text: text})
        endif
      endfor
    endfor
    entries->sort((left, right) => left.filename < right.filename ? -1 : left.filename > right.filename ? 1 : 0)
    setqflist([], ' ', {title: 'vulcan doctor', items: entries})
    if entries->empty()
      echomsg 'vulcan doctor: no problems'
    else
      copen
    endif
  }, root)
enddef

def Status()
  Request('vulcan/status', {textDocument: {uri: Uri()}}, (result, error) => {
    if !error->empty()
      Error(get(error, 'message', string(error)))
      return
    endif
    echomsg $'vulcan: {result.vault}, cache {result.cache}, watcher {result.watcher}'
  })
enddef

# Run a Vulcan TUI in a terminal popup.
def Terminal(args: list<string>)
  var width = float2nr(&columns * config.popup.width)
  var height = float2nr(&lines * config.popup.height)
  var command = [config.binary, '--vault', VaultRoot()] + args
  var buffer = term_start(command, {hidden: true, term_finish: 'close', term_cols: width, term_rows: height})
  popup_create(buffer, {minwidth: width, minheight: height, border: []})
enddef

const SUBCOMMANDS = [
  'backlinks', 'bases', 'browse', 'capture', 'daily', 'doctor', 'extract', 'health', 'kanban',
  'monthly', 'move', 'notes', 'periodic', 'problems', 'render', 'scan', 'search', 'status',
  'tags', 'task', 'template', 'weekly',
]

# `:Vulcan {subcommand} [args]`. A bang may follow the subcommand
# (`:Vulcan scan!`) or the command (`:Vulcan! scan`).
export def Dispatch(arguments: string, command_bang: bool)
  var words = arguments->split()
  var name = get(words, 0, '')
  var bang = command_bang
  if name =~ '!$'
    name = name[: -2]
    bang = true
  endif
  var args = words[1 :]
  var rest = args->join(' ')
  var file = get(args, 0, expand('%:p'))
  if name == 'notes'
    pickers.Notes()
  elseif name == 'search'
    pickers.Search(rest)
  elseif name == 'tags'
    pickers.Tags()
  elseif name == 'backlinks'
    execute 'LspShowReferences'
  elseif name == 'daily' || name == 'weekly' || name == 'monthly'
    Periodic(name, get(args, 0, ''))
  elseif name == 'periodic'
    Periodic(get(args, 0, 'daily'), get(args, 1, ''))
  elseif name == 'template'
    if args->empty()
      pickers.Template()
    else
      InsertTemplate(args[0])
    endif
  elseif name == 'task'
    ToggleTask()
  elseif name == 'capture'
    Capture(rest)
  elseif name == 'move'
    Move(get(args, 0, ''))
  elseif name == 'extract'
    Extract(bang)
  elseif name == 'scan'
    Scan(bang)
  elseif name == 'render'
    render.Toggle()
  elseif name == 'doctor' || name == 'problems'
    # yegappan/lsp has no workspace diagnostics; doctor covers every note.
    Doctor()
  elseif name == 'status'
    Status()
  elseif name == 'health'
    health.Show()
  elseif name == 'browse'
    Terminal(['browse'])
  elseif name == 'bases'
    Terminal(['bases', 'tui', file])
  elseif name == 'kanban'
    Terminal(['kanban', 'show', file])
  else
    Error($'unknown subcommand {name}')
  endif
enddef

export def Complete(lead: string, line: string, _: number): list<string>
  if line =~ '^\S\+\s\+\S*$'
    return SUBCOMMANDS->copy()->filter((_, name) => name->stridx(lead) == 0)
  endif
  return []
enddef
