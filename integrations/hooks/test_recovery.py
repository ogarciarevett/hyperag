#!/usr/bin/env python3
"""Local process tests: no service, database, model, or credentials required."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

MODULE = Path(__file__).with_name("recovery.py")
spec = importlib.util.spec_from_file_location("recovery", MODULE)
recovery = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recovery)


class RecoveryTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.state = self.root / "state"
        self.children = []
        self.addCleanup(self.reap)

    def reap(self):
        for pid in self.children:
            try:
                os.waitpid(pid, 0)
            except ChildProcessError:
                pass

    def wait_for(self, predicate):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.01)
        self.fail("recovery did not reach the expected checkpoint")

    def completed(self):
        path = self.state / "status.json"
        return path.exists() and json.loads(path.read_text())["state"] != "running"

    def script(self, name, body):
        path = self.root / name
        path.write_text(body)
        return path

    def test_recovery_survives_hook_process_group_timeout(self):
        stopped, restored = self.root / "stopped", self.root / "restored"
        command = self.script("restore.py", f"""import time
from pathlib import Path
Path({str(stopped)!r}).touch()
time.sleep(.3)
Path({str(restored)!r}).touch()
""")
        hook = self.script("hook.py", f"""import importlib.util,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('recovery',{str(MODULE)!r})
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
module.dispatch([{sys.executable!r},{str(command)!r}],Path({str(self.state)!r}))
time.sleep(10)
""")
        parent = subprocess.Popen([sys.executable, str(hook)], start_new_session=True)
        try:
            self.wait_for(stopped.exists)
            os.killpg(parent.pid, signal.SIGKILL)
            parent.wait(timeout=2)
            self.wait_for(restored.exists)
            self.wait_for(self.completed)
            self.assertEqual(json.loads((self.state / "status.json").read_text())["state"], "succeeded")
        finally:
            if parent.poll() is None:
                os.killpg(parent.pid, signal.SIGKILL)
                parent.wait(timeout=2)

    def test_duplicate_dispatch_runs_one_recovery(self):
        started, release = self.root / "started", self.root / "release"
        command = self.script("restore.py", f"""import time
from pathlib import Path
with Path({str(started)!r}).open('a') as file:file.write('started\\n')
deadline=time.monotonic()+5
while not Path({str(release)!r}).exists() and time.monotonic()<deadline:time.sleep(.01)
""")
        self.children.append(recovery.dispatch([sys.executable, str(command)], self.state))
        self.wait_for(started.exists)
        second = recovery.dispatch([sys.executable, str(command)], self.state)
        self.children.append(second)
        def exited():
            try:
                return os.waitpid(second, os.WNOHANG)[0] != 0
            except ChildProcessError:
                return True
        try:
            self.wait_for(exited)
            self.assertEqual(json.loads((self.state / "status.json").read_text())["state"], "running")
            self.assertIsNone(recovery.status_for(self.state, second))
            self.assertEqual(started.read_text().splitlines(), ["started"])
        finally:
            release.touch()
        self.wait_for(self.completed)
        self.assertEqual(stat.S_IMODE((self.state / "status.json").stat().st_mode), 0o600)

    def test_failure_records_exit_code_without_command_output(self):
        command = self.script("failure.py", "import sys\nprint('private-output',file=sys.stderr)\nsys.exit(17)\n")
        self.children.append(recovery.dispatch([sys.executable, str(command)], self.state))
        self.wait_for(self.completed)
        raw = (self.state / "status.json").read_text()
        result = json.loads(raw)
        self.assertEqual((result["state"], result["exit_code"]), ("failed", 17))
        self.assertNotIn("private-output", raw)
        self.assertNotIn(str(command), raw)

    def test_confirmed_dispatch_waits_for_this_workers_durable_status(self):
        for phase in ("running", "succeeded", "failed"):
            with self.subTest(phase=phase), patch.object(recovery, "dispatch", return_value=123), \
                    patch.object(recovery, "status_for", side_effect=[None, {"pid": 123, "state": phase}]) as status, \
                    patch.object(recovery.time, "sleep"):
                self.assertEqual(recovery.dispatch_confirmed(["/fixture/restore"], self.state), 123)
                self.assertEqual(status.call_count, 2)
                self.assertEqual(status.call_args.args, (self.state, 123))

    def test_confirmed_dispatch_rejects_an_existing_workers_status(self):
        self.state.mkdir()
        recovery._status(self.state, {"pid": 9, "state": "running"})
        with patch.object(recovery, "dispatch", return_value=123), \
                patch.object(recovery.time, "monotonic", side_effect=[0, .1]):
            with self.assertRaises(TimeoutError) as raised:
                recovery.dispatch_confirmed(["/fixture/restore"], self.state, timeout=.05)
        self.assertEqual(raised.exception.worker_pid, 123)
        self.assertEqual(recovery.status_for(self.state, 9)["state"], "running")

    def test_reused_directory_cannot_report_previous_success_for_new_worker(self):
        self.assertIsNone(recovery.status_for(self.state, -1))
        initial = self.script("initial.py", "pass\n")
        first = recovery.dispatch([sys.executable, str(initial)], self.state)
        self.children.append(first)
        self.wait_for(self.completed)
        self.assertEqual(recovery.status_for(self.state, first)["state"], "succeeded")
        self.assertIsNone(recovery.status_for(self.state, -1))
        release = self.root / "release"
        command = self.script("next.py", f"""import time
from pathlib import Path
deadline=time.monotonic()+5
while not Path({str(release)!r}).exists() and time.monotonic()<deadline:time.sleep(.01)
""")
        second = recovery.dispatch([sys.executable, str(command)], self.state)
        self.children.append(second)
        try:
            status = recovery.status_for(self.state, second)
            self.assertTrue(status is None or status["state"] == "running")
            self.wait_for(lambda: recovery.status_for(self.state, second) is not None)
            self.assertEqual(recovery.status_for(self.state, second)["state"], "running")
        finally:
            release.touch()
        self.wait_for(lambda: recovery.status_for(self.state, second)["state"] == "succeeded")
        self.assertIsNone(recovery.status_for(self.state, first))

    def test_spawn_failure_records_exception_type(self):
        self.children.append(recovery.dispatch([str(self.root / "missing")], self.state))
        self.wait_for(self.completed)
        result = json.loads((self.state / "status.json").read_text())
        self.assertEqual((result["state"], result["error_type"]), ("failed", "FileNotFoundError"))

    def test_worker_does_not_inherit_caller_credentials(self):
        os.environ["HYPERAG_RECOVERY_TEST_SECRET"] = "test-only"
        self.addCleanup(os.environ.pop, "HYPERAG_RECOVERY_TEST_SECRET", None)
        command = self.script("environment.py", "import os\nassert os.getenv('HYPERAG_RECOVERY_TEST_SECRET') is None\n")
        self.children.append(recovery.dispatch([sys.executable, str(command)], self.state))
        self.wait_for(self.completed)
        self.assertEqual(json.loads((self.state / "status.json").read_text())["state"], "succeeded")


if __name__ == "__main__":
    unittest.main()
