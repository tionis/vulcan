-- Evaluated query results from `vulcan/render`, drawn as virtual text:
-- block results (Dataview, Tasks, Bases, notices) as virtual lines below
-- their block, and inline expression values at the end of their line.

local vulcan = require('vulcan')
local M = {}

--- @class vulcan.RenderConfig
--- @field enabled boolean
--- @field sources string[]
--- @field max_lines integer

M.namespace = vim.api.nvim_create_namespace('vulcan_render')

--- Buffers with rendering turned off by `toggle`.
local disabled = {}

local function wanted(source)
  return vim.tbl_contains(vulcan.config.render.sources, source)
end

--- Draw `regions` into `bufnr`, replacing earlier results.
--- @param bufnr integer
--- @param regions table[]
function M.draw(bufnr, regions)
  if not vim.api.nvim_buf_is_valid(bufnr) then
    return
  end
  vim.api.nvim_buf_clear_namespace(bufnr, M.namespace, 0, -1)
  local line_count = vim.api.nvim_buf_line_count(bufnr)
  for _, region in ipairs(regions) do
    if wanted(region.source) then
      if region.kind == 'inline' then
        local line = math.min(region.range.start.line, line_count - 1)
        vim.api.nvim_buf_set_extmark(bufnr, M.namespace, line, 0, {
          virt_text = { { ' → ' .. region.markdown, 'Comment' } },
          virt_text_pos = 'eol',
        })
      else
        local lines = vim.split(vim.trim(region.markdown), '\n')
        local max = vulcan.config.render.max_lines
        if #lines > max then
          local hidden = #lines - max
          lines = vim.list_slice(lines, 1, max)
          lines[#lines + 1] = ('… %d more line%s'):format(hidden, hidden == 1 and '' or 's')
        end
        local virt_lines = vim.tbl_map(function(text)
          return { { '│ ', 'NonText' }, { text, 'Comment' } }
        end, lines)
        -- An end at column 0 belongs to the line after the block.
        local finish = region.range['end']
        local last = finish.character == 0 and finish.line - 1 or finish.line
        vim.api.nvim_buf_set_extmark(bufnr, M.namespace, math.min(math.max(last, 0), line_count - 1), 0, {
          virt_lines = virt_lines,
        })
      end
    end
  end
end

--- Ask the server for the buffer's regions and draw them.
--- @param bufnr? integer
function M.refresh(bufnr)
  bufnr = bufnr or vim.api.nvim_get_current_buf()
  if disabled[bufnr] or not vulcan.config.render.enabled then
    vim.api.nvim_buf_clear_namespace(bufnr, M.namespace, 0, -1)
    return
  end
  if not vulcan.client(bufnr) then
    return
  end
  vulcan.request('vulcan/render', { textDocument = { uri = vim.uri_from_bufnr(bufnr) } }, function(err, result)
    if err or not result then
      return
    end
    M.draw(bufnr, result.regions or {})
  end, bufnr)
end

--- Refresh every visible buffer the server is attached to.
function M.refresh_visible()
  for _, win in ipairs(vim.api.nvim_list_wins()) do
    local bufnr = vim.api.nvim_win_get_buf(win)
    if vulcan.client(bufnr) then
      M.refresh(bufnr)
    end
  end
end

--- Keep a buffer's results current.
--- @param bufnr integer
function M.attach(bufnr)
  local group = vim.api.nvim_create_augroup('vulcan_render_' .. bufnr, { clear = true })
  vim.api.nvim_create_autocmd({ 'BufEnter', 'BufWritePost', 'InsertLeave' }, {
    group = group,
    buffer = bufnr,
    callback = function()
      M.refresh(bufnr)
    end,
  })
  vim.api.nvim_create_autocmd('BufWipeout', {
    group = group,
    buffer = bufnr,
    callback = function()
      disabled[bufnr] = nil
      pcall(vim.api.nvim_del_augroup_by_id, group)
    end,
  })
  M.refresh(bufnr)
end

--- Turn results on or off for the current buffer.
function M.toggle()
  local bufnr = vim.api.nvim_get_current_buf()
  disabled[bufnr] = not disabled[bufnr] or nil
  M.refresh(bufnr)
end

return M
