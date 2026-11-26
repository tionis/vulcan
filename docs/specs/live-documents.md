# Live documents: shared scope, vault functions, bound inputs, and calculator blocks

**Status:** Design contract for Roadmap Phase 9.42. 9.42.1 (shared scope), 9.42.2 (CustomJS), and
9.42.4 (Numerals) are implemented. 9.42.3 (Meta Bind) renders fields and has `list`, `set`, and
`apply` and editor code actions; button presses and JS views are still planned.

SilverBullet's Space Lua and Ink & Switch's Potluck make a note a small live program: values
defined in one place, computed inline elsewhere, edited in place, with helpers defined in the
space itself. Obsidian spreads those capabilities across plugins (Dataview, CustomJS, Meta Bind,
Numerals). Vulcan implements those plugins' syntax, so the same notes work in Obsidian and in
Vulcan, rather than inventing a Vulcan-only notation. This document pins down what Vulcan
implements and where it differs.

Pinned plugin versions (sources were read from these releases): CustomJS 1.0.21, Meta Bind 1.5.1,
Numerals 1.10.2 with mathjs 15.2.0. Clone them into `references/` (gitignored) to check details:
`saml-dev/obsidian-custom-js`, `mProjectsCode/obsidian-meta-bind-plugin`,
`gtg922r/obsidian-numerals`.

Licensing: Meta Bind is GPL-3.0 and Vulcan is MIT OR Apache-2.0, so Vulcan's Meta Bind support is a
clean reimplementation of the syntax and behavior described here; no Meta Bind source may be
copied. CustomJS and Numerals are MIT (ports keep attribution in the code). The bundled mathjs is
Apache-2.0 and ships with its `LICENSE`, `NOTICE`, and bundled-dependency notices.

## Principles

1. **Obsidian syntax, not new syntax.** Every construct here is one an Obsidian plugin already
   renders. Where Vulcan's behavior is stricter or more deterministic than the plugin's, the
   difference is listed below.
2. **Rendering never writes.** Rendering, `note get --evaluate`, the editor's virtual text, and
   exports only read. Writing a bound value or a view's computed value is always an explicit
   command or editor action, with `--dry-run`.
3. **Same trust model as DataviewJS.** Vault JavaScript (blocks, inline expressions, CustomJS
   classes) runs under the render's `DataviewJS` policy: the strict `static` sandbox in an
   untrusted vault, the configured tier only in a vault trusted with `vulcan trust`. Pure
   computation (mathjs, DQL calls into CustomJS functions) runs in an engine with no vault access
   at all, so it is safe in any vault.
4. **Everything is a diagnostic or a value.** Unsupported syntax (Meta Bind buttons that need
   Obsidian commands, `memory^` targets that cannot persist) renders a visible notice and a
   diagnostic, never silently nothing.
5. **`js_runtime` gating.** Features that need JavaScript (CustomJS, mathjs) are behind the
   `js_runtime` feature. Without it, their regions render a "requires `js_runtime`" notice.

## 9.42.1 One scope per note (implemented)

A note's inline DataviewJS (`` `$= …` ``) and `dataviewjs` blocks run in one `DataviewJsSession`,
top to bottom, as non-strict global code:

- `` `$= test = 3` `` renders `3`; a later `` `$= test + 3` `` renders `6`.
- A block may define helpers with a plain assignment (`fmt = (x) => …`); later inline expressions
  and blocks see them.
- Blocks are wrapped in braces, so their `const`, `let`, and `class` declarations stay local and
  two blocks can declare the same name (Dataview runs each block as its own function).
- Inline DQL (`` `= …` ``) cannot assign; it reads note fields (`[test:: 3]`, frontmatter).
- `vulcan dataview inline` reports inline JS values under `js_results`; `dataview eval --block N`
  first runs every script above block N.

Obsidian difference: Dataview runs the same code by leaking globals, and its render order is not
guaranteed, so a note that must render identically there should assign before use.

## 9.42.2 CustomJS classes and the DQL bridge (implemented)

### CustomJS compatibility

Settings come from `[customjs]` in `.vulcan/config.toml`, importable with
`vulcan config import customjs` from `.obsidian/plugins/customjs/data.json`:

| CustomJS key | Vulcan key | Default | Meaning |
| --- | --- | --- | --- |
| `jsFiles` | `js_files` (list) | `[]` | Explicit files; CustomJS stores a comma-separated string, trimmed, `.js` only |
| `jsFolder` | `js_folder` | `""` | Every vault `.js` file whose path starts with this string (a raw prefix, so recursive) |
| `startupScriptNames` | `startup_scripts` | `[]` | Classes whose `invoke()` runs on `vulcan plugin customjs startup` |
| `registeredInvocableScriptNames` | `invocable_scripts` | `[]` | Classes listed by `vulcan plugin customjs list` |

