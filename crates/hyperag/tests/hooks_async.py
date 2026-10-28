"""Real native hook entrypoints against a synthetic slow backend; no models or database."""
import hashlib
import json
import math
import os
from pathlib import Path
import signal
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

BIN = str(Path(os.getenv("HYPERAG_BIN", "target/release/hyperag")).resolve())


class Backend:
    def __init__(self):
        self.lock = threading.Lock()
        self.grants = {"project:fixture": (True, True), "project:second": (True, True)}
        self.tokens = {"fixture-token-a", "fixture-token-b"}
        self.scope_gets = 0
        self.requests = []
        self.stored = []
        self.active = 0
        self.maximum_active = 0
        self.post_delay = .4
        self.recent_delay = 0
        self.scope_delay = 0
        self.gate = None
        self.started = threading.Event()
        backend = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def reply(self, code, body):
                raw = json.dumps(body).encode()
                try:
                    self.send_response(code)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(raw)))
                    self.end_headers()
                    self.wfile.write(raw)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def authorized(self):
                return self.headers.get("Authorization", "").removeprefix("Bearer ") in backend.tokens

            def do_GET(self):
                route = urlsplit(self.path).path
                with backend.lock:
                    backend.requests.append(("GET", route))
                if not self.authorized():
                    return self.reply(401, {})
                if route == "/v1/scopes":
                    time.sleep(backend.scope_delay)
                    with backend.lock:
                        backend.scope_gets += 1
                        grants = dict(backend.grants)
                    return self.reply(200, {"read": [s for s, (r, _w) in grants.items() if r],
                                            "write": [s for s, (_r, w) in grants.items() if w]})
                if route == "/v1/recent":
                    time.sleep(backend.recent_delay)
                    return self.reply(200, {"text": "synthetic recent", "items": []})
                self.reply(404, {})

            def do_POST(self):
                route = urlsplit(self.path).path
                raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                body = json.loads(raw)
                with backend.lock:
                    backend.requests.append(("POST", route))
                    writable = backend.grants.get(body.get("scope"), (False, False))[1]
                if not self.authorized():
                    return self.reply(401, {})
                if route != "/v1/memories":
                    return self.reply(404, {})
                if not writable:
                    return self.reply(403, {})
                with backend.lock:
                    backend.active += 1
                    backend.maximum_active = max(backend.maximum_active, backend.active)
                backend.started.set()
                try:
                    if backend.gate is not None:
                        backend.gate.wait(timeout=5)
                    time.sleep(backend.post_delay)
                    with backend.lock:
                        backend.stored.append(body)
                    self.reply(200, {"id": "fixture-id", "version": 1, "created": True})
                finally:
                    with backend.lock:
                        backend.active -= 1

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.url = "http://127.0.0.1:" + str(self.server.server_port)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        if self.gate is not None:
            self.gate.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


