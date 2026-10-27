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
# Stop only the owned fixture. The 100ms invalid-executable/non-Z window is synthetic;
# the authorized macOS probe observed Z then absence, not this invalid-path window.
DARWIN_METADATA_ADAPTER = r'''import importlib.util,json,os,signal,subprocess,sys,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('rollback',@MODULE@)
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
current=@CURRENT_PID@
actual_run=subprocess.run
actual_kill=os.kill
armed=False
late_race=@LATE_RACE@
exit_window=0
live_start=None
post_signal_comm_reads=0
stubborn_target=@STUBBORN_TARGET@
target_pid=None
target_term_at=None

def fields(pid):
    try:return (Path('/proc')/str(pid)/'stat').read_text().rsplit(')',1)[1].split()
    except FileNotFoundError:return None

def kill(pid, signum):
    global armed,target_term_at
    if pid==current and signum==signal.SIGTERM:
        actual_kill(pid,signal.SIGSTOP)
        deadline=time.monotonic()+5
        while (value:=fields(pid)) is not None and value[0]!='T':
            if time.monotonic()>deadline:raise TimeoutError('fixture stop metadata')
            time.sleep(.01)
        actual_kill(pid,signum);armed=True
    else:
        if pid==target_pid and signum==signal.SIGTERM:
            actual_kill(pid,signal.SIGSTOP);target_term_at=time.monotonic()
        if pid==target_pid and signum==signal.SIGKILL:
            Path(@TARGET_PROOF@).write_text(json.dumps({'pid':pid,'sigkill':True,'stopped':fields(pid)[0]=='T','term_wait_seconds':time.monotonic()-target_term_at}))
        actual_kill(pid,signum)

def metadata(command, **kwargs):
    global armed,exit_window,live_start,post_signal_comm_reads
    if command[0]!='/bin/ps':return actual_run(command,**kwargs)
    pid=int(command[2]);field=command[-1];value=fields(pid)
    if pid==current and armed and field=='lstart=':
        armed=False;live_start=value[19];actual_kill(pid,signal.SIGCONT)
        exit_window=time.monotonic()+.1
        Path(@RACE_PROOF@).write_text(json.dumps({'synthetic_exit_window':True,'post_signal_comm_reads':0}))
    if pid==current and time.monotonic()<exit_window:
        if field=='stat=':text='S'
        elif field=='lstart=':text=live_start
        elif field=='comm=':
            post_signal_comm_reads+=1;text='<defunct>'
            Path(@RACE_PROOF@).write_text(json.dumps({'synthetic_exit_window':True,'post_signal_comm_reads':post_signal_comm_reads}))
        else:raise AssertionError('unexpected metadata field')
    elif value is None:return subprocess.CompletedProcess(command,1,stdout='')
    elif field=='stat=':text=value[0]
    elif field=='lstart=':text=value[19]
    elif field=='comm=':text=str((Path('/proc')/str(pid)/'exe').resolve())
    else:raise AssertionError('unexpected metadata field')
    return subprocess.CompletedProcess(command,0,stdout=text)

module.sys.platform='darwin'
module.subprocess.run=metadata
module.os.kill=kill
if late_race:
    spec=importlib.util.spec_from_file_location('switch',Path(@MODULE@).with_name('switch.py'))
    switch=importlib.util.module_from_spec(spec);spec.loader.exec_module(switch)
    switch.rollback=module
    if stubborn_target:
        actual_healthy=module.healthy
        def healthy(url):
            global target_pid
            result=actual_healthy(url)
            if result and target_pid is None:
                target_pid=json.loads(Path(sys.argv[1]).with_name('switch-status.json').read_text())['target_pid']
                raise RuntimeError('fixture target health failure')
            return result
        module.healthy=healthy
    raise SystemExit(switch.main(sys.argv[1:]))
raise SystemExit(module.main(sys.argv[1:]))
'''

