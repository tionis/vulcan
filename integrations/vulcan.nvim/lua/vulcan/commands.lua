-- `:Vulcan` subcommands. Each one asks the language server or the CLI;
-- none of them interprets vault content itself.

local vulcan = require('vulcan')
local cli = require('vulcan.cli')
local M = {}

local function current_uri()
  return vim.uri_from_bufnr(0)
end

--- Open (creating from its template when missing) a periodic note.
--- @param period string `daily`, `weekly`, `monthly`, ...
--- @param date? string `YYYY-MM-DD` or another date the CLI understands
function M.periodic(period, date)
  -- `uri` names the vault when the editor has several open.
  vulcan.execute('vulcan.periodic.open', { period = period, date = date, uri = current_uri() }, function(result)
    -- Editors without `window/showDocument` get no automatic jump.
    local path = result and result.report and result.report.path
    if path and vim.fs.normalize(vim.api.nvim_buf_get_name(0)) ~= vim.fs.normalize(vim.fs.joinpath(vulcan.vault_root(), path)) then
      require('vulcan.pickers').open(vulcan.vault_root(), path)
    end
  end)
end

--- Insert a template at the cursor.
--- @param name string
function M.insert_template(name)
  local client = vulcan.client()
  if not client then
    vim.notify('vulcan: no language server attached to this buffer', vim.log.levels.WARN)
    return
  end
  -- Positions use the encoding negotiated with the server.
  local params = vim.lsp.util.make_position_params(0, client.offset_encoding)
  local bufnr = vim.api.nvim_get_current_buf()
  vulcan.execute('vulcan.template.insert', {
    path = current_uri(),
    template = name,
    position = params.position,
  }, function(result)
    -- Place the cursor where the template called `tp.file.cursor()`.
    local cursor = result and result.report and result.report.cursor
    if cursor and cursor ~= vim.NIL and vim.api.nvim_get_current_buf() == bufnr then
      M.set_cursor(cursor, client.offset_encoding)
    end
  end)
end

--- Move the cursor to an LSP position in the current buffer.
--- @param position { line: integer, character: integer }
--- @param encoding string
function M.set_cursor(position, encoding)
  local line = vim.api.nvim_buf_get_lines(0, position.line, position.line + 1, false)[1] or ''
  local column
  if vim.fn.has('nvim-0.11') == 1 then
    column = vim.str_byteindex(line, encoding, position.character, false)
  else
    ---@diagnostic disable-next-line: param-type-mismatch
    column = vim.str_byteindex(line, position.character, encoding == 'utf-16')
  end
  pcall(vim.api.nvim_win_set_cursor, 0, { position.line + 1, column })
end

--- Complete or reopen the task on the cursor line.
function M.toggle_task()
  vulcan.execute('vulcan.task.toggle', {
    path = current_uri(),
    line = vim.api.nvim_win_get_cursor(0)[1] - 1,
  })
end

--- Extract the section of the heading on the cursor line into a new note.
function M.extract()
  vulcan.execute('vulcan.section.extract', {
    path = current_uri(),
    position = { line = vim.api.nvim_win_get_cursor(0)[1] - 1, character = 0 },
  }, function(result)
    local destination = result and result.report and result.report.destination
    if destination then
      vim.notify('vulcan: extracted to ' .. destination)
    end
  end)
end

--- Capture text into the configured inbox note.
--- @param text? string Prompted for when omitted.
function M.capture(text)
  local function run(value)
    if value and value ~= '' then
      vulcan.execute('vulcan.capture', { text = value, uri = current_uri() }, function(result)
        local path = result and result.report and result.report.path
        if path then
          vim.notify('vulcan: captured to ' .. path)
        end
      end)
    end
  end
  if text then
    run(text)
  else
    vim.ui.input({ prompt = 'Capture: ' }, run)
  end
end

--- Move the current note (link-safe) to a vault path.
--- @param destination? string
function M.move(destination)
  local function run(to)
    if to and to ~= '' then
      vulcan.execute('vulcan.note.move', { from = current_uri(), to = to })
    end
  end
  if destination then
    run(destination)
  else
    local current = vim.fs.relpath and vim.fs.relpath(vulcan.vault_root(), vim.api.nvim_buf_get_name(0))
    vim.ui.input({ prompt = 'Move note to: ', default = current }, run)
  end
end

--- Rescan the vault; `full` rebuilds every document.
--- @param full boolean
function M.scan(full)
  vulcan.execute('vulcan.scan', { full = full }, function(result)
    local summary = result and result.summary
    if summary then
      vim.notify(('vulcan: %d added, %d updated, %d deleted'):format(summary.added, summary.updated, summary.deleted))
    end
  end)
end

