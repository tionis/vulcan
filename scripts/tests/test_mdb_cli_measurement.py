import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location("measure_mdb_cli", SCRIPTS / "measure_mdb_cli.py")
measurement = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(measurement)


class MdbCliMeasurementTests(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("VULCAN_TEST_BINARY"), "set VULCAN_TEST_BINARY for real CLI checks")
    def test_real_cli_unrestricted_and_restricted_queries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "fixture"
            measurement.fixture.generate(root, 120)
            for restricted in (False, True):
                report = measurement.measure(Path(os.environ["VULCAN_TEST_BINARY"]), root,
                                             samples=8, restricted=restricted)
                self.assertTrue(report["complete_and_valid"], report)
                self.assertEqual(len(report["repeated_requests"]), 8)

    def test_digest_verification_detects_changed_records(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "fixture"
            manifest = measurement.fixture.generate(root, 120)
            self.assertEqual(measurement.verify_fixture(root), manifest)
            extra = root / "collection/extra.md"
            extra.write_text("Extra record")
            with self.assertRaisesRegex(ValueError, "membership"):
                measurement.verify_fixture(root)
            extra.unlink()
            (root / "collection" / measurement.fixture.record_path(1)).write_text("Changed")
            with self.assertRaisesRegex(ValueError, "digest"):
                measurement.verify_fixture(root)

    def test_oracle_handles_defaults_permissions_and_order(self):
        self.assertEqual(len(measurement.expected_paths(10000, "task", "open", False)), 1556)
        for restricted in (False, True):
            paths = measurement.expected_paths(120, "task", "open", restricted)
            report = {"results": [{"file": {"path": path}} for path in paths[:50]],
                      "meta": {"total_count": len(paths)}, "diagnostics": []}
            data = json.dumps(report).encode()
            self.assertEqual(measurement.check_response(data, 120, "task", "open", restricted)["rows"], len(paths))
            report["meta"]["total_count"] += 1
            with self.assertRaisesRegex(ValueError, "total"):
                measurement.check_response(json.dumps(report).encode(), 120, "task", "open", restricted)
        self.assertEqual(measurement.expected_paths(120, "contact", 1, True), [measurement.fixture.record_path(1)])

    def test_nearest_rank_and_empty_samples(self):
        self.assertEqual(measurement.percentiles([3, 1, 2]), {"p50": 2, "p95": 3, "p99": 3})
        with self.assertRaises(ValueError):
            measurement.percentiles([])

    def test_runner_separates_first_request_and_reports_failures(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "fixture"
            measurement.fixture.generate(root, 120)
            binary = Path(sys.executable)

            def run(command, **kwargs):
                name = Path(command[command.index("--file") + 1]).stem
                kind, parameter = name.split("-")
                if kind == "contact":
                    parameter = int(parameter)
                paths = measurement.expected_paths(120, kind, parameter, False)
                return subprocess.CompletedProcess(command, 0, json.dumps({
                    "results": [{"file": {"path": path}} for path in paths[:50]],
                    "meta": {"total_count": len(paths)}, "diagnostics": []}).encode(), b"")

            with patch.object(measurement, "run_cli", side_effect=run):
                report = measurement.measure(binary, root, samples=9)
            self.assertTrue(report["complete_and_valid"])
            self.assertEqual(len(report["repeated_requests"]), 9)
            self.assertEqual(report["acceptance_gate_result"], "not_evaluated")
            self.assertEqual(len({row["query"] for row in report["repeated_requests"]}), 9)
            with patch.object(measurement, "run_cli", side_effect=subprocess.TimeoutExpired("query", 1)):
                report = measurement.measure(binary, root, samples=2)
            self.assertFalse(report["complete_and_valid"])
            self.assertFalse(report["first_request"]["ok"])
            self.assertIsNone(report["repeated_wall_seconds"])


if __name__ == "__main__":
    unittest.main()
