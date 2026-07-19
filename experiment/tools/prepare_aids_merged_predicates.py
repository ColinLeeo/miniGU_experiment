#!/usr/bin/env python3
import argparse
import csv
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXP = ROOT / "experiment"
SRC_DATA = EXP / "datasets/aids_merged"
SRC_PATTERNS = EXP / "patterns/gcard/aids_merged"
DATASET = EXP / "datasets/aids_merged_predicates"
GCARD_PATTERNS = EXP / "patterns/gcard/aids_merged_predicates"
SQL_PATTERNS = EXP / "patterns/sql/aids_merged_predicates"
META = EXP / "result/aids_merged_predicates/workload_manifest.csv"

VERTEX_PROPS = [
    {"name": "v_mod10", "logical_type": "Int64", "nullable": False},
    {"name": "v_deg_bin", "logical_type": "Int64", "nullable": False},
    {"name": "v_parity", "logical_type": "Int64", "nullable": False},
]
EDGE_PROPS = [
    {"name": "e_mod10", "logical_type": "Int64", "nullable": False},
    {"name": "e_label", "logical_type": "Int64", "nullable": False},
    {"name": "e_src_mod10", "logical_type": "Int64", "nullable": False},
]


def read_edges(src_dir):
    edges = {}
    max_vid = -1
    degrees = {}
    for label in ("0", "1", "2", "3"):
        rows = []
        with (src_dir / f"{label}.csv").open(newline="", encoding="utf-8") as f:
            for row in csv.DictReader(f):
                s = int(row["src"])
                d = int(row["dst"])
                rows.append((s, d))
                max_vid = max(max_vid, s, d)
                degrees[s] = degrees.get(s, 0) + 1
                degrees[d] = degrees.get(d, 0) + 1
        edges[label] = rows
    return edges, max_vid, degrees


def deg_bin(deg):
    if deg <= 1:
        return 0
    if deg <= 3:
        return 1
    if deg <= 6:
        return 2
    if deg <= 10:
        return 3
    return 4


