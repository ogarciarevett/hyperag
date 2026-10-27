"""Black-box hook contract. Only synthetic profiles and a loopback stub service."""
import json
import os
from pathlib import Path
import subprocess
import shutil
import tempfile
import threading
import tomllib
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BIN = str(Path(os.environ.get('HYPERAG_BIN', 'target/debug/hyperag')).resolve())
FIXTURES = Path(__file__).parent / 'fixtures/hooks'
TARGETS = {
    'claude-code': ('.claude/settings.json', ['SessionStart', 'UserPromptSubmit', 'SessionEnd']),
    'codex': ('.codex/config.toml', ['SessionStart', 'UserPromptSubmit', 'SessionEnd']),
    'cursor': ('.cursor/hooks.json', ['sessionStart', 'beforeSubmitPrompt', 'sessionEnd']),
    'gemini': ('.gemini/settings.json', ['SessionStart', 'BeforeAgent', 'SessionEnd']),
    'kimi': ('.kimi-code/config.toml', ['SessionStart', 'UserPromptSubmit', 'SessionEnd']),
}

class Hooks(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='hyperag-hooks-')
        self.home = Path(self.tmp.name)
        self.env = {name: value for name in ('PATH', 'TMPDIR', 'SystemRoot') if (value := os.getenv(name)) is not None}
        self.env.update(HOME=str(self.home), XDG_CONFIG_HOME=str(self.home / '.config'))
        for rel, _ in TARGETS.values():
            (self.home / rel).parent.mkdir(parents=True, exist_ok=True)

    def tearDown(self):
        self.tmp.cleanup()

    def run_cli(self, *args, input=None):
        return subprocess.run([BIN, *args], env=self.env, input=json.dumps(input or {}), text=True, capture_output=True, timeout=8)

    def install(self, *args):
        r = self.run_cli('hooks', 'install', *args)
        self.assertEqual(r.returncode, 0, r.stderr)
        return r

    def load(self, harness):
        rel, _ = TARGETS[harness]
        raw = (self.home / rel).read_text()
        return tomllib.loads(raw) if rel.endswith('toml') else json.loads(raw)

    def commands(self, harness):
        hooks = self.load(harness)['hooks']
        if harness == 'kimi':
            return {h['event']: h['command'] for h in hooks}
        if harness == 'cursor':
            return {event: groups[-1]['command'] for event, groups in hooks.items()}
        return {event: groups[-1]['hooks'][0]['command'] for event, groups in hooks.items()}

    def test_golden_and_idempotent(self):
        self.install()
        for harness, (rel, _) in TARGETS.items():
            with self.subTest(harness=harness):
                config = json.dumps(self.load(harness), indent=2).replace(BIN, '<hyperag>') + '\n'
                self.assertEqual(config, (FIXTURES / (harness + '.json')).read_text())
        before = {rel: (self.home / rel).read_bytes() for rel, _ in TARGETS.values()}
        self.install()
        self.assertEqual(before, {rel: (self.home / rel).read_bytes() for rel in before})

    def test_merge_backup_and_exact_uninstall(self):
        for harness, (rel, events) in TARGETS.items():
            path = self.home / rel
            if harness == 'kimi':
                path.write_text('# keep comment\n[models]\nchosen = "keep"\n[[hooks]]\nevent = "SessionStart"\ncommand = "echo keep"\n')
            elif rel.endswith('toml'):
                path.write_text('# keep comment\nmodel = "keep"\n[[hooks.SessionStart]]\n[[hooks.SessionStart.hooks]]\ntype = "command"\ncommand = "echo keep"\n')
            else:
                own = {'command': 'echo keep'} if harness == 'cursor' else {'hooks': [{'type': 'command', 'command': 'echo keep'}]}
                path.write_text(json.dumps({'keep': True, 'hooks': {events[0]: [own]}}))
        originals = {rel: (self.home / rel).read_bytes() for rel, _ in TARGETS.values()}
        self.install()
        for rel, _ in TARGETS.values():
            self.assertIn(originals[rel], [p.read_bytes() for p in (self.home / rel).parent.glob('*.hyperag-backup-*')])
        r = self.run_cli('hooks', 'uninstall')
        self.assertEqual(r.returncode, 0, r.stderr)
        for harness, (rel, _) in TARGETS.items():
            self.assertIn('echo keep', (self.home / rel).read_text())
            self.assertNotIn('hyperag-managed', (self.home / rel).read_text())
            doc = self.load(harness)
            if harness == 'kimi': self.assertEqual(doc['models']['chosen'], 'keep')
            elif rel.endswith('toml'): self.assertEqual(doc['model'], 'keep')
            else: self.assertTrue(doc['keep'])
            if rel.endswith('toml'):
                self.assertIn('# keep comment', (self.home / rel).read_text())
        self.assertEqual(self.run_cli('hooks', 'uninstall').returncode, 0)

    def test_dry_run_missing_unknown_and_malformed(self):
        self.install('--dry-run')
        self.assertEqual(list(self.home.rglob('*.*json')), [])
        self.assertFalse((self.home / '.config/hyperag').exists())
        r = self.install('--harness', 'unknown,opencode')
        self.assertIn('skip', r.stdout.lower())
        (self.home / '.cursor').rmdir()
        self.assertIn('skip', self.install('--harness', 'cursor').stdout.lower())
        p = self.home / '.claude/settings.json'
        p.write_text('{broken')
        self.assertNotEqual(self.run_cli('hooks', 'install', '--harness', 'claude-code').returncode, 0)
        self.assertEqual(p.read_text(), '{broken')

    def test_status_and_config_disables(self):
        self.install()
        self.assertIn('installed', self.run_cli('hooks', 'status').stdout)
        p = self.home / '.config/hyperag/hooks.toml'
        p.write_text('[events]\nprompt = false\n[harnesses.cursor]\nenabled = false\n[harnesses.kimi]\nevents = ["session-end"]\n')
        self.install()
        self.assertNotIn('beforeSubmitPrompt', self.load('cursor')['hooks'])
        self.assertNotIn('UserPromptSubmit', self.load('claude-code')['hooks'])
        self.assertEqual(list(self.commands('kimi')), ['SessionEnd'])

    def test_user_edit_inside_group_and_partial_status(self):
        self.install('--harness', 'claude-code')
        path = self.home / '.claude/settings.json'
        doc = self.load('claude-code')
        doc['hooks']['SessionStart'][0]['hooks'].append({'type': 'command', 'command': 'echo later'})
        del doc['hooks']['SessionEnd']
        path.write_text(json.dumps(doc))
        self.assertIn('modified or missing, 2/3', self.run_cli('hooks', 'status').stdout)
        self.assertEqual(self.run_cli('hooks', 'uninstall').returncode, 0)
        self.assertEqual(self.load('claude-code')['hooks']['SessionStart'], [{'hooks': [{'type': 'command', 'command': 'echo later'}]}])
        self.assertNotIn('hyperag-managed', path.read_text())

    def test_inline_toml_and_unowned_collision(self):
        for name in ('codex', 'kimi'):
            path = self.home / TARGETS[name][0]
            path.write_text('hooks = [{event="SessionStart", command="echo keep"}]\n' if name == 'kimi' else 'hooks = {SessionStart = [{hooks = [{type="command", command="echo keep"}]}]}\n')
            self.install('--harness', name)
            self.assertEqual(len(self.commands(name)), 3)
            self.install('--harness', name)
            self.assertEqual(len(self.commands(name)), 3)
            self.assertEqual(self.run_cli('hooks', 'uninstall', '--harness', name).returncode, 0)
            self.assertIn('echo keep', path.read_text())
            self.assertNotIn('hyperag-managed', path.read_text())
        self.install('--harness', 'claude-code')
        (self.home / '.config/hyperag/hooks-installed.json').write_text('{}')
        self.assertNotEqual(self.run_cli('hooks', 'install', '--harness', 'claude-code').returncode, 0)

    def test_gemini_allowance_ownership_and_protections(self):
        path = self.home / '.gemini/settings.json'
        security = {'environmentVariableRedaction': {'enabled': True, 'allowed': ['KEEP_TOKEN'], 'blocked': ['OTHER_TOKEN']}, 'folderTrust': {'enabled': True}}
        path.write_text(json.dumps({'security': security}))
        self.install('--harness', 'gemini')
        expected = json.loads(json.dumps(security))
        expected['environmentVariableRedaction']['allowed'].append('HYPERAG_TOKEN')
        self.assertEqual(self.load('gemini')['security'], expected)
        self.install('--harness', 'gemini')
        self.assertEqual(self.load('gemini')['security'], expected)
        self.assertEqual(self.run_cli('hooks', 'uninstall', '--harness', 'gemini').returncode, 0)
        self.assertEqual(self.load('gemini')['security'], security)
        security['environmentVariableRedaction']['allowed'].append('HYPERAG_TOKEN')
        path.write_text(json.dumps({'security': security}))
        self.install('--harness', 'gemini')
        self.assertEqual(self.run_cli('hooks', 'uninstall', '--harness', 'gemini').returncode, 0)
        self.assertEqual(self.load('gemini')['security'], security)

    def gemini_sanitized_env(self, allowed):
        # Execute the pinned upstream pure sanitizer, without starting a harness.
        script = """
import {readFileSync} from 'node:fs';
const {sanitizeEnvironment} = await import(process.argv[1]);
const {env, allowed} = JSON.parse(readFileSync(0, 'utf8'));
process.stdout.write(JSON.stringify(sanitizeEnvironment(env, {
  enableEnvironmentVariableRedaction: true,
  allowedEnvironmentVariables: allowed,
  blockedEnvironmentVariables: ['OTHER_TOKEN'],
})));
"""
        env = {name: self.env[name] for name in ('HOME', 'PATH', 'XDG_CONFIG_HOME', 'SYNTHETIC_HOOK_TOKEN')}
        env['OTHER_TOKEN'] = 'synthetic-must-not-pass'
        result = subprocess.run(['node', '--input-type=module', '-e', script, (FIXTURES / 'gemini-environment.ts').resolve().as_uri()], input=json.dumps({'env': env, 'allowed': allowed}), text=True, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        sanitized = json.loads(result.stdout)
        self.assertNotIn('OTHER_TOKEN', sanitized)
        return sanitized

    def test_toml_user_handler_inside_owned_group(self):
        self.install('--harness', 'codex')
        path = self.home / '.codex/config.toml'
        command = json.dumps(self.commands('codex')['SessionStart'])
        path.write_text(f'[[hooks.SessionStart]]\n[[hooks.SessionStart.hooks]]\ntype = "command"\ncommand = {command}\ntimeout = 3\n[[hooks.SessionStart.hooks]]\ntype = "command"\ncommand = "echo later"\n')
        self.assertEqual(self.run_cli('hooks', 'uninstall', '--harness', 'codex').returncode, 0)
        self.assertEqual(self.load('codex')['hooks']['SessionStart'], [{'hooks': [{'type': 'command', 'command': 'echo later'}]}])

    @unittest.skipIf(os.geteuid() == 0, 'requires normal filesystem permissions')
    def test_retry_after_target_write_failure(self):
        profile = self.home / '.claude'
        profile.chmod(0o500)
        try:
            self.assertNotEqual(self.run_cli('hooks', 'install', '--harness', 'claude-code').returncode, 0)
        finally:
            profile.chmod(0o700)
        self.install('--harness', 'claude-code')
        self.assertEqual(len(self.commands('claude-code')), 3)
        self.assertEqual(self.run_cli('hooks', 'uninstall', '--harness', 'claude-code').returncode, 0)
        self.assertNotIn('hyperag-managed', (profile / 'settings.json').read_text())

    def test_preserves_unrelated_empty_values_and_edited_handlers(self):
        for harness in ('claude-code', 'gemini', 'cursor'):
            path = self.home / TARGETS[harness][0]
            group = {'command': 'echo keep'} if harness == 'cursor' else {'hooks': []}
            event = TARGETS[harness][1][0]
            path.write_text(json.dumps({'keep': [], 'hooks': {'OtherEvent': [], event: [group]}}))
            self.install('--harness', harness)
            self.assertEqual(self.run_cli('hooks', 'uninstall', '--harness', harness).returncode, 0)
            remaining = self.load(harness)
            if harness == 'gemini':
                self.assertEqual(remaining.pop('security'), {'environmentVariableRedaction': {'allowed': []}})
            expected = {'keep': [], 'hooks': {'OtherEvent': [], event: [group]}}
            if harness == 'cursor': expected['version'] = 1
            self.assertEqual(remaining, expected)
        self.install('--harness', 'claude-code')
        path = self.home / '.claude/settings.json'
        doc = self.load('claude-code')
        doc['hooks']['SessionEnd'][0]['hooks'][0]['timeout'] = 7
        path.write_text(json.dumps(doc))
        self.assertEqual(self.run_cli('hooks', 'uninstall', '--harness', 'claude-code').returncode, 0)
        self.assertEqual(self.load('claude-code')['hooks']['SessionEnd'][0]['hooks'][0]['timeout'], 7)

    def test_binary_path_with_shell_characters(self):
        binary = self.home / "bin with a ' quote" / 'hyperag'
        binary.parent.mkdir()
        shutil.copyfile(BIN, binary); binary.chmod(0o700)
        r = subprocess.run([str(binary), 'hooks', 'install', '--harness', 'cursor'], env=self.env, text=True, capture_output=True, timeout=5)
        self.assertEqual(r.returncode, 0, r.stderr)
        command = self.commands('cursor')['beforeSubmitPrompt']
        r = subprocess.run(command, shell=True, input='{}', env=self.env, text=True, capture_output=True, timeout=5)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads(r.stdout), {'continue': True})

    def test_runtime_invalid_input_deadline_and_config(self):
        self.install('--harness', 'cursor')
        cfg = self.home / '.config/hyperag/hooks.toml'
        cfg.write_text('timeout_ms = 100\n')
        proc = subprocess.Popen([BIN, 'hook', 'session-start', '--harness', 'cursor'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=self.env)
        try:
            self.assertEqual(proc.wait(timeout=2), 0)
            self.assertEqual(json.loads(proc.stdout.read()), {})
            self.assertIn(b'timed out', proc.stderr.read())
        finally:
            proc.kill() if proc.poll() is None else None
            proc.stdin.close(); proc.stdout.close(); proc.stderr.close()
        for raw in ('{broken', 'x' * 1_048_577):
            r = subprocess.run([BIN, 'hook', 'prompt', '--harness', 'cursor'], env=self.env, input=raw, text=True, capture_output=True, timeout=3)
            self.assertEqual(r.returncode, 0)
            self.assertEqual(json.loads(r.stdout), {'continue': True})
        cfg.write_text('token = "must-not-be-accepted"\n')
        self.assertNotEqual(self.run_cli('hooks', 'install').returncode, 0)
        self.assertEqual(self.run_cli('hook', 'prompt', '--harness', 'cursor').returncode, 0)
        self.assertEqual(self.run_cli('hooks', 'uninstall').returncode, 0)

    def test_ungranted_project_hooks_quietly_skip_without_post_or_grant_changes(self):
        requests = []
        grants = {'read': ['project:read-only'], 'write': []}
        original_grants = json.loads(json.dumps(grants))
        class Service(BaseHTTPRequestHandler):
            def log_message(self, *_): pass
            def do_GET(self):
                requests.append(('GET', self.path, self.headers.get('Authorization')))
                self.send_response(200); self.end_headers()
                self.wfile.write(json.dumps(grants).encode())
            def do_POST(self):
                requests.append(('POST', self.path, self.headers.get('Authorization')))
                self.send_response(403); self.end_headers(); self.wfile.write(b'{}')
        server = ThreadingHTTPServer(('127.0.0.1', 0), Service)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close); self.addCleanup(server.shutdown)
        self.env.update(HYPERAG_URL=f'http://127.0.0.1:{server.server_port}', HYPERAG_TOKEN='synthetic-valid-hook-token')
        for project in ('Unknown-Project', 'Read-Only'):
            directory = self.home / project
            directory.mkdir()
            for event in ('session-start', 'prompt', 'session-end'):
                with self.subTest(project=project, event=event):
                    before = len(requests)
                    result = self.run_cli('hook', event, '--harness', 'claude-code', input={'cwd': str(directory), 'session_id': 'fixture', 'prompt': 'synthetic input'})
                    self.assertEqual((result.returncode, result.stdout, result.stderr), (0, '', ''))
                    self.assertEqual(requests[before:], [('GET', '/v1/scopes', 'Bearer synthetic-valid-hook-token')])
                    self.assertEqual(grants, original_grants)

    def test_native_events_reach_runtime(self):
        received = []
        authorization = []
        class Service(BaseHTTPRequestHandler):
            def log_message(self, *_): pass
            def do_GET(self):
                authorization.append(self.headers.get('Authorization'))
                if self.path == '/v1/scopes':
                    self.send_response(200); self.end_headers()
                    self.wfile.write(b'{"read":["project:demo-repo","test:hooks","override:scope"],"write":["project:demo-repo","test:hooks","override:scope"]}')
                    return
                received.append(('session-start', self.path))
                self.send_response(200); self.end_headers(); self.wfile.write(b'{"text":"remember synthetic context"}')
            def do_POST(self):
                authorization.append(self.headers.get('Authorization'))
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                received.append((body['kind'], body))
                self.send_response(200); self.end_headers(); self.wfile.write(b'{}')
        server = ThreadingHTTPServer(('127.0.0.1', 0), Service)
        thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
        self.addCleanup(server.server_close); self.addCleanup(server.shutdown)
        self.install()
        (self.home / '.config/hyperag/hooks.toml').write_text(f'url = "http://127.0.0.1:{server.server_port}"\ntoken_env = "SYNTHETIC_HOOK_TOKEN"\nscope = "test:hooks"\ntimeout_ms = 500\n')
        self.env['SYNTHETIC_HOOK_TOKEN'] = 'fake-test-token'
        # The token source changed; reconcile Gemini's one-variable allowance.
        self.install('--harness', 'gemini')
        allowed = self.load('gemini')['security']['environmentVariableRedaction']['allowed']
        self.assertEqual(allowed, ['SYNTHETIC_HOOK_TOKEN'])
        sanitized = self.gemini_sanitized_env(allowed)
        self.assertEqual(sanitized['SYNTHETIC_HOOK_TOKEN'], 'fake-test-token')
        redacted = self.gemini_sanitized_env([])
        self.assertNotIn('SYNTHETIC_HOOK_TOKEN', redacted)
        for harness, (_, events) in TARGETS.items():
            for event, common in zip(events, ['session-start', 'prompt', 'session-end']):
                payload = {'hook_event_name': event, 'session_id': 'test-session', 'conversation_id': 'test-session', 'cwd': str(self.home), 'prompt': 'synthetic prompt', 'reason': 'exit'}
                if harness == 'cursor':
                    del payload['cwd']; del payload['session_id']
                    payload['workspace_roots'] = [str(self.home)]
                cmd = self.commands(harness)[event]
                r = subprocess.run(cmd, shell=True, input=json.dumps(payload), env=sanitized if harness == 'gemini' else self.env, text=True, capture_output=True, timeout=5)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(received[-1][0], common if common != 'session-end' else 'session')
                if common == 'session-start':
                    if harness in ('cursor', 'gemini'):
                        data = json.loads(r.stdout)
                        context = data.get('additional_context') if harness == 'cursor' else data['hookSpecificOutput']['additionalContext']
                        self.assertIn('synthetic context', context)
                    else:
                        self.assertIn('synthetic context', r.stdout)
                if common == 'prompt':
                    self.assertEqual(received[-1][1]['scope'], 'test:hooks')
                    if harness == 'cursor': self.assertEqual(json.loads(r.stdout), {'continue': True})
        self.assertEqual(len(received), 15)
        result = subprocess.run(self.commands('gemini')['BeforeAgent'], shell=True, input='{"prompt":"redacted"}', env=redacted, text=True, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(len(received), 15)
        self.assertTrue(all(header == 'Bearer fake-test-token' for header in authorization))
        cfg = self.home / '.config/hyperag/hooks.toml'
        cfg.write_text(f'url = "http://127.0.0.1:{server.server_port}"\ntoken_env = "SYNTHETIC_HOOK_TOKEN"\n[events]\nprompt = false\n')
        self.assertEqual(self.run_cli('hook', 'prompt', '--harness', 'codex', input={'prompt': 'disabled'}).returncode, 0)
        self.assertEqual(len(received), 15)
        cfg.write_text(f'url = "http://127.0.0.1:{server.server_port}"\ntoken_env = "SYNTHETIC_HOOK_TOKEN"\n')
        self.run_cli('hook', 'session-start', '--harness', 'cursor', input={'workspace_roots': [str(self.home / 'Demo-Repo')]})
        self.assertIn('project%3Ademo-repo', received[-1][1])
        self.env['HYPERAG_SCOPE'] = 'override:scope'
        self.run_cli('hook', 'session-start', '--harness', 'cursor')
        self.assertIn('override%3Ascope', received[-1][1])
        self.env['HYPERAG_URL'] = 'http://127.0.0.1:9'
        for harness in TARGETS:
            r = self.run_cli('hook', 'session-start', '--harness', harness)
            self.assertEqual(r.returncode, 0)
            if harness in ('cursor', 'gemini'): self.assertEqual(json.loads(r.stdout), {})
        self.assertEqual(self.run_cli('hook', 'prompt', input={'prompt': 'x'}).returncode, 0)

if __name__ == '__main__': unittest.main()
