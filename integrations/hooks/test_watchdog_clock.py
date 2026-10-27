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
    def test_delivery_failure_precedes_simultaneous_mode_mismatch(self):
        for counters in ({'categories': {'http401': 1}}, {'failed': 1}, {'blocked': True},
                         {'rejected': 1}, {'enqueue_errors': {'rejected': 1}},
                         {'enqueue_errors': {'categories': {'queue_full': 1}}}):
            with self.subTest(counters=counters):
                response = {'async_writes': False, 'token_file': False, **counters}
                with patch.object(guard, '_native', return_value=(0, json.dumps(response).encode(), False, b'')), \
                        self.assertRaises(guard.WriterFailure):
                    guard._status({'active_marker': '/fixture-marker'})
        with patch.object(guard, '_native', return_value=(0, b'{"async_writes":false}', False, b'')), \
                self.assertRaisesRegex(ValueError, 'writer_mode_mismatch') as error:
            guard._status({'active_marker': '/fixture-marker'})
        self.assertNotIsInstance(error.exception, guard.WriterFailure)

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

    def test_trip_persists_first_trace_across_recovery_retry_without_payload(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory);marker = root / 'active';marker.touch()
            config = {'state_dir': str(root), 'active_marker': str(marker), 'rollback_command': ['/fixture']}
            trace = guard._trace('watchdog', 'hook-writer-status', child_pid=123, agent='codex', event='prompt',
                                 _duration_ms=250.123, payload='private-fixture-input')
            with patch.object(guard._recovery, 'dispatch_confirmed', side_effect=[TimeoutError(), 456]):
                guard._trip(config, 'TimeoutError', trace)
                guard._trip(config, 'recovery_pending', guard._trace('watchdog', 'retry'))
            raw = (root / 'trip.json').read_text();value = json.loads(raw)
            self.assertEqual((value['stage'], value['operation'], value['child_pid'], value['duration_ms']),
                             ('watchdog', 'hook-writer-status', 123, 250.123))
            self.assertEqual((value['agent'], value['event'], value['category']), ('codex', 'prompt', 'TimeoutError'))
            self.assertNotIn('private-fixture-input', raw)
            self.assertNotIn('_started', raw)
            self.assertFalse(marker.exists())

    def test_non_child_trip_has_explicit_null_pid_and_elapsed_duration(self):
        trace = guard._trace('watchdog', 'heartbeat')
        value = guard._snapshot(trace)
        self.assertIsNone(value['child_pid'])
        self.assertGreaterEqual(value['duration_ms'], 0)
        self.assertIn('started_at', value)

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
