"""Mem0 open source (AsyncMemory) with its default pipeline: add() with infer=True, so the LLM
extracts facts and decides add/update/delete; search() with its defaults (top_k, threshold 0.1,
hybrid vector + keyword). Embedded Qdrant on disk, the default store. The LLM is the shared
endpoint, the embedder the shared local bge-small (see shared.py).

A BEIR doc is one add() of one user message; docs with a speaker in one ingest() call are one
session and go in one add() as a message list (per-session extraction). Each memory keeps the
doc ids of its source in metadata, so hits map back to docs.

  uv venv --python 3.12 eval/public/.venv-mem0
  uv pip install --python eval/public/.venv-mem0/bin/python "mem0ai[nlp]==2.2.1" \
    "en_core_web_sm @ https://github.com/explosion/spacy-models/releases/download/en_core_web_sm-3.8.0/en_core_web_sm-3.8.0-py3-none-any.whl"
"""
import asyncio
import os
from importlib.metadata import version

os.environ.setdefault("MEM0_TELEMETRY", "False")
from mem0 import AsyncMemory  # noqa: E402

from systems.shared import CONCURRENCY, EMBED_DIMS, EMBED_MODEL, EMBED_URL, llm, rank

VERSION = {"mem0ai": version("mem0ai"), "qdrant-client": version("qdrant-client"), "spacy": version("spacy"), "en_core_web_sm": version("en_core_web_sm")}
CONFIG = {"infer": True, "vector_store": "qdrant embedded, on disk", "embedder": f"openai-compatible {EMBED_MODEL}",
          "llm": "shared endpoint (EVAL_LLM_MODEL)", "search": "defaults", "concurrency": CONCURRENCY, "asOf": "none (Mem0 search reference_date is platform-only, not in the open-source build)"}


class Mem0:
    def __init__(self, workdir: str):
        url, model, key = llm()
        self.loop = asyncio.new_event_loop()
        self.sessions: set[str] = set()
        self.m = AsyncMemory.from_config({
            "llm": {"provider": "openai", "config": {"model": model, "openai_base_url": url, "api_key": key}},
            "embedder": {"provider": "openai", "config": {"model": EMBED_MODEL, "openai_base_url": EMBED_URL, "api_key": "local", "embedding_dims": EMBED_DIMS}},
            "vector_store": {"provider": "qdrant", "config": {"path": os.path.join(workdir, "qdrant"), "on_disk": True, "embedding_model_dims": EMBED_DIMS}},
            "history_db_path": os.path.join(workdir, "history.db"),
        })

    def ingest(self, ns, docs):
        if docs and docs[0].get("speaker"):
            self.sessions.add(ns)
            # the runner's text already carries speaker and date
            calls = [([{"role": "user", "content": "\n".join(d["text"] for d in docs)}], ",".join(d["id"] for d in docs))]
        else:
            calls = [([{"role": "user", "content": f"{d['title']}\n{d['text']}" if d.get("title") else d["text"]}], d["id"]) for d in docs]
        gate = asyncio.Semaphore(CONCURRENCY)

        async def one(messages, ids):
            async with gate:
                await self.m.add(messages, user_id=ns, metadata={"doc_ids": ids})

        async def all_():
            await asyncio.gather(*(one(*c) for c in calls))

        self.loop.run_until_complete(all_())

    def search(self, ns, query, k):
        res = self.loop.run_until_complete(self.m.search(query, top_k=k, filters={"user_id": ns}))
        mems = [([d for d in (r.get("metadata") or {}).get("doc_ids", "").split(",") if d], float(r.get("score") or 0), r["memory"]) for r in res["results"]]
        return rank(mems, k, ns in self.sessions)

    def close(self):
        self.loop.close()


def open(workdir: str, profile: str) -> Mem0:
    if profile != "default":
        raise ValueError("mem0 profile is default")
    return Mem0(workdir)
