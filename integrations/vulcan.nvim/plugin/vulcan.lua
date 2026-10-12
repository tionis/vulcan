-- `:Vulcan` is available before setup; it loads the plugin on first use.
if vim.g.loaded_vulcan then
  return
end
vim.g.loaded_vulcan = true

vim.api.nvim_create_user_command('Vulcan', function(opts)
  require('vulcan.commands').dispatch(opts)
end, {
  nargs = '+',
  bang = true,
  desc = 'Vulcan vault commands',
  complete = function(arg_lead, cmdline)
    return require('vulcan.commands').complete(arg_lead, cmdline)
  end,
})
