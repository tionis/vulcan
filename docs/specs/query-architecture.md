# Shared query planning and execution

Status: proposed. Roadmap track: [QRY](../ROADMAP.md#qry-shared-query-planning-and-execution).

## 1. Purpose and scope

Vulcan has grown several query frontends. Each frontend parses its own syntax and keeps its own
expression semantics. Planning, candidate selection, freshness, permission scoping, and record
hydration have also been reimplemented per frontend, with very different performance. This
specification defines layers that frontends share so that an optimization or fix made for one
query language serves all of them.

In scope:

- frontends that select, filter, order, and page notes or collection records: Dataview DQL,
  Obsidian Bases, the canonical `QueryAst` (`notes`, `query`, saved reports, serve handlers),
  mdbase queries, the Tasks plugin query DSL, and DataviewJS `dv.pages()`;
- the predicate, source, and plan representations those frontends compile into;
- the physical layer: candidate selection, freshness proofs, hydration, and retained host
  sessions.

Out of scope:

- unifying expression languages: Dataview/Bases expressions and CEL keep their own parsers,
  evaluators, diagnostics, and semantics;
- full-text and vector search, which have their own indexes and ranking;
- changing any frontend's user-visible results, except to fix a demonstrated bug in a separate,
  reviewed change.

## 2. Current state

| Frontend | Plan representation | SQL lowering | Data source | Freshness | Hydration |
| --- | --- | --- | --- | --- | --- |
| `QueryAst` (`notes`, `query`, serve) | `QueryAst` → `NoteQuery` | `FilterExpression` conditions, exact | note tables (`documents`, `properties`, …) | incremental scan before the command | rows per result |
| DQL | `CompiledDqlQuery` | `FROM` tags and simple `WHERE` → `FilterExpression`; the SQL result is used as exact, with no residual | whole-vault `NoteRecord` index | incremental scan | every note (list items scoped since `9c9ca7cd`) |
| Bases | Bases view model | `query_notes` candidates for supported filters | `query_notes` plus the whole-vault index for formulas and links | incremental scan | every note |
| Tasks DSL | own AST | none | whole-vault index | incremental scan | every note |
| DataviewJS `dv.pages()` | JavaScript | none | whole-vault index | incremental scan | every note |
| mdbase | `StructuredQueryPlan` + `MdbasePreparedQuery` | `MdbaseSqlPredicate`: conjunctive atoms decided exactly when type-valid, otherwise residual CEL | record cache (`mdbase_record_cache`, narrow `mdbase_record_query`) | stat-fingerprint proof, optionally retained and watched | page only |

Observations:

1. **Two universes.** Notes are loaded into an eager, whole-vault `HashMap<String, NoteRecord>`
   built from note tables. mdbase records come from a separate cache with stat-proven
   freshness, per-record identity facts, and on-demand hydration. Dozens of call sites load the
   whole note index.
2. **Two predicate lowerings with different contracts.** `FilterExpression` (used by `QueryAst`,
   DQL, and Bases) is treated as exact: DQL keeps only the SQL matches and never re-evaluates
   them, so any divergence between SQL and Dataview comparison semantics changes results, and
   no differential test checks it. `MdbaseSqlPredicate` is three-valued: an atom decides a row
   only when the value's type makes SQL and CEL agree, everything else is residual, and an
   in-memory decider is differentially tested against the SQL rendering.
3. **Duplicated source algebra.** DQL's `FROM` (folder, tag, incoming/outgoing link, `and`/`or`/
   `not`), `QueryAst` sources, Bases `file.inFolder`/`file.hasTag` filters, and mdbase type and
   path-prefix selection each select candidate sets in their own way.
4. **Measured cost of whole-vault materialization.** On a 2,941-note personal vault, a folder
   `LIST` query took about 320 ms (about 120–145 ms after `9c9ca7cd`). The equivalent warm mdbase
   query took about 75 ms. Most of the Dataview cost is loading and hydrating notes the query
   never reads.
5. **`StructuredQueryPlan` is already documented** as the rich plan "shared by frontends whose
   semantics exceed `QueryAst`", but only mdbase compiles into it.

## 3. Principles

1. **Frontends own syntax and expression semantics.** Shared layers never reinterpret a
   frontend's expression. Anything the shared layers cannot decide exactly is evaluated by the
   frontend's own evaluator.
2. **Lowered predicates are exact-or-undecided.** Every atom lowered to SQL carries the
   dialect whose comparison semantics it implements. For each row it yields *match*,
   *no match*, or *undecided*, and undecided rows go to residual evaluation. A lowering may
   never turn a residual match into a non-match. Every dialect has a differential test of the
   SQL rendering and the in-memory decider against the frontend evaluator.
3. **Permissions scope the universe before hydration.** Candidate SQL narrows work; it is never
   authorization evidence. Tag grants, policy hooks, and hidden-equals-absent rules are applied
   exactly as the guarded index load applies them today.
