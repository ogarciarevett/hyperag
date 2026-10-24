"""Writes the README benchmark tables from the result files, between the bench markers.

    python3 -I eval/public/readme_table.py [--results eval/public/results] [--readme README.md]

One table per benchmark family, one row per system and profile: Hyperag's default template first,
then other Hyperag profiles, then every other system. A missing observation is N/D, never zero.
BEIR rows show nDCG@10 (official test split, pytrec_eval); memory rows show the benchmark's own
metric (LoCoMo token F1, LongMemEval and DMR judge accuracy).
"""
import argparse
import glob
import json
import math
import os
import re

# The default template has the graph on (Neo4j); Hyperag rows without +graph are labelled graph off.
DEFAULT = ("hyperag", "egemma2+bgev2m3+graph")
BEIR = ["scifact", "nfcorpus", "arguana", "scidocs", "fiqa", "trec-covid"]
# Published BM25 nDCG@10, BEIR paper (Thakur et al., 2021, table 2), for reference.
BEIR_BM25 = {"scifact": 0.665, "nfcorpus": 0.325, "arguana": 0.315, "scidocs": 0.158, "fiqa": 0.236, "trec-covid": 0.656}
MEMORY = [("locomo", "LoCoMo F1 (cat 1 to 4)"), ("lme-s100", "LME-S subset n=100"), ("lme-s", "LME-S n=500 (includes dev)"), ("lme-oracle", "LME oracle"), ("dmr", "MSC/DMR")]

p = argparse.ArgumentParser()
p.add_argument("--results", default="eval/public/results")
p.add_argument("--readme", default="README.md")
a = p.parse_args()


def order(key):
    system, profile = key
    return (0 if key == DEFAULT else 1 if system == "hyperag" else 2, system, profile)


def name(key):
    if key == DEFAULT:
        return "**Hyperag (default template)**"
    if key[0] == "hyperag" and "graph" not in key[1].split("+"):
        return "hyperag, graph off (not the default template)"
    return key[0]


def fmt(v):
    return "N/D" if v is None else f"{v:.3f}"


beir = {}
path = os.path.join(a.results, "beir-scores.json")
if os.path.exists(path):
    for r in json.load(open(path)):
        # A run over part of the test split is not an official score.
        full = r.get("test_queries") is not None and r["queries"] == r["test_queries"]
        beir.setdefault((r["system"], r["profile"]), {})[r["dataset"]] = r["ndcg@10"] if full else None

memory = {}
for f in glob.glob(os.path.join(a.results, "memory", "*.meta.json")):
    filename = os.path.basename(f)
    m = json.load(open(f))
    # Ingest-only and --limit runs are partial; they never fill a cell.
    if m.get("ingestOnly") or ".ingest." in filename or re.search(r"\.limit\d+\.", filename):
        continue
    bench = filename.split(".")[0]  # memory.ts writes "bench" as a display label; the file name has the id
    value = (m.get("locomo") or {}).get("f1_cat1_4") if bench == "locomo" else m.get("accuracy")
    count = m.get("questions")
    if (type(count) is not int or count <= 0 or count != m.get("expectedQuestions")
            or type(value) not in (int, float) or not math.isfinite(value) or not 0 <= value <= 1):
        value = None
    memory.setdefault((m["system"], m["profile"]), {})[bench] = value

lines = ["### BEIR (nDCG@10, official test split)", "", "| System | Profile | " + " | ".join(BEIR) + " |", "|" + " --- |" * (len(BEIR) + 2)]
lines.append("| BM25, published (BEIR paper) | | " + " | ".join(fmt(BEIR_BM25.get(d)) for d in BEIR) + " |")
beir.setdefault(DEFAULT, {})
memory.setdefault(DEFAULT, {})
for key in sorted(beir, key=order):
    label = name(key)
    lines.append(f"| {label} | {key[1]} | " + " | ".join(fmt(beir[key].get(d)) for d in BEIR) + " |")
lines += ["", "### Agent memory (one open model as generator and judge for every system)", "", "| System | Profile | " + " | ".join(m[1] for m in MEMORY) + " |", "|" + " --- |" * (len(MEMORY) + 2)]
for key in sorted(memory, key=order):
    label = name(key)
    lines.append(f"| {label} | {key[1]} | " + " | ".join(fmt(memory.get(key, {}).get(m[0])) for m in MEMORY) + " |")
lines += ["", "N/D: not observed (not run, or the run did not finish). Reproduce: `bun eval/public/beir.ts`, then `python3 eval/public/score_beir.py eval/public/results/*.run` (every run file: it rewrites beir-scores.json), `bun eval/public/memory.ts`, `python3 eval/public/score_locomo.py` for LoCoMo, then this script."]
lines += ["", "The full LME-S n=500 includes the frozen dev questions and is not held out for the temporal channel. The n=100 subset is reserved for reporting; see [the temporal protocol](docs/temporal.md)."]
table = "\n".join(lines)

readme = open(a.readme).read()
start, end = "<!-- bench:start -->", "<!-- bench:end -->"
if start not in readme or end not in readme.split(start, 1)[1]:
    raise SystemExit(f"{a.readme} needs {start} followed by {end}")
readme = re.sub(re.escape(start) + r".*?" + re.escape(end), lambda _: start + "\n" + table + "\n" + end, readme, count=1, flags=re.S)
open(a.readme, "w").write(readme)
print(f"wrote {len(beir)} BEIR rows and {len(memory)} memory rows")
