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

This artifact establishes reproducible workload generation only. It contains no latency samples, memory measurements, work counters, reference-machine designation, or performance pass. The full baseline and acceptance procedure in the parent performance contract remains outstanding. Assistant-skill review: developer-only fixture tooling changes no installed command or authorization workflow; existing query and permission skills need no change.
