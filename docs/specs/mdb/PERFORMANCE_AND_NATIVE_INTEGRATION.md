# MDB performance and native integration

Status: accepted implementation direction and acceptance criteria, 2026-09-07. This document defines future work; it does not claim that the current CLI or App host meets these targets. The [MDB implementation contract](IMPLEMENTATION_CONTRACTS.md) controls authorization, revisions, recovery, and feature negotiation. The [Vulcan App v1 contract](../vulcan-app/v1/IMPLEMENTATION_CONTRACTS.md) controls App bindings and transport. These criteria add implementation gates without changing upstream profiles or frozen wire formats.

## 1. Decision and boundaries

Keep mdbase a first-class portable collection model. Its future adoption is uncertain; TaskNotes is one concrete consumer, not a limit on supported uses. Build efficient native Vulcan services beneath that model for Vulcan Apps and standalone CLI/Rust/script integrations. Reuse the existing query representation, SQLite cache, permission machinery, and core/app boundary.

The first integration does not introduce another collection marker, schema language, type/contract format, query dialect, generic storage-provider framework, or independently maintained database engine. Native here describes execution and integration, not a competing data standard. Ordinary native note queries remain available with their existing semantics; they must not silently stand in for an mdbase operation that promises defaults, validation, or contract views. Consider an additional native semantic feature only after a consumer demonstrates a gap, with an explicit capability and compatibility review.

Markdown and collection controls remain canonical. Parsed records, membership, effective properties, validation evidence, contract projections, and link indexes belong in Vulcan's rebuildable cache. Durable plans, recovery journals, and reconciliation state retain the separate storage boundary in the MDB contract. A fast cache hit is never authority to omit permission checks or weaken write validation.

## 2. Focused native integration

Use one reusable synchronous collection service. Pure parsing, validation, query planning/evaluation, and cache representations belong in `vulcan-core`; filesystem workflows, refresh, mutation planning/apply, and cross-operation orchestration belong in `vulcan-app`. CLI and App transports adapt these services. Async scheduling and long-lived host sessions stay outside core.

| Operation | Initial integration contract |
| --- | --- |
| Bind and inspect | Resolve an explicitly selected collection within the caller's vault/store scope; expose readable type/contract definitions, effective schema, revisions, diagnostics, and supported capabilities. No ambient filesystem search or schema ownership inferred from a type name. |
| Read | Preserve canonical complete-record semantics, exact-source opt-in, revisions, and permission-filtered diagnostics. Hydrate body/source only as needed and verify consistency with the selected snapshot. |
| Query | Accept the existing canonical mdbase JSON/YAML request and preserve its result envelope. Use the shared native planning machinery internally, with mdbase semantics and CEL fallback. |
| Plan and apply | First write pilot: a revision-checked patch to persisted frontmatter. Use the existing MDB plan/dependency/authorization/recovery contract and standard mutation apply/status flow. Never treat effective defaults or computed projections as already persisted values. |

The App adapter uses the already specified `stores.mdbase.*` and `mutations.*` operations. Standalone scripts invoke structured CLI commands or shared Rust services without an App package, App instance, or daemon. A direct caller carries its own vault scope and permission grant; it never receives a fabricated unrestricted App context. Shell and Python pilots use supported JSON/NDJSON envelopes, structured arguments/files/stdin where implemented, and explicit exit/error handling. Do not shell out from the App host to implement shared operations.

Retain existing CLI output contracts, including the distinction between successful execution and a validation envelope with `valid: false`. Generate public request/report/error schemas from the operation registry as the corresponding bindings ship. An unimplemented CLI route or App operation must remain explicitly unavailable, not documented as callable. No new public query-mode discriminator or generic `stores.collections` API is introduced by this document; any such extension requires a separate versioned design review.

Deliver read/query/schema first. The patch pilot follows MDB.7; create/delete/rename/batch and declarative lifecycle remain in that track rather than a parallel script-specific write implementation. A working patch pilot does not establish the complete `vulcan.record_write.v1` feature or upstream `core_write` profile. Saved views, automatic subscriptions, runtime activation, SDK generators, and additional collection backends are not prerequisites for this first integration.

