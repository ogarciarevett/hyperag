#!/usr/bin/env python3
"""Hosted Linux regression using two real service builds and an owned database."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.request
import uuid

DIRECTORY = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("rollback", DIRECTORY / "rollback.py")
rollback = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rollback)



# Feed the production Darwin identity branch metadata from an actual Linux service.
# The adapter stops only the owned fixture to force its exit between stat and comm.
DARWIN_METADATA_ADAPTER = r'''import importlib.util,json,os,signal,subprocess,sys,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('rollback',@MODULE@)
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
current=@CURRENT_PID@
actual_run=subprocess.run
actual_kill=os.kill
armed=False

def fields(pid):
    try:return (Path('/proc')/str(pid)/'stat').read_text().rsplit(')',1)[1].split()
    except FileNotFoundError:return None

def kill(pid, signum):
    global armed
    if pid==current and signum==signal.SIGTERM:
        actual_kill(pid,signal.SIGSTOP)
        deadline=time.monotonic()+5
        while (value:=fields(pid)) is not None and value[0]!='T':
            if time.monotonic()>deadline:raise TimeoutError('fixture stop metadata')
            time.sleep(.01)
        actual_kill(pid,signum);armed=True
    else:actual_kill(pid,signum)

def metadata(command, **kwargs):
    global armed
    if command[0]!='/bin/ps':return actual_run(command,**kwargs)
    pid=int(command[2]);field=command[-1];value=fields(pid)
    if value is None:return subprocess.CompletedProcess(command,1,stdout='')
    if field=='stat=':text=value[0]
    elif field=='lstart=':text=value[19]
    elif field=='comm=':
        if pid==current and armed:
            armed=False;actual_kill(pid,signal.SIGCONT)
            deadline=time.monotonic()+15
            while (value:=fields(pid)) is not None and value[0]!='Z':
                if time.monotonic()>deadline:raise TimeoutError('fixture service did not exit')
                time.sleep(.01)
            Path(@RACE_PROOF@).write_text(json.dumps({'executable_metadata':'defunct'}))
            text='<defunct>'
        else:text=str((Path('/proc')/str(pid)/'exe').resolve())
    else:raise AssertionError('unexpected metadata field')
    return subprocess.CompletedProcess(command,0,stdout=text)

module.sys.platform='darwin'
module.subprocess.run=metadata
module.os.kill=kill
raise SystemExit(module.main(sys.argv[1:]))
'''

@unittest.skipUnless(sys.platform == "linux" and os.getenv("GITHUB_ACTIONS") == "true", "hosted Linux proof only")
class ServiceRecoveryTest(unittest.TestCase):
    def test_old_service_restored_after_hook_process_group_deadline(self):
        self.assert_real_service_recovery(force_exit_race=False)

    def test_macos_metadata_exit_race_restores_prior_service(self):
        self.assert_real_service_recovery(force_exit_race=True)

    def assert_real_service_recovery(self, force_exit_race):
        current = Path(os.environ["HYPERAG_RECOVERY_CURRENT_BIN"]).resolve()
        old = Path(os.environ["HYPERAG_RECOVERY_OLD_BIN"]).resolve()
        self.assertNotEqual(rollback.binary_hash(current), rollback.binary_hash(old))
        database = "hooks_recovery_" + uuid.uuid4().hex
        pg = "postgresql://hyperag@127.0.0.1:55432/hyperag"
        def sql(statement):
            subprocess.run(["psql", pg, "-X", "-q", "-v", "ON_ERROR_STOP=1", "-c", statement],
                           check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                           env={"PATH": os.defpath, "PGPASSWORD": "fake-test-only"})
        sql("CREATE DATABASE " + database)
        try:
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                with socket.socket() as listener:
                    listener.bind(("127.0.0.1", 0))
                    port = listener.getsockname()[1]
                self.assertNotIn(port, (7481, 7482))
                health_url = f"http://127.0.0.1:{port}/healthz"
                tokens = root / "tokens.json"
                tokens.write_text("{}");tokens.chmod(0o600)
                environment = {"HYPERAG_PG_URL": f"postgresql://hyperag:fake-test-only@127.0.0.1:55432/{database}",
                               "HYPERAG_TOKENS_FILE": str(tokens), "HYPERAG_EMBEDDING": "none",
                               "HYPERAG_RERANKER": "none", "HYPERAG_LEXICAL_SCORER": "bm25"}
                argv = ["serve", "--host", "127.0.0.1", "--port", str(port)]
                current_environment = dict(environment, HOME=str(root), HYPERAG_LEXICAL_SCORER="tantivy",
                                           HYPERAG_LEXICAL_INDEX_ROOT=str(root / "index"))
                service = subprocess.Popen([str(current), *argv], env=current_environment, stdin=subprocess.DEVNULL,
                                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
                hook = None
                def healthy():
                    try:
                        with urllib.request.urlopen(health_url, timeout=.3) as response:
                            return json.load(response).get("ok") is True
                    except OSError:
                        return False
                def wait_for(predicate, timeout=40, message="isolated service recovery checkpoint timed out"):
                    deadline = time.monotonic() + timeout
                    while time.monotonic() < deadline:
                        status_file = root / "rollback-status.json"
                        if status_file.exists():
                            state = json.loads(status_file.read_text())
                            if state.get("state") == "failed":
                                self.fail("rollback failed: " + state.get("error_type", "unknown") + " phase=" + state.get("phase", "unknown"))
                        if predicate():return
                        time.sleep(.05)
                    self.fail(message)
                try:
                    wait_for(healthy)
                    config = {"current": rollback.identity(service.pid), "restore": {
                        "argv": [str(old), *argv], "sha256": rollback.binary_hash(old), "env": environment},
                        "health_url": health_url, "restart_delay_seconds": 5}
                    command = [sys.executable, str(DIRECTORY / "rollback.py")]
                    if force_exit_race:
                        config["current"]["start_ticks"] = "darwin:" + config["current"]["start_ticks"]
                        adapter = root / "darwin-metadata.py"
                        adapter.write_text(DARWIN_METADATA_ADAPTER.replace("@MODULE@", repr(str(DIRECTORY / "rollback.py")))
                                           .replace("@CURRENT_PID@", str(service.pid)).replace("@RACE_PROOF@", repr(str(root / "exit-race.json"))))
                        command = [sys.executable, str(adapter)]
                    path = root / "rollback.json";path.write_text(json.dumps(config));path.chmod(0o600)
                    fixture = root / "fixture";fixture.mkdir()
                    credential = root / "credential.json"
                    credential.write_text(json.dumps({"token": "invalid-test-only", "read_scopes": ["project:fixture"],
                                                      "write_scopes": ["project:fixture"]}));credential.chmod(0o600)
                    active = root / "active";active.touch()
                    guard_config = root / "guard.json"
                    guard_config.write_text(json.dumps({"binary": str(current), "credential_file": str(credential),
                        "active_marker": str(active), "state_dir": str(root / "guard"), "url": health_url.removesuffix("/healthz"),
                        "rollback_command": [*command, str(path)]}))
                    guard_config.chmod(0o600)
                    driver = root / "hook.py"
                    driver.write_text(f"""import importlib.util,time
