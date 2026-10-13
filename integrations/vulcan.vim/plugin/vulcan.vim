vim9script
# vulcan.vim: Vulcan vaults in Vim, over yegappan/lsp and `vulcan lsp`.
#
# `:Vulcan` is available at once; the server is registered when
# yegappan/lsp sets up (its `User LspSetup` event), so the two plugins can
# load in any order.

if exists('g:loaded_vulcan') || v:version < 900
  finish
endif
g:loaded_vulcan = true

command! -nargs=+ -bang -complete=customlist,vulcan#Complete Vulcan vulcan#Dispatch(<q-args>, <bang>false)

augroup vulcan
  autocmd!
  autocmd User LspSetup vulcan#Register()
  autocmd User LspAttached vulcan#Attached()
augroup END

# Loaded after yegappan/lsp already set up (a late :packadd): register now.
if v:vim_did_enter && exists('*g:LspAddServer')
  vulcan#Register()
endif
