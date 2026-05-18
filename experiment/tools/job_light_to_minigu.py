#!/usr/bin/env python3
from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path


TABLES = {
    "title": {
        "properties": ["production_year", "kind_id"],
    },
    "movie_companies": {
        "properties": ["company_id", "company_type_id"],
        "edge": ("title_id_movie_companies_movie_id", "movie_id"),
    },
    "movie_info_idx": {
        "properties": ["info_type_id"],
        "edge": ("title_id_movie_info_idx_movie_id", "movie_id"),
    },
    "movie_info": {
        "properties": ["info_type_id"],
        "edge": ("title_id_movie_info_movie_id", "movie_id"),
    },
    "movie_keyword": {
        "properties": ["keyword_id"],
        "edge": ("title_id_movie_keyword_movie_id", "movie_id"),
    },
    "cast_info": {
        "properties": ["role_id"],
        "edge": ("title_id_cast_info_movie_id", "movie_id"),
    },
}


def lower_row(row: dict[str, str]) -> dict[str, str]:
    return {k.strip().lower(): v for k, v in row.items()}


def require_columns(table: str, row: dict[str, str], columns: list[str]) -> None:
    missing = [c for c in columns if c not in row]
    if missing:
        raise KeyError(f"{table}.csv missing columns {missing}; available={sorted(row)}")


def write_table(source_dir: Path, output_dir: Path, table: str, spec: dict) -> None:
    input_path = source_dir / f"{table}.csv"
    if not input_path.exists():
        raise FileNotFoundError(f"missing source csv: {input_path}")

    props = spec["properties"]
    vertex_fields = ["id", *props]
    vertex_path = output_dir / f"{table}.csv"

    edge_info = spec.get("edge")
    edge_file = None
    edge_writer = None
    movie_id_col = None
    if edge_info:
        edge_label, movie_id_col = edge_info
        edge_file = (output_dir / f"{edge_label}.csv").open("w", newline="", encoding="utf-8")
        edge_writer = csv.DictWriter(edge_file, fieldnames=["src", "dst"])
        edge_writer.writeheader()

    try:
        with input_path.open(newline="", encoding="utf-8", errors="ignore") as src, vertex_path.open(
            "w", newline="", encoding="utf-8"
        ) as dst:
            reader = csv.DictReader(src)
            writer = csv.DictWriter(dst, fieldnames=vertex_fields)
            writer.writeheader()

            checked = False
            for raw in reader:
                row = lower_row(raw)
                if not checked:
                    required = ["id", *props]
                    if movie_id_col:
                        required.append(movie_id_col)
                    require_columns(table, row, required)
                    checked = True

                writer.writerow({field: row.get(field, "") for field in vertex_fields})
                if edge_writer and movie_id_col:
                    edge_writer.writerow({"src": row[movie_id_col], "dst": row["id"]})
    finally:
        if edge_file:
            edge_file.close()


def build_manifest() -> dict:
    vertices = []
    edges = []
    for table, spec in TABLES.items():
        vertices.append(
            {
                "label": table,
                "file": {"path": f"{table}.csv", "format": "csv"},
                "properties": [
                    {"name": prop, "logical_type": "Int64", "nullable": True}
                    for prop in spec["properties"]
                ],
            }
        )
        if "edge" in spec:
            edge_label, _ = spec["edge"]
            edges.append(
                {
                    "label": edge_label,
                    "src_label": "title",
                    "dst_label": table,
                    "file": {"path": f"{edge_label}.csv", "format": "csv"},
                    "properties": [],
                }
            )
    return {"vertices": vertices, "edges": edges}


def main() -> None:
    parser = argparse.ArgumentParser(description="Prepare only JOB-light tables as a miniGU property graph.")
    parser.add_argument("--source-dir", required=True, help="Directory with JOB IMDB CSV files.")
    parser.add_argument("--output-dir", required=True, help="Output directory, e.g. experiment/datasets/job_light.")
    args = parser.parse_args()

    source_dir = Path(args.source_dir).resolve()
    output_dir = Path(args.output_dir).resolve()
    output_dir.mkdir(parents=True, exist_ok=True)

    for table, spec in TABLES.items():
        write_table(source_dir, output_dir, table, spec)

    (output_dir / "manifest.json").write_text(json.dumps(build_manifest(), indent=2) + "\n", encoding="utf-8")
    print(f"Wrote JOB-light miniGU dataset to {output_dir}")


if __name__ == "__main__":
    main()
