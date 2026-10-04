#!/usr/bin/env python3
"""Diagnostic public-fixture MDB CLI measurements, not an acceptance verdict."""

import argparse
import hashlib
import json
import math
from pathlib import Path
import platform
import subprocess
from subprocess import run as run_cli
import time

import generate_mdb_fixture as fixture


def query_cases():
    return [(kind, parameter) for kind in fixture.KINDS
            for parameter in ((1, 4, 7) if kind == "contact" else ("open", "active", "done"))]


def verify_fixture(root):
    """Check the versioned generation-order digest before executing any query."""
    manifest = json.loads((root / "manifest.json").read_bytes())
    if manifest["generator_version"] != fixture.VERSION:
        raise ValueError("unsupported fixture generator version")
    count = manifest["records"]
    if not isinstance(count, int) or not 12 <= count <= 100_000:
        raise ValueError("invalid public fixture record count")
    paths = ["collection/mdbase.yaml", "collection/.vulcan/config.toml"]
    paths += [f"collection/_types/{kind}.md" for kind in fixture.KINDS]
    paths += ["collection/" + fixture.record_path(i) for i in range(count)]
    paths += [f"queries/{kind}-{parameter}.json" for kind, parameter in query_cases()]
    expected_sources = {path for path in paths if path.startswith("collection/")
                        and not path.startswith("collection/.vulcan/")}
    actual_sources = set()
    for path in (root / "collection").rglob("*"):
        if path.is_symlink():
            raise ValueError("fixture collection must not contain symlinks")
        relative = path.relative_to(root / "collection")
        if path.is_file() and relative.parts[0] not in (".vulcan", ".mdbase", ".git"):
            actual_sources.add("collection/" + relative.as_posix())
    if actual_sources != expected_sources:
        raise ValueError("fixture source membership differs from generated payload")
    digest = hashlib.sha256()
    for relative in paths:
        path = root / relative
        if any(part.is_symlink() for part in (path, *path.parents)):
            raise ValueError("fixture payload must not contain symlinks")
        name, data = relative.encode(), path.read_bytes()
        digest.update(len(name).to_bytes(8, "big") + name)
        digest.update(len(data).to_bytes(8, "big") + data)
    if manifest["payload_files"] != len(paths) or manifest["payload_sha256"] != digest.hexdigest():
        raise ValueError("fixture payload does not match its recorded digest")
    return manifest


