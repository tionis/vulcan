vim9script
# Pickers: fzf.vim when it is installed, otherwise a `popup_menu()`.

import autoload 'vulcan.vim'
import autoload 'vulcan/cli.vim'

# Let the user pick one of `labels`; `Choose` gets the 0-based index.
# `g:vulcan_picker` set to 'popup' forces the built-in menu.
export def Pick(title: string, labels: list<string>, Choose: func)
  if labels->empty()
    echomsg $'vulcan: {title}: nothing to pick'
    return
  endif
  if exists('*fzf#run') && get(g:, 'vulcan_picker', '') != 'popup'
    # Prefix each line with its index so duplicates stay distinct.
    var source = labels->copy()->map((index, label) => $'{index}{"\t"}{label}')
    fzf#run(fzf#wrap({
      source: source,
      sink: (line) => Choose(line->matchstr('^\d\+')->str2nr()),
      options: ['--prompt', title .. '> ', '--with-nth', '2..', '--delimiter', "\t"],
    }))
    return
  endif
  popup_menu(labels, {
    title: $' {title} ',
    maxheight: max([5, &lines - 6]),
    callback: (_, result) => {
      if result > 0
        Choose(result - 1)
      endif
    },
  })
enddef

export def Notes(tag: string = '')
  var root = vulcan.VaultRoot()
  var args = ['ls']
  if !tag->empty()
    args += ['--tag', tag]
  endif
  cli.JsonOrWarn(args, (paths) => {
    var labels = paths->copy()->filter((_, path) => type(path) == v:t_string)
    Pick(tag->empty() ? 'Notes' : $'Notes tagged #{tag}', labels, (index) => vulcan.Open(root, labels[index]))
  }, root)
enddef

export def Search(query: string = '')
  var text = query->empty() ? input('Search vault: ') : query
  if text->empty()
    return
  endif
  var root = vulcan.VaultRoot()
  cli.JsonOrWarn(['search', text], (hits) => {
    var labels = hits->copy()->map((_, hit) => {
      var snippet = get(hit, 'snippet', '')->substitute('\s\+', ' ', 'g')
      return $'{hit.document_path}  {snippet}'
    })
    Pick($'Search: {text}', labels, (index) => {
      var hit = hits[index]
      var spans = get(hit, 'line_spans', [])
      var line = spans->empty() ? 0 : get(spans[0], 'start_line', 0)
      vulcan.Open(root, hit.document_path, line)
    })
  }, root)
enddef

export def Tags()
  cli.JsonOrWarn(['tags'], (tags) => {
    var labels = tags->copy()->map((_, tag) => $'#{tag.tag} ({tag.count})')
    Pick('Tags', labels, (index) => Notes(tags[index].tag))
  })
enddef

export def Template()
  cli.JsonOrWarn(['template', 'list'], (reports) => {
    var report = get(reports, 0, {})
    var templates = type(report) == v:t_dict ? get(report, 'templates', []) : []
    var labels = templates->copy()->map((_, template) => template.name)
    Pick('Insert template', labels, (index) => vulcan.InsertTemplate(labels[index]))
  })
enddef