@unittest.skipUnless(sys.platform == "linux", "hosted Linux native IPC proof")
class AsyncHooks(unittest.TestCase):
    def setUp(self):
        self.assertTrue(Path(BIN).is_file(), "the built native binary is required")
        self.temporary = tempfile.TemporaryDirectory(prefix="ha-")
        self.home = Path(self.temporary.name).resolve()
        self.backend = Backend()
        self.token = "fixture-token-a"
        self.credential = self.home / "credential.json"
        self.credential.write_text(json.dumps({"token": self.token}))
        self.credential.chmod(0o600)
        self.marker = self.home / "active"
        self.marker.touch()
        self.project = self.home / "fixture"
        self.project.mkdir()
        self.environment = {name: value for name in ("PATH", "TMPDIR") if (value := os.getenv(name)) is not None}
        self.environment.update(HOME=str(self.home), XDG_CONFIG_HOME=str(self.home / ".config"),
                                HYPERAG_TOKEN="wrong-inherited-identity", TOKIO_WORKER_THREADS="1")
        directory = self.home / ".config/hyperag"
        directory.mkdir(mode=0o700, parents=True)
        self.config = directory / "hooks.toml"
        self.configure()

    def configure(self, asynchronous=True):
        self.config.write_text("\n".join([
            "url = " + json.dumps(self.backend.url), "token_file = " + json.dumps(str(self.credential)),
            "async_writes = " + str(asynchronous).lower(), "timeout_ms = 1500",
            "async_active_marker = " + json.dumps(str(self.marker)), "",
        ]))
        self.config.chmod(0o600)

    def queue(self, token=None, url=None):
        key = hashlib.sha256(((url or self.backend.url).rstrip("/") + "\0" + (token or self.token)).encode()).hexdigest()[:32]
        return self.home / ".hyperag-hooks" / key

    def status(self, root=None):
        path = (root or self.queue()) / "status.json"
        return json.loads(path.read_text()) if path.exists() else {}

    def wait_for(self, predicate, timeout=4):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(.01)
        self.fail("native asynchronous hook did not reach expected checkpoint")

    def hook(self, event="prompt", prompt="synthetic prompt payload", cwd=None):
        started = time.perf_counter()
        result = subprocess.run([BIN, "hook", event, "--harness", "claude-code"], env=self.environment,
                                input=json.dumps({"cwd": str(cwd or self.project), "session_id": "fixture-session", "prompt": prompt}),
                                text=True, capture_output=True, timeout=4)
        self.assertEqual(result.returncode, 0)
        return result, time.perf_counter() - started

    def native_worker(self, pid):
        try:
            return Path(f"/proc/{pid}/exe").resolve(strict=True) == Path(BIN)
        except FileNotFoundError:
            return False

    def tearDown(self):
        self.marker.unlink(missing_ok=True)
        if self.backend.gate is not None:
            self.backend.gate.set()
        self.backend.post_delay = 0
        pids = []
        for path in (self.home / ".hyperag-hooks").glob("*/status.json"):
            pid = json.loads(path.read_text()).get("pid")
            if pid and self.native_worker(pid):
                pids.append(pid)
                try:os.killpg(pid, signal.SIGTERM)
                except ProcessLookupError:pass
        deadline = time.monotonic() + 3
        while any(self.native_worker(pid) for pid in pids) and time.monotonic() < deadline:
            time.sleep(.02)
        for pid in pids:
            if self.native_worker(pid):
                try:os.killpg(pid, signal.SIGKILL)
                except ProcessLookupError:pass
        self.backend.close()
        self.temporary.cleanup()

    def test_default_off_keeps_writes_synchronous(self):
        self.configure(asynchronous=False)
        self.config.write_text(self.config.read_text().replace("async_writes = false\n", ""))
        result, elapsed = self.hook()
        self.assertEqual(result.stderr, "")
        self.assertGreaterEqual(elapsed, .38)
        self.assertEqual(len(self.backend.stored), 1)
        self.assertEqual(self.status(), {})

    def test_async_requires_explicit_private_credential_file(self):
        self.config.write_text("\n".join(line for line in self.config.read_text().splitlines() if not line.startswith("token_file")))
        self.environment["HYPERAG_TOKEN"] = self.token
        result, _elapsed = self.hook()
        self.assertIn("token_file", result.stderr)
        self.assertEqual(self.backend.requests, [])

    def test_unsafe_credential_mode_and_queue_symlink_fail_closed(self):
        self.credential.chmod(0o644)
        rejected, _elapsed = self.hook()
        self.assertTrue(rejected.stderr)
        self.assertEqual(self.backend.requests, [])
        self.credential.chmod(0o600)
        target = self.home / "outside";target.mkdir()
        (self.home / ".hyperag-hooks").symlink_to(target, target_is_directory=True)
        rejected, _elapsed = self.hook()
        self.assertTrue(rejected.stderr)
        self.assertEqual(list(target.iterdir()), [])
        self.assertEqual(self.backend.requests, [("GET", "/v1/scopes")])

    def test_native_fast_fixture_reports_session_start_and_prompt_quantiles(self):
        self.backend.post_delay = 0
        self.hook("session-start")
        read_times = []
        for _ in range(20):
            result, elapsed = self.hook("session-start")
            self.assertEqual(result.stderr, "")
            read_times.append(elapsed * 1000)
        self.hook();self.wait_for(lambda: self.status().get("stored") == 1)
        write_times = []
        for completed in range(2, 22):
            result, elapsed = self.hook()
            self.assertEqual(result.stderr, "")
            write_times.append(elapsed * 1000)
            # Keep this latency sample independent of the preceding delivery.
            self.wait_for(lambda: self.status().get("stored") == completed)
        for cohort, samples in [("session-start", read_times), ("prompt", write_times)]:
            values = sorted(samples)
            p95 = values[math.ceil(.95 * len(values)) - 1]
            print(json.dumps({"fixture": "native_cli_mock_backend", "cohort": cohort, "calls": len(values),
                              "p50_ms": round(statistics.median(values), 3), "p95_ms": round(p95, 3)}), flush=True)
            self.assertLess(p95, 300, "native fixture latency, not live guarded acceptance")

    def test_marker_cancels_queued_write_with_category_and_no_second_post(self):
        self.backend.gate = threading.Event()
        self.hook();self.assertTrue(self.backend.started.wait(timeout=1))
        self.hook("session-end")
        self.marker.unlink()
        self.backend.post_delay = 0;self.backend.gate.set()
        self.wait_for(lambda: self.status().get("cancelled") == 1)
        self.assertEqual(self.status()["accepted"], self.status()["stored"] + self.status()["failed"])
        self.assertEqual(self.status()["categories"]["cancelled"], 1)
        self.assertEqual(sum(method == "POST" for method, _route in self.backend.requests), 1)

    def test_native_prompt_and_end_return_before_slow_posts_then_store_in_order(self):
        before = dict(self.backend.grants)
        first, prompt_wall = self.hook()
        self.assertEqual(first.stderr, "")
        self.assertLess(prompt_wall, .3)
        self.assertEqual(self.backend.stored, [])
        end, end_wall = self.hook("session-end")
        self.assertEqual(end.stderr, "")
        self.assertLess(end_wall, .3)
        self.wait_for(lambda: self.status().get("stored") == 2)
        self.assertEqual([item["kind"] for item in self.backend.stored], ["prompt", "session"])
        self.assertEqual(self.backend.maximum_active, 1)
        self.assertEqual(self.backend.grants, before)
        self.assertEqual(self.backend.scope_gets, 2)
        self.assertNotIn(("POST", "/v1/grants"), self.backend.requests)
        self.assertEqual(self.status()["accepted"], 2)
        self.assertEqual(self.status()["failed"], 0)

    def test_session_start_is_sync_and_fresh_native_read_is_fast(self):
        self.backend.recent_delay = .4
        result, elapsed = self.hook("session-start")
        self.assertEqual(result.stderr, "")
        self.assertIn("synthetic recent", result.stdout)
        self.assertGreaterEqual(elapsed, .38)
        self.assertEqual(self.status(), {})
        self.backend.recent_delay = 0
        warm, elapsed = self.hook("session-start")
        self.assertEqual(warm.stderr, "")
        self.assertLess(elapsed, .3)
        self.assertEqual(self.backend.scope_gets, 2)

    def test_every_call_refreshes_grants_and_denial_or_regrant_is_immediate(self):
        self.backend.post_delay = 0
        before = dict(self.backend.grants)
        self.hook();self.wait_for(lambda: self.status().get("stored") == 1)
        self.hook();self.wait_for(lambda: self.status().get("stored") == 2)
        self.assertEqual(self.backend.scope_gets, 2)
        self.backend.grants["project:fixture"] = (True, False)
        denied, _elapsed = self.hook()
        self.assertEqual((denied.stdout, denied.stderr), ("", ""))
        self.assertEqual(self.status()["accepted"], 2)
        self.assertEqual(len(self.backend.stored), 2)
        self.assertEqual(self.backend.scope_gets, 3)
        self.backend.grants["project:fixture"] = (True, True)
        self.hook();self.wait_for(lambda: self.status().get("stored") == 3)
        self.assertEqual(self.backend.scope_gets, 4)
        second = self.home / "second";second.mkdir()
        self.backend.grants["project:second"] = (True, False)
        denied, _elapsed = self.hook(cwd=second)
        self.assertEqual((denied.stdout, denied.stderr), ("", ""))
        self.assertEqual(self.backend.scope_gets, 5)
        self.assertEqual(self.status()["accepted"], 3)
        self.backend.grants["project:second"] = (True, True)
        self.hook(cwd=second);self.wait_for(lambda: self.status().get("stored") == 4)
        self.assertEqual(self.backend.scope_gets, 6)
        self.assertEqual(self.backend.grants, before)
        self.assertNotIn(("POST", "/v1/grants"), self.backend.requests)
        self.assertEqual(list(self.queue().glob("scope*")), [])
        self.assertFalse((self.queue() / "grant-generation.json").exists())

    def test_absent_marker_records_cancelled_without_enqueue_or_post(self):
        self.marker.unlink()
        result, _elapsed = self.hook()
        self.assertEqual(result.stderr, "")
        self.assertEqual(self.status(), {})
        ledger = json.loads((self.queue() / "enqueue-errors.json").read_text())
        self.assertEqual(ledger["categories"]["cancelled"], 1)
        self.assertFalse(any(method == "POST" for method, _route in self.backend.requests))

    def test_ungranted_fixed_scope_is_quiet_before_async_enqueue(self):
        scope = "personal:fixture"
        self.config.write_text(self.config.read_text() + 'scope = "personal:fixture"\n')
        for event in ("session-start", "prompt", "session-end"):
            result, _elapsed = self.hook(event)
            self.assertEqual((result.stdout, result.stderr), ("", ""))
        self.assertEqual(self.backend.scope_gets, 3)
        self.assertEqual(self.status(), {})
        self.assertFalse(any(method == "POST" for method, _route in self.backend.requests))
        ledger = json.loads((self.queue() / "enqueue-errors.json").read_text())
        self.assertEqual(ledger["categories"]["skipped_ungranted"], 3)
        self.backend.grants[scope] = (True, True)
        self.backend.post_delay = 0
        accepted, _elapsed = self.hook()
        self.assertEqual(accepted.stderr, "")
        self.wait_for(lambda: self.status().get("stored") == 1)
        self.assertEqual(self.backend.scope_gets, 4)
        self.assertEqual(self.backend.stored[0]["scope"], scope)

    def test_ungranted_project_never_posts_or_starts_worker(self):
        self.backend.grants["project:fixture"] = (True, False)
        before = dict(self.backend.grants)
        result, _elapsed = self.hook()
        self.assertEqual((result.stdout, result.stderr), ("", ""))
        self.assertEqual(self.status(), {})
        self.assertFalse(any(method == "POST" for method, _route in self.backend.requests))
        self.assertEqual(self.backend.grants, before)
        ledger = json.loads((self.queue() / "enqueue-errors.json").read_text())
        self.assertEqual(ledger["categories"]["skipped_ungranted"], 1)
        self.assertEqual(ledger["rejected"], 0)

    def test_revocation_after_enqueue_is_counted_as_background_delivery_failure(self):
        self.backend.gate = threading.Event()
        self.hook();self.assertTrue(self.backend.started.wait(timeout=1))
        accepted, _elapsed = self.hook("session-end")
        self.assertEqual(accepted.stderr, "")
        self.assertEqual(self.backend.scope_gets, 2)
        self.assertEqual(self.status()["accepted"], 2)
        self.backend.grants["project:fixture"] = (True, False)
        self.backend.post_delay = 0;self.backend.gate.set()
        self.wait_for(lambda: self.status().get("categories", {}).get("http403") == 1 or
                      self.status().get("categories", {}).get("http_403") == 1)
        self.assertTrue(self.status()["blocked"])
        self.assertEqual(self.status()["stored"], 1)
        self.assertEqual(self.status()["failed"], 1)
        skipped, _elapsed = self.hook()
        self.assertEqual((skipped.stdout, skipped.stderr), ("", ""))
        self.assertEqual(self.backend.scope_gets, 3)
        self.assertEqual(self.status()["accepted"], 2)
        self.assertEqual(sum(method == "POST" for method, _route in self.backend.requests), 2)
        self.assertNotIn(("POST", "/v1/grants"), self.backend.requests)

    def raw_enqueue(self, body):
        raw = json.dumps(body).encode()
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(1)
            stream.connect(str(self.queue() / "q"))
            stream.sendall(struct.pack(">I", len(raw)))
            credit = stream.recv(1)
            if credit != b"\0":return credit
            stream.sendall(raw)
            return stream.recv(1)

    def test_full_event_queue_rejects_actual_hook_and_counts_every_rejection(self):
        self.backend.gate = threading.Event()
        self.hook();self.assertTrue(self.backend.started.wait(timeout=1))
        try:
            body = {"scope": "project:fixture", "kind": "prompt", "source": "fixture", "text": "queued fixture"}
            for _ in range(63):self.assertEqual(self.raw_enqueue(body), b"\0")
            result, _elapsed = self.hook()
            self.assertIn("queue_full", result.stderr)
            self.assertEqual(self.status()["accepted"], 64)
            self.assertGreaterEqual(self.status()["rejected"], 1)
        finally:
            self.backend.post_delay = 0;self.backend.gate.set()
        self.wait_for(lambda: self.status().get("stored") == 64)

    def test_full_byte_budget_rejects_hook_before_more_payload_is_accepted(self):
        self.backend.gate = threading.Event()
        self.hook();self.assertTrue(self.backend.started.wait(timeout=1))
        try:
            body = {"scope": "project:fixture", "kind": "prompt", "source": "fixture", "text": "x" * (1_048_576 - 512)}
            self.assertEqual(self.raw_enqueue(body), b"\0")
            result, _elapsed = self.hook(prompt="y" * 4_000)
            self.assertIn("queue_full", result.stderr)
            self.assertEqual(self.status()["accepted"], 2)
            self.assertGreaterEqual(self.status()["rejected"], 1)
        finally:
            self.backend.post_delay = 0;self.backend.gate.set()
        self.wait_for(lambda: self.status().get("stored") == 2)

    def crash_pending_worker(self):
        self.backend.gate = threading.Event()
        self.hook();self.assertTrue(self.backend.started.wait(timeout=1))
        self.assertEqual(self.status()["accepted"], 1)
        self.assertEqual(self.status()["stored"], 0)
        pid = self.status()["pid"]
        os.killpg(pid, signal.SIGKILL)
        self.wait_for(lambda: not self.native_worker(pid))
        return pid

    def test_accepted_status_save_failure_never_acknowledges_or_posts_item(self):
        self.backend.post_delay = 0
        self.hook();self.wait_for(lambda: self.status().get("stored") == 1)
        status = self.queue() / "status.json"
        backup = self.queue() / "status-backup.json"
        raw = json.dumps({"scope": "project:fixture", "kind": "prompt", "source": "fixture", "text": "must not publish"}).encode()
        try:
            with socket.socket(socket.AF_UNIX) as stream:
                stream.settimeout(1);stream.connect(str(self.queue() / "q"))
                stream.sendall(struct.pack(">I", len(raw)));self.assertEqual(stream.recv(1), b"\0")
                status.rename(backup);status.mkdir(mode=0o700)
                stream.sendall(raw)
                try:ack = stream.recv(1)
                except ConnectionResetError:ack = b""
                self.assertNotEqual(ack, b"\0")
            rejected, _elapsed = self.hook()
            self.assertTrue(rejected.stderr)
            ledger = json.loads((self.queue() / "enqueue-errors.json").read_text())
            self.assertGreaterEqual(ledger["rejected"], 1)
            self.assertEqual(len(self.backend.stored), 1)
            self.assertEqual(sum(method == "POST" for method, _route in self.backend.requests), 1)
        finally:
            if status.is_dir():status.rmdir()
            if backup.exists():backup.replace(status)

    def test_idle_exit_reprobes_worker_lock_and_starts_clean_replacement(self):
        parent = self.home / ".hyperag-hooks";parent.mkdir(mode=0o700)
        self.queue().mkdir(mode=0o700)
        held = self.home / "held"
        helper = self.home / "idle-exit.py"
        helper.write_text("import fcntl,time\nfrom pathlib import Path\n" +
            "root=Path(" + repr(str(self.queue())) + ")\n" +
            "with (root/'worker.lock').open('a+b') as worker:\n" +
            " fcntl.flock(worker,fcntl.LOCK_EX)\n" +
            " (root/'worker.lock').chmod(0o600)\n Path(" + repr(str(held)) + ").touch()\n" +
            " deadline=time.monotonic()+2\n while time.monotonic()<deadline:\n" +
            "  if (root/'startup.lock').exists():\n" +
            "   with (root/'startup.lock').open('a+b') as startup:\n" +
            "    try:fcntl.flock(startup,fcntl.LOCK_EX|fcntl.LOCK_NB)\n" +
            "    except BlockingIOError:time.sleep(.02);break\n" +
            "    else:fcntl.flock(startup,fcntl.LOCK_UN)\n" +
            "  time.sleep(.001)\n")
        process = subprocess.Popen([sys.executable, str(helper)], stdin=subprocess.DEVNULL,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            self.wait_for(held.exists)
            result, elapsed = self.hook()
            self.assertEqual(result.stderr, "")
            self.assertLess(elapsed, .3)
            self.wait_for(lambda: self.status().get("stored") == 1)
        finally:
            process.wait(timeout=3)

    def test_worker_crash_is_lost_queue_not_silent_restart(self):
        old_pid = self.crash_pending_worker()
        refused, _elapsed = self.hook()
        self.assertIn("lost_queue", refused.stderr)
        self.assertEqual(self.status()["pid"], old_pid)
        self.assertEqual(self.status()["categories"]["lost_queue"], 1)
        self.assertTrue(self.status()["blocked"])
        self.assertEqual(self.status()["accepted"], 1)

    def test_status_watch_promotes_dead_pending_without_another_enqueue(self):
        old_pid = self.crash_pending_worker()
        result = subprocess.run([BIN, "hook-writer-status"], env=self.environment, text=True, capture_output=True, timeout=2)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stderr, "")
        status = json.loads(result.stdout)
        self.assertEqual(status["pid"], old_pid)
        self.assertEqual(status["categories"]["lost_queue"], 1)
        self.assertEqual(status["failed"], 1)
        self.assertTrue(status["blocked"])

    def test_worker_survives_parent_hook_process_group_kill(self):
        marker = self.home / "driver-returned"
        driver = self.home / "driver.py"
        driver.write_text("import subprocess,time\nfrom pathlib import Path\n" +
                          "result=subprocess.run(" + repr([BIN, "hook", "prompt", "--harness", "claude-code"]) +
                          ",input=" + repr(json.dumps({"cwd": str(self.project), "prompt": "detached fixture"})) +
                          ",text=True,capture_output=True)\nassert result.returncode==0 and not result.stderr\n" +
                          "Path(" + repr(str(marker)) + ").touch()\ntime.sleep(60)\n")
        process = subprocess.Popen([sys.executable, str(driver)], env=self.environment, start_new_session=True,
                                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            self.wait_for(marker.exists)
            worker_pid = self.status()["pid"]
            self.assertEqual(os.getpgid(worker_pid), worker_pid)
            os.killpg(process.pid, signal.SIGKILL);process.wait(timeout=2)
            self.wait_for(lambda: self.status().get("stored") == 1)
        finally:
            if process.poll() is None:os.killpg(process.pid, signal.SIGKILL);process.wait(timeout=2)

    def test_native_credentials_and_common_scope_need_no_python_or_git_wrapper(self):
        self.backend.post_delay = 0
        poisoned = self.home / "bin";poisoned.mkdir()
        unexpected = self.home / "unexpected-wrapper"
        for name in ("python", "python3", "git"):
            path = poisoned / name
            path.write_text("#!/bin/sh\nprintf called >> " + repr(str(unexpected)) + "\nexit 99\n");path.chmod(0o700)
        self.environment["PATH"] = str(poisoned)
        repository = self.home / "Common"
        metadata = repository / ".git/worktrees/chief";metadata.mkdir(parents=True)
        (metadata / "commondir").write_text("../..\n")
        worktree = self.home / "chief";worktree.mkdir()
        (worktree / ".git").write_text("gitdir: " + str(metadata) + "\n")
        self.backend.grants["project:common"] = (True, True)
        payload = "private synthetic queue payload"
        result, elapsed = self.hook(prompt=payload, cwd=worktree)
        self.assertEqual(result.stderr, "")
        self.assertLess(elapsed, .3)
        self.wait_for(lambda: self.status().get("stored") == 1)
        self.assertFalse(unexpected.exists())
        self.assertEqual(self.backend.stored[0]["scope"], "project:common")
        for path in (self.home / ".hyperag-hooks").rglob("*"):
            if path.is_file():
                raw = path.read_bytes()
                self.assertNotIn(self.token.encode(), raw)
                self.assertNotIn(payload.encode(), raw)
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_worker_is_credential_and_endpoint_bound(self):
        self.backend.post_delay = 0
        self.hook();self.wait_for(lambda: self.status().get("stored") == 1)
        first_pid = self.status()["pid"]
        self.hook();self.wait_for(lambda: self.status().get("stored") == 2)
        self.assertEqual(self.status()["pid"], first_pid)
        self.credential.write_text(json.dumps({"token": "fixture-token-b"}))
        second_root = self.queue(token="fixture-token-b")
        self.hook();self.wait_for(lambda: self.status(second_root).get("stored") == 1)
        self.assertNotEqual(first_pid, self.status(second_root)["pid"])
        self.assertEqual(self.backend.scope_gets, 3)
        other = Backend();other.post_delay = 0
        try:
            self.environment["HYPERAG_URL"] = other.url
            third_root = self.queue(token="fixture-token-b", url=other.url)
            self.hook();self.wait_for(lambda: self.status(third_root).get("stored") == 1)
            self.assertEqual(other.scope_gets, 1)
            self.assertNotEqual(self.status(second_root)["pid"], self.status(third_root)["pid"])
        finally:
            other.close()


if __name__ == "__main__":
    unittest.main()
