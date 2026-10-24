"""Offline socket/SIGSTOP regression; scales the adapter's 120s deadline to 80ms.

Run: python3 -I eval/public/test_gpu_pause.py
No model, credentials, database or external network is used.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def main():
    requested, resumed = threading.Event(), threading.Event()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            self.rfile.read(int(self.headers["Content-Length"]))
            requested.set()
            resumed.wait(5)
            time.sleep(0.05)  # The reply arrives after resume, not before the expired timer.
            try:
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(b'{"embeddings": [1, 2, 3]}')
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix="gpu-pause-") as folder:
            folder = Path(folder)
            result = folder / "result.json"
            child = folder / "request.ts"
            module = Path(__file__).resolve().parent / "systems/gpu-request.ts"
            child.write_text(
                f'import {{ gpuRequest }} from {json.dumps(str(module))};\n'
                'const original = AbortSignal.timeout.bind(AbortSignal);\n'
                'AbortSignal.timeout = (ms) => original(ms === 120000 ? 80 : ms);\n'
                'process.env.GPU_RETRY_PROXY = "1";\n'
                f'const output = {json.dumps(str(result))};\n'
                f'try {{ const value = await gpuRequest("http://127.0.0.1:{server.server_port}", "/v2/embed", {{inputs:["test"]}});'
                ' await Bun.write(output, JSON.stringify({ok:true,value})); }\n'
                'catch(error) { await Bun.write(output, JSON.stringify({ok:false,name:error.name})); }\n'
            )
            proc = subprocess.Popen(["bun", str(child)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                assert requested.wait(5), "request did not reach the local socket"
                os.kill(proc.pid, signal.SIGSTOP)
                time.sleep(0.35)
                os.kill(proc.pid, signal.SIGCONT)
                resumed.set()
                assert proc.wait(timeout=5) == 0
                value = json.loads(result.read_text())
                assert value == {"ok": True, "value": {"embeddings": [1, 2, 3]}}, value
                print("PASS: a proxied request survives a machine pause beyond the old deadline")
            finally:
                if proc.poll() is None:
                    os.kill(proc.pid, signal.SIGCONT)
                    proc.terminate()
                    proc.wait()
    finally:
        resumed.set()
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
