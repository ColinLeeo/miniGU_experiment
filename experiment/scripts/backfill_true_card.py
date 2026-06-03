#!/usr/bin/env python3
"""Backfill true_card and qerror columns into a finished insert_impact CSV.

For each row:
  - query JSON path: base → <PATTERN_NP_DIR>/<pattern>.json
                     else  → <PATTERN_P_DIR>/<query_variant>.json
  - update_pct == 0 → use <dataset_dir> directly (cached per pattern,variant)
  - update_pct >  0 → assemble a temp dir from <dataset_dir> CSVs (symlinks)
                      + the updated edge CSV at
                      <result_dir>/<pattern>/<position>/pct<label>/<edge_label>.csv
  - call true_cardinality.py via subprocess, capture cardinality.

Writes incrementally: after every row updates the output CSV, so the script
is safe to Ctrl+C and resume — rows that already have a numeric true_card
are skipped on the next run.

Usage:
  python3 backfill_true_card.py \\
      --csv  /path/to/insert_impact_results.csv \\
      --dataset /path/to/ldbc/sf1 \\
      --result-dir /path/to/experiment/result/insert_impact \\
      --pattern-np-dir experiment/patterns/gcard/lsqb \\
      --pattern-p-dir experiment/patterns/gcard/lsqb_glogs_with_pred_10/lsqb \\
      --true-card-script experiment/scripts/true_cardinality.py \\
      --out insert_impact_results_with_truecard.csv
"""
import argparse
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path

import pandas as pd


CARD_RE = re.compile(r":\s*(\d+)")


def true_card(true_card_script: Path, data_dir: Path, query_json: Path):
    r = subprocess.run(
        ["python3", str(true_card_script), str(data_dir), str(query_json)],
        capture_output=True, text=True,
    )
    if r.returncode != 0:
        err = r.stderr.strip()[:200] or f"exit={r.returncode} (no stderr, likely killed by signal)"
        return "ERR", err
    for line in r.stdout.splitlines():
        m = CARD_RE.search(line)
        if m:
            return int(m.group(1)), ""
    return "ERR", "no cardinality found in output"


def assemble_data_dir(dataset_dir: Path, updated_label: str,
                      updated_csv: Path, out_dir: Path):
    out_dir.mkdir(parents=True, exist_ok=True)
    for f in out_dir.iterdir():
        f.unlink()
    for csv in dataset_dir.glob("*.csv"):
        (out_dir / csv.name).symlink_to(csv.resolve())
    target = out_dir / f"{updated_label}.csv"
    if target.exists() or target.is_symlink():
        target.unlink()
    shutil.copy(updated_csv, target)


def qerror_value(est: float, tru: float) -> float:
    if tru == 0 and est == 0:
        return 1.0
    if tru == 0:
        return math.inf
    if est == 0:
        return tru
    return round(max(est / tru, tru / est), 4)


def is_already_done(value) -> bool:
    if value is None or (isinstance(value, float) and math.isnan(value)):
        return False
    try:
        float(value)
        return True
    except (ValueError, TypeError):
        return False


