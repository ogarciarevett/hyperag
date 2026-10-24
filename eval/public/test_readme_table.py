import json
from pathlib import Path
import subprocess
import tempfile
import unittest


class ReadmeTableTest(unittest.TestCase):
    def test_memory_results_require_complete_counts_and_keep_split_limits(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            memory = root / "memory"
            memory.mkdir()
            for profile, extra in {
                "bm25": {"questions": 1986, "expectedQuestions": 1986},
                "partial": {"questions": 1, "expectedQuestions": 1986},
                "missing": {},
                "empty": {"questions": 0, "expectedQuestions": 0},
                "invalid": {"questions": 1986, "expectedQuestions": 1986,
                            "locomo": {"f1_cat1_4": float("nan")}},
                "boolean": {"questions": 1986, "expectedQuestions": 1986,
                            "locomo": {"f1_cat1_4": True}},
            }.items():
                (memory / f"locomo.hyperag.{profile}.meta.json").write_text(json.dumps({
                    "system": "hyperag", "profile": profile,
                    "locomo": {"f1_cat1_4": 0.25}, **extra,
                }))
            readme = root / "README.md"
            readme.write_text("<!-- bench:start -->\n<!-- bench:end -->\n")
            subprocess.run([
                "python3", "-I", str(Path(__file__).with_name("readme_table.py")),
                "--results", str(root), "--readme", str(readme),
            ], check=True, capture_output=True, text=True)
            rendered = readme.read_text()
            self.assertIn("| bm25 | 0.250 |", rendered)
            for profile in ["partial", "missing", "empty", "invalid", "boolean"]:
                self.assertIn(f"| {profile} | N/D |", rendered)
            self.assertIn("not held out for the temporal channel", rendered)


if __name__ == "__main__":
    unittest.main()
