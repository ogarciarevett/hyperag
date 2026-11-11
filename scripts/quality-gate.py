#!/usr/bin/env python3
"""Read-only language gates; the first failure stops the caller."""

import argparse
import json
from importlib.metadata import version
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def run(*command):
    print("+", " ".join(command), flush=True)
    subprocess.run(command, cwd=ROOT, check=True)


def check_node(manifest):
    engine = manifest.get("engines", {}).get("node")
    if engine != "^24.22.1":
        raise RuntimeError("package.json engines.node must stay ^24.22.1")
    actual = subprocess.check_output(["node", "--version"], cwd=ROOT, text=True).strip()
    # Stable releases only: >=24.22.1 <25, without evaluating any range syntax.
    release = re.fullmatch(r"v24\.(\d+)\.(\d+)", actual)
    if not release or tuple(map(int, release.groups())) < (22, 1):
        raise RuntimeError(f"Node must satisfy {engine}; found {actual}")


def check_bun():
    actual = subprocess.check_output(["bun", "--version"], cwd=ROOT, text=True).strip()
    if actual != "1.4.3":
        raise RuntimeError(f"Bun must be 1.4.3; found {actual}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "language",
        nargs="?",
        choices=["all", "python", "typescript", "rust"],
        default="all",
    )
    language = parser.parse_args().language
    if language in ("all", "python"):
        for package, pin in [("black", "26.10.1"), ("ruff", "0.14.3")]:
            if version(package) != pin:
                raise RuntimeError(
                    f"{package} must be {pin}; install requirements-quality.txt"
                )
        run(sys.executable, "-m", "black", "--check", "--workers", "1", ".")
        run(sys.executable, "-m", "ruff", "check", ".")
    if language in ("all", "typescript"):
        manifest = json.loads((ROOT / "package.json").read_text())
        check_node(manifest)
        check_bun()
        pin = manifest["devDependencies"]["@biomejs/biome"]
        biome = str(ROOT / "node_modules/.bin/biome")
        actual = subprocess.check_output(
            [biome, "--version"], cwd=ROOT, text=True
        ).strip()
        if pin != "2.6.4" or actual != "Version: 2.6.4":
            raise RuntimeError("Biome must be 2.6.4; run bun install --frozen-lockfile")
        run(biome, "check", ".")
        run("bun", "run", "typecheck")
    if language in ("all", "rust"):
        run("cargo", "+1.99.0", "fmt", "--all", "--", "--check")
        run(
            "cargo",
            "+1.99.0",
            "clippy",
            "--release",
            "--locked",
            "-p",
            "hyperag",
            "--lib",
            "--no-deps",
        )


if __name__ == "__main__":
    try:
        main()
    except (subprocess.CalledProcessError, RuntimeError, OSError) as error:
        print(f"quality gate failed: {error}", file=sys.stderr)
        sys.exit(1)
