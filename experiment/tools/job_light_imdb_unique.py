#!/usr/bin/env python3
from __future__ import annotations

import argparse
import csv
import json
import re
import subprocess
import time
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DATASET = ROOT / "experiment/datasets/imdb/imdb"
SRC_GCARD = ROOT / "experiment/patterns/gcard/job_light"
OUT_GCARD = ROOT / "experiment/patterns/gcard/job_light_imdb_unique"
OUT_SQL = ROOT / "experiment/patterns/sql/job_light_imdb_unique"
TRUECARD = ROOT / "experiment/patterns/truecard/job_light.csv"
DUCKDB = ROOT / "experiment/tools/duckdb"
DUCKDB_PATH = ROOT / "experiment/run_tmp/job_light_imdb_unique/job_light.duckdb"

VERTEX_LABEL = {
    "title": "title",
    "movie_info": "infoVertex",
    "movie_info_idx": "infoIdxVertex",
    "cast_info": "castInfoVertex",
    "movie_companies": "companyName",
    "movie_keyword": "keyword",
}
EDGE_MAP = {
    "title_id_movie_companies_movie_id": ("title_movieCompanies_companyName", "title", "movie_companies"),
    "title_id_movie_keyword_movie_id": ("title_keywordEdge_keyword", "title", "movie_keyword"),
    "title_id_movie_info_movie_id": ("title_infoEdge_infoVertex", "title", "movie_info"),
    "title_id_movie_info_idx_movie_id": ("title_infoEdge_infoIdxVertex", "title", "movie_info_idx"),
    "title_id_cast_info_movie_id": ("castInfoVertex_castInfoEdge_title", "cast_info", "title"),
}
EDGE_PREDICATE_PROPS = {
    ("movie_companies", "company_id"),
    ("movie_companies", "company_type_id"),
    ("movie_keyword", "keyword_id"),
}
OP_SQL = {"eq": "=", "ne": "!=", "gt": ">", "ge": ">=", "lt": "<", "le": "<="}


def unwrap(value: Any) -> Any:
    if isinstance(value, dict):
        return next(iter(value.values()))
    return value


def sql_literal(value: Any) -> str:
    value = unwrap(value)
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        return "TRUE" if value else "FALSE"
    if isinstance(value, (int, float)):
        return str(value)
    return "'" + str(value).replace("'", "''") + "'"


