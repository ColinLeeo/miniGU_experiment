#!/usr/bin/env python3
"""
BayesCard wrapper for LDBC cardinality estimation.

Usage:
    # Build model
    python run_bayescard.py build --data-dir experiment/datasets/ldbc/sf1 \
                                  --model-dir experiment/catalogs/ldbc/bayescard/sf1

    # Estimate cardinality
    python run_bayescard.py estimate --model-dir experiment/catalogs/ldbc/bayescard/sf1 \
                                     --query-pattern experiment/pattern/LDBC/L1_path/L1.json

Output (estimate mode): cardinality,time_s on stdout
"""
import sys
import os
import json
import time
import pickle
import argparse
import logging

import numpy as np
import pandas as pd

BAYESCARD_DIR = os.path.abspath(
    os.path.join(os.path.dirname(__file__), "../../baseline/SafeBound/bayescard")
)
sys.path.insert(0, BAYESCARD_DIR)
os.chdir(BAYESCARD_DIR)

logging.basicConfig(level=logging.WARNING)


def _is_edge_table(name):
    """Check if a table name is an edge table (contains a verb connecting two entities)."""
    edge_verbs = [
        "_knows_", "_likes_", "_hascreator_", "_hastag_", "_hastype_",
        "_hasinterest_", "_hasmember_", "_hasmoderator_", "_islocatedin_",
        "_ispartof_", "_issubclassof_", "_replyof_", "_containerof_",
        "_studyat_", "_workat_",
    ]
    return any(v in name for v in edge_verbs)


def build_model(data_dir, model_dir, learning_algo="chow-liu", max_parents=1, sample_size=200000):
    """Train BayesCard BN ensemble on graph-wrapper CSV tables.

    Every non-empty CSV table, including edge tables, must get a full BN.
    Edge row counts are stored only as audit metadata, not as an estimation fallback.
    """
    from Models.Bayescard_BN import Bayescard_BN

    # Discover all CSV tables
    csv_files = sorted([
        f for f in os.listdir(data_dir)
        if f.endswith(".csv") and not os.path.islink(os.path.join(data_dir, f))
    ])

    all_bns = {}
    edge_row_counts = {}
    for csv_file in csv_files:
        t_name = csv_file.replace(".csv", "")
        csv_path = os.path.join(data_dir, csv_file)

        df = pd.read_csv(csv_path, sep=",")
        if len(df) == 0:
            raise ValueError(f"Cannot train BayesCard BN for empty table {t_name}")

        # Edge tables: only need row count, skip expensive BN training.
        # LDBC names contain edge verbs; graph-converted IMDB tables expose src/dst columns.
        lower_cols = {str(c).lower() for c in df.columns}
        is_edge_table = _is_edge_table(t_name) or {"src", "dst"}.issubset(lower_cols)
        if is_edge_table:
            edge_row_counts[t_name] = len(df)

        table_kind = "edge" if is_edge_table else "vertex"
        print(f"  Training BN for {table_kind} table {t_name}...")
        bn = Bayescard_BN(t_name)
        bn.build_from_data(df, algorithm=learning_algo, max_parents=max_parents,
                           ignore_cols=[], sample_size=min(sample_size, len(df)))
        bn.init_inference_method()
        bn.infer_algo = "exact"
        all_bns[t_name] = bn
        print(f"    OK ({len(df)} rows, {len(df.columns)} cols)")

    os.makedirs(model_dir, exist_ok=True)
    model_path = os.path.join(model_dir, "bayescard.pkl")
    with open(model_path, "wb") as f:
        pickle.dump({"bns": all_bns, "edge_row_counts": edge_row_counts}, f)
    edge_bn_count = sum(1 for table in edge_row_counts if table in all_bns)
    if edge_bn_count != len(edge_row_counts):
        missing = sorted(set(edge_row_counts) - set(all_bns))
        raise RuntimeError(f"Missing BayesCard edge BNs: {missing}")
    print(f"Saved: {model_path} ({len(all_bns)} BNs, {len(edge_row_counts)} edge tables, {edge_bn_count} edge BNs)")


