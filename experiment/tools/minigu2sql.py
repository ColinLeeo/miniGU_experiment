#!/usr/bin/env python3
"""
Convert miniGU gcard_query patterns to SQL COUNT(*) queries.

The generated SQL references table names directly (not read_csv), suitable for
SQL-based cardinality estimation tools (FactorJoin, BayesCard, FLAT, SafeBound).

Usage:
    # Single file -> print SQL to stdout
    python minigu2sql.py -p pattern.json

    # Single file -> write to file
    python minigu2sql.py -p pattern.json -o output.sql

    # Directory -> write .sql files preserving structure
    python minigu2sql.py -d pattern_dir/ -o output_dir/

    # Also output structured JSON (for SafeBound etc.)
    python minigu2sql.py -d pattern_dir/ -o output_dir/ --json
"""
import sys
import json
import argparse
from pathlib import Path


OP_MAP = {
    "eq": "=",
    "ne": "!=",
    "gt": ">",
    "ge": ">=",
    "lt": "<",
    "le": "<=",
}


def pattern_to_sql(query):
    """Convert a miniGU pattern JSON to a standard SQL COUNT(*) query.

    Adapted from true_cardinality.py:query_to_sql, but outputs table names
    instead of read_csv() calls.
    """
    vertices = {v["id"]: v["label"] for v in query["vertices"]}
    edges = query["edges"]
    predicates = query.get("predicates", [])

    # Track which (edge_alias, column) references each vertex
    vertex_expr = {}  # vertex_id -> SQL expression (e.g., "e1.src")

    from_parts = []
    join_conditions = []

    for i, e in enumerate(edges):
        alias = f"e{e['id']}"
        table_name = e["label"]

        if i == 0:
            from_parts.append(f"{table_name} {alias}")
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
                from_parts.append(
                    f"JOIN {table_name} {alias} ON {' AND '.join(on_conds)}"
                )
            else:
                from_parts.append(f"CROSS JOIN {table_name} {alias}")

    # Handle predicates
    where_parts = []
    vertex_table_joined = {}

    for pred in predicates:
        if pred["target"] == "vertex":
            vid = pred["id"]
            label = vertices[vid]
            prop = pred["property"]
            op = OP_MAP[pred["op"]]
            val_sql = _format_value(pred["value"])

            if vid not in vertex_table_joined:
                v_alias = f"v{vid}"
                pin_expr = vertex_expr[vid]
                from_parts.append(
                    f"JOIN {label} {v_alias} ON {v_alias}.id = {pin_expr}"
                )
                vertex_table_joined[vid] = v_alias

            v_alias = vertex_table_joined[vid]
            where_parts.append(f"{v_alias}.{prop} {op} {val_sql}")

        elif pred["target"] == "edge":
            eid = pred["id"]
            alias = f"e{eid}"
            prop = pred["property"]
            op = OP_MAP[pred["op"]]
            val_sql = _format_value(pred["value"])
            where_parts.append(f"{alias}.{prop} {op} {val_sql}")

    sql = "SELECT COUNT(*)\nFROM " + "\n".join(from_parts)
    if where_parts:
        sql += "\nWHERE " + " AND ".join(where_parts)

    return sql


def _format_value(val):
    """Format a predicate value for SQL."""
    if isinstance(val, dict):
        if "String" in val:
            return f"'{val['String']}'"
        elif "Int64" in val:
            return str(val["Int64"])
        elif "Float64" in val:
            return str(val["Float64"])
        elif "Boolean" in val:
            return "true" if val["Boolean"] else "false"
        else:
            return str(list(val.values())[0])
    else:
        return f"'{val}'" if isinstance(val, str) else str(val)


def pattern_to_structured(query):
    """Convert a miniGU pattern to a structured JSON description of the join.

    Useful for tools like SafeBound that need FK-PK info rather than raw SQL.
    """
    vertices = {v["id"]: v["label"] for v in query["vertices"]}
    edges = query["edges"]
    predicates = query.get("predicates", [])

    tables = []
    joins = []
    filters = []

    vertex_expr = {}

    for i, e in enumerate(edges):
        alias = f"e{e['id']}"
        tables.append({
            "alias": alias,
            "table": e["label"],
            "columns": ["src", "dst"],
        })

        if i == 0:
            vertex_expr[e["src"]] = f"{alias}.src"
            vertex_expr[e["dst"]] = f"{alias}.dst"
        else:
            for side in ("src", "dst"):
                vid = e[side]
                col_expr = f"{alias}.{side}"
                if vid in vertex_expr:
                    joins.append({
                        "left": vertex_expr[vid],
                        "right": col_expr,
                    })
                else:
                    vertex_expr[vid] = col_expr

    for pred in predicates:
        if pred["target"] == "vertex":
            vid = pred["id"]
            filters.append({
                "table": vertices[vid],
                "vertex_id": vid,
                "join_on": vertex_expr.get(vid),
                "property": pred["property"],
                "op": pred["op"],
                "value": pred["value"],
            })
        elif pred["target"] == "edge":
            eid = pred["id"]
            filters.append({
                "table": f"e{eid}",
                "edge_id": eid,
                "property": pred["property"],
                "op": pred["op"],
                "value": pred["value"],
            })

    return {"tables": tables, "joins": joins, "filters": filters}


def convert_file(input_path, output_path=None, write_json=False):
    """Convert a single pattern file."""
    with open(input_path) as f:
        query = json.load(f)

    sql = pattern_to_sql(query)

    if output_path:
        Path(output_path).parent.mkdir(parents=True, exist_ok=True)
        with open(output_path, "w") as f:
            f.write(sql + "\n")
        print(f"  sql: {input_path} -> {output_path}")

        if write_json:
            json_path = str(output_path).replace(".sql", ".json")
            structured = pattern_to_structured(query)
            with open(json_path, "w") as f:
                json.dump(structured, f, indent=2)
            print(f"  json: {input_path} -> {json_path}")
    else:
        print(sql)

    return sql


def main():
    parser = argparse.ArgumentParser(
        description="Convert miniGU patterns to SQL COUNT(*) queries"
    )
    parser.add_argument("-p", "--pattern", help="Single pattern file")
    parser.add_argument("-d", "--dir", help="Directory of pattern files (recursive)")
    parser.add_argument("-o", "--output", help="Output file or directory")
    parser.add_argument(
        "--json", action="store_true",
        help="Also output structured JSON (for SafeBound etc.)"
    )
    args = parser.parse_args()

    if args.pattern:
        convert_file(args.pattern, args.output, args.json)
    elif args.dir:
        if not args.output:
            print("Error: -o required with -d", file=sys.stderr)
            sys.exit(1)
        input_dir = Path(args.dir)
        output_dir = Path(args.output)
        output_dir.mkdir(parents=True, exist_ok=True)

        for p in sorted(input_dir.rglob("*.json")):
            rel = p.relative_to(input_dir)
            out = output_dir / rel.with_suffix(".sql")
            out.parent.mkdir(parents=True, exist_ok=True)
            try:
                convert_file(p, out, args.json)
            except (ValueError, KeyError) as e:
                print(f"  SKIP {p}: {e}", file=sys.stderr)
    else:
        print("Error: specify either -p or -d", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
