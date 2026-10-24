"""Cite the first `cite` retrieved memories; abstain when Hyperag flags insufficient evidence."""


def run(task):
    p = task["params"]
    out = {}
    for item in task["items"]:
        x = item["input"]
        out[item["id"]] = [] if p["abstain_on_flag"] and x["insufficient_evidence"] else [h["id"] for h in x["hits"][:p["cite"]]]
    return out