def expected_paths(count, kind, parameter, restricted):
    matches = []
    for index in range(count):
        if fixture.KINDS[index % 3] != kind or (restricted and index % 10 == 0):
            continue
        status = "open" if index % 5 == 0 else ("open", "active", "done")[(index // 3) % 3]
        if (index == parameter if kind == "contact" else status == parameter):
            matches.append(fixture.record_path(index))
    return matches


def check_response(stdout, count, kind, parameter, restricted):
    report = json.loads(stdout)
    expected = expected_paths(count, kind, parameter, restricted)
    rows = report["results"]
    if report["meta"]["total_count"] != len(expected):
        raise ValueError("incorrect exact total")
    if [row["file"]["path"] for row in rows] != expected[:50]:
        raise ValueError("incorrect ordered result paths")
    if len(stdout) > 256 * 1024 or any("body" in row for row in rows):
        raise ValueError("response exceeds metadata-query payload contract")
    if report.get("diagnostics"):
        raise ValueError("query returned diagnostics")
    return {"rows": len(rows), "exact_total": len(expected), "stdout_bytes": len(stdout)}


def percentiles(samples):
    if not samples:
        raise ValueError("cannot summarize empty samples")
    ordered = sorted(samples)
    return {f"p{p}": ordered[max(0, math.ceil(len(ordered) * p / 100) - 1)]
            for p in (50, 95, 99)}


def measure(binary, root, samples=9, refresh="blocking", restricted=False, timeout=180):
    if samples < 1 or refresh not in ("blocking", "off") or timeout <= 0:
        raise ValueError("invalid measurement parameters")
    root, binary = root.resolve(), binary.resolve(strict=True)
    manifest = verify_fixture(root)
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    cache_existed = (root / "collection/.vulcan/cache.db").exists()
    observations = []
    # The first request is reported separately; no warm-cache claim is inferred
    # merely because a cache file exists or requests have already run.
    for iteration in range(samples + 1):
        kind, parameter = query_cases()[iteration % len(query_cases())]
        query = root / f"queries/{kind}-{parameter}.json"
        command = [str(binary), "--vault", str(root / "collection"), "--output", "json",
                   "--refresh", refresh, "mdbase", "query", "--file", str(query)]
        if restricted:
            command += ["--permissions", "benchmark_public"]
        start = time.perf_counter()
        try:
            result = run_cli(command, capture_output=True, timeout=timeout, check=False)
            elapsed = time.perf_counter() - start
            if result.returncode:
                raise ValueError(f"CLI exited {result.returncode}: {result.stderr[:2048]!r}")
            checked = check_response(result.stdout, manifest["records"], kind, parameter, restricted)
            if result.stderr:
                raise ValueError(f"unexpected stderr: {result.stderr[:2048]!r}")
            observations.append({"iteration": iteration, "query": query.name,
                                 "wall_seconds": elapsed, "ok": True, **checked})
        except (subprocess.TimeoutExpired, ValueError, KeyError, TypeError) as error:
            observations.append({"iteration": iteration, "query": query.name, "ok": False,
                                 "wall_seconds": time.perf_counter() - start, "error": str(error)})
            break
    unchanged = verify_fixture(root) == manifest
    same_binary = hashlib.sha256(binary.read_bytes()).hexdigest() == binary_hash
    complete = len(observations) == samples + 1 and all(row["ok"] for row in observations)
    repeated = [row["wall_seconds"] for row in observations[1:] if row["ok"]]
    peak_rss_kib = None
    if platform.system() == "Linux":
        import resource
        peak_rss_kib = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    return {
        "measurement": "public_mdb_cli_diagnostic", "acceptance_gate_result": "not_evaluated",
        "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "binary": str(binary), "binary_sha256": binary_hash, "binary_unchanged": same_binary,
        "fixture": manifest, "fixture_unchanged": unchanged,
        "machine": {"platform": platform.platform(), "architecture": platform.machine(),
                    "acceptance_reference_machine": False},
        "method": {"refresh": refresh, "permission_profile": "benchmark_public" if restricted else None,
                   "cache_existed_before": cache_existed, "requested_repeated_samples": samples,
                   "timeout_seconds": timeout, "concurrency": 1, "timing": "process start through exit",
                   "fixture_verification": "pre/post digest reads outside timing; preflight warms OS file caches",
                   "memory": "Linux aggregate child ru_maxrss in KiB; not retained memory growth",
                   "percentiles": "nearest rank; repeated successful requests only"},
        "child_peak_rss_kib": peak_rss_kib,
        "complete_and_valid": complete and unchanged and same_binary,
        "first_request": observations[0], "repeated_requests": observations[1:],
        "repeated_wall_seconds": percentiles(repeated) if repeated else None,
        "limitations": ["No acceptance verdict or warm indexed-generation proof",
                        "No stage/source-read counters, memory-growth measurement, or concurrent writers",
                        "Host power/frequency/background load and OS page cache are uncontrolled"],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixture", type=Path)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=9, help="repeated requests after one separately reported request")
    parser.add_argument("--refresh", choices=("blocking", "off"), default="blocking")
    parser.add_argument("--restricted", action="store_true")
    parser.add_argument("--timeout", type=float, default=180)
    args = parser.parse_args()
    report = measure(args.binary, args.fixture, args.samples, args.refresh, args.restricted, args.timeout)
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["complete_and_valid"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
