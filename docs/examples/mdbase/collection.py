"""Structured access to an mdbase collection through the vulcan CLI.

Each method runs one `vulcan mdbase` command with `--output json` and returns
its canonical envelope as Python data, so scripts share the CLI's inspect,
schema, read, and query service and its permission checks. Standard library
only; no daemon or App package is needed.

    from collection import Collection
    tasks = Collection("~/notes").query({"types": ["task"], "where": "status == 'open'"})

Run as a script to print open tasks as JSON lines:

    python3 collection.py VAULT [--status open] [--permissions PROFILE]
"""

import argparse
import json
import os
import subprocess


class CollectionError(RuntimeError):
    """A vulcan command failed; the message is its stderr."""


class Collection:
    def __init__(self, vault, binary=None, permissions=None):
        self.vault = os.path.expanduser(vault)
        self.binary = binary or os.environ.get("VULCAN", "vulcan")
        self.permissions = permissions

    def _run(self, *args):
        command = [self.binary, "--vault", self.vault, "--output", "json"]
        if self.permissions:
            command += ["--permissions", self.permissions]
        result = subprocess.run(
            command + ["mdbase", *args], capture_output=True, text=True, check=False
        )
        if result.returncode != 0:
            # JSON mode reports errors as {"code": ..., "error": ...} on stdout.
            try:
                error = json.loads(result.stdout)
                raise CollectionError(f"{error['code']}: {error['error']}")
            except (ValueError, KeyError, TypeError):
                pass
            raise CollectionError(result.stderr.strip() or f"exit status {result.returncode}")
        return json.loads(result.stdout)

    def status(self):
        """Collection discovery and registry health."""
        return self._run("status")

    def types(self):
        """Portable type definitions, including their schemas."""
        return self._run("types")

    def read(self, path, metadata=True):
        """One record; metadata only (no body, links, or tags) by default."""
        return self._run("read", path, *(["--metadata"] if metadata else []))

    def patch(self, path, revision, set=None, unset=(), dry_run=False):
        """Patch persisted frontmatter, checked against `revision` from read().

        Values are JSON; a changed record raises CollectionError starting
        with `concurrent_modification`. Returns the new revision.
        """
        args = ["patch", path, "--if-revision", revision]
        for key, value in (set or {}).items():
            args += ["--set", f"{key}={json.dumps(value)}"]
        for key in unset:
            args += ["--unset", key]
        if dry_run:
            args.append("--dry-run")
        return self._run(*args)["result"]["revision"]

    def query(self, query):
        """A canonical mdbase query given as a dict; JSON is valid query YAML."""
        return self._run("query", json.dumps(query))


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("vault")
    parser.add_argument("--status", default="open")
    parser.add_argument("--permissions")
    arguments = parser.parse_args()
    collection = Collection(arguments.vault, permissions=arguments.permissions)
    report = collection.query(
        {
            "types": ["task"],
            "where": f"status == {json.dumps(arguments.status)}",
            "order_by": [{"field": "title"}],
            "select": ["title"],
        }
    )
    for row in report["results"]:
        record = collection.read(row["file"]["path"])["result"]
        print(json.dumps({"path": record["path"], "title": row["values"]["title"],
                          "types": record["types"], "valid": not any(
                              d["severity"] == "error" for d in record["diagnostics"])}))


if __name__ == "__main__":
    main()