4. **The vault stays canonical.** New tables are rebuildable cache state. Freshness proofs use
   the same stat-fingerprint standard as mdbase indexed reads, including its documented limit
   (an in-place, same-size rewrite within one timestamp tick).
5. **Lazy by default, bounded by plan.** Records are hydrated by field group, for candidates
   and pages only, and link dereferences load the target on demand. Whole-collection work is
   reserved for queries that need it (no `FROM`, global aggregates), and is visible in work
   counters.
6. **Every step is gated by equivalence.** Each migration keeps results identical on the
   fixture vaults and on representative user-shaped vaults, and records before/after timings.
   Where the previous behavior was accidental (for example SQL that disagreed with the
   frontend's own evaluator), the step instead fixes it deliberately, with a regression test
   and a skill review.
7. **One filter semantics per expression family.** Vulcan is pre-alpha, so semantics are
   chosen for coherence rather than preserved for compatibility (§4.7).

## 4. Target architecture

The layers below are listed bottom-up. Names are indicative; the first work item that
introduces a type fixes its final name.

### 4.1 Record stores (physical layer)

A `RecordStore` provides, for one universe of records (vault notes or one mdbase collection):

- `candidates(source, atoms, scope)`: the keys of authorized records selected by a source
  expression (§4.4) and decided-or-undecided by predicate atoms (§4.3), computed in SQL;
- `prove_fresh(keys)`: a freshness proof for those keys (a stat walk, or a retained proof under
  the watched policy);
- `hydrate(keys, groups)`: field groups (properties, tags, links, inlinks, aliases, list items,
  tasks, inline expressions, body facts) for exactly those keys.

The mdbase record cache already implements this shape internally. Notes gain a narrow,
trigger-maintained query table with stat fingerprints and query-ready values (JSONB
properties, tag and link membership), mirroring `mdbase_record_query`, so the same proof and
candidate machinery applies.

### 4.2 Lazy note lookup

Expression evaluation receives a `NoteLookup` instead of `&HashMap<String, NoteRecord>`. It
resolves notes by path, basename, or alias with the same disambiguation rules as today, and
loads field groups on first access. Every vault path and identity fact needed for resolution is
available from the cache without hydration. The existing map implements the trait, so call sites
migrate one at a time.

This removes the remaining reason to load the whole vault eagerly: link dereferences
(`[[x]].status`, `l.file.tasks`, `asFile()`) touch only their targets.

### 4.3 Shared predicate atoms

One predicate representation replaces both `FilterExpression` and `MdbaseSqlPredicate`:

- **Field:** a property path, a file metadata field (path, name, extension, mtime, ctime,
  size), tag membership, link membership, or mdbase type membership.
- **Comparison:** equality, ordering, prefix, containment, existence, tag/link membership, and
  regular-expression match where SQL can decide it.
- **Literal:** null, boolean, integer, number, string, date, or link.
- **Dialect:** the comparison semantics implemented (Dataview/Bases or CEL), including type
  coercion, null and missing handling, string versus number ordering, date handling, and list
  semantics.
- **Combination:** conjunctions with bounded size, and disjunctions whose decisions combine
  three-valued.

Each atom has one SQL renderer and one in-memory decider. Atoms are decided only for values
whose type makes the dialect's result certain, and are undecided otherwise.

### 4.4 Shared source algebra

`SourceExpr` covers folder (recursive), exact path, tag (with nested tags), mdbase type,
incoming and outgoing link (including `this`), and `and`/`or`/`not`. DQL `FROM`, `QueryAst`
sources, Bases folder and tag filters, Tasks path and tag filters, and mdbase `types` and path
prefixes all compile into it. Candidate selection for a source runs in SQL against the store.

`vulcan-core::source::SourceExpr` implements the note-store part: folder, exact path, tag,
links-to, linked-from, and `and`/`or`/`not`, rendered as one SQL boolean that is never NULL, so
`not` is the complement within the query's readable notes. Folders and tags are byte-exact
(`folder/` ≤ path < `folder0` under BINARY collation; a tag or `tag/…`). Frontends resolve
vault-dependent syntax first: DQL turns a `FROM "x"` path into a folder or a file by what
exists, and link targets (including `this`) into document ids.

### 4.5 Shared logical plan

`StructuredQueryPlan` becomes the plan every frontend compiles into. It holds:

- the source expression;
- lowered predicate atoms, plus the residual as an opaque frontend program;
- projections, group-by, flatten, sort keys, limit, and offset, each marked as evaluable by the
  shared layer or by the frontend;
- the hydration groups the residual and output need, derived from the frontend's expression
  analysis (as `query_reaches_other_file_objects` does today for list items).

Execution is: candidates (§4.1) → freshness proof → hydrate the residual's groups for candidates →
residual evaluation by the frontend → ordering, using SQL-extracted keys where available →
page → hydrate output groups for the page only.

### 4.6 Retained host sessions

The daemon's mdbase query session (per-scope proofs, retained decoded rows bound to row
versions, single-flight proving, lock-free reads with pre-write proofs) generalizes to note
stores, so every frontend served by the daemon gets the same warm path.

