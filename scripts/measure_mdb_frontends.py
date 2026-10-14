#!/usr/bin/env python3
"""Diagnostic cross-frontend mdbase query latency and parity, not an acceptance verdict.

Runs the public fixture's canonical query cases through each standalone
frontend of the shared query service and records per-frontend latency:

- cli: one `vulcan mdbase query` process per request (normal blocking refresh)
- python: `docs/examples/mdbase/collection.py`'s `Collection.query` (a CLI
  process plus the wrapper's JSON handling)
- http_oneshot: `vulcan serve --no-watch` over loopback HTTP (the ordinary service)
- http_watched: `vulcan serve` with watching (the retained query session)

Every response is checked against the generator's known answer, and the
envelopes of the different frontends must be identical for each query case.
"""

import argparse
import hashlib
import json
from pathlib import Path
import platform
import subprocess
import sys
import time
import urllib.parse
import urllib.request

import generate_mdb_fixture as fixture
import measure_mdb_cli as cli_measure

ROOT = Path(__file__).resolve().parent.parent
FRONTENDS = ("cli", "python", "http_oneshot", "http_watched")


class Server:
    """A `vulcan serve` process on a loopback port it picked itself."""

    def __init__(self, binary, collection, watch, restricted):
        command = [str(binary), "--vault", str(collection)]
        if restricted:
            command += ["--permissions", "benchmark_public"]
        command += ["serve", "--bind", "127.0.0.1:0"]
        if not watch:
            command.append("--no-watch")
        self.process = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                                        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        self.address = self.token = None
        while self.address is None or self.token is None:
            line = self.process.stderr.readline()
            if not line:
                raise ValueError(f"serve exited {self.process.wait()}")
            if line.startswith("Vulcan serve token: "):
                self.token = line.split(": ", 1)[1].strip()
            elif line.startswith("Vulcan serve listening on http://"):
                self.address = line.rsplit("http://", 1)[1].strip()

    def query(self, query_text, timeout):
        url = f"http://{self.address}/mdbase/query?" + urllib.parse.urlencode({"query": query_text})
        request = urllib.request.Request(url, headers={"x-vulcan-token": self.token})
        with urllib.request.urlopen(request, timeout=timeout) as response:
            body = json.loads(response.read())
        if body.get("ok") is not True:
            raise ValueError(f"HTTP error: {body.get('error')!r}")
        return json.dumps(body["result"])

    def close(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()


def frontend_runner(name, binary, collection, restricted, timeout):
    """A function running one query file through `name`, returning the
    envelope as JSON text, and a cleanup function."""
    if name == "cli":
        def run(query_path):
            command = [str(binary), "--vault", str(collection), "--output", "json",
                       "mdbase", "query", "--file", str(query_path)]
            if restricted:
                command += ["--permissions", "benchmark_public"]
            result = subprocess.run(command, capture_output=True, text=True, timeout=timeout, check=False)
            if result.returncode:
                raise ValueError(f"CLI exited {result.returncode}: {result.stderr[:2048]!r}")
            return result.stdout
        return run, lambda: None
    if name == "python":
        sys.path.insert(0, str(ROOT / "docs/examples/mdbase"))
        from collection import Collection  # pylint: disable=import-outside-toplevel
        collection_client = Collection(str(collection), binary=str(binary),
                                       permissions="benchmark_public" if restricted else None)

        def run(query_path):
            return json.dumps(collection_client.query(json.loads(query_path.read_text())))
        return run, lambda: None
    server = Server(binary, collection, watch=name == "http_watched", restricted=restricted)

    def run(query_path):
        return server.query(query_path.read_text(), timeout)
    return run, server.close


def canonical(envelope_text):
    return json.dumps(json.loads(envelope_text), sort_keys=True)


def measure(binary, root, samples, restricted, timeout, frontends):
    root, binary = root.resolve(), binary.resolve(strict=True)
    manifest = cli_measure.verify_fixture(root)
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    collection = root / "collection"
    cases = cli_measure.query_cases()
    reports, envelopes = {}, {}
    # `vulcan serve` keeps the index fresh, so it needs a profile with index
    # access; the fixture's `benchmark_public` (part of its digest) has none.
    # vulcan-cli/tests/mdbase_frontends.rs covers restricted HTTP parity.
    skipped = {name: "benchmark_public grants no index access, which vulcan serve requires"
               for name in frontends if restricted and name.startswith("http_")}
    for name in frontends:
        if name in skipped:
            continue
        run, close = frontend_runner(name, binary, collection, restricted, timeout)
        observations = []
        try:
            for iteration in range(samples + 1):
                kind, parameter = cases[iteration % len(cases)]
                query_path = root / f"queries/{kind}-{parameter}.json"
                start = time.perf_counter()
                try:
                    stdout = run(query_path)
                    elapsed = time.perf_counter() - start
                    checked = cli_measure.check_response(stdout, manifest["records"], kind,
                                                         parameter, restricted)
                    envelopes.setdefault(query_path.name, {}).setdefault(name, canonical(stdout))
                    observations.append({"iteration": iteration, "query": query_path.name,
                                         "wall_seconds": elapsed, "ok": True, **checked})
                except (subprocess.TimeoutExpired, ValueError, KeyError, TypeError, OSError) as error:
                    observations.append({"iteration": iteration, "query": query_path.name, "ok": False,
                                         "wall_seconds": time.perf_counter() - start, "error": str(error)})
                    break
        finally:
            close()
        repeated = [row["wall_seconds"] for row in observations[1:] if row["ok"]]
        reports[name] = {
            "complete_and_valid": len(observations) == samples + 1 and all(row["ok"] for row in observations),
            "first_request": observations[0],
            "repeated_samples": len(repeated),
            "repeated_wall_seconds": cli_measure.percentiles(repeated) if repeated else None,
            "errors": [row for row in observations if not row["ok"]],
        }
    mismatched = sorted(query for query, by_frontend in envelopes.items()
                        if len(set(by_frontend.values())) != 1)
    unchanged = cli_measure.verify_fixture(root) == manifest
    same_binary = hashlib.sha256(binary.read_bytes()).hexdigest() == binary_hash
    return {
        "measurement": "public_mdb_frontends_diagnostic", "acceptance_gate_result": "not_evaluated",
        "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "binary_sha256": binary_hash, "binary_unchanged": same_binary,
        "fixture": {key: manifest[key] for key in ("generator_version", "records", "payload_sha256")
                    if key in manifest},
        "fixture_unchanged": unchanged,
        "machine": {"platform": platform.platform(), "architecture": platform.machine(),
                    "processor": platform.processor(), "acceptance_reference_machine": False},
        "method": {"permission_profile": "benchmark_public" if restricted else None,
                   "requested_repeated_samples": samples, "concurrency": 1,
                   "query_cases": [f"{kind}-{parameter}" for kind, parameter in cases],
                   "timing": {"cli": "process start through exit",
                              "python": "Collection.query call, including its CLI process and JSON decoding",
                              "http_oneshot": "loopback request through decoded response (urllib, one connection per request)",
                              "http_watched": "loopback request through decoded response (urllib, one connection per request)"},
                   "percentiles": "nearest rank; repeated successful requests only"},
        "envelope_parity": {"query_cases_compared": len(envelopes), "mismatched": mismatched},
        "frontends": reports,
        "skipped_frontends": skipped,
        "complete_and_valid": (unchanged and same_binary and not mismatched
                               and all(report["complete_and_valid"] for report in reports.values())),
        "limitations": ["Development host, not the designated reference machine; no acceptance verdict",
                        "Sequential single-client requests; no concurrent readers or writers",
                        "Host power/frequency/background load and OS page cache are uncontrolled"],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("fixture", type=Path, help="a directory made by generate_mdb_fixture.py")
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=1000,
                        help="repeated requests per frontend after one separately reported request")
    parser.add_argument("--restricted", action="store_true")
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--frontends", default=",".join(FRONTENDS),
                        help=f"comma-separated subset of {', '.join(FRONTENDS)}")
    args = parser.parse_args()
    frontends = [name for name in args.frontends.split(",") if name]
    if args.samples < 1 or any(name not in FRONTENDS for name in frontends):
        parser.error("invalid samples or frontends")
    report = measure(args.binary, args.fixture, args.samples, args.restricted, args.timeout, frontends)
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["complete_and_valid"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
