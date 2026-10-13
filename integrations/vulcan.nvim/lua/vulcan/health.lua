-- `:checkhealth vulcan`

local M = {}

local PROTOCOL_VERSION = 1

--- Whether this copy of the plugin is the one the binary bundles. Copies
--- from `vulcan editor nvim install` carry a marker; a checkout or a plugin
--- manager's copy does not, and is reported as such.
--- @param binary string
function M.check_install(binary)
  local source = debug.getinfo(1, 'S').source:sub(2)
  local root = vim.fs.dirname(vim.fs.dirname(vim.fs.dirname(source)))
  local result = vim.system({ binary, '--output', 'json', 'editor', 'nvim', 'status', '--dir', root }, { text = true })
    :wait()
  local ok, status = pcall(vim.json.decode, result.stdout or '')
  if result.code ~= 0 or not ok or type(status) ~= 'table' then
    vim.health.info('Could not compare this plugin with the binary (`vulcan editor nvim status` failed)')
    return
  end
  if status.state == 'current' then
    vim.health.ok('Plugin matches the binary (' .. status.version .. ')')
  elseif status.state == 'outdated' then
    vim.health.warn(
      ('Plugin was installed from Vulcan %s; the binary bundles %s'):format(status.installed_version, status.version),
      { 'Run `vulcan editor nvim install` and restart Neovim' }
    )
  elseif status.state == 'modified' then
    vim.health.warn(
      'Plugin files changed since install: ' .. table.concat(status.modified_files, ', '),
      { 'Run `vulcan editor nvim install --force` to restore them' }
    )
  else
    vim.health.info('Plugin loaded from ' .. root .. ' (not installed by `vulcan editor nvim install`)')
  end
end

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
  M.check_install(binary)

  -- `:checkhealth` runs in its own buffer, so look for any running client.
  local client = vulcan.client() or vim.lsp.get_clients({ name = vulcan.client_name })[1]
  if not client then
    vim.health.warn('No Vulcan language server is running', {
      'Open a Markdown note inside a vault (a folder with .vulcan/, .vulcan.toml, or .obsidian/)',
    })
    return
  end
  local bufnr = next(client.attached_buffers) or 0
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
    response = client:request_sync('vulcan/status', {}, 2000, bufnr)
  else
    ---@diagnostic disable-next-line: param-type-mismatch
    response = client.request_sync('vulcan/status', {}, 2000, bufnr)
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
