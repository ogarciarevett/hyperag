"""Check a built release binary without models, databases or exposing a generated token."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile

binary = str(Path(sys.argv[1]).resolve())
version = subprocess.run([binary, "--version"], capture_output=True, text=True, check=True).stdout
assert version.startswith("hyperag ")
with tempfile.TemporaryDirectory(prefix="hyperag-release-check-") as directory:
    registry = Path(directory) / "tokens.json"
    token = subprocess.run([binary, "token", "release-test", "--file", str(registry)], capture_output=True, text=True, check=True).stdout.strip()
    assert token.startswith("hr_") and len(token) == 67
    assert json.loads(registry.read_text()) == {hashlib.sha256(token.encode()).hexdigest(): "release-test"}
print("Release CLI smoke: version and secure token creation passed.")
