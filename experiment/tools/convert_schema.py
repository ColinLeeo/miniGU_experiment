#!/usr/bin/env python
import sys
import json
import argparse

parser = argparse.ArgumentParser(
    prog=sys.argv[0],
    description="Convert a gCard schema to the corresponding GLogS schema")
parser.add_argument("-s",
                    "--schema",
                    help="Specify the schema path",
                    required=True)
parser.add_argument("-o",
                    "--output",
                    help="Specify the output path",
                    required=True)
args = parser.parse_args()

with open(args.schema) as f:
    schema = json.load(f)

new_schema = {
    "entities": [],
    "relations": [],
    "is_column_id": False,
    "is_table_id": True,
}

for vertex_label, vertex_label_id in schema["vertex_labels"].items():
    new_schema["entities"].append({
        "columns": [],
        "label": {
            "id": vertex_label_id,
            "name": vertex_label
        }
    })

vertex_label_map = {
    vertex_label_id: vertex_label
    for vertex_label, vertex_label_id in schema["vertex_labels"].items()
}
edge_label_map = {
    edge_label_id: edge_label
    for edge_label, edge_label_id in schema["edge_labels"].items()
}

for edge in schema["edges"]:
    src_label_id = edge["from"]
    dst_label_id = edge["to"]
    edge_label_id = edge["label"]
    src_label = vertex_label_map[src_label_id]
    dst_label = vertex_label_map[dst_label_id]
    edge_label = edge_label_map[edge_label_id]
    new_schema["relations"].append({
        "columns": [],
        "entity_pairs": [{
            "src": {
                "id": src_label_id,
                "name": src_label
            },
            "dst": {
                "id": dst_label_id,
                "name": dst_label
            },
        }],
        "label": {
            "id": edge_label_id,
            "name": edge_label
        }
    })

with open(args.output, "w+") as f:
    json.dump(new_schema, f)
