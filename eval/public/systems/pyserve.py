"""Serves one Python adapter, systems/<name>.py, to the TypeScript runners (py.ts) as JSON lines.

A Python adapter has VERSION (package -> version), CONFIG (dict), optional CONTAINERS (docker
names counted in RAM) and open(workdir, profile) returning an object with ingest(ns, docs),
search(ns, query, k) -> [{"id", "score", "text"}] and close(). Docs are the runner's Doc objects.
"""
import importlib
import inspect
import json
import os
import subprocess
import sys
import tempfile
import traceback

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
name, profile = sys.argv[1], sys.argv[2]
# the protocol gets a private copy of fd 1; fd 1 itself (Python, native code, children) goes to stderr
reply = os.fdopen(os.dup(1), "w")
os.dup2(2, 1)
sys.stdout = sys.stderr
DOCKER = os.environ.get("DOCKER", "docker")


def tree_rss_bytes() -> int:
    rows = [r.split() for r in subprocess.run(["ps", "-A", "-o", "pid=,ppid=,rss="], capture_output=True, text=True).stdout.splitlines()]
    kids: dict[int, list[tuple[int, int]]] = {}
    for pid, ppid, rss in rows:
        kids.setdefault(int(ppid), []).append((int(pid), int(rss)))
    total = next(int(r[2]) for r in rows if int(r[0]) == os.getpid())
    todo = [os.getpid()]
    while todo:
        for pid, rss in kids.get(todo.pop(), []):
            total, _ = total + rss, todo.append(pid)
    return total * 1024


def container_bytes(names: list[str]) -> int:
    if not names:
        return 0
    out = subprocess.run([DOCKER, "stats", "--no-stream", "--format", "{{.MemUsage}}", *names], capture_output=True, text=True).stdout
    units = {"KiB": 2**10, "MiB": 2**20, "GiB": 2**30, "B": 1}
    total = 0
    for line in out.splitlines():
        used = line.split("/")[0].strip()
        unit = next(u for u in units if used.endswith(u))
        total += int(float(used[: -len(unit)]) * units[unit])
    return total


mod = importlib.import_module(f"systems.{name}")
system = mod.open(tempfile.mkdtemp(prefix=f"{name}-", dir=os.environ.get("TMPDIR")), profile)
methods = {
    "describe": lambda: {"system": name, "version": ", ".join(f"{k} {v}" for k, v in mod.VERSION.items()), "profile": profile, "config": mod.CONFIG},
    "open": lambda ns: None,
    "ingest": lambda ns, docs: system.ingest(ns, docs),
    # the question's date goes only to an adapter whose search takes as_of (a native date anchor)
    "search": lambda ns, query, k, opts=None: system.search(ns, query, k, **({"as_of": opts["asOf"]} if opts and opts.get("asOf") and "as_of" in inspect.signature(system.search).parameters else {})),
    "rss": lambda: tree_rss_bytes() + container_bytes(getattr(mod, "CONTAINERS", [])),
    "close": lambda: system.close(),
}
for line in sys.stdin:
    req = json.loads(line)
    try:
        msg = {"id": req["id"], "result": methods[req["method"]](*req.get("args", []))}
    except Exception as e:
        traceback.print_exc()
        msg = {"id": req["id"], "error": f"{type(e).__name__}: {e}"[:500]}
    reply.write(json.dumps(msg) + "\n")
    reply.flush()
    if req["method"] == "close":
        break