def qident(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def query_num(path: Path) -> int:
    m = re.search(r"(\d+)", path.stem)
    return int(m.group(1)) if m else 0


def convert_pattern(pattern: dict[str, Any]) -> dict[str, Any]:
    orig_vertices = {v["id"]: v["label"] for v in pattern["vertices"]}
    vertices = [
        {"id": v["id"], "label": VERTEX_LABEL[v["label"]]}
        for v in pattern["vertices"]
    ]

    edge_by_orig_vertex: dict[int, int] = {}
    edges = []
    for edge in pattern["edges"]:
        label, src_label, dst_label = EDGE_MAP[edge["label"]]
        src = edge["src"] if orig_vertices[edge["src"]] == src_label else edge["dst"]
        dst = edge["src"] if orig_vertices[edge["src"]] == dst_label else edge["dst"]
        edges.append({"id": edge["id"], "label": label, "src": src, "dst": dst})
        for vid in (edge["src"], edge["dst"]):
            if orig_vertices[vid] != "title":
                edge_by_orig_vertex[vid] = edge["id"]

    predicates = []
    for pred in pattern.get("predicates", []):
        if pred.get("target") != "vertex":
            predicates.append(pred)
            continue
        orig_label = orig_vertices[pred["id"]]
        prop = pred["property"]
        new_pred = dict(pred)
        if (orig_label, prop) in EDGE_PREDICATE_PROPS:
            new_pred["target"] = "edge"
            new_pred["id"] = edge_by_orig_vertex[pred["id"]]
        predicates.append(new_pred)

    return {"vertices": vertices, "edges": edges, "predicates": predicates}


def pattern_to_sql(pattern: dict[str, Any]) -> str:
    vertex_labels = {v["id"]: v["label"] for v in pattern["vertices"]}
    required_vertices = {
        pred["id"]
        for pred in pattern.get("predicates", [])
        if pred.get("target") == "vertex"
    }
    # Title is the shared join key in JOB-light; non-predicate leaf vertices are
    # referentially implied by the edge tables after unique_vid conversion.
    required_vertices.update(
        v["id"] for v in pattern["vertices"] if v["label"] == "title"
    )

    from_terms = []
    where = []
    for vid in sorted(required_vertices):
        from_terms.append(f"{qident(vertex_labels[vid])} AS v{vid}")
    for e in pattern["edges"]:
        alias = f"e{e['id']}"
        from_terms.append(f"{qident(e['label'])} AS {alias}")
        if e["src"] in required_vertices:
            where.append(f"{alias}.src = v{e['src']}.id")
        if e["dst"] in required_vertices:
            where.append(f"{alias}.dst = v{e['dst']}.id")
    for pred in pattern.get("predicates", []):
        alias = f"v{pred['id']}" if pred["target"] == "vertex" else f"e{pred['id']}"
        where.append(f"{alias}.{qident(pred['property'])} {OP_SQL[pred['op']]} {sql_literal(pred['value'])}")
    return "SELECT COUNT(*) FROM " + ", ".join(from_terms) + " WHERE " + " AND ".join(where) + ";"


def convert_all(src_dir: Path, out_gcard: Path, out_sql: Path) -> list[dict[str, Any]]:
    out_gcard.mkdir(parents=True, exist_ok=True)
    out_sql.mkdir(parents=True, exist_ok=True)
    rows = []
    for old in out_gcard.glob("*.json"):
        old.unlink()
    for old in out_sql.glob("*.sql"):
        old.unlink()
    for path in sorted(src_dir.glob("*.json"), key=query_num):
        q = path.stem
        converted = convert_pattern(json.loads(path.read_text(encoding="utf-8")))
        gpath = out_gcard / f"{q}.json"
        spath = out_sql / f"{q}.sql"
        gpath.write_text(json.dumps(converted, indent=2) + "\n", encoding="utf-8")
        spath.write_text(pattern_to_sql(converted) + "\n", encoding="utf-8")
        rows.append({
            "query": q,
            "gcard_path": str(gpath),
            "sql_path": str(spath),
            "num_tables": len(converted["vertices"]) + len(converted["edges"]),
            "num_edges": len(converted["edges"]),
            "num_predicates": len(converted.get("predicates", [])),
        })
    return rows


def run_duckdb(db_path: Path, sql: str) -> str:
    proc = subprocess.run(
        [str(DUCKDB), str(db_path), "-csv", "-noheader"],
        input=sql,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )
    return proc.stdout.strip()


def ensure_duckdb(db_path: Path, dataset_dir: Path, force: bool = False) -> None:
    db_path.parent.mkdir(parents=True, exist_ok=True)
    if force and db_path.exists():
        db_path.unlink()
    if db_path.exists():
        return
    tables = [
        "title",
        "infoVertex",
        "infoIdxVertex",
        "castInfoVertex",
        "companyName",
        "keyword",
        "title_infoEdge_infoVertex",
        "title_infoEdge_infoIdxVertex",
        "castInfoVertex_castInfoEdge_title",
        "title_movieCompanies_companyName",
        "title_keywordEdge_keyword",
    ]
    stmts = []
    for table in tables:
        path = dataset_dir / f"{table}.csv"
        stmts.append(
            f"CREATE TABLE {qident(table)} AS SELECT * FROM read_csv_auto('{path.as_posix()}', header=true);"
        )
    run_duckdb(db_path, "\n".join(stmts))


def compute_truecards(rows: list[dict[str, Any]], db_path: Path, output: Path, force_db: bool = False) -> None:
    ensure_duckdb(db_path, DATASET, force_db)
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("w", newline="", encoding="utf-8") as f:
        fieldnames = ["query", "true_cardinality", "gcard_path", "sql_path", "num_tables", "num_edges", "num_predicates"]
        writer = csv.DictWriter(f, fieldnames=fieldnames)
        writer.writeheader()
        for row in rows:
            sql = Path(row["sql_path"]).read_text(encoding="utf-8")
            start = time.perf_counter()
            value = run_duckdb(db_path, sql)
            elapsed = time.perf_counter() - start
            true_card = float(value.splitlines()[-1]) if value else 0.0
            out = dict(row)
            out["true_cardinality"] = true_card
            writer.writerow(out)
            print(f"{row['query']}: true={true_card:g} duckdb_s={elapsed:.3f}", flush=True)


def main() -> None:
    ap = argparse.ArgumentParser(description="Convert JOB-light patterns to processed IMDb graph patterns and compute truecard.")
    ap.add_argument("--src-gcard", type=Path, default=SRC_GCARD)
    ap.add_argument("--out-gcard", type=Path, default=OUT_GCARD)
    ap.add_argument("--out-sql", type=Path, default=OUT_SQL)
    ap.add_argument("--truecard", type=Path, default=TRUECARD)
    ap.add_argument("--db", type=Path, default=DUCKDB_PATH)
    ap.add_argument("--force-db", action="store_true")
    ap.add_argument("--skip-truecard", action="store_true")
    args = ap.parse_args()

    rows = convert_all(args.src_gcard, args.out_gcard, args.out_sql)
    print(f"converted {len(rows)} patterns to {args.out_gcard} and {args.out_sql}")
    if not args.skip_truecard:
        compute_truecards(rows, args.db, args.truecard, args.force_db)
        print(f"truecard={args.truecard}")


if __name__ == "__main__":
    main()
