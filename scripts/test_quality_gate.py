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
    def check_python(self, source, versions=None, pins=None):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "scripts").mkdir()
            shutil.copy2(ROOT / "scripts/quality-gate.py", root / "scripts")
            shutil.copy2(ROOT / "pyproject.toml", root)
            (root / "example.py").write_text(source)
            env = version_shims(root, versions)
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

    def check_typescript(
        self,
        text,
        format_source=False,
        typecheck="touch continued-typecheck",
        versions=None,
        engines={"node": "^24.22.1"},
        language="typescript",
        biome_pin="2.6.4",
        biome_version=None,
    ):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "scripts").mkdir()
            shutil.copy2(ROOT / "scripts/quality-gate.py", root / "scripts")
            shutil.copy2(ROOT / "biome.json", root)
            shutil.copy2(ROOT / "pyproject.toml", root)
            if biome_version:
                tools = root / "node_modules/.bin"
                tools.mkdir(parents=True)
                wrapper = tools / "biome"
                wrapper.write_text(
                    '#!/bin/sh\nif [ "$*" = --version ]; then\n'
                    f"  echo {shlex.quote(biome_version)}\n  exit 0\nfi\n"
                    f'exec {shlex.quote(str(ROOT / "node_modules/.bin/biome"))} "$@"\n'
                )
                wrapper.chmod(0o755)
            else:
                (root / "node_modules").symlink_to(
                    ROOT / "node_modules", target_is_directory=True
                )
            (root / "package.json").write_text(
                json.dumps(
                    {
                        "engines": engines,
                        "devDependencies": {"@biomejs/biome": biome_pin},
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

    def test_wrong_biome_pins_stop_before_lint_and_typecheck(self):
        for drift in (
            {"biome_pin": "0.0.0"},
            {"biome_version": "Version: 0.0.0"},
        ):
            with self.subTest(drift):
                result, continued = self.check_typescript(
                    "export const answer = 42;\n", **drift
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Biome must be 2.6.4", result.stderr)
                self.assertNotIn("+", result.stdout)
                self.assertNotIn("CONTINUED", result.stdout)
                self.assertFalse(continued)

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

    def test_wrong_python_metadata_pin_stops_before_tools(self):
        for package, pin in (("black", "26.10.1"), ("ruff", "0.14.3")):
            with self.subTest(package):
                result = self.check_python("answer = 42\n", pins={package: "0.0.0"})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(f"{package} must be {pin}", result.stderr)
                self.assertNotIn("+", result.stdout)
                self.assertNotIn("CONTINUED", result.stdout)

    def test_real_pre_push_rejects_missing_quality_venv(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True, timeout=10)
            (root / "scripts").mkdir()
            shutil.copy2(ROOT / "scripts/quality-gate.py", root / "scripts")
            hook = root / "pre-push"
            shutil.copy2(ROOT / ".githooks/pre-push", hook)
            result = subprocess.run(
                ["sh", "-c", 'sh "$1" && printf CONTINUED', "hook-test", str(hook)],
                cwd=root,
                capture_output=True,
                text=True,
                timeout=30,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Missing quality venv; follow CONTRIBUTING.md", result.stderr)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
