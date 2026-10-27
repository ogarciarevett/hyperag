"""Offline statistical checks for the native guard's fixed latency margin."""
import math


def percentile(values, fraction):
    if not values:
        raise ValueError('calibration needs samples')
    return sorted(values)[math.ceil(fraction * len(values)) - 1]


def check(tails, full_wall, adjusted, margin):
    if len(tails) != len(full_wall) or len(tails) != len(adjusted):
        raise ValueError('paired calibration samples required')
    tail_p99 = percentile(tails, .99)
    wall_p95, adjusted_p95 = percentile(full_wall, .95), percentile(adjusted, .95)
    if tail_p99 > margin:
        raise AssertionError(f'p99 tail {tail_p99:.3f} ms exceeds margin {margin} ms')
    if adjusted_p95 < wall_p95:
        raise AssertionError('adjusted p95 under-counts full-wall p95')
    return {'calls': len(tails), 'tail_p99_ms': round(tail_p99, 3),
            'tail_max_ms': round(max(tails), 3), 'full_wall_p95_ms': round(wall_p95, 3),
            'adjusted_p95_ms': round(adjusted_p95, 3), 'margin_ms': margin}
