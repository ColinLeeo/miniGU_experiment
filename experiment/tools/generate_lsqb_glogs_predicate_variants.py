#!/usr/bin/env python3
"""
Generate predicate variants for the 16 LSQB/Glogs GCard patterns.

For each base pattern under experiment/patterns/gcard/{lsqb,glogs}, this script
generates 10 predicate-bearing variants, writes both GCard JSON and SQL, and
verifies each generated query has non-zero true cardinality in DuckDB.
"""

from __future__ import annotations

import argparse
import csv
import json
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any


OP_MAP = {
    "eq": "=",
    "ne": "!=",
    "gt": ">",
    "ge": ">=",
    "lt": "<",
    "le": "<=",
}


VERTEX_PROPS: dict[str, list[tuple[str, str, list[str]]]] = {
    "person": [
        ("gender", "String", ["eq"]),
        ("browserused", "String", ["eq"]),
        ("birthday", "Int64", ["ge", "le"]),
        ("creationdate", "Int64", ["ge", "le"]),
    ],
    "comment": [
        ("length", "Int64", ["ge", "le"]),
        ("browserused", "String", ["eq"]),
        ("creationdate", "Int64", ["ge", "le"]),
        ("explicitlydeleted", "Boolean", ["eq"]),
    ],
    "post": [
        ("length", "Int64", ["ge", "le"]),
        ("browserused", "String", ["eq"]),
        ("language", "String", ["eq"]),
        ("creationdate", "Int64", ["ge", "le"]),
        ("explicitlydeleted", "Boolean", ["eq"]),
    ],
    "forum": [
        ("creationdate", "Int64", ["ge", "le"]),
        ("explicitlydeleted", "Boolean", ["eq"]),
    ],
    "tag": [("name", "String", ["eq"])],
    "tagclass": [("name", "String", ["eq"])],
    "city": [("name", "String", ["eq"]), ("type", "String", ["eq"])],
    "country": [("name", "String", ["eq"]), ("type", "String", ["eq"])],
    "continent": [("name", "String", ["eq"]), ("type", "String", ["eq"])],
    "company": [("name", "String", ["eq"]), ("type", "String", ["eq"])],
    "university": [("name", "String", ["eq"]), ("type", "String", ["eq"])],
}

EDGE_PROPS: list[tuple[str, str, list[str]]] = [
    ("creationdate", "Int64", ["ge", "le"]),
    ("deletiondate", "Int64", ["ge", "le"]),
    ("explicitlydeleted", "Boolean", ["eq"]),
    ("classyear", "Int64", ["ge", "le"]),
    ("workfrom", "Int64", ["ge", "le"]),
]


@dataclass(frozen=True)
class PredicateSpec:
    target: str
    id: int
    label: str
    prop: str
    value_type: str
    op: str
    value: Any

    @property
    def key(self) -> tuple[str, int, str]:
        return (self.target, self.id, self.prop)


def sql_literal(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value)
    text = str(value)
    return "'" + text.replace("'", "''") + "'"


def scalar_value(value: Any, value_type: str) -> dict[str, Any]:
    if value_type == "Boolean":
        if isinstance(value, str):
            value = value.lower() == "true"
        return {"Boolean": bool(value)}
    if value_type == "Int64":
        return {"Int64": int(value)}
    return {"String": str(value)}


def format_predicate_value(value: dict[str, Any]) -> str:
    if "String" in value:
        return sql_literal(value["String"])
    if "Boolean" in value:
        return "true" if value["Boolean"] else "false"
    return str(next(iter(value.values())))


