#!/usr/bin/env python3
"""
Generate SQL schema metadata from LDBC pathce schema and CSV headers.

Outputs:
  - DDL: CREATE TABLE statements for all vertex and edge tables
  - FK-PK constraints JSON (for SafeBound etc.)

Usage:
    python generate_sql_schema.py -s schema.json -d data_dir/ -o output_dir/

Example:
    python generate_sql_schema.py \
        -s experiment/schemas/ldbc/ldbc_pathce_schema.json \
        -d experiment/datasets/ldbc/sf1 \
        -o experiment/schemas/ldbc/sql/
"""
import json
import csv
import sys
import argparse
from pathlib import Path


def read_csv_header(csv_path):
    """Read the header row from a CSV file."""
    with open(csv_path) as f:
        reader = csv.reader(f, delimiter="|")
        return next(reader)


def infer_column_type(col_name, sample_values):
    """Infer SQL column type from column name and sample values."""
    if col_name in ("id", "src", "dst"):
        return "BIGINT"
    # Try to detect numeric columns
    for v in sample_values:
        if v == "":
            continue
        try:
            int(v)
            return "BIGINT"
        except ValueError:
            try:
                float(v)
                return "DOUBLE"
            except ValueError:
                return "VARCHAR"
    return "VARCHAR"


def read_csv_samples(csv_path, n=5):
    """Read header and a few sample rows from a CSV file."""
    with open(csv_path) as f:
        reader = csv.reader(f, delimiter="|")
        header = next(reader)
        samples = []
        for i, row in enumerate(reader):
            if i >= n:
                break
            samples.append(row)
    return header, samples


def find_csv(data_dir, name):
    """Find a CSV file, trying lowercase first then original case."""
    p = Path(data_dir) / f"{name.lower()}.csv"
    if p.exists():
        return p
    p = Path(data_dir) / f"{name}.csv"
    if p.exists():
        return p
    return None


def generate_ddl_and_fk(schema, data_dir):
    """Generate CREATE TABLE DDL and FK-PK constraints."""
    vertex_id_to_name = {}
    for name, label_id in schema["vertex_labels"].items():
        vertex_id_to_name[label_id] = name.lower()

    edge_id_to_name = {}
    for name, label_id in schema["edge_labels"].items():
        edge_id_to_name[label_id] = name.lower()

    ddl_statements = []
    table_columns = {}  # table_name -> [columns with types]
    fk_constraints = []

    # Vertex tables
    for name, label_id in schema["vertex_labels"].items():
        table_name = name.lower()
        csv_path = find_csv(data_dir, table_name)
        if csv_path is None:
            print(f"  WARN: no CSV for vertex {table_name}, skipping", file=sys.stderr)
            continue

        header, samples = read_csv_samples(csv_path)
        columns = []
        for i, col in enumerate(header):
            col_samples = [row[i] for row in samples if i < len(row)]
            col_type = infer_column_type(col, col_samples)
            columns.append(f"    {col} {col_type}")

        table_columns[table_name] = header
        ddl = f"CREATE TABLE {table_name} (\n"
        ddl += ",\n".join(columns)
        ddl += f",\n    PRIMARY KEY (id)"
        ddl += "\n);"
        ddl_statements.append(ddl)

    # Edge tables
    for name, label_id in schema["edge_labels"].items():
        table_name = name.lower()
        csv_path = find_csv(data_dir, table_name)
        if csv_path is None:
            print(f"  WARN: no CSV for edge {table_name}, skipping", file=sys.stderr)
            continue

        header, samples = read_csv_samples(csv_path)
        columns = []
        for i, col in enumerate(header):
            col_samples = [row[i] for row in samples if i < len(row)]
            col_type = infer_column_type(col, col_samples)
            columns.append(f"    {col} {col_type}")

        table_columns[table_name] = header
        ddl = f"CREATE TABLE {table_name} (\n"
        ddl += ",\n".join(columns)
        ddl += "\n);"
        ddl_statements.append(ddl)

    # FK constraints from edges metadata
    for edge_def in schema.get("edges", []):
        edge_label = edge_def["label"]
        from_vertex = edge_def["from"]
        to_vertex = edge_def["to"]
        card = edge_def.get("card", "ManyToMany")

        edge_name = edge_id_to_name.get(edge_label, f"edge_{edge_label}")
        from_name = vertex_id_to_name.get(from_vertex, f"vertex_{from_vertex}")
        to_name = vertex_id_to_name.get(to_vertex, f"vertex_{to_vertex}")

        fk_constraints.append({
            "edge_table": edge_name,
            "src_column": "src",
            "src_references": {"table": from_name, "column": "id"},
            "dst_column": "dst",
            "dst_references": {"table": to_name, "column": "id"},
            "cardinality": card,
        })

    return ddl_statements, fk_constraints, table_columns


def main():
    parser = argparse.ArgumentParser(
        description="Generate SQL schema from LDBC pathce schema"
    )
    parser.add_argument("-s", "--schema", required=True, help="pathce schema JSON")
    parser.add_argument("-d", "--data-dir", required=True, help="CSV data directory")
    parser.add_argument("-o", "--output", required=True, help="Output directory")
    args = parser.parse_args()

    with open(args.schema) as f:
        schema = json.load(f)

    output_dir = Path(args.output)
    output_dir.mkdir(parents=True, exist_ok=True)

    ddl_statements, fk_constraints, table_columns = generate_ddl_and_fk(
        schema, args.data_dir
    )

    # Write DDL
    ddl_path = output_dir / "create_tables.sql"
    with open(ddl_path, "w") as f:
        f.write("-- Auto-generated DDL for LDBC dataset\n\n")
        for stmt in ddl_statements:
            f.write(stmt + "\n\n")
    print(f"DDL: {ddl_path} ({len(ddl_statements)} tables)")

    # Write FK constraints
    fk_path = output_dir / "fk_constraints.json"
    with open(fk_path, "w") as f:
        json.dump(fk_constraints, f, indent=2)
    print(f"FK:  {fk_path} ({len(fk_constraints)} constraints)")

    # Write table list with columns
    tables_path = output_dir / "tables.json"
    with open(tables_path, "w") as f:
        json.dump(table_columns, f, indent=2)
    print(f"Tables: {tables_path} ({len(table_columns)} tables)")


if __name__ == "__main__":
    main()
