"""Run the harness2 pilot runner (round2.py, unchanged) on the Hyperag dev packs.

Side A: native meta-harness, one Codex proposal plus one native search round.
Side B: harness2 at the pinned commit, parallel mode, k=1, one Propose, Probe, Compose step.
Both sides: same packs, same saved Hyperag retrieval, gpt-6.1-sol medium, 1200 active seconds,
Codex sandbox on, command network off. The pilot folder is only read.
Usage: PILOT=/path/to/harness2-pilot $PILOT/.venv/bin/python eval/harness2/run.py --pack handoff
"""
import argparse
import json
import os
import subprocess
import time
from pathlib import Path
import sys

sys.dont_write_bytecode = True  # never write caches into the read-only pilot folder
HERE = Path(__file__).resolve().parent
sys.path[:0] = [os.environ["PILOT"], str(HERE)]
import round2
from harness.loop import loop, packs as meta_packs
import packs

meta_packs.PACKS.update(packs.PACKS)
loop.SEEDS = HERE / "seeds"
round2.ROOT = HERE
round2.TASK = round2.TASK.replace(
    "using the supplied examples. ",
    "using the supplied examples. Each item carries its question and the memory hits one Hyperag "
    "retrieval returned for it; use only those hits. ")
assert "Hyperag" in round2.TASK
_last_sample = [0., 100, 0.]


def _sample():
    """Free memory percent and used swap in MB, sampled at most every 15 seconds (memory_pressure is slow)."""
    if time.monotonic() - _last_sample[0] > 15:
        line = subprocess.run(["memory_pressure"], capture_output=True, text=True).stdout.strip().splitlines()[-1]
        swap = subprocess.run(["sysctl", "-n", "vm.swapusage"], capture_output=True, text=True).stdout.split()[5]
        _last_sample[:] = [time.monotonic(), int(line.rsplit(" ", 1)[1].rstrip("%")), float(swap.rstrip("M"))]
    return _last_sample[1:]


# Host rules: under 40 percent free memory or over 8 GB swap, report a load stop so round2 pauses
# through its own resumable path (its error text still says load); each stop is logged in host-stops.jsonl.
_getloadavg = os.getloadavg
def _guarded_loadavg():
    load = _getloadavg()
    free, swap_mb = _sample()
    if free < 40 or swap_mb > 8192:
        with (HERE / "runs/host-stops.jsonl").open("a") as log:
            log.write(json.dumps({"utc": time.time(), "free_percent": free, "swap_mb": swap_mb}) + "\n")
        return (99., *load[1:])
    return load
round2.os.getloadavg = _guarded_loadavg
_config_args = round2.config_args
round2.config_args = lambda: [*_config_args(), "-c", 'service_tier="standard"']

# round2 creates an editor workspace before its budget check; a stop in between leaves an empty
# folder that the resumed call then cannot create. Remove only a folder that never got a reply.
_proposer_call = round2.CodexProposer.call
def _proposer_call_clean(self, *args, **kwargs):
    stale = self.directory / f"editor-call-{len(self.budget.calls):02d}"
    if stale.is_dir() and not (stale / "reply.txt").exists():
        round2.shutil.rmtree(stale)
    return _proposer_call(self, *args, **kwargs)
round2.CodexProposer.call = _proposer_call_clean


def harness_diff(base, final):
    read = lambda d: {str(p.relative_to(d)): p.read_bytes() for p in d.rglob("*") if p.is_file() and "__pycache__" not in p.parts}
    a, b = read(base), read(final)
    return {"added": sorted(b.keys() - a.keys()), "removed": sorted(a.keys() - b.keys()),
            "modified": sorted(k for k in a.keys() & b.keys() if a[k] != b[k])}


def phases(pack):
    root = HERE / "runs/round2" / pack
    calls = json.loads((root / "B/model-calls/calls.json").read_text())
    done = lambda test: any(test(c["label"]) and c["status"] == "complete" for c in calls)
    reached = {"baseline": done(lambda l: l.endswith("/base1")), "propose": done(lambda l: l.endswith(".a1") and not l.startswith("agg1")),
               "probe": done(lambda l: l.endswith(("/inv1", "/dom1"))), "compose": done(lambda l: l == "agg1.a1")}
    candidates = list((root / "B/attempt-00/recursion").glob(f"*/{pack}/harnesses"))
    if len(candidates) != 1:
        raise RuntimeError("expected exactly one materialized pilot harness tree")
    harnesses = candidates[0]
    final = harnesses / "agg1"
    # Compare with harness2's own materialized base: it adds empty surface notes (README.md) to every tree.
    diff = harness_diff(harnesses / "base", final) if final.is_dir() else None
    out = {"phases": reached, "composed_harness_found": final.is_dir(), "harness_diff": diff,
           "compose_changed_harness": bool(diff and any(diff.values())) if diff else None}
    round2.write_json(root / "B/phases.json", out)
    return out


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--pack", choices=sorted(packs.PACKS), required=True)
    parser.add_argument("--resume", action="store_true", help="resume a load stop with only the unspent cap")
    parser.add_argument("--phases", action="store_true", help="only recompute side B phases and the no-op check")
    args = parser.parse_args()
    if not args.phases:
        # Same serialization as round2.py's own entry point: k=1, one model call at a time.
        round2.schedule.recursion.PHASE_WIDTH = 1
        round2.schedule.ROLLOUT_PARALLEL = 1
        original = round2.schedule.recursion.run_parallel
        round2.schedule.recursion.run_parallel = lambda *v, **k: original(*v, **k, propose_workers=1, rollout_workers=1)
        round2.run_pack(args.pack, resume=args.resume)
    print(json.dumps(phases(args.pack)))
