# Public MDB performance fixtures

Generate each corpus into a **new** directory below an existing parent:

```sh
python3 scripts/generate_mdb_fixture.py /tmp/mdb-10k --records 10000
python3 scripts/generate_mdb_fixture.py /tmp/mdb-100k --records 100000
```

The generator refuses an existing destination, never reads a user's vault, and uses only Python's standard library. It leaves partial output for inspection on an I/O failure; choose another destination rather than overwriting it. Allow approximately 50 MiB / 500 MiB plus filesystem overhead. Only generated public data belongs in these corpora.

Each output contains `collection/` (the vault), nine canonical query files in `queries/`, and `manifest.json`. Queries and the manifest are outside the collection so they cannot accidentally become records. The manifest records parameters, expected population counts, and a length-framed SHA-256 of every payload file in versioned generation order, excluding the manifest itself. Bytes do not depend on the output path, wall clock, Python hash seed, or random-number implementation. File creation/modification times are not fixed; do not use this corpus's filesystem timestamps as a cross-run oracle.

Version 1, seed 42, verified generated payload digests:

| Records | Links | SHA-256 |
| --- | --- | --- |
| 10,000 | 100,000 | `ba3b33b62cbbcea1e68628920268cd143416d1216345c5aeb029220c28e9a348` |
| 100,000 | 1,000,000 | `b934b38bb58223d523aba60a8b4e346c553deee93828d3d52c0570dca9166d5f` |

Records cycle through task/contact/project types with a 20-property representative schema, exactly 4,096 UTF-8 body bytes and ten distinct resolvable collection-relative wikilinks. Every tenth record is private. Every fifth record omits status (effective default `open`); every 97th deliberately violates the integer priority schema. Description cycles through missing, null, empty, and nonempty. These are synthetic schemas, not a claim of TaskNotes plugin-schema equivalence or migration compatibility.

The `benchmark_public` permission profile includes complete control namespaces and public records, excluding private records and all writes. Run queries both unrestricted and with this profile; do not compare a restricted total to an unrestricted total. The nine queries vary task/project status and exact contact email, request four fields, sort by title, and limit to 50. Their canonical envelopes retain exact counts and diagnostics. Additional path-prefix, update, concurrent-write, native/DQL parity, clock, and stress workloads remain required by the performance contract.

```sh
target/debug/vulcan --vault /tmp/mdb-10k/collection mdbase status --output json
target/debug/vulcan --vault /tmp/mdb-10k/collection mdbase query \
  --file /tmp/mdb-10k/queries/task-open.json --permissions benchmark_public --output json
python3 -m unittest discover -s scripts/tests -v
VULCAN_TEST_BINARY="$PWD/target/debug/vulcan" python3 -m unittest discover -s scripts/tests -v
```

The optional CLI test uses 120 records to check actual registry loading, membership counts, all query files, restricted visibility, invalid-record validation, and effective-default versus persisted-value behavior. Unit tests independently check deterministic bytes, digest framing, semantic-case coverage, body/link counts, and overwrite refusal.

## Repeatable CLI diagnostic measurements

After building the desired release commit, run the diagnostic driver against only a generated public corpus:

```sh
python3 scripts/measure_mdb_cli.py /tmp/mdb-10k --binary target/release/vulcan --samples 9
python3 scripts/measure_mdb_cli.py /tmp/mdb-10k --binary target/release/vulcan --samples 9 --refresh off
python3 scripts/measure_mdb_cli.py /tmp/mdb-10k --binary target/release/vulcan --samples 9 --restricted
```

The driver emits JSON on stdout. It checks the versioned payload digest and source-file membership before and after requests, rejects symlinks, and records binary/runner SHA-256 values. A first request is separate from the requested repeated samples; the latter cycle through all nine parameter-varied queries. Exact totals, ordered paths, response size, omitted body, query diagnostics, exit status, and stderr are checked. A failure stops the run and makes the process exit nonzero; partial samples never establish a pass. Record the binary's commit, build command/features, and reference-machine details alongside the report.

Fixture verification reads all payload bytes outside the timed region and therefore warms OS file caches. The first request may populate the MDB cache if it does not exist; neither an existing cache file nor repetition proves a warm indexed generation. Process wall times include startup, stdout/stderr capture, and exit, but exclude subsequent JSON checking. Reported percentiles use nearest rank over repeated successful requests only. Linux child peak RSS is the aggregate `RUSAGE_CHILDREN` maximum in KiB, not retained-memory growth. Run without concurrent Cargo jobs or other deliberate benchmark load.

`--samples 1000` supports larger diagnostic runs, but the driver deliberately reports `acceptance_gate_result: not_evaluated`: reference-host designation, stage/source-read work counters, cold I/O, retained-memory behavior, concurrent writers, additional query frontends, and the full acceptance protocol remain separate work. The optional `VULCAN_TEST_BINARY` test exercises the driver itself against all nine queries on 120 records, both unrestricted and permission-filtered. Assistant-skill review: this developer measurement tool does not change installed query workflows.

