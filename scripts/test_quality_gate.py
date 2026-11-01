"""Exercise the contributor command with real pinned tools and disposable sources."""

import json
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


class QualityGate(unittest.TestCase):
    def check_python(self, source):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "scripts").mkdir()
            shutil.copy2(ROOT / "scripts/quality-gate.py", root / "scripts")
            shutil.copy2(ROOT / "pyproject.toml", root)
            (root / "example.py").write_text(source)
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
                capture_output=True,
                text=True,
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

    def check_typescript(
        self, text, format_source=False, typecheck="touch continued-typecheck"
    ):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "scripts").mkdir()
            shutil.copy2(ROOT / "scripts/quality-gate.py", root / "scripts")
            shutil.copy2(ROOT / "biome.json", root)
            (root / "node_modules").symlink_to(
                ROOT / "node_modules", target_is_directory=True
            )
            (root / "package.json").write_text(
                json.dumps(
                    {
                        "devDependencies": {"@biomejs/biome": "2.6.4"},
                        "scripts": {"typecheck": typecheck},
                    }
                )
            )
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / ".gitignore").write_text("")
            source = root / "example.ts"
            source.write_text(text)
            if format_source:
                subprocess.run(
                    [
                        str(ROOT / "node_modules/.bin/biome"),
                        "format",
                        "--write",
                        "example.ts",
                    ],
                    cwd=root,
                    check=True,
                    capture_output=True,
                )
            result = subprocess.run(
                [sys.executable, str(root / "scripts/quality-gate.py"), "typescript"],
                cwd=root,
                capture_output=True,
                text=True,
            )
            return result, (root / "continued-typecheck").exists()

    def test_typescript_gate_stops_before_typecheck_on_nested_ternary(self):
        result, continued = self.check_typescript(
            "const a = true; const b = false; const c = a ? 1 : b ? 2 : 3;\n", True
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("noNestedTernary", result.stdout + result.stderr)
        self.assertFalse(continued)

    def test_typescript_format_failure_stops_typecheck(self):
        result, continued = self.check_typescript("const answer=42\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Formatter", result.stdout + result.stderr)
        self.assertFalse(continued)

    def test_typescript_positive_control_reaches_typecheck(self):
        result, continued = self.check_typescript("export const answer = 42;\n", True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(continued)

    def test_typescript_type_failure_stops_continuation(self):
        result, continued = self.check_typescript(
            'export const answer: number = "wrong";\n',
            True,
            "tsc --ignoreConfig --noEmit --strict --skipLibCheck --target ES2022 example.ts && touch continued-typecheck",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("TS2322", result.stdout + result.stderr)
        self.assertFalse(continued)


if __name__ == "__main__":
    unittest.main()
