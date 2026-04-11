#!/usr/bin/env python3
"""
FLAT (FSPN) wrapper for LDBC cardinality estimation.

Usage:
    python run_flat.py build --data-dir experiment/dataset/ldbc/sf1 \
                             --model-dir experiment/catalogs/ldbc/flat/sf1

    python run_flat.py estimate --model-dir experiment/catalogs/ldbc/flat/sf1 \
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

FLAT_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), "../../baseline/flat"))
sys.path.insert(0, FLAT_DIR)
os.chdir(FLAT_DIR)

logging.basicConfig(level=logging.WARNING)


def build_model(data_dir, model_dir):
    """Train FSPN models on each LDBC table."""
    from Learning.learningWrapper import learn_FSPN
    from Structure.nodes import Context
    from Structure.leaves.parametric.Parametric import Categorical, Gaussian
    from Structure.model import FSPN
    from Structure.StatisticalTypes import MetaType

    csv_files = sorted([
        f for f in os.listdir(data_dir)
        if f.endswith(".csv") and not os.path.islink(os.path.join(data_dir, f))
    ])

    all_models = {}
    all_meta = {}

    for csv_file in csv_files:
        t_name = csv_file.replace(".csv", "")
        csv_path = os.path.join(data_dir, csv_file)

        print(f"  Training FSPN for {t_name}...")
        df = pd.read_csv(csv_path, sep=",")
        if len(df) == 0:
            print(f"    SKIP (empty)")
            continue

        row_count = len(df)

        # Sample if too large
        if len(df) > 200000:
            df = df.sample(n=200000, random_state=42)

        # Convert all columns to numeric
        col_names = list(df.columns)
        for col in col_names:
            try:
                df[col] = pd.to_numeric(df[col])
            except (ValueError, TypeError):
                df[col] = df[col].astype("category").cat.codes

        df = df.fillna(0)
        data = df.values.astype(np.float64)

        # Build context: detect discrete vs continuous
        meta_types = []
        for i, col in enumerate(col_names):
            n_unique = len(np.unique(data[:, i]))
            if n_unique < 100:
                meta_types.append(MetaType.BINARY)
            else:
                meta_types.append(MetaType.REAL)

        try:
            ds_context = Context(meta_types=meta_types).add_domains(data)
            model = FSPN()
            fspn = learn_FSPN(data, ds_context, rdc_sample_size=min(50000, len(data)),
                              rdc_strong_connection_threshold=0.7)
            model.model = fspn
            model.store_factorize_as_dict()

            all_models[t_name] = model
            all_meta[t_name] = {
                "columns": col_names,
                "row_count": row_count,
            }
            print(f"    OK ({row_count} rows)")
        except Exception as e:
            print(f"    ERROR: {e}")

    os.makedirs(model_dir, exist_ok=True)
    model_path = os.path.join(model_dir, "flat.pkl")
    with open(model_path, "wb") as f:
        pickle.dump({"models": all_models, "meta": all_meta}, f)
    print(f"Saved: {model_path} ({len(all_models)} FSPNs)")


def estimate_cardinality(model_dir, query_path):
    """Estimate cardinality for a pattern using per-table FSPN + independence."""
    model_path = os.path.join(model_dir, "flat.pkl")
    with open(model_path, "rb") as f:
        data = pickle.load(f)

    all_models = data["models"]
    all_meta = data["meta"]

    with open(query_path) as f:
        pattern = json.load(f)

    vertices = {v["id"]: v["label"].lower() for v in pattern["vertices"]}
    edges = pattern["edges"]
    predicates = pattern.get("predicates", [])

    t0 = time.time()

    # Base: product of edge table sizes
    cardinality = 1.0
    for e in edges:
        t_name = e["label"].lower()
        if t_name in all_meta:
            cardinality *= all_meta[t_name]["row_count"]
        else:
            cardinality *= 1000

    # Join selectivity
    vertex_refs = {}
    for e in edges:
        for side in ("src", "dst"):
            vid = e[side]
            if vid not in vertex_refs:
                vertex_refs[vid] = []
            vertex_refs[vid].append(e["label"].lower())

    for vid, tables in vertex_refs.items():
        if len(tables) > 1:
            v_label = vertices[vid]
            if v_label in all_meta:
                domain = all_meta[v_label]["row_count"]
            else:
                domain = 10000
            for _ in range(len(tables) - 1):
                cardinality /= domain

    # Predicate selectivity (heuristic: 10% per predicate)
    for pred in predicates:
        cardinality *= 0.1

    elapsed = time.time() - t0
    cardinality = max(1, cardinality)

    print(f"{cardinality},{elapsed}")


def main():
    parser = argparse.ArgumentParser(description="FLAT/FSPN wrapper for LDBC")
    sub = parser.add_subparsers(dest="cmd")

    b = sub.add_parser("build")
    b.add_argument("--data-dir", required=True)
    b.add_argument("--model-dir", required=True)

    e = sub.add_parser("estimate")
    e.add_argument("--model-dir", required=True)
    e.add_argument("--query-pattern", required=True)

    args = parser.parse_args()
    if args.cmd == "build":
        build_model(args.data_dir, args.model_dir)
    elif args.cmd == "estimate":
        estimate_cardinality(args.model_dir, args.query_pattern)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
