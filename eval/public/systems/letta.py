"""Letta (MemGPT) self-hosted server, its REST API, one agent per namespace. Letta 0.16.8 is the
last self-hosted server release; current Letta (Letta Code) runs its agents on Letta Cloud by
default (see SYSTEMS.md). Docs go to the agent's archival memory (POST archival-memory, chunked
and embedded by the server, doc id as a tag); search is archival-memory/search, the function
behind the agent's own archival_memory_search tool. Neither call uses the LLM; the agent's LLM
config points at the shared endpoint when it is set.

  docker run -d --name competitors-letta -p 58283:8283 -e LETTA_TELEMETRY_ENABLED=false letta/letta:0.16.8
"""
import json
import os
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor

from systems.shared import CONCURRENCY, EMBED_DIMS, EMBED_MODEL, EMBED_URL, rank

URL = os.environ.get("LETTA_URL", "http://127.0.0.1:58283")
# the server reaches the host's embedder through OrbStack's host alias
EMBED_FROM_CONTAINER = EMBED_URL.replace("127.0.0.1", "host.docker.internal").replace("localhost", "host.docker.internal")
VERSION = {"letta server": "0.16.8 (docker letta/letta:0.16.8)"}
CONFIG = {"ingest": "POST /v1/agents/{id}/archival-memory, one per doc, doc id tag", "search": "GET archival-memory/search (semantic), top_k",
          "embedding": f"openai-compatible {EMBED_MODEL}, server default chunk size 300", "llm": "not called on this path", "concurrency": CONCURRENCY, "asOf": "none (its search has only a time filter, and stored times are load times, so a 2023 date would hide every memory)"}
CONTAINERS = ["competitors-letta"]


def call(method: str, path: str, body=None):
    req = urllib.request.Request(URL + path, method=method, data=None if body is None else json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=600) as r:
        return json.loads(r.read())


class Letta:
    def __init__(self):
        self.agents: dict[str, str] = {}
        self.sessions: set[str] = set()
        base, model = os.environ.get("EVAL_LLM_BASE_URL", "http://127.0.0.1:9/v1"), os.environ.get("EVAL_LLM_MODEL", "unset")
        self.llm = {"model": model, "model_endpoint_type": "openai", "model_endpoint": base, "context_window": 32000}

    def agent(self, ns: str) -> str:
        if ns not in self.agents:
            # agent names cannot hold "/" (a <set>/<item> namespace): the server answers 500
            a = call("POST", "/v1/agents/", {"name": ns.replace("/", "_"), "llm_config": self.llm, "embedding_config": {
                "embedding_endpoint_type": "openai", "embedding_endpoint": EMBED_FROM_CONTAINER, "embedding_model": EMBED_MODEL,
                "embedding_dim": EMBED_DIMS, "embedding_chunk_size": 300}})
            self.agents[ns] = a["id"]
        return self.agents[ns]

    def ingest(self, ns, docs):
        agent = self.agent(ns)
        if docs and docs[0].get("speaker"):
            self.sessions.add(ns)
        def text(d):
            if d.get("speaker"):
                return d["text"]  # the runner's text already carries speaker and date
            return f"{d['title']}\n{d['text']}" if d.get("title") else d["text"]

        with ThreadPoolExecutor(CONCURRENCY) as pool:
            list(pool.map(lambda d: call("POST", f"/v1/agents/{agent}/archival-memory", {"text": text(d), "tags": [f"doc:{d['id']}"]}), docs))

    def search(self, ns, query, k):
        q = urllib.parse.urlencode({"query": query, "top_k": k})
        res = call("GET", f"/v1/agents/{self.agent(ns)}/archival-memory/search?{q}")
        res = res.get("results", res) if isinstance(res, dict) else res
        doc = lambda r: next((t[4:] for t in r.get("tags") or [] if t.startswith("doc:")), None)
        # a long turn can be several passages: each is a memory on a conversation, one doc on BEIR
        return rank([([doc(r)] if doc(r) else [], 1.0 / (i + 1), r["content"]) for i, r in enumerate(res)], k, ns in self.sessions)

    def close(self):
        for a in self.agents.values():
            call("DELETE", f"/v1/agents/{a}")


def open(workdir: str, profile: str) -> Letta:
    if profile != "default":
        raise ValueError("letta profile is default")
    return Letta()
