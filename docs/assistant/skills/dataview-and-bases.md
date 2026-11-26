---
name: dataview-and-bases
description: Work with Dataview, DataviewJS, Bases, and .base files. Use when the user asks about Dataview DQL, inline fields, DataviewJS blocks, Bases views, formulas, saved task views, or .base editing/evaluation.
version: 18
tools:
  - dataview
  - bases
  - query
  - help
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# Dataview and Bases

## When to Use This Skill

Use this skill for Obsidian Dataview compatibility, `.base` files, formula/view troubleshooting,
and translating view logic into Vulcan queries.

## Recommended Flow

- To see a note the way Obsidian shows it, read it with `vulcan note get <note> --evaluate`: inline expressions, `dataview`, `tasks`, and embedded `.base` views are replaced by their results. `dataviewjs` blocks run in the strict sandbox (no file access, network, or writes; the clock is readable) by default, and with full evaluation (the configured sandbox tier, still read-only) in a vault trusted with `vulcan trust`. The vault's `[dataview] render_dataview_js` can lower that, but its `on` needs trust. `--dataviewjs off|static|on` chooses explicitly for one command; do not reach for `on` in a vault the user has not trusted. `vulcan render <file>` evaluates the same way for terminal output.
- Use `vulcan dataview query` for DQL strings and `vulcan dataview eval` to debug individual Dataview blocks and their raw result data.
- Use `vulcan dataview query-js` only when JavaScript behavior matters.
- Use `vulcan bases eval <file>` to inspect `.base` output and diagnostics.
- Use shared `query` when the workflow does not require Dataview-specific syntax.
- Read diagnostics before editing view definitions; unsupported syntax should surface as diagnostics.

## Guardrails

