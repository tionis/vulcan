vim9script
# Calls to `vulcan --output json`, for features the language server does
# not cover (listing notes, search, tags, templates, doctor).

import autoload 'vulcan.vim'

# Decode JSON output: one document, or one per line (streamed lists).
export def Decode(text: string): list<any>
  try
    var value = json_decode(text)
    return type(value) == v:t_list ? value : [value]
  catch
  endtry
  var values = []
  for line in text->split("\n")
    try
      values->add(json_decode(line))
    catch
    endtry
  endfor
  return values
enddef

# Run `vulcan --vault <root> --output json <args...>`; `Callback` gets the
# decoded values and an error message (empty on success).
export def Json(args: list<string>, Callback: func(list<any>, string), root: string = '')
  var vault = root->empty() ? vulcan.VaultRoot() : root
  var command = [vulcan.config.binary, '--vault', vault, '--output', 'json'] + args
  var state = {out: [], err: [], code: -1, closed: false, done: false}
  var Finish = () => {
    if !state.closed || state.code < 0 || state.done
      return
    endif
    state.done = true
    var stdout = state.out->join("\n")
    if state.code != 0
      var stderr = state.err->join("\n")->trim()
      Callback([], stderr->empty() ? stdout->trim() : stderr)
    else
      Callback(Decode(stdout), '')
    endif
  }
  job_start(command, {
    out_cb: (_, line) => state.out->add(line),
    err_cb: (_, line) => state.err->add(line),
    exit_cb: (_, code) => {
      state.code = code
      Finish()
    },
    close_cb: (_) => {
      state.closed = true
      Finish()
    },
  })
enddef

# `Json`, but report failures instead of returning them.
export def JsonOrWarn(args: list<string>, Callback: func(list<any>), root: string = '')
  Json(args, (values, error) => {
    if !error->empty()
      vulcan.Error($'{args[0]}: {error}')
      return
    endif
    Callback(values)
  }, root)
enddef
