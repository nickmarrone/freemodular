#!/usr/bin/env python3
"""Summarize a Drift simulator run.

usage: analyze.py <out dir> [window_start_s window_end_s label ...]

Per window: compute time per sample, output range, largest sample-to-sample jump,
and how often the output rises through its midpoint (the frequency, for LFO mode).
"""
import os
import sys

out = sys.argv[1]
dac = [tuple(map(int, line.split())) for line in open(os.path.join(out, "dac.log"))]
compute = [tuple(map(int, line.split())) for line in open(os.path.join(out, "compute.log"))]
CYCLES_PER_S = 16e6

intervals = [b[0] - a[0] for a, b in zip(dac, dac[1:])]
period = sorted(intervals)[len(intervals) // 2]
print(f"{len(dac)} samples, period {period} cycles = {period / 16:.1f} us")

args = sys.argv[2:]
windows = [(float(args[i]), float(args[i + 1]), args[i + 2]) for i in range(0, len(args), 3)]
windows = windows or [(0.05, dac[-1][0] / CYCLES_PER_S, "all")]
for lo, hi, label in windows:
    seg = [d for t, d in compute if lo * CYCLES_PER_S <= t < hi * CYCLES_PER_S]
    vals = [(t, v) for t, v in dac if lo * CYCLES_PER_S <= t < hi * CYCLES_PER_S]
    if not seg or len(vals) < 2:
        continue
    print(f"{label:12s} compute per sample: avg {sum(seg) / len(seg) / 16:4.0f} us, "
          f"max {max(seg) / 16:4.0f} us (budget {period / 16:.0f} us)")
    lo_v, hi_v = min(v for _, v in vals), max(v for _, v in vals)
    jump = max(abs(b[1] - a[1]) for a, b in zip(vals, vals[1:]))
    mid = (lo_v + hi_v) / 2
    rises = [b[0] for a, b in zip(vals, vals[1:]) if a[1] < mid <= b[1]]
    freq = ""
    if len(rises) >= 3:
        freq = f", {(len(rises) - 1) * CYCLES_PER_S / (rises[-1] - rises[0]):.3f} Hz"
    print(f"{label:12s} output {lo_v}..{hi_v}, max jump {jump}, {len(rises)} mid crossings{freq}")

step, t, j, sketch = int(0.25 * CYCLES_PER_S), 0, 0, []
while t < dac[-1][0]:
    while j < len(dac) - 1 and dac[j + 1][0] <= t:
        j += 1
    sketch.append(dac[j][1])
    t += step
print("output every 250 ms:", sketch)
