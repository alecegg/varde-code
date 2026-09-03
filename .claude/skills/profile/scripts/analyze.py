#!/usr/bin/env python3
# Analysis half of rank.sh, split into its own file because the sandbox
# blocks bash heredocs (they materialize as temp files under /tmp).
# Usage: analyze.py <data-file>  (tab-separated: mode repo files cold warm)
import sys
from collections import defaultdict

rows = []
with open(sys.argv[1]) as f:
    for line in f:
        mode, repo, files, cold, warm = line.rstrip("\n").split("\t")
        rows.append((mode, repo, int(files), int(cold), int(warm)))

if not rows:
    print("No data — no reference repos found. See SKIP warnings above.")
    sys.exit(0)

print("\n## Ranked by warm_ms (slowest first)\n")
print(f"{'mode':<22}{'repo':<16}{'files':>8}{'cold_ms':>10}{'warm_ms':>10}")
for mode, repo, files, cold, warm in sorted(rows, key=lambda r: -r[4]):
    print(f"{mode:<22}{repo:<16}{files:>8}{cold:>10}{warm:>10}")

by_mode = defaultdict(dict)
for mode, repo, files, cold, warm in rows:
    by_mode[mode][repo] = (files, warm)

repos_seen = sorted({repo for _, repo, *_ in rows}, key=lambda r: by_mode[rows[0][0]].get(r, (0, 0))[0])
if len(repos_seen) < 2:
    print("\nOnly one repo profiled — skipping scaling-flag table (needs >=2 to compare).")
    sys.exit(0)

biggest = max(repos_seen, key=lambda r: max(by_mode[m].get(r, (0, 0))[0] for m in by_mode))
smallest = min(repos_seen, key=lambda r: min(by_mode[m].get(r, (float('inf'), 0))[0] for m in by_mode))

print(f"\n## Scaling check: {biggest} (bigger) vs {smallest} (smaller)\n")
print(f"{'mode':<22}{'file_ratio':>12}{'time_ratio':>12}  flag")
for mode in by_mode:
    if biggest not in by_mode[mode] or smallest not in by_mode[mode]:
        continue
    bf, bw = by_mode[mode][biggest]
    sf, sw = by_mode[mode][smallest]
    if sf == 0 or sw == 0:
        continue
    file_ratio = bf / sf
    time_ratio = bw / sw
    flag = "SUPERLINEAR" if time_ratio > 2 * file_ratio and bw > 200 else ""
    print(f"{mode:<22}{file_ratio:>12.2f}{time_ratio:>12.2f}  {flag}")
