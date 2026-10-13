vim9script
# Evaluated query results from `vulcan/render`, drawn as virtual text:
# block results (Dataview, Tasks, Bases, notices) as lines below their
# block, and inline expression values after their line.

import autoload 'vulcan.vim'

const INLINE = 'vulcan_render_inline'
const BLOCK = 'vulcan_render_block'

def EnsureTypes()
  highlight default link VulcanRender Comment
  for type in [INLINE, BLOCK]
    if prop_type_get(type)->empty()
      prop_type_add(type, {highlight: 'VulcanRender'})
    endif
  endfor
enddef

def Clear(bnr: number)
  if !bufexists(bnr)
    return
  endif
  for type in [INLINE, BLOCK]
    if !prop_type_get(type)->empty()
      prop_remove({type: type, bufnr: bnr, all: true})
    endif
  endfor
enddef

def Wanted(source: string): bool
  return index(vulcan.config.render.sources, source) >= 0
enddef

# Draw `regions` into buffer `bnr`, replacing earlier results.
export def Draw(bnr: number, regions: list<dict<any>>)
  if !bufexists(bnr)
    return
  endif
  EnsureTypes()
  Clear(bnr)
  var line_count = getbufinfo(bnr)[0].linecount
  for region in regions
    if !Wanted(get(region, 'source', ''))
      continue
    endif
    var markdown = get(region, 'markdown', '')
    if region.kind == 'inline'
      var line = min([region.range.start.line + 1, line_count])
      prop_add(line, 0, {type: INLINE, bufnr: bnr, text: $' → {markdown}', text_align: 'after'})
      continue
    endif
    var lines = markdown->trim()->split("\n", true)
    var max_lines = vulcan.config.render.max_lines
    if len(lines) > max_lines
      var hidden = len(lines) - max_lines
      lines = lines[: max_lines - 1] + [printf('… %d more line%s', hidden, hidden == 1 ? '' : 's')]
    endif
    # An end at column 0 belongs to the line after the block.
    var finish = region.range.end
    var last = finish.character == 0 ? finish.line : finish.line + 1
    last = max([1, min([last, line_count])])
    for text in lines
      prop_add(last, 0, {type: BLOCK, bufnr: bnr, text: $'│ {text}', text_align: 'below'})
    endfor
  endfor
enddef

# Ask the server for the current buffer's regions and draw them.
export def Refresh()
  var bnr = bufnr()
  if getbufvar(bnr, 'vulcan_render_off', false) || !vulcan.config.render.enabled
    Clear(bnr)
    return
  endif
  if vulcan.Server()->empty()
    return
  endif
  vulcan.Request('vulcan/render', {textDocument: {uri: vulcan.Uri(bnr)}}, (result, error) => {
    if error->empty() && type(result) == v:t_dict
      Draw(bnr, get(result, 'regions', []))
    endif
  })
enddef

# Refresh every visible vault note.
export def RefreshVisible()
  for info in getwininfo()
    if getbufvar(info.bufnr, 'vulcan_adopted', false)
      win_execute(info.winid, 'vulcan#render#Refresh()')
    endif
  endfor
enddef

# Keep a buffer's results current.
export def Attach(bnr: number)
  execute $'augroup vulcan_render_{bnr}'
  autocmd!
  execute $'autocmd BufEnter,BufWritePost,InsertLeave <buffer={bnr}> vulcan#render#Refresh()'
  augroup END
  Refresh()
enddef

# Turn results on or off for the current buffer.
export def Toggle()
  b:vulcan_render_off = !get(b:, 'vulcan_render_off', false)
  Refresh()
enddef
