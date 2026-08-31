#!/usr/bin/env python3
"""Aggregate verified per-target artifact records into one release manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib

EXPECTED_TARGETS = {
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
}


def sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def aggregate(
    directory: pathlib.Path,
    version: str,
    expected_targets: set[str] | None = None,
) -> tuple[pathlib.Path, pathlib.Path]:
    artifacts = []
    targets = set()
    for record_path in sorted(directory.glob("*.artifact.json")):
        record = json.loads(record_path.read_text(encoding="utf-8"))
        archive = directory / record["name"]
        if record["version"] != version:
            raise ValueError(f"version mismatch in {record_path.name}")
        if record["target"] in targets:
            raise ValueError(f"duplicate target {record['target']}")
        if not archive.is_file() or sha256(archive) != record["sha256"]:
            raise ValueError(f"checksum mismatch for {record['name']}")
        targets.add(record["target"])
        artifacts.append(record)
    if not artifacts:
        raise ValueError("no artifact records found")
    if expected_targets is not None and targets != expected_targets:
        missing = sorted(expected_targets - targets)
        unexpected = sorted(targets - expected_targets)
        raise ValueError(
            f"release target set mismatch: missing={missing}, unexpected={unexpected}"
        )
    manifest_path = directory / f"vulcan-{version}-manifest.json"
    manifest_path.write_text(
        json.dumps(
            {"schema_version": 1, "product": "vulcan", "version": version, "artifacts": artifacts},
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
        newline="\n",
    )
    checksums_path = directory / "SHA256SUMS"
    checksums_path.write_text(
        "".join(f"{artifact['sha256']}  {artifact['name']}\n" for artifact in artifacts),
        encoding="ascii",
        newline="\n",
    )
    return manifest_path, checksums_path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--directory", required=True, type=pathlib.Path)
    parser.add_argument("--version", required=True)
    arguments = parser.parse_args()
    manifest, checksums = aggregate(
        arguments.directory.resolve(), arguments.version, EXPECTED_TARGETS
    )
    print(manifest)
    print(checksums)


if __name__ == "__main__":
    main()
