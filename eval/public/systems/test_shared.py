"""python3 -I eval/public/systems/test_shared.py"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from systems.shared import rank  # noqa: E402

session = ["D1:1", "D1:2", "D1:3"]
m = [(session, 0.9, "Caroline adopted a dog"), (session, 0.8, "Melanie paints"), ([], 0.7, "no source"), (["x"], 0.6, "z")]
assert [h["text"] for h in rank(m, 10, True)] == ["Caroline adopted a dog", "Melanie paints", "no source", "z"]
assert len(rank(m, 2, True)) == 2
assert [h["id"] for h in rank(m, 10, False)] == ["D1:1", "D1:2", "D1:3", "x"]
assert [h["id"] for h in rank(m, 2, False)] == ["D1:1", "D1:2"]
print("ok")

from systems.shared import iso_date  # noqa: E402

assert iso_date("1:56 pm on 8 May, 2023") == "2023-05-08T13:56:00Z"
assert iso_date("12:09 am on 13 September, 2023") == "2023-09-13T00:09:00Z"
assert iso_date("2023/05/30 (Tue) 23:40") == "2023-05-30T23:40:00Z"
assert iso_date("7 days 8 hours ago") is None and iso_date(None) is None
print("iso_date ok")
