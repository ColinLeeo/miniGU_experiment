#!/usr/bin/env python3
import argparse
import csv
import re
import subprocess
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXP = ROOT / "experiment"
GCARE = EXP / "baseline/gcare/build/gcare_graph"
GRAPH_TEXT = EXP / "catalogs/dblp/color/dblp.graph"
GRAPH_DIR = EXP / "catalogs/dblp/gcare/dblp"
PATTERNS = EXP / "patterns/gcare/dblp"
TRUTH = EXP / "result/truecard/dblp/true_card.csv"
OUT = EXP / "result/baselines/gcare/estimate/dblp_wj_n1.csv"
BUILD_LOG = EXP / "result/baselines/gcare/build-logs/dblp_wj.log"
NUM_RE = re.compile(r"([0-9.eE+-]+)")
EST_TIME_RE = re.compile(r"^\s*([0-9.eE+-]+)\s*,\s*([0-9.eE+-]+)\s*$")


def build(args):
    GRAPH_DIR.parent.mkdir(parents=True, exist_ok=True)
    BUILD_LOG.parent.mkdir(parents=True, exist_ok=True)
    if Path(str(GRAPH_DIR) + ".graph.meta").exists() and not args.force_build:
        return
    with BUILD_LOG.open("w", encoding="utf-8") as log:
        proc = subprocess.run([str(GCARE), "-b", "-m", "wj", "-i", str(GRAPH_TEXT), "-d", str(GRAPH_DIR)], stdout=log, stderr=subprocess.STDOUT, text=True)
    if proc.returncode != 0:
        raise RuntimeError(f"GCare build failed; see {BUILD_LOG}")


def parse(stdout):
    lines = [line.strip() for line in stdout.splitlines() if line.strip()]
    if lines:
        match = EST_TIME_RE.match(lines[-1])
        if match:
            return match.group(1), match.group(2)
    nums = NUM_RE.findall(stdout)
    if len(nums) >= 2:
        return nums[-2], nums[-1]
    return "", ""


def main():
    ap = argparse.ArgumentParser(description="Run GCare WanderJoin on corrected DBLP patterns.")
    ap.add_argument("--ratio", default="0.03")
    ap.add_argument("--repeat", default="1")
    ap.add_argument("--seed", default="0")
    ap.add_argument("--timeout", type=float, default=300.0)
    ap.add_argument("--force-build", action="store_true")
    ap.add_argument("--limit", type=int)
    args = ap.parse_args()
    build(args)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    if TRUTH.exists():
        with TRUTH.open(newline="", encoding="utf-8") as tf:
            names = [row["query"] for row in csv.DictReader(tf) if row.get("status") == "ok" and row.get("query")]
        patterns = [PATTERNS / f"{name}.graph" for name in names]
    else:
        patterns = sorted(PATTERNS.glob("*.graph"))
    if args.limit is not None:
        patterns = patterns[: args.limit]
    with OUT.open("w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(["query", "estimate", "latency_s", "status", "error"])
        for p in patterns:
            start = time.perf_counter()
            try:
                proc = subprocess.run([str(GCARE), "-q", "-m", "wj", "-i", str(p), "-d", str(GRAPH_DIR), "-p", str(args.ratio), "-n", str(args.repeat), "-s", str(args.seed)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=args.timeout)
            except subprocess.TimeoutExpired:
                w.writerow([p.stem, "", f"{time.perf_counter()-start:.6f}", "timeout", f"timeout_s={args.timeout:g}"])
                continue
            elapsed = time.perf_counter() - start
            est, query_time = parse(proc.stdout)
            if proc.returncode == 0 and est:
                w.writerow([p.stem, est, query_time or f"{elapsed:.6f}", "ok", ""])
            else:
                err = (proc.stderr or proc.stdout).replace("\n", " ").replace(",", " ")[:500]
                w.writerow([p.stem, "", f"{elapsed:.6f}", "failed", err])
    print(f"Saved GCare DBLP estimates: {OUT}")


if __name__ == "__main__":
    main()