- Prefer canonical `query` for agent workflows unless the user specifically needs Dataview/Bases compatibility.
- DQL `WHERE` clauses and `.base` filters compare like the expression evaluator: a missing property is `null`, so `status != "done"` also selects notes without a `status`, and a property that a frontmatter key and an inline field of the same name both set is a list. Write `status != null && status != "done"` when only notes that have the property should match.
- Base views may set `limit` (rows kept after sorting), as in Obsidian Bases.
- `query --language dql --explain` and `bases eval --explain` add the note plan (candidates, decided and excluded rows, rows left to the DQL or Bases evaluator, rows loaded with stored fields only, hydrated notes, stage timings) to the output.
- DQL `TABLE`/`LIST` results give links as the vault paths they resolve to (`[[People/Sven]]`, even when the note wrote `[[../../People/Sven]]` or `[[Sven]]`); unresolved links stay as written. Rendered Markdown adds the title (`[[People/Sven|Sven]]`).
- A grouped DQL result names its key column after the `GROUP BY` field (`GROUP BY status` → `status`, `GROUP BY x AS kind` → `kind`), as Dataview does; JSON output names it in `group_column`.
- An inline value of comma-separated links, numbers, dates, tags, booleans, or quoted strings is a list (`people:: [[Ann]], [[Bob]]`), as in Dataview; plain words (`a, b`) stay text.
- DQL `FROM "Folder"` matches the folder's exact, case-sensitive spelling and everything below it (`FROM ""` is the whole vault); `FROM #tag` includes nested tags (`#tag/sub`) but not tags that merely share a prefix (`#tags`).
- DataviewJS runs inside Vulcan's JS sandbox; write/network helpers depend on sandbox and permissions.
- Inline DataviewJS (`` `$= ...` ``) is evaluated only when the vault sets `[dataview] enable_inline_dataview_js = true` (off by default, as in Dataview), and then follows the same `--dataviewjs` policy as blocks. A note's inline expressions and `dataviewjs` blocks share one scope, run top to bottom: `` `$= test = 3` `` then `` `$= test + 3` `` shows `6`, and a block can define helpers with a plain assignment (`fmt = (x) => …`) for later expressions; block `const`/`let` stay local, and a block may use `await`. `vulcan dataview inline <note>` lists inline DQL values and, under `js_results`, the inline DataviewJS values. Obsidian comments (`%% ... %%`) are hidden in rendered output.
- As in Dataview, `dv.query()`, `dv.tryQuery()`, `dv.queryMarkdown()`, `dv.io.load()`, `dv.io.csv()`, and `dv.view()` return promises: `await` them in a block, or use `vault.query()` for a synchronous query in Vulcan-only scripts. A promise rejected with no handler fails the block with its error. `dv.evaluate()`/`dv.tryEvaluate()`, `dv.pagePaths()`, `dv.markdownTable()`, `dv.markdownList()`, and `dv.markdownTaskList()` match Dataview.
- Links in DataviewJS are Dataview `Link` objects with a resolved `path` (for example `People/Alice.md`), not `[[...]]` strings: compare `link.path`, pass a link to `dv.page(link)`, and use `String(link)` or `link.markdown()` for text.
- Date and duration fields in DataviewJS are Luxon-like `DateTime` and `Duration` objects (also `file.mtime`/`file.ctime`), not strings: use `date.toFormat("yyyy-MM-dd")`, `date < dv.date("today")`, `date.plus({ days: 1 })`, and `duration.as("minutes")`; `dv.parse()` and `dv.literal()` type raw text and plain values the way Dataview does.
- Fields on plain list items (`- item [cost:: 12]`) are page fields, as in Dataview, while task fields stay on the task. DataviewJS pages also answer to canonical field names such as `page["due-date"]` for `Due Date::`.
- `await dv.query()`/`dv.tryQuery()` give Dataview's result (`result.type`, `result.values`, and `result.headers` for tables), not Vulcan's `rows`; `vault.query()` returns Vulcan's `{ columns, rows }`.
- DataviewJS list and task items match Dataview: `line` is 0-based, `position.start.offset`/`position.end.offset` slice the note's text, and tasks do not carry page fields (read them with `dv.page(task.path)`).
- DataviewJS `file.tags` and `file.etags` include the `#` (`page.file.tags.includes("#project")`), and `file.tags` also lists parent tags.
- CustomJS classes (`[customjs] js_files`/`js_folder`, importable with `vulcan config import customjs`) are available in every DataviewJS context as `customJS.<ClassName>` and through `await cJS()`, as in Obsidian. `vulcan plugin customjs list` shows the loaded classes and load errors. `[customjs.dql_functions]` (Vulcan-only) maps a DQL function to a class method (`fmt = "Util.fmt"` makes `` `= fmt(this.price)` `` work); those calls see only their arguments, must return synchronously, and fail as unknown functions in Obsidian.
- Meta Bind fields (`` `INPUT[toggle:done]` ``, `` `VIEW[{hp} / {max_hp}]` ``, `BUTTON[id]`, ` ```meta-bind ` blocks) render with the values they bind to. Never edit the frontmatter behind an input by hand when a field exists: `vulcan metabind list <note>` shows each field's target and allowed values, `vulcan metabind set <note> <target> <value> --dry-run` checks the value against every input bound to it before writing, and `vulcan metabind apply <note>` writes the results of `VIEW[...][math:target]` views and the `save to {target}` results of ` ```meta-bind-js-view ` blocks (which run only with `metabind.enable_js`). `vulcan metabind press <note> <button-id> --dry-run` shows what a button's actions would change before pressing it for real; actions that need Obsidian (commands, links, Templater) are reported, not run.
- Numerals calculator blocks (` ```math `) and inline calculations (`` `#: 2 + 2` ``) render where `numerals.enabled` is on (`vulcan config import numerals`); `vulcan numerals eval <note>` shows the results and `vulcan numerals apply <note> --dry-run` previews the `@[name::result]` lines it would write. Rendering never writes them. See `docs/guide/live-documents.md`.
- Rendered reads run `dataviewjs` read-only: a block that writes or executes shows an error there instead. Use `dataview eval` only when the user wants the block's side effects.
- Evaluated reads (`note get --evaluate`, `note get --mode html`, `render`, browse previews) run every query under the selected `--permissions` profile, so tables and lists cover only readable notes. A short table under a restricted profile is not a reason to broaden the profile.
- With a read-scoped query, `file.inlinks` describes readable backlink sources, including readable notes outside the query's selected rows. Do not treat its count as a whole-vault count or broaden the grant to discover hidden sources. Authored outgoing links in readable notes are still source content, not proof that their targets are readable.
- `bases eval`, `bases tui`, and saved Bases reports keep the selected `--permissions` scope. The `.base` file must be readable. Rows, `file.inlinks`, and linked-note values such as `link.asFile()` come only from notes that pass path/tag grants and the policy hook. A link to a hidden note behaves like a link to a missing one, so a null or failed formula is not proof the target is absent. A broken policy hook or a changed grant is an error, not an empty view. `vulcan browse` scopes every surface to the selected profile (lists, search, tags, links, doctor rows, calendar, previews, and the embedded Bases TUI show only readable notes; edits, new notes, moves, Git history, and auto-commit need their grants) but refuses profiles with a policy hook; use `bases eval` there.
- `bases create` (and note creation from `bases tui`) needs read access to the `.base` file and write access to the derived note path, not to the `.base` file. Like other note creates, it also needs read access to `mdbase.yaml` for routing. The view's template is chosen only among readable templates and rendered with the same authority. Use `--dry-run` to preview the path; a denial is not a reason to broaden the profile.
- DQL (`dataview query`, `dataview eval`, `query` in DQL mode, MCP DQL) and `dataview inline` use the same scope. Tag-granted notes are included, policy-hidden notes and their backlinks are not, and a broken hook is an error. An empty result under a restricted profile describes only that scope.
- `.base` edits should preserve view structure and formulas; avoid broad text rewrites.

## Example Moves

- Explain why a Dataview query returns different rows than a property query.
- Evaluate a `.base` file and patch one view filter.
- Convert a working DQL query into a reusable Vulcan query or saved report.
