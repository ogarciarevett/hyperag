import json, os, sys, tempfile
import numpy as np
os.environ["MEMVID_TELEMETRY"] = "0"
import memvid_sdk
docs = [json.loads(l) for l, _ in zip(open(sys.argv[1]), range(30))]
rng = np.random.default_rng(0)
V = rng.normal(size=(len(docs), 8)); V /= np.linalg.norm(V, axis=1, keepdims=True)
path = os.path.join(tempfile.mkdtemp(), "a.mv2")
mv = memvid_sdk.create(path, enable_vec=True)
mv.put_many([{"title": d["title"], "label": "doc", "text": d["text"], "uri": f"mv2://x/{i}"} for i, d in enumerate(docs)], embeddings=V.tolist())
mv.commit()
hit = lambda m, i: m.find("q", k=1, mode="sem", query_embedding=V[i].tolist())["hits"][0]["uri"].split("/")[-1].split("#")[0] == str(i)
print("after commit:     ", "".join("1" if hit(mv, i) else "0" for i in range(len(docs))), mv.stats()["frame_count"], "frames")
mv.seal()
mv2 = memvid_sdk.use("basic", path, mode="open")
print("after seal + open:", "".join("1" if hit(mv2, i) else "0" for i in range(len(docs))))
