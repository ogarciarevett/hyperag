"""Forward phase persistence; hosted CI only, with no service needed for this case."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("forward_switch", Path(__file__).with_name("switch.py"))
switch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(switch)


class SwitchTest(unittest.TestCase):
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
