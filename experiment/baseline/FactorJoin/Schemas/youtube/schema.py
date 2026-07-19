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


def gen_youtube_schema(data_path):
    schema = SchemaGraph()
    root = Path(data_path.format("__table__")).parent

    vertex_paths = []
    for path in root.glob("v*.csv"):
        if re.match(r"^v\d+$", path.stem):
            vertex_paths.append((int(path.stem[1:]), path))

    for _label, vertex_path in sorted(vertex_paths):
        table = vertex_path.stem
        schema.add_table(
            Table(
                table,
                primary_key=["id"],
                attributes=_attrs_from_csv(_table_path(data_path, table), ["id"]),
                csv_file_location=_table_path(data_path, table),
                table_size=0,
            )
        )

    edge_path = root / "e0.csv"
    if edge_path.exists():
        schema.add_table(
            Table(
                "e0",
                primary_key=[],
                attributes=_attrs_from_csv(_table_path(data_path, "e0"), ["src", "dst"]),
                csv_file_location=_table_path(data_path, "e0"),
                table_size=0,
            )
        )
        for label, _path in sorted(vertex_paths):
            table = f"v{label}"
            schema.add_relationship("e0", "src", table, "id")
            schema.add_relationship("e0", "dst", table, "id")

    return schema
