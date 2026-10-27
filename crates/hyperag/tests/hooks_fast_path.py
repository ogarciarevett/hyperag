"""Native uncached hook contract; synthetic credentials and loopback fixtures only."""
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BIN = str(Path(os.getenv('HYPERAG_BIN', 'target/release/hyperag')).resolve())


class Backend:
    def __init__(self):
        self.granted = {'project:fixture'}
        self.tokens = {'fixture-owner', 'fixture-other'}
        self.scope_calls = self.recent_calls = self.posts = self.requests = 0
        self.delay = self.scope_delay = 0
        self.scope_status = self.recent_status = 200
        self.read_started = threading.Event()
        backend = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_): pass
            def reply(self, status, value):
                body = json.dumps(value).encode()
                self.send_response(status); self.send_header('Content-Length', str(len(body)))
                self.end_headers(); self.wfile.write(body)
            def do_GET(self):
                backend.requests += 1
                token = self.headers.get('Authorization', '').removeprefix('Bearer ')
                if token not in backend.tokens:
                    self.reply(401, {}); return
                if self.path == '/v1/scopes':
                    backend.scope_calls += 1
                    time.sleep(backend.scope_delay)
                    scopes = sorted(backend.granted) if token == 'fixture-owner' else []
                    self.reply(backend.scope_status, {'read': scopes, 'write': scopes})
                else:
                    backend.recent_calls += 1; backend.read_started.set()
                    time.sleep(backend.delay)
                    self.reply(backend.recent_status, {'text': 'synthetic context'})
            def do_POST(self):
                backend.requests += 1
                self.rfile.read(int(self.headers['Content-Length']))
                backend.posts += 1
                time.sleep(backend.delay)
                self.reply(200, {})
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f'http://127.0.0.1:{self.server.server_port}'

    def close(self):
        self.server.shutdown(); self.server.server_close()


