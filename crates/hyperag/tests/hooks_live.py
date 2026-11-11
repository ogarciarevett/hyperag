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
from unittest.mock import patch

import hooks_async as fixture_module

DIRECTORY = Path(__file__).resolve().parents[3] / 'integrations/hooks'
GUARD = Path(os.getenv('HYPERAG_HOOK_GUARD', str(DIRECTORY / 'native_guard.py'))).resolve()


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

    def arm_diagnostics(self):
        diagnostics = {'marker_present': self.fixture.marker.exists()}
        for name, fields in [('trip.json', ('category', 'stage', 'operation', 'child_pid', 'duration_ms', 'recovery_pid', 'pending_recovery_pid', 'dispatch_error')),
                             ('recovery/status.json', ('state', 'pid', 'exit_code', 'error_type', 'phase')),
                             ('watchdog-ready.json', ('ready', 'pid', 'heartbeat'))]:
            try:
                value = self.read(name)
                diagnostics[name] = {field: value[field] for field in fields if field in value}
                if name == 'watchdog-ready.json':
                    diagnostics[name].update(has_fingerprint=bool(value.get('fingerprint')), has_nonce=bool(value.get('nonce')))
            except Exception as error:diagnostics[name] = {'metadata_error': type(error).__name__}
        return diagnostics

    def call_diagnostics(self):
        value = self.arm_diagnostics()
        try:
            rows = (self.state / 'calls.jsonl').read_text().splitlines()
            last = json.loads(rows[-1]) if rows else {}
            value['last_call'] = {key: last[key] for key in ('agent', 'event', 'cohort', 'error', 'latency_ms', 'tail_margin_ms', 'warmup') if key in last}
            worker = self.fixture.status()
            value['writer'] = {key: worker[key] for key in ('accepted', 'stored', 'failed', 'rejected', 'blocked', 'last_category', 'categories') if key in worker}
            ledger = self.fixture.queue() / 'enqueue-errors.json'
            if ledger.exists():
                errors = json.loads(ledger.read_text())
                value['enqueue_categories'] = errors.get('categories', {})
        except Exception as error:
            value['call_metadata_error'] = type(error).__name__
        return value

    def arm(self):
        result = subprocess.run(self.command('arm'), env=self.fixture.environment,
                                text=True, capture_output=True, timeout=4)
        diagnostic = json.dumps(self.arm_diagnostics()) if result.returncode or result.stderr else ''
        self.assertEqual((result.returncode, bool(result.stderr)), (0, False), diagnostic)
        status = json.loads(result.stdout)
        self.assertTrue(status['ready'])
        self.watch_signature = status['pid'], self.process_signature(status['pid'])
        self.assertIsNotNone(self.watch_signature[1])
        self.assertEqual(os.getpgid(status['pid']), status['pid'])
        return status['pid']

    def call(self, event='prompt', cwd=None):
        payload = json.dumps({'cwd': str(cwd or self.fixture.project),
                              'session_id': 'fixture-session', 'prompt': 'synthetic guarded input'})
        environment = dict(self.fixture.environment)
        started = time.perf_counter()
        environment['HYPERAG_HOOK_STARTED_AT'] = str(time.time())
        result = subprocess.run(self.command('hook', event, '--harness', 'claude-code'),
                                env=environment, input=payload, text=True, capture_output=True, timeout=5)
        elapsed = time.perf_counter() - started
        if result.returncode or result.stderr:
            print(json.dumps({'fixture': 'guard_failure', 'test': self._testMethodName,
                              'returncode': result.returncode, 'stderr_present': bool(result.stderr),
                              'metadata': self.call_diagnostics()}), flush=True)
        return result, elapsed

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

    def test_prompt_budget_skip_is_quiet_measured_and_does_no_write(self):
        self.fixture.backend.scope_delay = .7
        self.arm()
        result, elapsed = self.call()
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, '', ''), json.dumps(self.call_diagnostics()))
        row = json.loads((self.state / 'calls.jsonl').read_text().splitlines()[-1])
        self.assertEqual(row['category'], 'budget_skip')
        self.assertIsNone(row['error'])
        self.assertLessEqual(row['latency_ms'], 250)
        self.assertGreaterEqual(row['wall_ms'], row['latency_ms'])
        self.assertLess(elapsed, .7, 'prompt waited for the slow scopes GET')
        self.wait(lambda: self.read('summary.json').get('budget_skips') == 1)
        self.assertFalse(self.read('trip.json'))
        self.assertTrue(self.fixture.marker.exists())
        self.assertEqual(json.loads((self.state / 'skips.jsonl').read_text().splitlines()[-1]), {'category': 'budget_skip'})
        self.assertFalse(any(method == 'POST' for method, _ in self.fixture.backend.requests))

    def test_already_spent_prompt_budget_does_not_launch_native_or_write(self):
        self.arm()
        environment = dict(self.fixture.environment, HYPERAG_HOOK_STARTED_AT=str(time.time() - .4))
        result = subprocess.run(self.command('hook', 'prompt', '--harness', 'claude-code'),
                                env=environment, input='{}', text=True, capture_output=True, timeout=4)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, '', ''))
        row = json.loads((self.state / 'calls.jsonl').read_text().splitlines()[-1])
        self.assertEqual(row['category'], 'budget_skip')
        self.assertEqual(row['latency_ms'], 250)
        self.assertIsNone(row['native_ms'])
        self.assertFalse(self.fixture.backend.requests)
        self.assertFalse(self.read('trip.json'))

    def test_native_prompt_budget_reports_the_slow_stage_without_stderr(self):
        self.fixture.backend.scope_delay = .7
        self.fixture.environment['HYPERAG_HOOK_STARTED_AT'] = str(time.time())
        frame = self.report_call()
        self.assertEqual(frame['category'], 'budget_skip')
        self.assertEqual(frame['stage'], 'scopes_get')
        self.assertGreater(frame['steps_ms']['scopes_get'], 50)
        self.assertFalse(any(method == 'POST' for method, _ in self.fixture.backend.requests))

    def test_stderr_before_prompt_budget_still_raises(self):
        spec = importlib.util.spec_from_file_location('budget_guard', GUARD)
        module = importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        config = {'binary': sys.executable, 'url': self.fixture.backend.url, 'native_env': {'HOME': str(self.fixture.home)}}
        with self.assertRaises(TimeoutError):
            module._native(config, ['-c', 'import os,time;os.write(2,b"error");time.sleep(5)'], budget=.2, prompt_budget=True)

    def test_prompt_breakdown_has_no_search_or_recent_and_writer_is_detached(self):
        self.arm()
        result, _ = self.call()
        self.assertEqual((result.returncode, result.stderr), (0, ''), json.dumps(self.call_diagnostics()))
        row = json.loads((self.state / 'calls.jsonl').read_text().splitlines()[-1])
        self.assertEqual(row['category'], 'enqueued')
        for step in ('process_start', 'scopes_get', 'writer_enqueue', 'native_total'):
            self.assertGreaterEqual(row['steps_ms'][step], 0)
        self.assertNotIn('recent_get', row['steps_ms'])
        self.assertEqual(len(row['machine_load']), 3)
        self.wait(lambda: self.fixture.status().get('stored') == 1)
        status = self.fixture.status()
        self.assertEqual(status['delivery_count'], 1)
        self.assertGreaterEqual(status['delivery_last_ms'], 350)
        self.assertEqual(status['delivery_total_ms'], status['delivery_last_ms'])
        self.assertEqual(set(self.fixture.backend.requests), {('GET', '/v1/scopes'), ('POST', '/v1/memories')})

    def test_slow_async_write_returns_fast_with_file_credential_only(self):
        self.arm()
        result, elapsed = self.call()
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, '', ''))
        self.assertLess(elapsed, .3)
        self.assertEqual(self.fixture.backend.stored, [])
        self.wait(lambda: self.fixture.status().get('stored') == 1)
        self.fixture.backend.post_delay = .01
        spec = importlib.util.spec_from_file_location('tail_calibration', DIRECTORY / 'tail_calibration.py')
        calibration = importlib.util.module_from_spec(spec);spec.loader.exec_module(calibration)
        tails, all_samples, all_adjusted = [], [], []
        for event in ('session-start', 'prompt'):
            samples, adjusted, event_tails = [], [], []
            for index in range(300):
                result, elapsed = self.call(event)
                diagnostic = json.dumps(self.call_diagnostics()) if result.returncode or result.stderr else ''
                self.assertEqual((result.returncode, bool(result.stderr)), (0, False), diagnostic)
                samples.append(elapsed * 1000)
                row = json.loads((self.state / 'calls.jsonl').read_text().splitlines()[-1])
                margin = row['tail_margin_ms']
                tail = elapsed * 1000 - (row['latency_ms'] - margin)
                tails.append(tail);event_tails.append(tail)
                adjusted.append(row['latency_ms'])
                if event == 'prompt':
                    self.wait(lambda: self.fixture.status().get('stored') == index + 2)
            p95 = sorted(samples)[math.ceil(.95 * len(samples)) - 1]
            print(json.dumps({'fixture': 'full_native_guard_wall', 'event': event, 'calls': len(samples),
                              'p50_ms': round(statistics.median(samples), 3), 'p95_ms': round(p95, 3), 'errors': 0}), flush=True)
            calibration.check(event_tails, samples, adjusted, margin)
            self.assertLess(calibration.percentile(adjusted, .95), 300)
            self.assertLess(p95, 300)
            all_samples.extend(samples);all_adjusted.extend(adjusted)
        print(json.dumps({'fixture':'statistical_tail_calibration', **calibration.check(tails, all_samples, all_adjusted, margin)}), flush=True)
        tail_p99 = sorted(tails)[math.ceil(.99 * len(tails)) - 1]
        print(json.dumps({'fixture': 'guard_exit_tail', 'calls': len(tails), 'p99_ms': round(tail_p99, 3),
                          'max_ms': round(max(tails), 3), 'fixed_margin_ms': margin}), flush=True)
        self.assertLessEqual(tail_p99, margin)
        self.wait(lambda: self.fixture.status().get('stored') == 301)
        self.assertEqual(self.fixture.backend.scope_gets, 601)
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
        driver.write_text('import os,subprocess,time\nfrom pathlib import Path\nos.environ["HYPERAG_HOOK_STARTED_AT"]=str(time.time())\nresult=subprocess.run(' +
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

    def test_confirmed_recovery_survives_guard_kill_after_marker_unlink(self):
        watchdog = self.arm()
        checkpoint = self.fixture.home / 'marker-removed'
        driver = self.fixture.home / 'trip-window.py'
        driver.write_text('import importlib.util,time\nfrom pathlib import Path\n' +
            'spec=importlib.util.spec_from_file_location("native_guard",' + repr(str(GUARD)) + ')\n' +
            'module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)\n' +
            'original=Path.unlink\n' +
            'def unlink(path,*args,**kwargs):\n    result=original(path,*args,**kwargs)\n    if path==Path(' +
            repr(str(self.fixture.marker)) + '):\n        Path(' + repr(str(checkpoint)) + ').touch()\n        time.sleep(60)\n    return result\n' +
            'Path.unlink=unlink\nraise SystemExit(module.main(' +
            repr(['--config', str(self.config), 'hook', 'prompt', '--harness', 'claude-code']) + '))\n')
        environment = dict(self.fixture.environment)
        environment['HYPERAG_HOOK_STARTED_AT'] = str(time.time())
        process = subprocess.Popen([sys.executable, str(driver)], env=environment, start_new_session=True,
                                   stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            process.stdin.write(b'{broken'); process.stdin.close()
            self.wait(checkpoint.exists)
            signature = self.process_signature(watchdog)
            if signature is not None:
                self.assertEqual(signature, self.watch_signature[1])
                try:os.killpg(watchdog, signal.SIGKILL)
                except ProcessLookupError:pass
            os.killpg(process.pid, signal.SIGKILL); process.wait(timeout=2)
            self.recovered()
            self.assertFalse(any(method == 'POST' for method, _ in self.fixture.backend.requests))
        finally:
            if process.poll() is None:os.killpg(process.pid, signal.SIGKILL); process.wait(timeout=2)

    def test_bootstrap_retries_watchdog_lock_collision_with_parent_probe(self):
        child = self.fixture.home / 'collision-watchdog.py'
        started = self.fixture.home / 'child-started'
        release = self.fixture.home / 'child-enter-watchdog'
        collided = self.fixture.home / 'child-collided'
        arguments = ['--config', str(self.config), 'watchdog']
        child.write_text('MODULE=' + repr(str(GUARD)) + '\nARGUMENTS=' + repr(arguments) +
            '\nSTARTED=' + repr(str(started)) + '\nRELEASE=' + repr(str(release)) +
            '\nCOLLIDED=' + repr(str(collided)) + '\n' + r'''import fcntl,importlib.util,os,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('native_guard',MODULE)
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
Path(STARTED).write_text(str(os.getpid()))
deadline=time.monotonic()+2
while not Path(RELEASE).exists():
    if time.monotonic()>deadline:raise TimeoutError('parent probe gate')
    time.sleep(.001)
original=fcntl.flock
def flock(file,operation):
    try:return original(file,operation)
    except BlockingIOError:
        Path(COLLIDED).touch()
        raise
fcntl.flock=flock
raise SystemExit(module.main(ARGUMENTS))
''')
        driver = self.fixture.home / 'collision-arm.py'
        driver.write_text('MODULE=' + repr(str(GUARD)) + '\nCONFIG=' + repr(str(self.config)) +
            '\nCHILD=' + repr(str(child)) + '\nSTARTED=' + repr(str(started)) +
            '\nRELEASE=' + repr(str(release)) + '\nCOLLIDED=' + repr(str(collided)) + '\n' + r'''import importlib.util,subprocess,sys,time
from contextlib import contextmanager
from pathlib import Path
spec=importlib.util.spec_from_file_location('native_guard',MODULE)
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
original_spawn=subprocess.Popen
original_lock=module._lock
spawned=held=False
def spawn(command,*args,**kwargs):
    global spawned
    if command[-1]=='watchdog':
        result=original_spawn([sys.executable,CHILD],*args,**kwargs)
        spawned=True
        return result
    return original_spawn(command,*args,**kwargs)
def wait(path):
    deadline=time.monotonic()+.5
    while not Path(path).exists():
        if time.monotonic()>deadline:raise TimeoutError('collision checkpoint')
        time.sleep(.001)
@contextmanager
def locked(path):
    global held
    with original_lock(path) as file:
        try:yield file
        finally:
            if path.name=='watchdog.lock' and spawned and not held:
                held=True
                wait(STARTED)
                Path(RELEASE).touch()
                wait(COLLIDED)
                time.sleep(.1)
subprocess.Popen=spawn
module._lock=locked
raise SystemExit(module.main(['--config',CONFIG,'arm']))
''')
        result = subprocess.run([sys.executable, str(driver)], env=self.fixture.environment,
                                text=True, capture_output=True, timeout=5)
        if started.exists():
            pid = int(started.read_text()); self.watch_signature = pid, self.process_signature(pid)
        self.assertEqual((result.returncode, bool(result.stderr)), (0, False), json.dumps(self.arm_diagnostics()))
        ready = json.loads(result.stdout)
        self.assertTrue(collided.exists())
        self.assertTrue(ready['ready'])
        self.assertEqual(ready['pid'], self.watch_signature[0])
        self.assertTrue(self.fixture.marker.exists())
        self.assertFalse((self.state / 'trip.json').exists())

    def test_fast_call_with_separate_process_monotonic_origins_never_trips(self):
        driver = self.fixture.home / 'separate-clock-guard.py'
        observed = self.fixture.home / 'watchdog-observed-call.json'
        driver.write_text('MODULE=' + repr(str(GUARD)) + '\nOBSERVED=' + repr(str(observed)) + '\n' + r'''import importlib.util,json,os,sys,time
from pathlib import Path
original_clock=time.monotonic
time.monotonic=lambda:original_clock()+(60000 if 'watchdog' in sys.argv else -60000)
spec=importlib.util.spec_from_file_location('native_guard',MODULE)
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
# Bootstrap must spawn this instrumented watchdog as an independent process.
module.__file__=str(Path(__file__).resolve())
original_check=module._check_inflight
original_write=module._write
def check(path, trace=None):
    original_check(path, trace)
    try:value=json.loads(path.read_text())
    except FileNotFoundError:return
    value.update(watchdog_pid=os.getpid(),age=module._shared_time()-value['started'],
                 different_origin=abs(time.monotonic()-module._shared_time())>1000)
    Path(OBSERVED).write_text(json.dumps(value))
def write(path,value,*args,**kwargs):
    original_write(path,value,*args,**kwargs)
    if path.name.startswith('inflight-'):
        deadline=module._shared_time()+.5
        while not Path(OBSERVED).exists():
            if (path.parent/'trip.json').exists():break
            if module._shared_time()>deadline:raise TimeoutError('watchdog observation gate')
            time.sleep(.001)
module._check_inflight=check
module._write=write
raise SystemExit(module.main())
''')
        command = lambda *args: [sys.executable, str(driver), '--config', str(self.config), *args]
        latencies = []
        with patch.object(self, 'command', side_effect=command):
            watchdog = self.arm()
            for _ in range(5):
                observed.unlink(missing_ok=True)
                result, elapsed = self.call('session-start')
                self.assertEqual((result.returncode, result.stderr), (0, ''), json.dumps(self.call_diagnostics()))
                self.assertLess(elapsed, 1)
                self.assertTrue(observed.exists(), json.dumps(self.call_diagnostics()))
                value = json.loads(observed.read_text())
                self.assertEqual(value['watchdog_pid'], watchdog)
                self.assertNotEqual(value['pid'], watchdog)
                self.assertTrue(value['different_origin'])
                self.assertTrue(0 <= value['age'] < 1)
                self.assertLess(abs(time.time()-value['started_at']), 1)
                latencies.append(elapsed*1000)
        time.sleep(.2)
        self.assertTrue(self.fixture.marker.exists(), json.dumps(self.call_diagnostics()))
        self.assertFalse((self.state / 'trip.json').exists())
        self.assertFalse(list(self.state.glob('inflight-*.json')))
        print(json.dumps({'fixture':'different_process_clock_origins', 'calls':5,
                          'max_full_wall_ms':round(max(latencies),3), 'trips':0}), flush=True)

    def probe_driver(self, failed_calls):
        driver = self.fixture.home / 'probe-guard.py'
        driver.write_text('MODULE=' + repr(str(GUARD)) + '\nFAILED=' + repr(failed_calls) + '\n' + r'''import importlib.util,subprocess,sys
from pathlib import Path
spec=importlib.util.spec_from_file_location('native_guard',MODULE)
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
module.__file__=str(Path(__file__).resolve())
original=subprocess.Popen
attempts=0
def spawn(command,*args,**kwargs):
    global attempts
    if command[1:]==['hook-writer-status']:
        attempts+=1
        if attempts in FAILED:
            command=[sys.executable,'-c','import time;time.sleep(60)']
    return original(command,*args,**kwargs)
subprocess.Popen=spawn
raise SystemExit(module.main())
''')
        return lambda *args: [sys.executable, str(driver), '--config', str(self.config), *args]

    def probe_failures(self):
        path = self.state / 'probe-failures.jsonl'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def test_isolated_probe_timeouts_reset_and_do_not_block_fast_hooks(self):
        with patch.object(self, 'command', side_effect=self.probe_driver((1, 3))):
            self.arm()
            time.sleep(.85)  # Longer than heartbeat freshness; the probe is still running.
            result, elapsed = self.call('session-start')
            self.assertEqual((result.returncode, result.stderr), (0, ''), json.dumps(self.call_diagnostics()))
            self.assertLess(elapsed, .3)
            self.wait(lambda: len(self.probe_failures()) == 2, timeout=8)
            self.wait(lambda: self.read('watchdog-ready.json').get('probe_failures') == 0)
        failures = self.probe_failures()
        self.assertEqual([value['consecutive_failures'] for value in failures], [1, 1])
        for value in failures:
            self.assertEqual((value['stage'], value['operation'], value['category']),
                             ('background_probe', 'hook-writer-status', 'TimeoutError'))
            self.assertGreater(value['child_pid'], 0)
            self.assertGreaterEqual(value['duration_ms'], 2000)
        self.assertTrue(self.fixture.marker.exists())
        self.assertFalse((self.state / 'trip.json').exists())

    def test_mode_mismatch_cannot_delay_authoritative_delivery_failure(self):
        driver = self.fixture.home / 'mismatch-failure-guard.py'
        fault = self.fixture.home / 'mixed-status'
        response = json.dumps({'async_writes': False, 'token_file': False,
                               'failed': 1, 'categories': {'http401': 1}})
        driver.write_text('MODULE=' + repr(str(GUARD)) + '\nFAULT=' + repr(str(fault)) +
            '\nRESPONSE=' + repr(response) + '\n' + r'''import importlib.util,subprocess,sys
from pathlib import Path
spec=importlib.util.spec_from_file_location('native_guard',MODULE)
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
module.__file__=str(Path(__file__).resolve())
original=subprocess.Popen
def spawn(command,*args,**kwargs):
    if command[1:]==['hook-writer-status'] and Path(FAULT).exists():
        command=[sys.executable,'-c','print('+repr(RESPONSE)+')']
    return original(command,*args,**kwargs)
subprocess.Popen=spawn
raise SystemExit(module.main())
''')
        command = lambda *args: [sys.executable, str(driver), '--config', str(self.config), *args]
        with patch.object(self, 'command', side_effect=command):
            self.arm()
            fault.touch()
            self.recovered()
        trip = self.read('trip.json')
        self.assertEqual((trip['category'], trip['stage'], trip['consecutive_failures']),
                         ('http401', 'background_probe', 1))
        self.assertEqual(len(self.probe_failures()), 1)

    def test_three_consecutive_probe_timeouts_trip_with_stage(self):
        with patch.object(self, 'command', side_effect=self.probe_driver((1, 2, 3))):
            self.arm()
            self.wait(self.restored.exists, timeout=10)
            self.recovered()
        failures = self.probe_failures()
        self.assertEqual([value['consecutive_failures'] for value in failures], [1, 2, 3])
        trip = self.read('trip.json')
        self.assertEqual((trip['stage'], trip['operation'], trip['category'], trip['consecutive_failures']),
                         ('background_probe', 'hook-writer-status', 'TimeoutError', 3))
        self.assertEqual(trip['child_pid'], failures[-1]['child_pid'])
        self.assertGreaterEqual(trip['duration_ms'], 2000)

    def test_real_hook_error_trips_while_first_probe_is_pending(self):
        with patch.object(self, 'command', side_effect=self.probe_driver((1, 2, 3))):
            self.arm()
            self.fixture.backend.tokens.clear()
            result, elapsed = self.call('session-start')
            self.assertTrue(result.stderr)
            self.assertLess(elapsed, 1)
            self.recovered()
        trip = self.read('trip.json')
        self.assertEqual((trip['category'], trip['stage']), ('hook_error', 'hook'))
        self.assertFalse(self.probe_failures())

    def test_completed_native_status_survives_late_supervisor_wakeup(self):
        spec = importlib.util.spec_from_file_location('native_guard', GUARD)
        module = importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        config = module._context(self.config)
        spawn = subprocess.Popen
        def delayed_poll(*args, **kwargs):
            child = spawn(*args, **kwargs)
            poll = child.poll
            paused = False
            def completed():
                nonlocal paused
                if not paused:
                    paused = True
                    child.wait(timeout=2)
                    time.sleep(.3)  # Supervisor resumes beyond the 250 ms status deadline.
                return poll()
            child.poll = completed
            return child
        with patch.object(module.subprocess, 'Popen', side_effect=delayed_poll):
            status = module._status(config)
        self.assertTrue(status['async_writes'])
        self.assertTrue(self.fixture.marker.exists())
        self.assertFalse((self.state / 'trip.json').exists())

    def test_status_timeout_trace_records_real_child_pid_and_duration(self):
        spec = importlib.util.spec_from_file_location('native_guard', GUARD)
        module = importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        config = module._context(self.config)
        config['binary'] = sys.executable
        trace = module._trace('watchdog', 'hook-writer-status')
        with self.assertRaises(TimeoutError):
            module._native(config, ['-c', 'import time;time.sleep(60)'], budget=.1, trace=trace)
        module._trip(config, 'TimeoutError', trace)
        self.recovered()
        trip = self.read('trip.json')
        self.assertEqual((trip['stage'], trip['operation']), ('watchdog', 'hook-writer-status'))
        self.assertGreater(trip['child_pid'], 0)
        self.assertGreaterEqual(trip['duration_ms'], 100)
        self.assertLess(trip['duration_ms'], 2000)

    def test_native_stderr_remains_bounded(self):
        spec = importlib.util.spec_from_file_location('native_guard', GUARD)
        module = importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        config = module._context(self.config)
        config['binary'] = sys.executable
        with self.assertRaisesRegex(ValueError, 'oversized native stderr'):
            module._native(config, ['-c', 'import os;os.write(2,b"x"*100000)'], budget=2)

    def test_running_native_child_still_obeys_deadline(self):
        self.fixture.backend.recent_delay = .4
        spec = importlib.util.spec_from_file_location('native_guard', GUARD)
        module = importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        config = module._context(self.config)
        payload = json.dumps({'cwd': str(self.fixture.project)}).encode()
        with self.assertRaises(TimeoutError):
            module._native(config, ['hook', 'session-start', '--harness', 'claude-code'], payload, budget=.1)

    def test_successful_foreground_does_not_wait_for_fsync(self):
        self.arm()
        driver = self.fixture.home / 'slow-fsync.py'
        counts = self.fixture.home / 'foreground-fsync-count'
        diagnostics = self.fixture.home / 'foreground-diagnostics.json'
        arguments = ['--config', str(self.config), 'hook', 'prompt', '--harness', 'claude-code']
        driver.write_text('MODULE=' + repr(str(GUARD)) + '\nARGUMENTS=' + repr(arguments) +
            '\nCOUNTS=' + repr(str(counts)) + '\nDIAGNOSTICS=' + repr(str(diagnostics)) + '\n' + r'''import importlib.util,json,os,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('native_guard',MODULE)
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
original=os.fsync
count=0
trace={}
def fsync(descriptor):
    global count
    count+=1
    time.sleep(.2)
    return original(descriptor)
def instrument(name):
    function=getattr(module,name)
    def run(*args,**kwargs):
        started=time.monotonic()
        value={}
        try:
            result=function(*args,**kwargs)
            if name=='_native':
                code,output,stderr,report=result
                value.update(returncode=code,stderr_present=bool(stderr))
                if report:
                    try:value['native_category']=json.loads(report)['category']
                    except (ValueError,KeyError):value['native_category']='invalid_report'
            return result
        except Exception as error:
            value['error_type']=type(error).__name__
            allowed={'watchdog_lost_or_stale','watchdog handshake','native deadline','watchdog_startup'}
            if str(error) in allowed:value['category']=str(error)
            raise
        finally:
            value['elapsed_ms']=round((time.monotonic()-started)*1000,3)
            trace[name]=value
    setattr(module,name,run)
for name in ('arm','_input','_native','_record'):instrument(name)
os.fsync=fsync
result=module.main(ARGUMENTS)
Path(COUNTS).write_text(str(count))
Path(DIAGNOSTICS).write_text(json.dumps(trace))
raise SystemExit(result)
''')
        payload = json.dumps({'cwd': str(self.fixture.project), 'prompt': 'synthetic guarded input'})
        environment = dict(self.fixture.environment)
        started = time.perf_counter()
        environment['HYPERAG_HOOK_STARTED_AT'] = str(time.time())
        result = subprocess.run([sys.executable, str(driver)], env=environment, input=payload,
                                text=True, capture_output=True, timeout=5)
        diagnostic = self.call_diagnostics()
        if diagnostics.exists():diagnostic['foreground_stages'] = json.loads(diagnostics.read_text())
        if result.returncode or result.stderr:
            print(json.dumps({'fixture':'fsync_guard_failure','metadata':diagnostic}), flush=True)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, '', ''), json.dumps(diagnostic))
        self.assertLess(time.perf_counter() - started, .3)
        self.assertEqual(counts.read_text(), '0')
        self.assertTrue(self.fixture.marker.exists())
        self.assertFalse((self.state / 'trip.json').exists())

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
            self.assertEqual(set(frame), {'category', 'scope', 'steps_ms', 'stage'})
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
        calibration = json.loads((DIRECTORY.parents[1] / 'docs/evidence/hooks-guard-tail-2026-10-27.json').read_text())
        self.assertEqual(module.TAIL_MARGIN_MS, calibration['selected_margin_ms'])
        hosted = max(record['p99_ms'] for record in calibration['reference_hosted_runs'])
        selected = math.ceil(max(2 * hosted, 1.25 * calibration['retained_mac']['p99_ms']) / 10) * 10
        self.assertEqual(module.TAIL_MARGIN_MS, selected)
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
        rows = [dict(row(1, True), cohort='controlled')] + [dict(row(1), cohort=name)
            for name in ('controlled', 'organic') for _ in range(50)]
        written = {}
        with patch.object(module, '_rows', return_value=rows), \
                patch.object(module, '_write', side_effect=lambda path, value: written.update({path.name: value})):
            module._reports({'state_dir': str(self.state)})
        checkpoints = written['checkpoints.json']
        self.assertEqual((checkpoints['50']['calls'], checkpoints['50']['cohorts']),
                         (50, {'controlled': 50, 'organic': 0}))
        self.assertEqual((checkpoints['100']['calls'], checkpoints['100']['cohorts']),
                         (100, {'controlled': 50, 'organic': 50}))

    def test_first_slow_warmup_call_still_trips_hard_budget(self):
        self.fixture.backend.recent_delay = 1.05
        self.arm()
        result, elapsed = self.call('session-start')
        self.assertEqual(result.returncode, 0)
        self.assertGreater(elapsed, 1)
        self.recovered()
        self.wait(lambda: self.read('summary.json').get('warmup_calls') == 1)


if __name__ == '__main__':
    unittest.main()
