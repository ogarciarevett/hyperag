#!/usr/bin/env python3
"""Identity-checked service rollback. Configuration is an owner-only JSON file."""
import hashlib
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request

SERVICE_VARIABLES = (
    "HOME", "HYPERAG_PG_URL", "HYPERAG_TOKENS_FILE", "HYPERAG_ADMINS",
    "HYPERAG_EMBEDDING", "HYPERAG_RERANKER", "HYPERAG_RERANK_PROVIDER",
    "HYPERAG_RERANK_CANDIDATES", "HYPERAG_LEXICAL_SCORER", "HYPERAG_LEXICAL_INDEX_ROOT",
    "HYPERAG_NEO4J_URL", "HYPERAG_NEO4J_USER", "HYPERAG_NEO4J_PASSWORD",
    "HYPERAG_MODEL_THREADS", "HYPERAG_THREADS", "TOKIO_WORKER_THREADS", "RUST_LOG",
)


def binary_hash(path):
    with Path(path).open("rb") as source:
        digest = hashlib.sha256()
        for chunk in iter(lambda: source.read(1048576), b""):
            digest.update(chunk)
        return digest.hexdigest()


def identity(pid):
    """Executable identity/start marker, using metadata only; never process arguments."""
    if sys.platform == "darwin":
        def metadata(field):
            result = subprocess.run(["/bin/ps", "-p", str(pid), "-o", field + "="],
                                    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                                    env={"PATH": os.defpath, "LC_ALL": "C", "TZ": "UTC"})
            if result.returncode == 1:
                return None
            if result.returncode or not result.stdout.strip():
                raise OSError("process metadata unavailable")
            return result.stdout.strip()
        process_state = metadata("stat")
        if process_state is None or process_state.startswith("Z"):
            return None
        started, executable = metadata("lstart"), metadata("comm")
        if started is None or executable is None:
            return None
        def same_live_process():
            latest_state = metadata("stat")
            if latest_state is None or latest_state.startswith("Z"):
                return False
            latest_start = metadata("lstart")
            if latest_start is None:
                return False
            if latest_start != started:
                raise ValueError("process start marker changed")
            return True
        # ps reads are separate snapshots. A terminating process can become defunct
        # after the first state read; that is absence, not an invalid live executable.
        if not same_live_process():
            return None
        if not Path(executable).is_absolute():
            final_state = metadata("stat")
            if final_state is None or final_state.startswith("Z"):
                return None
            raise ValueError("absolute process executable required")
        try:
            digest = binary_hash(executable)
        except OSError:
            if not same_live_process():
                return None
            raise
        if not same_live_process():
            return None
        return {"pid": pid, "binary": str(Path(executable).resolve()),
                "sha256": digest, "start_ticks": "darwin:" + started}
    if sys.platform != "linux":
        raise NotImplementedError("process identity unsupported on this platform")
    try:
        directory = Path("/proc") / str(pid)
        fields = (directory / "stat").read_text().rsplit(")", 1)[1].split()
        if fields[0] == "Z":
            return None
        executable = directory / "exe"
        return {"pid": pid, "binary": str(executable.resolve()),
                "sha256": binary_hash(executable), "start_ticks": fields[19]}
    except FileNotFoundError:
        return None


def healthy(url):
    try:
        with urllib.request.urlopen(url, timeout=.3) as response:
            return response.status == 200 and json.load(response).get("ok") is True
    except (OSError, ValueError):
        return False


def write_status(directory, value, name="rollback-status.json"):
    with tempfile.NamedTemporaryFile(mode="w", dir=directory, delete=False) as file:
        temporary = Path(file.name)
        try:
            os.fchmod(file.fileno(), 0o600)
            json.dump(value, file)
            file.flush()
            os.fsync(file.fileno())
            temporary.replace(directory / name)
        finally:
            temporary.unlink(missing_ok=True)


