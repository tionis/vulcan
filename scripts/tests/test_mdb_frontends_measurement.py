import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location("measure_mdb_frontends", SCRIPTS / "measure_mdb_frontends.py")
measurement = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(measurement)


class MdbFrontendMeasurementTests(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("VULCAN_TEST_BINARY"), "set VULCAN_TEST_BINARY for real frontend checks")
    def test_every_frontend_answers_alike_unrestricted_and_restricted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "fixture"
            measurement.fixture.generate(root, 120)
            for restricted in (False, True):
                report = measurement.measure(Path(os.environ["VULCAN_TEST_BINARY"]), root, samples=9,
                                             restricted=restricted, timeout=60,
                                             frontends=measurement.FRONTENDS)
                self.assertTrue(report["complete_and_valid"], report)
                self.assertEqual(report["envelope_parity"]["mismatched"], [])
                self.assertEqual(report["envelope_parity"]["query_cases_compared"], 9)
                measured = [name for name in measurement.FRONTENDS
                            if not (restricted and name.startswith("http_"))]
                self.assertEqual(sorted(report["frontends"]), sorted(measured))
                for name in measured:
                    self.assertEqual(report["frontends"][name]["repeated_samples"], 9)

    def test_envelopes_compare_independently_of_key_order(self):
        self.assertEqual(measurement.canonical('{"b": 1, "a": [2]}'),
                         measurement.canonical('{"a": [2], "b": 1}'))
        self.assertNotEqual(measurement.canonical('{"a": [2, 1]}'),
                            measurement.canonical('{"a": [1, 2]}'))


if __name__ == "__main__":
    unittest.main()
