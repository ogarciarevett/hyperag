"""Forward phase persistence; hosted CI only, with no service needed for this case."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("forward_switch", Path(__file__).with_name("switch.py"))
switch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(switch)


class SwitchTest(unittest.TestCase):
    def config(self, root):
        old, new = root / 'old', root / 'new'
        old.write_bytes(b'old fixture');new.write_bytes(b'new fixture')
        current = {'pid': 111, 'binary': str(old), 'sha256': switch.rollback.binary_hash(old), 'start_ticks': 'prior'}
        target = {'pid': 222, 'binary': str(new), 'sha256': switch.rollback.binary_hash(new), 'start_ticks': 'target'}
        value = {'current': current, 'restore': {'argv': [str(old)], 'sha256': current['sha256'], 'env': {}},
                 'target': {'argv': [str(new)], 'sha256': target['sha256'], 'env': {}},
                 'health_url': 'http://127.0.0.1:12345/healthz'}
        path = root / 'switch.json';path.write_text(json.dumps(value));path.chmod(0o600)
        return path, current, target

    def test_reporting_failure_after_stop_still_restores_prior(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary);path, current, _target = self.config(root)
            original = switch.rollback.write_status
            def write(directory, value, name='rollback-status.json'):
                if name == 'switch-status.json' and value['phase'] == 'starting':
                    raise OSError('private-diagnostic')
                original(directory, value, name)
            with patch.object(switch.rollback, 'write_status', side_effect=write), \
                    patch.object(switch.rollback, 'identity', side_effect=[current, None]), \
                    patch.object(switch.rollback, 'restore') as restore, patch.object(switch.os, 'kill'):
                with self.assertRaises(OSError):switch.forward(path)
            restore.assert_called_once_with(root / 'rollback.json')
            self.assertNotIn('private-diagnostic', (root / 'switch-status.json').read_text())

    def test_target_exit_during_health_cannot_report_success(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary);path, current, target = self.config(root)
            process = Mock(pid=target['pid'])
            process.poll.side_effect = [None, 0, 0]
            with patch.object(switch.rollback, 'identity', side_effect=[current, None, target]), \
                    patch.object(switch.rollback, 'healthy', return_value=True), \
                    patch.object(switch.rollback, 'restore') as restore, patch.object(switch.os, 'kill'), \
                    patch.object(switch.subprocess, 'Popen', return_value=process):
                with self.assertRaises(RuntimeError):switch.forward(path)
            restore.assert_called_once_with(root / 'rollback.json')
            value = json.loads((root / 'switch-status.json').read_text())
            self.assertEqual((value['state'], value['phase'], value['category']), ('failed', 'health', 'RuntimeError'))

    def test_failure_preserves_phase_category_without_diagnostic_values(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with patch.object(switch.rollback, "read_config", side_effect=ValueError("private-diagnostic")), \
                    patch.object(switch.os, "kill") as kill, patch.object(switch.subprocess, "Popen") as start:
                with self.assertRaises(ValueError):
                    switch.forward(root / "forward.json")
            value = json.loads((root / "switch-status.json").read_text())
            self.assertEqual((value["state"], value["phase"], value["category"]), ("failed", "checking", "ValueError"))
            self.assertEqual([item["category"] for item in value["history"]], ["validate_configuration", "ValueError"])
            self.assertNotIn("private-diagnostic", (root / "switch-status.json").read_text())
            self.assertEqual((root / "switch-status.json").stat().st_mode & 0o777, 0o600)
            kill.assert_not_called()
            start.assert_not_called()


if __name__ == "__main__":
    unittest.main()