Loading follows CustomJS exactly:

- Files from both settings are combined, sorted by **basename** (`localeCompare` order, directory
  ignored), and evaluated in order. A file listed twice loads twice, as in CustomJS.
- Each file is evaluated as `(` + source + `)`: it must be a single class expression. The
  constructor gets no arguments. The instance is stored as `customJS[instance.constructor.name]`
  (the class name, not the filename; a later duplicate wins), and
  `customJS["create" + Name + "Instance"]` returns a fresh instance.
- A file that fails to load is skipped with a diagnostic; loading continues.
- Globals: `customJS` (with `state`, kept for the session), `cJS(arg?)` (async; a string returns
  that class, a function is called with `customJS`, no argument returns `customJS`), and
  `forceLoadCustomJS()`. `obsidian` and `app` are absent; scripts that need them fail with a clear
  error.
- Classes load lazily, the first time a session reads `customJS` or calls `cJS()`, in every
  DataviewJS session: rendered notes (sharing the note's scope, 9.42.1), `dataview query-js`,
  `dataview eval`, `vulcan run`, plugins, and skill commands. Class files are read through the
  session's read permissions. Sessions that never touch them pay nothing.
- A `dataviewjs` block that uses `await` runs as an async function, as Dataview runs every block,
  so `const { Util } = await cJS()` works; its rejection renders as the block's error.
- `invoke()` convention: `vulcan plugin customjs invoke <Class>` runs `instance.invoke()` (awaited)
  at the configured sandbox tier, like `vulcan plugin run`: it needs a trusted vault and the
  `execute` permission, and may write. `--dry-run` runs it with every write refused.
  `vulcan plugin customjs startup` runs the `startup_scripts` in order in one session.
  `deconstructor()` is not called; sessions are short-lived.
- `vulcan plugin customjs list` loads the classes in the strict sandbox, read-only (no trust
  needed), and reports files in load order, classes, invocability, registration, and load errors.

### DQL bridge (Vulcan-only)

`[customjs.dql_functions]` maps a DQL function name to a CustomJS method:

```toml
[customjs.dql_functions]
fmt = "Util.fmt"        # = fmt(this.price) calls customJS.Util.fmt(price)
```

- Built-in DQL functions take precedence; a mapping with a built-in name is never called.
- Arguments are converted DQL value → JSON → JS; the return value converts back (`undefined`
  becomes `null`).
- Calls run in a separate QuickJS engine per evaluation that holds only the CustomJS classes: no
  `dv`, no vault reads, no network, under the vault's `js_runtime` memory, stack, and timeout
  limits. A function sees only its arguments, so it is safe in any vault and cannot read past a
  permission-scoped query; class files the reader cannot read are not loaded. Methods must return
  synchronously: a Promise is an error. Without `js_runtime`, or for a class or method that does
  not exist, the call is a DQL error.
- The bridge applies to DQL (queries, inline `` `= …` ``, `dataview` blocks), not to Bases
  formulas, whose function set follows Obsidian.

Obsidian difference: in Obsidian these calls fail as unknown functions; use them only in vaults
read mainly through Vulcan, or keep a DataviewJS fallback.

## 9.42.3 Meta Bind inputs and views (implemented)

### Where fields appear

- Inline code whose untrimmed text starts with `INPUT[`, `VIEW[`, or `BUTTON[` and ends with `]`.
- A ` ```meta-bind ` block holding exactly one such declaration (trimmed).
- ` ```meta-bind-button ` (YAML), ` ```meta-bind-js-view `, and ` ```meta-bind-embed ` blocks.
- Files under `excluded_folders` (default `["templates"]`, a plain path prefix) render a notice
  instead.

### Grammar

```
Ident       := (Letter | '_' | '$') (Alnum | '-' | '_' | '$')*
SQString    := "'" ( '\' any | [^'\\] )* "'"     -- \' -> ', \\ -> \, other \x kept
ArgValue    := SQString | /[^()',]+/
Argument    := Ident ( '(' ws ArgValue (ws ',' ws ArgValue)* ws ')' )?
Arguments   := Argument (ws ',' ws Argument)*
INPUT       := 'INPUT[' ws Ident ws ('(' ws Arguments ws ')')? ws (':' ws BindTarget ws)? ']'
INPUT_TPL   := 'INPUT[' ws Ident (ws Ident)* ws '][' ws Ident? ws ('(' Arguments ')')? (':' BindTarget)? ']'
VIEW        := 'VIEW[' Template ']' ws ( '[' ws Ident? ws ('(' ws Arguments ws ')')? ws (':' ws BindTarget)? ws ']' )?
Template    := Text ( '{' BindTarget '}' Text )*   -- Text unescapes \[ \] \{ \} \\
BUTTON      := 'BUTTON[' ButtonId (ws ',' ws ButtonId)* ']'
BindTarget  := (Ident '^')? (FilePath '#')? PropPath
FilePath    := [^{}\[\]#^|:?]+
PropPath    := First ('.' Ident Bracket*)*      First := Bracket+ | Ident Bracket*
Bracket     := '[' digits ']' | '[' DQString ']'
```

`ws` includes newlines. Argument values are parsed as literals: `null` (any case) → null, exactly
`true`/`false` → boolean, `-?\d+(\.\d+)?` → number, otherwise a string. Property names containing
spaces must be written `["a b"]`.

### Input types

| Type | Value | Default | Inline |
| --- | --- | --- | --- |
| `toggle` | `onValue`/`offValue` (true/false) | false | yes |
| `slider`, `progressBar` | number clamped to `minValue`..`maxValue` (0..100) | `minValue` | slider yes, progressBar block only |
| `text`, `textArea` | string | `""` | yes |
| `number` | number or null | null | yes |
| `select`, `multiSelect` | literal / list | null / `[]` | block only |
| `inlineSelect` | literal | null | yes |
| `suggester`, `imageSuggester` | literal | null | yes |
| `date` | `YYYY-MM-DD` | none | yes |
| `time` | `HH:mm` | none | yes |
| `dateTime` | `YYYY-MM-DDTHH:mm` | none | yes |
| `datePicker` | string in `preferred_date_format` (`YYYY-MM-DD`) | none | yes |
| `editor` | string | `""` | block only |
| `list`, `listSuggester`, `imageListSuggester` | list | `[]` | block only |
| `inlineList`, `inlineListSuggester` | list | `[]` | yes |

Meta Bind's date inputs default to today. Vulcan's library takes no clock, so a missing date
shows empty.

Arguments: `class` (multi), `defaultValue`, `title`, `showcase`, `addLabels`, `minValue`,
`maxValue`, `stepSize`, `onValue`, `offValue`, `option(value[, label])` (multi), `optionQuery`
(multi, a Dataview source) and `optionSource` (multi: `file|folder|tag` with `from:`, `ext:`,
`tag:` filters), both of which Vulcan reports as Obsidian-only with an informational diagnostic
and leaves out of the offered values, `placeholder`, `limit`, `multiLine`, `useLinks` (`true` → `[[path|name]]`, `partial` →
`[[name]]`, `false` → `name`), `allowOther`.

Diagnostics follow Meta Bind's levels. Errors (the field renders as an error): unknown input or view
type, bad bind target or storage type, unresolvable file, duplicate non-multi argument, an inline
field of a block-only type (unless `ignore_code_block_restrictions`), `minValue >= maxValue`.
Warnings (the field still renders): unknown or inapplicable argument, wrong value count, bad
number, unknown template.

### Bind targets

- Default storage is frontmatter of the current note. `File#prop` resolves `File` with Obsidian
  link resolution from the vault root; an unresolvable file is an error.
- `memory^` and `globalMemory^` are not persisted in Meta Bind either. Vulcan renders them as the
  default value with an informational diagnostic, and VIEW write-backs to them are kept for the
  rest of the render, so chained views in one note still compute. `scope^` is an error outside an
  API scope, as in Meta Bind.
- Reading a missing or wrongly typed value shows the type's default (or `defaultValue`). The
  default is not written.
- Writing creates missing intermediate objects or lists and writes typed YAML values (numbers,
  booleans, null, lists). It rewrites only the lines of the top-level property it changes and
  keeps the rest of the frontmatter byte-for-byte, falling back to Vulcan's frontmatter round
  trip only when that edit would not read back correctly.
- `set` refuses targets no input in the note binds (`vulcan note set-property` covers those) and
  memory targets.

### Rendering

- `INPUT` renders its current value in the type's display form (`[x]`/`[ ]` for toggles, the
  selected option's label for selects, `3 / 10` for sliders, a comma list for lists), so terminal
  and Markdown output read naturally. HTML output uses disabled form controls.
- `VIEW` types: `math` (default) substitutes each `{target}` as a mathjs variable and evaluates with
  full mathjs; missing values become `""`; errors render as an inline error. `text` concatenates
  (`null` → `null`, or empty with `view_field_display_null_as_empty`; lists joined with `", "`;
  objects as JSON), with `renderMarkdown` rendering the result. `link` turns a value into a
  wikilink or URL; `image` embeds one target. `hidden` suppresses output.
- `BUTTON` and ` meta-bind-button ` render the label with a "buttons run in Obsidian" notice,
  except the button actions listed under "Buttons" below.
- ` meta-bind-js-view ` blocks render a "runs in Obsidian" notice. `enable_js` is imported so a
  later slice can run them under the render's DataviewJS policy.
- ` meta-bind-embed ` transcludes the linked note like a note embed (max depth 8).

### Editing in place

- `vulcan metabind set <note> <target> <value>` validates the value against every INPUT bound to
  that target in the note (type, options, min/max, limit) and writes it; `--dry-run` shows the
  frontmatter change. `vulcan metabind list <note>` lists fields with their type, target, current
  value, and allowed values (`--output json` for editors).
- `vulcan metabind apply <note>` evaluates VIEW fields with write-back targets
  (`VIEW[…][math:target]`) and writes the results to frontmatter targets, with `--dry-run`. Meta
  Bind writes these continuously while a note is open; Vulcan writes them only on request.
- LSP code actions on an INPUT field: toggles flip directly; selects and inline selects offer one
  action per option; other types run a `vulcan.metabind.set` command that `vulcan.nvim` and
  `vulcan.vim` turn into an input prompt. Edits to open buffers are returned as `TextDocumentEdit`s
  (editor integration principle 4).
- The editor's virtual text shows INPUT values and VIEW results like other dynamic regions.

### Buttons

Button actions that only edit the vault map to explicit commands:
`vulcan metabind press <note> <button-id>` runs `updateMetadata` (with `evaluate: false`, or
`evaluate: true` under the DataviewJS policy), `createNote`, `replaceInNote`,
`regexpReplaceInNote`, `replaceSelf`, `insertIntoNote`, `sleep`, and `js`/`inlineJS` (at the
configured sandbox tier), with `--dry-run`. `command`, `open`, `input`, and the Templater actions
report "runs in Obsidian" diagnostics.

### Settings

`[metabind]` in `.vulcan/config.toml`, importable from `.obsidian/plugins/obsidian-meta-bind-plugin/data.json`:
`preferred_date_format` (`YYYY-MM-DD`), `enable_js` (false), `view_field_display_null_as_empty`
(false), `ignore_code_block_restrictions` (false), `excluded_folders` (`["templates"]`),
`input_field_templates` (`[{name, declaration}]`), `button_templates`.

## 9.42.4 Numerals calculator blocks (implemented)

Numerals evaluates with mathjs; Vulcan bundles mathjs 15.2.0 (Apache-2.0, vendored under
`vulcan-core/vendor/mathjs/` with its license) and a port of the plugin's processing
(`vulcan-core/src/numerals_prelude.js`, MIT, from Numerals 1.10.2) into a dedicated QuickJS engine
with no vault access, so calculator blocks evaluate in every vault, trusted or not, whenever
`js_runtime` is compiled in. Each rendered note that has Numerals regions starts from a clean
note state; the note's metadata and the metadata of the notes its references name are passed in as
data.

Engine cost. Loading mathjs dominates evaluation, so:

- `vulcan-core/build.rs` compiles mathjs and the prelude to QuickJS bytecode at build time, so an
  engine loads in tens of milliseconds instead of compiling about 650 KB of JavaScript. If the
  bytecode does not load (another QuickJS build), the same source is compiled at runtime instead.
- The build applies a small, checked patch to mathjs's bundled typed-function: its signature sort
  remembers per-signature counts instead of recounting them on every comparison, which halves
  the first `math.evaluate` and leaves the sort order unchanged. The vendored file stays as
  released.
- Each thread keeps up to two loaded engines, keyed by the `[numerals]` settings and
  `[js_runtime]` limits, and reuses them for later notes, so the language server, the daemon, and
  renders of many notes pay the load once. Note state (metadata, `$` globals, `@prev`) is reset per
  note. An engine is discarded instead of reused when a call was interrupted or ran out of memory,
  or when an expression changed mathjs itself (`createUnit(...)`, `config(...)`, `import(...)`), so
  one note's units or settings never reach another.

Numerals is off until configured (`[numerals] enabled = true`, or `vulcan config import numerals`,
which turns it on): other tools render ` ```math ` blocks as LaTeX.

### Blocks and inline triggers

| Language | Style |
| --- | --- |
| `math`, `Math` | `default_render_style` (`plain`) |
| `math-plain` | plain |
| `math-tex`, `math-TeX` | TeX |
| `math-highlight` | syntax-highlighted |

Inline code `` `#: expr` `` renders the result; `` `#=: expr` `` renders `expr = result`;
`` `#$: expr` `` and `` `#$=: expr` `` render the same as TeX. Triggers are configurable and
matched longest first.

### Evaluation (per block, one mathjs scope)

1. Cross-note references `[[Note]].key` are replaced by the value of `key` in that note's
   metadata, filtered by its `numerals` key; a failed reference fails the block.
2. `@format <style>` and `@decimalPlaces <n>` directive lines (hidden).
3. Line flags: emitter lines contain `=>` before any `#`; `@[name]`/`@[name::value]` insertion
   lines; `@hideRows`.
4. Rewrites, in order: strip `=>…`; `@[name::old]rest` → `name rest`; `@sum`/`@total` → `__total`;
   `@prev` → `__prev`; remove `@hideRows`; remove thousands commas (`,ddd` → `ddd`, as Numerals
   does, including inside function calls); currency symbols before numbers become units
   (`$1,000` → `1000 USD`).
5. Scope from metadata: keys starting with `$` always, others per the note's `numerals` key
   (`all`, `none`, one key, or a list); lists use their last element, strings are evaluated, keys
   like `f(x)` define functions.
6. Lines evaluate in order. `__prev` is the previous result (comments and blank lines count and
   yield nothing). `__total` sums results since the last blank, comment, or heading line. The first
   error stops the block: earlier lines render, then one error line.
7. Keys starting with `$` assigned in a block are visible to later blocks and inline triggers in
   the same note.

Currency units: `$`→USD, `€`→EUR, `£`→GBP, `¥`→JPY, `₹`→INR (remappable), created with
`createUnit` with the code, its lowercase form, and the symbol as aliases, as Numerals creates
them.

### Output

Markdown renders a plain or highlight block as a `text` code block of aligned
`input → result  # comment` lines (`result_separator`), and a TeX block as a display-math
`aligned` environment of `input \to result` rows. HTML renders the plugin's structure
(`numerals-line`, `numerals-input`, `numerals-result`), with TeX parts as math spans. Hidden lines
are dropped, and when a block has emitter lines only those render (`hide_non_emitters`). Errors
render as a final `input → Name: message` line; frontmatter and reference warnings follow the
block as callouts. Inline results render as text, or inline math for the TeX triggers; inline
errors render `*(Numerals error: …)*`.

QuickJS has no `Intl`, so the locale formats are written out: `system` is the comma-thousands,
period-decimal format (as `comma-period`), which also keeps renders deterministic. Without a
`@decimalPlaces` directive, locale formats show at most three decimals, as `Intl.NumberFormat`
does, falling back to 15 significant digits (or exponential notation) for small nonzero values.
Currency follows Numerals 1.10.2: with `currency_precision = "currency-standard"` the currency's
minor units (ISO 4217: two for most, zero for JPY), followed by the code, or preceded by the
symbol with `currency_display = "symbol"`.

Consecutive inline calculations in one paragraph share `@prev`; a blank line or a block starts a
new section, as the plugin's per-section rendering does.

### Writing results

`@[name]` insertion lines are rewritten to `@[name::result]` only by
`vulcan numerals apply <note>` (`--dry-run`), never by rendering. `vulcan numerals eval <note>`
shows the evaluated regions (`--output json` for the structured form).

### Settings

`[numerals]` in `.vulcan/config.toml`, importable from `.obsidian/plugins/numerals/data.json`:
`enabled` (false), `render_style` (`plain`, `tex`, `highlight`), `result_separator` (`" → "`),
`number_format` (`system`, `fixed`, `exponential`, `engineering`, `comma-period`,
`period-comma`, `space-comma`, `indian`), `currency_precision`, `currency_display`,
`dollar_currency` (`USD`), `yen_currency` (`JPY`), `custom_currency_symbol`,
`custom_currency_code`, `custom_currency_decimal_places` (2), `force_all_frontmatter`,
`hide_non_emitters` (true), `hide_emitter_markup` (true), `inline` (true),
`inline_result_trigger` (`#:`), `inline_equation_trigger` (`#=:`), `inline_tex_result_trigger`
(`#$:`), `inline_tex_equation_trigger` (`#$=:`), `inline_equation_separator` (`" = "`), and
`cross_note_references` (true). The plugin's layout, row-color, suggestion, and autocomplete
settings shape Obsidian's editor only; the importer reports them as skipped.

Meta Bind's `math` views use the same bundled mathjs context.
