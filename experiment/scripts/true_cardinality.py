#!/usr/bin/env python3
"""
Compute true cardinality of a graph pattern query using DuckDB.

Usage:
    python3 true_cardinality.py <data_dir> <query.json> [query2.json ...]
    python3 true_cardinality.py <data_dir> --all

Example:
    python3 true_cardinality.py experiment/dataset/ldbc/sf1 experiment/pattern/LDBC/L4_cycle/L4.json
"""


import json
import sys
import os
import glob
import time

try:
    import duckdb
except ImportError:
    print("pip install duckdb", file=sys.stderr)
    sys.exit(1)


OP_MAP = {
    "eq": "=", "ne": "!=", "gt": ">", "ge": ">=", "lt": "<", "le": "<=",
}


def load_query(path):
    with open(path) as f:
        return json.load(f)


def query_to_sql(query, data_dir):
    vertices = {v["id"]: v["label"] for v in query["vertices"]}
    edges = query["edges"]
    predicates = query.get("predicates", [])

    vertex_expr = {}
    from_parts = []

    for i, e in enumerate(edges):
        alias = f"e{e['id']}"
        csv_file = os.path.join(data_dir, f"{e['label']}.csv")
        table_expr = f"read_csv('{csv_file}', sample_size=-1) {alias}"

        if i == 0:
            from_parts.append(table_expr)
            vertex_expr[e["src"]] = f"{alias}.src"
            vertex_expr[e["dst"]] = f"{alias}.dst"
        else:
            on_conds = []
            for side in ("src", "dst"):
                vid = e[side]
                col_expr = f"{alias}.{side}"
                if vid in vertex_expr:
                    on_conds.append(f"{col_expr} = {vertex_expr[vid]}")
                else:
                    vertex_expr[vid] = col_expr
            if on_conds:
                from_parts.append(f"JOIN {table_expr} ON {' AND '.join(on_conds)}")
            else:
                from_parts.append(f"CROSS JOIN {table_expr}")

    where_parts = []
    vertex_table_joined = {}

    for pred in predicates:
        val = pred["value"]
        if isinstance(val, dict):
            val_sql = f"'{list(val.values())[0]}'" if "String" in val else str(list(val.values())[0])
        else:
            val_sql = f"'{val}'" if isinstance(val, str) else str(val)
        op = OP_MAP[pred["op"]]

        if pred["target"] == "vertex":
            vid = pred["id"]
            if vid not in vertex_table_joined:
                v_alias = f"v{vid}"
                csv_file = os.path.join(data_dir, f"{vertices[vid]}.csv")
                from_parts.append(f"JOIN read_csv('{csv_file}', sample_size=-1) {v_alias} ON {v_alias}.id = {vertex_expr[vid]}")
                vertex_table_joined[vid] = v_alias
            where_parts.append(f"{vertex_table_joined[vid]}.{pred['property']} {op} {val_sql}")
        elif pred["target"] == "edge":
            where_parts.append(f"e{pred['id']}.{pred['property']} {op} {val_sql}")

    sql = "SELECT COUNT(*) AS cardinality\nFROM " + "\n".join(from_parts)
    if where_parts:
        sql += "\nWHERE " + " AND ".join(where_parts)
    return sql


def run_query(data_dir, query_path):
    query = load_query(query_path)
    sql = query_to_sql(query, data_dir)
    name = os.path.splitext(os.path.basename(query_path))[0]

    con = duckdb.connect()
    t0 = time.time()
    result = con.execute(sql).fetchone()[0]
    elapsed = time.time() - t0
    con.close()

    print(f"{name}: {result}  ({elapsed:.2f}s)")
    return name, result, elapsed


def main():
    if len(sys.argv) < 3:
        print(__doc__.strip())
        sys.exit(1)

    data_dir = sys.argv[1]
    if not os.path.isdir(data_dir):
        print(f"ERROR: data directory not found: {data_dir}", file=sys.stderr)
        sys.exit(1)

    if sys.argv[2] == "--all":
        pattern_dir = os.path.join(
            os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
            "pattern", "LDBC",
        )
        query_files = sorted(glob.glob(os.path.join(pattern_dir, "**/*.json"), recursive=True))
    else:
        query_files = sys.argv[2:]

    if not query_files:
        print("No query files found.", file=sys.stderr)
        sys.exit(1)

    results = []
    for qf in query_files:
        try:
            name, card, elapsed = run_query(data_dir, qf)
            results.append((name, card, elapsed))
        except Exception as e:
            print(f"{os.path.basename(qf)}: ERROR - {e}", file=sys.stderr)

    if len(results) > 1:
        print("\n=== Summary ===")
        for name, card, elapsed in results:
            print(f"  {name:20s}  {card:>15,}  ({elapsed:.2f}s)")


if __name__ == "__main__":
    main()
