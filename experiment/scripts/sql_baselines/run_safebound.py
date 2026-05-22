#!/usr/bin/env python3
"""
SafeBound wrapper for LDBC cardinality estimation.

Usage:
    # Build statistics (one-time)
    python run_safebound.py build --data-dir experiment/datasets/ldbc/sf1 \
                                  --schema experiment/schemas/ldbc/ldbc_pathce_schema.json \
                                  --model-dir experiment/catalogs/ldbc/safebound/sf1

    # Estimate cardinality for a single query
    python run_safebound.py estimate --model-dir experiment/catalogs/ldbc/safebound/sf1 \
                                     --query experiment/pattern/LDBC/L1_path/L1.json

Output (estimate mode): cardinality,time_s on stdout
"""
import sys
import os
import json
import time
import pickle
import argparse

import pandas as pd
import numpy as np


def find_csv(data_dir, name):
    """Find CSV file, trying lowercase first."""
    for n in [name.lower(), name]:
        p = os.path.join(data_dir, f"{n}.csv")
        if os.path.exists(p):
            return p
    return None


def load_schema(schema_path):
    """Load pathce schema and build label mappings."""
    with open(schema_path) as f:
        schema = json.load(f)

    vertex_id_to_name = {}
    for name, lid in schema["vertex_labels"].items():
        vertex_id_to_name[lid] = name.lower()

    edge_id_to_name = {}
    for name, lid in schema["edge_labels"].items():
        edge_id_to_name[lid] = name.lower()

    return schema, vertex_id_to_name, edge_id_to_name


def build_statistics(data_dir, schema_path, model_dir):
    """Build SafeBound statistics from LDBC CSV data."""
    # Import SafeBound
    safebound_src = os.path.join(
        os.path.dirname(__file__), "../../baseline/safebound/Source"
    )
    sys.path.insert(0, safebound_src)
    from SafeBoundUtils import SafeBound as SB

    schema, vertex_id_to_name, edge_id_to_name = load_schema(schema_path)

    table_dfs = []
    table_names = []
    table_join_cols = []
    filter_cols = []
    fk_to_k_dict = {}

    # Load vertex tables
    for name, lid in schema["vertex_labels"].items():
        tname = name.lower()
        csv_path = find_csv(data_dir, tname)
        if csv_path is None:
            continue
        df = pd.read_csv(csv_path, sep=",")
        table_dfs.append(df)
        table_names.append(tname)
        table_join_cols.append(["id"])
        # All non-id columns are potential filter columns
        filter_cols.append([c for c in df.columns if c != "id"])

    # Load edge tables
    for name, lid in schema["edge_labels"].items():
        tname = name.lower()
        csv_path = find_csv(data_dir, tname)
        if csv_path is None:
            continue
        df = pd.read_csv(csv_path, sep=",")
        table_dfs.append(df)
        table_names.append(tname)
        table_join_cols.append(["src", "dst"])
        filter_cols.append([c for c in df.columns if c not in ("src", "dst")])

    # Build FK-PK relationships from schema edges.
    # SafeBound requires FK-PK graph to be acyclic, so we skip self-referencing
    # edges (e.g., person_knows_person where src and dst both reference person).
    for edge_def in schema.get("edges", []):
        edge_name = edge_id_to_name.get(edge_def["label"])
        from_name = vertex_id_to_name.get(edge_def["from"])
        to_name = vertex_id_to_name.get(edge_def["to"])

        if not (edge_name and from_name and to_name):
            continue
        if edge_name not in [t for t in table_names]:
            continue
        # Skip self-referencing (src and dst point to same vertex table)
        if from_name == to_name:
            continue
        # Skip if referenced vertex table not loaded
        if from_name not in table_names or to_name not in table_names:
            continue

        if edge_name not in fk_to_k_dict:
            fk_to_k_dict[edge_name] = []
        fk_to_k_dict[edge_name].append(["src", from_name, "id"])
        fk_to_k_dict[edge_name].append(["dst", to_name, "id"])

    print(f"Building SafeBound with {len(table_names)} tables, {len(fk_to_k_dict)} FK relationships...")
    # Debug: print first table's info
    print(f"  First table: {table_names[0]}, cols: {list(table_dfs[0].columns)[:5]}, joinCols: {table_join_cols[0]}")
    sb = SB(
        tableDFs=table_dfs,
        tableNames=table_names,
        tableJoinCols=table_join_cols,
        relativeErrorPerSegment=0.01,
        originalFilterCols=filter_cols,
        FKtoKDict=fk_to_k_dict,
        numCores=4,
        numBuckets=25,
        verbose=True,
    )

    os.makedirs(model_dir, exist_ok=True)
    model_path = os.path.join(model_dir, "safebound.pkl")
    with open(model_path, "wb") as f:
        pickle.dump(sb, f)
    print(f"Saved: {model_path}")


