# mdbase Collections

A directory with an `mdbase.yaml` file is an mdbase collection: ordinary Markdown records whose
frontmatter is described by portable type files, validated by JSON Schema, and queried with CEL.
Vulcan implements the mdbase v0.3 specification pinned at upstream commit `68b9a979`, and keeps
the files canonical: the cache under `.vulcan/` is derived and rebuildable.

Vulcan stays an Obsidian-compatible vault tool at the same time. Notes outside a collection, and
every ordinary command, behave exactly as before. mdbase semantics apply only to the explicit
`vulcan mdbase ...` commands and to writes that land on collection records.

## Layout

```text
mdbase.yaml            spec_version: "0.3.0", settings, optional x-* extensions
_types/task.md         kind: mdbase.type (name, match rules, schema, collection rules, lifecycle)
_contracts/...         kind: mdbase.contract (portable data contracts)
tasks/write-docs.md    a record: frontmatter + body
views/tasks.md         a saved-view record (type: view)
```

`settings.types_folder`, `settings.contracts_folder`, `settings.exclude`, record extensions, the
type key, the validation level, and the collection timezone are configured in `mdbase.yaml`.
Keys starting with `x-` are extensions and never produce unknown-key warnings.

## Reading

| Command | Purpose |
| --- | --- |
| `vulcan mdbase status` | Collection discovery, record/type/contract counts, registry health |
| `vulcan mdbase types` / `contracts` | Type and contract definitions with source revisions |
| `vulcan mdbase schema TYPE...` | Effective schema of one type composition (see below) |
| `vulcan mdbase validate [PATH]` | Validation diagnostics for every readable record, or one |
| `vulcan mdbase read PATH [--metadata\|--source]` | One complete record; `--metadata` skips body, links, and tags |
| `vulcan mdbase query QUERY` / `--file` | A canonical mdbase query (YAML or JSON) |

All of them accept `--output json` and return canonical mdbase envelopes. None of them changes the
collection; an initialized vault may update its rebuildable cache.

### Queries

```yaml
types: [task]
where: 'status != "done" && priority >= 2'
projections:
  overdue: { expr: 'present.record.due && due < today()' }
select: [title, status, projection.overdue]
order_by: [{ field: priority, direction: desc }]
limit: 20
```

Top-level names are effective values (read defaults applied); `raw.*` is the persisted frontmatter,
`present.raw.*` and `present.record.*` say whether a field exists, `file.*` is file metadata, and
`this` is the optional invocation context (`context: { this: { path: projects/alpha.md } }`).
Integral numbers are CEL `int`, so `priority * 2` works. Simple shapes (type selection, `&&` of
field comparisons and `startsWith` against literals, field selections, ordering, limits) run from
the index without loading bodies.

### Effective schemas

`vulcan mdbase schema task tracked` describes a record matched by both types: each type's source
and `schema_revision`, and every field with its declaring types, whether persisted frontmatter
requires it (a read default does not satisfy that), its read default, the lifecycle events that
generate it, and whether it is `editable`. A non-empty `conflicts` list means that composition
cannot be written until the type files agree; `required_features` names what a client must
support (`vulcan.lifecycle.v1`, `links`, `cel`, `cel_match`).

## Saved views

A record matched by the `view` type stores a shared query fragment and named views:

```yaml
---
type: view
id: task.views
version: 1
name: Task views
query:
  types: [task]
  where: 'status != "archived"'
views:
  - id: open
    name: Open tasks
    where: 'status == "open"'
    select: [title, priority]
    order_by: [{ field: priority, direction: desc }]
    presentation: { type: tasknotes.task-list }
---
```

- `vulcan mdbase views` lists every visible source with its revision and named-view properties.
- `vulcan mdbase view SOURCE VIEW` runs one; `SOURCE` is the record path or its `id`. Shared and
  named `where` combine with AND, projections merge, and a named `context` replaces the shared one.
- `context.this.on_missing` decides what `this` is without `--context`: `view` (the view record,
  default), `null`, or `error` (`context_required`). `--context PATH` always wins and must match the
  declared context `types` (`context_type_mismatch`); `--no-context` binds null.
- `--limit`, `--offset`, and `--timezone` override only this run. Presentation is advisory: every
  view runs headlessly, and rendered output is reported `unsupported_presentation`.
