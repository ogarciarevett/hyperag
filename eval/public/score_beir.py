"""Scores TREC run files with pytrec_eval exactly as the BEIR toolkit's EvaluateRetrieval does:
nDCG@10, Recall@100, MAP@100 and MRR@10 over the official test qrels, ignoring a hit whose id equals
the query id (BEIR's default ignore_identical_ids, which matters on ArguAna and Quora).

.venv-bench/bin/python -I eval/public/score_beir.py --data <BEIR dir> eval/public/results/*.run
"""
import argparse, csv, json, os, sys
import pytrec_eval

p = argparse.ArgumentParser()
p.add_argument("--data", default="data/beir")
p.add_argument("runs", nargs="+")
a = p.parse_args()

rows = []
for path in a.runs:
    meta = json.load(open(path.removesuffix(".run") + ".meta.json"))
    qrels = {}
    with open(os.path.join(a.data, meta["dataset"], "qrels", "test.tsv")) as f:
        for r in csv.DictReader(f, delimiter="\t"):
            qrels.setdefault(r["query-id"], {})[r["corpus-id"]] = int(r["score"])
    run = {}
    for line in open(path):
        if not line.strip():
            continue
        qid, _, did, _, score, _ = line.split()
        if qid == did:
            continue
        run.setdefault(qid, {})[did] = float(score)
    # a --limit run scores exactly the queries it asked; empty or failed ones score 0
    asked = set(meta["askedIds"]) if "askedIds" in meta else set(qrels)
    for q in asked:
        run.setdefault(q, {})
    ev = pytrec_eval.RelevanceEvaluator({q: qrels[q] for q in asked}, {"ndcg_cut.10", "recall.100", "map_cut.100", "recip_rank"})
    # pytrec_eval ranks by score; feed rank-derived scores so the system's own order is kept.
    ranked = {q: {d: 1.0 / (i + 1) for i, d in enumerate(sorted(docs, key=docs.get, reverse=True))} for q, docs in run.items() if q in asked}
    per = ev.evaluate(ranked)
    mean = lambda m: sum(v[m] for v in per.values()) / len(per)
    rows.append({"dataset": meta["dataset"], "system": meta["system"], "version": meta["version"], "profile": meta["profile"],
                 "queries": len(per), "test_queries": len(qrels), "ndcg@10": mean("ndcg_cut_10"), "recall@100": mean("recall_100"), "map@100": mean("map_cut_100"),
                 "p50_ms": meta["latencyP50Ms"], "p95_ms": meta["latencyP95Ms"], "ingest_s": meta["ingestMs"] / 1000, "date": meta["date"][:10]})
print("| dataset | system | profile | queries | nDCG@10 | Recall@100 | MAP@100 | p50 ms | p95 ms | ingest s |")
print("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |")
for r in rows:
    print(f"| {r['dataset']} | {r['system']} {r['version']} | {r['profile']} | {r['queries']} | {r['ndcg@10']:.3f} | {r['recall@100']:.3f} | {r['map@100']:.3f} | {r['p50_ms']:.1f} | {r['p95_ms']:.1f} | {r['ingest_s']:.0f} |")
json.dump(rows, open(os.path.join(os.path.dirname(a.runs[0]), "beir-scores.json"), "w"), indent=1)
