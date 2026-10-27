#!/usr/bin/env python3
"""Native hook supervision: metadata-only policy and an independent delivery watchdog."""
import time
STARTED = time.monotonic()
import argparse
import fcntl
import hashlib
import importlib.util
import json
import math
import secrets
import os
from pathlib import Path
import selectors
import stat
import statistics
import subprocess
import sys
import tempfile


def _module(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_helpers = _module('guard')
_append, _input = _helpers._append, _helpers._input
_recovery = _module('recovery')
SKIPS = {'skipped_ungranted', 'disabled'}
REPORTS = SKIPS | {'completed', 'enqueued', 'empty_prompt', 'error', 'cancelled'}
# Twice the hosted maximum tail (10.663 ms), rounded up to 10 ms; see docs/evidence/hooks-guard-tail-2026-10-27.json.
TAIL_MARGIN_MS = 30


def evaluate(rows):
    measured, errors, warmups, hard = [], 0, 0, False
    for row in rows:
        latency = row['latency_ms']
        if isinstance(latency, bool) or not isinstance(latency, (int, float)) or not math.isfinite(latency) or latency < 0:
            raise ValueError('invalid latency')
        warmup = row.get('warmup', False)
        if not isinstance(warmup, bool):
            raise ValueError('invalid warmup')
        warmups += warmup
        errors += bool(row.get('error'))
        hard |= latency > 1000
        if not warmup:
            measured.append(latency)
    measured.sort()
    p95 = measured[math.ceil(.95 * len(measured)) - 1] if measured else None
    reason = ('hook_error' if errors else 'warmup_count_exceeded' if warmups > 1 else
              'single_call_limit' if hard else 'p95_limit' if len(measured) >= 20 and p95 > 300 else None)
    return {'calls': len(measured), 'warmup_calls': warmups, 'errors': errors,
            'p50_ms': statistics.median(measured) if measured else None, 'p95_ms': p95, 'reason': reason}


def _write(path, value, durable=True):
    with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, delete=False) as file:
        temporary = Path(file.name)
        try:
            json.dump(value, file)
            file.flush()
            if durable:
                os.fsync(file.fileno())
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


def _private(path, mode, directory=False):
    info = path.lstat()
    expected = stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode)
    if not expected or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != mode:
        raise PermissionError('private owned path required')
    return info


def _ancestors(path):
    for part in reversed((path, *path.parents)):
        info = part.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid not in (0, os.getuid()) or (info.st_mode & 0o022 and not info.st_mode & stat.S_ISVTX):
            raise PermissionError('unsafe path ancestry')


def _lock(path):
    descriptor = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    file = os.fdopen(descriptor, 'r+')
    opened, named = os.fstat(descriptor), _private(path, 0o600)
    if (opened.st_dev, opened.st_ino) != (named.st_dev, named.st_ino):
        file.close()
        raise PermissionError('lock changed while opening')
    return file


def _context(path):
    path = path.absolute()
    if '..' in path.parts:
        raise ValueError('invalid configuration path')
    _ancestors(path.parent)
    before = _private(path, 0o600)
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(descriptor, 'rb') as file:
        opened = os.fstat(descriptor)
        raw = file.read(65537)
    after = _private(path, 0o600)
    if len(raw) > 65536 or (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino) or (after.st_dev, after.st_ino) != (opened.st_dev, opened.st_ino) or not stat.S_ISREG(opened.st_mode) or opened.st_uid != os.getuid() or stat.S_IMODE(opened.st_mode) != 0o600:
        raise PermissionError('configuration changed while opening')
    config = json.loads(raw)
    directory = Path(config['state_dir'])
    if not directory.is_absolute() or '..' in directory.parts:
        raise ValueError('absolute state directory required')
    _ancestors(directory.parent)
    directory.mkdir(mode=0o700, exist_ok=True)
    _private(directory, 0o700, directory=True)
    environment = config.get('native_env', {})
    if set(environment) - {'HOME', 'XDG_CONFIG_HOME', 'HYPERAG_URL'} or not Path(environment.get('HOME', '')).is_absolute():
        raise ValueError('explicit named native HOME required')
    if 'XDG_CONFIG_HOME' in environment and not Path(environment['XDG_CONFIG_HOME']).is_absolute():
        raise ValueError('absolute native configuration directory required')
    if environment.get('HYPERAG_URL', config['url']) != config['url']:
        raise ValueError('endpoint mismatch')
    if not Path(config['binary']).is_absolute() or not Path(config['active_marker']).is_absolute():
        raise ValueError('absolute native paths required')
    config['_path'] = str(path)
    config['_fingerprint'] = hashlib.sha256(raw).hexdigest()
    return config


