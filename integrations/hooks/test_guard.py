#!/usr/bin/env python3
import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("guard", Path(__file__).with_name("guard.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


class GuardTest(unittest.TestCase):
    def test_linked_worktree_uses_common_repository_scope(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo, worktree = root / "Coordination", root / "chief"
            def git(*args):
                subprocess.run(["git", "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", *args],
                               check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            git("init", "--quiet", str(repo))
            git("-C", str(repo), "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                "commit", "--allow-empty", "-m", "fixture")
            git("-C", str(repo), "worktree", "add", "--detach", str(worktree))
            nested = worktree / "nested"
            nested.mkdir()
            self.assertEqual(guard.scope_for(repo), "project:coordination")
            self.assertEqual(guard.scope_for(nested), "project:coordination")

    def test_ungranted_project_is_a_quiet_skip(self):
        logs = []
        credential = {"read_scopes": ["project:coordination"], "write_scopes": ["project:coordination"]}
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            self.assertFalse(guard.check_scope("project:unknown", credential, logs.append))
        self.assertEqual(logs, [{"category": "skipped_ungranted"}])
        self.assertEqual(stderr.getvalue(), "")
        self.assertTrue(guard.check_scope("project:coordination", credential, logs.append))
        with self.assertRaises(ValueError):
            guard.check_scope("shared:other", credential, logs.append)


if __name__ == "__main__":
    unittest.main()
