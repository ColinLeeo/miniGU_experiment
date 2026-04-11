#!/usr/bin/env python3
"""
Convert STATS-CEB SQL queries to miniGU GCard JSON pattern format.

Reads the FactorJoin stats_CEB_query.sql file (with ground truth cardinalities)
and generates JSON pattern files compatible with miniGU's gcard_query procedure.

Usage:
    python3 stats_sql2minigu.py \
        -i experiment/baseline/factorjoin/checkpoints/stats_CEB_query.sql \
        -o experiment/pattern/stats_ceb

Output: one JSON file per query (q001.json, q002.json, ...) + a truth.csv file
"""
import argparse
import json
import os
import re
import time

# ── FK definitions: (table, fk_col) → (referenced_table, "id") ──
# This defines which columns are foreign keys and what they reference.
FK_DEFS = {
    ("comments",    "postid"):       ("posts", "id"),
    ("comments",    "userid"):       ("users", "id"),
    ("badges",      "userid"):       ("users", "id"),
    ("tags",        "excerptpostid"):("posts", "id"),
    ("postlinks",   "postid"):       ("posts", "id"),
    ("postlinks",   "relatedpostid"):("posts", "id"),
    ("posthistory", "postid"):       ("posts", "id"),
    ("posthistory", "userid"):       ("users", "id"),
    ("votes",       "postid"):       ("posts", "id"),
    ("votes",       "userid"):       ("users", "id"),
    ("posts",       "owneruserid"):  ("users", "id"),
    ("posts",       "lasteditoruserid"): ("users", "id"),
}

# edge_label for a FK: (table, fk_col) → label
def _edge_label(table, fk_col):
    ref_table, _ = FK_DEFS[(table, fk_col)]
    return f"{table}_{fk_col}_{ref_table}"

# Build reverse lookup: (table, fk_col) → referenced table
# Also build: for each (table, fk_col), what's the edge label and direction
# Edge direction: src=FK holder, dst=PK holder

OP_MAP = {
    "=": "eq",
    "!=": "ne",
    "<>": "ne",
    ">": "gt",
    ">=": "ge",
    "<": "lt",
    "<=": "le",
}

# Standard STATS-CEB aliases
ALIAS_MAP = {
    "b": "badges",
    "c": "comments",
    "p": "posts",
    "ph": "posthistory",
    "pl": "postlinks",
    "t": "tags",
    "u": "users",
    "v": "votes",
}


def resolve_alias(alias_str, aliases):
    """Resolve 'c.UserId' → ('comments', 'userid')"""
    parts = alias_str.strip().split(".")
    if len(parts) != 2:
        return None, None
    alias, col = parts[0].strip().lower(), parts[1].strip().lower()
    table = aliases.get(alias)
    if table is None:
        return None, None
    return table, col


def parse_timestamp(val_str):
    """Parse timestamp string to epoch seconds (Int64)."""
    val_str = val_str.strip().strip("'\"")
    val_str = re.sub(r"::timestamp\s*$", "", val_str).strip().strip("'\"")
    for fmt in ("%Y-%m-%d %H:%M:%S", "%Y-%m-%d"):
        try:
            t = time.strptime(val_str, fmt)
            return int(time.mktime(t))
        except ValueError:
            continue
    return None


def parse_value(val_str):
    """Parse a SQL value into a GCard ScalarValue dict."""
    val_str = val_str.strip()
    if "::timestamp" in val_str or (val_str.startswith("'") and "-" in val_str):
        ts = parse_timestamp(val_str)
        if ts is not None:
            return {"Int64": ts}
    val_str = val_str.strip("'\"")
    try:
        return {"Int64": int(val_str)}
    except ValueError:
        pass
    try:
        return {"Float64": float(val_str)}
    except ValueError:
        pass
    return {"String": val_str}


