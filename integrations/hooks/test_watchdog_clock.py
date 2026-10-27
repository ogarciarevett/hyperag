#!/usr/bin/env python3
"""Clock and marker completion races; all process probes are mocked."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('native_guard', Path(__file__).with_name('native_guard.py'))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


class WatchdogClockTest(unittest.TestCase):
    def test_shared_clock_uses_raw_clock_not_process_relative_monotonic(self):
        with patch.object(guard.time, 'clock_gettime', return_value=123) as clock, \
                patch.object(guard.time, 'monotonic', side_effect=AssertionError('process-relative clock')):
            self.assertEqual(guard._shared_time(), 123)
        clock.assert_called_once_with(guard.time.CLOCK_MONOTONIC_RAW)

    def test_only_a_live_call_with_a_remaining_marker_can_time_out(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'inflight-123.json'
            for remove, dead, reason in [(False, False, 'foreground_timeout'),
                                         (True, False, None), (True, True, None),
                                         (False, True, 'foreground_lost')]:
                with self.subTest(remove=remove, dead=dead):
                    path.write_text(json.dumps({'pid': 123, 'started': 1}))
                    def probe(pid, signal):
                        self.assertEqual((pid, signal), (123, 0))
                        if remove:path.unlink()
                        if dead:raise ProcessLookupError()
                    with patch.object(guard, '_shared_time', return_value=3), patch.object(guard.os, 'kill', side_effect=probe):
                        if reason:
                            with self.assertRaisesRegex(ValueError, reason):guard._check_inflight(path)
                        else:guard._check_inflight(path)

    def test_invalid_shared_age_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'inflight-123.json'
            path.write_text(json.dumps({'pid': 123, 'started': 1}))
            for value in (0, float('nan'), float('inf')):
                with self.subTest(value=value), patch.object(guard, '_shared_time', return_value=value), \
                        patch.object(guard.os, 'kill'), self.assertRaisesRegex(ValueError, 'invalid_foreground_clock'):
                    guard._check_inflight(path)

    def test_fast_call_and_missing_marker_do_not_trip(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'inflight-123.json'
            with patch.object(guard.os, 'kill') as probe:
                guard._check_inflight(path)
                probe.assert_not_called()
                path.write_text(json.dumps({'pid': 123, 'started': 1}))
                with patch.object(guard, '_shared_time', return_value=1.059):guard._check_inflight(path)
                probe.assert_called_once_with(123, 0)


if __name__ == '__main__':
    unittest.main()
