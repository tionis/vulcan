# Typed Filters

Vulcan uses one typed predicate grammar across `query --where`, `search --where`,
`ls --where`, saved reports, and query-driven mutation commands. Filters select indexed notes by
property or file metadata; they are separate from full-text search expressions and from Dataview
DQL.

## Filter shape

Each `--where` value contains exactly one predicate:

```text
<field> <operator> <value>
```

Repeat the flag to combine predicates with logical `AND`:

```bash
vulcan query \
  --where 'status = active' \
  --where 'due <= 2026-04-01'
```

The shortcut grammar does not currently support `OR`, parentheses, or `and` inside one `--where`
value. Use the native query DSL when several predicates should be written in one expression.

## Fields

A field may be any indexed property key, such as `status`, `due`, `reviewed`, or `tags`. Vulcan
also exposes these file fields:

- `file.path`
- `file.name`
- `file.ext`
- `file.mtime`

Use explicit `file.*` names for filesystem metadata. Other names address frontmatter or inline
properties.

## Operators

The supported operators are:

| Operator | Intended use |
| --- | --- |
| `=` | Equality, including booleans and `null` |
| `>` | Greater than |
| `>=` | Greater than or equal |
| `<` | Less than |
| `<=` | Less than or equal |
| `starts_with` | Text-prefix matching |
| `contains` | Membership in list-valued properties such as `tags` |
| `matches` | Case-sensitive Rust regular expression over text |
| `matches_i` | Case-insensitive Rust regular expression over text |

There is no `!=`, `ends_with`, `in`, `not in`, `is null`, or `is not null` operator. Test null with
`field = null`; select non-null values using a more specific positive predicate.

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