def sql_to_pattern(sql_line):
    """Convert a single SQL line (with truth prefix) to a miniGU pattern dict.

    Handles both direct FK-PK joins (c.PostId = p.Id) and indirect joins
    (c.UserId = b.UserId) by inserting implicit intermediate vertices.

    Returns (pattern_dict, truth_cardinality).
    """
    # Parse truth||SQL format
    parts = sql_line.split("||", 1)
    if len(parts) == 2:
        truth = int(parts[0].strip())
        sql = parts[1].strip()
    else:
        truth = -1
        sql = sql_line.strip()

    # Extract FROM clause
    from_match = re.search(r"\bFROM\b\s+(.*?)\s+WHERE\b", sql, re.IGNORECASE)
    if not from_match:
        return None, truth

    from_str = from_match.group(1)
    aliases = {}
    alias_order = []
    for part in from_str.split(","):
        part = part.strip()
        m = re.match(r"(\w+)\s+as\s+(\w+)", part, re.IGNORECASE)
        if m:
            table_name = m.group(1).lower()
            alias = m.group(2).lower()
            aliases[alias] = table_name
            alias_order.append((alias, table_name))

    # Parse WHERE clause
    where_match = re.search(r"\bWHERE\b\s+(.*)", sql, re.IGNORECASE)
    if not where_match:
        return None, truth

    where_str = where_match.group(1).rstrip(";").strip()
    conditions = re.split(r"\s+AND\s+", where_str, flags=re.IGNORECASE)

    raw_joins = []
    raw_preds = []

    for cond in conditions:
        cond = cond.strip()
        if not cond:
            continue
        join_match = re.match(r"(\w+\.\w+)\s*=\s*(\w+\.\w+)$", cond)
        if join_match:
            lt, lc = resolve_alias(join_match.group(1), aliases)
            rt, rc = resolve_alias(join_match.group(2), aliases)
            if lt and rt:
                raw_joins.append((lt, lc, rt, rc))
            continue
        pred_match = re.match(r"(\w+\.\w+)\s*(>=|<=|!=|<>|=|>|<)\s*(.+)$", cond)
        if pred_match:
            table, col = resolve_alias(pred_match.group(1), aliases)
            op_sql = pred_match.group(2)
            val_str = pred_match.group(3)
            if table and op_sql in OP_MAP:
                raw_preds.append((table, col, OP_MAP[op_sql], val_str))
            continue

    # ── Build vertices ──
    # Start with explicit tables from FROM clause.
    # Each (alias, table) gets a unique vertex.
    # alias → vertex_id
    alias_vid = {}
    next_vid = 1
    vertices = []
    for alias, table_name in alias_order:
        alias_vid[alias] = next_vid
        vertices.append({"id": next_vid, "label": table_name})
        next_vid += 1

    # We also need a way to find the alias for a given table
    # (for inserting implicit vertices)
    table_aliases = {}
    for alias, table_name in alias_order:
        table_aliases.setdefault(table_name, []).append(alias)

    # ── Build edges from joins ──
    edges = []
    edge_id = 1

    # Helper: find the alias for a table in the vertex list
    def find_alias_for_table(table):
        als = table_aliases.get(table, [])
        return als[0] if als else None

    # Helper: get or create a vertex for a table, returns alias
    def ensure_vertex(table_name):
        nonlocal next_vid
        existing = find_alias_for_table(table_name)
        if existing:
            return existing
        # Create implicit vertex
        implicit_alias = f"_implicit_{table_name}_{next_vid}"
        alias_vid[implicit_alias] = next_vid
        vertices.append({"id": next_vid, "label": table_name})
        table_aliases.setdefault(table_name, []).append(implicit_alias)
        next_vid += 1
        return implicit_alias

    for lt, lc, rt, rc in raw_joins:
        # Case 1: Direct FK-PK join (e.g., c.PostId = p.Id or p.Id = c.PostId)
        l_is_fk = (lt, lc) in FK_DEFS
        r_is_fk = (rt, rc) in FK_DEFS
        l_is_pk = (lc == "id")
        r_is_pk = (rc == "id")

        if l_is_fk and r_is_pk and FK_DEFS[(lt, lc)][0] == rt:
            # Left is FK, right is PK: lt -[edge]-> rt
            src_alias = _find_alias(lt, alias_order)
            dst_alias = _find_alias(rt, alias_order)
            label = _edge_label(lt, lc)
            edges.append({"id": edge_id, "label": label,
                          "src": alias_vid[src_alias], "dst": alias_vid[dst_alias]})
            edge_id += 1

        elif r_is_fk and l_is_pk and FK_DEFS[(rt, rc)][0] == lt:
            # Right is FK, left is PK: rt -[edge]-> lt
            src_alias = _find_alias(rt, alias_order)
            dst_alias = _find_alias(lt, alias_order)
            label = _edge_label(rt, rc)
            edges.append({"id": edge_id, "label": label,
                          "src": alias_vid[src_alias], "dst": alias_vid[dst_alias]})
            edge_id += 1

        elif l_is_fk and r_is_fk:
            # Case 2: Indirect join (e.g., c.UserId = b.UserId)
            # Both are FKs pointing to the same PK table
            l_ref_table = FK_DEFS[(lt, lc)][0]
            r_ref_table = FK_DEFS[(rt, rc)][0]
            if l_ref_table == r_ref_table:
                # Insert implicit intermediate vertex if needed
                mid_alias = ensure_vertex(l_ref_table)
                mid_vid = alias_vid[mid_alias]

                src1 = _find_alias(lt, alias_order)
                src2 = _find_alias(rt, alias_order)
                label1 = _edge_label(lt, lc)
                label2 = _edge_label(rt, rc)

                # Check we don't duplicate edges
                existing = {(e["label"], e["src"], e["dst"]) for e in edges}
                if (label1, alias_vid[src1], mid_vid) not in existing:
                    edges.append({"id": edge_id, "label": label1,
                                  "src": alias_vid[src1], "dst": mid_vid})
                    edge_id += 1
                if (label2, alias_vid[src2], mid_vid) not in existing:
                    edges.append({"id": edge_id, "label": label2,
                                  "src": alias_vid[src2], "dst": mid_vid})
                    edge_id += 1
            else:
                print(f"  WARNING: indirect join across different tables: "
                      f"{lt}.{lc}({l_ref_table}) = {rt}.{rc}({r_ref_table})")

        elif l_is_fk and not r_is_pk:
            # e.g., c.UserId = ph.UserId — both FKs to same table
            # Already handled above, but check if rt.rc is also FK
            if (rt, rc) in FK_DEFS:
                l_ref = FK_DEFS[(lt, lc)][0]
                r_ref = FK_DEFS[(rt, rc)][0]
                if l_ref == r_ref:
                    mid_alias = ensure_vertex(l_ref)
                    mid_vid = alias_vid[mid_alias]
                    src1 = _find_alias(lt, alias_order)
                    src2 = _find_alias(rt, alias_order)
                    existing = {(e["label"], e["src"], e["dst"]) for e in edges}
                    l1 = _edge_label(lt, lc)
                    l2 = _edge_label(rt, rc)
                    if (l1, alias_vid[src1], mid_vid) not in existing:
                        edges.append({"id": edge_id, "label": l1,
                                      "src": alias_vid[src1], "dst": mid_vid})
                        edge_id += 1
                    if (l2, alias_vid[src2], mid_vid) not in existing:
                        edges.append({"id": edge_id, "label": l2,
                                      "src": alias_vid[src2], "dst": mid_vid})
                        edge_id += 1
            else:
                print(f"  WARNING: unhandled join: {lt}.{lc} = {rt}.{rc}")
        else:
            print(f"  WARNING: unhandled join: {lt}.{lc} = {rt}.{rc}")

    # ── Build predicates ──
    pred_list = []
    pred_id = 1
    for table, col, op, val_str in raw_preds:
        alias = _find_alias(table, alias_order)
        if alias is None:
            continue
        vid = alias_vid[alias]
        value = parse_value(val_str)
        pred_list.append({
            "predicate_id": pred_id,
            "target": "vertex",
            "id": vid,
            "property": col,
            "op": op,
            "value": value,
        })
        pred_id += 1

    return {
        "vertices": vertices,
        "edges": edges,
        "predicates": pred_list,
    }, truth


