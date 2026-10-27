#!/usr/bin/env python3
"""Run a trusted local recovery command outside a harness hook's process group."""
import fcntl
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time


def _status(directory, value):
    with tempfile.NamedTemporaryFile(mode="w", dir=directory, delete=False) as file:
        temporary = Path(file.name)
        try:
            os.fchmod(file.fileno(), 0o600)
            json.dump(value, file)
            file.flush()
            os.fsync(file.fileno())
            temporary.replace(directory / "status.json")
        finally:
            temporary.unlink(missing_ok=True)


def dispatch(command: list[str], state_dir: Path) -> int:
    """Return the detached worker PID promptly. Arguments must contain no secrets.

    The caller disables hooks before dispatch and supplies a trusted, private state
    directory. The command must use absolute paths and load its own named settings.
    """
    if not command or not all(isinstance(value, str) for value in command) or not Path(command[0]).is_absolute():
        raise ValueError("recovery command requires an absolute executable")
    directory = Path(state_dir).absolute()
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    child = subprocess.Popen(
        [sys.executable, str(Path(__file__).resolve()), "--worker", str(directory), "--", *command],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True, close_fds=True, env={"PATH": os.defpath},
    )
    # Reap in long-lived callers without keeping the hook alive.
    threading.Thread(target=child.wait, daemon=True).start()
    return child.pid


def status_for(state_dir: Path, worker_pid: int):
    """Return this worker's status; missing or another attempt's status is unknown."""
    try:
        value = json.loads((Path(state_dir) / "status.json").read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return None
    return value if isinstance(value, dict) and value.get("pid") == worker_pid else None


def dispatch_confirmed(command: list[str], state_dir: Path, timeout=1.0) -> int:
    """Confirm this detached attempt published status, never another worker's status.

    Confirmation is not restoration success. A suppressed single-flight contender
    remains unconfirmed; timeout leaves its worker untouched for the caller's watch.
    """
    if isinstance(timeout, bool) or not isinstance(timeout, (int, float)) or not math.isfinite(timeout) or timeout < 0:
        raise ValueError("confirmation timeout must be finite and nonnegative")
    pid = dispatch(command, state_dir)
    deadline = time.monotonic() + timeout
    while True:
        status = status_for(state_dir, pid)
        if status is not None and status.get("state") in {"running", "succeeded", "failed"}:
            return pid
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            error = TimeoutError("recovery acknowledgement timed out")
            error.worker_pid = pid
            raise error
        time.sleep(min(.01, remaining))


def _worker(command, directory):
    os.umask(0o077)
    with (directory / "recovery.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return 0
        state = {"state": "running", "pid": os.getpid(), "started_at": time.time()}
        _status(directory, state)
        try:
            result = subprocess.run(
                command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL, close_fds=True, env={"PATH": os.defpath},
            )
            state.update(state="succeeded" if result.returncode == 0 else "failed", exit_code=result.returncode)
        except Exception as error:
            state.update(state="failed", error_type=type(error).__name__)
        state["completed_at"] = time.time()
        _status(directory, state)
        return 0 if state["state"] == "succeeded" else 1


if __name__ == "__main__":
    if len(sys.argv) < 5 or sys.argv[1] != "--worker" or sys.argv[3] != "--":
        raise SystemExit("use dispatch(command, state_dir) from a local guard")
    raise SystemExit(_worker(sys.argv[4:], Path(sys.argv[2])))
