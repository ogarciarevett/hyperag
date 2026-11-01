#!/usr/bin/env python3
import contextlib
import http.server
import json
import os
import sys
import threading
import time
import importlib.util
import io
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "guard", Path(__file__).with_name("guard.py")
)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


class GuardTest(unittest.TestCase):
    def test_linked_worktree_uses_common_repository_scope(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo, worktree = root / "Coordination", root / "chief"

            def git(*args):
                subprocess.run(
                    [
                        "git",
                        "-c",
                        "core.hooksPath=/dev/null",
                        "-c",
                        "commit.gpgsign=false",
                        *args,
                    ],
                    check=True,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )

            git("init", "--quiet", str(repo))
            git(
                "-C",
                str(repo),
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            )
            git("-C", str(repo), "worktree", "add", "--detach", str(worktree))
            nested = worktree / "nested"
            nested.mkdir()
            self.assertEqual(guard.scope_for(repo), "project:coordination")
            self.assertEqual(guard.scope_for(nested), "project:coordination")

    def test_ungranted_project_is_a_quiet_skip(self):
        logs = []
        credential = {
            "read_scopes": ["project:coordination"],
            "write_scopes": ["project:coordination"],
        }
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            self.assertFalse(
                guard.check_scope("project:unknown", credential, logs.append)
            )
        self.assertEqual(logs, [{"category": "skipped_ungranted"}])
        self.assertEqual(stderr.getvalue(), "")
        self.assertTrue(
            guard.check_scope("project:coordination", credential, logs.append)
        )
        with self.assertRaises(ValueError):
            guard.check_scope("shared:other", credential, logs.append)

    def test_real_entrypoint_revocation_skips_same_and_fresh_config(self):
        writes = []

        class Service(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                self.rfile.read(int(self.headers.get("Content-Length", "0")))
                writes.append(self.path)
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"{}")

            def log_message(self, *args):
                pass

        service = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Service)
        thread = threading.Thread(target=service.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as name:
                root = Path(name)
                project = root / "Known"
                project.mkdir()
                credential = root / "credential.json"
                grants = {
                    "token": "fixture-token",
                    "read_scopes": ["project:known"],
                    "write_scopes": ["project:known"],
                }
                credential.write_text(json.dumps(grants))
                credential.chmod(0o600)
                invocations = root / "child-invocations"
                executable = root / "hook"
                executable.write_text(
                    "#!"
                    + sys.executable
                    + "\nimport os,urllib.request\n"
                    + f"with open({str(invocations)!r}, 'a') as marker: marker.write('invoked\\n')\n"
                    + "request=urllib.request.Request(os.getenv('HYPERAG_URL')+'/v1/memories',data=b'{}',method='POST')\nwith urllib.request.urlopen(request,timeout=2) as response:response.read()\n"
                )
                executable.chmod(0o700)
                # Both namespaces exist before A runs; neither is reset or reused.
                configs = []
                for label in ("A", "B"):
                    namespace = root / label
                    namespace.mkdir(mode=0o700)
                    (namespace / "active").touch()
                    rollback = namespace / "rollback"
                    rollback.write_text(
                        '#!/bin/sh\nprintf "recovered\\n" >> "'
                        + str(namespace / "recovery-called")
                        + '"\n'
                    )
                    rollback.chmod(0o700)
                    config = namespace / "config.json"
                    config.write_text(
                        json.dumps(
                            {
                                "binary": str(executable),
                                "credential_file": str(credential),
                                "active_marker": str(namespace / "active"),
                                "state_dir": str(namespace / "state"),
                                "url": "http://127.0.0.1:" + str(service.server_port),
                                "rollback_command": [str(rollback)],
                            }
                        )
                    )
                    config.chmod(0o600)
                    configs.append(config)
                denied = root / "B"
                active, state = denied / "active", denied / "state"
                baseline = {path.name: path.read_bytes() for path in denied.iterdir()}
                command = [
                    sys.executable,
                    "-I",
                    str(Path(__file__).with_name("guard.py")),
                    "--config",
                    str(configs[0]),
                    "hook",
                    "prompt",
                    "--harness",
                    "claude-code",
                ]
                payload = json.dumps(
                    {"cwd": str(project), "prompt": "synthetic fixture input"}
                )

                def invoke(config):
                    command[4] = str(config)
                    return subprocess.run(
                        command,
                        input=payload,
                        text=True,
                        capture_output=True,
                        timeout=5,
                        env={"PATH": os.defpath},
                    )

                # Positive control proves this CLI really forwards an eligible hook.
                granted = invoke(configs[0])
                self.assertEqual((granted.returncode, granted.stderr), (0, ""))
                self.assertEqual(writes, ["/v1/memories"])
                self.assertEqual(invocations.read_text().splitlines(), ["invoked"])
                # A may legitimately trip latency policy; retain all its history.
                control = root / "A"
                control_state = control / "state"
                recovery_status = control_state / "recovery/status.json"
                if recovery_status.parent.exists():
                    deadline = time.monotonic() + 3
                    status = {}
                    while time.monotonic() < deadline:
                        if recovery_status.exists():
                            status = json.loads(recovery_status.read_text())
                            if status.get("state") in ("succeeded", "failed"):
                                break
                        time.sleep(0.01)
                    self.assertEqual(status.get("state"), "succeeded", status)
                    self.assertEqual(
                        (control / "recovery-called").read_text().splitlines(),
                        ["recovered"],
                    )
                diagnostics = {
                    str(path.relative_to(control_state)): path.read_text()
                    for path in (
                        control_state / "calls.jsonl",
                        control_state / "trips.jsonl",
                        recovery_status,
                    )
                    if path.exists()
                }
                self.assertTrue(
                    (control / "active").exists(),
                    f"same-config proof requires active positive control: {diagnostics}",
                )

                def artifacts(namespace):
                    return {
                        str(path.relative_to(namespace)): path.read_bytes()
                        for path in namespace.rglob("*")
                        if path.is_file()
                    }

                control_baseline = artifacts(control)
                grants["write_scopes"] = []
                credential.write_text(json.dumps(grants))
                original = credential.read_bytes()
                self.assertEqual(credential.stat().st_mode & 0o777, 0o600)
                self.assertTrue(active.exists())
                self.assertEqual(
                    {path.name: path.read_bytes() for path in denied.iterdir()},
                    baseline,
                )
                skipped = invoke(configs[1])
                self.assertEqual(
                    (skipped.returncode, skipped.stdout, skipped.stderr), (0, "", "")
                )
                self.assertEqual(writes, ["/v1/memories"])
                self.assertEqual(invocations.read_text().splitlines(), ["invoked"])
                self.assertEqual(credential.read_bytes(), original)
                self.assertTrue(active.exists())
                self.assertFalse((state / "trips.jsonl").exists())
                self.assertFalse((state / "recovery").exists())
                self.assertFalse((denied / "recovery-called").exists())
                self.assertEqual(
                    [path.name for path in state.iterdir()], ["skips.jsonl"]
                )
                self.assertEqual(
                    (state / "skips.jsonl").read_text().splitlines(),
                    [json.dumps({"category": "skipped_ungranted"})],
                )
                self.assertTrue((control / "active").exists())
                revoked = invoke(configs[0])
                self.assertEqual(
                    (revoked.returncode, revoked.stdout, revoked.stderr), (0, "", "")
                )
                self.assertEqual(
                    writes, ["/v1/memories"], "same-config revocation forwarded HTTP"
                )
                self.assertEqual(invocations.read_text().splitlines(), ["invoked"])
                self.assertEqual(credential.read_bytes(), original)
                self.assertTrue((control / "active").exists())
                self.assertEqual(
                    artifacts(control),
                    {
                        **control_baseline,
                        "state/skips.jsonl": (
                            json.dumps({"category": "skipped_ungranted"}) + "\n"
                        ).encode(),
                    },
                )
        finally:
            service.shutdown()
            service.server_close()
            thread.join(timeout=2)


if __name__ == "__main__":
    unittest.main()