### 4.7 Filter language

There are two expression families: the Vulcan expression language (Dataview DQL, Bases, and
Vulcan's own filters) and CEL (mdbase). Each family has exactly one semantics, defined by its
evaluator.

- **The note filter DSL is surface syntax.** `key op value` filters (`ls`/`query --where`,
  `QueryAst` predicates, search filters, saved reports, and simple `.base` comparisons) compile
  to the Vulcan expression AST and mean exactly what that expression means. For example, a
  missing property is `null`, so `status != done` also selects notes without a status; a key
  that both frontmatter and an inline field set is a list; `starts_with` is a byte-exact,
  case-sensitive prefix; `exists` is `!= null`; `contains` is the evaluator's `contains()`;
  `matches` is a regex search over text or list elements. A predicate value is one literal;
  any other `--where` value is itself an expression. `QueryAst` filters are ordered predicates
  or expressions.
- **SQL only narrows.** No consumer runs a filter as exact SQL. Filters lower to shared
  predicate atoms (§4.3), whose SQL removes decided non-matches before loading. The in-memory
  decider then decides each loaded row before anything else is read for it (hydration, raw
  frontmatter, filesystem metadata); only undecided rows are evaluated, and only they load
  the lookup index. The decider resolves keys as the evaluator does (exact, then normalized
  name, else null), so it decides more than SQL, which reads exact keys only.
- **Sources are not filters.** Folder, path, tag, type, and link selection belong to the source
  algebra (§4.4) with their own exact semantics, such as byte-exact folder prefixes and nested
  tags, and may run entirely in SQL. `file.tags has_tag t` (and `file.tags contains t`) is such
  a tag source: notes tagged `t` or a tag nested under it, the same selection as
  `file.hasTag(t)`. `has_tag` on any other field is an error.

## 5. Migration plan

Each step is independently shippable, keeps results identical, and records before/after
measurements. Detailed acceptance criteria live in the roadmap's QRY items.

1. **Shared predicate atoms (QRY.1).** Introduce the atom representation with Dataview and CEL
   dialects. Port `MdbaseSqlPredicate` onto it. Port `FilterExpression` lowering for DQL and
   `QueryAst`, converting DQL's exact SQL `WHERE` path to exact-or-undecided with residual
   evaluation. The existing CEL differential test and a new Dataview differential test gate it.
2. **Shared source algebra (QRY.2).** Compile DQL `FROM`, `QueryAst` sources, and mdbase
   type/path selection into `SourceExpr` with one SQL candidate selector per store.
3. **Lazy note lookup (QRY.3).** Introduce `NoteLookup` with on-demand field groups. Migrate DQL,
   then Bases, Tasks, and DataviewJS. Remove the eager whole-vault load from those paths.
4. **Note store freshness and narrow table (QRY.4).** Add the notes' narrow query table, stat
   fingerprints, and proofs, reusing the mdbase mechanics. Run indexed note execution for plans
   whose residual is empty or bounded by the candidates.
5. **One logical plan (QRY.5).** Compile DQL, Bases, `QueryAst`, and Tasks into
   `StructuredQueryPlan` and execute through one planner. Add an explain report with work
   counters.
6. **Retained sessions for notes (QRY.6).** Generalize the daemon session.
7. **Cross-frontend parity and performance gate (QRY.7).** Equivalent queries in different
   frontends return identical result sets on shared fixtures. DQL and Bases folder, tag, and
   property queries meet the MDB.10 warm-query targets.

## 6. Risks and open questions

- **Dataview semantics.** Implicit coercions (numbers in strings, dates, durations, links
  compared to strings), list membership, and null propagation must be enumerated per atom before
  anything is decided in SQL. Uncertain cases stay undecided.
- **Exactness change for DQL.** Moving DQL `WHERE` from exact SQL to exact-or-undecided may
  surface latent divergences. Any result difference found by the differential test is reviewed
  as a bug fix in its own commit.
- **Identity for link resolution.** Lazy lookup still needs every path, basename, and alias.
  These come from the cache's identity facts, not hydration, and must stay consistent with scan
  link resolution.
- **Permissions.** Tag grants and policy hooks must behave identically with candidate SQL,
  including hidden backlinks and `this`.
- **Scale.** Targets must hold at 100K records. Retained rows and proofs bound memory per scope.
- **Schema churn.** The narrow note table is an additive, rebuildable migration. Breaking changes
  trigger a cache rebuild.
