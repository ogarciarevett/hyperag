#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('tail_calibration', Path(__file__).with_name('tail_calibration.py'))
calibration = importlib.util.module_from_spec(spec)
spec.loader.exec_module(calibration)


class TailCalibrationTest(unittest.TestCase):
    def test_isolated_runner_outlier_does_not_fail_p99(self):
        tails = [10.] * 199 + [116.359]
        result = calibration.check(tails, [40 + t for t in tails], [120.] * 200, 80)
        self.assertEqual(result['tail_p99_ms'], 10)
        self.assertEqual(result['tail_max_ms'], 116.359)

    def test_sustained_tail_over_margin_fails(self):
        tails = [10.] * 197 + [80.001] * 3
        with self.assertRaisesRegex(AssertionError, 'p99 tail'):
            calibration.check(tails, [40 + t for t in tails], [120.] * 200, 80)

    def test_equal_p99_boundary_passes(self):
        tails = [10.] * 197 + [80.] * 3
        self.assertEqual(calibration.check(tails, [40 + t for t in tails], [120.] * 200, 80)['tail_p99_ms'], 80)

    def test_underestimated_p95_fails_even_if_tail_p99_passes(self):
        raw = [0.] * 190 + [100.] * 10
        tails = [200.] + [10.] * 199
        with self.assertRaisesRegex(AssertionError, 'under-counts'):
            calibration.check(tails, [r+t for r,t in zip(raw,tails)], [r+80 for r in raw], 80)


if __name__ == '__main__':
    unittest.main()
