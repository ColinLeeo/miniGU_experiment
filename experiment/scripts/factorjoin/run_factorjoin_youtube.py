#!/usr/bin/env python3
import argparse
import csv
import json
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
    build_left_deep_subplans,
    estimate_prepared_factorjoin_query,
    prepare_factorjoin_query,
)

DEFAULT_DATA_PATH = str(EXPERIMENT / "datasets/youtube/{}.csv")
DEFAULT_PATTERN_DIR = EXPERIMENT / "patterns/factorjoin/youtube"
DEFAULT_TRUE_DIR = EXPERIMENT / "patterns/truecard/youtube"
DEFAULT_OUTPUT = EXPERIMENT / "result/baselines/factorjoin/estimate/youtube.csv"


def natural_key(path: Path):
    import re
    return [int(x) if x.isdigit() else x for x in re.split(r"(\d+)", path.as_posix())]


def model_path(model_dir, bucket_method, n_bins):
    return Path(model_dir) / f"model_youtube_{bucket_method}_{n_bins}.pkl"


def build(args):
    from Evaluation.training import train_one_stats

    out = Path(args.model_dir)
    out.mkdir(parents=True, exist_ok=True)
    target = model_path(out, args.bucket_method, args.n_bins)
    if target.exists() and not args.force:
        print(f"model exists: {target}")
        return target
    train_one_stats(
        dataset="youtube",
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


def build_youtube_factorjoin_query(pattern_path):
    pattern = json.loads(Path(pattern_path).read_text(encoding="utf-8"))
    vertices = {int(v["id"]): str(v["label"]).lower() for v in pattern["vertices"]}
    edges = pattern["edges"]

    from_terms = []
    alias_to_table = {}
    adjacency = {}
    join_conditions = []

    for vertex_id in sorted(vertices):
        alias = f"v{vertex_id}"
        table = vertices[vertex_id]
        alias_to_table[alias] = table
        adjacency.setdefault(alias, set())
        from_terms.append(f"{table} AS {alias}")

    for edge in edges:
        alias = f"e{edge['id']}"
        table = str(edge["label"]).lower()
        src_alias = f"v{edge['src']}"
        dst_alias = f"v{edge['dst']}"
        alias_to_table[alias] = table
        adjacency.setdefault(alias, set())
        from_terms.append(f"{table} AS {alias}")
        for left, right in ((f"{alias}.src", f"{src_alias}.id"), (f"{alias}.dst", f"{dst_alias}.id")):
            join_conditions.append((left, right))
            a0 = left.split(".", 1)[0]
            a1 = right.split(".", 1)[0]
            adjacency.setdefault(a0, set()).add(a1)
            adjacency.setdefault(a1, set()).add(a0)

    conditions = [f"{left} = {right}" for left, right in join_conditions]
    sql = "SELECT COUNT(*) FROM " + ", ".join(from_terms)
    if conditions:
        sql += " WHERE " + " AND ".join(conditions)

    aliases = list(alias_to_table)
    return {
        "sql": sql + ";",
        "aliases": aliases,
        "alias_to_table": alias_to_table,
        "join_conditions": join_conditions,
        "table_queries": {},
        "subplans": build_left_deep_subplans(aliases, adjacency),
        "has_self_join": len(set(alias_to_table.values())) != len(alias_to_table),
    }


def read_truth(true_dir: Path):
    truth = {}
    for path in true_dir.glob("*.graph"):
        text = path.read_text(encoding="utf-8").strip()
        if text:
            truth[path.stem] = float(text)
    return truth


def qerror(pred, truth):
    pred = max(float(pred), 1.0)
    truth = max(float(truth), 1.0)
    return max(pred / truth, truth / pred)


def query(args):
    pattern_dir = Path(args.pattern_dir)
    output = Path(args.output_csv)
    output.parent.mkdir(parents=True, exist_ok=True)
    truth = read_truth(Path(args.true_dir))

    with Path(args.model_path).open("rb") as f:
        model = pickle.load(f)
    validate_bucket_shapes(model)
    if model.bns is not None:
        for bn in model.bns.values():
            bn.init_inference_method()

    patterns = sorted(
        (path for path in pattern_dir.glob("*.json") if path.stem in truth),
        key=natural_key,
    )
    if args.limit is not None:
        patterns = patterns[: args.limit]

    with output.open("w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(["query", "estimate", "latency_s", "status", "notes", "true_cardinality", "qerror"])
        for pattern_path in patterns:
            started = None
            true = truth.get(pattern_path.stem)
            try:
                context = build_youtube_factorjoin_query(pattern_path)
                alias_model = prepare_factorjoin_query(model, context)
                started = time.perf_counter()
                pred = estimate_prepared_factorjoin_query(alias_model, context)
                elapsed = time.perf_counter() - started
                note_prefix = "alias_vertex_label_subplan_self_join" if context["has_self_join"] else "alias_vertex_label_subplan"
                note_prefix += f";catalog={Path(args.model_path).name}"
                note_prefix += ";latency_scope=bound_inference_only"
                if pred is None or not math.isfinite(float(pred)) or float(pred) <= 0:
                    est = 1.0
                    status = "failed_non_finite"
                    notes = f"{note_prefix}: invalid estimate {pred}; accuracy fallback=1"
                else:
                    est = float(pred)
                    status = "ok"
                    notes = note_prefix
                qe = qerror(est, true) if true is not None else ""
                writer.writerow([pattern_path.stem, f"{est:.17g}", f"{elapsed:.6f}", status, notes, f"{true:.17g}" if true is not None else "", f"{qe:.17g}" if qe != "" else ""])
            except Exception as exc:
                elapsed = time.perf_counter() - started if started is not None else ""
                latency = f"{elapsed:.6f}" if elapsed != "" else ""
                qe = qerror(1.0, true) if true is not None else ""
                writer.writerow([pattern_path.stem, "1", latency, f"failed: {type(exc).__name__}: {exc}", "accuracy fallback=1", f"{true:.17g}" if true is not None else "", f"{qe:.17g}" if qe != "" else ""])
    print(f"Saved FactorJoin YouTube estimates: {output}")
    print(f"Queries: {len(patterns)}")


def main():
    parser = argparse.ArgumentParser(description="Build/query FactorJoin for YouTube graph patterns.")
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
    q.add_argument("--true-dir", default=str(DEFAULT_TRUE_DIR))
    q.add_argument("--pattern-dir", default=str(DEFAULT_PATTERN_DIR))
    q.add_argument("--model-path", required=True)
    q.add_argument("--output-csv", default=str(DEFAULT_OUTPUT))
    q.add_argument("--limit", type=int)

    args = parser.parse_args()
    if args.command == "build":
        build(args)
    elif args.command == "query":
        query(args)


if __name__ == "__main__":
    main()
