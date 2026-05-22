import json
import os
import sys


REPRO_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
SAFEB0UND_BAYESCARD = os.path.abspath(
    os.path.join(REPRO_DIR, "..", "bayescard")
)
if SAFEB0UND_BAYESCARD not in sys.path:
    sys.path.insert(0, SAFEB0UND_BAYESCARD)

from Schemas.graph_representation import SchemaGraph, Table

DEFAULT_DATA_DIR = os.path.join(REPRO_DIR, "prepared_csv")
DEFAULT_MANIFEST = "/home/zxz/miniGU/experiment/datasets/ldbc/sf1/manifest.json"

DROP_ATTRIBUTES = {
    "city": ["url"],
    "comment": ["locationip", "content"],
    "company": ["url"],
    "continent": ["url"],
    "country": ["url"],
    "forum": ["title"],
    "person": [
        "firstname",
        "lastname",
        "locationip",
        "language",
        "email",
    ],
    "post": ["imagefile", "locationip", "language", "content"],
    "tag": ["url"],
    "tagclass": ["url"],
    "university": ["url"],
}


def _csv(data_dir, table):
    return os.path.join(data_dir, "{}.csv".format(table))


def _read_header(path):
    with open(path, encoding="utf-8") as handle:
        return handle.readline().strip().split(",")


def _row_count(path):
    with open(path, "rb") as handle:
        return max(sum(1 for _ in handle) - 1, 0)


def _load_manifest(manifest_path):
    with open(manifest_path, encoding="utf-8") as handle:
        return json.load(handle)


def gen_ldbc_sf1_full_schema(data_dir=DEFAULT_DATA_DIR, manifest_path=DEFAULT_MANIFEST):
    manifest = _load_manifest(manifest_path)
    schema = SchemaGraph()

    vertex_labels = {
        vertex["label"]
        for vertex in manifest["vertices"]
    }
    edge_specs = manifest["edges"]
    edge_labels = {
        edge["label"]
        for edge in edge_specs
    }

    for table in sorted(vertex_labels | edge_labels):
        csv_path = _csv(data_dir, table)
        attributes = _read_header(csv_path)
        primary_key = ["id"]
        irrelevant_attributes = [
            attr
            for attr in DROP_ATTRIBUTES.get(table, [])
            if attr in attributes
        ]
        no_compression = None
        if table in edge_labels:
            no_compression = ["src", "dst"]
        schema.add_table(
            Table(
                table,
                primary_key=primary_key,
                table_size=_row_count(csv_path),
                csv_file_location=csv_path,
                attributes=attributes,
                irrelevant_attributes=irrelevant_attributes,
                no_compression=no_compression,
            )
        )

    for edge in edge_specs:
        label = edge["label"]
        src_label = edge["src_label"]
        dst_label = edge["dst_label"]
        schema.add_relationship(label, "src", src_label, "id")
        schema.add_relationship(label, "dst", dst_label, "id")

    return schema


if __name__ == "__main__":
    graph = gen_ldbc_sf1_full_schema()
    print("tables={}".format(len(graph.tables)))
    print("relationships={}".format(len(graph.relationships)))
    for relationship in graph.relationships:
        print(relationship.identifier)
