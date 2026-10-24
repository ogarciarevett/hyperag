"""Scores memory.ts LoCoMo answers with the official metric, ported verbatim from
snap-research/locomo@3eb6f2c task_eval/evaluation.py (normalize_answer, Porter-stemmed token F1;
category 1 split into sub-answers; category 3 keeps the first ';' answer) and gpt_utils.py
get_cat_5_answer (category 5: the chosen option must be "Not mentioned").

  uv pip install --python .venv-bench/bin/python nltk==3.9.2 regex==2025.11.3
  .venv-bench/bin/python -I eval/public/score_locomo.py eval/public/results/memory/locomo.*.jsonl
"""
import json
import string
import sys
from collections import Counter
from pathlib import Path

import regex
from nltk.stem import PorterStemmer

ps = PorterStemmer()


def normalize_answer(s):
    s = s.replace(",", "")
    s = regex.sub(r"\b(a|an|the|and)\b", " ", "".join(ch for ch in s.lower() if ch not in set(string.punctuation)))
    return " ".join(s.split())


def f1_score(prediction, ground_truth):
    p = [ps.stem(w) for w in normalize_answer(prediction).split()]
    g = [ps.stem(w) for w in normalize_answer(ground_truth).split()]
    same = sum((Counter(p) & Counter(g)).values())
    if same == 0:
        return 0
    precision, recall = same / len(p), same / len(g)
    return 2 * precision * recall / (precision + recall)


def f1_multi(prediction, ground_truth):
    preds = [x.strip() for x in prediction.split(",")]
    golds = [x.strip() for x in ground_truth.split(",")]
    return sum(max(f1_score(p, g) for p in preds) for g in golds) / len(golds)


def cat5_choice(prediction, options):
    p = prediction.strip().lower()
    if len(p) == 1:
        return options["a"] if "a" in p else options["b"]
    if len(p) == 3:
        return options["a"] if "(a)" in p else options["b"]
    return prediction


def score(row):
    if row.get("searchError"):  # a failed search scores 0
        return 0.0
    cat, out, gold = int(row["type"]), row["answer"], row["gold"]
    if cat == 3:
        gold = gold.split(";")[0].strip()
    if cat in (2, 3, 4):
        return f1_score(out, gold)
    if cat == 1:
        return f1_multi(out, gold)
    out = cat5_choice(out, row["extra"]["options"])
    return 1.0 if "no information available" in out.lower() or "not mentioned" in out.lower() else 0.0


for path in sys.argv[1:]:
    rows = [json.loads(l) for l in open(path) if l.strip()]
    meta = Path(path.removesuffix(".jsonl") + ".meta.json")
    m = json.loads(meta.read_text()) if meta.exists() else {}
    if m.get("expectedQuestions") is None or len(rows) != m["expectedQuestions"]:
        # a run that died partway is never scored on the questions it reached
        print(Path(path).name, json.dumps({"f1_cat1_4": "N/D", "reason": f"{len(rows)} rows, expected {m.get('expectedQuestions', 'unknown')}"}))
        continue
    by: dict[str, list[float]] = {}
    for r in rows:
        by.setdefault(r["type"], []).append(score(r))
    main = [s for t, xs in by.items() if t != "5" for s in xs]
    result = {"f1_cat1_4": round(sum(main) / len(main), 4) if main else "N/D",
              "per_category": {t: {"n": len(xs), "f1": round(sum(xs) / len(xs), 4)} for t, xs in sorted(by.items())}}
    m["locomo"] = result
    meta.write_text(json.dumps(m, indent=2) + "\n")
    print(Path(path).name, json.dumps(result))