def _trip(config, category):
    directory = Path(config['state_dir'])
    with _lock(directory / 'trip.lock') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return
        path = directory / 'trip.json'
        previous = json.loads(path.read_text()) if path.exists() else {}
        if previous.get('recovery_pid'):
            Path(config['active_marker']).unlink(missing_ok=True)
            return
        # The watcher retries this durable intent if the foreground process dies before dispatch.
        value = {'category': previous.get('category', category), 'time': previous.get('time', time.time())}
        pending = previous.get('pending_recovery_pid')
        if pending:
            acknowledged = _recovery.status_for(directory / 'recovery', pending)
            if acknowledged and acknowledged.get('state') in ('running', 'succeeded', 'failed'):
                value['recovery_pid'] = pending
            else:
                try:
                    os.kill(pending, 0)
                except ProcessLookupError:
                    pending = None
                if pending:
                    return
        _write(path, value)
        try:
            if not value.get('recovery_pid'):
                value['recovery_pid'] = _recovery.dispatch_confirmed(config['rollback_command'], directory / 'recovery')
        except Exception as error:
            value['dispatch_error'] = type(error).__name__
            if getattr(error, 'worker_pid', None):
                value['pending_recovery_pid'] = error.worker_pid
            _write(path, value)
            return
        _write(path, value)
        # A detached worker has acknowledged its durable start before the watcher can stop.
        Path(config['active_marker']).unlink(missing_ok=True)