--- Load `vulcan doctor` problems into the quickfix list.
function M.doctor()
  local root = vulcan.vault_root()
  cli.json_or_notify({ 'doctor' }, function(reports)
    local items = {}
    for kind, issues in pairs(reports[1] or {}) do
      if kind ~= 'summary' and type(issues) == 'table' and vim.islist(issues) then
        for _, issue in ipairs(issues) do
          if type(issue) == 'table' and issue.document_path then
            items[#items + 1] = {
              filename = vim.fs.joinpath(root, issue.document_path),
              lnum = issue.line or 1,
              text = ('[%s] %s'):format(kind, issue.message or ''),
            }
          end
        end
      end
    end
    table.sort(items, function(left, right)
      return left.filename < right.filename
    end)
    vim.fn.setqflist({}, ' ', { title = 'vulcan doctor', items = items })
    if #items == 0 then
      vim.notify('vulcan doctor: no problems')
    else
      vim.cmd.copen()
    end
  end, root)
end

--- Load problems in every note into |vim.diagnostic| from the language
--- server. They stay current as the index changes. Before Neovim 0.12 this
--- falls back to |M.doctor()|.
function M.problems()
  local client = vulcan.client()
  if not client then
    vim.notify('vulcan: no language server attached to this buffer', vim.log.levels.WARN)
    return
  end
  if not vim.lsp.buf.workspace_diagnostics then
    vim.notify('vulcan: workspace diagnostics need Neovim 0.12; using :Vulcan doctor', vim.log.levels.WARN)
    M.doctor()
    return
  end
  vim.lsp.buf.workspace_diagnostics({ client_id = client.id })
end

--- Show the server's status.
function M.status()
  vulcan.request('vulcan/status', { textDocument = { uri = current_uri() } }, function(err, result)
    if err then
      vim.notify('vulcan: ' .. err.message, vim.log.levels.ERROR)
      return
    end
    vim.notify(('vulcan: %s, cache %s, watcher %s'):format(result.vault, result.cache, result.watcher))
  end)
end

--- Run a Vulcan TUI in a floating terminal.
--- @param args string[]
function M.float(args)
  local float = vulcan.config.float
  local width = math.floor(vim.o.columns * float.width)
  local height = math.floor(vim.o.lines * float.height)
  local buf = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_open_win(buf, true, {
    relative = 'editor',
    width = width,
    height = height,
    col = math.floor((vim.o.columns - width) / 2),
    row = math.floor((vim.o.lines - height) / 2),
    border = float.border,
    style = 'minimal',
  })
  local cmd = { vulcan.config.binary, '--vault', vulcan.vault_root() }
  vim.list_extend(cmd, args)
  local on_exit = function()
    vim.schedule(function()
      if vim.api.nvim_buf_is_valid(buf) then
        vim.api.nvim_buf_delete(buf, { force = true })
      end
    end)
  end
  if vim.fn.has('nvim-0.11') == 1 then
    vim.fn.jobstart(cmd, { term = true, on_exit = on_exit })
  else
    vim.fn.termopen(cmd, { on_exit = on_exit })
  end
  vim.cmd.startinsert()
end

local subcommands = {
  notes = function() require('vulcan.pickers').notes() end,
  search = function(args) require('vulcan.pickers').search(#args > 0 and table.concat(args, ' ') or nil) end,
  tags = function() require('vulcan.pickers').tags() end,
  backlinks = function() vim.lsp.buf.references({ includeDeclaration = false }) end,
  daily = function(args) M.periodic('daily', args[1]) end,
  weekly = function(args) M.periodic('weekly', args[1]) end,
  monthly = function(args) M.periodic('monthly', args[1]) end,
  periodic = function(args) M.periodic(args[1] or 'daily', args[2]) end,
  template = function(args)
    if args[1] then
      M.insert_template(args[1])
    else
      require('vulcan.pickers').template()
    end
  end,
  task = function() M.toggle_task() end,
  capture = function(args) M.capture(#args > 0 and table.concat(args, ' ') or nil) end,
  move = function(args) M.move(args[1]) end,
  scan = function(_, bang) M.scan(bang) end,
  render = function() require('vulcan.render').toggle() end,
  doctor = function() M.doctor() end,
  problems = function() M.problems() end,
  extract = function() M.extract() end,
  status = function() M.status() end,
  browse = function() M.float({ 'browse' }) end,
  bases = function(args) M.float({ 'bases', 'tui', args[1] or vim.api.nvim_buf_get_name(0) }) end,
  kanban = function(args) M.float({ 'kanban', 'show', args[1] or vim.api.nvim_buf_get_name(0) }) end,
}

--- Dispatch `:Vulcan {subcommand} [args]`.
--- @param opts { fargs: string[], bang: boolean }
function M.dispatch(opts)
  local name = opts.fargs[1]
  local handler = name and subcommands[name]
  if not handler then
    vim.notify('vulcan: unknown subcommand ' .. tostring(name), vim.log.levels.ERROR)
    return
  end
  handler(vim.list_slice(opts.fargs, 2), opts.bang)
end

--- Completion for `:Vulcan`.
--- @param arg_lead string
--- @param cmdline string
--- @return string[]
function M.complete(arg_lead, cmdline)
  if cmdline:match('^%S+%s+%S*$') then
    local names = vim.tbl_keys(subcommands)
    table.sort(names)
    return vim.tbl_filter(function(name)
      return vim.startswith(name, arg_lead)
    end, names)
  end
  return {}
end

return M
