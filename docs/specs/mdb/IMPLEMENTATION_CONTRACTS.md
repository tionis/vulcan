# MDB implementation contract

Status: normative Vulcan integration decisions for the remaining MDB.6–MDB.10 work, reviewed 2026-09-07. The pinned upstream artifacts remain authoritative for upstream format/profile semantics. This document specifies Vulcan authorization, transactional guarantees, and native feature negotiation; it does not silently amend an upstream conformance claim.

The [performance and focused native integration contract](PERFORMANCE_AND_NATIVE_INTEGRATION.md) adds MDB.10 acceptance gates for indexed reads, App bindings, and standalone scripts. It retains the portable mdbase model and existing wire contracts; documented targets do not establish implemented capability or performance.

## 1. Profile evidence and delivery dependencies

Use the immutable revision and tree digest in `vulcan-core/resources/mdbase/v0.3/PROVENANCE.md`. Evaluate every profile against both its normative requirements and pinned coverage ledger; passing fixtures is necessary, not proof that an empty or draft requirement set is complete. Record upstream profile, exact revision, covered requirements, supplemental tests, unsupported features, and known gaps separately in machine-readable evidence.

The pinned `tests/manifest.yaml` marks `links`, `core_write`, and `lifecycle` draft. In particular `core_write` lists managed type-pack preflight, provenance, concurrency, adoption, diff, idempotency, and atomicity requirements, although type-pack installation is deferred by Vulcan. Do not advertise `core_write` after implementing record CRUD alone. Either explicitly promote and implement every required type-pack behavior or review a new upstream pin before claiming that profile. Do not edit bundled upstream fixtures to make a claim pass.

Bounded record CRUD may ship first as the Vulcan-owned feature `vulcan.record_write.v1`, with `core_read` and `collection_semantics` prerequisites. Its contract is sections 2–5 below and MDB.7's draft/lifecycle pipeline. Declarative lifecycle support is separately advertised as `vulcan.lifecycle.v1` after its CEL dependency and provider tests pass. Named views use `vulcan.saved_views.v1`; `.base` adaptation separately advertises `obsidian_bases_views` only with the required oracle evidence. These names are feature claims, not invented upstream profiles. Apps negotiate profile and feature names explicitly and report which namespace a claim belongs to.

MDB.6 owns link resolution/helpers; MDB.7 owns record writes and declarative lifecycle; MDB.8 owns saved views, `.base` adaptation, and explicit TaskNotes migration. MDB.9 owns watcher integration. Studio's first-party read-only backend requires MDB.4/5 and the MDB.10 read acceptance gates, with no saved-view default until MDB.8; linked pickers require MDB.6, writes require MDB.7 and MDB.10 write acceptance, saved views/import migration require MDB.8, and automatic refresh requires MDB.9. Missing optional features disable their controls, not unrelated reads. No MDB slice becomes a global daemon gate.

## 2. Authorization and constraint scope