class FastPath(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='hook-read-')
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name).resolve()
        self.backend = Backend(); self.addCleanup(self.backend.close)
        self.config = self.home / '.config/hyperag/hooks.toml'
        self.config.parent.mkdir(parents=True)
        self.credential = self.home / 'credential.json'
        self.credential.write_text(json.dumps({'token': 'fixture-owner'})); self.credential.chmod(0o600)
        self.env = {'PATH': os.defpath, 'HOME': str(self.home), 'XDG_CONFIG_HOME': str(self.home / '.config'),
                    'HYPERAG_TOKEN': 'wrong-inherited-token', 'TOKIO_WORKER_THREADS': '16'}
        self.configure()

    def configure(self, **values):
        config = {'url': self.backend.url, 'scope': 'project:fixture', 'token_file': str(self.credential), 'timeout_ms': 1500, **values}
        self.config.write_text(''.join(key + ' = ' + json.dumps(value) + '\n' for key, value in config.items()))

    def hook(self, event='session-start'):
        started = time.perf_counter()
        result = subprocess.run([BIN, 'hook', event, '--harness', 'claude-code'], env=self.env,
                                input=json.dumps({'cwd': str(self.home), 'session_id': 'fixture', 'prompt': 'synthetic input'}),
                                text=True, capture_output=True, timeout=4)
        return result, (time.perf_counter() - started) * 1000

    def success(self, result):
        self.assertEqual((result.returncode, result.stderr), (0, ''))

    def no_runtime_state(self):
        self.assertFalse((self.home / '.hyperag-hooks').exists())
        self.assertEqual({path.relative_to(self.home).as_posix() for path in self.home.rglob('*') if path.is_file()},
                         {'credential.json', '.config/hyperag/hooks.toml'})

    def measure_fifty(self, scope_delay):
        self.backend.scope_delay = scope_delay
        warm, _ = self.hook(); self.success(warm)
        values = []
        for _ in range(50):
            result, elapsed = self.hook(); self.success(result); values.append(elapsed)
            self.assertIn('synthetic context', result.stdout)
        p95 = sorted(values)[math.ceil(.95 * len(values)) - 1]
        print(json.dumps({'mode': 'native_no_grant_cache', 'calls': len(values), 'warmups': 1,
                          'injected_scope_delay_ms': round(scope_delay * 1000),
                          'p50_ms': round(statistics.median(values), 3), 'p95_ms': round(p95, 3), 'errors': 0,
                          'scopes_get_including_warmup': self.backend.scope_calls,
                          'recent_get_including_warmup': self.backend.recent_calls, 'posts': self.backend.posts,
                          'cache_state_created': (self.home / '.hyperag-hooks').exists()}))
        self.assertLess(p95, 300)
        self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (51, 51, 0))
        self.no_runtime_state()

    def test_fifty_native_uncached_reads(self):
        self.measure_fifty(0)

    def test_fifty_native_uncached_reads_with_100ms_scope_delay(self):
        self.measure_fifty(.1)

    def test_allow_deny_and_regrant_take_effect_immediately(self):
        first, _ = self.hook(); self.success(first); self.assertIn('synthetic context', first.stdout)
        self.backend.granted.clear()
        for event in ('session-start', 'prompt', 'session-end'):
            denied, _ = self.hook(event); self.success(denied); self.assertEqual(denied.stdout, '')
        self.backend.granted.add('project:fixture')
        granted, _ = self.hook(); self.success(granted); self.assertIn('synthetic context', granted.stdout)
        self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (5, 2, 0))
        self.no_runtime_state()

    def test_fixed_non_project_scope_preflights_and_only_writes_when_granted(self):
        scope = 'personal:fixture'
        self.configure(scope=scope)
        for index, event in enumerate(('session-start', 'prompt', 'session-end'), 1):
            with self.subTest(event=event):
                denied, _ = self.hook(event)
                self.success(denied); self.assertEqual(denied.stdout, '')
                self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (index, 0, 0))
        self.no_runtime_state()
        self.backend.granted.add(scope)
        allowed, _ = self.hook('prompt'); self.success(allowed); self.assertEqual(allowed.stdout, '')
        self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (4, 0, 1))
        self.no_runtime_state()

    def test_project_credential_and_endpoint_isolation(self):
        first, _ = self.hook(); self.success(first)
        self.configure(scope='project:other')
        other_scope, _ = self.hook(); self.success(other_scope); self.assertEqual(other_scope.stdout, '')
        self.configure()
        self.credential.write_text(json.dumps({'token': 'fixture-other'}))
        other_token, _ = self.hook(); self.success(other_token); self.assertEqual(other_token.stdout, '')
        self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (3, 1, 0))
        self.credential.write_text(json.dumps({'token': 'fixture-owner'}))
        endpoint = Backend(); self.addCleanup(endpoint.close); endpoint.granted.clear()
        self.configure(url=endpoint.url)
        other_endpoint, _ = self.hook(); self.success(other_endpoint); self.assertEqual(other_endpoint.stdout, '')
        self.assertEqual((endpoint.scope_calls, endpoint.recent_calls, endpoint.posts), (1, 0, 0))
        self.no_runtime_state()

    def test_slow_read_is_waited_without_worker_thread_pool(self):
        self.backend.delay = .4
        process = subprocess.Popen([BIN, 'hook', 'session-start', '--harness', 'claude-code'], env=self.env,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            started = time.perf_counter(); process.stdin.write(b'{}'); process.stdin.close()
            self.assertTrue(self.backend.read_started.wait(2))
            if Path('/proc').is_dir():
                self.assertLessEqual(len(list((Path('/proc') / str(process.pid) / 'task').iterdir())), 3)
            self.assertEqual(process.wait(timeout=3), 0)
            self.assertEqual(process.stderr.read(), b'')
            self.assertIn(b'synthetic context', process.stdout.read())
            self.assertGreaterEqual(time.perf_counter() - started, .35)
            self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (1, 1, 0))
            self.no_runtime_state()
        finally:
            if process.poll() is None:process.kill(); process.wait()
            process.stdout.close(); process.stderr.close()

    def test_writes_always_revalidate_and_remain_synchronous(self):
        first, _ = self.hook(); self.success(first)
        self.backend.granted.clear()
        for event in ('prompt', 'session-end'):
            skipped, _ = self.hook(event); self.success(skipped); self.assertEqual(skipped.stdout, '')
        self.assertEqual((self.backend.scope_calls, self.backend.posts), (3, 0))
        self.backend.granted.add('project:fixture'); self.backend.delay = .4
        for event in ('prompt', 'session-end'):
            write, elapsed = self.hook(event); self.success(write); self.assertGreaterEqual(elapsed, 350)
        self.assertEqual((self.backend.scope_calls, self.backend.posts), (5, 2))
        self.no_runtime_state()

    def test_token_file_permissions_symlink_and_ancestry_fail_before_http(self):
        self.credential.chmod(0o644)
        bad, _ = self.hook(); self.assertNotEqual(bad.stderr, '')
        self.credential.chmod(0o600)
        alias = self.home / 'alias.json'; alias.symlink_to(self.credential)
        self.configure(token_file=str(alias))
        bad, _ = self.hook(); self.assertNotEqual(bad.stderr, '')
        unsafe = self.home / 'unsafe'; unsafe.mkdir(); unsafe.chmod(0o777)
        nested = unsafe / 'token.json'; nested.write_bytes(self.credential.read_bytes()); nested.chmod(0o600)
        self.configure(token_file=str(nested))
        bad, _ = self.hook(); self.assertNotEqual(bad.stderr, '')
        self.assertEqual((self.backend.requests, self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (0, 0, 0, 0))

    def test_invalid_token_file_never_falls_back_to_environment(self):
        for value in ('{broken', '{}', '{"token":""}', json.dumps({'token': 'x' * 65537})):
            self.credential.write_text(value)
            result, _ = self.hook(); self.assertNotEqual(result.stderr, '')
        self.assertEqual((self.backend.requests, self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (0, 0, 0, 0))
        self.no_runtime_state()

    def test_http_errors_are_reported_without_local_state(self):
        self.credential.write_text(json.dumps({'token': 'invalid-fixture-token'}))
        bad, _ = self.hook(); self.assertIn('401', bad.stderr)
        self.credential.write_text(json.dumps({'token': 'fixture-owner'}))
        self.backend.scope_status = 500
        failed, _ = self.hook(); self.assertIn('500', failed.stderr)
        self.backend.scope_status = 200; self.backend.recent_status = 403
        revoked, _ = self.hook(); self.assertIn('403', revoked.stderr)
        self.backend.granted.clear(); self.backend.recent_status = 200
        skipped, _ = self.hook(); self.success(skipped); self.assertEqual(skipped.stdout, '')
        self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (3, 1, 0))
        self.no_runtime_state()


if __name__ == '__main__':
    unittest.main()
