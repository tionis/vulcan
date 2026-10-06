# Typed Filters

Vulcan uses one filter language across `query --where`, `ls --where`, saved reports, Bases, and
query-driven mutation commands. Filters select indexed notes by property or file metadata; they
are separate from full-text search expressions and from Dataview DQL. (`search --where` still
uses its previous exact-SQL interpretation and is being moved onto the same semantics.)

## Filter shape

Each `--where` value is one predicate or one Vulcan expression:

```text
<field> <operator> <value>
status = "done" || priority > 2
```

A predicate is shorthand for the expression it names, so both forms mean exactly what the
expression evaluator says (the same evaluator Bases and Dataview expressions use).

Repeat the flag to combine predicates with logical `AND`:

```bash
vulcan query \
  --where 'status = active' \
  --where 'due <= 2026-04-01'
```

Write `OR`, parentheses, and negation inside one `--where` value as an expression with `||`,
`&&`, and `!`. A predicate's value must be one literal; quote values containing whitespace.

## Fields

A field may be any indexed property key, such as `status`, `due`, `reviewed`, or `tags`. Vulcan
also exposes these file fields:

- `file.path`
- `file.name`
- `file.ext`
- `file.mtime`
- `file.ctime`
- `file.tags`

Use explicit `file.*` names for filesystem metadata. Other names address frontmatter or inline
properties, matched like the expression evaluator matches them (exact key first, then a key with
the same normalized name such as `Due Date` for `due-date`). When frontmatter and an inline field
both set a key, its value is a list of every value.

## Operators

The supported operators are:

| Operator | Intended use |
| --- | --- |
| `=` | Equality, including booleans and `null` |
| `!=` | Inequality; a missing property is `null` |
| `>` | Greater than |
| `>=` | Greater than or equal |
| `<` | Less than |
| `<=` | Less than or equal |
| `starts_with` | Byte-exact, case-sensitive text prefix (no wildcards) |
| `contains` | Like `contains()`: a list element, or a substring of text |
| `has_tag` | `file.tags` only: the tag or a tag nested under it |
| `matches` | Case-sensitive Rust regular expression search over text or list elements |
| `matches_i` | Case-insensitive Rust regular expression search over text or list elements |

A missing property is `null`: `status != done` also selects notes without a status, and
`field != null` selects notes that set the field. Values of different kinds are unequal and
unordered, so `priority > 1` does not match the text `"2"`. `file.tags contains tag` is the same
tag selection as `has_tag`. Use an expression for anything else, such as `!contains(tags, "x")`.

## Values

Values are parsed as these types:

- Text: `done`, `"In Progress"`, or `'Rule Index'`
- Boolean: `true` or `false`
- Null: `null`
- Number: `42` or `3.5`
- Date or datetime text: `2026-04-01` or `2026-04-01T09:30:00Z`
- `file.mtime`: integer milliseconds since the Unix epoch

Quote values containing whitespace. Dates are written directly; `date(...)` is not part of this
grammar.

## Examples

```bash
vulcan query --where 'status = active'
vulcan query --where 'reviewed = true'
vulcan query --where 'due <= 2026-04-01'
vulcan query --where 'tags contains project'
vulcan query --where 'file.path starts_with "Projects/"'
vulcan query --where 'file.name matches "^2026-"'
vulcan query --where 'owner matches_i "^(eric|sam)$"'
vulcan query --where 'archived = null'
vulcan query --where 'status != done'
vulcan query --where 'file.tags has_tag project'
vulcan query --where 'status = "active" || length(owner) > 0'
```

Filters compose with commands that provide their own selection or mutation behavior:

```bash
vulcan search release --where 'team = platform'
vulcan ls --where 'file.path starts_with "Daily/"'
vulcan note update --where 'status = draft' --key status --value done --dry-run
vulcan refactor rewrite --where 'file.path starts_with "Archive/"' \
  --find TODO --replace DONE --dry-run
```

Always preview a mutating command with `--dry-run` when it supports one.

## Filters, native queries, and search

- Use repeated `--where` flags for short, programmatic conjunctions.
- Use the [native query DSL](query-dsl.md) for one structured expression with projection,
  ordering, limits, or offsets.
- Use `search` for ranked note text and add `--where` only when metadata should narrow those
  results.
- Use `vulcan query --language dql` for Dataview Query Language. DQL has a different grammar and
  should not be copied into `--where`.

Run `vulcan help query`, `vulcan help search`, or `vulcan help query-dsl` for the surrounding
command syntax.
