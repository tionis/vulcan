vim9script
# Headless smoke test against a real `vulcan` binary and yegappan/lsp:
#
#   VULCAN_BIN=/path/to/vulcan VULCAN_VIM_LSP=/path/to/yegappan/lsp \
#   VULCAN_SMOKE_LOG=/tmp/log vim -Nu NONE -i NONE -es -S test/smoke.vim
#
# Builds a temporary vault, starts the language server through the
# plugin, and exercises rendering, the CLI wrapper, doctor, task toggling,
# renames and moves, templates, capture, periodic notes, section
# extraction, and health. The log ends with `all checks passed`, and Vim
# exits non-zero when a check fails.

const BINARY = $VULCAN_BIN
const LOG = $VULCAN_SMOKE_LOG
const PLUGIN_ROOT = expand('<sfile>:p:h:h')

var failures = 0

def Log(line: string)
  writefile([line], LOG, 'a')
enddef

def Check(condition: bool, message: string)
  if condition
    Log($'ok   {message}')
  else
    failures += 1
    Log($'FAIL {message}')
  endif
enddef

# Wait (processing server messages) until `Condition` holds.
def Wait(Condition: func, timeout: number = 15000): bool
  var waited = 0
  while waited < timeout
    if Condition()
      return true
    endif
    # Headless Vim never redraws, which is when yegappan/lsp's change
    # listeners would report edits; flush them as a redraw would.
    for info in getbufinfo({bufloaded: true})
      listener_flush(info.bufnr)
    endfor
    sleep 50m
    waited += 50
  endwhile
  return Condition()
enddef

var vault = tempname()
def Write(path: string, lines: list<string>)
  var file = $'{vault}/{path}'
  mkdir(fnamemodify(file, ':h'), 'p')
  writefile(lines, file)
enddef

def Run(args: list<string>)
  var output = system(([BINARY, '--vault', vault] + args)->map((_, arg) => shellescape(arg))->join(' '))
  if v:shell_error != 0
    throw $'{args->join(" ")}: {output}'
  endif
enddef

def Lines(): list<string>
  return getline(1, '$')
enddef

