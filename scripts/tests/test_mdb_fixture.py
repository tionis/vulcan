import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


MODULE = Path(__file__).resolve().parents[1] / "generate_mdb_fixture.py"
SPEC = importlib.util.spec_from_file_location("generate_mdb_fixture", MODULE)
fixture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fixture)


class MdbFixtureTests(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("VULCAN_TEST_BINARY"), "set VULCAN_TEST_BINARY for CLI semantic checks")
    def test_cli_accepts_controls_queries_and_restricted_profile(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "fixture"
            fixture.generate(root, 120)

            def run(*arguments):
                result = subprocess.run([os.environ["VULCAN_TEST_BINARY"], "--vault",
                                         str(root / "collection"), "--output", "json",
                                         "mdbase", *arguments], capture_output=True, text=True,
                                        check=True, timeout=60)
                return json.loads(result.stdout)

            status = run("status")
            self.assertTrue(status["valid"], status)
            self.assertEqual(status["result"]["types"], 3)
            self.assertEqual(status["result"]["records"], 120)
            restricted = run("status", "--permissions", "benchmark_public")
            self.assertEqual(restricted["result"]["records"], 108)
            invalid = run("validate", fixture.record_path(97))
            self.assertFalse(invalid["valid"], invalid)
            valid = run("validate", fixture.record_path(15))
            self.assertTrue(valid["valid"], valid)
            record = run("read", fixture.record_path(15))["result"]
            self.assertNotIn("status", record["frontmatter"])
            self.assertEqual(record["effective_frontmatter"]["status"], "open")
            for query in sorted((root / "queries").glob("*.json")):
                result = run("query", "--file", str(query), "--permissions", "benchmark_public")
                self.assertIn("meta", result)
                self.assertLessEqual(len(result["results"]), 50)
                self.assertTrue(all(row["file"]["path"].startswith("public/")
                                    for row in result["results"]))
                self.assertFalse(any(d["code"] == "invalid_query" for d in result["diagnostics"]))
                if query.name.startswith("contact-"):
                    self.assertEqual(result["meta"]["total_count"], 1)

    def test_reproducible_and_seeded(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            first = fixture.generate(root / "first", 120)
            second = fixture.generate(root / "second", 120)
            self.assertEqual(first, second)
            for path in (root / "first").rglob("*"):
                if path.is_file():
                    self.assertEqual(path.read_bytes(), (root / "second" / path.relative_to(root / "first")).read_bytes())
            third = fixture.generate(root / "third", 120, seed=43)
            self.assertNotEqual(first["payload_sha256"], third["payload_sha256"])

    def test_records_have_exact_bodies_links_and_semantic_cases(self):
        seen = set()
        for index in range(120):
            data = fixture.note(index, 120, 42)
            _, frontmatter, body = data.split(b"---\n", 2)
            fields = json.loads(frontmatter)
            self.assertEqual(len(body), 4096)
            links = [line[2:-2] for line in body.decode().splitlines() if line.startswith("[[")]
            self.assertEqual(len(links), 10)
            self.assertEqual(len(set(links)), 10)
            self.assertTrue(all(link in {fixture.record_path(i) for i in range(120)} for link in links))
            seen.add("missing" if "description" not in fields else repr(fields["description"]))
            self.assertEqual("status" in fields, index % 5 != 0)
            self.assertEqual(isinstance(fields["priority"], str), index % 97 == 0)
        self.assertEqual(seen, {"missing", "None", "''", "'Public synthetic record'"})

    def test_refuses_existing_destination_and_invalid_parameters(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sentinel = root / "keep"
            sentinel.write_text("untouched")
            with self.assertRaises(FileExistsError):
                fixture.generate(root, 12)
            self.assertEqual(sentinel.read_text(), "untouched")
            for count, seed in [(11, 42), (100001, 42), (12, -1), (12, 2**32)]:
                with self.assertRaises(ValueError):
                    fixture.generate(root / "bad", count, seed)
                self.assertFalse((root / "bad").exists())

    def test_manifest_counts_and_framed_digest(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "fixture"
            manifest = fixture.generate(root, 121)
            paths = ["collection/mdbase.yaml", "collection/.vulcan/config.toml"]
            paths += [f"collection/_types/{kind}.md" for kind in fixture.KINDS]
            paths += ["collection/" + fixture.record_path(i) for i in range(121)]
            paths += [f"queries/{kind}-{parameter}.json" for kind in fixture.KINDS
                      for parameter in ((1, 4, 7) if kind == "contact" else ("open", "active", "done"))]
            digest = hashlib.sha256()
            for relative in paths:
                name, data = relative.encode(), (root / relative).read_bytes()
                digest.update(len(name).to_bytes(8, "big") + name)
                digest.update(len(data).to_bytes(8, "big") + data)
            self.assertEqual(manifest["payload_sha256"], digest.hexdigest())
            self.assertEqual(manifest["type_counts"], {"task": 41, "contact": 40, "project": 40})
            self.assertEqual(manifest["private_records"], 13)
            self.assertEqual(manifest["links"], 1210)
            self.assertEqual(manifest["payload_files"], len(paths))
            self.assertEqual(json.loads((root / "manifest.json").read_bytes()), manifest)


if __name__ == "__main__":
    unittest.main()
