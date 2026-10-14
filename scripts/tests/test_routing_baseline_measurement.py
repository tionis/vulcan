import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location("measure_routing_baseline",
                                              SCRIPTS / "measure_routing_baseline.py")
measurement = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(measurement)


class RoutingBaselineMeasurementTests(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("VULCAN_TEST_BINARY"), "set VULCAN_TEST_BINARY for real frontend checks")
    def test_every_operation_answers_on_every_frontend_it_has(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "fixture"
            measurement.mdb_cli.fixture.generate(root, 120)
            report = measurement.measure(Path(os.environ["VULCAN_TEST_BINARY"]), root,
                                         samples=2, timeout=120)
            self.assertTrue(report["complete_and_valid"], report)
            for name, cli, mcp, http in measurement.operations():
                expected = {"cli"} | ({"mcp"} if mcp else set()) | ({"http"} if http else set())
                self.assertEqual(set(report["operations"][name]), expected, name)
                for frontend in expected:
                    self.assertEqual(
                        report["operations"][name][frontend]["repeated_ms"]["samples"], 2)

    def test_operation_names_are_unique_and_every_one_has_a_cli_form(self):
        names = [name for name, *_ in measurement.operations()]
        self.assertEqual(len(names), len(set(names)))
        self.assertTrue(all(cli for _, cli, _, _ in measurement.operations()))

    def test_summary_reports_the_first_request_apart_and_keeps_failures(self):
        rows = [{"ok": True, "ms": 900.0}] + [{"ok": True, "ms": float(ms)} for ms in range(1, 11)]
        summary = measurement.summarize(rows)
        self.assertEqual(summary["first_ms"], 900.0)
        self.assertEqual(summary["repeated_ms"]["p50"], 5.0)
        self.assertEqual(summary["repeated_ms"]["p95"], 10.0)
        self.assertTrue(summary["complete"])
        failed = measurement.summarize(rows + [{"ok": False, "ms": 1.0, "error": "x"}])
        self.assertFalse(failed["complete"])
        self.assertEqual(failed["failures"][0]["error"], "x")

    def test_streamed_listings_parse_line_by_line(self):
        self.assertEqual(measurement.parse_output('{"a": 1}'), [{"a": 1}])
        self.assertEqual(measurement.parse_output('{"a": 1}\n{"b": 2}\n'), [{"a": 1}, {"b": 2}])
        with self.assertRaises(ValueError):
            measurement.parse_output("not json")


if __name__ == "__main__":
    unittest.main()