- `vulcan mdbase view-source read|create|update|delete` exchanges complete documents. Invalid
  documents fail with `invalid_view` before anything is written, `create` never replaces a file
  (`path_conflict`), and `update`/`delete` take `--if-revision` (`concurrent_modification`).

### Obsidian bases

```yaml
x-obsidian:
  bases:
    include: ['TaskNotes/Views/**/*.base']
```

Selected `.base` files are listed as `obsidian.base` sources (the path is the source ID; view IDs
derive from view names, `Open Work` → `open-work`, repeats get `-2`). They run with Vulcan's Bases
evaluator, not CEL, and stay authoritative: nothing is converted. An explicit `--context` is not
supported for bases yet (`unsupported_context`).

## Writing

Collection records are written through one validated pipeline: `note set`, `note patch`,
`note append`, `note create`, property `update`/`unset`, moves and renames, scripts, and
`view-source` all plan the complete change, check permissions and uniqueness across everything they
affect, apply lifecycle policies, journal the batch, and replace files atomically. A failure leaves
no partial write. `vulcan repair mdbase-write status|recover|accept-current` inspects and resolves
an interrupted write.

## Permissions

Every mdbase command honors `--permissions PROFILE`. Reading anything requires read access to
`mdbase.yaml` and every type and contract file; without it commands fail with `permission_denied`
instead of applying a less restrictive schema. Records outside the read scope are absent: they are
not listed, not valid link or context targets, and do not appear in diagnostics, views, or change
listings. Writes need read and write access to every affected path.

## Hosts and change notifications

`vulcan serve` (and the daemon's vault host) exposes `GET /mdbase/query`, `/mdbase/read`,
`/mdbase/types`, `/mdbase/contracts`, `/mdbase/schema?type=`, `/mdbase/views`, and
`/mdbase/view?source=&view=`. With watching enabled, `GET /mdbase/changes?after=N` returns numbered
notifications (`controls_changed` and the changed paths) that are published only after the derived
cache reflects them; `reconcile: true` means older notifications were dropped and the client should
re-read what it shows. Without a watcher the route answers 501.

Loading a collection is passive: opening configuration, types, contracts, provider- or
workflow-shaped records, or views never runs lifecycle policies, plugins, or embedded scripts.

## Conformance and features

`vulcan mdbase conformance` runs the pinned upstream suites and Vulcan's native feature gates;
`--claim` emits a machine-readable claim only when every required case passes.

| Name | Kind | Since |
| --- | --- | --- |
| `core_read`, `collection_semantics` | upstream profile | MDB.4 |
| `cel`, `cel_match`, `cel_query`, `links` | upstream profile | MDB.5, MDB.6 |
| `view_records`, `writable_view_sources` | upstream optional feature | MDB.8 |
| `vulcan.record_write.v1`, `vulcan.lifecycle.v1` | Vulcan feature | MDB.7 |
| `vulcan.saved_views.v1` | Vulcan feature | MDB.8 |

Not claimed: upstream `core_write` and `lifecycle` (managed type packs are deferred) and
`obsidian_bases_views` (no oracle corpus captured from Obsidian exists yet).

### When a collection or feature is absent

- Without `mdbase.yaml`, `vulcan mdbase ...` commands fail with `not an mdbase collection: missing
  mdbase.yaml`, and every other command treats the files as ordinary notes.
- A feature that is not claimed is simply not offered: for example, base views reject an explicit
  context, and rendered view output is unsupported. Nothing silently approximates it.

### Upgrade notes

- The specification revision is pinned. Upgrading it is an explicit, reviewed change of the bundled
  schemas and suites; claims name the pinned commit in `x-vulcan-upstream-commit`.
- Type definitions now report `revision` and `schema_revision`, and contracts report `revision`;
  clients that compare whole definitions should expect these members.
- `x-*` keys in `mdbase.yaml` no longer produce `unknown_config_key` warnings.
- Integral frontmatter numbers bind as CEL `int` instead of `uint`; arithmetic such as
  `priority * 2` that previously failed now evaluates.
- `this` in queries now uses the context record's own schema fields, so `this.missing == null` and
  `this.present.raw.*` evaluate instead of failing when the candidate's type lacks those fields.
