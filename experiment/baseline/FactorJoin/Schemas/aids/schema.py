import csv

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


def gen_aids_merged_schema(data_path):
    schema = SchemaGraph()
    schema.add_table(
        Table(
            "vertex",
            primary_key=["id"],
            attributes=_attrs_from_csv(data_path.format("vertex"), ["id"]),
            csv_file_location=data_path.format("vertex"),
            table_size=0,
        )
    )
    for label in ("0", "1", "2", "3"):
        schema.add_table(
            Table(
                label,
                primary_key=[],
                attributes=_attrs_from_csv(data_path.format(label), ["src", "dst"]),
                csv_file_location=data_path.format(label),
                table_size=0,
            )
        )
        schema.add_relationship(label, "src", "vertex", "id")
        schema.add_relationship(label, "dst", "vertex", "id")
    return schema
