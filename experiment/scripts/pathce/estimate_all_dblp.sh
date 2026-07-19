#!/bin/bash
set -eu
set -o pipefail

k=${1:-2}
d=${2:-5}
m=${3:-200}
timeout_s=${4:-10}

workspace=$(realpath $(dirname $0)/../../)
python3 - "$workspace" "$k" "$d" "$m" "$timeout_s" <<'PY_EST'
import csv
import json
import re
import subprocess
import sys
import time
from collections import defaultdict, deque
from pathlib import Path

workspace = Path(sys.argv[1])
k, d, m = sys.argv[2], sys.argv[3], sys.argv[4]
timeout_s = float(sys.argv[5])
catalog = workspace / 'catalogs/dblp/pathce' / f'dblp_{k}_{d}_{m}'
patterns = workspace / 'patterns/pathce/dblp'
truth_csv = workspace / 'result/truecard/dblp/true_card.csv'
pathce = workspace / 'baseline/pathce/target/release/pathce'
out_dir = workspace / 'result/baselines/pathce/estimate'
out_dir.mkdir(parents=True, exist_ok=True)
out = out_dir / f'dblp_k{k}_d{d}.csv'

num_re = re.compile(r'^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$')

def connected(pattern):
    vids = {int(v['tag_id']) for v in pattern.get('vertices', [])}
    if not vids:
        return False
    adj = defaultdict(set)
    for e in pattern.get('edges', []):
        s, t = int(e['src']), int(e['dst'])
        adj[s].add(t)
        adj[t].add(s)
    start = next(iter(vids))
    seen = {start}
    q = deque([start])
    while q:
        x = q.popleft()
        for y in adj[x]:
            if y not in seen:
                seen.add(y)
                q.append(y)
    return seen == vids

def parse_estimate(stdout):
    val = ''
    for line in stdout.splitlines():
        stripped = line.strip()
        if num_re.match(stripped):
            val = stripped
    if val:
        return val
    nums = re.findall(r'[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?', stdout)
    return nums[-1] if nums else ''

with out.open('w', newline='', encoding='utf-8') as f:
    writer = csv.writer(f)
    writer.writerow(['query', 'estimate', 'latency_s', 'status', 'error'])
    query_names = []
    if truth_csv.exists():
        with truth_csv.open(newline='', encoding='utf-8') as tf:
            for row in csv.DictReader(tf):
                if row.get('status') == 'ok' and row.get('query'):
                    query_names.append(row['query'])
    pattern_paths = [patterns / f'{q}.json' for q in query_names] if query_names else sorted(patterns.glob('*.json'))
    for pattern_path in pattern_paths:
        query = pattern_path.stem
        if not pattern_path.exists():
            writer.writerow([query, '', '0.000000', 'missing_pattern', str(pattern_path)])
            continue
        pattern = json.loads(pattern_path.read_text(encoding='utf-8'))
        edge_count = len(pattern.get('edges', []))
        if edge_count > 64:
            writer.writerow([query, '', '0.000000', 'skipped_too_many_edges', f'edges={edge_count}'])
            continue
        if not connected(pattern):
            writer.writerow([query, '', '0.000000', 'skipped_disconnected', 'pattern not connected'])
            continue
        start = time.perf_counter()
        try:
            proc = subprocess.run([
                str(pathce), 'estimate', '-c', str(catalog), '-p', str(pattern_path),
                '--max-path-length', str(k), '--max-star-degree', str(d),
            ], text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout_s)
        except subprocess.TimeoutExpired:
            elapsed = time.perf_counter() - start
            writer.writerow([query, '', f'{elapsed:.6f}', 'timeout', f'timeout_s={timeout_s:g}'])
            continue
        elapsed = time.perf_counter() - start
        if proc.returncode == 0:
            writer.writerow([query, parse_estimate(proc.stdout), f'{elapsed:.6f}', 'ok', ''])
        else:
            err = (proc.stderr or proc.stdout).replace('\n', ' ').replace(',', ' ')
            writer.writerow([query, '', f'{elapsed:.6f}', 'failed', err])
print(f'Saved PathCE DBLP estimates: {out}')
PY_EST
