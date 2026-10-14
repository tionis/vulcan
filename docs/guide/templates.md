# Templates

Vulcan renders note templates with two engines: a native engine for `{{date}}`-style variables
and QuickAdd tokens, and a Templater-compatible engine for `<% … %>` tags. `--engine auto` (the
default) switches to Templater when a template contains a tag.

```bash
vulcan template --list                      # every template from all configured folders
vulcan template show meeting                # source, plus the inputs it asks for
vulcan template preview meeting --path Meetings/Kickoff
vulcan template meeting --path Meetings/Kickoff --var note_type=Idea
vulcan template insert meeting Projects/Alpha --var project=Vulcan
```

Templates live in `.vulcan/templates`, plus any folder configured for Obsidian core templates or
the Templater plugin (`vulcan config import --all` imports those settings).

## How Templater templates run

A template that needs JavaScript runs the way Templater runs it: as one async function. Text
becomes output, `<% expr %>` appends the awaited value of `expr`, and `<%* code %>` runs `code`
with `tR` holding the output so far. Because the template is one program:

- declarations, `if` blocks, and loops can span tags:

  ```markdown
  <%* if (tp.file.title.startsWith("20")) { -%>
  A dated note.
  <%* } else { -%>
  An undated note.
  <%* } -%>
  ```

- `await` behaves like JavaScript, including promises returned by user scripts;
- `-%>`/`<%-` trim one adjacent newline and `_%>`/`<%_` trim all adjacent whitespace.

Templates that only read `tp.file.*`, `tp.frontmatter.*`, `tp.date.*`, and similar values render
with the native interpreter and also work in builds without the `js_runtime` feature.

A template script that runs longer than `[js_runtime] default_timeout_seconds` (30 by default)
is stopped. Time spent waiting for you to answer a prompt does not count.

## Prompts and `--var`

`tp.system.prompt()`, `tp.system.suggester()`, `tp.system.multi_suggester()`, and QuickAdd
`{{VALUE}}`/`{{VDATE}}` tokens get their answers, in order, from:

1. a `--var KEY=VALUE` binding;
2. an interactive terminal prompt, when both stdin and stderr are terminals. Servers (`serve`,
   `mcp`, `lsp`, `daemon`, `site`, `editor`) never prompt;
3. the call's default value.

When none of those answers a prompt, rendering stops with a `template_input_required` error
that names the exact `--var` key and, for suggesters, the available options. Nothing is
guessed.

| Call | `--var` key |
| --- | --- |
| `tp.system.prompt("Project name")` | `project_name` (or the exact text `"Project name"`) |
| `tp.system.prompt()` with no text | `prompt_1`, `prompt_2`, … in call order |
| `tp.system.suggester(…, …, false, "Note type")` | `note_type` (the placeholder) |
| `tp.system.suggester(…)` with no placeholder | `suggester_1`, `suggester_2`, … |
| `tp.system.multi_suggester(…)` with no title | `multi_suggester_N` |
| `{{VALUE:client}}` | `client` |

`vulcan template show NAME` lists the inputs a template asks for and their keys.

A suggester `--var` value may name an option label, the item itself (a string or a file path),
the label case-insensitively, or a 1-based option number. An unmatched value is an error that
lists the options. `multi_suggester` accepts a comma-separated list or a JSON array.

In a terminal, a suggester lists numbered options: type a number to choose, or text to filter.
Press Ctrl-D to cancel. A cancelled prompt returns `null`, as in Templater, unless the call
passed `throw_on_cancel`.

## Errors

Errors name the template, the line, and the JavaScript error, and show the failing line:

```text
template `.vulcan/templates/meeting.md` failed at line 4: ReferenceError: attendes is not defined
  4 | <% attendes.join(", ") %>
```

Errors from `tp.*` helpers name the helper (`tp.file.include(): File [[Missing]] doesn't
exist`), errors inside user scripts name the script function and file, and unsupported Obsidian
APIs fail by name (`app.commands is not available in Vulcan templates`). With `--output json`
the error carries a code: `template_input_required`, `template_syntax_error` (for example an
unclosed `<%`), `template_script_error`, or `template_script_timeout`.

## Compatibility

Supported Templater modules:

- `tp.date`: `now`, `tomorrow`, `yesterday`, `weekday`, with moment.js format tokens.
- `tp.file`: `title`, `content`, `tags`, `path(relative)`, `folder(absolute)`,
  `creation_date`, `last_modified_date`, `exists`, `find_tfile`, `include` (including
  `[[Note#Heading]]` and `[[Note#^block]]`), `create_new`, `move`, `rename`, `cursor`.
- `tp.frontmatter`, `tp.config`, `tp.hooks.on_all_templates_executed`.
- `tp.system`: `prompt`, `suggester`, `multi_suggester`, `clipboard`.
- `tp.web`: `request` (returns parsed JSON, or the value at a dotted path), `daily_quote`,
  `random_picture`. Hosts must be allowed by `[templates] web_allowlist`.
- `tp.user.*`: CommonJS scripts from `[templates] user_scripts_folder`, plus configured system
  command functions when `enable_system_commands` is on.
- `tp.obsidian`: `moment`, `normalizePath`, `htmlToMarkdown`, `parseYaml`, `stringifyYaml`,
  `requestUrl` and `request` (GET only), `Notice` (recorded as a diagnostic).
- `moment` (moment.js 2.30.1, as bundled with Obsidian) and `window.moment` are globals.

`tp.app` and the global `app` provide the Obsidian API calls templates commonly use:
`vault.getName`, `getMarkdownFiles`, `getFiles`, `getAbstractFileByPath`, `getFileByPath`,
`read`, `cachedRead`, `create`, `modify`, `process`, `adapter.exists`, `adapter.read`;
`metadataCache.getFileCache`, `getCache`, `getFirstLinkpathDest`; `workspace.getActiveFile`;
and `fileManager.processFrontMatter` and `generateMarkdownLink`. `getMarkdownFiles` reads the
cache, so run `vulcan scan` first in a new vault. `modify`, `process`, and `processFrontMatter`
can only change the note being rendered, and only inside `tp.hooks.on_all_templates_executed`,
after the template output exists. Anything else under `app` fails with its name.

Differences from Obsidian:

- `tp.file.path()` returns the filesystem path and `tp.file.path(true)` the vault-relative path.
  `tp.file.folder()` returns the folder's name and `tp.file.folder(true)` its vault-relative
  path. This matches Templater.
- `tp.file.create_new(template, …)` renders a `TFile` (for example from `tp.file.find_tfile`) as
  a template for the new note. A string is itself template content rendered for the new note.
- There is no editor: `tp.file.cursor()` is a no-op outside `vulcan editor` integrations,
  `tp.file.selection()` returns `""`, and `tp.file.cursor_append()` records a diagnostic.
- `<%+ %>` dynamic tags render once, when the template runs.
- `require()` is not available; Node and Obsidian modules cannot be loaded.
- `template preview` disables `create_new`, `move`, `rename`, and `app.vault.create`, and
  reports a diagnostic instead.