def pattern_to_sql(query: dict[str, Any]) -> str:
    vertices = {v["id"]: v["label"] for v in query["vertices"]}
    edges = query["edges"]
    predicates = query.get("predicates", [])
    vertex_predicates: dict[int, list[dict[str, Any]]] = {}
    edge_predicates: dict[int, list[dict[str, Any]]] = {}
    for pred in predicates:
        if pred["target"] == "vertex":
            vertex_predicates.setdefault(pred["id"], []).append(pred)
        else:
            edge_predicates.setdefault(pred["id"], []).append(pred)

    def pred_sql(alias: str, pred: dict[str, Any]) -> str:
        op = OP_MAP[pred["op"]]
        value = format_predicate_value(pred["value"])
        return f"{alias}.{pred['property']} {op} {value}"

    def filtered_table(table_name: str, alias: str, preds: list[dict[str, Any]]) -> str:
        if not preds:
            return f"{table_name} {alias}"
        where = " AND ".join(pred_sql(alias, pred) for pred in preds)
        return f"(SELECT * FROM {table_name} {alias} WHERE {where}) {alias}"

    vertex_expr: dict[int, str] = {}
    from_parts: list[str] = []

    for i, edge in enumerate(edges):
        alias = f"e{edge['id']}"
        table_name = edge["label"]
        table_expr = filtered_table(table_name, alias, edge_predicates.get(edge["id"], []))
        if i == 0:
            from_parts.append(table_expr)
            vertex_expr[edge["src"]] = f"{alias}.src"
            vertex_expr[edge["dst"]] = f"{alias}.dst"
        else:
            on_conds = []
            for side in ("src", "dst"):
                vid = edge[side]
                expr = f"{alias}.{side}"
                if vid in vertex_expr:
                    on_conds.append(f"{expr} = {vertex_expr[vid]}")
                else:
                    vertex_expr[vid] = expr
            if on_conds:
                from_parts.append(f"JOIN {table_expr} ON {' AND '.join(on_conds)}")
            else:
                from_parts.append(f"CROSS JOIN {table_expr}")

    where_parts: list[str] = []
    joined_vertices: dict[int, str] = {}
    for vid, preds in vertex_predicates.items():
        alias = f"v{vid}"
        from_parts.append(f"JOIN {filtered_table(vertices[vid], alias, preds)} ON {alias}.id = {vertex_expr[vid]}")
        joined_vertices[vid] = alias

    sql = "SELECT COUNT(*)\nFROM " + "\n".join(from_parts)
    if where_parts:
        sql += "\nWHERE " + " AND ".join(where_parts)
    return sql


def discover_specs(
    query: dict[str, Any],
    table_columns: dict[str, set[str]],
    stats: dict[str, list[Any]],
) -> list[PredicateSpec]:
    specs: list[PredicateSpec] = []
    for vertex in query["vertices"]:
        label = vertex["label"]
        for prop, value_type, ops in VERTEX_PROPS.get(label, []):
            if prop not in table_columns.get(label, set()):
                continue
            values = stats.get(f"{label}.{prop}", [])
            if not values:
                continue
            for op in ops:
                for value in values:
                    specs.append(
                        PredicateSpec(
                            "vertex",
                            int(vertex["id"]),
                            label,
                            prop,
                            value_type,
                            op,
                            value,
                        )
                    )
    for edge in query["edges"]:
        label = edge["label"]
        cols = table_columns.get(label, set())
        for prop, value_type, ops in EDGE_PROPS:
            if prop not in cols:
                continue
            values = stats.get(f"{label}.{prop}", [])
            if not values:
                continue
            for op in ops:
                for value in values:
                    specs.append(
                        PredicateSpec(
                            "edge",
                            int(edge["id"]),
                            label,
                            prop,
                            value_type,
                            op,
                            value,
                        )
                    )
    return specs


def run_count(duckdb: Path, db: Path, sql: str) -> int:
    proc = subprocess.run(
        [str(duckdb), str(db), "-readonly", "-csv", "-noheader"],
        input=sql,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        timeout=180,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip())
    out = proc.stdout.strip().splitlines()
    return int(out[-1]) if out and out[-1] else 0


def read_table_columns(data_dir: Path) -> dict[str, set[str]]:
    columns = {}
    for csv_path in data_dir.glob("*.csv"):
        with csv_path.open("r", encoding="utf-8", newline="") as f:
            header = f.readline().strip()
        columns[csv_path.stem] = set(header.split(","))
    return columns


