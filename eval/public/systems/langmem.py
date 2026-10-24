"""LangMem with its default background pipeline: create_memory_store_manager (default instructions,
inserts on, deletes off) extracts and updates memories with the LLM after each ingest() call;
search is the store's semantic search. The store is LangGraph's InMemoryStore with a vector index,
as in LangMem's quickstart. The LLM is the shared endpoint through ChatOpenAI, the embedder the
shared local bge-small through OpenAIEmbeddings.

Memories carry no source ids, so the adapter records which doc ids each memory key was written or
updated from (one manager call per namespace at a time keeps that exact). A BEIR doc is one
call; docs with a speaker in one ingest() call are one session and one call.

  uv venv --python 3.12 eval/public/.venv-langmem && uv pip install --python eval/public/.venv-langmem/bin/python langmem==0.0.30
"""
from importlib.metadata import version

from langchain_openai import ChatOpenAI, OpenAIEmbeddings
from langgraph.store.memory import InMemoryStore
from langmem import create_memory_store_manager

from systems.shared import EMBED_DIMS, EMBED_MODEL, EMBED_URL, llm, rank

VERSION = {"langmem": version("langmem"), "langgraph": version("langgraph"), "langchain-openai": version("langchain-openai")}
CONFIG = {"manager": "create_memory_store_manager defaults", "store": "InMemoryStore with vector index", "search": "store.search, limit k",
          "llm": "shared endpoint via ChatOpenAI temperature 0", "embedder": f"OpenAIEmbeddings {EMBED_MODEL}", "asOf": "none (no date parameter at search)"}


class LangMem:
    def __init__(self):
        url, model, key = llm()
        embed = OpenAIEmbeddings(model=EMBED_MODEL, base_url=EMBED_URL, api_key="local", check_embedding_ctx_length=False)
        self.store = InMemoryStore(index={"dims": EMBED_DIMS, "embed": embed})
        self.manager = create_memory_store_manager(ChatOpenAI(model=model, base_url=url, api_key=key, temperature=0),
                                                   namespace=("memories", "{ns}"), store=self.store)
        self.sources: dict[tuple[str, str], set[str]] = {}
        self.sessions: set[str] = set()

    def _call(self, ns, text, ids):
        before = {i.key: i.value for i in self.store.search(("memories", ns), limit=100000)}
        self.manager.invoke({"messages": [{"role": "user", "content": text}]}, config={"configurable": {"ns": ns}})
        for item in self.store.search(("memories", ns), limit=100000):
            if before.get(item.key) != item.value:
                self.sources.setdefault((ns, item.key), set()).update(ids)

    def ingest(self, ns, docs):
        if docs and docs[0].get("speaker"):
            self.sessions.add(ns)
            self._call(ns, "\n".join(d["text"] for d in docs), [d["id"] for d in docs])
            return
        for d in docs:
            self._call(ns, f"{d['title']}\n{d['text']}" if d.get("title") else d["text"], [d["id"]])

    def search(self, ns, query, k):
        items = self.store.search(("memories", ns), query=query, limit=k)
        mems = [(sorted(self.sources.get((ns, i.key), ())), float(i.score or 0), str(i.value.get("content", i.value))) for i in items]
        return rank(mems, k, ns in self.sessions)

    def close(self):
        pass


def open(workdir: str, profile: str) -> LangMem:
    if profile != "default":
        raise ValueError("langmem profile is default")
    return LangMem()
