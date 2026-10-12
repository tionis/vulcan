-- Calls to `vulcan --output json`, for features the language server does
-- not cover (listing notes, search, tags, templates, doctor).

local M = {}

--- Decode JSON output: one document, or one per line (streamed lists).
--- @param text string
--- @return any[]
function M.decode(text)
  local ok, value = pcall(vim.json.decode, text)
  if ok then
    return vim.islist(value) and value or { value }
  end
  local values = {}
  for line in vim.gsplit(text, '\n', { trimempty = true }) do
    local line_ok, decoded = pcall(vim.json.decode, line)
    if line_ok then
      values[#values + 1] = decoded
    end
  end
  return values
end

--- Run `vulcan --vault <root> --output json <args...>`.
--- @param args string[]
--- @param callback fun(values: any[]?, err: string?)
--- @param root? string Vault root; the current buffer's vault by default.
function M.json(args, callback, root)
  local vulcan = require('vulcan')
  local cmd = { vulcan.config.binary, '--vault', root or vulcan.vault_root(), '--output', 'json' }
  vim.list_extend(cmd, args)
  vim.system(cmd, { text = true }, function(result)
    vim.schedule(function()
      if result.code ~= 0 then
        callback(nil, vim.trim(result.stderr ~= '' and result.stderr or result.stdout))
        return
      end
      callback(M.decode(result.stdout))
    end)
  end)
end

--- `M.json`, but report failures instead of returning them.
--- @param args string[]
--- @param callback fun(values: any[])
--- @param root? string
function M.json_or_notify(args, callback, root)
  M.json(args, function(values, err)
    if not values then
      vim.notify(('vulcan %s: %s'):format(args[1], err), vim.log.levels.ERROR)
      return
    end
    callback(values)
  end, root)
end

return M
