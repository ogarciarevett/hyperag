"""Hosted native guard/watchdog integration: real CLI, owned fixture, sentinel recovery."""
import importlib.util
import json
import math
import os
from pathlib import Path
import select
import signal
import statistics
import subprocess
import sys
import threading
import time
import unittest

import hooks_async as fixture_module

DIRECTORY = Path(__file__).resolve().parents[3] / 'integrations/hooks'
GUARD = DIRECTORY / 'native_guard.py'


@unittest.skipUnless(sys.platform == 'linux' and os.getenv('GITHUB_ACTIONS') == 'true', 'hosted native proof only')
class LiveHooks(unittest.TestCase):
    def setUp(self):
        # Composition avoids discovering/running the imported queue suite again.
        self.fixture = fixture_module.AsyncHooks('runTest')
        self.fixture.setUp()
        self.addCleanup(self.cleanup)
        self.fixture.environment.pop('HYPERAG_TOKEN', None)
        self.state = self.fixture.home / 'guard'
        self.restored = self.fixture.home / 'restored'
        recovery = self.fixture.home / 'restore.py'
        recovery.write_text('from pathlib import Path\nPath(' + repr(str(self.restored)) + ').touch()\n')
        self.config = self.fixture.home / 'guard.json'
        self.config.write_text(json.dumps({'binary': fixture_module.BIN, 'active_marker': str(self.fixture.marker),
            'state_dir': str(self.state), 'url': self.fixture.backend.url,
            'rollback_command': [str(Path(sys.executable).resolve()), str(recovery)],
            'native_env': {'HOME': str(self.fixture.home), 'XDG_CONFIG_HOME': str(self.fixture.home / '.config')}}))
        self.config.chmod(0o600)
        self.watch_signature = None

    def process_signature(self, pid):
        try:return (Path('/proc') / str(pid) / 'stat').read_text().rsplit(')', 1)[1].split()[19]
        except FileNotFoundError:return None

    def cleanup(self):
        self.fixture.marker.unlink(missing_ok=True)
        if self.watch_signature is not None:
            pid, signature = self.watch_signature
            if signature is not None and self.process_signature(pid) == signature:
                try:os.killpg(pid, signal.SIGTERM)
                except ProcessLookupError:pass
        self.fixture.tearDown()

    def command(self, *arguments):
        return [sys.executable, str(GUARD), '--config', str(self.config), *arguments]

    def arm(self):
        result = subprocess.run(self.command('arm'), env=self.fixture.environment,
                                text=True, capture_output=True, timeout=4)
        self.assertEqual((result.returncode, result.stderr), (0, ''))
        status = json.loads(result.stdout)
        self.assertTrue(status['ready'])
        self.watch_signature = status['pid'], self.process_signature(status['pid'])
        self.assertIsNotNone(self.watch_signature[1])
        self.assertEqual(os.getpgid(status['pid']), status['pid'])
        return status['pid']

    def call(self, event='prompt', cwd=None):
        started = time.perf_counter()
        result = subprocess.run(self.command('hook', event, '--harness', 'claude-code'),
                                env=self.fixture.environment, input=json.dumps({'cwd': str(cwd or self.fixture.project),
                                'session_id': 'fixture-session', 'prompt': 'synthetic guarded input'}),
                                text=True, capture_output=True, timeout=5)
        return result, time.perf_counter() - started

    def wait(self, predicate, timeout=5):
        self.fixture.wait_for(predicate, timeout=timeout)

    def read(self, name):
        path = self.state / name
        return json.loads(path.read_text()) if path.exists() else {}

    def recovered(self):
        self.wait(lambda: self.restored.exists())
        self.wait(lambda: self.read('recovery/status.json').get('state') == 'succeeded')
        self.assertFalse(self.fixture.marker.exists())
        self.assertTrue(self.read('trip.json').get('category'))

    def test_slow_async_write_returns_fast_with_file_credential_only(self):
        self.arm()
        result, elapsed = self.call()
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, '', ''))
        self.assertLess(elapsed, .3)
        self.assertEqual(self.fixture.backend.stored, [])
        self.wait(lambda: self.fixture.status().get('stored') == 1)
        self.fixture.backend.post_delay = .05
        for event in ('session-start', 'prompt'):
            samples = []
            for _ in range(20):
                result, elapsed = self.call(event)
                self.assertEqual((result.returncode, result.stderr), (0, ''))
                samples.append(elapsed * 1000)
            p95 = sorted(samples)[math.ceil(.95 * len(samples)) - 1]
            print(json.dumps({'fixture': 'full_native_guard_wall', 'event': event, 'calls': len(samples),
                              'p50_ms': round(statistics.median(samples), 3), 'p95_ms': round(p95, 3), 'errors': 0}), flush=True)
            self.assertLess(p95, 300)
        self.wait(lambda: self.fixture.status().get('stored') == 21)
        self.assertEqual(self.fixture.backend.scope_gets, 41)
        self.assertTrue(self.fixture.marker.exists())
        self.assertFalse((self.state / 'trip.json').exists())
        self.assertNotIn('HYPERAG_TOKEN', self.fixture.environment)

    def test_background_delivery_failure_trips_without_another_hook(self):
        self.fixture.backend.gate = threading.Event()
        self.arm()
        first, _ = self.call(); self.assertEqual(first.stderr, '')
        self.assertTrue(self.fixture.backend.started.wait(2))
        queued, _ = self.call('session-end'); self.assertEqual(queued.stderr, '')
        self.fixture.backend.tokens.clear()
        self.fixture.backend.post_delay = 0; self.fixture.backend.gate.set()
        self.recovered()
        self.assertEqual(self.fixture.status()['stored'], 1)
        self.assertEqual(self.fixture.status()['failed'], 1)
        self.assertIn('401', self.read('trip.json')['category'])
        self.assertEqual(len((self.state / 'calls.jsonl').read_text().splitlines()), 2)

    def test_dead_pending_worker_trips_without_another_hook(self):
        self.fixture.backend.gate = threading.Event()
        self.arm()
        result, _ = self.call(); self.assertEqual(result.stderr, '')
        self.assertTrue(self.fixture.backend.started.wait(2))
        pid = self.fixture.status()['pid']
        os.killpg(pid, signal.SIGKILL)
        self.recovered()
        self.assertIn('lost_queue', self.read('trip.json')['category'])
        self.assertEqual(len((self.state / 'calls.jsonl').read_text().splitlines()), 1)

    def test_watchdog_survives_hook_process_group_kill(self):
        self.fixture.backend.gate = threading.Event()
        watchdog = self.arm()
        returned = self.fixture.home / 'hook-returned'
        driver = self.fixture.home / 'driver.py'
        payload = json.dumps({'cwd': str(self.fixture.project), 'prompt': 'synthetic guarded input'})
        driver.write_text('import subprocess,time\nfrom pathlib import Path\nresult=subprocess.run(' +
            repr(self.command('hook', 'prompt', '--harness', 'claude-code')) + ',input=' + repr(payload) +
            ',text=True,capture_output=True)\nassert result.returncode==0 and not result.stderr\nPath(' +
            repr(str(returned)) + ').touch()\ntime.sleep(60)\n')
        process = subprocess.Popen([sys.executable, str(driver)], env=self.fixture.environment,
                                   start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            self.wait(returned.exists)
            self.assertTrue(self.fixture.backend.started.wait(2))
            worker = self.fixture.status()['pid']
            os.killpg(process.pid, signal.SIGKILL); process.wait(timeout=2)
            self.assertEqual(self.process_signature(watchdog), self.watch_signature[1])
            os.killpg(worker, signal.SIGKILL)
            self.recovered()
        finally:
            if process.poll() is None:os.killpg(process.pid, signal.SIGKILL); process.wait(timeout=2)

    def test_dead_armed_watchdog_fails_closed_without_restarting_or_writing(self):
        watchdog = self.arm()
        _, signature = self.watch_signature
        self.assertEqual(self.process_signature(watchdog), signature)
        self.assertEqual(os.getpgid(watchdog), watchdog)
        os.killpg(watchdog, signal.SIGKILL)
        def exited():
            try:
                fields = (Path('/proc') / str(watchdog) / 'stat').read_text().rsplit(')', 1)[1].split()
                return fields[0] == 'Z' or fields[19] != signature
            except FileNotFoundError:return True
        self.wait(exited)
        result, _ = self.call()
        self.assertEqual(result.returncode, 0)
        self.recovered()
        self.assertFalse(any(method == 'POST' for method, _ in self.fixture.backend.requests))
        self.assertEqual(self.fixture.status().get('accepted', 0), 0)
        self.assertIn(self.read('watchdog-ready.json').get('pid'), (None, watchdog))

    def test_quiet_skip_has_no_post_stderr_observation_or_trip(self):
        self.fixture.backend.grants['project:fixture'] = (True, False)
        before = dict(self.fixture.backend.grants)
        self.arm()
        for event in ('session-start', 'prompt', 'session-end'):
            result, _ = self.call(event)
            self.assertEqual((result.returncode, result.stdout, result.stderr), (0, '', ''))
        self.assertEqual(self.fixture.backend.grants, before)
        self.assertFalse(any(method == 'POST' for method, _ in self.fixture.backend.requests))
        self.assertFalse((self.state / 'trip.json').exists())
        rows = (self.state / 'skips.jsonl').read_text().splitlines()
        self.assertEqual([json.loads(row) for row in rows], [{'category': 'skipped_ungranted'}] * 3)
        calls = self.state / 'calls.jsonl'
        self.assertEqual(calls.read_text().splitlines() if calls.exists() else [], [])
        self.assertTrue(self.fixture.marker.exists())

    def report_call(self):
        reader, writer = os.pipe()
        environment = dict(self.fixture.environment, HYPERAG_HOOK_REPORT_FD=str(writer))
        payload = json.dumps({'cwd': str(self.fixture.project), 'session_id': 'private-session-fixture',
                              'prompt': 'private-prompt-fixture'})
        try:
            result = subprocess.run([fixture_module.BIN, 'hook', 'prompt', '--harness', 'claude-code'],
                                    env=environment, input=payload, text=True, capture_output=True,
                                    pass_fds=(writer,), timeout=4)
            os.close(writer); writer = None
            raw = b''; deadline = time.monotonic() + .3
            while True:
                left = deadline - time.monotonic()
                self.assertGreater(left, 0, 'report pipe inherited by detached worker')
                self.assertTrue(select.select([reader], [], [], left)[0], 'report pipe has no prompt EOF')
                value = os.read(reader, 4096)
                if not value:break
                raw += value
            self.assertEqual((result.returncode, result.stdout, result.stderr), (0, '', ''))
            self.assertNotIn(self.fixture.token.encode(), raw)
            self.assertNotIn(b'private-session-fixture', raw); self.assertNotIn(b'private-prompt-fixture', raw)
            frame = json.loads(raw)
            self.assertEqual(set(frame), {'category', 'scope'})
            self.assertEqual(frame['scope'], 'project:fixture')
            return frame
        finally:
            os.close(reader)
            if writer is not None:os.close(writer)

    def test_native_report_eof_is_prompt_while_worker_remains_alive(self):
        self.fixture.backend.gate = threading.Event()
        frame = self.report_call()
        self.assertEqual(frame['category'], 'enqueued')
        self.assertTrue(self.fixture.native_worker(self.fixture.status()['pid']))

    def test_native_cancelled_report_counts_drop_without_delivery(self):
        self.fixture.marker.unlink()
        frame = self.report_call()
        self.assertEqual(frame['category'], 'cancelled')
        ledger = json.loads((self.fixture.queue() / 'enqueue-errors.json').read_text())
        self.assertEqual(ledger['categories']['cancelled'], 1)
        self.assertFalse(any(method == 'POST' for method, _ in self.fixture.backend.requests))

    def test_policy_warmup_thresholds_errors_and_global_quantile(self):
        spec = importlib.util.spec_from_file_location('native_guard_policy', GUARD)
        module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
        row = lambda latency, warmup=False, error=None: {'latency_ms': latency, 'warmup': warmup, 'error': error}
        self.assertIsNone(module.evaluate([row(999, True)])['reason'])
        self.assertTrue(module.evaluate([row(1000.001, True)])['reason'])
        self.assertTrue(module.evaluate([row(1, True, 'cancelled')])['reason'])
        self.assertTrue(module.evaluate([row(1, True, 'timeout')])['reason'])
        self.assertIsNone(module.evaluate([row(1000, True)] + [row(300) for _ in range(20)])['reason'])
        mixed = [row(350, True)] + [dict(row(350), cohort='read') for _ in range(9)] + [dict(row(1), cohort='write') for _ in range(10)]
        self.assertIsNone(module.evaluate(mixed)['reason'])
        result = module.evaluate(mixed + [dict(row(1), cohort='write')])
        self.assertEqual((result['calls'], result['warmup_calls'], result['errors'], result['p95_ms']), (20, 1, 0, 350))
        self.assertTrue(result['reason'])
        across_both_cohorts = module.evaluate(mixed + [dict(row(1), cohort='write') for _ in range(81)])
        self.assertEqual((across_both_cohorts['calls'], across_both_cohorts['p95_ms']), (100, 350))
        self.assertTrue(across_both_cohorts['reason'])

    def test_first_slow_warmup_call_still_trips_hard_budget(self):
        self.fixture.backend.recent_delay = 1.05
        self.arm()
        result, elapsed = self.call('session-start')
        self.assertEqual(result.returncode, 0)
        self.assertGreater(elapsed, 1)
        self.recovered()
        self.assertEqual(self.read('summary.json')['warmup_calls'], 1)


if __name__ == '__main__':
    unittest.main()
