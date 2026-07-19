#!/usr/bin/env python3
import argparse
import csv
import re
import subprocess
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXP = ROOT / "experiment"
GRAPH = EXP / "catalogs/dblp/color/dblp.graph"
PATTERNS = EXP / "patterns/gcare/dblp"
TRUTH = EXP / "result/truecard/dblp/true_card.csv"
SUMMARY = EXP / "catalogs/dblp/color/dblp_mix_6_50000.obj"
OUT = EXP / "result/baselines/color/estimate/dblp.csv"
BUILD_LOG = EXP / "result/baselines/color/build-logs/dblp.log"
JULIA_PROJECT = EXP / "baseline/color"
BUILD_JL = EXP / "baseline/color/scripts/build.jl"
EST_JL = EXP / "baseline/color/scripts/estimate.jl"
EST_BATCH_JL = EXP / "baseline/color/scripts/estimate_batch.jl"
RUN_TMP = EXP / "run_tmp/color_dblp_queries.txt"
NUM_RE = re.compile(r"([0-9.eE+-]+)\s*,\s*([0-9.eE+-]+)")


def julia_bin():
    for p in ("julia", str(Path.home() / ".local/bin/julia")):
        try:
            subprocess.run([p, "--version"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            return p
        except Exception:
            pass
    raise FileNotFoundError("julia not found")


def build(args, jb):
    SUMMARY.parent.mkdir(parents=True, exist_ok=True)
    BUILD_LOG.parent.mkdir(parents=True, exist_ok=True)
    if SUMMARY.exists() and not args.force_build:
        return
    with BUILD_LOG.open("w", encoding="utf-8") as log:
        proc = subprocess.run([jb, f"--project={JULIA_PROJECT}", str(BUILD_JL), "-d", str(GRAPH), "-o", str(SUMMARY)], stdout=log, stderr=subprocess.STDOUT, text=True)
    if proc.returncode != 0:
        raise RuntimeError(f"Color build failed; see {BUILD_LOG}")


def main():
    ap = argparse.ArgumentParser(description="Run Color on corrected DBLP GCare patterns.")
    ap.add_argument("--force-build", action="store_true")
    ap.add_argument("--limit", type=int)
    ap.add_argument("--legacy-per-query", action="store_true", help="Use the old one-Julia-process-per-query path.")
    args = ap.parse_args()
    jb = julia_bin()
    build(args, jb)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    if TRUTH.exists():
        with TRUTH.open(newline="", encoding="utf-8") as tf:
            names = [row["query"] for row in csv.DictReader(tf) if row.get("status") == "ok" and row.get("query")]
        patterns = [PATTERNS / f"{name}.graph" for name in names]
    else:
        patterns = sorted(PATTERNS.glob("*.graph"))
    if args.limit is not None:
        patterns = patterns[: args.limit]
    if not args.legacy_per_query:
        missing = [str(p) for p in patterns if not p.exists()]
        if missing:
            raise FileNotFoundError(f"missing Color patterns: {missing[:3]}")
        RUN_TMP.parent.mkdir(parents=True, exist_ok=True)
        RUN_TMP.write_text("\n".join(str(p) for p in patterns) + "\n", encoding="utf-8")
        proc = subprocess.run([jb, f"--project={JULIA_PROJECT}", str(EST_BATCH_JL), "-f", str(RUN_TMP), "-s", str(SUMMARY), "-o", str(OUT)], text=True)
        if proc.returncode != 0:
            raise RuntimeError(f"Color batch estimate failed with exit code {proc.returncode}")
        print(f"Saved Color DBLP estimates: {OUT}")
        return
    with OUT.open("w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(["query", "estimate", "latency_s", "status", "error"])
        for p in patterns:
            start = time.perf_counter()
            proc = subprocess.run([jb, f"--project={JULIA_PROJECT}", str(EST_JL), "-q", str(p), "-s", str(SUMMARY)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            elapsed = time.perf_counter() - start
            m = NUM_RE.search(proc.stdout.strip().splitlines()[-1] if proc.stdout.strip() else "")
            if proc.returncode == 0 and m:
                w.writerow([p.stem, m.group(1), m.group(2), "ok", ""])
            else:
                err = (proc.stderr or proc.stdout).replace("\n", " ").replace(",", " ")[:500]
                w.writerow([p.stem, "", f"{elapsed:.6f}", "failed", err])
    print(f"Saved Color DBLP estimates: {OUT}")


if __name__ == "__main__":
    main()
