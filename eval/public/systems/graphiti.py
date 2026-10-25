"""Graphiti (the temporal knowledge graph inside Zep) with its default pipeline: add_episode()
runs LLM entity and edge extraction, dedupe and invalidation; search() is its default
EDGE_HYBRID_SEARCH_RRF over facts (BM25 + cosine, RRF). Neo4j, the store its quickstart uses,
in OrbStack. The LLM is the shared endpoint through OpenAIGenericClient (the client Graphiti
documents for OpenAI-compatible servers), the embedder the shared local bge-small.

A BEIR doc is one text episode; docs with a speaker in one ingest() call are one session and one
message episode. A fact maps back to the docs of the episodes it came from.

  uv venv --python 3.12 eval/public/.venv-graphiti && uv pip install -c eval/public/python-ca-constraints.txt --python eval/public/.venv-graphiti/bin/python graphiti-core==0.31.2 \
    httpx==0.28.1  # graphiti imports httpx; openai 3.x moved to httpx2
  docker run -d --name fake-test-only-neo4j -p 57687:7687 -e NEO4J_AUTH=neo4j/fake-test-only -e NEO4J_server_memory_heap_max__size=2G neo4j:5.26.28-community
"""
import asyncio
import os
from datetime import datetime, timezone
from importlib.metadata import version

os.environ.setdefault("GRAPHITI_TELEMETRY_ENABLED", "false")
from graphiti_core import Graphiti  # noqa: E402
from graphiti_core.cross_encoder.openai_reranker_client import OpenAIRerankerClient  # noqa: E402
from graphiti_core.embedder.openai import OpenAIEmbedder, OpenAIEmbedderConfig  # noqa: E402
from graphiti_core.llm_client.config import LLMConfig, ModelSize  # noqa: E402
from graphiti_core.llm_client.openai_generic_client import OpenAIGenericClient  # noqa: E402
from graphiti_core.nodes import EpisodeType  # noqa: E402
from graphiti_core.utils.maintenance.graph_data_operations import clear_data  # noqa: E402

from systems.shared import CONCURRENCY, EMBED_DIMS, EMBED_MODEL, EMBED_URL, llm, rank  # noqa: E402

NEO4J = os.environ.get("GRAPHITI_NEO4J_URI", "bolt://127.0.0.1:57687")
VERSION = {"graphiti-core": version("graphiti-core"), "openai": version("openai"), "neo4j": "5.26.28-community"}
CONFIG = {"ingest": "add_episode, defaults", "search": "Graphiti.search (EDGE_HYBRID_SEARCH_RRF)", "llm": "shared endpoint via OpenAIGenericClient",
          "embedder": f"openai-compatible {EMBED_MODEL}", "store": "Neo4j 5.26 in OrbStack", "SEMAPHORE_LIMIT": CONCURRENCY, "max_tokens": 8192,
          "asOf": "none (no date parameter at search; episodes carry their session dates)",
          "output_token_cap_note": "Graphiti 0.31.2 edge extraction requests 16384 tokens natively; this run caps requests at the shared 8192-token ceiling on the 32768-token endpoint"}
CONTAINERS = ["fake-test-only-neo4j"]
# Edge extraction explicitly requests 16,384 tokens, bypassing the generic client's default.
MAX_OUT = 8192


class OutputCappedClient(OpenAIGenericClient):
    async def _generate_response(self, messages, response_model=None, max_tokens=MAX_OUT, model_size=ModelSize.medium, **overrides):
        return await super()._generate_response(messages, response_model, min(max_tokens, MAX_OUT), model_size, **overrides)


def parse_date(ts: str | None) -> datetime:
    """Episode time for Graphiti's temporal model: ISO, LoCoMo ("1:56 pm on 8 May, 2023") or
    LongMemEval ("2023/04/10 (Mon) 17:50"); relative dates (DMR "7 days ago") fall back to now."""
    for fmt in (None, "%I:%M %p on %d %B, %Y", "%Y/%m/%d (%a) %H:%M"):
        try:
            d = datetime.fromisoformat(ts) if fmt is None else datetime.strptime(ts, fmt)
            return d if d.tzinfo else d.replace(tzinfo=timezone.utc)
        except (TypeError, ValueError):
            continue
    return datetime.now(timezone.utc)


def group(ns: str) -> str:
    return ns.replace("/", "_")  # group ids allow letters, digits, "-" and "_" only


class GraphitiSystem:
    def __init__(self):
        url, model, key = llm()
        os.environ["SEMAPHORE_LIMIT"] = str(CONCURRENCY)
        cfg = LLMConfig(api_key=key, model=model, small_model=model, base_url=url)
        self.loop = asyncio.new_event_loop()
        self.g = Graphiti(NEO4J, "neo4j", "fake-test-only", llm_client=OutputCappedClient(config=cfg, max_tokens=MAX_OUT),
                          embedder=OpenAIEmbedder(OpenAIEmbedderConfig(embedding_model=EMBED_MODEL, api_key="local", base_url=EMBED_URL, embedding_dim=EMBED_DIMS)),
                          cross_encoder=OpenAIRerankerClient(config=cfg))
        self.loop.run_until_complete(self.g.build_indices_and_constraints())
        self.docs: dict[str, str] = {}  # episode uuid -> doc ids
        self.cleared: set[str] = set()
        self.sessions: set[str] = set()

    def ingest(self, ns, docs):
        ns = group(ns)
        if ns not in self.cleared:  # a namespace starts empty even on a reused container
            self.loop.run_until_complete(clear_data(self.g.driver, group_ids=[ns]))
            self.cleared.add(ns)
        if docs and docs[0].get("speaker"):
            self.sessions.add(ns)
            body = "\n".join(d["text"] for d in docs)  # the runner's text already carries speaker and date
            eps = [(",".join(d["id"] for d in docs), body, EpisodeType.message, docs[0].get("timestamp"))]
        else:
            eps = [(d["id"], f"{d['title']}\n{d['text']}" if d.get("title") else d["text"], EpisodeType.text, None) for d in docs]
        for ids, body, kind, ts in eps:  # episodes of one group in order, as Graphiti requires
            when = parse_date(ts)
            r = self.loop.run_until_complete(self.g.add_episode(name=ids[:200], episode_body=body, source_description=ns, reference_time=when, source=kind, group_id=ns))
            self.docs[r.episode.uuid] = ids

    def search(self, ns, query, k):
        edges = self.loop.run_until_complete(self.g.search(query, group_ids=[group(ns)], num_results=k))
        mems = [([d for ep in e.episodes for d in self.docs.get(ep, "").split(",") if d], 1.0 / (i + 1), e.fact) for i, e in enumerate(edges)]
        return rank(mems, k, group(ns) in self.sessions)

    def close(self):
        self.loop.run_until_complete(self.g.close())
        self.loop.close()


def open(workdir: str, profile: str) -> GraphitiSystem:
    if profile != "default":
        raise ValueError("graphiti profile is default")
    return GraphitiSystem()
