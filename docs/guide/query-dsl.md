# Native Query DSL

Vulcan's native query DSL is the structured language accepted by `vulcan query` and other
query-aware workflows. It selects indexed notes, applies typed predicates, projects fields, orders
results, and bounds the result window.

It is distinct from Dataview Query Language (DQL). Use `--language vulcan` to force this grammar,
or leave `--language auto` in place when the input is unambiguous.

## Grammar

```text
from notes
  [where <field> <operator> <value> [and <field> <operator> <value>...]]
  [select <field>[, <field>...]]
  [order by <field> [asc|desc]]
  [limit <count>]
  [offset <count>]
```

`notes` is currently the only native source. Select tags or folders with predicates rather than
inventing another source:

```text
from notes where tags contains project
from notes where file.path starts_with "Projects/"
```

`from #project`, saved-source names, and `sort by` are not native DSL syntax. Dataview DQL does
support tag sources, but it is a separate language.

## Predicates

The DSL accepts the same typed fields, values, and operators as [`--where` filters](filters.md):

```text
=  >  >=  <  <=  starts_with  contains  matches  matches_i
```

Join predicates with `and`:

```text
from notes where status = active and due <= 2026-04-01
```

The native DSL does not currently support `OR`, parentheses, or negation. Write `null` directly
for a null value, and write dates directly rather than using `date(...)`.

## Projection, ordering, and paging

Use `select` to return particular fields:

```text
from notes where status = active select file.path, owner, due
```

Use `order by`, not `sort by`, for ordering:

```text
from notes order by file.mtime desc
```

`limit` bounds the number of results, while `offset` skips an initial result window:

```text
from notes order by file.path asc limit 25 offset 50
```

## CLI examples

```bash
# Bare query defaults to `from notes`.
vulcan query

vulcan query 'from notes where status = done order by file.mtime desc limit 10'
vulcan query 'from notes where tags contains sprint and reviewed = true'
vulcan query --format paths 'from notes where file.name matches "^2026-"'
vulcan query --glob 'Projects/**' 'from notes'
vulcan query --explain 'from notes where status = backlog'
```

The shortcut form builds the same kind of note query without writing the DSL:

```bash
vulcan query --where 'status = done' --sort due
```

Repeat `--where` in shortcut form. Do not put `and` inside one shortcut filter.

## DQL and language selection

Automatic language selection recognizes inputs beginning with `TABLE`, `LIST`, `TASK`, or
`CALENDAR` as Dataview DQL. Force the intended parser when input comes from an external system:

```bash
vulcan query --language vulcan 'from notes where status = active'
vulcan query --language dql 'TABLE status FROM #project'
```

Native DSL sources and DQL sources are not interchangeable.

## JSON form

Automations may pass the canonical query AST as JSON:

```json
{
  "source": "notes",
  "predicates": [
    {"field": "status", "operator": "eq", "value": "done"}
  ],
  "sort": {"field": "file.mtime", "descending": true},
  "limit": 10,
  "offset": 0
}
```

```bash
vulcan query --json \
  '{"source":"notes","predicates":[{"field":"status","operator":"eq","value":"done"}]}'
```

Use `vulcan describe` when a caller needs the runtime machine-readable schema. Use
`vulcan --output json query ...` to obtain structured results; this output choice is independent of
the query input language.

See also `vulcan help filters`, `vulcan help query`, and the [CLI guide](../cli.md).
