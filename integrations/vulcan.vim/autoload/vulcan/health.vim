vim9script
# `:Vulcan health`: check the setup and show the report in a scratch buffer.

import autoload 'vulcan.vim'

const PROTOCOL_VERSION = 1
# The plugin's root (this file is autoload/vulcan/health.vim).
const PLUGIN_ROOT = expand('<sfile>:p:h:h:h')

# The checks, as `[level, message]` pairs (level: OK, WARN, ERROR, INFO),
# for the current buffer. The server's cache status arrives
# asynchronously, so `Show` appends it.
export def Checks(): list<list<string>>
  var checks: list<list<string>> = []
  var Add = (level: string, message: string) => {
    checks->add([level, message])
  }
  if has('patch-9.0.0067')
    Add('OK', $'Vim {v:version / 100}.{v:version % 100} with virtual text')
  else
    Add('ERROR', 'Vim 9.0.0067 or newer is required (virtual text)')
  endif
  if exists('*g:LspAddServer')
    Add('OK', 'yegappan/lsp is loaded')
  else
    Add('ERROR', 'yegappan/lsp is not loaded; install it (see :help vulcan-setup)')
    return checks
  endif
  var binary = vulcan.config.binary
  if !executable(binary)
    Add('ERROR', $'`{binary}` is not executable; install Vulcan or set g:vulcan.binary')
    return checks
  endif
  Add('OK', $'`{binary}` found: {system($'{shellescape(binary)} --version')->trim()}')
  checks += InstallChecks(binary)

  var server = vulcan.Server()
  if server->empty()
    Add('WARN', 'No Vulcan language server is attached to this buffer; open a note inside a vault (a folder with .vulcan/, .vulcan.toml, or .obsidian/)')
    return checks
  endif
  Add('OK', $'Language server attached (root {vulcan.VaultRoot()})')
  var advertised = get(get(get(server.caps, 'experimental', {}), 'vulcan', {}), 'protocolVersion', -1)
  if advertised == PROTOCOL_VERSION
    Add('OK', $'Custom protocol version {advertised}')
  else
    Add('ERROR', $'Server speaks protocol {advertised}; this plugin expects {PROTOCOL_VERSION}. Update vulcan.vim and the binary together')
  endif
  return checks
enddef

# Whether this copy of the plugin is the one the binary bundles. Copies
# from `vulcan editor vim install` carry a marker; a checkout or a plugin
# manager's copy does not.
def InstallChecks(binary: string): list<list<string>>
  var root = PLUGIN_ROOT
  var output = system($'{shellescape(binary)} --output json editor vim status --dir {shellescape(root)}')
  var status: any = {}
  try
    status = json_decode(output)
  catch
  endtry
  if v:shell_error != 0 || type(status) != v:t_dict || !has_key(status, 'state')
    return [['INFO', 'Could not compare this plugin with the binary (`vulcan editor vim status` failed)']]
  elseif status.state == 'current'
    return [['OK', $'Plugin matches the binary ({status.version})']]
  elseif status.state == 'outdated'
    return [['WARN', $'Plugin was installed from Vulcan {status.installed_version}; the binary bundles {status.version}. Run `vulcan editor vim install` and restart Vim']]
  elseif status.state == 'modified'
    return [['WARN', $'Plugin files changed since install: {status.modified_files->join(", ")}. Run `vulcan editor vim install --force` to restore them']]
  endif
  return [['INFO', $'Plugin loaded from {root} (not installed by `vulcan editor vim install`)']]
enddef

def Format(check: list<string>): string
  return $'- {check[0]}: {check[1]}'
enddef

export def Show()
  var checks = Checks()
  var server = vulcan.Server()
  var uri = vulcan.Uri()
  new
  setlocal buftype=nofile bufhidden=wipe noswapfile filetype=markdown
  setline(1, ['# vulcan.vim health', ''] + checks->copy()->map((_, check) => Format(check)))
  var report = bufnr()
  if server->empty()
    return
  endif
  server.rpc_a('vulcan/status', {textDocument: {uri: uri}}, (_, status, error) => {
    if !bufexists(report)
      return
    endif
    var line = !error->empty() || type(status) != v:t_dict
      ? Format(['WARN', 'vulcan/status did not answer'])
      : Format([status.cache == 'fresh' ? 'OK' : 'WARN', $'Cache {status.cache}, watcher {status.watcher}'])
    appendbufline(report, '$', line)
  })
enddef