def load_predicate_stats(data_dir: Path, table_columns: dict[str, set[str]]) -> dict[str, list[Any]]:
    del data_dir
    common_dates = [1262304000000, 1285917807738, 1300000000000, 1325376000000]
    end_dates = [1356998400000, 1577664000000]
    stats: dict[str, list[Any]] = {
        "person.gender": ["male", "female"],
        "person.browserused": ["Chrome", "Firefox", "Internet Explorer"],
        "person.birthday": [315532800000, 447811200000, 520905600000, 631152000000],
        "person.creationdate": common_dates,
        "comment.length": [2, 20, 50, 88, 120],
        "comment.browserused": ["Chrome", "Firefox", "Internet Explorer"],
        "comment.creationdate": common_dates,
        "comment.explicitlydeleted": [False],
        "post.length": [0, 1, 10, 50, 108],
        "post.browserused": ["Chrome", "Firefox", "Internet Explorer"],
        "post.language": ["en", "fa", "de"],
        "post.creationdate": common_dates,
        "post.explicitlydeleted": [False],
        "forum.creationdate": common_dates,
        "forum.explicitlydeleted": [False],
        "tag.name": ["Rumi", "Hamid_Karzai", "Wolfgang_Amadeus_Mozart"],
        "tagclass.name": ["Thing", "Person", "Place"],
        "city.type": ["place"],
        "country.type": ["place"],
        "continent.type": ["place"],
        "company.type": ["organisation"],
        "university.type": ["organisation"],
    }
    for table, cols in table_columns.items():
        if "creationdate" in cols:
            stats.setdefault(f"{table}.creationdate", common_dates)
        if "deletiondate" in cols:
            stats.setdefault(f"{table}.deletiondate", end_dates)
        if "explicitlydeleted" in cols:
            stats.setdefault(f"{table}.explicitlydeleted", [False])
        if "classyear" in cols:
            stats.setdefault(f"{table}.classyear", [2000, 2005, 2010])
        if "workfrom" in cols:
            stats.setdefault(f"{table}.workfrom", [2000, 2005, 2010])
    return stats


def choose_specs(specs: list[PredicateSpec], variant_idx: int, pred_count: int) -> list[PredicateSpec]:
    if len(specs) < pred_count:
        return []
    target_preference = [
        "vertex",
        "edge",
        "vertex",
        "edge",
    ] if variant_idx % 2 == 0 else ["edge", "vertex", "vertex", "edge"]
    prop_classes = ["String", "Int64", "Boolean", "Int64", "String"]
    start = (variant_idx * 7) % len(specs)
    ordered = specs[start:] + specs[:start]

    chosen: list[PredicateSpec] = []
    used_keys: set[tuple[str, int, str]] = set()
    used_targets: set[str] = set()

    for desired_target, desired_type in zip(target_preference * 2, prop_classes * 2):
        for spec in ordered:
            if spec.key in used_keys:
                continue
            if spec.target != desired_target:
                continue
            if spec.value_type != desired_type:
                continue
            chosen.append(spec)
            used_keys.add(spec.key)
            used_targets.add(spec.target)
            break
        if len(chosen) == pred_count:
            return chosen

    for spec in ordered:
        if spec.key in used_keys:
            continue
        if pred_count >= 3 and len(used_targets) == 1 and len(chosen) == pred_count - 1:
            if spec.target in used_targets:
                continue
        chosen.append(spec)
        used_keys.add(spec.key)
        used_targets.add(spec.target)
        if len(chosen) == pred_count:
            return chosen
    return []


def build_predicates(specs: list[PredicateSpec]) -> list[dict[str, Any]]:
    predicates = []
    for pid, spec in enumerate(specs, start=1):
        predicates.append(
            {
                "predicate_id": pid,
                "target": spec.target,
                "id": spec.id,
                "property": spec.prop,
                "op": spec.op,
                "value": scalar_value(spec.value, spec.value_type),
            }
        )
    return predicates


