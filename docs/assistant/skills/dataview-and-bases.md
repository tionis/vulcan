---
name: dataview-and-bases
description: Work with Dataview, DataviewJS, Bases, and .base files. Use when the user asks about Dataview DQL, inline fields, DataviewJS blocks, Bases views, formulas, saved task views, or .base editing/evaluation.
version: 9
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

- Use `vulcan dataview query` for DQL strings and `vulcan dataview eval` for indexed Dataview blocks.
- Use `vulcan dataview query-js` only when JavaScript behavior matters.
- Use `vulcan bases eval <file>` to inspect `.base` output and diagnostics.
- Use shared `query` when the workflow does not require Dataview-specific syntax.
- Read diagnostics before editing view definitions; unsupported syntax should surface as diagnostics.

## Guardrails

- Prefer canonical `query` for agent workflows unless the user specifically needs Dataview/Bases compatibility.
- DQL `WHERE` clauses and `.base` filters compare like the expression evaluator: a missing property is `null`, so `status != "done"` also selects notes without a `status`, and a property that a frontmatter key and an inline field of the same name both set is a list. Write `status != null && status != "done"` when only notes that have the property should match.
- `query --language dql --explain` and `bases eval --explain` add the note plan (candidates, decided and excluded rows, rows left to the DQL or Bases evaluator, hydrated notes, stage timings) to the output.
- DQL `FROM "Folder"` matches the folder's exact, case-sensitive spelling and everything below it; `FROM #tag` includes nested tags (`#tag/sub`) but not tags that merely share a prefix (`#tags`).
- DataviewJS runs inside Vulcan's JS sandbox; write/network helpers depend on sandbox and permissions.
- With a read-scoped query, `file.inlinks` describes readable backlink sources, including readable notes outside the query's selected rows. Do not treat its count as a whole-vault count or broaden the grant to discover hidden sources. Authored outgoing links in readable notes are still source content, not proof that their targets are readable.
- `bases eval`, `bases tui`, and saved Bases reports keep the selected `--permissions` scope. The `.base` file must be readable. Rows, `file.inlinks`, and linked-note values such as `link.asFile()` come only from notes that pass path/tag grants and the policy hook. A link to a hidden note behaves like a link to a missing one, so a null or failed formula is not proof the target is absent. A broken policy hook or a changed grant is an error, not an empty view. `vulcan browse` refuses restricted profiles; use `bases eval` instead.
- `bases create` (and note creation from `bases tui`) needs read access to the `.base` file and write access to the derived note path, not to the `.base` file. Like other note creates, it also needs read access to `mdbase.yaml` for routing. The view's template is chosen only among readable templates and rendered with the same authority. Use `--dry-run` to preview the path; a denial is not a reason to broaden the profile.
- DQL (`dataview query`, `dataview eval`, `query` in DQL mode, MCP DQL) and `dataview inline` use the same scope. Tag-granted notes are included, policy-hidden notes and their backlinks are not, and a broken hook is an error. An empty result under a restricted profile describes only that scope.
- `.base` edits should preserve view structure and formulas; avoid broad text rewrites.

## Example Moves

- Explain why a Dataview query returns different rows than a property query.
- Evaluate a `.base` file and patch one view filter.
- Convert a working DQL query into a reusable Vulcan query or saved report.
