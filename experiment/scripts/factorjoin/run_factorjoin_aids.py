#!/usr/bin/env python3
import argparse
import csv
import math
import pickle
import sys
import time
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[3]
EXPERIMENT = ROOT / "experiment"
FACTORJOIN_DIR = EXPERIMENT / "baseline/FactorJoin"
sys.path.insert(0, str(FACTORJOIN_DIR))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from run_factorjoin_ldbc import (
    build_factorjoin_query,
    estimate_prepared_factorjoin_query,
    prepare_factorjoin_query,
)

DEFAULT_DATA_PATH = str(EXPERIMENT / "datasets/aids_merged/{}.csv")
DEFAULT_PATTERN_DIR = EXPERIMENT / "patterns/gcard/aids_merged"
DEFAULT_TRUTH = EXPERIMENT / "result/truecard/aids_merged_current/true_card.csv"


def model_path(model_dir, bucket_method, n_bins):
    return Path(model_dir) / f"model_aids_merged_{bucket_method}_{n_bins}.pkl"


def build(args):
    from Evaluation.training import train_one_stats

    out = Path(args.model_dir)
    out.mkdir(parents=True, exist_ok=True)
    target = model_path(out, args.bucket_method, args.n_bins)
    if target.exists() and not args.force:
        print(f"model exists: {target}")
        return target
    train_one_stats(
        dataset="aids_merged",
        data_path=args.data_path,
        model_folder=str(out),
        n_dim_dist=args.n_dim_dist,
        n_bins=args.n_bins,
        bucket_method=args.bucket_method,
        save_bucket_bins=args.save_bucket_bins,
        get_bin_means=args.get_bin_means,
        seed=args.seed,
        validate=False,
    )
    return target



def validate_bucket_shapes(model):
    for table, bucket in model.table_buckets.items():
        for key in bucket.id_attributes:
            expected = (bucket.bin_sizes[key],)
            actual = np.asarray(bucket.oned_bin_modes[key]).shape
            if actual != expected:
                raise ValueError(
                    f"{table}.{key} 1D mode shape {actual}, expected {expected}"
                )
        if len(bucket.id_attributes) == 2:
            expected = tuple(
                bucket.bin_sizes[key] for key in bucket.id_attributes
            )
            for key in bucket.id_attributes:
                actual = np.asarray(bucket.twod_bin_modes[key]).shape
                if actual != expected:
                    raise ValueError(
                        f"{table}.{key} 2D mode shape {actual}, expected {expected}"
                    )


def query(args):
    truth_path = Path(args.truth)
    pattern_dir = Path(args.pattern_dir)
    output = Path(args.output_csv)
    output.parent.mkdir(parents=True, exist_ok=True)

    with Path(args.model_path).open("rb") as f:
        model = pickle.load(f)
    validate_bucket_shapes(model)
    if model.bns is not None:
        for bn in model.bns.values():
            bn.init_inference_method()

    rows = list(csv.DictReader(truth_path.open(newline="", encoding="utf-8")))
    with output.open("w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(["query_file", "prediction", "latency_s", "status", "notes", "topology", "query", "pattern", "true_cardinality"])
        for row in rows:
            pattern_rel = row["pattern"] + ".json"
            pattern_path = pattern_dir / pattern_rel
            started = None
            try:
                context = build_factorjoin_query(pattern_path)
                alias_model = prepare_factorjoin_query(model, context)
                started = time.perf_counter()
                pred = estimate_prepared_factorjoin_query(alias_model, context)
                elapsed = time.perf_counter() - started
                note_prefix = "alias_subplan_self_join" if context["has_self_join"] else "alias_subplan"
                note_prefix += f";catalog={Path(args.model_path).name}"
                note_prefix += ";latency_scope=bound_inference_only"
                if pred is None or not math.isfinite(float(pred)) or float(pred) <= 0:
                    writer.writerow([pattern_rel, "1", f"{elapsed:.6f}", "failed_non_finite", f"{note_prefix}: invalid estimate {pred}; accuracy fallback=1", row["topology"], row["query"], row["pattern"], row["true_cardinality"]])
                else:
                    writer.writerow([pattern_rel, f"{float(pred):.17g}", f"{elapsed:.6f}", "ok", note_prefix, row["topology"], row["query"], row["pattern"], row["true_cardinality"]])
            except Exception as exc:
                elapsed = time.perf_counter() - started if started is not None else ""
                latency = f"{elapsed:.6f}" if elapsed != "" else ""
                writer.writerow([pattern_rel, "1", latency, f"failed: {type(exc).__name__}: {exc}", "accuracy fallback=1", row["topology"], row["query"], row["pattern"], row["true_cardinality"]])
    print(f"Saved FactorJoin estimates: {output}")
    print(f"Queries: {len(rows)}")


def main():
    parser = argparse.ArgumentParser(description="Build/query FactorJoin for AIDS merged patterns.")
    sub = parser.add_subparsers(dest="command", required=True)

    b = sub.add_parser("build")
    b.add_argument("--data-path", default=DEFAULT_DATA_PATH)
    b.add_argument("--model-dir", required=True)
    b.add_argument("--n-dim-dist", type=int, default=2)
    b.add_argument("--n-bins", type=int, default=100)
    b.add_argument("--bucket-method", default="greedy")
    b.add_argument("--save-bucket-bins", action="store_true")
    b.add_argument("--get-bin-means", action="store_true")
    b.add_argument("--seed", type=int, default=0)
    b.add_argument("--force", action="store_true")

    q = sub.add_parser("query")
    q.add_argument("--truth", default=DEFAULT_TRUTH)
    q.add_argument("--pattern-dir", default=DEFAULT_PATTERN_DIR)
    q.add_argument("--model-path", required=True)
    q.add_argument("--output-csv", required=True)

    args = parser.parse_args()
    if args.command == "build":
        build(args)
    elif args.command == "query":
        query(args)


if __name__ == "__main__":
    main()