## 3. Performance acceptance contract

These are acceptance targets on a documented reference machine, using a release build with pinned build settings. The benchmark artifact must record CPU, memory, storage, OS/filesystem, binary commit/features, fixture seed/digest, cache state, concurrency, request mix, and timings. Select and record the reference machine before claiming a pass; do not change it to conceal a regression.

The primary reference fixture has 10,000 Markdown records, about 4 KiB mean body size, multiple types, a representative TaskNotes-sized schema, and about 100,000 links. Include missing/null/empty properties, defaults, invalid records, and restricted grants. A 100,000-record fixture tests scaling; larger bodies, high link fan-out, expensive expressions, and schema changes are separate stress cases. Mimir remains an optional local validation workload; do not commit private vault contents or paths into benchmark fixtures.

The fast list workload returns at most 50 records and at most 256 KiB of serialized response, with a small selected field set, an indexed type/path/property selection and ordering, and the canonical exact total count. Include task status lists, contact lookups, and project lists. Warm means initialization, schema compilation, and initial indexing have completed, with one coherent current indexed generation and no pending relevant source changes. The compiled plan may be reused; full-result memoization must be disabled for the baseline. Vary predicate parameters and execute queries after updates as well as repeating identical requests.

| Boundary and workload | Acceptance target |
| --- | --- |
| Local App/shared-service metadata query: complete request available at host ingress through authorization, execution, and fully serialized response; excludes browser rendering and remote network transit | p95 < 50 ms; p99 < 100 ms |
| Internal execution component of that query | p95 < 25 ms; explanatory budget, not a replacement for the end-to-end gate |
| Warm single-record metadata read, without body/source hydration | p95 < 10 ms |
| Direct CLI metadata query with existing index and unchanged vault, normal blocking incremental freshness check; process start through final stdout and exit | p95 < 100 ms |

Also report direct CLI `--refresh off` separately; it is not a substitute for the normal-refresh gate. Cold process/host startup, initial indexing, cache rebuild, external-edit reconciliation, source/body reads, and broad control/schema invalidation have separate latency/throughput results. Do not hide them by labeling a stale response warm. No sub-100 ms promise applies to an arbitrary CEL scan, unbounded result, global grouping, or high-fan-out traversal.

Before enabling the writable App/script pilot, benchmark planning, durable apply (including required journal persistence/fsync), derived-state publication, and the following read separately and end-to-end. Record an explicit write acceptance budget based on that evidence. The read gate must not be presented as a write guarantee or met by acknowledging an undurable write. Large batches and cross-vault synchronization are separate workloads.

Run at least 1,000 measured requests per warm workload/mode after a documented warmup. Report p50/p95/p99, sample count, errors, throughput, and memory growth. Test single-client execution and eight concurrent readers with two independent single-record writes per second; concurrent acceptance uses the same read thresholds once that write path exists. Report queueing as part of latency and failures/timeouts as failures, not discarded samples. Serialize benchmark runs on the reference machine. Ordinary CI uses deterministic work assertions and semantic tests; controlled performance runs enforce timing gates and retain baseline artifacts.

## 4. Execution and freshness design

Prepare mdbase state when source dependencies change, then query a consistent indexed snapshot. Cache refresh must discover and rederive changed records and affected dependents, rather than rederive the whole collection and merely avoid unchanged SQLite writes. The existing record cache is a starting point, not evidence of this gate: its current refresh calls the whole-collection loader before comparing rows.

Index collection/type membership, path selection, and measured high-value property filters/sort keys, retaining enough persisted/effective/presence information to implement mdbase semantics. Preserve source revisions, dependency versions, and validation evidence. Do not eagerly index every arbitrary frontmatter combination. Extend the existing shared query representation with a physical execution plan rather than a second public AST.

Compile each distinct CEL expression once per prepared plan and each schema once per relevant control revision. Share immutable link/schema data; never deep-copy the collection link index per candidate. Classify query dependencies and avoid body/link/contract hydration for queries that do not use them. Keep query-level clocks fixed, and invalidate or reevaluate time-dependent projections correctly; plan reuse does not freeze `now` or `today` across operations.