def _native(config, args, raw=b'', report=False, budget=.25, forward=False):
    """Drain bounded streams while the process runs; report completion never relies on EOF."""
    environment = {'PATH': os.defpath, **config.get('native_env', {}), 'HYPERAG_URL': config['url']}
    reader, writer = os.pipe() if report else (None, None)
    if report:
        environment['HYPERAG_HOOK_REPORT_FD'] = str(writer)
    child = subprocess.Popen([config['binary'], *args], env=environment, stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, close_fds=True,
                             pass_fds=(writer,) if report else ())
    if writer is not None:
        os.close(writer)
    selector = selectors.DefaultSelector()
    output, reported, stderr = bytearray(), bytearray(), False
    offset, total = 0, 0
    for pipe, kind in [(child.stdin, 'input'), (child.stdout, 'output'), (child.stderr, 'error')]:
        os.set_blocking(pipe.fileno(), False)
        if kind == 'input' and not raw:
            pipe.close()
        else:
            selector.register(pipe, selectors.EVENT_WRITE if kind == 'input' else selectors.EVENT_READ, kind)
    if reader is not None:
        os.set_blocking(reader, False)
        selector.register(reader, selectors.EVENT_READ, 'report')
    deadline = time.monotonic() + max(0, budget)
    try:
        while True:
            if time.monotonic() >= deadline:
                raise TimeoutError('native deadline')
            exited = child.poll() is not None
            events = selector.select(0 if exited else min(.01, deadline - time.monotonic()))
            for key, _mask in events:
                if key.data == 'input':
                    try:
                        offset += os.write(key.fd, raw[offset:offset + 65536])
                    except BrokenPipeError:
                        offset = len(raw)
                    if offset == len(raw):
                        selector.unregister(key.fileobj);child.stdin.close()
                    continue
                chunk = os.read(key.fd, 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                if key.data == 'error':
                    stderr = True
                elif key.data == 'report':
                    reported.extend(chunk)
                    if len(reported) > 8192:
                        raise ValueError('oversized native report')
                else:
                    total += len(chunk)
                    if total > 65536:
                        raise ValueError('oversized native output')
                    if forward:
                        sys.stdout.buffer.write(chunk);sys.stdout.buffer.flush()
                    else:
                        output.extend(chunk)
            if exited and not events:
                break
        return child.returncode, bytes(output), stderr, bytes(reported)
    finally:
        if child.poll() is None:
            child.kill();child.wait(timeout=1)
        selector.close()
        for pipe in (child.stdin, child.stdout, child.stderr):
            pipe.close()
        if reader is not None:
            os.close(reader)


def _status(config):
    code, output, stderr, _report = _native(config, ['hook-writer-status'])
    if code or stderr:
        raise ValueError('writer_status_error')
    value = json.loads(output)
    if value.get('async_writes') is not True or value.get('token_file') is not True or value.get('async_active_marker') != config['active_marker']:
        raise ValueError('writer_mode_mismatch')
    counters = [value.get('categories', {}), value.get('enqueue_errors', {}).get('categories', {})]
    for categories in counters:
        for category, count in categories.items():
            if count and category != 'skipped_ungranted':
                raise ValueError(category)
    if value.get('blocked') or value.get('failed') or value.get('rejected') or value.get('enqueue_errors', {}).get('rejected'):
        raise ValueError('writer_failure')
    return value


def _watchdog(config):
    directory = Path(config['state_dir'])
    with _lock(directory / 'watchdog.lock') as lock:
        deadline = time.monotonic() + 1
        while True:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                # The arming parent briefly owns this lock while probing readiness.
                if time.monotonic() >= deadline:
                    return
                time.sleep(.002)
        nonce = secrets.token_hex(16)
        lock.seek(0);lock.truncate();lock.write(json.dumps({'pid': os.getpid(), 'nonce': nonce}));lock.flush();os.fsync(lock.fileno())
        try:
            while Path(config['active_marker']).exists():
                try:
                    if (directory / 'trip.json').exists():
                        _trip(config, 'recovery_pending')
                        if not Path(config['active_marker']).exists():
                            return
                    _reports(config)
                    _status(config)
                    for path in directory.glob('inflight-*.json'):
                        try:value = json.loads(path.read_text())
                        except FileNotFoundError:continue
                        if time.monotonic() - value['started'] > 1:
                            raise ValueError('foreground_timeout')
                        try:
                            os.kill(value['pid'], 0)
                        except ProcessLookupError:
                            raise ValueError('foreground_lost')
                    _write(directory / 'watchdog-ready.json', {'ready': True, 'pid': os.getpid(),
                           'fingerprint': config['_fingerprint'], 'nonce': nonce, 'heartbeat': time.monotonic()})
                except Exception as error:
                    category = str(error) if isinstance(error, ValueError) and str(error) in {'writer_mode_mismatch', 'writer_status_error', 'foreground_timeout', 'foreground_lost', 'writer_failure', 'lost_queue', 'http401', 'http403', 'transport', 'backend', 'cancelled', 'drain_deadline', 'queue_full', 'ipc_rejected', 'enqueue_timeout', 'enqueue_io', 'startup', 'background_failure', 'ack_failure', 'writer_failure'} else type(error).__name__
                    _trip(config, category)
                    if not Path(config['active_marker']).exists():
                        return
                time.sleep(.1)
        finally:
            _reports(config)


def arm(config, bootstrap=False):
    directory = Path(config['state_dir'])
    deadline = time.monotonic() + (2 if bootstrap else .5)
    with _lock(directory / 'arm.lock') as startup:
        owned = spawned = False
        while time.monotonic() < deadline:
            with _lock(directory / 'watchdog.lock') as lock:
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    live = False
                except BlockingIOError:
                    live = True
                ready_path = directory / 'watchdog-ready.json'
                ready = json.loads(ready_path.read_text()) if ready_path.exists() else {}
                lock.seek(0)
                try:owner = json.loads(lock.read() or '{}')
                except ValueError:owner = {}
                if live and ready.get('fingerprint') == config['_fingerprint'] and ready.get('nonce') == owner.get('nonce') and ready.get('pid') == owner.get('pid') and time.monotonic() - ready.get('heartbeat', 0) < .75:
                    return ready
            if not bootstrap:
                raise ValueError('watchdog_lost_or_stale')
            if not owned:
                try:
                    fcntl.flock(startup, fcntl.LOCK_EX | fcntl.LOCK_NB);owned = True
                except BlockingIOError:
                    pass
            if owned and not spawned and not live:
                subprocess.Popen([sys.executable, str(Path(__file__).resolve()), '--config', config['_path'], 'watchdog'],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                 close_fds=True, start_new_session=True, env={'PATH': os.defpath})
                spawned = True
            if not Path(config['active_marker']).exists():
                raise ValueError('activation_disabled')
            time.sleep(.002)
    raise TimeoutError('watchdog handshake')


def _rows(config):
    directory = Path(config['state_dir'])
    with _lock(directory / 'calls.lock') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        path = directory / 'calls.jsonl'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []


def _summary(rows):
    summary = evaluate(rows)
    summary['cohorts'] = {name: sum(r.get('cohort') == name and not r.get('warmup') for r in rows) for name in ('controlled', 'organic')}
    summary['per_cohort'] = {name: {k: v for k, v in evaluate([r for r in rows if r.get('cohort') == name]).items() if k != 'reason'} for name in ('controlled', 'organic')}
    return summary


def _reports(config):
    # Never hold the foreground append lock during a filesystem sync.
    rows = _rows(config)
    directory = Path(config['state_dir'])
    _write(directory / 'summary.json', _summary(rows))
    checkpoints = {}
    controlled = total = 0
    for index, row in enumerate(rows):
        if not row.get('warmup'):
            total += 1
            controlled += row.get('cohort') == 'controlled'
        if controlled == 50 and '50' not in checkpoints:
            checkpoints['50'] = _summary(rows[:index + 1])
        if total == 100 and '100' not in checkpoints:
            checkpoints['100'] = _summary(rows[:index + 1])
    _write(directory / 'checkpoints.json', checkpoints)


def _record(config, row):
    directory = Path(config['state_dir'])
    with _lock(directory / 'calls.lock') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        path = directory / 'calls.jsonl'
        rows = [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []
        if len(rows) >= 10000:
            raise ValueError('monitor_capacity')
        row['warmup'] = not rows
        # All fsync/report work runs in the independent watchdog, after this cheap append.
        row['tail_margin_ms'] = TAIL_MARGIN_MS
        row['latency_ms'] = round((time.monotonic() - STARTED) * 1000 + TAIL_MARGIN_MS, 3)
        _append(path, row)
        summary = evaluate([*rows, row])
    if summary['reason']:
        _trip(config, summary['reason'])
    return summary


def _hook(config, event, harness):
    global STARTED
    if not Path(config['active_marker']).exists():
        return 0
    directory = Path(config['state_dir'])
    if (directory / 'trip.json').exists():
        _trip(config, 'recovery_pending')
        return 0
    inflight = directory / ('inflight-' + str(os.getpid()) + '.json')
    report, error = {}, None
    try:
        epoch = os.getenv('HYPERAG_HOOK_STARTED_AT')
        if epoch is None:
            raise ValueError('launcher timestamp required')
        age = time.time() - float(epoch)
        if not math.isfinite(age) or not 0 <= age <= 5:raise ValueError('invalid launcher timestamp')
        STARTED = time.monotonic() - age
        arm(config)
        _write(inflight, {'pid': os.getpid(), 'started': STARTED}, durable=False)
        raw = _input(STARTED + 1)
        code, _output, stderr, raw_report = _native(config, ['hook', event, '--harness', harness], raw,
                                                   report=True, forward=True, budget=STARTED + 1 - time.monotonic())
        lines = raw_report.splitlines()
        if len(lines) != 1:
            raise ValueError('missing_or_multiple_native_report')
        report = json.loads(lines[0])
        if not isinstance(report, dict) or report.get('category') not in REPORTS or set(report) - {'category', 'scope'}:
            raise ValueError('invalid_native_report')
        if 'scope' in report and (not isinstance(report['scope'], str) or len(report['scope']) > 1024):
            raise ValueError('invalid_native_report')
        if code or stderr or report['category'] == 'error':
            error = 'native_hook_error'
        elif report['category'] == 'cancelled':
            if Path(config['active_marker']).exists():error = 'cancelled'
        elif report['category'] == 'empty_prompt' and event == 'prompt':
            pass
        elif report['category'] not in SKIPS and report['category'] != ('completed' if event == 'session-start' else 'enqueued'):
            error = 'native_mode_mismatch'
    except Exception as exception:
        error = type(exception).__name__
    latency = round((time.monotonic() - STARTED) * 1000 + TAIL_MARGIN_MS, 3)
    row = {'event': event, 'agent': harness, 'scope': report.get('scope'), 'cohort': 'controlled' if os.getenv('HYPERAG_HOOK_CHECK_COHORT') == 'controlled' else 'organic', 'latency_ms': latency, 'error': error}
    try:
        if not error and report.get('category') in SKIPS | {'cancelled'}:
            _append(directory / 'skips.jsonl', {'category': report['category']})
            if latency > 1000:_trip(config, 'single_call_limit')
        else:
            _record(config, row)
    except Exception:
        _trip(config, 'monitor_failure')
    finally:
        inflight.unlink(missing_ok=True)
    if error:
        sys.stderr.write('hyperag native guard: hook failed; recovery requested\n')
    return 0


def main(argv=None):
    os.umask(0o077)
    parser = argparse.ArgumentParser()
    parser.add_argument('--config', required=True, type=Path)
    parser.add_argument('command', choices=['arm', 'watchdog', 'hook'])
    parser.add_argument('event', nargs='?', choices=['session-start', 'prompt', 'session-end'])
    parser.add_argument('--harness', choices=['claude-code', 'codex', 'cursor', 'gemini', 'kimi'])
    args = parser.parse_args(argv)
    config = _context(args.config)
    if args.command == 'watchdog':
        _watchdog(config);return 0
    if args.command == 'arm':
        try:print(json.dumps(arm(config, bootstrap=True)));return 0
        except Exception:_trip(config, 'watchdog_startup');return 1
    if not args.event or not args.harness:
        parser.error('hook requires event and harness')
    return _hook(config, args.event, args.harness)


if __name__ == '__main__':
    raise SystemExit(main())
