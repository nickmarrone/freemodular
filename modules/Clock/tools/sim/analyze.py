#!/usr/bin/env python3
"""Summarize a simulator portd.log: per-channel rising edges, intervals, pulse widths.

usage: analyze.py portd.log [channel]
"""
import sys

log = [tuple(map(int, line.split())) for line in open(sys.argv[1])]
channels = [int(sys.argv[2])] if len(sys.argv) > 2 else range(8)
CYCLES_PER_S = 16e6
for ch in channels:
    prev, rise, rises, highs = 0, 0, [], []
    for cycle, value in log:
        now, was = value >> ch & 1, prev >> ch & 1
        if now and not was:
            rises.append(cycle)
            rise = cycle
        if was and not now:
            highs.append(cycle - rise)
        prev = value
    intervals = [round((b - a) / CYCLES_PER_S, 4) for a, b in zip(rises, rises[1:])]
    print(f"ch{ch}: {len(rises)} rises at (s) {[round(r / CYCLES_PER_S, 3) for r in rises[:16]]}")
    print(f"     intervals (s) {intervals[:16]}")
    if highs:
        print(f"     high time (ms) min {min(highs) / 16000:.2f} max {max(highs) / 16000:.2f}")