def _find_alias(table, alias_order):
    """Find first alias for a given table name."""
    for alias, t in alias_order:
        if t == table:
            return alias
    return None


def main():
    parser = argparse.ArgumentParser(description="Convert STATS-CEB SQL to miniGU JSON patterns")
    parser.add_argument("-i", "--input", required=True, help="Path to stats_CEB_query.sql")
    parser.add_argument("-o", "--output", required=True, help="Output directory for JSON patterns")
    args = parser.parse_args()

    os.makedirs(args.output, exist_ok=True)

    with open(args.input) as f:
        lines = [l.strip() for l in f if l.strip()]

    truth_file = os.path.join(args.output, "truth.csv")
    with open(truth_file, "w") as tf:
        tf.write("query,truth_cardinality,num_tables,num_edges,num_predicates\n")

        for i, line in enumerate(lines):
            qname = f"q{i+1:03d}"
            pattern, truth = sql_to_pattern(line)
            if pattern is None:
                print(f"  SKIP {qname}: parse failed")
                continue

            out_path = os.path.join(args.output, f"{qname}.json")
            with open(out_path, "w") as f:
                json.dump(pattern, f, indent=2)

            nt = len(pattern["vertices"])
            ne = len(pattern["edges"])
            np_ = len(pattern["predicates"])
            tf.write(f"{qname},{truth},{nt},{ne},{np_}\n")
            print(f"  {qname}: {nt} vertices, {ne} edges, {np_} predicates, truth={truth}")

    print(f"\nDone. {len(lines)} queries -> {args.output}")
    print(f"Truth file: {truth_file}")


if __name__ == "__main__":
    main()
