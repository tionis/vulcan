-- vulcan.nvim: a thin Neovim layer over `vulcan lsp` and `vulcan --output json`.
--
-- Vault semantics (links, tasks, templates, periodic dates, frontmatter)
-- live in Vulcan. This plugin only registers the language server, wires
-- commands to it or to the CLI, and draws what they return.

local M = {}

--- @class vulcan.Config
--- @field cmd string[] Command that starts the language server.
--- @field binary string The `vulcan` executable used for CLI calls.
--- @field root_markers string[] Files or folders that mark a vault root.
--- @field filetypes string[] Filetypes the server attaches to.
--- @field init_options table `initializationOptions` sent to the server.
--- @field render vulcan.RenderConfig
--- @field float { width: number, height: number, border: string }
--- @field keymaps table<string, string|false> Buffer-local mappings in vault notes.
M.defaults = {
  cmd = nil,
  binary = 'vulcan',
  root_markers = { '.vulcan', '.vulcan.toml', '.obsidian' },
  filetypes = { 'markdown' },
  init_options = {},
  render = {
    enabled = true,
    -- Region sources to show: inline, dataview, dataviewjs, tasks, base, notice.
    sources = { 'inline', 'dataview', 'dataviewjs', 'tasks', 'base', 'notice' },
    max_lines = 30,
  },
  float = { width = 0.9, height = 0.85, border = 'rounded' },
  keymaps = {
    notes = '<leader>vn',
    search = '<leader>vs',
    tags = '<leader>vt',
    daily = '<leader>vd',
    template = '<leader>vi',
    toggle_task = '<leader>vx',
    render = '<leader>vr',
  },
}

--- @type vulcan.Config
M.config = vim.deepcopy(M.defaults)

M.client_name = 'vulcan'

--- The running language server client for a buffer, if any.
--- @param bufnr? integer
--- @return vim.lsp.Client?
function M.client(bufnr)
  return vim.lsp.get_clients({ bufnr = bufnr or 0, name = M.client_name })[1]
end

--- The vault root for a buffer: the attached server's root, else the
--- nearest root marker, else the working directory.
--- @param bufnr? integer
--- @return string
function M.vault_root(bufnr)
  local client = M.client(bufnr)
  if client and client.config.root_dir then
    return client.config.root_dir
  end
  return vim.fs.root(bufnr or 0, M.config.root_markers) or vim.fn.getcwd()
end

--- Send a request to the buffer's server.
--- @param method string
--- @param params table
--- @param callback fun(err: table?, result: any)
--- @param bufnr? integer
--- @return boolean sent
function M.request(method, params, callback, bufnr)
  local client = M.client(bufnr)
  if not client then
    vim.notify('vulcan: no language server attached to this buffer', vim.log.levels.WARN)
    return false
  end
  if vim.fn.has('nvim-0.11') == 1 then
    client:request(method, params, callback, bufnr or 0)
  else
    ---@diagnostic disable-next-line: param-type-mismatch
    client.request(method, params, callback, bufnr or 0)
  end
  return true
end

--- Run a `vulcan.*` command through `workspace/executeCommand`.
--- @param command string
--- @param argument table
--- @param callback? fun(result: table)
--- @param bufnr? integer
function M.execute(command, argument, callback, bufnr)
  M.request('workspace/executeCommand', { command = command, arguments = { argument } }, function(err, result)
    if err then
      vim.notify(('vulcan: %s'):format(err.message or vim.inspect(err)), vim.log.levels.ERROR)
      return
    end
    if callback then
      callback(result)
    end
  end, bufnr)
end

local function lsp_config()
  return {
    cmd = M.config.cmd or { M.config.binary, 'lsp' },
    filetypes = M.config.filetypes,
    root_markers = M.config.root_markers,
    -- An empty Lua table would be sent as a JSON array.
    init_options = next(M.config.init_options) and M.config.init_options or vim.empty_dict(),
    handlers = {
      ['vulcan/indexChanged'] = function()
        require('vulcan.render').refresh_visible()
      end,
    },
  }
end

local function register_lsp()
  local config = lsp_config()
  if vim.fn.has('nvim-0.11') == 1 then
    vim.lsp.config(M.client_name, config)
    vim.lsp.enable(M.client_name)
    return
  end
  -- Neovim 0.10: start the client per buffer.
  vim.api.nvim_create_autocmd('FileType', {
    group = vim.api.nvim_create_augroup('vulcan_lsp', { clear = true }),
    pattern = config.filetypes,
    callback = function(args)
      local root = vim.fs.root(args.buf, config.root_markers)
      if root then
        vim.lsp.start(vim.tbl_extend('force', config, { name = M.client_name, root_dir = root }), {
          bufnr = args.buf,
        })
      end
    end,
  })
end

local function set_keymaps(bufnr)
  local actions = {
    notes = function() require('vulcan.pickers').notes() end,
    search = function() require('vulcan.pickers').search() end,
    tags = function() require('vulcan.pickers').tags() end,
    daily = function() require('vulcan.commands').periodic('daily') end,
    template = function() require('vulcan.pickers').template() end,
    toggle_task = function() require('vulcan.commands').toggle_task() end,
    render = function() require('vulcan.render').toggle() end,
  }
  for name, lhs in pairs(M.config.keymaps or {}) do
    if lhs and actions[name] then
      vim.keymap.set('n', lhs, actions[name], { buffer = bufnr, desc = 'Vulcan: ' .. name:gsub('_', ' ') })
    end
  end
end

--- Configure the plugin and register the language server.
--- @param opts? vulcan.Config
function M.setup(opts)
  M.config = vim.tbl_deep_extend('force', vim.deepcopy(M.defaults), opts or {})
  register_lsp()
  local group = vim.api.nvim_create_augroup('vulcan', { clear = true })
  vim.api.nvim_create_autocmd('LspAttach', {
    group = group,
    callback = function(args)
      local client = vim.lsp.get_client_by_id(args.data.client_id)
      if not client or client.name ~= M.client_name then
        return
      end
      set_keymaps(args.buf)
      require('vulcan.render').attach(args.buf)
    end,
  })
  -- A distribution (or a lazy-loaded setup) may have attached the server
  -- before this ran; adopt those buffers too.
  for _, client in ipairs(vim.lsp.get_clients({ name = M.client_name })) do
    for bufnr in pairs(client.attached_buffers) do
      set_keymaps(bufnr)
      require('vulcan.render').attach(bufnr)
    end
  end
end

return M