def Main()
  mkdir(vault, 'p')
  Write('Home.md', [
    '---',
    'status: draft',
    '---',
    '# Home',
    '',
    'Status: `= this.status`',
    '',
    '```tasks',
    'not done',
    '```',
    '',
    '- [ ] Water plants 🔁 every week 📅 2026-10-12',
    '',
    'See [[Missing]] and [[Todo]].',
  ])
  Write('Todo.md', ['- [ ] Open item'])
  Write('Sections.md', ['# Sections', '', '## Part', '', 'Inside'])
  Write('.vulcan/config.toml', ['[periodic.daily]', 'folder = "Journal"', 'format = "YYYY-MM-DD"'])
  Run(['index', 'init', '--no-import'])
  Run(['index', 'scan'])

  execute $'edit {fnameescape(vault .. "/Home.md")}'
  Check(Wait(() => !vulcan#Server()->empty() && vulcan#Server().ready), 'language server attaches')
  Check(Wait(() => get(b:, 'vulcan_adopted', false)), 'the plugin adopts the note')

  var Props = (type: string) => prop_type_get(type)->empty() ? [] : prop_list(1, {end_lnum: -1, types: [type]})
  Check(Wait(() => !Props('vulcan_render_inline')->empty()), 'inline expression value appears after its line')
  Check(Props('vulcan_render_inline')->get(0, {})->get('text', '') =~ 'draft', 'the inline value is evaluated')
  Check(Wait(() => Props('vulcan_render_block')->copy()->filter((_, prop) => prop.text =~ 'Open item')->len() > 0), 'tasks block results appear below the block')

  var listed: list<any> = []
  var done = false
  vulcan#cli#Json(['ls'], (values, _) => {
    listed = values
    done = true
  })
  Check(Wait(() => done) && index(listed, 'Home.md') >= 0, 'CLI wrapper lists notes')

  vulcan#Doctor()
  Check(Wait(() => !getqflist()->empty()), 'doctor fills the quickfix list')
  cclose
  execute $'buffer {bufnr(vault .. "/Home.md")}'

  cursor(12, 1)
  Vulcan task
  Check(Wait(() => getline(13) =~ '^- \[x\]'), 'toggling completes the task in the buffer')
  Check(getline(12) == '- [ ] Water plants 🔁 every week 📅 2026-10-19', 'completing a recurring task inserts its next occurrence')
  Check(readfile(vault .. '/Home.md')[11] =~ '^- \[ \]', 'the file on disk is untouched')
  silent write

  # Rename a linked note: the closed note moves on disk, the open buffer's
  # link is rewritten through the editor.
  var link_line = search('\[\[Todo\]\]', 'w')
  cursor(link_line, col('.') + 2)
  LspRename Chores
  Check(Wait(() => filereadable(vault .. '/Chores.md')), 'renaming a link moves the note')
  Check(Wait(() => getline(link_line) =~ '\[\[Chores\]\]'), 'the open buffer link is rewritten')
  silent write

  # Moving the open note: yegappan/lsp renames the file, and the plugin
  # re-points the buffer from `vulcan/indexChanged`.
  var moved = vault .. '/archive/Home.md'
  Vulcan move archive/Home.md
  Check(Wait(() => fnamemodify(bufname(), ':p') == fnamemodify(moved, ':p')), 'moving the open note re-points its buffer')
  Check(filereadable(moved) && !filereadable(vault .. '/Home.md'), 'the note moved on disk')
  Check(!&modified && readfile(moved)->join("\n") =~ 'Chores', 'the moved buffer is saved with its rewrites')

  # A template's tp.file.cursor() places the cursor.
  Write('.vulcan/templates/Todo.md', ['- [ ] <% tp.file.cursor() %>'])
  var before = line('$')
  cursor(1, 1)
  vulcan#InsertTemplate('Todo')
  Check(Wait(() => line('$') > before), 'template is inserted')
  Check(Wait(() => getline('.') =~ '^- \[ \] $' && charcol('.') >= 6), 'tp.file.cursor() places the cursor')
  silent write

  Vulcan capture remember the milk
  Check(Wait(() => filereadable(vault .. '/Inbox.md') && readfile(vault .. '/Inbox.md')->join("\n") =~ 'remember the milk'), 'capture appends to the inbox note')

  Vulcan daily 2026-10-13
  var daily = vault .. '/Journal/2026-10-13.md'
  Check(Wait(() => filereadable(daily)), 'periodic note is created')
  Check(Wait(() => fnamemodify(bufname(), ':p') == fnamemodify(daily, ':p')), 'periodic note is opened')

  execute $'edit {fnameescape(vault .. "/Sections.md")}'
  Check(Wait(() => !vulcan#Server()->empty() && get(b:, 'vulcan_adopted', false)), 'the server attaches to another note')
  cursor(3, 1)
  Vulcan extract!
  var part = vault .. '/Part.md'
  Check(Wait(() => filereadable(part) && fnamemodify(bufname(), ':p') == fnamemodify(part, ':p')), 'extract! creates and opens the new note')
  Check(getbufline(bufnr(vault .. '/Sections.md'), 3)->get(0, '') == '[[Part]]', 'the section is replaced by a link')
  silent! wall

  Vulcan scan!
  Check(Wait(() => execute('messages') =~ 'added'), ':Vulcan scan! reports its summary')

  var checks = vulcan#health#Checks()
  Check(checks->copy()->filter((_, check) => check[0] == 'ERROR')->empty(), 'health reports no errors: ' .. string(checks))
  Check(checks->copy()->filter((_, check) => check[1] =~ 'Custom protocol version 1')->len() == 1, 'health finds the running server')
enddef

def Finish()
  try
    Main()
  catch
    failures += 1
    Log($'FAIL {v:exception} at {v:throwpoint}')
  endtry
  delete(vault, 'rf')
  if failures > 0
    Log('messages: ' .. execute('messages'))
    Log($'{failures} check(s) failed')
    cquit 1
  endif
  Log('all checks passed')
  qall!
enddef

# Load yegappan/lsp and the plugin like a package would, then run once
# Vim has started (yegappan/lsp enables itself on VimEnter).
execute $'set runtimepath^={fnameescape($VULCAN_VIM_LSP)}'
execute $'set runtimepath^={fnameescape(PLUGIN_ROOT)}'
filetype on
runtime plugin/lsp.vim
runtime plugin/vulcan.vim
g:vulcan = {binary: BINARY}
# Nested, so editing notes fires filetype detection and yegappan/lsp's events.
autocmd VimEnter * ++once ++nested Finish()