def generate_for_pattern(
    duckdb: Path,
    db: Path,
    table_columns: dict[str, set[str]],
    stats: dict[str, list[Any]],
    pattern_path: Path,
    out_gcard_dir: Path,
    out_sql_dir: Path,
    variants: int,
) -> list[dict[str, Any]]:
    with pattern_path.open("r", encoding="utf-8") as f:
        base_query = json.load(f)

    specs = discover_specs(base_query, table_columns, stats)
    if len(specs) < 2:
        raise RuntimeError(f"not enough predicate-capable fields: {pattern_path}")

    produced = []
    seen_predicates: set[str] = set()
    attempts = 0
    while len(produced) < variants and attempts < variants * 300:
        attempts += 1
        pred_count = 2 + ((len(produced) + attempts) % 3)
        selected = choose_specs(specs, attempts, pred_count)
        if len(selected) != pred_count:
            continue
        query = json.loads(json.dumps(base_query))
        query["predicates"] = build_predicates(selected)
        pred_key = json.dumps(query["predicates"], sort_keys=True)
        if pred_key in seen_predicates:
            continue
        sql = pattern_to_sql(query)
        card = run_count(duckdb, db, sql)
        if card <= 0:
            continue
        seen_predicates.add(pred_key)
        variant_no = len(produced) + 1
        stem = pattern_path.stem
        out_name = f"{stem}_{variant_no:02d}"
        gcard_path = out_gcard_dir / f"{out_name}.json"
        sql_path = out_sql_dir / f"{out_name}.sql"
        gcard_path.parent.mkdir(parents=True, exist_ok=True)
        sql_path.parent.mkdir(parents=True, exist_ok=True)
        with gcard_path.open("w", encoding="utf-8") as f:
            json.dump(query, f, indent=2)
            f.write("\n")
        with sql_path.open("w", encoding="utf-8") as f:
            f.write(sql + "\n")
        produced.append(
            {
                "base": pattern_path.stem,
                "variant": out_name,
                "gcard_path": str(gcard_path),
                "sql_path": str(sql_path),
                "predicate_count": len(query["predicates"]),
                "true_cardinality": card,
                "predicates": json.dumps(query["predicates"], sort_keys=True),
            }
        )

    if len(produced) < variants:
        raise RuntimeError(f"only generated {len(produced)}/{variants}: {pattern_path}")
    return produced


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--duckdb", type=Path, default=Path("experiment/tools/duckdb"))
    parser.add_argument("--db", type=Path, default=Path("experiment/graphs/ldbc/duckdb/ldbc_sf1.duckdb"))
    parser.add_argument("--data-dir", type=Path, default=Path("experiment/datasets/ldbc/sf1"))
    parser.add_argument("--variants", type=int, default=10)
    parser.add_argument("--out-name", default="lsqb_glogs_with_pred_10")
    args = parser.parse_args()

    repo = args.repo.resolve()
    duckdb = (repo / args.duckdb).resolve() if not args.duckdb.is_absolute() else args.duckdb
    db = (repo / args.db).resolve() if not args.db.is_absolute() else args.db
    data_dir = (repo / args.data_dir).resolve() if not args.data_dir.is_absolute() else args.data_dir
    table_columns = read_table_columns(data_dir)
    stats = load_predicate_stats(data_dir, table_columns)

    all_rows: list[dict[str, Any]] = []
    for group in ("lsqb", "glogs"):
        pattern_dir = repo / "experiment" / "patterns" / "gcard" / group
        out_gcard_dir = repo / "experiment" / "patterns" / "gcard" / args.out_name / group
        out_sql_dir = repo / "experiment" / "patterns" / "sql" / args.out_name / group
        for pattern_path in sorted(pattern_dir.glob("*.json")):
            rows = generate_for_pattern(
                duckdb,
                db,
                table_columns,
                stats,
                pattern_path,
                out_gcard_dir,
                out_sql_dir,
                args.variants,
            )
            for row in rows:
                row["group"] = group
            all_rows.extend(rows)
            print(f"{group}/{pattern_path.stem}: {len(rows)} variants")

    manifest_path = repo / "experiment" / "patterns" / "sql" / args.out_name / "truth.csv"
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    with manifest_path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=[
                "group",
                "base",
                "variant",
                "predicate_count",
                "true_cardinality",
                "gcard_path",
                "sql_path",
                "predicates",
            ],
        )
        writer.writeheader()
        writer.writerows(all_rows)
    print(f"wrote {len(all_rows)} variants")
    print(f"truth manifest: {manifest_path}")


if __name__ == "__main__":
    main()
