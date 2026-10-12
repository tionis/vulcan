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

- To see a note the way Obsidian shows it, read it with `vulcan note get <note> --evaluate`: inline expressions, `dataview`, `tasks`, and embedded `.base` views are replaced by their results. `dataviewjs` blocks run in the strict sandbox (deterministic; no file access, network, or writes) by default, and with full evaluation (the configured sandbox tier, still read-only) in a vault trusted with `vulcan trust`. The vault's `[dataview] render_dataview_js` can lower that, but its `on` needs trust. `--dataviewjs off|static|on` chooses explicitly for one command; do not reach for `on` in a vault the user has not trusted. `vulcan render <file>` evaluates the same way for terminal output.
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
- DQL `FROM "Folder"` matches the folder's exact, case-sensitive spelling and everything below it (`FROM ""` is the whole vault); `FROM #tag` includes nested tags (`#tag/sub`) but not tags that merely share a prefix (`#tags`).
- DataviewJS runs inside Vulcan's JS sandbox; write/network helpers depend on sandbox and permissions.
- Inline DataviewJS (`` `$= ...` ``) is evaluated only when the vault sets `[dataview] enable_inline_dataview_js = true` (off by default, as in Dataview), and then follows the same `--dataviewjs` policy as blocks. Obsidian comments (`%% ... %%`) are hidden in rendered output.
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