def read_config(config_path):
    config_path = Path(config_path)
    info = config_path.lstat()
    if not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) != 0o600 or info.st_uid != os.getuid():
        raise ValueError("private configuration required")
    config = json.loads(config_path.read_text())
    url = urllib.parse.urlsplit(config["health_url"])
    if url.scheme != "http" or url.hostname != "127.0.0.1" or not url.port or url.path != "/healthz":
        raise ValueError("loopback health endpoint required")
    if url.username or url.password or url.query or url.fragment:
        raise ValueError("invalid health endpoint")
    return config


def checked_launch(launch):
    if not isinstance(launch["argv"], list) or not launch["argv"] or not all(isinstance(value, str) for value in launch["argv"]) or not Path(launch["argv"][0]).is_absolute():
        raise ValueError("absolute launch executable required")
    if set(launch["env"]) - set(SERVICE_VARIABLES) or not all(isinstance(v, str) for v in launch["env"].values()):
        raise ValueError("only named service variables are accepted")
    if binary_hash(launch["argv"][0]) != launch["sha256"]:
        raise ValueError("launch binary identity mismatch")
    return {name: launch["env"][name] for name in SERVICE_VARIABLES if name in launch["env"]}


def restore(config_path):
    config_path = Path(config_path)
    config = read_config(config_path)
    current, old = config["current"], config["restore"]
    environment = checked_launch(old)
    delay = float(config.get("restart_delay_seconds", 0))
    if not 0 <= delay <= 30:
        raise ValueError("invalid restore delay")
    state = {"state": "stopping", "started_at": time.time()}
    write_status(config_path.parent, state)
    actual_current = identity(current["pid"])
    if actual_current is None:
        # A prior attempt may have stopped the service before its restarter was killed.
        if healthy(config["health_url"]):
            raise ValueError("healthy endpoint has no verified process owner")
    elif actual_current != current:
        state["identity_changed_keys"] = sorted(key for key in current.keys() | actual_current.keys()
                                                if current.get(key) != actual_current.get(key))
        write_status(config_path.parent, state)
        raise ValueError("current process identity changed")
    else:
        os.kill(current["pid"], signal.SIGTERM)
    deadline = time.monotonic() + 15
    while identity(current["pid"]) is not None:
        if time.monotonic() >= deadline:
            raise TimeoutError("current service did not stop")
        time.sleep(.05)
    state.update(state="stopped", stopped_at=time.time())
    write_status(config_path.parent, state)
    # A configured holdoff also exercises recovery across harness deadlines in CI.
    time.sleep(delay)
    service = subprocess.Popen(old["argv"], env=environment, stdin=subprocess.DEVNULL,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                               start_new_session=True, close_fds=True)
    state.update(state="starting", restored_pid=service.pid)
    write_status(config_path.parent, state)
    deadline = time.monotonic() + 35
    while time.monotonic() < deadline:
        if service.poll() is not None:
            raise RuntimeError("restore process exited")
        actual = identity(service.pid)
        if actual is None or actual["binary"] != str(Path(old["argv"][0]).resolve()) or actual["sha256"] != old["sha256"]:
            raise ValueError("restored process identity mismatch")
        if healthy(config["health_url"]) and service.poll() is None:
            state.update(state="succeeded", health=True, restored_sha256=actual["sha256"], completed_at=time.time())
            write_status(config_path.parent, state)
            return
        time.sleep(.05)
    raise TimeoutError("restored service health timed out")


def main(arguments=None):
    os.umask(0o077)
    path = Path((sys.argv[1:] if arguments is None else arguments)[0])
    try:
        restore(path)
    except Exception as error:
        status_path = path.parent / "rollback-status.json"
        state = json.loads(status_path.read_text()) if status_path.exists() else {}
        state.update(phase=state.get("state", "config"), state="failed", error_type=type(error).__name__, completed_at=time.time())
        write_status(path.parent, state)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
