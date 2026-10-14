#!/usr/bin/env python3
"""Diagnostic baseline for daemon client routing (Roadmap 10.5), not an acceptance verdict.

For each candidate routed read operation, measures the same request on the public MDB fixture
through up to three frontends:

- cli: one `vulcan` process per request (direct mode, default blocking refresh)
- mcp: repeated `tools/call` requests in one `vulcan mcp` stdio process (a warm process
  without retained note-store sessions)
- http: repeated requests to one watched `vulcan serve` process (a warm hosted process,
  with retained sessions where the route has them)

It also measures the process floor (`vulcan --version`), which bounds any routed command from
below because a routed client still starts a process, and the cost of checking for a missing
daemon runtime record. Frontends return different report shapes, so this records latency and
success only; result equivalence belongs to the routing slices' differential tests.
"""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import subprocess
import time
import urllib.parse
import urllib.request

import measure_mdb_cli as mdb_cli

NOTE = "public/task/03/record-000003.md"
LINKED = "public/contact/46/record-000046.md"
DSL = "from notes where type = task and status = active order by title limit 50"
DQL = 'TABLE title FROM "public/task" WHERE status = "active" SORT title ASC LIMIT 50'


def operations():
    """Candidate routed reads: (name, CLI arguments, MCP call or None, HTTP request or None)."""
    return [
        ("status", ["status"], ("status", {}), None),
        ("query", ["query", DSL], ("query", {"query": DSL}), ("/query", {"dsl": DSL})),
        ("dql", ["query", "--language", "dql", DQL], ("query", {"query": DQL, "engine": "dql"}),
         ("/dataview/query", {"dql": DQL})),
        ("ls", ["ls", "--where", "type = task"], None, None),
        ("search", ["search", "Task 000027"], ("search", {"query": "Task 000027"}),
         ("/search", {"q": "Task 000027"})),
        ("note_get", ["note", "get", NOTE], ("note_get", {"note": NOTE}), None),
        ("note_info", ["note", "info", NOTE], ("note_info", {"note": NOTE}), None),
        ("note_outline", ["note", "outline", NOTE], ("note_outline", {"note": NOTE}), None),
        ("note_backlinks", ["note", "backlinks", LINKED], None, None),
        ("tasks_query", ["tasks", "query", "not done"], ("task_query", {"query": "not done"}), None),
        ("graph_stats", ["graph", "stats"], None, ("/graph/stats", {})),
        ("graph_hubs", ["graph", "hubs"], None, None),
        ("graph_path", ["graph", "path", NOTE, LINKED], None, None),
    ]


def summarize(observations):
    """First request separately; nearest-rank percentiles over the repeated successful ones."""
    if not observations:
        return {"complete": False, "first_ms": None, "repeated_ms": None, "failures": []}
    failures = [row for row in observations if not row["ok"]]
    repeated = [row["ms"] for row in observations[1:] if row["ok"]]
    summary = None
    if repeated:
        ordered = sorted(repeated)
        summary = {f"p{p}": ordered[max(0, math.ceil(len(ordered) * p / 100) - 1)]
                   for p in (50, 95, 99)}
        summary["max"] = ordered[-1]
        summary["samples"] = len(ordered)
    return {"complete": not failures, "first_ms": observations[0]["ms"],
            "repeated_ms": summary, "failures": failures[:3]}


def timed(call):
    start = time.perf_counter()
    try:
        call()
        return {"ok": True, "ms": (time.perf_counter() - start) * 1000}
    except Exception as error:  # every failure is recorded, never retried
        return {"ok": False, "ms": (time.perf_counter() - start) * 1000, "error": str(error)[:2048]}


def run_cli(command, timeout):
    result = subprocess.run(command, capture_output=True, timeout=timeout, check=False)
    if result.returncode:
        raise ValueError(f"exit {result.returncode}: {result.stderr[:1024]!r}")
    parse_output(result.stdout)


def parse_output(stdout):
    """One JSON document, or line-delimited JSON for streamed listings."""
    try:
        return [json.loads(stdout)]
    except json.JSONDecodeError:
        return [json.loads(line) for line in stdout.splitlines() if line.strip()]


class Mcp:
    """One `vulcan mcp` stdio process speaking line-delimited JSON-RPC."""

    def __init__(self, binary, vault):
        # Packs holding every measured tool; `note_info` is a read tool in `notes-manage`.
        packs = ("notes-read", "notes-manage", "search", "status", "graph", "tasks")
        command = [str(binary), "--vault", str(vault), "mcp"]
        for pack in packs:
            command += ["--tool-pack", pack]
        self.process = subprocess.Popen(command,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, text=True)
        self.next_id = 0
        self.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                    "clientInfo": {"name": "routing-baseline", "version": "1"}})
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def send(self, message):
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def request(self, method, params):
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params})
        while True:
            line = self.process.stdout.readline()
            if not line:
                raise ValueError(f"mcp exited {self.process.poll()}")
            message = json.loads(line)
            if message.get("id") == self.next_id:
                break
        if "error" in message:
            raise ValueError(f"mcp error {message['error']}")
        return message["result"]

    def call(self, tool, arguments):
        result = self.request("tools/call", {"name": tool, "arguments": arguments})
        if result.get("isError"):
            raise ValueError(f"tool error {json.dumps(result)[:1024]}")

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=30)


