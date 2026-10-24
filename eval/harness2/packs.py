"""Two meta-harness packs over the dev suite. Every item carries the same saved Hyperag retrieval.

handoff: return the memory ids that together answer a handoff or multi-evidence question, or [] to
         abstain. Quality: F1 of the cited set against the required evidence; abstention items score 1
         only for [].
engineering: fill the requested release-record fields (owner, rollback image, root cause, launch
         date) from the hits. Quality: share of fields that match exactly. Every value must appear in
         a retrieved hit, so an invented value is a hard failure.
"""
import json
from pathlib import Path
import re

from harness.loop.packs import Pack

DATA = Path(__file__).resolve().parent / "data"
HANDOFF = {"cross_team_handoff", "multi_evidence_2", "multi_evidence_3", "unanswerable", "unauthorized_probe"}
FIELDS = {"owner": ("owner", r"technical owner is ([^.]+)\."),
          "incident": ("root_cause", r"root cause is ([^.]+)\."),
          "rollback": ("rollback_image", r"restore image (\S+)\. Preserve"),
          "schedule-current": ("launch_date", r"launch is (November \d+,2026)")}


def norm(text):
    return " ".join(text.casefold().split())


def load():
    docs = {d["id"]: d for d in json.loads((DATA / "parents.json").read_text())}
    queries = json.loads((DATA / "queries.json").read_text())
    hits = json.loads((DATA / "hits.json").read_text())["hits"]
    return docs, queries, hits


def item(q, hits):
    return {"question": q["query"], "scope": q["scope"], **hits[q["id"]]}


class Handoff(Pack):
    name, seed = "handoff", "handoff-top"

    def __init__(self):
        _, queries, hits = load()
        self.rows = [{"id": q["id"], "key": q["id"], "input": item(q, hits), "label": q["gold_ids"]}
                     for q in queries if q["category"] in HANDOFF]

    def quality(self, prediction, row):
        gold, cited = set(row["label"]), set(prediction)
        if not gold:
            return float(not cited)
        right = len(gold & cited)
        return 2 * right / (len(gold) + len(cited)) if right else 0.

    def hard_fail(self, prediction, row):
        if not isinstance(prediction, list) or not all(isinstance(i, str) for i in prediction):
            return "prediction is not a list of memory ids"
        if len(set(prediction)) != len(prediction):
            return "cites a memory twice"
        if set(prediction) - {h["id"] for h in row["input"]["hits"]}:
            return "cites a memory outside the retrieved hits"


class Engineering(Pack):
    name, seed = "engineering", "engineering-phrase"

    def __init__(self):
        docs, queries, hits = load()
        self.rows = []
        for q in queries:
            kinds = [docs[g]["artifact"] for g in q["gold_ids"]]
            if q["scope"] != "engineering" or not kinds or not set(kinds) <= set(FIELDS):
                continue
            label = {}
            for gold, kind in zip(q["gold_ids"], kinds):
                field, pattern = FIELDS[kind]
                label[field] = re.search(pattern, docs[gold]["text"]).group(1)
            self.rows.append({"id": q["id"], "key": q["id"], "input": {**item(q, hits), "fields": sorted(label)},
                              "label": label})

    def quality(self, prediction, row):
        return sum(norm(prediction.get(f, "")) == norm(v) for f, v in row["label"].items()) / len(row["label"])

    def hard_fail(self, prediction, row):
        if not isinstance(prediction, dict) or not all(isinstance(v, str) for v in prediction.values()):
            return "prediction is not a mapping of field to text"
        if set(prediction) != set(row["input"]["fields"]):
            return "prediction does not fill exactly the requested fields"
        texts = [norm(h["text"]) for h in row["input"]["hits"]]
        if any(not any(norm(v) in t for t in texts) for v in prediction.values()):
            return "a value does not appear in any retrieved hit"


PACKS = {"handoff": Handoff, "engineering": Engineering}