def estimate_cardinality(model_dir, query_path):
    """Estimate cardinality for a miniGU pattern using SafeBound."""
    safebound_src = os.path.join(
        os.path.dirname(__file__), "../../baseline/safebound/Source"
    )
    sys.path.insert(0, safebound_src)
    from JoinGraphUtils import JoinQueryGraph

    model_path = os.path.join(model_dir, "safebound.pkl")
    with open(model_path, "rb") as f:
        sb = pickle.load(f)

    with open(query_path) as f:
        pattern = json.load(f)

    vertices = {v["id"]: v["label"].lower() for v in pattern["vertices"]}
    edges = pattern["edges"]
    predicates = pattern.get("predicates", [])

    # Build JoinQueryGraph
    query = JoinQueryGraph()

    # Add edge table aliases
    for e in edges:
        alias = f"e{e['id']}"
        query.addAlias(e["label"].lower(), alias)

    # Track vertex pinning
    vertex_expr = {}  # vertex_id -> (alias, "src"|"dst")

    for i, e in enumerate(edges):
        alias = f"e{e['id']}"
        if i == 0:
            vertex_expr[e["src"]] = (alias, "src")
            vertex_expr[e["dst"]] = (alias, "dst")
        else:
            for side in ("src", "dst"):
                vid = e[side]
                if vid in vertex_expr:
                    prev_alias, prev_col = vertex_expr[vid]
                    query.addJoin(prev_alias, prev_col, alias, side)
                else:
                    vertex_expr[vid] = (alias, side)

    # Handle vertex predicates
    vertex_aliases = {}
    for pred in predicates:
        if pred["target"] == "vertex":
            vid = pred["id"]
            label = vertices[vid]
            if vid not in vertex_aliases:
                v_alias = f"v{vid}"
                query.addAlias(label, v_alias)
                pin_alias, pin_col = vertex_expr[vid]
                query.addJoin(pin_alias, pin_col, v_alias, "id")
                vertex_aliases[vid] = v_alias

            v_alias = vertex_aliases[vid]
            val = pred["value"]
            if isinstance(val, dict):
                val = list(val.values())[0]
            op = {"eq": "=", "ne": "!=", "gt": ">", "ge": ">=", "lt": "<", "le": "<="}[pred["op"]]
            query.addPredicate(v_alias, pred["property"], op, val)

        elif pred["target"] == "edge":
            alias = f"e{pred['id']}"
            val = pred["value"]
            if isinstance(val, dict):
                val = list(val.values())[0]
            op = {"eq": "=", "ne": "!=", "gt": ">", "ge": ">=", "lt": "<", "le": "<="}[pred["op"]]
            query.addPredicate(alias, pred["property"], op, val)

    query.buildJoinGraph()

    t0 = time.time()
    bound = sb.functionalFrequencyBound(query)
    elapsed = time.time() - t0

    print(f"{bound},{elapsed}")


def main():
    parser = argparse.ArgumentParser(description="SafeBound wrapper for LDBC")
    sub = parser.add_subparsers(dest="cmd")

    b = sub.add_parser("build")
    b.add_argument("--data-dir", required=True)
    b.add_argument("--schema", required=True)
    b.add_argument("--model-dir", required=True)

    e = sub.add_parser("estimate")
    e.add_argument("--model-dir", required=True)
    e.add_argument("--query", required=True)

    args = parser.parse_args()
    if args.cmd == "build":
        build_statistics(args.data_dir, args.schema, args.model_dir)
    elif args.cmd == "estimate":
        estimate_cardinality(args.model_dir, args.query)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
