import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location("charts", Path(__file__).with_name("benchmark_charts.py"))
charts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(charts)


class Charts(unittest.TestCase):
    def test_partial_is_missing_and_completed_zero_is_preserved(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "memory").mkdir()
            def put(name, questions, expected, value):
                (root / "memory" / name).write_text(json.dumps(dict(system="hyperag", profile="e5+rerank", questions=questions, expectedQuestions=expected, accuracy=value)))
            put("dmr.hyperag.e5.meta.json", 1, 2, 1)
            put("lme-oracle.hyperag.e5.meta.json", 2, 2, 0)
            keys, values = charts.generate(root, root / "figures")
            key = ("hyperag", "e5+rerank")
            self.assertIsNone(values[("dmr", key)])
            self.assertEqual(values[("lme-oracle", key)], 0)
            for name, *_ in charts.CHARTS:
                svg = (root / "figures" / f"{name}.svg").read_text()
                ET.fromstring(svg)
                self.assertIn("N/D", svg)
                self.assertIn("prefers-color-scheme:dark", svg)
                self.assertNotIn('<rect width="880"', svg)
            self.assertIn("0.0%", (root / "figures/lme-oracle.svg").read_text())
            self.assertIn(("langmem", "default"), keys)

    def test_nonfinite_and_partial_beir_scores_do_not_become_bars(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "beir-scores.json").write_text(json.dumps([
                dict(system="engram", profile="default", dataset="scifact", queries=1, test_queries=2, **{"ndcg@10": .9}),
                dict(system="letta", profile="default", dataset="scifact", queries=2, test_queries=2, **{"ndcg@10": float("nan")})]))
            _, values = charts.load_results(root)
            self.assertTrue(all(value is None for value in values.values()))

    def test_second_window_default_and_new_beir_charts(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "memory").mkdir()
            for bench, metric in [("locomo", {"locomo": {"f1_cat1_4": .4913}}), ("dmr", {"accuracy": .822})]:
                (root / "memory" / f"{bench}.hyperag.default.meta.json").write_text(json.dumps(
                    dict(system="hyperag", profile=charts.DEFAULT[1], questions=2, expectedQuestions=2, **metric)))
            (root / "beir-scores.json").write_text(json.dumps([
                dict(system="hyperag", profile="nemotron", dataset="nfcorpus", queries=2, test_queries=2, **{"ndcg@10": .3724}),
                dict(system="hyperag", profile="nemotron", dataset="arguana", queries=2, test_queries=2, **{"ndcg@10": .5004})]))
            keys, values = charts.generate(root, root / "figures")
            self.assertEqual(keys[0], charts.DEFAULT)
            self.assertEqual(values[("locomo", charts.DEFAULT)], .4913)
            self.assertEqual(values[("dmr", charts.DEFAULT)], .822)
            for dataset, value in [("nfcorpus", "0.372"), ("arguana", "0.500")]:
                svg = (root / "figures" / f"{dataset}.svg").read_text()
                self.assertIn(value, svg)
                self.assertIn("N/D", svg)
                self.assertIn("two windows", svg)
            self.assertEqual(len(list((root / "figures").glob("*.svg"))), 7)

    def test_palette_is_stable_and_hyperag_is_blue(self):
        self.assertEqual(charts.color("hyperag"), "#5283a3")
        self.assertEqual(charts.color("engram"), "#8b915c")
        self.assertEqual(charts.color("letta"), "#8b915c")
        self.assertIn("graph off", charts.label(("hyperag", "e5+rerank")))
        self.assertNotIn("graph off", charts.label(charts.DEFAULT))


if __name__ == "__main__":
    unittest.main()