def worker_task(payload: dict):
    """Run one true-card computation in a worker process.

    payload keys:
      - idx (int)               row index in CSV
      - tcs (str)               path to true_cardinality.py
      - qjson (str)             path to query JSON
      - dataset (str)           original dataset CSV dir
      - mode ('baseline'|'update')
      - edge_label (str|None)   only for update mode
      - edge_csv (str|None)     only for update mode (updated edge CSV path)
    """
    if payload["mode"] == "baseline":
        tc, err = true_card(Path(payload["tcs"]),
                            Path(payload["dataset"]),
                            Path(payload["qjson"]))
    else:
        with tempfile.TemporaryDirectory(prefix="truecard_") as td:
            assemble_data_dir(Path(payload["dataset"]),
                              payload["edge_label"],
                              Path(payload["edge_csv"]),
                              Path(td))
            tc, err = true_card(Path(payload["tcs"]), Path(td),
                                Path(payload["qjson"]))
    return payload["idx"], tc, err


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--csv", type=Path, required=True)
    ap.add_argument("--dataset", type=Path, required=True)
    ap.add_argument("--result-dir", type=Path, required=True,
                    help="folder containing <pattern>/<position>/pct<label>/<edge>.csv")
    ap.add_argument("--pattern-np-dir", type=Path, required=True)
    ap.add_argument("--pattern-p-dir", type=Path, required=True)
    ap.add_argument("--true-card-script", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--workers", type=int, default=1,
                    help="parallel DuckDB processes (default 1 = serial)")
    args = ap.parse_args()

    # Use --out as the working CSV if it exists, so reruns resume from there.
    src = args.out if args.out.exists() else args.csv
    df = pd.read_csv(src)
    df["true_card"] = df["true_card"].astype(object)
    df["qerror"] = df["qerror"].astype(object)

    # ── Build task list: skip already-done rows; pre-mark NO_QUERY/NO_EDGE_CSV ──
    tasks = []
    for i, row in df.iterrows():
        if is_already_done(row["true_card"]):
            continue
        qvar = row["query_variant"]
        qjson = (args.pattern_np_dir / f"{row['pattern']}.json"
                 if qvar == "base"
                 else args.pattern_p_dir / f"{qvar}.json")
        if not qjson.exists():
            df.at[i, "true_card"] = "NO_QUERY"
            df.at[i, "qerror"] = "NO_QUERY"
            continue
        if row["update_pct"] == 0:
            tasks.append({
                "idx": i,
                "tcs": str(args.true_card_script),
                "qjson": str(qjson),
                "dataset": str(args.dataset),
                "mode": "baseline",
            })
        else:
            pct_label = int(round(row["update_pct"] * 100))
            edge_csv = (args.result_dir / row["pattern"] / row["position"]
                        / f"pct{pct_label}" / f"{row['edge_label']}.csv")
            if not edge_csv.exists():
                df.at[i, "true_card"] = "NO_EDGE_CSV"
                df.at[i, "qerror"] = "NO_EDGE_CSV"
                continue
            tasks.append({
                "idx": i,
                "tcs": str(args.true_card_script),
                "qjson": str(qjson),
                "dataset": str(args.dataset),
                "mode": "update",
                "edge_label": row["edge_label"],
                "edge_csv": str(edge_csv),
            })

    df.to_csv(args.out, index=False)  # flush pre-marked rows
    print(f"tasks queued: {len(tasks)}  workers: {args.workers}  "
          f"DUCKDB_THREADS={os.environ.get('DUCKDB_THREADS','<unset>')}",
          file=sys.stderr)

    if not tasks:
        print("nothing to do", file=sys.stderr)
        return

    # ── Run, in parallel if --workers > 1 ──
    completed = 0
    total = len(tasks)
    if args.workers <= 1:
        result_iter = ((worker_task(t),) for t in tasks)
        results = (r[0] for r in result_iter)
    else:
        ex = ProcessPoolExecutor(max_workers=args.workers)
        futures = [ex.submit(worker_task, t) for t in tasks]
        results = (f.result() for f in as_completed(futures))

    for idx, tc, err in results:
        row = df.loc[idx]
        df.at[idx, "true_card"] = tc
        if isinstance(tc, int):
            try:
                est = float(row["estimated_card"])
                df.at[idx, "qerror"] = qerror_value(est, float(tc))
            except (ValueError, TypeError):
                df.at[idx, "qerror"] = "EST_NOT_NUMERIC"
        else:
            df.at[idx, "qerror"] = tc

        df.to_csv(args.out, index=False)
        completed += 1
        tag = (f"[{completed}/{total}] row#{idx} "
               f"{row['pattern']}/{row['query_variant']}/{row['position']}"
               f"/pct={row['update_pct']}")
        if err:
            print(f"{tag} -> {tc}  ({err})", file=sys.stderr)
        else:
            print(f"{tag} -> true={tc} est={row['estimated_card']} "
                  f"qerr={df.at[idx,'qerror']}", flush=True)

    if args.workers > 1:
        ex.shutdown()
    print(f"\ndone. wrote {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
