#!/usr/bin/env python3
import argparse
import csv
import math
import re
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXP = ROOT / "experiment"
MINIGU = ROOT / "target/release/minigu"
DATASET = EXP / "run_tmp/dblp_gcard_partitioned_dataset"
PATTERNS = EXP / "run_tmp/dblp_gcard_partitioned_patterns"
TRUTH = EXP / "result/truecard/dblp/true_card.csv"
RUN_ROOT = EXP / "result/gcard_query/dblp_k2_d5"
TMP_ROOT = EXP / "run_tmp/dblp_gcard_partitioned_run"
DB = EXP / "run_tmp/dblp_gcard_partitioned_db"
GRAPH = "dblp_partitioned"
CARD_RE = re.compile(r",\s*([^,\s]+),\s*cardinality:\s*([0-9.eE+-]+)")
TIME_RE = re.compile(r"Time:\s*([0-9.]+)s")
PROFILE_RE = re.compile(r"total: (?P<total>\d+).*sample_time: (?P<sample>[0-9.]+), estimate_time: (?P<estimate>[0-9.]+), build_time: (?P<build>[0-9.]+).*cardinality: (?P<cardinality>[0-9.eE+-]+)")


def qerror(est, truth):
    est = max(float(est), 1.0)
    truth = max(float(truth), 1.0)
    return max(est / truth, truth / est)


def read_truth():
    rows = []
    with TRUTH.open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            if row.get("status") == "ok" and row.get("true_cardinality"):
                rows.append(row)
    return rows


def run_minigu(gql, log):
    with log.open("w", encoding="utf-8") as out:
        proc = subprocess.run([str(MINIGU), "execute", str(gql), "--path", str(DB)], cwd=str(ROOT), stdout=out, stderr=subprocess.STDOUT, text=True)
    if proc.returncode != 0:
        raise RuntimeError(f"minigu exited {proc.returncode}; see {log}")


def ensure_graph(args):
    if args.force_db and DB.exists():
        shutil.rmtree(DB)
    DB.mkdir(parents=True, exist_ok=True)
    RUN_ROOT.mkdir(parents=True, exist_ok=True)
    TMP_ROOT.mkdir(parents=True, exist_ok=True)
    stat = DB / f"{GRAPH}.statistic.bin"
    catalog = DB / "catalog.json"
    if stat.exists() and catalog.exists() and not args.force_db:
        return
    gql = TMP_ROOT / "setup.gql"
    log = RUN_ROOT / "setup_partitioned.log"
    gql.write_text("\n".join([
        f'call load_ldbc("{DATASET}", "{GRAPH}", {args.k})',
        ":quit",
    ]) + "\n", encoding="utf-8")
    run_minigu(gql, log)


def parse_profiles(text):
    profiles = []
    cli_times = []
    current = None
    for line in text.splitlines():
        if line.startswith("[build-prof]"):
            current = {k: float(v) for k, v in re.findall(r"([a-zA-Z_+]+): ([0-9.]+)s", line)}
        elif line.startswith("total:"):
            m = PROFILE_RE.search(line)
            if m:
                if current is None:
                    current = {}
                current.update({
                    "gcard_total_candidates": float(m.group("total")),
                    "sample_time_s": float(m.group("sample")),
                    "estimate_time_s": float(m.group("estimate")),
                    "build_time_s": float(m.group("build")),
                })
                profiles.append(current)
                current = None
        elif line.startswith("Time:"):
            m = TIME_RE.search(line)
            if m:
                cli_times.append(float(m.group(1)))
    return profiles, cli_times


def run_queries(args):
    rows = read_truth()
    if args.limit is not None:
        rows = rows[: args.limit]
    paths = [PATTERNS / f"{r['query']}.json" for r in rows]
    missing = [p for p in paths if not p.exists()]
    if missing:
        raise FileNotFoundError(f"missing patterns: {missing[:3]}")
    gql = TMP_ROOT / "query_partitioned.gql"
    log = RUN_ROOT / "dblp_partitioned.log"
    lines = [
        f"session set graph {GRAPH}",
        f'call load_catalog("{GRAPH}")',
        f"call set_gcard_star_config({args.star_len}, {args.star_degree})",
    ]
    for p in paths:
        lines.append(f':time call gcard_query("{p}", {args.k}, {args.sample_size}, 0, false, {args.tree_num})')
    lines.append(":quit")
    gql.write_text("\n".join(lines) + "\n", encoding="utf-8")
    run_minigu(gql, log)

    text = log.read_text(encoding="utf-8", errors="replace")
    estimates = [float(m.group(2)) for m in CARD_RE.finditer(text)]
    profiles, cli_times = parse_profiles(text)
    if len(estimates) != len(rows):
        raise RuntimeError(f"got {len(estimates)} estimates, expected {len(rows)}; see {log}")

    detail = RUN_ROOT / "detail_partitioned.csv"
    fields = ["dataset", "query", "pattern", "estimate", "true_cardinality", "qerror", "latency_s", "build_time_s", "estimate_time_s", "sample_time_s", "cli_time_s", "gcard_total_candidates", "status"]
    with detail.open("w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        for i, (row, est) in enumerate(zip(rows, estimates)):
            prof = profiles[i] if i < len(profiles) else {}
            latency = prof.get("build_time_s", 0.0) + prof.get("estimate_time_s", 0.0) + prof.get("sample_time_s", 0.0) if prof else (cli_times[i] if i < len(cli_times) else "")
            qe = qerror(est, row["true_cardinality"])
            w.writerow({
                "dataset": "dblp", "query": row["query"], "pattern": row["query"], "estimate": f"{est:.17g}",
                "true_cardinality": row["true_cardinality"], "qerror": f"{qe:.17g}",
                "latency_s": f"{latency:.9g}" if isinstance(latency, float) else latency,
                "build_time_s": prof.get("build_time_s", ""), "estimate_time_s": prof.get("estimate_time_s", ""), "sample_time_s": prof.get("sample_time_s", ""),
                "cli_time_s": f"{cli_times[i]:.9g}" if i < len(cli_times) else "", "gcard_total_candidates": prof.get("gcard_total_candidates", ""), "status": "ok",
            })
    print(f"detail={detail}")
    print(f"log={log}")


def main():
    ap = argparse.ArgumentParser(description="Run GCard on corrected partitioned DBLP patterns.")
    ap.add_argument("--k", type=int, default=2)
    ap.add_argument("--sample-size", type=int, default=500)
    ap.add_argument("--tree-num", type=int, default=10)
    ap.add_argument("--star-len", type=int, default=1)
    ap.add_argument("--star-degree", type=int, default=5)
    ap.add_argument("--force-db", action="store_true")
    ap.add_argument("--limit", type=int)
    args = ap.parse_args()
    ensure_graph(args)
    run_queries(args)


if __name__ == "__main__":
    main()
