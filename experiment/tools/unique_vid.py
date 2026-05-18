#!/usr/bin/env python3
import sys
import json
import argparse
import csv
import pathlib
import os

parser = argparse.ArgumentParser(
    prog=sys.argv[0],
    description=
    "Make vertex ids of a dataset unique (by building a global vertex map)")
parser.add_argument("-d",
                    "--dataset",
                    help="Specify the dataset dir",
                    required=True)
parser.add_argument("-s",
                    "--schema",
                    help="Specify the gCard schema path",
                    required=True)
args = parser.parse_args()

with open(args.schema) as f:
    schema = json.load(f)

next_global_id = 0
global_vertex_map = {}
dir = pathlib.Path(args.dataset)
assert dir.is_dir()

for vertex_label, vertex_label_id in schema["vertex_labels"].items():
    local_vertex_map = {}
    path = dir.joinpath(f"{vertex_label}.csv")
    with open(path) as f:
        reader = csv.DictReader(f)
        for record in reader:
            local_id = int(record["id"])
            local_vertex_map[local_id] = next_global_id
            next_global_id += 1
    global_vertex_map[vertex_label_id] = local_vertex_map
    new_path = dir.joinpath(f"{vertex_label}.csv.tmp")
    with open(new_path, "w+") as f:
        writer = csv.DictWriter(f, fieldnames=["id"])
        writer.writeheader()
        for global_id in sorted(local_vertex_map.values()):
            writer.writerow({"id": global_id})

for edge_label, edge_label_id in schema["edge_labels"].items():
    src_vertex_map = None
    dst_vertex_map = None
    for edge in schema["edges"]:
        if edge["label"] == edge_label_id:
            src_label_id = edge["from"]
            dst_label_id = edge["to"]
            src_vertex_map = global_vertex_map[src_label_id]
            dst_vertex_map = global_vertex_map[dst_label_id]
            break
    path = dir.joinpath(f"{edge_label}.csv")
    edges = []
    with open(path) as f:
        reader = csv.DictReader(f)
        for record in reader:
            src = int(record["src"])
            dst = int(record["dst"])
            src_global_id = src_vertex_map[src]
            dst_global_id = dst_vertex_map[dst]
            edges.append({"src": src_global_id, "dst": dst_global_id})
    new_path = dir.joinpath(f"{edge_label}.csv.tmp")
    with open(new_path, "w+") as f:
        writer = csv.DictWriter(f, ["src", "dst"])
        writer.writeheader()
        writer.writerows(edges)

for vertex_label in schema["vertex_labels"]:
    path = dir.joinpath(f"{vertex_label}.csv")
    new_path = dir.joinpath(f"{vertex_label}.csv.tmp")
    os.rename(new_path, path)

for edge_label in schema["edge_labels"]:
    path = dir.joinpath(f"{edge_label}.csv")
    new_path = dir.joinpath(f"{edge_label}.csv.tmp")
    os.rename(new_path, path)
