#!/usr/bin/env python3
"""Run the opt-in release benchmark serially; output timings, CPU and peak RSS."""
import argparse
import json
import re
import resource
import subprocess
import sys

parser = argparse.ArgumentParser()
parser.add_argument('binary')
parser.add_argument('--runs', type=int, default=3)
args = parser.parse_args()
rows = []
for _ in range(args.runs):
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    result = subprocess.run([args.binary, 'representative_tui_latency', '--ignored', '--nocapture'], capture_output=True, text=True, check=True)
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    row = {name: float(value) for name, value in re.findall(r'LATENCY (\w+) ([\d.]+)', result.stdout)}
    row['cpu_seconds_including_fixture'] = (after.ru_utime + after.ru_stime) - (before.ru_utime + before.ru_stime)
    # getrusage is a high-water mark over children; use the report's maximum.
    row['peak_rss_bytes'] = after.ru_maxrss * (1 if sys.platform == 'darwin' else 1024)
    rows.append(row)
print(json.dumps(rows, indent=2))
