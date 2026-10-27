#!/usr/bin/env python3
"""Mocked platform metadata and interrupted-recovery checks; no services are run."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("rollback", Path(__file__).with_name("rollback.py"))
rollback = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rollback)


class RollbackTest(unittest.TestCase):
    def test_macos_identity_uses_named_metadata_only(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "prior-service";binary.write_bytes(b"fixture executable")
            values = {"stat=": "S", "lstart=": "Tue Oct 27 00:00:00 2026", "comm=": str(binary)}
            def ps(command, **kwargs):
                self.assertEqual(command[:-1], ["/bin/ps", "-p", "123", "-o"])
                return subprocess.CompletedProcess(command, 0, stdout=values[command[-1]])
            with patch.object(rollback.sys, "platform", "darwin"), patch.object(rollback.subprocess, "run", side_effect=ps):
                result = rollback.identity(123)
            self.assertEqual(result, {"pid": 123, "binary": str(binary.resolve()),
                                     "sha256": rollback.binary_hash(binary), "start_ticks": "darwin:" + values["lstart="]})

    def test_macos_zombie_and_absent_process_are_not_running(self):
        for code, value in [(0, "Z"), (1, "")]:
            with self.subTest(code=code), patch.object(rollback.sys, "platform", "darwin"), \
                    patch.object(rollback.subprocess, "run", return_value=subprocess.CompletedProcess([], code, stdout=value)):
                self.assertIsNone(rollback.identity(123))

    def test_macos_exit_between_state_and_executable_is_not_a_live_identity(self):
        replies = iter([('stat=', 'S'), ('lstart=', 'start-a'), ('comm=', '<defunct>'), ('stat=', 'Z')])
        def ps(command, **kwargs):
            field, value = next(replies)
            self.assertEqual(command[-1], field)
            return subprocess.CompletedProcess(command, 0, stdout=value)
        with patch.object(rollback.sys, 'platform', 'darwin'), patch.object(rollback.subprocess, 'run', side_effect=ps):
            self.assertIsNone(rollback.identity(123))

    def test_macos_live_invalid_executable_and_reused_pid_fail_closed(self):
        for final_start in ('start-a', 'start-b'):
            with self.subTest(final_start=final_start):
                values = iter([('stat=', 'S'), ('lstart=', 'start-a'), ('comm=', '<defunct>'),
                               ('stat=', 'S'), ('lstart=', final_start)])
                def ps(command, **kwargs):
                    field, value = next(values)
                    self.assertEqual(command[-1], field)
                    return subprocess.CompletedProcess(command, 0, stdout=value)
                with patch.object(rollback.sys, 'platform', 'darwin'), patch.object(rollback.subprocess, 'run', side_effect=ps):
                    message = 'absolute process executable required' if final_start == 'start-a' else 'process start marker changed'
                    with self.assertRaisesRegex(ValueError, message):
                        rollback.identity(123)

    def test_failure_status_keeps_phase_without_exception_text(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def fail(_):
                rollback.write_status(root, {'state': 'stopping'})
                raise ValueError('synthetic-private-diagnostic')
            with patch.object(rollback, 'restore', side_effect=fail):
                self.assertEqual(rollback.main([str(root / 'config.json')]), 1)
            raw = (root / 'rollback-status.json').read_text()
            status = json.loads(raw)
            self.assertEqual((status['state'], status['phase'], status['error_type']), ('failed', 'stopping', 'ValueError'))
            self.assertNotIn('synthetic-private-diagnostic', raw)

    def test_unknown_platform_fails_closed(self):
        with patch.object(rollback.sys, "platform", "unsupported"), self.assertRaises(NotImplementedError):
            rollback.identity(123)

    def test_retry_after_current_exited_restores_without_signalling_another_process(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory);binary = root / "old";binary.write_bytes(b"fixture executable")
            old = {"pid": 456, "binary": str(binary.resolve()), "sha256": rollback.binary_hash(binary), "start_ticks": "old"}
            config = {"current": {"pid": 123}, "restore": {"argv": [str(binary)], "sha256": old["sha256"], "env": {}},
                      "health_url": "http://127.0.0.1:12345/healthz"}
            path = root / "config.json";path.write_text(json.dumps(config));path.chmod(0o600)
            service = Mock(pid=456);service.poll.return_value = None
            with patch.object(rollback, "identity", side_effect=[None, None, old]), \
                    patch.object(rollback, "healthy", side_effect=[False, True]), \
                    patch.object(rollback.os, "kill") as kill, patch.object(rollback.subprocess, "Popen", return_value=service):
                rollback.restore(path)
            kill.assert_not_called()
            self.assertEqual(json.loads((root / "rollback-status.json").read_text())["state"], "succeeded")
            with patch.object(rollback, "identity", return_value=None), patch.object(rollback, "healthy", return_value=True), \
                    patch.object(rollback.os, "kill") as kill, patch.object(rollback.subprocess, "Popen") as spawn:
                with self.assertRaises(ValueError):
                    rollback.restore(path)
                kill.assert_not_called();spawn.assert_not_called()


if __name__ == "__main__":
    unittest.main()