def estimate_cardinality(model_dir, query_path):
    """Estimate cardinality for a miniGU pattern.

    BayesCard is primarily a single-table estimator. For join queries,
    we estimate each table's selectivity independently and multiply by
    the product of table sizes (independence assumption).
    """
    model_path = os.path.join(model_dir, "bayescard.pkl")
    with open(model_path, "rb") as f:
        data = pickle.load(f)

    # Support both old format (dict of BNs) and new format (dict with bns + edge_row_counts)
    if isinstance(data, dict) and "bns" in data:
        all_bns = data["bns"]
        edge_row_counts = data.get("edge_row_counts", {})
    else:
        all_bns = data
        edge_row_counts = {}

    for bn in all_bns.values():
        bn.init_inference_method()
        bn.infer_algo = "exact"

    with open(query_path) as f:
        pattern = json.load(f)

    vertices = {v["id"]: v["label"].lower() for v in pattern["vertices"]}
    edges = pattern["edges"]
    predicates = pattern.get("predicates", [])

    t0 = time.time()

    # For each edge table, get its size
    cardinality = 1.0
    for e in edges:
        t_name = e["label"].lower()
        if t_name not in all_bns:
            raise KeyError(f"Missing BayesCard BN for edge table {t_name}")
        cardinality *= getattr(all_bns[t_name], "row_num", getattr(all_bns[t_name], "nrows", 1.0))

    # Apply selectivity from predicates
    for pred in predicates:
        if pred["target"] == "vertex":
            t_name = vertices[pred["id"]]
            if t_name in all_bns:
                bn = all_bns[t_name]
                # Build a simple single-table query string
                val = pred["value"]
                if isinstance(val, dict):
                    val = list(val.values())[0]
                op_map = {"eq": "=", "ne": "!=", "gt": ">", "ge": ">=", "lt": "<", "le": "<="}
                op = op_map[pred["op"]]
                sql = f"SELECT COUNT(*) FROM {t_name} WHERE {pred['property']} {op} {val}"
                try:
                    from Evaluation.cardinality_estimation import parse_query_single_table
                    query_obj = parse_query_single_table(sql, bn)
                    sel = bn.query(query_obj) / getattr(bn, "row_num", getattr(bn, "nrows", 1.0))
                    cardinality *= sel
                except Exception:
                    pass

    # Apply join selectivity heuristic (1/max(|R|, |S|) per join)
    vertex_refs = {}
    for e in edges:
        for side in ("src", "dst"):
            vid = e[side]
            if vid not in vertex_refs:
                vertex_refs[vid] = []
            vertex_refs[vid].append(e["label"].lower())

    for vid, tables in vertex_refs.items():
        if len(tables) > 1:
            # Each additional join on same vertex reduces by 1/max_table_size
            v_label = vertices[vid]
            if v_label not in all_bns:
                raise KeyError(f"Missing BayesCard BN for vertex table {v_label}")
            domain_size = getattr(all_bns[v_label], "row_num", getattr(all_bns[v_label], "nrows", 1.0))
            for _ in range(len(tables) - 1):
                cardinality /= domain_size

    elapsed = time.time() - t0
    cardinality = max(1, cardinality)

    print(f"{cardinality},{elapsed}")


def main():
    parser = argparse.ArgumentParser(description="BayesCard wrapper for LDBC")
    sub = parser.add_subparsers(dest="cmd")

    b = sub.add_parser("build")
    b.add_argument("--data-dir", required=True)
    b.add_argument("--model-dir", required=True)
    b.add_argument("--learning-algo", default="chow-liu")
    b.add_argument("--max-parents", type=int, default=1)
    b.add_argument("--sample-size", type=int, default=200000)

    e = sub.add_parser("estimate")
    e.add_argument("--model-dir", required=True)
    e.add_argument("--query-pattern", required=True, help="miniGU pattern JSON")

    args = parser.parse_args()
    if args.cmd == "build":
        build_model(args.data_dir, args.model_dir, args.learning_algo,
                     args.max_parents, args.sample_size)
    elif args.cmd == "estimate":
        estimate_cardinality(args.model_dir, args.query_pattern)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
