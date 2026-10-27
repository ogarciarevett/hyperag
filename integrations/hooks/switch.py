#!/usr/bin/env python3
"""Forward service replacement with the shared rollback identity and named settings."""
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import time

spec = importlib.util.spec_from_file_location("rollback", Path(__file__).with_name("rollback.py"))
rollback = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rollback)


def forward(config_path):
    os.umask(0o077)
    path = Path(config_path)
    directory = path.parent
    info = directory.lstat()
    if not path.is_absolute() or not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
        raise ValueError("private switch directory required")
    lock = (directory / "switch.lock").open("a")
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    state = {"state": "running", "history": [], "started_at": time.time()}
    process, interrupted = None, False
    def phase(name, category):
        state.update(phase=name, category=category)
        state["history"].append({"phase": name, "category": category, "time": time.time()})
        rollback.write_status(directory, state, "switch-status.json")
    try:
        phase("checking", "validate_configuration")
        config = rollback.read_config(path)
        current, target = config["current"], config["target"]
        rollback.checked_launch(config["restore"])
        if config["restore"]["sha256"] != current["sha256"] or str(Path(config["restore"]["argv"][0]).resolve()) != current["binary"]:
            raise ValueError("restore must match prior service")
        environment = rollback.checked_launch(target)
        phase("identity", "verify_prior_identity")
        if rollback.identity(current["pid"]) != current:
            raise ValueError("prior service identity changed")
        restore = {"current": current, "restore": config["restore"], "health_url": config["health_url"]}
        rollback_path = directory / "rollback.json"
        rollback.write_status(directory, restore, rollback_path.name)
        phase("stopping", "stop_verified_prior")
        try:
            os.kill(current["pid"], signal.SIGTERM)
        except ProcessLookupError:
            pass
        interrupted = True
        rollback.wait_exited(current)
        phase("stopped", "prior_exited")
        phase("starting", "launch_verified_target")
        process = subprocess.Popen(target["argv"], env=environment, stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True, close_fds=True)
        state["target_pid"] = process.pid
        phase("target_identity", "verify_target_identity")
        deadline = time.monotonic() + 35
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError("target service exited")
            actual = rollback.identity(process.pid)
            if actual is None or actual["binary"] != str(Path(target["argv"][0]).resolve()) or actual["sha256"] != target["sha256"]:
                raise ValueError("target service identity mismatch")
            restore["current"] = actual
            rollback.write_status(directory, restore, rollback_path.name)
            if state["phase"] != "health":
                phase("health", "check_target_health")
            if rollback.healthy(config["health_url"]):
                if process.poll() is not None:
                    raise RuntimeError("target exited during health check")
                if rollback.identity(process.pid) != actual:
                    raise ValueError("target identity changed during health check")
                state.update(state="succeeded", health=True, target_sha256=actual["sha256"], completed_at=time.time())
                phase("complete", "target_healthy")
                return state
            time.sleep(.2)
        raise TimeoutError("target service health deadline")
    except Exception as error:
        state.update(state="failed", error_type=type(error).__name__)
        category = {"absolute process executable required": "invalid_executable_identity",
                    "process start marker changed": "process_start_changed"}.get(str(error), type(error).__name__)
        try:
            phase(state.get("phase", "checking"), category)
        except Exception:
            pass  # A reporting failure must never prevent failback.
        if interrupted:
            # This child is still ours; stop/reap it before shared rollback inspects the old PID.
            try:
                try:
                    if process is not None and process.poll() is None:
                        process.terminate()
                        try:
                            process.wait(timeout=15)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait(timeout=5)
                finally:
                    rollback.restore(directory / "rollback.json")
                state["recovery"] = "succeeded"
            except Exception as recovery_error:
                state.update(recovery="failed", recovery_category=type(recovery_error).__name__)
            try:
                rollback.write_status(directory, state, "switch-status.json")
            except Exception:
                pass
        raise
    finally:
        lock.close()


def main(arguments=None):
    try:
        forward((sys.argv[1:] if arguments is None else arguments)[0])
        return 0
    except Exception:
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
