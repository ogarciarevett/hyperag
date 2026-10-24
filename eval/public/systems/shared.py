"""What every Python adapter shares: the one open-model endpoint (OpenAI-compatible, on the rented
GPU) for every LLM stage, and the local bge-small embedder (eval/fastembed_server.py), so no
system gets a better model than another.

  EVAL_LLM_BASE_URL, EVAL_LLM_MODEL, EVAL_LLM_API_KEY   the shared generator; unset means blocked
  EVAL_EMBED_BASE_URL                                     default http://127.0.0.1:58322/v1
  EVAL_CONCURRENCY                                        parallel ingestion calls, default 8
"""
import os

EMBED_URL = os.environ.get("EVAL_EMBED_BASE_URL", "http://127.0.0.1:58322/v1")
EMBED_MODEL = "BAAI/bge-small-en-v1.5"
EMBED_DIMS = 384
CONCURRENCY = int(os.environ.get("EVAL_CONCURRENCY", "8"))


def llm() -> tuple[str, str, str]:
    """(base_url, model, api_key) of the shared endpoint."""
    url = os.environ.get("EVAL_LLM_BASE_URL")
    if not url:
        raise RuntimeError("blocked: needs the shared LLM endpoint (set EVAL_LLM_BASE_URL, EVAL_LLM_MODEL, EVAL_LLM_API_KEY)")
    return url, os.environ["EVAL_LLM_MODEL"], os.environ.get("EVAL_LLM_API_KEY", "none")


def best_per_doc(hits) -> list[dict]:
    """Many memories can come from one doc; a doc ranks by its best memory (MaxP)."""
    out: dict[str, dict] = {}
    for h in hits:
        if h["id"] is not None:
            out.setdefault(h["id"], h)
    return list(out.values())


def rank(memories, k: int, sessions: bool) -> list[dict]:
    """memories: (doc ids, score, text), best first. For a conversation (sessions) every memory is
    one hit, since the answer model reads memories; for BEIR docs rank by their best memory."""
    if sessions:
        return [{"id": ids[0] if ids else None, "score": score, "text": text} for ids, score, text in memories][:k]
    return best_per_doc({"id": d, "score": score, "text": text} for ids, score, text in memories for d in ids)[:k]


def iso_date(s):
    """A benchmark date as ISO UTC, as dates.ts parses it: LoCoMo "1:56 pm on 8 May, 2023" or
    LongMemEval "2023/05/30 (Tue) 23:40"; None for anything else (DMR's "7 days ago")."""
    from datetime import datetime, timezone

    for fmt in ("%I:%M %p on %d %B, %Y", "%Y/%m/%d (%a) %H:%M"):
        try:
            return datetime.strptime(s, fmt).replace(tzinfo=timezone.utc).isoformat().replace("+00:00", "Z")
        except (TypeError, ValueError):
            continue
    return None