def build_dataset(force=False):
    DATASET.mkdir(parents=True, exist_ok=True)
    edges, max_vid, degrees = read_edges(SRC_DATA)

    vertex_path = DATASET / "vertex.csv"
    if force or not vertex_path.exists():
        with vertex_path.open("w", newline="", encoding="utf-8") as f:
            w = csv.writer(f)
            w.writerow(["id", "v_mod10", "v_deg_bin", "v_parity"])
            for vid in range(max_vid + 1):
                w.writerow([vid, vid % 10, deg_bin(degrees.get(vid, 0)), vid % 2])

    for label, rows in edges.items():
        out = DATASET / f"{label}.csv"
        if force or not out.exists():
            lab = int(label)
            with out.open("w", newline="", encoding="utf-8") as f:
                w = csv.writer(f)
                w.writerow(["src", "dst", "e_mod10", "e_label", "e_src_mod10"])
                for s, d in rows:
                    e_mod10 = (s * 31 + d * 17 + lab * 13) % 10
                    w.writerow([s, d, e_mod10, lab, s % 10])

    manifest = {
        "vertices": [
            {
                "label": "vertex",
                "file": {"path": "vertex.csv", "format": "csv"},
                "properties": VERTEX_PROPS,
            }
        ],
        "edges": [
            {
                "label": label,
                "src_label": "vertex",
                "dst_label": "vertex",
                "file": {"path": f"{label}.csv", "format": "csv"},
                "properties": EDGE_PROPS,
            }
            for label in ("0", "1", "2", "3")
        ],
    }
    (DATASET / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


def pattern_key(path):
    return (path.parent.name, int(path.stem))


def topology_group(name):
    return name.split("_", 1)[0]


def choose_vertex(pattern):
    degree = {v["id"]: 0 for v in pattern["vertices"]}
    for edge in pattern["edges"]:
        degree[edge["src"]] = degree.get(edge["src"], 0) + 1
        degree[edge["dst"]] = degree.get(edge["dst"], 0) + 1
    return sorted(degree.items(), key=lambda kv: (-kv[1], kv[0]))[0][0]


def choose_edge(pattern):
    return sorted(pattern["edges"], key=lambda e: e["id"])[0]


def add_predicates(pattern, rel):
    pattern = json.loads(json.dumps(pattern))
    vid = choose_vertex(pattern)
    edge = choose_edge(pattern)
    pattern["predicates"] = [
        {
            "predicate_id": 1,
            "target": "vertex",
            "id": vid,
            "property": "v_mod10",
            "op": "le",
            "value": {"Int64": 4},
        },
        {
            "predicate_id": 2,
            "target": "edge",
            "id": edge["id"],
            "property": "e_mod10",
            "op": "le",
            "value": {"Int64": 6},
        },
    ]
    return pattern, vid, edge["id"]


def sql_literal(value):
    if isinstance(value, dict):
        value = next(iter(value.values()))
    if isinstance(value, str):
        return "'" + value.replace("'", "''") + "'"
    if isinstance(value, bool):
        return "true" if value else "false"
    return str(value)


def sql_op(op):
    return {"eq": "=", "ne": "!=", "gt": ">", "ge": ">=", "lt": "<", "le": "<="}[op]


def pattern_to_sql(pattern, quote_numeric=False):
    vertices = sorted(v["id"] for v in pattern["vertices"])
    vertex_alias = {vid: f"v{vid}" for vid in vertices}
    from_terms = [f"vertex {vertex_alias[vid]}" for vid in vertices]
    where = []
    for edge in sorted(pattern["edges"], key=lambda e: e["id"]):
        alias = f"e{edge['id']}"
        label = str(edge["label"])
        table = f'"{label}"' if quote_numeric else label
        from_terms.append(f"{table} {alias}")
        where.append(f"{alias}.src = {vertex_alias[edge['src']]}.id")
        where.append(f"{alias}.dst = {vertex_alias[edge['dst']]}.id")
    for pred in pattern.get("predicates", []):
        alias = f"v{pred['id']}" if pred["target"] == "vertex" else f"e{pred['id']}"
        where.append(f"{alias}.{pred['property']} {sql_op(pred['op'])} {sql_literal(pred['value'])}")
    return "select count(*) from " + ", ".join(from_terms) + " where " + " and ".join(where)


def build_patterns(force=False):
    GCARD_PATTERNS.mkdir(parents=True, exist_ok=True)
    SQL_PATTERNS.mkdir(parents=True, exist_ok=True)
    META.parent.mkdir(parents=True, exist_ok=True)
    rows = []
    files = sorted(SRC_PATTERNS.glob("*/*.json"), key=pattern_key)
    for src in files:
        rel = f"{src.parent.name}/{src.stem}"
        pattern = json.loads(src.read_text(encoding="utf-8"))
        pred_pattern, vertex_id, edge_id = add_predicates(pattern, rel)
        out_json = GCARD_PATTERNS / src.parent.name / f"{src.stem}.json"
        out_sql = SQL_PATTERNS / src.parent.name / f"{src.stem}.sql"
        out_json.parent.mkdir(parents=True, exist_ok=True)
        out_sql.parent.mkdir(parents=True, exist_ok=True)
        if force or not out_json.exists():
            out_json.write_text(json.dumps(pred_pattern, indent=2) + "\n", encoding="utf-8")
        if force or not out_sql.exists():
            out_sql.write_text(pattern_to_sql(pred_pattern, quote_numeric=False) + "\n", encoding="utf-8")
        rows.append({
            "topology": src.parent.name,
            "query": src.stem,
            "pattern": rel,
            "topology_group": topology_group(src.parent.name),
            "gcard_path": str(out_json),
            "sql_path": str(out_sql),
            "predicate_count": 2,
            "vertex_predicate": f"v{vertex_id}.v_mod10 <= 4",
            "edge_predicate": f"e{edge_id}.e_mod10 <= 6",
        })
    with META.open("w", newline="", encoding="utf-8") as f:
        fieldnames = ["topology", "query", "pattern", "topology_group", "gcard_path", "sql_path", "predicate_count", "vertex_predicate", "edge_predicate"]
        w = csv.DictWriter(f, fieldnames=fieldnames)
        w.writeheader()
        w.writerows(rows)
    return len(rows)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--force", action="store_true")
    args = ap.parse_args()
    build_dataset(force=args.force)
    n = build_patterns(force=args.force)
    print(f"dataset={DATASET}")
    print(f"gcard_patterns={GCARD_PATTERNS}")
    print(f"sql_patterns={SQL_PATTERNS}")
    print(f"metadata={META}")
    print(f"patterns={n}")


if __name__ == "__main__":
    main()
