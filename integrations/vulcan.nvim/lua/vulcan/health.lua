-- `:checkhealth vulcan`

local M = {}

local PROTOCOL_VERSION = 1

function M.check()
  local vulcan = require('vulcan')
  vim.health.start('vulcan.nvim')

  if vim.fn.has('nvim-0.10') == 1 then
    vim.health.ok('Neovim ' .. tostring(vim.version()))
  else
    vim.health.error('Neovim 0.10 or newer is required')
  end

  local binary = vulcan.config.binary
  if vim.fn.executable(binary) == 1 then
    local version = vim.system({ binary, '--version' }, { text = true }):wait()
    vim.health.ok(('`%s` found: %s'):format(binary, vim.trim(version.stdout or '')))
  else
    vim.health.error(('`%s` is not executable'):format(binary), {
      'Install Vulcan or set `binary` (and `cmd`) in require("vulcan").setup()',
    })
    return
  end

  local client = vulcan.client()
  if not client then
    vim.health.warn('No Vulcan language server attached to the current buffer', {
      'Open a Markdown note inside a vault (a folder with .vulcan/, .vulcan.toml, or .obsidian/)',
    })
    return
  end
  vim.health.ok(('Language server attached (root %s, positions %s)'):format(
    client.config.root_dir,
    client.offset_encoding
  ))

  local advertised = vim.tbl_get(client.server_capabilities, 'experimental', 'vulcan', 'protocolVersion')
  if advertised == PROTOCOL_VERSION then
    vim.health.ok('Custom protocol version ' .. advertised)
  else
    vim.health.error(('Server speaks protocol %s; this plugin expects %d'):format(
      tostring(advertised),
      PROTOCOL_VERSION
    ), { 'Update vulcan.nvim and the vulcan binary together' })
  end

  local response
  if vim.fn.has('nvim-0.11') == 1 then
    response = client:request_sync('vulcan/status', {}, 2000, 0)
  else
    ---@diagnostic disable-next-line: param-type-mismatch
    response = client.request_sync('vulcan/status', {}, 2000, 0)
  end
  local status = response and response.result
  if not status then
    vim.health.warn('vulcan/status did not answer')
    return
  end
  local report = status.cache == 'fresh' and vim.health.ok or vim.health.warn
  report(('Cache %s, watcher %s'):format(status.cache, status.watcher))
end

return M
