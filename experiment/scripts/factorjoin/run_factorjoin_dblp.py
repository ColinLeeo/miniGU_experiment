#!/usr/bin/env python3
import argparse
import csv
import math
import pickle
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
EXPERIMENT = ROOT / "experiment"
FACTORJOIN_DIR = EXPERIMENT / "baseline/FactorJoin"
sys.path.insert(0, str(FACTORJOIN_DIR))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from run_factorjoin_aids import validate_bucket_shapes
from run_factorjoin_ldbc import (
    build_factorjoin_query,
    estimate_prepared_factorjoin_query,
    prepare_factorjoin_query,
)

DEFAULT_DATA_PATH = str(EXPERIMENT / "datasets/dblp/{}.csv")
DEFAULT_PATTERN_DIR = EXPERIMENT / "run_tmp/dblp_gcard_partitioned_patterns"
DEFAULT_TRUTH_LONG = EXPERIMENT / "result/truecard/dblp/true_card.csv"
DEFAULT_OUTPUT = EXPERIMENT / "result/sql_baselines/dblp/factorjoin/estimate.csv"


def model_path(model_dir, bucket_method, n_bins):
    return Path(model_dir) / f"model_dblp_{bucket_method}_{n_bins}.pkl"


def build(args):
    from Evaluation.training import train_one_stats

    out = Path(args.model_dir)
    out.mkdir(parents=True, exist_ok=True)
    target = model_path(out, args.bucket_method, args.n_bins)
    if target.exists() and not args.force:
        print(f"model exists: {target}")
        return target
    train_one_stats(
        dataset="dblp",
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


def read_truth_rows(path):
    by_pattern = {}
    with Path(path).open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            pattern = row.get("pattern") or row.get("query")
            truth = row.get("true_cardinality")
            if not pattern or not truth or pattern in by_pattern:
                continue
            by_pattern[pattern] = {
                "pattern": pattern,
                "topology": row.get("topology", pattern.split("_", 2)[1] if "_" in pattern else ""),
                "query": row.get("query", pattern),
                "true_cardinality": truth,
            }
    return [by_pattern[key] for key in sorted(by_pattern)]


def query(args):
    pattern_dir = Path(args.pattern_dir)
    output = Path(args.output_csv)
    output.parent.mkdir(parents=True, exist_ok=True)

    with Path(args.model_path).open("rb") as f:
        model = pickle.load(f)
    validate_bucket_shapes(model)
    if model.bns is not None:
        for bn in model.bns.values():
            bn.init_inference_method()

    rows = read_truth_rows(args.truth_long)
    if args.limit is not None:
        rows = rows[: args.limit]

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
                note_prefix = "alias_subplan_self_join_partitioned" if context["has_self_join"] else "alias_subplan_partitioned"
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
    parser = argparse.ArgumentParser(description="Build/query FactorJoin for DBLP partitioned graph patterns.")
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
    b.add_argument("--num-tables", type=int, default=0)

    q = sub.add_parser("query")
    q.add_argument("--truth-long", default=DEFAULT_TRUTH_LONG)
    q.add_argument("--pattern-dir", default=DEFAULT_PATTERN_DIR)
    q.add_argument("--model-path", required=True)
    q.add_argument("--output-csv", default=DEFAULT_OUTPUT)
    q.add_argument("--limit", type=int)

    args = parser.parse_args()
    if args.command == "build":
        build(args)
    elif args.command == "query":
        query(args)


if __name__ == "__main__":
    main()
