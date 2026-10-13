-- Headless smoke test against a real `vulcan` binary:
--
--   nvim --headless -u NONE -l test/smoke.lua /path/to/vulcan
--
-- Builds a temporary vault, starts the language server through the
-- plugin, and exercises rendering, the CLI wrapper, doctor, task toggling,
-- and periodic notes. Exits non-zero on the first failure.

local binary = assert(_G.arg[1], 'usage: smoke.lua <vulcan binary>')
local plugin_root = vim.fs.dirname(vim.fs.dirname(debug.getinfo(1, 'S').source:sub(2)))
vim.opt.runtimepath:prepend(plugin_root)
vim.cmd.runtime('plugin/vulcan.lua')
vim.cmd('filetype on')

local failures = 0
local function check(condition, message)
  if condition then
    print('ok   ' .. message)
  else
    failures = failures + 1
    print('FAIL ' .. message)
  end
end

local function wait(condition, timeout)
  return vim.wait(timeout or 15000, condition, 50)
end

local vault = vim.fn.tempname()
vim.fn.mkdir(vault, 'p')
local function write(path, lines)
  local file = vim.fs.joinpath(vault, path)
  vim.fn.mkdir(vim.fs.dirname(file), 'p')
  vim.fn.writefile(lines, file)
end
write('Home.md', {
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
})
write('Todo.md', { '- [ ] Open item' })
write('Stale.md', { 'Points at [[Nowhere]].' })
write('Sections.md', { '# Sections', '', '## Part', '', 'Inside' })
write('.vulcan/config.toml', { '[periodic.daily]', 'folder = "Journal"', 'format = "YYYY-MM-DD"' })
local function run(args)
  local result = vim.system(vim.list_extend({ binary, '--vault', vault }, args), { text = true }):wait()
  assert(result.code == 0, table.concat(args, ' ') .. ': ' .. (result.stderr or ''))
end
run({ 'index', 'init', '--no-import' })
run({ 'index', 'scan' })

local vulcan = require('vulcan')
vulcan.setup({ binary = binary, keymaps = {} })
vim.cmd.edit(vim.fs.joinpath(vault, 'Home.md'))
check(wait(function() return vulcan.client() ~= nil end), 'language server attaches')

local render = require('vulcan.render')
local function marks()
  return vim.api.nvim_buf_get_extmarks(0, render.namespace, 0, -1, { details = true })