@unittest.skipUnless(sys.platform == "linux" and os.getenv("GITHUB_ACTIONS") == "true", "hosted Linux proof only")
class ServiceRecoveryTest(unittest.TestCase):
    def test_old_service_restored_after_hook_process_group_deadline(self):
        self.assert_real_service_recovery(force_exit_race=False)

    def test_macos_metadata_exit_race_restores_prior_service(self):
        self.assert_real_service_recovery(force_exit_race=True)

    def test_forward_switch_tolerates_late_defunct_metadata(self):
        self.assert_real_service_recovery(force_exit_race=True, forward=True)

    def test_forward_switch_restores_prior_when_target_cannot_handle_sigterm(self):
        self.assert_real_service_recovery(force_exit_race=True, forward=True, stubborn_target=True)

    def assert_real_service_recovery(self, force_exit_race, forward=False, stubborn_target=False):
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
                service = subprocess.Popen([str(old if forward else current), *argv], env=environment if forward else current_environment, stdin=subprocess.DEVNULL,
                                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
                hook = None
                def healthy():
                    try:
                        with urllib.request.urlopen(health_url, timeout=.3) as response:
                            return json.load(response).get("ok") is True
                    except OSError:
                        return False
                def wait_for(predicate, timeout=40, message="isolated service recovery checkpoint timed out", check_failure=True):
                    deadline = time.monotonic() + timeout
                    while time.monotonic() < deadline:
                        status_file = root / ("switch-status.json" if forward else "rollback-status.json")
                        if check_failure and status_file.exists():
                            state = json.loads(status_file.read_text())
                            if state.get("state") == "failed":
                                self.fail(("switch" if forward else "rollback") + " failed: " + state.get("error_type", state.get("category", "unknown")) + " phase=" + state.get("phase", "unknown"))
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
                                           .replace("@CURRENT_PID@", str(service.pid)).replace("@RACE_PROOF@", repr(str(root / "exit-race.json")))
                                           .replace("@LATE_RACE@", repr(forward)).replace("@STUBBORN_TARGET@", repr(stubborn_target))
                                           .replace("@TARGET_PROOF@", repr(str(root / "target-killed.json"))))
                        command = [sys.executable, str(adapter)]
                    if forward:
                        config["target"] = {"argv": [str(current), *argv], "sha256": rollback.binary_hash(current), "env": current_environment}
                    path = root / ("switch.json" if forward else "rollback.json");path.write_text(json.dumps(config));path.chmod(0o600)
                    if forward:
                        result = subprocess.run([*command, str(path)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=70 if stubborn_target else 40)
                        status = json.loads((root / "switch-status.json").read_text())
                        if stubborn_target:
                            self.assertEqual(result.returncode, 1)
                            self.assertEqual((status['state'], status['phase'], status['error_type'], status['recovery']),
                                             ('failed', 'health', 'RuntimeError', 'succeeded'))
                            proof = json.loads((root / 'target-killed.json').read_text())
                            self.assertEqual((proof['sigkill'], proof['stopped']), (True, True))
                            self.assertGreaterEqual(proof['term_wait_seconds'], 14.5)
                            self.assertIsNone(rollback.identity(proof['pid']))
                            restored = json.loads((root / 'rollback-status.json').read_text())
                            self.assertEqual(restored['state'], 'succeeded')
                            self.assertEqual(rollback.identity(restored['restored_pid'])['sha256'], rollback.binary_hash(old))
                            self.assertEqual(rollback.identity(restored['restored_pid'])['binary'], str(old))
                            self.assertIsNone(rollback.identity(service.pid))
                            self.assertTrue(healthy())
                            self.assertTrue(any(entry['phase'] == 'health' and entry['category'] == 'RuntimeError' for entry in status['history']))
                            return
                        self.assertEqual(result.returncode, 0, "switch failed: " + status.get("error_type", status.get("category", "unknown")) + " phase=" + status.get("phase", "unknown"))
                        self.assertEqual(status["state"], "succeeded")
                        self.assertTrue(healthy())
                        self.assertIsNone(rollback.identity(service.pid))
                        target = json.loads((root / "rollback.json").read_text())["current"]
                        self.assertEqual((rollback.identity(target["pid"])["sha256"], rollback.identity(target["pid"])["binary"]),
                                         (rollback.binary_hash(current), str(current)))
                        self.assertEqual((status['target_pid'], status['target_sha256']), (target['pid'], target['sha256']))
                        self.assertEqual(json.loads((root / "exit-race.json").read_text())["post_signal_comm_reads"], 0)
                        phases = [entry['phase'] if isinstance(entry, dict) else entry for entry in status['history']]
                        required = ['stopping', 'stopped', 'starting', 'target_identity', 'health', 'complete']
                        self.assertEqual([phase for phase in phases if phase in required], required)
                        print(json.dumps({"test_port": port, "forward_switch": True, "health": True,
                                          "synthetic_exit_window": True, "target_sha256": target["sha256"], "old_exited": True}))
                        return
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
                        self.assertEqual(json.loads((root / "exit-race.json").read_text())["post_signal_comm_reads"], 0)
                    print(json.dumps({"test_port": port, "prior_commit": "a42ad2c6e7ad137b6b113bcfb12978fd18ccebf5",
                        "restored_binary_sha256": result["restored_sha256"], "health": True,
                        "recovery_after_stop_seconds": round(result["completed_at"] - result["stopped_at"], 3),
                        "hook_process_group_killed": True, "forced_macos_exit_race": force_exit_race}))
                finally:
                    if hook is not None and hook.poll() is None:
                        os.killpg(hook.pid, signal.SIGKILL);hook.wait(timeout=2)
                    status_path = root / "rollback-status.json"
                    if forward and (root / "rollback.json").exists():
                        target = json.loads((root / "rollback.json").read_text())["current"]
                        actual = rollback.identity(target["pid"])
                        if actual and actual["sha256"] == rollback.binary_hash(current):
                            os.killpg(target["pid"], signal.SIGKILL if stubborn_target else signal.SIGTERM)
                            wait_for(lambda: rollback.identity(target["pid"]) is None, timeout=15, check_failure=False)
                    if status_path.exists():
                        restored_pid = json.loads(status_path.read_text()).get("restored_pid")
                        if restored_pid:
                            actual = rollback.identity(restored_pid)
                            if actual and actual["sha256"] == rollback.binary_hash(old):
                                os.killpg(restored_pid, signal.SIGTERM)
                                wait_for(lambda: rollback.identity(restored_pid) is None, timeout=15, check_failure=False)
                    if service.poll() is None:
                        os.killpg(service.pid, signal.SIGTERM)
                    service.wait(timeout=15)
        finally:
            sql("DROP DATABASE " + database + " WITH (FORCE)")


if __name__ == "__main__":
    unittest.main()
