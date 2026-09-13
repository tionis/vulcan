from pathlib import Path
import re
import unittest


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
WORKFLOW = REPOSITORY_ROOT / ".github" / "workflows" / "codeql.yml"
PINNED_ACTION = "b96794f015dfd88f77b49b1c93e0fa7110f94c63"


class CodeQlWorkflowTests(unittest.TestCase):
    def test_advanced_scanning_contract_is_checked_in(self) -> None:
        source = WORKFLOW.read_text(encoding="utf-8")

        self.assertIn("security-events: write", source)
        self.assertIn("queries: security-extended", source)
        self.assertIn("build-mode: none", source)
        self.assertIn("pull_request:", source)
        self.assertIn("schedule:", source)
        for language in ("actions", "javascript-typescript", "python", "rust"):
            self.assertRegex(source, rf"(?m)^\s+- {re.escape(language)}$")

        action_uses = re.findall(r"github/codeql-action/(?:init|analyze)@([^\s]+)", source)
        self.assertEqual(action_uses, [PINNED_ACTION, PINNED_ACTION])


if __name__ == "__main__":
    unittest.main()
