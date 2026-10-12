-- Pickers over `vim.ui.select`, so telescope, fzf-lua, snacks, or any other
-- `ui.select` provider the user has configured presents them.

local cli = require('vulcan.cli')
local M = {}

--- Open a vault-relative path, optionally at a 1-based line.
--- @param root string
--- @param path string
--- @param line? integer
function M.open(root, path, line)
  vim.cmd.edit(vim.fn.fnameescape(vim.fs.joinpath(root, path)))
  if line and line > 0 then
    pcall(vim.api.nvim_win_set_cursor, 0, { line, 0 })
  end
end

--- Pick a note by path.
--- @param opts? { tag: string? }
function M.notes(opts)
  opts = opts or {}
  local root = require('vulcan').vault_root()
  local args = { 'ls' }
  if opts.tag then
    vim.list_extend(args, { '--tag', opts.tag })
  end
  cli.json_or_notify(args, function(paths)
    vim.ui.select(paths, { prompt = opts.tag and ('Notes tagged #' .. opts.tag) or 'Notes' }, function(path)
      if path then
        M.open(root, path)
      end
    end)
  end, root)
end

--- Full-text search; prompts for the query when none is given.
--- @param query? string
function M.search(query)
  local root = require('vulcan').vault_root()
  local function run(text)
    if not text or text == '' then
      return
    end
    cli.json_or_notify({ 'search', text }, function(hits)
      vim.ui.select(hits, {
        prompt = 'Search: ' .. text,
        format_item = function(hit)
          local snippet = (hit.snippet or ''):gsub('%s+', ' ')
          return ('%s  %s'):format(hit.document_path, snippet)
        end,
      }, function(hit)
        if hit then
          local span = hit.line_spans and hit.line_spans[1]
          M.open(root, hit.document_path, span and span.start_line)
        end
      end)
    end, root)
  end
  if query then
    run(query)
  else
    vim.ui.input({ prompt = 'Search vault: ' }, run)
  end
end

--- Pick a tag, then a note carrying it.
function M.tags()
  cli.json_or_notify({ 'tags' }, function(tags)
    vim.ui.select(tags, {
      prompt = 'Tags',
      format_item = function(tag)
        return ('#%s (%d)'):format(tag.tag, tag.count)
      end,
    }, function(tag)
      if tag then
        M.notes({ tag = tag.tag })
      end
    end)
  end)
end

--- Pick a template and insert it at the cursor.
function M.template()
  cli.json_or_notify({ 'template', 'list' }, function(reports)
    local templates = (reports[1] or {}).templates or {}
    vim.ui.select(templates, {
      prompt = 'Insert template',
      format_item = function(template)
        return template.name
      end,
    }, function(template)
      if template then
        require('vulcan.commands').insert_template(template.name)
      end
    end)
  end)
end

return M
