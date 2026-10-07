---
name: mdbase-collections
description: Inspect, query, and safely edit an mdbase typed-Markdown collection (a folder with mdbase.yaml), including saved views and effective schemas.
version: 2
tools:
  - help
metadata:
  vulcan:
    managed: true
require_confirmation: false
---

# mdbase Collections

## When to Use This Skill

Use this skill when the vault (or a folder you work in) contains `mdbase.yaml`: its records are
typed by `_types/*.md` files, validated by JSON Schema, and queried with CEL. Ordinary notes outside
the collection keep their normal Vulcan behavior; use the other skills for them.

## Recommended Flow

1. Orient: `vulcan mdbase status --output json` (counts and registry health), then
   `vulcan mdbase types --output json`. `vulcan help mdbase` is the reference.
2. Before writing records of a type, read its shape: `vulcan mdbase schema <type>... --output json`
   lists each field's declaring types, persisted-required rule, read default, and `editable` flag,
   plus `conflicts` and `required_features`.
3. Read: `vulcan mdbase read <path> --metadata --output json` for one record's types, frontmatter,
   and diagnostics; add `--source` only when the exact text matters.
4. Query: write the canonical query to a file and run `vulcan mdbase query --file q.yaml --output json`.
   Top-level names are effective values, `raw.*` is persisted frontmatter, `present.raw.*` /
   `present.record.*` test existence, and `this` is the optional context record.
5. Prefer existing saved views: `vulcan mdbase views --output json`, then
   `vulcan mdbase view <source> <view-id> --output json` (`--context <record>` when the view reads
   `this`). Obsidian `.base` sources selected by `x-obsidian.bases.include` appear there too.
6. To change fields of one record, read its revision with `mdbase read <path> --metadata`, then run
   `vulcan mdbase patch <path> --if-revision <revision> --set key=value --unset key --output json`
   (values are YAML: `priority=3`, `tags=[a, b]`). Only the named persisted fields change; the body,
   comments, and other keys stay as they are, and the report gives the new revision. For body edits
   use the normal note commands, which route collection records through the same validated,
   journaled pipeline. Edit saved views with
   `vulcan mdbase view-source read|update` and `--if-revision`.

## Guardrails

- Do not hand-edit record files to get around a validation error; fix the data or the type. A
  rejected write changed nothing.
- Never write fields whose schema entry says `editable: false`: lifecycle policies generate them
  or the schema marks them read-only. Read the record after a write to see generated values.
- Missing, `null`, empty, and defaulted values are different. A read default appears in effective
  values but does not satisfy a persisted `required` rule; do not persist a default just to make
  it visible.
- `stale_state` or `concurrent_modification` means the data changed underneath you: re-read and
  rebuild the change instead of retrying blindly or forcing it.
- `permission_denied` for mdbase controls means the profile cannot read `mdbase.yaml` or a type or
  contract file. Ask for an authorized profile; do not widen permissions yourself.
- Records you cannot read behave as if they do not exist (`view_not_found`, `context_not_found`,
  absent from results and diagnostics). Do not try to infer them from failures.
- An interrupted write blocks further writes until resolved: run
  `vulcan repair mdbase-write status`, then `recover`, and use `accept-current` only when the user
  confirms the on-disk state is right.
- CEL is not Dataview or the native query DSL; do not mix their syntax. Integral numbers are CEL
  `int`, strings use double or single quotes, and `&&`/`||` combine conditions.

## Example Moves

- "Which open tasks are overdue?" Read `mdbase schema task`, then query
  `types: [task]`, `where: 'status == "open" && present.record.due && due < today()'`.
- "Show the project's task board." `mdbase views`, then `mdbase view task.views project-open
  --context projects/alpha.md`.
- "Rename the saved view." `mdbase view-source read views/tasks.md`, edit the document, then
  `mdbase view-source update views/tasks.md --file doc.md --if-revision <revision>`.
