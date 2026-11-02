"""Exercise the contributor command with real pinned tools and disposable sources."""

import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


class QualityGate(unittest.TestCase):
    def check_python(self, source, pins=None):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "scripts").mkdir()
            shutil.copy2(ROOT / "scripts/quality-gate.py", root / "scripts")
            shutil.copy2(ROOT / "pyproject.toml", root)
            (root / "example.py").write_text(source)
            env = dict(os.environ)
            if pins:
                metadata = root / "metadata"
                metadata.mkdir()
                for package, version in pins.items():
                    distribution = metadata / f"{package}-{version}.dist-info"
                    distribution.mkdir()
                    (distribution / "METADATA").write_text(
                        f"Metadata-Version: 2.1\nName: {package}\nVersion: {version}\n"
                    )
                env["PYTHONPATH"] = str(metadata)
            result = subprocess.run(
                [
                    "sh",
                    "-c",
                    '"$1" "$2" python && printf CONTINUED',
                    "quality-test",
                    sys.executable,
                    str(root / "scripts/quality-gate.py"),
                ],
                cwd=root,
                env=env,
                capture_output=True,
                text=True,
                timeout=30,
            )
            return result

    def test_formatted_python_allows_continuation(self):
        result = self.check_python("answer = 42\n")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("CONTINUED", result.stdout)

    def test_python_format_failure_stops_continuation(self):
        result = self.check_python("answer=42\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("would reformat", result.stderr)
        self.assertNotIn("ruff check", result.stdout)
        self.assertNotIn("CONTINUED", result.stdout)

    def test_python_lint_failure_stops_continuation(self):
        result = self.check_python("answer = missing_name\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("F821", result.stdout)
        self.assertNotIn("CONTINUED", result.stdout)

    def test_unannotated_e402_stops_continuation(self):
        result = self.check_python("answer = 42\nimport os\n\nprint(os.name)\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("E402", result.stdout)
        self.assertNotIn("CONTINUED", result.stdout)

    def test_wrong_python_metadata_pin_stops_before_tools(self):
        for package, pin in (("black", "26.10.1"), ("ruff", "0.14.3")):
            with self.subTest(package):
                result = self.check_python("answer = 42\n", pins={package: "0.0.0"})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(f"{package} must be {pin}", result.stderr)
                self.assertNotIn("+", result.stdout)
                self.assertNotIn("CONTINUED", result.stdout)


if __name__ == "__main__":
    unittest.main()