Initially evaluate CEL on indexed candidates. Add predicate/order lowering into SQLite only where equivalence is proven, retaining residual CEL elsewhere. Preserve missing/null distinctions, effective defaults, numeric and date behavior, explicit/inferred membership, evaluation errors, diagnostics, sorting, and pagination. Index selection must not silently remove records whose evaluation has observable required diagnostics. Exact `meta.total_count`, residual filters, and grouping may require evaluating all candidates: do not push `limit` before those semantics or return approximate totals under the existing envelope.

For the steady-state metadata-list baseline require zero Markdown body/source reads, zero schema compilations, zero full collection rederivations, and zero per-candidate whole-index clones. CEL compilation counts must be bounded by distinct expressions for a new plan and zero for a reused plan. Instrument candidate counts, rows visited/hydrated, and dependency work; the indexed selection must avoid visiting unrelated record bodies as fixture size increases.

A host retains collection sessions, compiled plans, registries, and prepared statements with bounded memory/eviction. Direct CLI uses the same persistent cache and synchronous services without requiring that host. Cache generations and plan identities bind applicable record-model, schema/control, engine, query, and permission dependencies. Recheck effective authorization on every operation, including cached plans; apply visibility before evaluation, totals, grouping, pagination, link resolution, and diagnostics. Shared unrestricted indexes must not become permission-filtered answer caches.

Read one coherent generation. Cooperating Vulcan mutations publish derived state or make dependent reads wait/reconcile before successful following reads; errors cannot expose a partly applied batch. External filesystem edits are discovered through watching and reconciliation, with explicit freshness policy. A strict disk-reconciled read may wait for a scan and is timed accordingly. Missed watcher events must remain repairable. Do not silently replace strict freshness with stale reads to achieve the target. Keep internal generation bookkeeping out of canonical envelopes unless an explicit versioned API extension is reviewed.

## 5. Delivery and evidence

Deliver these slices as separate working changes:

1. **Baseline and instrumentation:** deterministic public fixtures, native/DQL/mdbase comparisons, stage timing/work counters, explicit current limitations, and the reference-machine benchmark artifact.
2. **Repeated-work removal:** CEL/schema compilation reuse, shared link state, and dependency-driven hydration. Remeasure each change; do not presume this removes whole-collection preparation.
3. **Incremental indexed reads:** real changed-record/dependency updates, permission-safe snapshots, query/read cache integration, invalidation, and indexed candidate selection. Establish the warm read gate in direct mode before waiting for the full App platform.
4. **Focused bindings:** a shell task-list script, a Python read/query/validation integration, and Collection Studio read/schema/query views use the same semantic services. App acceptance follows Phase 19 host availability; standalone scripts do not depend on it.
5. **Write and live-state gates:** use MDB.7 for the revision-checked status-patch pilot and its write budget/recovery evidence; use MDB.9 for subscriptions and sustained read latency under edits. Add further SQL lowering only when the benchmark identifies it as necessary and semantic parity passes.

Differential tests compare the optimized path to the source-derived evaluator using identical source snapshots, operation clocks, permissions, and query parameters. Compare record sets, persisted/effective values, contract views, exact-source opt-in, diagnostics, ordering, totals, groups, and errors. Include external edits, same-size changes with misleading mtimes, renamed/deleted records, new matching types, local schema-reference changes, permission revocation, clock/timezone changes, phantom uniqueness conflicts, crash recovery, and full cache rebuild. Performance cannot justify dropping unsupported-syntax diagnostics, validation, or authorization.

First-party App-backend readiness requires the read performance and parity gates; writable readiness additionally requires the existing MDB mutation contract and explicit write budget. Read-only exploratory CLI use may continue while the gates remain unmet. No gate claims a new upstream profile, removes optional profile requirements, or blocks unrelated daemon work. Review installed assistant guidance when callable behavior actually changes; planning documents must not teach unimplemented commands.
