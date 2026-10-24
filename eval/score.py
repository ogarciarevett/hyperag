"""Scores Hyperag and Engram runs with the baseline's own scorer and prints a per-metric table.

python3 -I eval/score.py --artifacts <hyperag-artifacts dir> --suite cross-team --runs eval/results/cross-team-*.jsonl
The scorer module (benchmarks/packaged/evaluate.py) is imported unchanged from the artifacts.
"""
import argparse
import glob
import importlib.util
import json
import os
import sys

p = argparse.ArgumentParser()
p.add_argument("--artifacts", required=True)
p.add_argument("--baseline-results", required=True, help="folder holding the sealed packaged-20261021-v4 and -quick runs")
p.add_argument("--suite", default="cross-team")
p.add_argument("--runs", nargs="+", required=True)
p.add_argument("--json-out")
a = p.parse_args()

spec = importlib.util.spec_from_file_location("evaluate", os.path.join(a.artifacts, "benchmarks/packaged/evaluate.py"))
ev = importlib.util.module_from_spec(spec)
sys.modules["evaluate"] = ev
spec.loader.exec_module(ev)

data = os.path.join(a.artifacts, "benchmarks/packaged/data", a.suite)
corpus = json.load(open(os.path.join(data, "corpus.json")))
queries = {q["id"]: q for q in json.load(open(os.path.join(data, "queries.json")))}
index = ev.CorpusIndex.build(corpus)


def score(records):
    """records: (query_id, response, latency_ms, workload, concurrency)."""
    scored = [ev.score_query(queries[qid], resp, index, latency_ms=lat, workload=w, concurrency=c) for qid, resp, lat, w, c in records]
    quality = [r for r in scored if r["workload"] == "quality"]
    out = ev.aggregate(quality, expected_query_ids=queries)
    first = {r["query_id"]: r for r in quality}
    answerable = [r for r in first.values() if r["answerable"]]
    out["false_abstention_rate"] = ev._mean(r["abstained"] for r in answerable if r.get("abstained") is not None)
    out["handoff_all_evidence_at_10"] = out["strata"]["category"].get("cross_team_handoff", {}).get("all_evidence_at_10")
    for c in (4, 8):
        lat = [r["latency_ms"] for r in scored if r["workload"] == "load" and r.get("concurrency") == c]
        if lat:
            out[f"latency_p50_c{c}"] = ev.percentile(lat, 0.5)
            out[f"latency_p95_c{c}"] = ev.percentile(lat, 0.95)
    return out, quality


def load_run(path):
    rows = [json.loads(line) for line in open(path) if line.strip()]
    return [(r["query_id"], r["response"], r["latency_ms"], r["workload"], r["concurrency"]) for r in rows]


def baseline_runs():
    """The sealed baseline profiles: re-scored from their own recorded quality requests."""
    found = {}
    for run in ["packaged-20261021-v4", "packaged-20261021-quick"]:
        for path in glob.glob(os.path.join(a.baseline_results, run, "*", a.suite, "requests.jsonl")):
            engine = path.split(os.sep)[-3]
            rows = [json.loads(line) for line in open(path) if line.strip()]
            quality = [r for r in rows if r.get("workload") == "quality"]
            # The agents-runtime adapter is the one baseline engine whose responses report `retrieval`.
            if quality and "retrieval" in (quality[0].get("raw_response") or {}):
                engine = "agents-runtime-v4"
            recs = [(r["query_id"], r.get("raw_response") or {"ok": False}, r.get("end_to_end_ms"), "quality", 1)
                    for r in quality if r.get("concurrency", 1) == 1]
            found[f"baseline:{engine}"] = recs
    return found


results, firsts = {}, {}
for name, recs in baseline_runs().items():
    try:
        results[name], firsts[name] = score(recs)
    except ValueError as error:
        print(f"skip {name}: {error}", file=sys.stderr)
for path in a.runs:
    name = os.path.basename(path).removesuffix(".jsonl")
    results[name], firsts[name] = score(load_run(path))

COLS = ["ndcg_at_10", "recall_at_10", "all_evidence_at_10", "handoff_all_evidence_at_10", "probe_explicit_abstention_rate",
        "false_abstention_rate", "cross_scope_hit_count", "latency_p50_ms", "latency_p95_ms", "latency_p95_c8"]
fmt = lambda v: "n/a" if v is None else (f"{v:.3f}" if isinstance(v, float) else str(v))
print("| run | " + " | ".join(COLS) + " |")
print("|" + " --- |" * (len(COLS) + 1))
for name, r in results.items():
    print(f"| {name} | " + " | ".join(fmt(r.get(c)) for c in COLS) + " |")
print()
print("| run | lang en nDCG | lang es nDCG | " + " | ".join(sorted(next(iter(results.values()))["strata"]["category"])) + " (all evidence@10) |")
for name, r in results.items():
    cats = r["strata"]["category"]
    lang = r["strata"]["language"]
    print(f"| {name} | {fmt(lang['en']['ndcg_at_10'])} | {fmt(lang['es']['ndcg_at_10'])} | " + " | ".join(fmt(cats[c]["all_evidence_at_10"]) for c in sorted(cats)) + " |")

if a.json_out:
    paired = {}
    base = "baseline:agents-runtime-v4"
    for name in results:
        if name != base and base in firsts and not name.startswith("baseline:"):
            paired[f"{name} - {base}"] = ev.compare_paired(firsts[name], firsts[base], bootstrap_samples=10000)
    json.dump({"results": results, "paired": paired}, open(a.json_out, "w"), indent=1, default=str)
