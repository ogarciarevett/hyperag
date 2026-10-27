"""Scope checks shared by local hook guards; no credentials or input are logged."""
import os
from pathlib import Path
import subprocess


def scope_for(cwd: Path) -> str:
    directory = Path(cwd)
    root = next((path for path in [directory, *directory.parents] if (path / ".git").exists()), None)
    name = (root or directory).name
    if root is not None:
        try:
            result = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "--path-format=absolute", "--git-common-dir"],
                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                check=True, timeout=1, env={"PATH": os.defpath},
            )
            common = Path(result.stdout.strip())
            if common.name == ".git":
                name = common.parent.name
        except (OSError, subprocess.SubprocessError):
            pass
    return "project:" + (name.lower() or "unknown")


def check_scope(scope: str, credential: dict, log) -> bool:
    if not scope.startswith("project:"):
        raise ValueError("hook scope must be a project scope")
    if scope in credential["write_scopes"] and scope in credential["read_scopes"]:
        return True
    log({"category": "skipped_ungranted"})
    return False
