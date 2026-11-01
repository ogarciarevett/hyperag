"""Exercise the contributor command with real pinned tools and disposable sources."""

import json
import os
import pathlib
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
REAL_TYPECHECK = (
    "tsc --ignoreConfig --noEmit --strict --skipLibCheck --target ES2022 example.ts"
    " && touch continued-typecheck"
)


def version_shims(root, versions):
    """Environment whose runtimes misreport only `--version`; all else is real."""
    env = dict(os.environ)
    if versions:
        shims = root / "version-shims"
        shims.mkdir()
        for command, reported in versions.items():
            real = shlex.quote(shutil.which(command))
            shim = shims / command
            shim.write_text(
                '#!/bin/sh\nif [ "$*" = --version ]; then\n'
                f"  echo {reported}\n  exit 0\nfi\n"
                f'exec {real} "$@"\n'
            )
            shim.chmod(0o755)
        env["PATH"] = f"{shims}{os.pathsep}{env['PATH']}"
    return env


class QualityGate(unittest.TestCase):
    def check_python(self, source, versions=None):
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
                env=version_shims(root, versions),
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
        self,
        text,
        format_source=False,
        typecheck="touch continued-typecheck",
        versions=None,
        engines={"node": "^24.22.1"},
        language="typescript",
    ):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "scripts").mkdir()
            shutil.copy2(ROOT / "scripts/quality-gate.py", root / "scripts")
            shutil.copy2(ROOT / "biome.json", root)
            shutil.copy2(ROOT / "pyproject.toml", root)
            (root / "node_modules").symlink_to(
                ROOT / "node_modules", target_is_directory=True
            )
            (root / "package.json").write_text(
                json.dumps(
                    {
                        "engines": engines,
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
                [
                    "sh",
                    "-c",
                    '"$1" "$2" "$3" && printf CONTINUED',
                    "quality-test",
                    sys.executable,
                    str(root / "scripts/quality-gate.py"),
                    language,
                ],
                cwd=root,
                env=version_shims(root, versions),
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

    def assert_runtime_rejected(self, checked, name):
        result, continued = checked
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(name, result.stderr)
        self.assertNotIn("biome", result.stdout)
        self.assertFalse(continued)
        self.assertNotIn("CONTINUED", result.stdout)

    def test_unsupported_node_stops_before_lint_and_typecheck(self):
        for reported in (
            "v26.6.0",
            "v25.0.0",
            "v24.22.0",
            "v24.19.0",
            "v23.99.0",
            "v24.23.0-rc.1",
            "v124.22.1",
        ):
            with self.subTest(reported):
                self.assert_runtime_rejected(
                    self.check_typescript(
                        "export const answer = 42;\n",
                        True,
                        versions={"node": reported},
                    ),
                    "Node",
                )

    def test_compatible_node_runs_real_lint_and_typecheck(self):
        for reported in ("v24.22.2", "v24.23.0"):
            with self.subTest(reported):
                result, continued = self.check_typescript(
                    "export const answer = 42;\n",
                    True,
                    REAL_TYPECHECK,
                    versions={"node": reported},
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("biome", result.stdout)
                self.assertTrue(continued)
                self.assertIn("CONTINUED", result.stdout)

    def test_ambient_node_runs_real_lint_typecheck_and_continuation(self):
        ambient = subprocess.check_output(["node", "--version"], text=True).strip()
        result, continued = self.check_typescript(
            "export const answer = 42;\n",
            True,
            f"node --version && {REAL_TYPECHECK}",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("biome", result.stdout)
        self.assertIn(ambient, result.stdout)
        self.assertTrue(continued)
        self.assertIn("CONTINUED", result.stdout)

    def test_unpinned_bun_stops_before_lint_and_typecheck(self):
        for reported in ("1.3.14", "1.4.2", "1.4.4", "1.4.3-canary.1"):
            with self.subTest(reported):
                self.assert_runtime_rejected(
                    self.check_typescript(
                        "export const answer = 42;\n",
                        True,
                        versions={"bun": reported},
                    ),
                    "Bun",
                )

    def test_package_engine_contract_cannot_drift(self):
        for engines in ({"node": "^26.0.0"}, {"node": ">=24.22.1"}, {}):
            with self.subTest(engines):
                self.assert_runtime_rejected(
                    self.check_typescript(
                        "export const answer = 42;\n", True, engines=engines
                    ),
                    "engines.node",
                )

    def test_all_gate_enforces_runtime_before_typescript_tools(self):
        self.assert_runtime_rejected(
            self.check_typescript(
                "export const answer = 42;\n",
                True,
                versions={"node": "v26.6.0"},
                language="all",
            ),
            "Node",
        )

    def test_python_gate_ignores_javascript_runtimes(self):
        result = self.check_python(
            "answer = 42\n", {"node": "v26.6.0", "bun": "1.3.14"}
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("CONTINUED", result.stdout)

    def test_rust_gate_ignores_javascript_runtimes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "scripts").mkdir()
            shutil.copy2(ROOT / "scripts/quality-gate.py", root / "scripts")
            env = version_shims(root, {"node": "v26.6.0", "bun": "1.3.14"})
            # Only the shims are reachable, so the gate stops at the absent cargo.
            env["PATH"] = str(root / "version-shims")
            result = subprocess.run(
                [sys.executable, str(root / "scripts/quality-gate.py"), "rust"],
                cwd=root,
                env=env,
                capture_output=True,
                text=True,
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cargo", result.stderr)
        self.assertNotIn("Node", result.stderr)
        self.assertNotIn("Bun", result.stderr)


if __name__ == "__main__":
    unittest.main()