end
check(wait(function() return #marks() >= 2 end), 'query results are drawn')
local inline, block = false, false
for _, mark in ipairs(marks()) do
  local details = mark[4]
  if details.virt_text and details.virt_text[1][1]:find('draft', 1, true) then
    inline = true
  end
  if details.virt_lines then
    for _, line in ipairs(details.virt_lines) do
      if line[2][1]:find('Open item', 1, true) then
        block = true
      end
    end
  end
end
check(inline, 'inline expression value appears at the end of its line')
check(block, 'tasks block results appear as virtual lines')

local lens = vulcan.client():request_sync('textDocument/codeLens', { textDocument = { uri = vim.uri_from_bufnr(0) } }, 5000, 0)
check(lens and lens.result and lens.result[1] and lens.result[1].command.title == '0 backlinks', 'the backlink code lens is served')
if vim.lsp.codelens.is_enabled then
  check(vim.lsp.codelens.is_enabled({ bufnr = 0 }), 'code lenses are enabled')
end
check(vim.lsp.inlay_hint.is_enabled({ bufnr = 0 }), 'inlay hints are enabled')

check(vim.api.nvim_get_hl(0, { name = '@lsp.type.wikiLink.markdown' }).link == '@markup.link', 'semantic token highlights have defaults')
local see = vim.fn.search('See \\[\\[Missing', 'n')
local function token_types()
  local types = {}
  for _, token in ipairs(vim.inspect_pos(0, see - 1, 6).semantic_tokens) do
    types[token.opts.hl_group] = true
  end
  return types
end
check(wait(function() return token_types()['@lsp.typemod.wikiLink.unresolved.markdown'] end), 'an unresolved link gets a semantic token')

if vim.lsp.buf.workspace_diagnostics then
  require('vulcan.commands').problems()
  local stale = vim.fs.joinpath(vault, 'Stale.md')
  local function stale_problems()
    local bufnr = vim.fn.bufnr(stale)
    return bufnr ~= -1 and #vim.diagnostic.get(bufnr) > 0
  end
  check(wait(stale_problems), 'problems in closed notes reach vim.diagnostic')
end

local listed
require('vulcan.cli').json({ 'ls' }, function(values) listed = values end)
check(wait(function() return listed ~= nil end) and vim.tbl_contains(listed, 'Home.md'), 'CLI wrapper lists notes')

require('vulcan.commands').doctor()
check(wait(function() return #vim.fn.getqflist() > 0 end), 'doctor fills the quickfix list')
vim.cmd.cclose()
vim.cmd.buffer(vim.fn.bufnr(vim.fs.joinpath(vault, 'Home.md')))

vim.api.nvim_win_set_cursor(0, { 12, 0 })
vim.cmd('Vulcan task')
check(wait(function()
  return vim.api.nvim_buf_get_lines(0, 12, 13, false)[1]:find('^%- %[x%]') ~= nil
end), 'toggling completes the task in the buffer')
check(
  vim.api.nvim_buf_get_lines(0, 11, 12, false)[1] == '- [ ] Water plants 🔁 every week 📅 2026-10-19',
  'completing a recurring task inserts its next occurrence'
)
check(vim.fn.readfile(vim.fs.joinpath(vault, 'Home.md'))[12]:find('^%- %[ %]') ~= nil, 'the file on disk is untouched')

vim.cmd('silent! write')

-- Rename a linked note: the closed note moves on disk, the open buffer's
-- link is rewritten through the editor.
local link_line = vim.fn.search('\\[\\[Todo\\]\\]', 'w')
vim.api.nvim_win_set_cursor(0, { link_line, vim.fn.col('.') + 2 })
vim.lsp.buf.rename('Chores', { name = 'vulcan' })
check(wait(function() return vim.uv.fs_stat(vim.fs.joinpath(vault, 'Chores.md')) ~= nil end), 'renaming a link moves the note')
check(wait(function()
  return vim.api.nvim_buf_get_lines(0, link_line - 1, link_line, false)[1]:find('[[Chores]]', 1, true) ~= nil
end), 'the open buffer link is rewritten')
vim.cmd('silent! write')

-- Moving the open note: the editor renames it so the buffer follows.
vim.cmd('Vulcan move archive/Home.md')
local moved = vim.fs.joinpath(vault, 'archive/Home.md')
check(wait(function() return vim.fs.normalize(vim.api.nvim_buf_get_name(0)) == vim.fs.normalize(moved) end), 'moving the open note re-points its buffer')
check(vim.uv.fs_stat(moved) ~= nil and vim.uv.fs_stat(vim.fs.joinpath(vault, 'Home.md')) == nil, 'the note moved on disk')

-- A template's tp.file.cursor() places the cursor.
vim.fn.mkdir(vim.fs.joinpath(vault, '.vulcan/templates'), 'p')
vim.fn.writefile({ '- [ ] <% tp.file.cursor() %>' }, vim.fs.joinpath(vault, '.vulcan/templates/Todo.md'))
vim.cmd('silent! write')
local before_lines = vim.api.nvim_buf_line_count(0)
vim.api.nvim_win_set_cursor(0, { 1, 0 })
require('vulcan.commands').insert_template('Todo')
check(wait(function() return vim.api.nvim_buf_line_count(0) > before_lines end), 'template is inserted')
local placed = wait(function()
  local cursor = vim.api.nvim_win_get_cursor(0)
  -- Normal mode cannot sit past the last character, so column 6 clamps to 5.
  return vim.api.nvim_get_current_line():find('^%- %[ %] $') ~= nil and cursor[2] >= 5
end)
if not placed then
  print(vim.inspect({ cursor = vim.api.nvim_win_get_cursor(0), line = vim.api.nvim_get_current_line(), tail = vim.api.nvim_buf_get_lines(0, -4, -1, false) }))
end
check(placed, 'tp.file.cursor() places the cursor')
vim.cmd('silent! write')

-- The cursor now sits on whitespace, where Neovim's own showDocument
-- handler throws (`expand('<cword>')`); the plugin's handler must not.
local errors_before = #vim.tbl_filter(function(line) return line:find('E348') end, vim.split(vim.fn.execute('messages'), '\n'))
vim.cmd('Vulcan capture remember the milk')
check(wait(function()
  local inbox = vim.fs.joinpath(vault, 'Inbox.md')
  return vim.uv.fs_stat(inbox) ~= nil and table.concat(vim.fn.readfile(inbox), '\n'):find('remember the milk', 1, true) ~= nil
end), 'capture appends to the inbox note')

vim.cmd('Vulcan daily 2026-10-13')
local daily = vim.fs.joinpath(vault, 'Journal/2026-10-13.md')
check(wait(function() return vim.uv.fs_stat(daily) ~= nil end), 'periodic note is created')
check(wait(function() return vim.fs.normalize(vim.api.nvim_buf_get_name(0)) == vim.fs.normalize(daily) end), 'periodic note is opened')
local errors_after = #vim.tbl_filter(function(line) return line:find('E348') end, vim.split(vim.fn.execute('messages'), '\n'))
check(errors_after == errors_before, 'opening a document from whitespace raises no error')

vim.cmd.edit(vim.fs.joinpath(vault, 'Sections.md'))
local sections = vim.api.nvim_get_current_buf()
check(wait(function() return vulcan.client() ~= nil end), 'the server attaches to another note')
vim.api.nvim_win_set_cursor(0, { 3, 0 })
vim.cmd('Vulcan extract')
local part = vim.fs.joinpath(vault, 'Part.md')
check(wait(function()
  return vim.uv.fs_stat(part) ~= nil and vim.api.nvim_buf_get_lines(sections, 2, 3, false)[1] == '[[Part]]'
end), 'a section is extracted into a new note and replaced by a link')
vim.api.nvim_buf_call(sections, function() vim.cmd('silent! write') end)

vim.cmd('checkhealth vulcan')
local report = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), '\n')
check(report:find('Custom protocol version 1', 1, true) ~= nil, 'checkhealth finds the running server')
check(report:find('ERROR', 1, true) == nil, 'checkhealth reports no errors')

for _, client in ipairs(vim.lsp.get_clients({ name = 'vulcan' })) do
  client:stop()
end
vim.fn.delete(vault, 'rf')
if failures > 0 then
  print(('%d check(s) failed'):format(failures))
  vim.cmd.cquit(1)
end
print('all checks passed')
