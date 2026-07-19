#!/usr/bin/env python3
import argparse
import csv
import json
import math
import pickle
import sys
import time
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[3]
EXP = ROOT / "experiment"
BAYESCARD_DIR = EXP / "baseline/SafeBound/bayescard"
sys.path.insert(0, str(BAYESCARD_DIR))
MODEL_DIR = EXP / "result/sql_baselines/aids_merged_predicates_4baselines/bayescard"
PATTERN_DIR = EXP / "patterns/gcard/aids_merged_predicates"
TRUTH = EXP / "result/truecard/aids_merged_predicates/true_card.csv"
OUT = EXP / "result/sql_baselines/aids_merged_predicates_4baselines/bayescard/estimate.csv"

OP_SQL = {
    "eq": "=",
    "ne": "!=",
    "gt": ">",
    "ge": ">=",
    "lt": "<",
    "le": "<=",
}


def unwrap(value):
    if isinstance(value, dict):
        return next(iter(value.values()))
    return value


def row_num(bn):
    return float(getattr(bn, "row_num", getattr(bn, "nrows", 1.0)))


def table_row_num(all_bns, table):
    if table not in all_bns:
        raise KeyError(f"Missing BayesCard BN for table {table}")
    return row_num(all_bns[table])


def table_for_predicate(pattern, vertices, pred):
    if pred["target"] == "vertex":
        return vertices[pred["id"]]
    edge = next(e for e in pattern["edges"] if e["id"] == pred["id"])
    return str(edge["label"]).lower()


def validate_model_covers_patterns(model, pattern_paths):
    all_bns = model.get("bns", {})
    required_tables = set()
    required_columns = {}
    for pattern_path in pattern_paths:
        pattern = json.loads(Path(pattern_path).read_text(encoding="utf-8"))
        vertices = {v["id"]: str(v["label"]).lower() for v in pattern["vertices"]}
        required_tables.update(vertices.values())
        required_tables.update(str(e["label"]).lower() for e in pattern["edges"])
        for pred in pattern.get("predicates", []):
            table = table_for_predicate(pattern, vertices, pred)
            required_columns.setdefault(table, set()).add(pred["property"])
    missing_tables = sorted(required_tables - set(all_bns))
    if missing_tables:
        raise RuntimeError(f"BayesCard model is missing required BNs: {missing_tables}")
    missing_columns = []
    for table, columns in sorted(required_columns.items()):
        bn_columns = set(getattr(all_bns[table], "node_names", []) or [])
        for column in sorted(columns):
            if column not in bn_columns:
                missing_columns.append(f"{table}.{column}")
    if missing_columns:
        raise RuntimeError(f"BayesCard model is missing predicate columns: {missing_columns}")


def predicate_selectivity(all_bns, table, prop, op, raw_value):
    if op not in OP_SQL:
        raise KeyError(f"Unsupported predicate operator {op}")
    bn = all_bns[table]
    before = row_num(bn)
    if before <= 0:
        raise ValueError(f"Invalid row count for BayesCard BN {table}: {before}")
    value = unwrap(raw_value)
    sql = f"SELECT COUNT(*) FROM {table} WHERE {prop} {OP_SQL[op]} {value}"
    from Evaluation.cardinality_estimation import parse_query_single_table
    query_obj = parse_query_single_table(sql, bn)
    bn.infer_algo = "progressive_sampling"
    estimated = float(bn.query(query_obj, sample_size=10000))
    if not math.isfinite(estimated):
        raise ValueError(f"Non-finite BayesCard predicate estimate for {sql}: {estimated}")
    return max(0.0, estimated) / before


def estimate_one(model, pattern_path):
    started = time.perf_counter()
    pattern = json.loads(Path(pattern_path).read_text(encoding="utf-8"))
    vertices = {v["id"]: str(v["label"]).lower() for v in pattern["vertices"]}
    all_bns = model.get("bns", {})

    cardinality = 1.0
    for edge in pattern["edges"]:
        cardinality *= table_row_num(all_bns, str(edge["label"]).lower())

    for pred in pattern.get("predicates", []):
        table = table_for_predicate(pattern, vertices, pred)
        cardinality *= predicate_selectivity(all_bns, table, pred["property"], pred["op"], pred["value"])

    vertex_refs = {}
    for edge in pattern["edges"]:
        table = str(edge["label"]).lower()
        vertex_refs.setdefault(edge["src"], []).append(table)
        vertex_refs.setdefault(edge["dst"], []).append(table)

    for vid, tables in vertex_refs.items():
        for _ in range(max(0, len(tables) - 1)):
            cardinality /= table_row_num(all_bns, vertices[vid])

    elapsed = time.perf_counter() - started
    return max(1.0, float(cardinality)), elapsed


def main():
    ap = argparse.ArgumentParser(description="Predicate-aware AIDS BayesCard-style batch estimator.")
    ap.add_argument("--model-dir", type=Path, default=MODEL_DIR)
    ap.add_argument("--pattern-dir", type=Path, default=PATTERN_DIR)
    ap.add_argument("--truth", type=Path, default=TRUTH)
    ap.add_argument("--output-csv", type=Path, default=OUT)
    args = ap.parse_args()

    np.random.seed(0)
    with (args.model_dir / "bayescard.pkl").open("rb") as f:
        model = pickle.load(f)
    for bn in model.get("bns", {}).values():
        bn.infer_algo = "progressive_sampling"

    rows = list(csv.DictReader(args.truth.open(newline="", encoding="utf-8")))
    pattern_paths = [args.pattern_dir / (row["pattern"] + ".json") for row in rows]
    validate_model_covers_patterns(model, pattern_paths)
    args.output_csv.parent.mkdir(parents=True, exist_ok=True)
    with args.output_csv.open("w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(["query_file", "prediction", "latency_s", "status", "notes", "topology", "query", "pattern", "true_cardinality"])
        for row in rows:
            pattern_rel = row["pattern"] + ".json"
            try:
                pred, elapsed = estimate_one(model, args.pattern_dir / pattern_rel)
                status = "ok" if math.isfinite(pred) and pred > 0 else "ok_floor"
                if status != "ok":
                    pred = 1.0
                writer.writerow([pattern_rel, f"{pred:.17g}", f"{elapsed:.6f}", status, "bayescard_independent_join_with_edge_bn_predicate_selectivity", row["topology"], row["query"], row["pattern"], row["true_cardinality"]])
            except Exception as exc:
                writer.writerow([pattern_rel, "", "0.000000", f"failed: {type(exc).__name__}: {exc}", "", row.get("topology", ""), row.get("query", ""), row.get("pattern", ""), row.get("true_cardinality", "")])
    print(f"Saved BayesCard estimates: {args.output_csv}")
    print(f"Queries: {len(rows)}")


if __name__ == "__main__":
    main()
