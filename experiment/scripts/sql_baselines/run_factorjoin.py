#!/usr/bin/env python3
"""
FactorJoin wrapper for LDBC cardinality estimation.

Usage:
    # Build model (one-time)
    python run_factorjoin.py build --data-dir experiment/dataset/ldbc/sf1 \
                                   --model-dir experiment/catalogs/ldbc/factorjoin/sf1

    # Estimate cardinality for a SQL query
    python run_factorjoin.py estimate --model-dir experiment/catalogs/ldbc/factorjoin/sf1 \
                                      --query-sql "SELECT COUNT(*) FROM ..."

    # Estimate from pattern file (auto-converts to SQL)
    python run_factorjoin.py estimate --model-dir experiment/catalogs/ldbc/factorjoin/sf1 \
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
import faulthandler

faulthandler.enable()

import numpy as np
import pandas as pd

# Add FactorJoin to path and change working dir (required for circular imports)
FACTORJOIN_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), "../../baseline/factorjoin"))
sys.path.insert(0, FACTORJOIN_DIR)
os.chdir(FACTORJOIN_DIR)

logging.basicConfig(level=logging.WARNING)


def _read_csv(path):
    """Read a pipe-delimited LDBC CSV."""
    return pd.read_csv(path, sep=",")


def _find_csv(data_dir, name):
    for n in [name.lower(), name]:
        p = os.path.join(data_dir, f"{n}.csv")
        if os.path.exists(p):
            return p
    return None


def build_model(data_dir, model_dir, n_bins=200, n_dim_dist=2, bucket_method="greedy"):
    """Train FactorJoin model on LDBC data."""
    # Force pure-Python fallback to avoid pomegranate segfault
    os.environ["FACTORJOIN_NO_POMEGRANATE"] = "1"

    print(f"[debug] importing FactorJoin modules...", flush=True)
    from Schemas.ldbc.schema import gen_ldbc_schema
    print(f"[debug] imported schema", flush=True)
    from BayesCard.Models.Bayescard_BN import Bayescard_BN
    print(f"[debug] imported Bayescard_BN", flush=True)
    from Join_scheme.binning import greedy_bucketize, Table_bucket
    from Join_scheme.bound import Bound_ensemble
    print(f"[debug] all imports done", flush=True)

    data_template = os.path.join(data_dir, "{}.csv")
    schema = gen_ldbc_schema(data_template)

    print(f"Training FactorJoin on {len(schema.tables)} tables...")

    # Train a BN per table
    all_bns = {}
    null_values = {}
    table_buckets = {}

    # Edge tables: only need row counts + bucket info, skip expensive BN training
    edge_verbs = [
        "_knows_", "_likes_", "_hascreator_", "_hastag_", "_hastype_",
        "_hasinterest_", "_hasmember_", "_hasmoderator_", "_islocatedin_",
        "_ispartof_", "_issubclassof_", "_replyof_", "_containerof_",
        "_studyat_", "_workat_",
    ]

    for table in schema.tables:
        t_name = table.table_name
        csv_path = table.csv_file_location
        if not os.path.exists(csv_path):
            print(f"  SKIP {t_name}: CSV not found")
            continue

        df = pd.read_csv(csv_path, sep=",")

        # Key columns = join columns (id, src, dst)
        key_cols = []
        for col in ["id", "src", "dst"]:
            if col in df.columns:
                key_cols.append(col)

        bin_sizes = {col: n_bins for col in key_cols}
        tb = Table_bucket(t_name, key_cols, bin_sizes)
        tb.table_size = len(df)
        table_buckets[t_name] = tb

        # Skip BN training for edge tables (only vertex tables have useful attributes)
        is_edge = any(v in t_name for v in edge_verbs)
        if is_edge:
            print(f"  Edge table {t_name}: {len(df)} rows (skip BN)")
            continue

        print(f"  Training BN for {t_name}...", flush=True)

        # Track null indicator per column
        col_nulls = {}
        for col in df.columns:
            if df[col].isna().any():
                col_nulls[col] = -1
            else:
                col_nulls[col] = None
        null_values[t_name] = col_nulls

        try:
            bn = Bayescard_BN(t_name, key_cols, n_bins,
                              column_names=list(df.columns), nrows=len(df),
                              null_values=col_nulls)
            bn.build_from_data(df, sample_size=min(200000, len(df)))
            bn.init_inference_method()
            bn.infer_algo = "exact-jit"
            all_bns[t_name] = bn
        except Exception as e:
            import traceback
            print(f"  ERROR training {t_name}: {e}")
            traceback.print_exc()

    # Build ensemble
    ensemble = Bound_ensemble(
        table_buckets=table_buckets,
        schema=schema,
        n_dim_dist=n_dim_dist,
        bns=all_bns,
        null_value=null_values,
    )

    os.makedirs(model_dir, exist_ok=True)
    model_path = os.path.join(model_dir, "factorjoin.pkl")
    with open(model_path, "wb") as f:
        pickle.dump(ensemble, f)
    print(f"Saved: {model_path}")


def estimate_cardinality(model_dir, sql_query):
    """Estimate cardinality for a SQL query using trained FactorJoin model."""
    model_path = os.path.join(model_dir, "factorjoin.pkl")
    with open(model_path, "rb") as f:
        ensemble = pickle.load(f)

    for table in ensemble.bns:
        bn = ensemble.bns[table]
        bn.init_inference_method()

    t0 = time.time()
    result = ensemble.get_cardinality_bound_one(sql_query)
    elapsed = time.time() - t0

    print(f"{result},{elapsed}")


def pattern_to_sql(query_path):
    """Convert miniGU pattern to SQL (inline, no external dependency)."""
    tools_dir = os.path.join(os.path.dirname(__file__), "../../tools")
    sys.path.insert(0, tools_dir)
    from minigu2sql import pattern_to_sql as _p2s

    with open(query_path) as f:
        pattern = json.load(f)
    return _p2s(pattern)


def main():
    parser = argparse.ArgumentParser(description="FactorJoin wrapper for LDBC")
    sub = parser.add_subparsers(dest="cmd")

    b = sub.add_parser("build")
    b.add_argument("--data-dir", required=True)
    b.add_argument("--model-dir", required=True)
    b.add_argument("--n-bins", type=int, default=200)
    b.add_argument("--n-dim-dist", type=int, default=2)
    b.add_argument("--bucket-method", default="greedy")

    e = sub.add_parser("estimate")
    e.add_argument("--model-dir", required=True)
    e.add_argument("--query-sql", help="Raw SQL query string")
    e.add_argument("--query-pattern", help="miniGU pattern JSON file")

    args = parser.parse_args()
    if args.cmd == "build":
        build_model(args.data_dir, args.model_dir, args.n_bins, args.n_dim_dist, args.bucket_method)
    elif args.cmd == "estimate":
        if args.query_pattern:
            sql = pattern_to_sql(args.query_pattern)
        elif args.query_sql:
            sql = args.query_sql
        else:
            print("Error: provide --query-sql or --query-pattern", file=sys.stderr)
            sys.exit(1)
        estimate_cardinality(args.model_dir, sql)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
