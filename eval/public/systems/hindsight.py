"""Hindsight (vectorize-io) with its default pipeline through its own server and client: retain()
runs LLM fact extraction, entity resolution and links; recall() is its default multi-strategy
retrieval (semantic, BM25, graph, temporal, fused and reranked by its local cross-encoder) with
the default budget. Server defaults: embedded Postgres (pg0), local BAAI/bge-small-en-v1.5
embeddings (the same model the other systems get), local ms-marco-MiniLM-L-6-v2 reranker. The LLM
is the shared endpoint. One memory bank per namespace, deleted at close.

A BEIR doc is one retain() with its id as document_id; docs with a speaker in one ingest() call
are one session and one retain(). A recalled fact maps back to the docs of its document.

  uv venv --python 3.12 eval/public/.venv-hindsight
  uv pip install -c eval/public/python-ca-constraints.txt --python eval/public/.venv-hindsight/bin/python hindsight-api==1.1.0 hindsight-client==1.1.0
  HINDSIGHT_API_LLM_PROVIDER=openai HINDSIGHT_API_LLM_BASE_URL=$EVAL_LLM_BASE_URL HINDSIGHT_API_LLM_MODEL=$EVAL_LLM_MODEL \\
    HINDSIGHT_API_LLM_API_KEY=$EVAL_LLM_API_KEY HINDSIGHT_API_DATABASE_URL=pg0://competitors \\
    eval/public/.venv-hindsight/bin/hindsight-api --host 127.0.0.1 --port 58888
"""
import os
from importlib.metadata import version

from hindsight_client import Hindsight

from systems.shared import iso_date, rank

URL = os.environ.get("HINDSIGHT_URL", "http://127.0.0.1:58888")
VERSION = {"hindsight-api": version("hindsight-api"), "hindsight-client": version("hindsight-client")}
CONFIG = {"retain": "defaults (LLM extraction)", "recall": "defaults (budget mid, max_tokens 4096)", "database": "pg0 embedded Postgres",
          "embeddings": "local BAAI/bge-small-en-v1.5 (server default)", "reranker": "local cross-encoder (server default)", "llm": "shared endpoint",
          # retain's default 64,000 output tokens exceeds the endpoint's whole 32,768 context
          "max_completion_tokens": "8192 for retain, consolidation and reflect",
          "asOf": "anchor (recall query_timestamp); session date passed to retain timestamp"}


class HindsightSystem:
    def __init__(self):
        self.h = Hindsight(base_url=URL, timeout=600)
        self.banks: set[str] = set()
        self.docs: dict[str, str] = {}  # document_id -> doc ids
        self.sessions: set[str] = set()

    def bank(self, ns: str) -> str:
        b = ns.replace("/", "_")
        if b not in self.banks:
            try:  # pg0 outlives a crashed run: start the bank empty
                self.h.delete_bank(bank_id=b)
            except Exception:
                pass
            self.h.create_bank(bank_id=b)
            self.banks.add(b)
        return b

    def ingest(self, ns, docs):
        b = self.bank(ns)
        if docs and docs[0].get("speaker"):
            self.sessions.add(b)
            key = f"{b}:{len(self.docs)}"
            self.docs[key] = ",".join(d["id"] for d in docs)
            when = iso_date(docs[0].get("timestamp"))
            self.h.retain(bank_id=b, content="\n".join(d["text"] for d in docs), document_id=key, context="conversation session", **({"timestamp": when} if when else {}))
            return
        for d in docs:
            self.docs[d["id"]] = d["id"]
            self.h.retain(bank_id=b, content=f"{d['title']}\n{d['text']}" if d.get("title") else d["text"], document_id=d["id"])

    def search(self, ns, query, k, as_of=None):
        # as_of: the question's own date, Hindsight's anchor for relative dates
        res = self.h.recall(bank_id=self.bank(ns), query=query, **({"query_timestamp": as_of} if as_of else {}))
        mems = [([d for d in self.docs.get(r.document_id or "", "").split(",") if d], 1.0 / (i + 1), r.text) for i, r in enumerate(res.results)]
        return rank(mems, k, self.bank(ns) in self.sessions)

    def close(self):
        for b in self.banks:
            self.h.delete_bank(bank_id=b)


def open(workdir: str, profile: str) -> HindsightSystem:
    if profile != "default":
        raise ValueError("hindsight profile is default")
    return HindsightSystem()
