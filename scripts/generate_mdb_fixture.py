#!/usr/bin/env python3
"""Public, deterministic MDB performance corpus; never consumes a user's vault."""

import argparse
import hashlib
import json
from pathlib import Path

VERSION = 1
KINDS = ("task", "contact", "project")
BODY_BYTES = 4096
LINKS = 10


def encoded(value):
    return (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()


def record_path(index):
    kind = KINDS[index % len(KINDS)]
    visibility = "private" if index % 10 == 0 else "public"
    return f"{visibility}/{kind}/{index % 100:02d}/record-{index:06d}.md"


def schema(kind):
    properties = {
        "type": {"const": kind}, "title": {"type": "string"},
        "id": {"type": "string"}, "status": {"type": "string"},
        "priority": {"type": "integer", "minimum": 0, "maximum": 5},
        "due": {"type": ["string", "null"], "format": "date"},
        "scheduled": {"type": ["string", "null"], "format": "date"},
        "completed": {"type": ["string", "null"]},
        "created": {"type": "string"}, "modified": {"type": "string"},
        "estimate": {"type": "number"}, "archived": {"type": "boolean"},
        "tags": {"type": "array", "items": {"type": "string"}},
        "contexts": {"type": "array", "items": {"type": "string"}},
        "projects": {"type": "array", "items": {"type": "string"}},
        "email": {"type": "string"}, "company": {"type": "string"},
        "description": {"type": ["string", "null"]},
        "effort": {"type": "number"}, "progress": {"type": "number"},
    }
    return {"kind": "mdbase.type", "name": kind, "version": 1,
            "schema": {"dialect": "json-schema-2020-12", "value": {
                "type": "object", "required": ["type", "title", "id"],
                "properties": properties}},
            "collection": {"read_defaults": {"status": "open", "priority": 2}}}


def note(index, count, seed):
    kind = KINDS[index % len(KINDS)]
    fields = {"type": kind, "id": f"fixture-{index:06d}",
              "title": f"{kind.title()} {index:06d}",
              "priority": index % 6, "archived": index % 13 == 0,
              "email": f"person-{index:06d}@example.invalid",
              "company": f"Company {index % 32}", "estimate": index % 120,
              "tags": [kind, f"group-{index % 8}"], "contexts": [],
              "projects": [], "due": "2026-09-30", "scheduled": None,
              "created": "2026-01-01", "modified": "2026-01-01",
              "effort": index % 8, "progress": (index % 101) / 100}
    # Missing, explicit null, empty string, and ordinary values remain distinct.
    variant = (index // 3) % 4
    if variant:
        fields["description"] = (None, "", "Public synthetic record")[variant - 1]
    if index % 5:
        fields["status"] = ("open", "active", "done")[(index // 3) % 3]
    if index % 97 == 0:
        fields["priority"] = "deliberately-invalid"
    targets = [record_path((index + seed + offset + 1) % count)
               for offset in range(LINKS)]
    body = f"# {fields['title']}\n\n" + "\n".join(f"[[{p}]]" for p in targets) + "\n\n"
    padding = "Synthetic public benchmark prose. "
    body += (padding * BODY_BYTES)[:BODY_BYTES - len(body.encode()) - 1] + "\n"
    assert len(body.encode()) == BODY_BYTES
    return b"---\n" + encoded(fields) + b"---\n" + body.encode()


def query(kind, parameter):
    predicate = (f'email == "person-{parameter:06d}@example.invalid"'
                 if kind == "contact" else f'status == "{parameter}"')
    return {"types": [kind], "where": predicate,
            "select": ["file.path", "title", "status", "priority"],
            "order_by": [{"field": "title", "direction": "asc"}],
            "limit": 50, "include_body": False}


def generate(destination, count=10_000, seed=42):
    if count < 12 or count > 100_000:
        raise ValueError("record count must be between 12 and 100000")
    if not 0 <= seed <= 2**32 - 1:
        raise ValueError("seed must be an unsigned 32-bit integer")
    # Exclusive root creation: no overwrite, resume, or traversal into existing vaults.
    destination.mkdir(parents=False, exist_ok=False)
    digest = hashlib.sha256()
    files = 0

    def emit(relative, contents):
        nonlocal files
        path = destination / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("xb") as stream:
            stream.write(contents)
        # Length framing disambiguates paths/content; emission order is versioned.
        name = relative.encode()
        digest.update(len(name).to_bytes(8, "big") + name)
        digest.update(len(contents).to_bytes(8, "big") + contents)
        files += 1

    emit("collection/mdbase.yaml", b'spec_version: "0.3.0"\n')
    emit("collection/.vulcan/config.toml", b'''[permissions.profiles.benchmark_public]
read = { allow = ["mdbase.yaml", "mdbase.lock.yaml", "_types/**", "_contracts/**", "public/**"] }
write = "none"
''')
    for kind in KINDS:
        emit(f"collection/_types/{kind}.md", b"---\n" + encoded(schema(kind)) + b"---\n")
    for index in range(count):
        emit("collection/" + record_path(index), note(index, count, seed))
    for kind in KINDS:
        parameters = (1, 4, 7) if kind == "contact" else ("open", "active", "done")
        for parameter in parameters:
            emit(f"queries/{kind}-{parameter}.json", encoded(query(kind, parameter)))
    manifest = {"generator_version": VERSION, "seed": seed, "records": count,
                "body_bytes_per_record": BODY_BYTES, "links": count * LINKS,
                "private_records": (count + 9) // 10,
                "invalid_priority_records": (count + 96) // 97,
                "missing_status_records": (count + 4) // 5,
                "type_counts": {kind: (count + 2 - i) // 3 for i, kind in enumerate(KINDS)},
                "payload_files": files, "payload_sha256": digest.hexdigest(),
                "digest_format": "generation-order u64be path-length/path/u64be byte-length/bytes; excludes manifest",
                "acceptance_gate_result": "not_evaluated"}
    with (destination / "manifest.json").open("xb") as stream:
        stream.write(encoded(manifest))
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path, help="new directory beneath an existing parent")
    parser.add_argument("--records", type=int, choices=(10_000, 100_000), default=10_000)
    parser.add_argument("--seed", type=int, default=42)
    args = parser.parse_args()
    print(json.dumps(generate(args.destination, args.records, args.seed), sort_keys=True))


if __name__ == "__main__":
    main()
