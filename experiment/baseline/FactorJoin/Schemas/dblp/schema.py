import csv
import re
from pathlib import Path

from Schemas.graph_representation import SchemaGraph, Table


def _attrs_from_csv(path, fallback):
    try:
        with open(path, newline="", encoding="utf-8") as f:
            row = next(csv.reader(f), None)
            if row:
                return row
    except OSError:
        pass
    return fallback


def _table_path(data_path, table_name):
    return data_path.format(table_name)


def gen_dblp_schema(data_path):
    schema = SchemaGraph()
    root = Path(data_path.format("__table__")).parent

    for vertex_path in sorted(root.glob("v*.csv"), key=lambda p: int(p.stem[1:])):
        table = vertex_path.stem
        schema.add_table(
            Table(
                table,
                primary_key=["id"],
                attributes=_attrs_from_csv(_table_path(data_path, table), ["id", "attr"]),
                csv_file_location=_table_path(data_path, table),
                table_size=0,
            )
        )

    edge_re = re.compile(r"pe0_(\d+)_(\d+)\.csv$")
    edge_paths = []
    for edge_path in root.glob("pe0_*_*.csv"):
        match = edge_re.match(edge_path.name)
        if match:
            edge_paths.append((int(match.group(1)), int(match.group(2)), edge_path))

    for src_label, dst_label, edge_path in sorted(edge_paths):
        table = edge_path.stem
        schema.add_table(
            Table(
                table,
                primary_key=[],
                attributes=_attrs_from_csv(_table_path(data_path, table), ["src", "dst"]),
                csv_file_location=_table_path(data_path, table),
                table_size=0,
            )
        )
        schema.add_relationship(table, "src", f"v{src_label}", "id")
        schema.add_relationship(table, "dst", f"v{dst_label}", "id")

    return schema
