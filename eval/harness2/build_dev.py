"""Build a development suite with the frozen cross-team generator, but new project names and seed.

The frozen cross-team suite is held out and never read here. This reuses the generator's
templates unchanged (`build_data.build_cross_team`) so the task families match, while every
project name, identifier and query differs from the held-out suite.
Usage: python3 eval/harness2/build_dev.py /path/to/benchmarks/packaged/data
"""
import inspect
import json
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
OUT = HERE / "data"
HELD_OUT_NAMES = '["Aster", "Astera", "Atlas", "Atla", "Helios", "Helio", "Borealis", "Boreal"]'
DEV_NAMES = '["Cygnus", "Cygna", "Draco", "Drac", "Orion", "Orio", "Lyra", "Lyrae"]'
DEV_SEED = 20261023


def build(packaged_data):
    sys.path.insert(0, str(packaged_data))
    import build_data
    source = inspect.getsource(build_data.build_cross_team)
    assert source.count(HELD_OUT_NAMES) == 1, "generator templates changed; refusing to derive a dev suite"
    captured = {}
    namespace = {**vars(build_data), "SEED": DEV_SEED,
                 "write_suite": lambda name, parents, queries, details: captured.update(parents=parents, queries=queries)}
    exec(source.replace(HELD_OUT_NAMES, DEV_NAMES), namespace)
    namespace["build_cross_team"]()
    held_out = json.loads((Path(packaged_data) / "cross-team/parents.json").read_text())
    assert not {p["id"] for p in held_out} & {p["id"] for p in captured["parents"]}
    assert not {p["project"] for p in held_out} & {p["project"] for p in captured["parents"]}
    for p in captured["parents"]:
        p["source"] = "deterministic synthetic cross-team fixture, dev split"
    OUT.mkdir(exist_ok=True)
    for name in ("parents", "queries"):
        (OUT / f"{name}.json").write_text(json.dumps(captured[name], indent=1, ensure_ascii=False) + "\n")
    print(len(captured["parents"]), "documents,", len(captured["queries"]), "queries")


if __name__ == "__main__":
    build(Path(sys.argv[1]))
