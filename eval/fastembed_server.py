"""The baseline's embedding service, for reproduction: fastembed 0.9.0 BAAI/bge-small-en-v1.5 on an
OpenAI-compatible /v1/embeddings endpoint (one plain JSON reply instead of the baseline's chunked
transport; same model, batch size 8, two threads, no cache).

uv venv --python 3.12 .venv-fastembed && uv pip install fastembed==0.9.0 onnxruntime==1.31.0 numpy==2.5.4
.venv-fastembed/bin/python eval/fastembed_server.py --cache <dir> --port 58321
"""
import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Lock

from fastembed import TextEmbedding

p = argparse.ArgumentParser()
p.add_argument("--cache", required=True)
p.add_argument("--port", type=int, default=58321)
a = p.parse_args()
MODEL = TextEmbedding("BAAI/bge-small-en-v1.5", cache_dir=a.cache, threads=2)
LOCK = Lock()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        inputs = body["input"] if isinstance(body["input"], list) else [body["input"]]
        with LOCK:
            vectors = [v.tolist() for v in MODEL.embed(inputs, batch_size=8)]
        raw = json.dumps({"object": "list", "model": "BAAI/bge-small-en-v1.5",
                          "data": [{"index": i, "embedding": v} for i, v in enumerate(vectors)]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)


ThreadingHTTPServer(("127.0.0.1", a.port), Handler).serve_forever()
