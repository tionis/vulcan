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

This artifact establishes reproducible workload generation only. It contains no latency samples, memory measurements, work counters, reference-machine designation, or performance pass. The full baseline and acceptance procedure in the parent performance contract remains outstanding. Assistant-skill review: developer-only fixture tooling changes no installed command or authorization workflow; existing query and permission skills need no change.
