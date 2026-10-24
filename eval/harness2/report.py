"""Collect both sides' saved results into results.json and print the table. Never reruns a model."""
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
PACKS = ("handoff", "engineering")
PHASES = ("baseline", "propose", "probe", "compose")


def fmt(value, digits=4):
    return "n/a" if value is None else f"{value:.{digits}f}"


def verdict(a, b):
    if a.get("status") != "complete" or b.get("status") != "complete":
        return "no verdict: a side did not finish"
    if a["score"] == b["score"]:
        return "tie"
    return ("A" if a["score"] > b["score"] else "B") + f" higher by {abs(a['score'] - b['score']):.4f} (one trial)"


def main():
    rows, out = [], {}
    for pack in PACKS:
        root = HERE / "runs/round2" / pack
        sides = {s: json.loads((root / s / "result.json").read_text()) if (root / s / "result.json").exists() else {"status": "not run"}
                 for s in ("A", "B")}
        phases = json.loads((root / "B/phases.json").read_text()) if (root / "B/phases.json").exists() else {}
        out[pack] = {**sides, "B_phases": phases, "verdict": verdict(sides["A"], sides["B"])}
        for side, r in sides.items():
            reached = "n/a (native loop)" if side == "A" else ", ".join(p for p in PHASES if phases.get("phases", {}).get(p)) or "none"
            changed = "n/a" if side == "A" else {True: "yes", False: "no (no-op)", None: "unknown"}[phases.get("compose_changed_harness")]
            base = r.get("own_baseline", {}).get("score")
            rows.append(f"| {pack} | {side} | {r.get('status')} | {fmt(r.get('score'))} | "
                        f"{ {True: 'pass', False: 'fail', None: 'n/a'}[r.get('pass')] } | {fmt(r.get('wall_seconds'), 1)} | "
                        f"{reached} | {changed} | {fmt(base)} | {fmt(r.get('delta_vs_own_baseline'))} |")
    (HERE / "results.json").write_text(json.dumps(out, indent=1) + "\n")
    table = ["| Task | Side | Status | Sealed score | Pass | Active s | Phases reached | Compose changed harness | B own baseline | B delta |",
             "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |", *rows, "",
             *(f"- {pack}: {out[pack]['verdict']}" for pack in PACKS)]
    print("\n".join(table))


if __name__ == "__main__":
    main()
