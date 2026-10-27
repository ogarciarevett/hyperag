#!/usr/bin/env python3
import contextlib
import http.server
import json
import os
import sys
import threading
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


    def test_real_entrypoint_skips_without_writes_stderr_or_recovery(self):
        writes = []
        class Service(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                self.rfile.read(int(self.headers.get('Content-Length', '0')))
                writes.append(self.path)
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b'{}')
            def log_message(self, *args):
                pass
        service = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Service)
        thread = threading.Thread(target=service.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as name:
                root = Path(name)
                project = root / 'Known'
                project.mkdir()
                active = root / 'active'
                active.touch()
                state = root / 'state'
                credential = root / 'credential.json'
                grants = {'token': 'fixture-token', 'read_scopes': ['project:known'], 'write_scopes': ['project:known']}
                credential.write_text(json.dumps(grants))
                credential.chmod(0o600)
                executable = root / 'hook'
                executable.write_text('#!' + sys.executable + "\nimport os,urllib.request\nrequest=urllib.request.Request(os.getenv('HYPERAG_URL')+'/v1/memories',data=b'{}',method='POST')\nwith urllib.request.urlopen(request,timeout=2) as response:response.read()\n")
                executable.chmod(0o700)
                rollback = root / 'rollback'
                rollback.write_text('#!/bin/sh\ntouch ' + str(root / 'recovery-called') + '\n')
                rollback.chmod(0o700)
                config = root / 'config.json'
                config.write_text(json.dumps({'binary': str(executable), 'credential_file': str(credential),
                    'active_marker': str(active), 'state_dir': str(state),
                    'url': 'http://127.0.0.1:' + str(service.server_port), 'rollback_command': [str(rollback)]}))
                command = [sys.executable, '-I', str(Path(__file__).with_name('guard.py')),
                           '--config', str(config), 'hook', 'prompt', '--harness', 'claude-code']
                payload = json.dumps({'cwd': str(project), 'prompt': 'synthetic fixture input'})
                def invoke():
                    return subprocess.run(command, input=payload, text=True, capture_output=True,
                                          timeout=5, env={'PATH': os.defpath})
                # Positive control proves this CLI really forwards an eligible hook.
                granted = invoke()
                self.assertEqual((granted.returncode, granted.stderr), (0, ''))
                self.assertEqual(writes, ['/v1/memories'])
                writes.clear()
                grants['write_scopes'] = []
                credential.write_text(json.dumps(grants))
                original = credential.read_bytes()
                skipped = invoke()
                self.assertEqual((skipped.returncode, skipped.stdout, skipped.stderr), (0, '', ''))
                self.assertEqual(writes, [])
                self.assertEqual(credential.read_bytes(), original)
                self.assertTrue(active.exists())
                self.assertFalse((state / 'trips.jsonl').exists())
                self.assertFalse((state / 'recovery').exists())
                self.assertFalse((root / 'recovery-called').exists())
                self.assertEqual((state / 'skips.jsonl').read_text().splitlines(),
                                 [json.dumps({'category': 'skipped_ungranted'})])
        finally:
            service.shutdown()
            service.server_close()
            thread.join(timeout=2)


if __name__ == "__main__":
    unittest.main()
