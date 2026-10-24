"""Memvid through its Python SDK with its defaults: create() (Tantivy lexical index), put_many,
find() mode "auto" (lexical when no OpenAI key is set). One .mv2 file per namespace, telemetry off.
Semantic mode is blocked on this SDK version: see SYSTEMS.md.

  uv venv --python 3.12 eval/public/.venv-memvid
  uv pip install -c eval/public/python-ca-constraints.txt --python eval/public/.venv-memvid/bin/python memvid-sdk==2.0.160
"""
import os
from importlib.metadata import version

os.environ["MEMVID_TELEMETRY"] = "0"
import memvid_sdk  # noqa: E402

VERSION = {"memvid-sdk": version("memvid-sdk")}
CONFIG = {"create": "defaults (lex only)", "ingest": "put_many, then commit", "find": "mode=auto (lex without OpenAI key)", "asOf": "none (its search has only a time filter, and stored times are load times, so a 2023 date would hide every memory)"}


class Memvid:
    def __init__(self, workdir: str):
        self.workdir = workdir
        self.files: dict[str, memvid_sdk.Memvid] = {}

    def mv(self, ns: str):
        if ns not in self.files:
            self.files[ns] = memvid_sdk.create(os.path.join(self.workdir, ns.replace("/", "_") + ".mv2"))
        return self.files[ns]

    def ingest(self, ns, docs):
        # put_many's returned frame ids do not match find()'s frame_id, so the doc id travels in the uri
        reqs = [{"title": d.get("title", ""), "label": "doc", "text": d["text"], "uri": f"mv2://doc/{d['id']}"} for d in docs]
        for i in range(0, len(reqs), 100):  # a 512-doc put_many hangs in the core (thread park); 100 does not
            self.mv(ns).put_many(reqs[i : i + 100])
            self.mv(ns).commit()  # put_many skips checkpoints; find() sees no index until a commit

    def search(self, ns, query, k):
        out: dict[str, dict] = {}
        # long docs are stored as pages (uri "#page-N"); a doc ranks by its best page, as in MaxP
        for h in self.mv(ns).find(query, k=k)["hits"]:
            doc = h["uri"].removeprefix("mv2://doc/").split("#")[0]
            out.setdefault(doc, {"id": doc, "score": float(h.get("score") or 0), "text": h.get("snippet", "")})
        return list(out.values())

    def close(self):
        for f in self.files.values():
            f.close()


def open(workdir: str, profile: str) -> Memvid:
    if profile != "default":
        raise ValueError("memvid profile is default")
    return Memvid(workdir)