## Shared-service stage diagnostics

The developer-only ignored test runs the same shared query service with explicit diagnostic metrics, not a new CLI request or response format:

```sh
VULCAN_MDB_PROFILE_FIXTURE=/tmp/mdb-10k cargo +1.88.0 test --release -p vulcan-app shared_query_stage_benchmark -- --ignored --nocapture --test-threads=1
VULCAN_MDB_PROFILE_FIXTURE=/tmp/mdb-10k VULCAN_MDB_PROFILE_SCOPE=benchmark_public cargo +1.88.0 test --release -p vulcan-app shared_query_stage_benchmark -- --ignored --nocapture --test-threads=1
```

Use only the generated public 10K/100K fixture. Verify its payload with `verify_fixture` from `scripts/measure_mdb_cli.py` before and after the run; this validation is outside timing and warms OS file caches. The test checks the version/count/seed, executes one first request plus nine varied repeated requests, and independently checks totals, ordered paths, body omission, diagnostics, and response bounds. It emits one JSON diagnostic per request. Pin the source revision or source-file hashes when measuring an uncommitted implementation, and record the release test executable hash/features and host conditions with captured results. Do not overlap other builds or tests with the measured test body.

Request timing starts after reading/parsing the canonical query and includes per-request permission-profile resolution, the synchronous service call, and compact canonical response serialization. It excludes Cargo/build/test startup, CLI formatting, output of diagnostic lines, and subsequent oracle assertions. Service metrics separate control loading/read-guard acquisition, canonical preparation, record preparation, and residual execution. Nested record metrics separate the two manifest captures, cache opening, cache SQL/payload decoding, collection overlays, refresh/rebuild attempts, and source fallback. Do not add nested times to their parents.

Counters describe completed visible manifests/bytes, decoded authorized cached records, overlay passes/records, cache attempts/hits, refresh/rebuild attempts, and source loads. Metrics reset before each operation, contain no record paths/values, and survive errors with partial work counts. They are not a complete I/O audit: partial failed manifest reads and source-fallback/refresh internals are not counted as completed manifests; schema/CEL compilation, query-candidate work, watcher freshness, and syscall counts remain separate instrumentation work. Neither these ten diagnostic requests nor the presence of a cache proves a warm indexed generation or an acceptance pass. Assistant-skill review: developer instrumentation only; installed query workflows and canonical envelopes are unchanged.

This document supplies reproducible tooling, not a performance pass. The full baseline and acceptance procedure in the parent performance contract remains outstanding.

## Note-query service diagnostics

The developer-only `note_session_benchmark` test runs the vault HTTP note routes (`/dataview/query`,
`/query`, `/bases/eval`, `/notes`, `/mdbase/query`) in process, with or without the retained
note-store session, under closed-loop readers and an optional paced writer:

```sh
VULCAN_NOTE_BENCH_FIXTURE=/tmp/mdb-10k/collection VULCAN_NOTE_BENCH_SAMPLES=5000 \
  VULCAN_NOTE_BENCH_READERS=1 \
  cargo +1.88.0 test --release -p vulcan-app --test note_session_benchmark -- --ignored --nocapture --test-threads=1
```

`VULCAN_NOTE_BENCH_READERS`, `VULCAN_NOTE_BENCH_WRITES_PER_SECOND`, `VULCAN_NOTE_BENCH_SESSION=0`
(direct path), `VULCAN_NOTE_BENCH_SCOPE=benchmark_public`, and `VULCAN_NOTE_BENCH_FRONTENDS`
(comma-separated `dql,dql-tag,query,bases,bases-tag,notes,mdbase`) vary the run;
`VULCAN_NOTE_BENCH_EXPLAIN=1` first prints each frontend's warm note plan with stage timings. The first scan indexes an
unindexed fixture; the test adds `public/_bench/*.base` files for its Bases views and removes them
afterwards. It checks every response against the ordered paths the direct path returned before the
run and prints a JSON report labeled `not_evaluated`. Results:
[note-query-frontends.json](note-query-frontends.json) and
[note-query-session-mixed-10k.json](note-query-session-mixed-10k.json).

### Designated evaluation host

Reference measurements of note queries run on a project-designated evaluation host (designated by
the project owner on 2026-10-07): Intel Core i7-10700 (8 cores, 16 threads), 32 GiB RAM, Debian 13
with kernel 6.12, otherwise idle. The benchmark builds there from a git bundle of the measured
commit, regenerates the public fixtures (checking their published payload digests), and runs each
configuration sequentially. Results: [note-query-frontends-reference.json](note-query-frontends-reference.json)
and [note-query-session-mixed-reference.json](note-query-session-mixed-reference.json);
[note-query-ordered-top-k.json](note-query-ordered-top-k.json) compares the ordered top-k walk with
its parent there (fixtures not re-verified for that run). The earlier
`note-query-frontends.json` and `note-query-session-mixed-10k.json` are development-host
diagnostics.
