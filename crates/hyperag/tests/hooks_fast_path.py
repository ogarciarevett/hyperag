"""Native read fast-path contract; synthetic credentials and loopback fixtures only."""
import fcntl
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
        self.scope_calls = 0
        self.recent_calls = 0
        self.posts = 0
        self.delay = 0
        self.recent_status = 200
        self.read_started = threading.Event()
        self.hold_next_grant = False
        self.grant_captured = threading.Event()
        self.release_grant = threading.Event()
        self.grant_observed_ms = []
        backend = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_): pass
            def reply(self, status, value):
                body = json.dumps(value).encode()
                self.send_response(status); self.send_header('Content-Length', str(len(body)))
                self.end_headers(); self.wfile.write(body)
            def do_GET(self):
                token = self.headers.get('Authorization', '').removeprefix('Bearer ')
                if token not in backend.tokens:
                    self.reply(401, {}); return
                if self.path == '/v1/scopes':
                    backend.scope_calls += 1
                    scopes = sorted(backend.granted) if token == 'fixture-owner' else []
                    backend.grant_observed_ms.append(time.time() * 1000)
                    if backend.hold_next_grant:
                        backend.hold_next_grant = False
                        backend.grant_captured.set()
                        backend.release_grant.wait(1.2)
                    self.reply(200, {'read': scopes, 'write': scopes})
                else:
                    backend.recent_calls += 1; backend.read_started.set()
                    time.sleep(backend.delay)
                    self.reply(backend.recent_status, {'text': 'synthetic context'})
            def do_POST(self):
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

    def test_twenty_native_reads_cache_and_private_metadata(self):
        warm, _ = self.hook(); self.success(warm)
        values = []
        for _ in range(20):
            result, elapsed = self.hook(); self.success(result); values.append(elapsed)
            self.assertIn('synthetic context', result.stdout)
        p95 = sorted(values)[math.ceil(.95 * len(values)) - 1]
        print(json.dumps({'native_session_start_calls': len(values), 'p50_ms': round(statistics.median(values), 3),
                          'p95_ms': round(p95, 3), 'errors': 0, 'scope_requests': self.backend.scope_calls,
                          'recent_requests': self.backend.recent_calls, 'posts': self.backend.posts}))
        self.assertLess(p95, 300)
        self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (1, 21, 0))
        caches = list((self.home / '.hyperag-hooks').glob('*/scope-[0-9a-f]*.json'))
        self.assertEqual(len(caches), 1)
        for path in caches:
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            text = path.read_text()
            self.assertNotIn('fixture-owner', text); self.assertNotIn('synthetic context', text)
            self.assertEqual(set(json.loads(text)), {'scope', 'eligible', 'expires_ms', 'generation'})
        self.assertEqual(caches[0].parent.stat().st_mode & 0o777, 0o700)

    def test_contended_atomic_cache_replacement_falls_back_to_authoritative_get(self):
        warm, _ = self.hook(); self.success(warm)
        cache = next((self.home / '.hyperag-hooks').glob('*/scope-[0-9a-f]*.json'))
        cached = cache.read_text()
        stop = threading.Event()
        replacements = []
        def replace_cache():
            while not stop.is_set():
                with tempfile.NamedTemporaryFile(mode='w', dir=cache.parent, delete=False) as file:
                    file.write(cached)
                    temporary = Path(file.name)
                temporary.replace(cache)
                replacements.append(True)
                stop.wait(.001)
        with (cache.parent / 'scopes.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            writer = threading.Thread(target=replace_cache, daemon=True); writer.start()
            try:
                for _ in range(10):
                    result, _ = self.hook(); self.success(result)
                    self.assertIn('synthetic context', result.stdout)
                self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (11, 11, 0))
                self.assertTrue(replacements)
            finally:
                stop.set(); writer.join(timeout=2)
                self.assertFalse(writer.is_alive())

    def held_grant_process(self):
        self.backend.hold_next_grant = True
        process = subprocess.Popen([BIN, 'hook', 'session-start', '--harness', 'claude-code'], env=self.env,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        process.stdin.write(b'{}'); process.stdin.close(); process.stdin = None
        self.assertTrue(self.backend.grant_captured.wait(2))
        return process

    def test_older_allow_response_cannot_replace_newer_deny(self):
        old = self.held_grant_process()
        try:
            self.backend.granted.clear()
            denied, _ = self.hook(); self.success(denied); self.assertEqual(denied.stdout, '')
            cache = next((self.home / '.hyperag-hooks').glob('*/scope-[0-9a-f]*.json'))
            latest = json.loads(cache.read_text()); self.assertFalse(latest['eligible'])
            self.assertLessEqual(latest['expires_ms'], self.backend.grant_observed_ms[-1] + 5002)
            self.backend.release_grant.set()
            out, err = old.communicate(timeout=3)
            self.assertEqual((old.returncode, out, err), (0, b'', b''))
            self.assertEqual(json.loads(cache.read_text()), latest)
            cached, _ = self.hook(); self.success(cached); self.assertEqual(cached.stdout, '')
            self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (2, 0, 0))
        finally:
            self.backend.release_grant.set()
            if old.poll() is None:old.kill(); old.wait()
            old.stdout.close(); old.stderr.close()

    def test_older_allow_cannot_publish_after_a_deny_that_could_not_cache(self):
        old = self.held_grant_process()
        try:
            root = next((self.home / '.hyperag-hooks').glob('*/grant-generation.json')).parent
            with (root / 'scopes.lock').open('a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                self.backend.granted.clear()
                denied, _ = self.hook(); self.success(denied); self.assertEqual(denied.stdout, '')
                self.assertEqual(list(root.glob('scope-[0-9a-f]*.json')), [])
            self.backend.release_grant.set()
            out, err = old.communicate(timeout=3)
            self.assertEqual((old.returncode, out, err), (0, b'', b''))
            self.assertEqual(list(root.glob('scope-[0-9a-f]*.json')), [])
            fresh, _ = self.hook(); self.success(fresh); self.assertEqual(fresh.stdout, '')
            cache = next(root.glob('scope-[0-9a-f]*.json'))
            self.assertFalse(json.loads(cache.read_text())['eligible'])
            self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (3, 0, 0))
        finally:
            self.backend.release_grant.set()
            if old.poll() is None:old.kill(); old.wait()
            old.stdout.close(); old.stderr.close()

    def test_cache_ttl_starts_before_delayed_grant_response(self):
        process = self.held_grant_process()
        try:
            observed = self.backend.grant_observed_ms[0]
            time.sleep(.35)
            self.backend.release_grant.set()
            out, err = process.communicate(timeout=3)
            self.assertEqual((process.returncode, err), (0, b'')); self.assertIn(b'synthetic context', out)
            cache = next((self.home / '.hyperag-hooks').glob('*/scope-[0-9a-f]*.json'))
            self.assertLessEqual(json.loads(cache.read_text())['expires_ms'], observed + 5002)
            self.assertEqual(self.backend.posts, 0)
        finally:
            self.backend.release_grant.set()
            if process.poll() is None:process.kill(); process.wait()
            process.stdout.close(); process.stderr.close()

    def test_negative_cache_expires_after_five_seconds(self):
        self.backend.granted.clear()
        first, _ = self.hook(); second, _ = self.hook()
        self.success(first); self.success(second); self.assertEqual(first.stdout + second.stdout, '')
        self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (1, 0, 0))
        self.backend.granted.add('project:fixture')
        cached, _ = self.hook(); self.success(cached); self.assertEqual(cached.stdout, '')
        time.sleep(5.1)
        fresh, _ = self.hook(); self.success(fresh); self.assertIn('synthetic context', fresh.stdout)
        self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (2, 1, 0))

    def test_project_credential_and_endpoint_isolation(self):
        result, _ = self.hook(); self.success(result)
        self.configure(scope='project:other')
        other_scope, _ = self.hook(); self.success(other_scope); self.assertEqual(other_scope.stdout, '')
        self.assertEqual(self.backend.scope_calls, 2)
        self.configure()
        self.credential.write_text(json.dumps({'token': 'fixture-other'}))
        other_token, _ = self.hook(); self.success(other_token); self.assertEqual(other_token.stdout, '')
        self.assertEqual(self.backend.scope_calls, 3)
        self.credential.write_text(json.dumps({'token': 'fixture-owner'}))
        endpoint = Backend(); self.addCleanup(endpoint.close); endpoint.granted.clear()
        self.configure(url=endpoint.url)
        other_endpoint, _ = self.hook(); self.success(other_endpoint); self.assertEqual(other_endpoint.stdout, '')
        self.assertEqual((endpoint.scope_calls, endpoint.recent_calls, endpoint.posts), (1, 0, 0))
        self.assertEqual(self.backend.posts, 0)

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
            self.assertEqual(self.backend.posts, 0)
        finally:
            if process.poll() is None:process.kill(); process.wait()
            process.stdout.close(); process.stderr.close()

    def test_writes_always_revalidate_and_remain_synchronous(self):
        result, _ = self.hook(); self.success(result)
        self.backend.granted.clear()
        for event in ('prompt', 'session-end'):
            skipped, _ = self.hook(event); self.success(skipped); self.assertEqual(skipped.stdout, '')
        self.assertEqual((self.backend.scope_calls, self.backend.posts), (3, 0))
        self.backend.granted.add('project:fixture'); self.backend.delay = .4
        for event in ('prompt', 'session-end'):
            write, elapsed = self.hook(event); self.success(write); self.assertGreaterEqual(elapsed, 350)
        self.assertEqual((self.backend.scope_calls, self.backend.posts), (5, 2))

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
        self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (0, 0, 0))

    def test_cached_read_revocation_invalidates_eligibility(self):
        first, _ = self.hook(); self.success(first)
        self.backend.granted.clear(); self.backend.recent_status = 404
        denied, _ = self.hook(); self.assertIn('404', denied.stderr)
        skipped, _ = self.hook(); self.success(skipped); self.assertEqual(skipped.stdout, '')
        self.assertEqual((self.backend.scope_calls, self.backend.recent_calls, self.backend.posts), (2, 2, 0))


if __name__ == '__main__':
    unittest.main()
