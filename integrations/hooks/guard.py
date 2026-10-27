"""Scope checks shared by local hook guards; no credentials or input are logged."""
import os
from pathlib import Path
import subprocess


def scope_for(cwd: Path) -> str:
    directory = Path(cwd)
    root = next((path for path in [directory, *directory.parents] if (path / ".git").exists()), None)
    name = (root or directory).name
    if root is not None:
        try:
            result = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "--path-format=absolute", "--git-common-dir"],
                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                check=True, timeout=1, env={"PATH": os.defpath},
            )
            common = Path(result.stdout.strip())
            if common.name == ".git":
                name = common.parent.name
        except (OSError, subprocess.SubprocessError):
            pass
    return "project:" + (name.lower() or "unknown")


def check_scope(scope: str, credential: dict, log) -> bool:
    if not scope.startswith("project:"):
        raise ValueError("hook scope must be a project scope")
    if scope in credential["write_scopes"] and scope in credential["read_scopes"]:
        return True
    log({"category": "skipped_ungranted"})
    return False


def _append(path, value):
    import fcntl
    import json
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with path.open('a') as file:
        fcntl.flock(file, fcntl.LOCK_EX)
        file.write(json.dumps(value) + '\n')


def _input(deadline):
    import select
    import time
    raw = b''
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([0], [], [], remaining)[0]:
            raise TimeoutError('hook input deadline')
        chunk = os.read(0, min(65_536, 1_048_577 - len(raw)))
        if not chunk:
            return raw
        raw += chunk
        if len(raw) > 1_048_576:
            raise ValueError('hook input too large')


def run_hook(config, event, harness):
    """Production guard entrypoint: skip before invoking the hook or recovery."""
    import importlib.util
    import json
    import math
    import stat
    import time
    started = time.monotonic()
    active = Path(config['active_marker'])
    if not active.exists():
        return 0
    os.umask(0o077)
    directory = Path(config['state_dir'])
    error = None
    scope = None
    try:
        path = Path(config['credential_file'])
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) != 0o600 or info.st_uid != os.getuid():
            raise PermissionError('credential file permissions')
        credential = json.loads(path.read_text())
        raw = _input(started + 1.5)
        value = json.loads(raw) if raw.strip() else {}
        if not isinstance(value, dict):
            raise ValueError('hook input must be an object')
        cwd = value.get('cwd')
        if cwd is None and harness == 'cursor':
            roots = value.get('workspace_roots', [])
            cwd = roots[0] if roots else None
        scope = os.getenv('HYPERAG_SCOPE') or scope_for(Path(cwd) if isinstance(cwd, str) else Path.cwd())
        if not check_scope(scope, credential, lambda row: _append(directory / 'skips.jsonl', row)):
            return 0
        if not active.exists():
            return 0
        token_env = config.get('token_env', 'HYPERAG_TOKEN')
        if token_env not in ('HYPERAG_TOKEN', 'HYPERAG_HOOKS_TOKEN'):
            raise ValueError('unsupported hook token variable')
        os.environ[token_env] = credential['token']
        os.environ['HYPERAG_URL'] = config['url']
        os.environ['HYPERAG_SCOPE'] = scope
        try:
            result = subprocess.run(
                [config['binary'], 'hook', event, '--harness', harness], input=raw,
                stderr=subprocess.PIPE, timeout=max(.05, started + 2 - time.monotonic()),
            )
        finally:
            os.environ.pop(token_env, None)
        if result.returncode or result.stderr:
            text = result.stderr.decode(errors='replace')
            error = next((category for phrase, category in [
                ('service answered 401', 'http_401'), ('service answered 403', 'http_403'),
                ('service answered 404', 'http_404'), ('error sending request', 'transport'),
                ('timed out', 'timeout'),
            ] if phrase in text), 'hook_failure')
    except Exception as exception:
        error = type(exception).__name__
    latency = round((time.monotonic() - started) * 1000, 3)
    cohort = 'controlled' if os.getenv('HYPERAG_HOOK_CHECK_COHORT') == 'controlled' else 'organic'
    row = {'event': event, 'agent': harness, 'scope': scope, 'cohort': cohort,
           'latency_ms': latency, 'error': error}
    try:
        import fcntl
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        with (directory / 'monitor.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            _append(directory / 'calls.jsonl', row)
            calls = [json.loads(line) for line in (directory / 'calls.jsonl').read_text().splitlines()]
            # Keep the rolling cohort window in call order before sorting its latencies.
            window = [call['latency_ms'] for call in calls if call['cohort'] == cohort][-50:]
            p95 = sorted(window)[math.ceil(.95 * len(window)) - 1]
            trip = error or ('p95_over_300ms' if p95 > 300 else None)
    except Exception:
        trip = 'monitor_failure'
    if trip:
        active.unlink(missing_ok=True)
        _append(directory / 'trips.jsonl', {'category': trip})
        spec = importlib.util.spec_from_file_location('recovery', Path(__file__).with_name('recovery.py'))
        recovery = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(recovery)
        recovery.dispatch(config['rollback_command'], directory / 'recovery')
        if error:
            import sys
            sys.stderr.write('hyperag hook failed; recovery dispatched\n')
    return 0


def main(argv=None):
    import argparse
    import json
    import sys
    parser = argparse.ArgumentParser(description='Guard hook calls with a private configuration file.')
    parser.add_argument('--config', required=True, type=Path)
    parser.add_argument('command', choices=['hook'])
    parser.add_argument('event', choices=['session-start', 'prompt', 'session-end'])
    parser.add_argument('--harness', required=True, choices=['claude-code', 'codex', 'cursor', 'gemini', 'kimi'])
    args = parser.parse_args(argv)
    try:
        config = json.loads(args.config.read_text())
        return run_hook(config, args.event, args.harness)
    except Exception as exception:
        sys.stderr.write('hyperag guard unavailable: ' + type(exception).__name__ + '\n')
        return 0


if __name__ == '__main__':
    raise SystemExit(main())
