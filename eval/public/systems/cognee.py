"""Cognee with its default pipeline: add() then cognify() (chunking, LLM graph extraction with the
default KnowledgeGraph model, LLM summaries, embeddings), embedded default stores (SQLite, LanceDB,
Ladybug graph) in the run's work dir. Search is SearchType.CHUNKS so BEIR can rank documents; the default
search type (an LLM answer) has no ranked list. The LLM is the shared endpoint through Cognee's
"custom" (litellm) provider, the embedder the shared local bge-small.

One ingest() call is one cognify() (a session for the memory benchmarks, a batch for BEIR). Each
doc is one data item with its id in external_metadata, so chunks map back to docs.

  uv venv --python 3.12 eval/public/.venv-cognee && uv pip install -c eval/public/python-ca-constraints.txt --python eval/public/.venv-cognee/bin/python cognee==1.6.5
  brew install openssl@3   # the ladybug wheel links /opt/homebrew/opt/openssl@3
"""
import asyncio
import json
import os
from importlib.metadata import version

from systems.shared import EMBED_DIMS, EMBED_MODEL, EMBED_URL, llm, rank

os.environ.setdefault("TELEMETRY_DISABLED", "1")
VERSION = {"cognee": version("cognee"), "ladybug": version("ladybug"), "litellm": version("litellm")}
CONFIG = {"pipeline": "add + cognify (defaults)", "search": "SearchType.CHUNKS, top_k", "stores": "defaults (sqlite, lancedb, ladybug graph)",
          "llm": "shared endpoint via provider custom, max completion tokens 8192", "embedder": f"openai_compatible {EMBED_MODEL}", "asOf": "none (no date parameter at search)"}


class Cognee:
    def __init__(self, workdir: str):
        url, model, key = llm()
        os.environ.update({"LLM_PROVIDER": "custom", "LLM_MODEL": f"openai/{model}", "LLM_ENDPOINT": url, "LLM_API_KEY": key,
                           "EMBEDDING_PROVIDER": "openai_compatible", "EMBEDDING_MODEL": EMBED_MODEL, "EMBEDDING_ENDPOINT": EMBED_URL,
                           "EMBEDDING_API_KEY": "local", "EMBEDDING_DIMENSIONS": str(EMBED_DIMS), "HUGGINGFACE_TOKENIZER": "BAAI/bge-small-en-v1.5",
                           # output cap: the default 16,384 plus a long prompt can exceed the endpoint's 32,768 context
                           "LLM_MAX_COMPLETION_TOKENS": "8192"})
        import cognee
        from cognee.tasks.ingestion.data_item import DataItem

        self.cognee, self.DataItem = cognee, DataItem
        cognee.config.system_root_directory(os.path.join(workdir, "system"))
        cognee.config.data_root_directory(os.path.join(workdir, "data"))
        self.loop = asyncio.new_event_loop()
        self.sessions: set[str] = set()

    def ingest(self, ns, docs):
        ds = ns.replace("/", "_")
        if docs and docs[0].get("speaker"):
            self.sessions.add(ns)
        items = [self.DataItem(data=f"{d['title']}\n{d['text']}" if d.get("title") else d["text"], external_metadata={"doc_id": d["id"]}, literal_text=True) for d in docs]
        self.loop.run_until_complete(self.cognee.add(items, dataset_name=ds))
        self.loop.run_until_complete(self.cognee.cognify(datasets=[ds]))

    def search(self, ns, query, k):
        from cognee.modules.search.types import SearchType

        res = self.loop.run_until_complete(self.cognee.search(query, query_type=SearchType.CHUNKS, datasets=[ns.replace("/", "_")], top_k=k))
        # a long turn can be several chunks: each is a memory on a conversation, one doc on BEIR
        return rank([([h["id"]] if h["id"] else [], h["score"], h["text"]) for h in self._hits(res)], k, ns in self.sessions)

    @staticmethod
    def _hits(res):
        # CHUNKS results come best first with a distance score; the doc id rides in external_metadata
        chunks = [c for r in res for c in r["search_result"]]
        return [{"id": json.loads(c.get("external_metadata") or "{}").get("doc_id"), "score": -float(c.get("score") or 0), "text": c["text"]} for c in chunks]

    def close(self):
        self.loop.close()


def open(workdir: str, profile: str) -> Cognee:
    if profile != "default":
        raise ValueError("cognee profile is default")
    return Cognee(workdir)