spec=importlib.util.spec_from_file_location('guard',{str(DIRECTORY / 'guard.py')!r})
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
module.main(['--config',{str(guard_config)!r},'hook','prompt','--harness','claude-code'])
time.sleep(60)
""")
                    hook = subprocess.Popen([sys.executable, str(driver)], start_new_session=True,
                                            env={"PATH": os.defpath, "HOME": str(root)},
                                            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    hook.stdin.write(json.dumps({"cwd": str(fixture), "prompt": "controlled permission check"}).encode())
                    hook.stdin.close()
                    status_path = root / "rollback-status.json"
                    def status():
                        return json.loads(status_path.read_text()) if status_path.exists() else {}
                    wait_for(lambda: status().get("state") == "stopped")
                    self.assertFalse(healthy())
                    self.assertFalse(active.exists())
                    trip = json.loads((root / "guard" / "trips.jsonl").read_text().splitlines()[0])
                    self.assertEqual(trip["category"], "http_401")
                    time.sleep(3.1)
                    self.assertEqual(status()["state"], "stopped")
                    os.killpg(hook.pid, signal.SIGKILL);hook.wait(timeout=2)
                    wait_for(lambda: status().get("state") == "succeeded", message="prior service not restored after hook timeout")
                    result = status()
                    self.assertTrue(healthy())
                    self.assertEqual(rollback.identity(result["restored_pid"])["sha256"], rollback.binary_hash(old))
                    self.assertEqual(rollback.identity(result["restored_pid"])["binary"], str(old))
                    self.assertGreater(result["completed_at"] - result["stopped_at"], 3)
                    if force_exit_race:
                        self.assertEqual(json.loads((root / "exit-race.json").read_text())["executable_metadata"], "defunct")
                    print(json.dumps({"test_port": port, "prior_commit": "a42ad2c6e7ad137b6b113bcfb12978fd18ccebf5",
                        "restored_binary_sha256": result["restored_sha256"], "health": True,
                        "recovery_after_stop_seconds": round(result["completed_at"] - result["stopped_at"], 3),
                        "hook_process_group_killed": True, "forced_macos_exit_race": force_exit_race}))
                finally:
                    if hook is not None and hook.poll() is None:
                        os.killpg(hook.pid, signal.SIGKILL);hook.wait(timeout=2)
                    status_path = root / "rollback-status.json"
                    if status_path.exists():
                        restored_pid = json.loads(status_path.read_text()).get("restored_pid")
                        if restored_pid:
                            actual = rollback.identity(restored_pid)
                            if actual and actual["sha256"] == rollback.binary_hash(old):
                                os.killpg(restored_pid, signal.SIGTERM)
                                wait_for(lambda: rollback.identity(restored_pid) is None, timeout=15)
                    if service.poll() is None:
                        os.killpg(service.pid, signal.SIGTERM)
                    service.wait(timeout=15)
        finally:
            sql("DROP DATABASE " + database + " WITH (FORCE)")


if __name__ == "__main__":
    unittest.main()
