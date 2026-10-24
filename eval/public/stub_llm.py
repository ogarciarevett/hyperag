"""A stub OpenAI-compatible chat endpoint for dry runs only: it answers every call with the
smallest reply that fits the requested shape (the tool's or response_format's JSON schema, else
"stub"), so a system's whole pipeline can be exercised before the real endpoint is up.
Nothing scored against it is ever reported.

  python3 eval/public/stub_llm.py --port 58399     # then EVAL_LLM_BASE_URL=http://127.0.0.1:58399/v1
"""
from __future__ import annotations

import argparse
import json
import os
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def minimal(schema: dict, defs: dict):
    if "$ref" in schema:
        return minimal(defs[schema["$ref"].split("/")[-1]], defs)
    for key in ("anyOf", "oneOf", "allOf"):
        if key in schema:
            options = [s for s in schema[key] if s.get("type") != "null"] or schema[key]
            return minimal(options[0], defs)
    if "enum" in schema:
        return schema["enum"][0]
    if "const" in schema:
        return schema["const"]
    kind = schema.get("type", "object")
    kind = next((k for k in kind if k != "null"), "string") if isinstance(kind, list) else kind
    if kind == "object":
        props = schema.get("properties", {})
        return {k: minimal(props[k], defs) for k in schema.get("required", props)}
    return {"array": [], "string": "stub", "integer": 0, "number": 0, "boolean": False, "null": None}.get(kind, "stub")


def schema_in_prompt(messages: list) -> dict | None:
    """The first JSON object with "properties" inside the prompt (systems that send json_object)."""
    text = "\n".join(m["content"] if isinstance(m.get("content"), str) else "" for m in messages)
    decoder = json.JSONDecoder()
    for i, ch in enumerate(text):
        if ch == "{" and '"properties"' in text[i : i + 20000]:
            try:
                obj, _ = decoder.raw_decode(text, i)
            except ValueError:
                continue
            if isinstance(obj, dict) and "properties" in obj:
                return obj
    return None


def reply(body: dict) -> dict:
    msg: dict = {"role": "assistant", "content": "stub"}
    tools = body.get("tools") or []
    fmt = body.get("response_format") or {}
    if tools and body.get("tool_choice") not in (None, "none", "auto"):
        choice = body["tool_choice"]
        name = choice["function"]["name"] if isinstance(choice, dict) else tools[0]["function"]["name"]
        fn = next(t["function"] for t in tools if t["function"]["name"] == name)
        params = fn.get("parameters", {})
        args = minimal(params, params.get("$defs", {}))
        msg = {"role": "assistant", "content": None, "tool_calls": [{"id": "call_stub", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}]}
    elif fmt.get("type") == "json_schema":
        schema = fmt["json_schema"]["schema"]
        msg["content"] = json.dumps(minimal(schema, schema.get("$defs", {})))
    elif fmt.get("type") == "json_object":
        schema = schema_in_prompt(body.get("messages") or [])
        msg["content"] = json.dumps(minimal(schema, schema.get("$defs", {}))) if schema else "{}"
    return {"id": "stub", "object": "chat.completion", "created": int(time.time()), "model": body.get("model", "stub"),
            "choices": [{"index": 0, "message": msg, "finish_reason": "tool_calls" if msg.get("tool_calls") else "stop", "logprobs": None}],
            "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def send(self, obj):
        raw = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        self.send({"object": "list", "data": [{"id": "stub", "object": "model", "owned_by": "stub"}]})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        if os.environ.get("STUB_LOG"):  # what a system really sends, to debug its dry run
            with open(os.environ["STUB_LOG"], "a") as f:
                f.write(json.dumps({k: v for k, v in body.items() if k != "messages"}) + "\n")
        self.send(reply(body))


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--port", type=int, default=58399)
    ThreadingHTTPServer(("127.0.0.1", p.parse_args().port), Handler).serve_forever()
