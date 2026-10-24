"""Generate five source-backed SVG benchmark figures; incomplete runs remain unscored.

    python3 -I eval/public/benchmark_charts.py --results eval/public/results --out docs/benchmarks
"""
import argparse
import html
import json
import math
import tempfile
from pathlib import Path

DEFAULT = ("hyperag", "egemma2+bgev2m3+graph")
COLORS = ["#55958c", "#8b915c", "#b08c55", "#987b9a"]
COMPETITORS = ["cognee", "engram", "graphiti", "hindsight", "langmem", "letta", "mem0", "memvid"]
CHARTS = [("locomo", "LoCoMo", "F1, categories 1–4", False),
          ("dmr", "DMR", "Answer accuracy", True),
          ("lme-oracle", "LongMemEval oracle", "Oracle haystack, top-10 retrieval · accuracy", True),
          ("lme-s100", "LongMemEval S100", "Reporting subset, 100 questions · accuracy", True),
          ("scifact", "BEIR SciFact", "Official test split · nDCG@10", False)]


def score(value):
    return value if type(value) in (int, float) and math.isfinite(value) and 0 <= value <= 1 else None


def load_results(root):
    values = {}
    keys = {DEFAULT, *((name, "default") for name in COMPETITORS)}
    for path in sorted((root / "memory").glob("*.meta.json")):
        m = json.loads(path.read_text())
        if m.get("ingestOnly") or ".ingest." in path.name or ".limit" in path.name:
            continue
        bench = path.name.split(".")[0]
        key = (m["system"], m["profile"])
        keys.add(key)
        count = m.get("questions")
        complete = type(count) is int and count > 0 and count == m.get("expectedQuestions")
        value = (m.get("locomo") or {}).get("f1_cat1_4") if bench == "locomo" else m.get("accuracy")
        values[(bench, key)] = score(value) if complete else None
    path = root / "beir-scores.json"
    if path.exists():
        for m in json.loads(path.read_text()):
            key = (m["system"], m["profile"])
            keys.add(key)
            complete = type(m.get("queries")) is int and m["queries"] > 0 and m["queries"] == m.get("test_queries")
            values[(m["dataset"], key)] = score(m["ndcg@10"]) if complete else None
    return sorted(keys, key=lambda k: (k != DEFAULT, k[0] != "hyperag", k)), values


def color(system):
    if system == "hyperag":
        return "#5283a3"
    return COLORS[COMPETITORS.index(system) % len(COLORS)]


def label(key):
    system, profile = key
    if key == DEFAULT:
        return "Hyperag · default template (graph on)"
    if system == "hyperag":
        return f"Hyperag · {profile}" + (" · graph on" if "graph" in profile.split("+") else " · graph off")
    return f"{system} · {profile}"


def render(bench, title, metric, percent, keys, values):
    # Zero is visible; absent/partial runs have labels but no bar or invented zero.
    left, width, top, row, right = 420, 370, 90, 31, 80
    height = top + row * len(keys) + 75
    esc = html.escape
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" width="880" height="{height}" viewBox="0 0 880 {height}" role="img" aria-labelledby="title description">',
           f'<title id="title">{esc(title)} — {esc(metric)}</title>',
           '<desc id="description">Completed runs from one evaluation window. N/D means not run or incomplete, never zero. Hyperag is blue; competitor colors are fixed across charts.</desc>',
           '<style>text{font-family:system-ui,-apple-system,Segoe UI,sans-serif;fill:#59636e;font-size:14px}.title{font-size:22px;font-weight:650}.small{font-size:12px}.grid{stroke:#a0a8b0;stroke-opacity:.25}.axis{stroke:#89939d}@media(prefers-color-scheme:dark){text{fill:#a9b3bd}.grid{stroke:#84909b;stroke-opacity:.28}.axis{stroke:#9ca7b2}}</style>',
           f'<text class="title" x="14" y="30">{esc(title)}</text>',
           f'<text x="14" y="54">{esc(metric)} · higher is better · one window, October 23–24, 2026</text>']
    for tick in (0, .25, .5, .75, 1):
        x = left + width * tick
        tick_label = f"{tick:.0%}" if percent else f"{tick:g}"
        out += [f'<line class="grid" x1="{x:g}" y1="{top-15}" x2="{x:g}" y2="{top+row*len(keys)-12}"/>',
                f'<text class="small" text-anchor="middle" x="{x:g}" y="{top-24}">{tick_label}</text>']
    for i, key in enumerate(keys):
        y = top + i * row
        value = values.get((bench, key))
        name = esc(label(key))
        out.append(f'<text x="14" y="{y+13}">{name}</text>')
        if value is None:
            out.append(f'<text x="{left+8}" y="{y+13}">N/D</text>')
        else:
            value_label = f"{value:.1%}" if percent else f"{value:.3f}"
            out.append(f'<rect x="{left}" y="{y}" width="{width*value:.3f}" height="18" fill="{color(key[0])}"><title>{name}: {value_label}</title></rect>')
            out.append(f'<text x="{left+width*value+7:.3f}" y="{y+13}">{value_label}</text>')
    bottom = top + row * len(keys)
    out += [f'<line class="axis" x1="{left}" y1="{top-15}" x2="{left}" y2="{bottom-12}"/>',
            f'<text class="small" x="14" y="{bottom+18}">N/D = unrun or incomplete. Failed native searches remain scored by the common harness.</text>',
            f'<text class="small" x="14" y="{bottom+39}">Raw scores and completion checks: eval/public/results. Graph is off unless marked on.</text>', '</svg>']
    return "\n".join(out) + "\n"


def generate(root, out):
    keys, values = load_results(root)
    out.mkdir(parents=True, exist_ok=True)
    for chart in CHARTS:
        (out / f"{chart[0]}.svg").write_text(render(*chart, keys, values))
    return keys, values


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--results", type=Path, default=Path("eval/public/results"))
    parser.add_argument("--out", type=Path, default=Path("docs/benchmarks"))
    parser.add_argument("--check", action="store_true", help="fail if committed figures differ from the result metadata")
    args = parser.parse_args()
    if args.check:
        with tempfile.TemporaryDirectory() as directory:
            generated = Path(directory)
            generate(args.results, generated)
            stale = [p.name for p in generated.glob("*.svg") if not (args.out / p.name).exists() or p.read_bytes() != (args.out / p.name).read_bytes()]
            if stale:
                raise SystemExit("Stale benchmark figures: " + ", ".join(sorted(stale)))
        print("Benchmark figures match the result metadata.")
    else:
        generate(args.results, args.out)
        print("Generated five benchmark figures from result metadata.")