class Serve:
    """One watched `vulcan serve` process on a loopback port it picked itself."""

    def __init__(self, binary, vault):
        self.process = subprocess.Popen([str(binary), "--vault", str(vault), "serve",
                                         "--bind", "127.0.0.1:0"], stdin=subprocess.DEVNULL,
                                        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                        text=True)
        self.address = self.token = None
        while self.address is None or self.token is None:
            line = self.process.stderr.readline()
            if not line:
                raise ValueError(f"serve exited {self.process.wait()}")
            if line.startswith("Vulcan serve token: "):
                self.token = line.split(": ", 1)[1].strip()
            elif line.startswith("Vulcan serve listening on http://"):
                self.address = line.rsplit("http://", 1)[1].strip()

    def get(self, path, params, timeout):
        url = f"http://{self.address}{path}"
        if params:
            url += "?" + urllib.parse.urlencode(params)
        request = urllib.request.Request(url, headers={"x-vulcan-token": self.token})
        with urllib.request.urlopen(request, timeout=timeout) as response:
            json.loads(response.read())

    def close(self):
        self.process.terminate()
        self.process.wait(timeout=30)


def missing_record_check_ns(samples=10_000):
    """Upper bound for the daemon-absent check: one `stat` of a missing file, from Python."""
    missing = Path("/nonexistent-vulcan-runtime-record") if os.name != "nt" else Path("Z:/none")
    start = time.perf_counter_ns()
    for _ in range(samples):
        missing.exists()
    return (time.perf_counter_ns() - start) / samples


def measure(binary, root, samples=20, timeout=600, only=None, frontends=("cli", "mcp", "http")):
    if samples < 1 or timeout <= 0:
        raise ValueError("invalid measurement parameters")
    root, binary = root.resolve(), binary.resolve(strict=True)
    manifest = mdb_cli.verify_fixture(root)
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    vault = root / "collection"
    # Index once outside timing: the baseline compares requests against a current index.
    subprocess.run([str(binary), "--vault", str(vault), "index", "scan"], capture_output=True,
                   check=True, timeout=timeout)
    selected = [op for op in operations() if only is None or op[0] in only]
    floor = summarize([timed(lambda: subprocess.run([str(binary), "--version"],
                                                    capture_output=True, check=True,
                                                    timeout=timeout))
                       for _ in range(samples + 1)])
    results = {name: {} for name, *_ in selected}
    if "cli" in frontends:
        for name, arguments, _, _ in selected:
            command = [str(binary), "--vault", str(vault), "--output", "json", *arguments]
            results[name]["cli"] = summarize(
                [timed(lambda: run_cli(command, timeout)) for _ in range(samples + 1)])
    if "mcp" in frontends:
        mcp = Mcp(binary, vault)
        try:
            for name, _, call, _ in selected:
                if call:
                    results[name]["mcp"] = summarize(
                        [timed(lambda: mcp.call(*call)) for _ in range(samples + 1)])
        finally:
            mcp.close()
    if "http" in frontends:
        serve = Serve(binary, vault)
        try:
            for name, _, _, http in selected:
                if http:
                    results[name]["http"] = summarize(
                        [timed(lambda: serve.get(*http, timeout)) for _ in range(samples + 1)])
        finally:
            serve.close()
    unchanged = mdb_cli.verify_fixture(root) == manifest
    same_binary = hashlib.sha256(binary.read_bytes()).hexdigest() == binary_hash
    complete = floor["complete"] and all(
        frontend["complete"] for result in results.values() for frontend in result.values())
    return {
        "measurement": "daemon_routing_baseline", "acceptance_gate_result": "not_evaluated",
        "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "binary": str(binary), "binary_sha256": binary_hash, "binary_unchanged": same_binary,
        "fixture": manifest, "fixture_unchanged": unchanged,
        "machine": {"platform": platform.platform(), "architecture": platform.machine(),
                    "cpus": os.cpu_count(),
                    "load_average": os.getloadavg() if hasattr(os, "getloadavg") else None},
        "method": {"requested_repeated_samples": samples, "concurrency": 1,
                   "cli": "process start through exit, default blocking refresh",
                   "mcp": "tools/call round trip in one stdio process",
                   "http": "loopback GET round trip to one watched serve process",
                   "percentiles": "nearest rank over repeated successful requests",
                   "missing_record_check": "Python os.stat of a missing path; an upper bound"},
        "complete_and_valid": complete and unchanged and same_binary,
        "process_floor": floor,
        "missing_record_check_ns": missing_record_check_ns(),
        "operations": results,
        "limitations": ["Frontends return different report shapes; equivalence is not checked",
                        "No concurrent writers or external edits during measurement",
                        "Host power, frequency, background load, and OS page cache are uncontrolled"],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixture", type=Path)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=20,
                        help="repeated requests after one separately reported request")
    parser.add_argument("--timeout", type=float, default=600)
    parser.add_argument("--only", action="append", help="operation name; repeatable")
    parser.add_argument("--frontend", action="append", choices=("cli", "mcp", "http"))
    args = parser.parse_args()
    report = measure(args.binary, args.fixture, args.samples, args.timeout, args.only,
                     tuple(args.frontend or ("cli", "mcp", "http")))
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["complete_and_valid"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
