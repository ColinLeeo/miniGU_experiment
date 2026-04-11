#!/usr/bin/env python3
"""
Convert a CSV dataset (LDBC format) to G-CARE graph format.

G-CARE requires vertex IDs to be consecutive integers starting from 0.
This script remaps original IDs accordingly.

Usage:
    python convert_csv_to_gcare.py -s schema.json -d dataset_dir -o output.txt
"""
import sys
import json
import argparse
import csv
from pathlib import Path

parser = argparse.ArgumentParser(
    prog=sys.argv[0], description="Convert a CSV dataset to G-CARE format")
parser.add_argument("-s", "--schema", help="Specify the schema path", required=True)
parser.add_argument("-d", "--dataset", help="Specify the dataset dir", required=True)
parser.add_argument("-o", "--output", help="Specify the output file path", required=True)
args = parser.parse_args()

dataset_dir = Path(args.dataset)
with open(args.schema) as f:
    schema = json.load(f)


def find_csv(name):
    """Find CSV file, trying lowercase first then original case."""
    p = dataset_dir / f"{name.lower()}.csv"
    if p.exists():
        return p
    p = dataset_dir / f"{name}.csv"
    if p.exists():
        return p
    raise FileNotFoundError(f"CSV not found for label '{name}' in {dataset_dir}")


# Pass 1: read all vertices, build id remap (original_id -> new_id starting from 0)
# Also store (new_id, label_id) pairs for output.
id_remap = {}  # (vlabel_id, original_id) -> new_id
vertices = []  # list of (new_id, label_id)
next_id = 0

for vlabel_name, vlabel_id in schema["vertex_labels"].items():
    csv_path = find_csv(vlabel_name)
    with open(csv_path) as f:
        reader = csv.DictReader(f)
        for row in reader:
            old_id = int(row["id"])
            id_remap[(vlabel_id, old_id)] = next_id
            vertices.append((next_id, vlabel_id))
            next_id += 1

print(f"vnum: {len(vertices)}", file=sys.stderr)

# Pass 2: read all edges, remap src/dst
edges = []  # list of (new_src, new_dst, elabel_id)

# Build edge schema lookup: elabel_name -> (src_vlabel_id, dst_vlabel_id)
edge_schema = {}
for edge_def in schema["edges"]:
    edge_schema[edge_def["label"]] = (edge_def["from"], edge_def["to"])

for elabel_name, elabel_id in schema["edge_labels"].items():
    csv_path = find_csv(elabel_name)
    src_vlabel_id, dst_vlabel_id = edge_schema[elabel_id]
    with open(csv_path) as f:
        reader = csv.DictReader(f)
        for row in reader:
            old_src = int(row["src"])
            old_dst = int(row["dst"])
            new_src = id_remap.get((src_vlabel_id, old_src))
            new_dst = id_remap.get((dst_vlabel_id, old_dst))
            if new_src is None or new_dst is None:
                # Skip edges with missing vertices
                continue
            edges.append((new_src, new_dst, elabel_id))

print(f"enum: {len(edges)}", file=sys.stderr)

# Write output: vertices must appear in order 0, 1, 2, ...
with open(args.output, "w") as f:
    writer = csv.writer(f, delimiter=" ", lineterminator="\n")
    writer.writerow(["t", "#", len(vertices)])
    # Vertices in order of new_id
    for new_id, label_id in vertices:
        writer.writerow(["v", new_id, label_id])
    for new_src, new_dst, elabel_id in edges:
        writer.writerow(["e", new_src, new_dst, elabel_id])

print(f"Written to {args.output}", file=sys.stderr)