Apps, native scripts, canonical mdbase commands, and generic managed edits to collection records use one Vulcan mutation pipeline enforcing the same mdbase semantics. Native frontends do not get a separate validator or a direct authoritative SQLite-write route. The [shared-write and raw-edit boundary](PERFORMANCE_AND_NATIVE_INTEGRATION.md#shared-writes-and-raw-edit-boundary) requires explicit raw repair, no silent fallback after validation failure, and cross-entrypoint parity before unified write support is claimed. This routing remains implementation work; it does not retroactively describe current generic note/property/task commands.

Keep current read filtering before validation: a filtered read describes only readable records and must not disclose hidden uniqueness relationships. Do not reuse that filtered result as write-integrity evidence.

A write planner computes the full validation dependency scope from collection config, matched types/contracts/schemas, uniqueness scopes, link constraints, rename references, and candidate paths. Before evaluating record-dependent constraints it verifies that the caller can read every relevant dependency and can write/delete every affected path. Prove read coverage over the rule's potential path namespace from policy, not only currently discovered records: type-wide uniqueness requires coverage of the collection candidate namespace, even if it is currently empty. Do not inspect hidden type membership or record existence to narrow the required grant. If selector coverage cannot be proved conservatively, deny the write. If complete scope visibility cannot be established, return `permission_denied` without probing whether a hidden record actually conflicts. The same restriction applies even when the candidate value happens to be unique. Do not return hidden paths, counts, values, or conflicting IDs. This conservative rule intentionally makes some narrow app bindings read-only; it does not grant read authority implicitly or add a uniqueness oracle.

An unrestricted local user or separately authorized service may plan the operation under its own explicit grant. A browser invocation never silently borrows that grant. Type/control/schema mutation requires dedicated control-path write authority and preview; ordinary record writes cannot alter validation rules. Apply rechecks grant lineage and both old/resulting record selectors. Link resolution, inferred membership, and CEL helpers use the same authorized scope; unavailable controls cannot be silently treated as absent.

## 3. Plans, revisions, and deterministic values

Plans are immutable, caller/instance/operation-bound, expiring artifacts outside `cache.db`. A plan records exact before/after bytes, changed paths, creation absence preconditions, accepted source revisions, collection root identity, config/type/contract/schema digests, relevant directory membership fingerprints (including uniqueness and inbound-link candidate sets), permission/config revisions, validation policy version, and all generated lifecycle values. Record revisions are opaque content-derived values. Control edits, new relevant records, deletes, renames, changed grants, or collection rebinding invalidate affected plans.

Resolve `now`, `today`, UUID/ULID, and other generated values once while planning. Apply persists those exact reviewed values; it never silently regenerates them. Plan expiry forces a new preview. Validation and membership are computed against the whole proposed final record set, including intra-batch uniqueness and exactly one post-lifecycle membership check. No provider, script, or plugin performs I/O during planning. Pure supported lifecycle providers run in core; reusable persistence orchestration stays in app.

An apply request includes plan ID, accepted revision set, and idempotency key. The durable operation identity binds key to caller, instance, plan digest, and exact input. Repeating the same apply returns its recorded outcome; reusing a key with different input fails. Direct single-record upstream `if_revision` operations can plan/apply internally under the same lock while retaining the upstream `concurrent_modification` envelope. App preview drift maps to `stale_state`. Never replace existing Vulcan/upstream wire envelopes globally.

## 4. Batch and rename transaction boundary

`vulcan.record_write.v1` batches and rename-with-reference-rewrites are all-or-nothing for cooperating Vulcan readers, mutations, scans, and managed sync. Version 1 has no best-effort batch mode. Reject plans over 1,000 changed files or 64 MiB combined before/after bytes with `limit_exceeded`; never split an accepted batch into unreviewed commits.

All cooperating entrypoints acquire the same cross-process vault lock. Before making file changes, persist a bounded journal with transaction ID, exact before/after artifacts, preconditions, and recovery phase outside the cache; fsync required files/directories according to the supported platform's durability contract. Securely stage all replacement files on the destination filesystem, revalidate dependencies, then begin the serialized apply. Missing parent directories and case-only renames are explicit planned operations. Never overwrite a newly appeared file, follow an app-controlled symlink, or rewrite an unplanned reference.

A crash before the durable commit decision rolls back staged/applied paths; after that decision recovery completes the committed state and repairs cache projections. Recovery compares observed bytes to the journal's expected before/after bytes. Unexpected externally edited bytes are preserved with both journal versions and block further affected operations for explicit repair, never overwritten by automatic rollback. Reads/scans/sync must recover or report the blocked transaction before exposing a mixed logical result. Emit one committed logical transaction with per-path events only after reconciliation. Delivery/outbox state and idempotency receipts survive cache rebuild. Post hooks and opt-in Git commit run after canonical commit; their failure reports committed-with-follow-up-failure and cannot turn a successful write into a retryable duplicate.

Ordinary filesystem tools and external editors do not honor Vulcan's lock and can observe intermediate path replacements. Do not claim filesystem-wide atomic rename or isolation against direct filesystem control. Recheck external drift before each replacement, preserve evidence on conflict, and document this boundary in CLI/UI previews. Fault-injection tests cover each journal/replace/commit/cache/outbox boundary, concurrent creators, phantom uniqueness records, case-only rename, denied inbound links, and externally edited files during recovery.

## 5. Schema discovery and editable projections

Expose collection/type/contract/schema/view metadata through shared core/app services, then the App API. Collection discovery returns only bound visible roots; it never scans arbitrary host directories. A schema report returns source revisions, effective matched composition, persisted-required rules, effective defaults, editable/read-only fields, conflicts, and required features. Filter source controls and projected contract fields before returning metadata. If the caller cannot read a necessary control, mark the dependent operation unavailable without exposing its content; do not fabricate a less restrictive schema.

Studio patches persisted frontmatter only. Missing, null, empty values, and effective defaults stay distinct; leaving a default untouched does not persist it. Rename and imports use the same planner. Unsupported schemas stay readable with diagnostics. Tests include control-only changes invalidating an open form, newly introduced matching types, hidden linked targets, scope-wide uniqueness authorization, body/comment/quoting preservation, and cache rebuild parity.

## 6. Handoff evidence

Before marking a slice complete, add unit/fixture/conformance tests, direct-mode and restricted-grant tests, failure recovery tests where it mutates files, generated CLI/API documentation, and the bundled-skill impact review. Do not teach installed skills unimplemented commands. Watchers are repair hints: control changes invalidate all affected projections, and notifications wait until a coherent read state exists. No loading or synchronization of a provider/workflow/type-pack record executes code.

## 7. Derived state and Git ignore convention

`.mdbase/` is reserved for derived implementation state and excluded from mdbase record discovery. When a Vulcan workflow or maintenance script creates this directory, it must also create `.mdbase/.gitignore`, if absent, with exactly these contents followed by a newline:

```gitignore
*
```

Preserve an existing `.mdbase/.gitignore` without overwriting or extending its rules. The default ignores itself as well as the directory's other untracked contents; each local creator supplies it rather than relying on Git to distribute it. Do not modify the vault-root `.gitignore` for this default. Ignore rules do not untrack files already committed to Git.

Keep canonical collection files such as `mdbase.yaml`, `mdbase.lock.yaml`, and configured type/contract folders eligible for version control. Required recovery backups and durable reconciliation state belong outside this disposable directory; a backup copy inside it must not be the only preserved original.

This is a Vulcan creation convention, not an upstream requirement for the directory's internal layout. Vulcan currently does not create `.mdbase/` in its mdbase commands. Do not add implicit writes to collection discovery or read-only commands solely to install the ignore file. When a creation workflow is implemented, test default creation, preservation of existing rules, Git exclusion, and unchanged read-only behavior.
