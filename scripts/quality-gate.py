#!/usr/bin/env python3
"""Read-only language gates; the first failure stops the caller."""

import argparse
from importlib.metadata import version
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def run(*command):
    print("+", " ".join(command), flush=True)
    subprocess.run(command, cwd=ROOT, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "language",
        nargs="?",
        choices=["all", "python"],
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


if __name__ == "__main__":
    try:
        main()
    except (subprocess.CalledProcessError, RuntimeError, OSError) as error:
        print(f"quality gate failed: {error}", file=sys.stderr)
        sys.exit(1)
