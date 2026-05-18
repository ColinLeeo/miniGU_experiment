#!/usr/bin/env python
import sys
import json
import argparse
import csv
import os
from tqdm import tqdm
from pathlib import Path

parser = argparse.ArgumentParser(
    prog=sys.argv[0],
    description="Convert AIDS query in G-CARE format to pathce format")
parser.add_argument("-i",
                    "--input",
                    help="Specify the input query dir",
                    required=True)
parser.add_argument("-s",
                    "--schema",
                    help="Specify the schema path",
                    required=True)
parser.add_argument("-o",
                    "--output",
                    help="Specify the output query dir",
                    required=True)
parser.add_argument("-t",
                    "--type",
                    help="Specify the conversion type",
                    default="regular",
                    choices=["regular", "merge", "extend"])
args = parser.parse_args()

with open(args.schema) as f:
    schema = json.load(f)
schema_edge_labels = schema["edge_labels"]
schema_edge_label_props = {int(e["label"]): (int(e["from"]), int(e["to"])) for e in schema["edges"]}

output_dir = Path(args.output)
os.makedirs(output_dir, exist_ok=True)


def load_query(path):
    vertices = []
    edges = []
    f = open(path)
    reader = csv.reader(f, delimiter=" ", lineterminator="\n", strict=False)
    next(reader)
    for row in reader:
        if row[0] == "v":
            assert len(row) == 4, "invalid vertex row"
            id = int(row[1])
            label = int(row[2])
            vertices.append((id, label))
        elif row[0] == "e":
            assert len(row) == 4, "invalid edge row"
            src = int(row[1])
            dst = int(row[2])
            label = int(row[3])
            edges.append((src, dst, label))
        else:
            assert False, "invalid object type"
    f.close()
    return vertices, edges


def convert_query_regular(path):
    vertices, edges = load_query(path)
    candidate_edge_labels = []
    for _, _, label in edges:
        candidates = []
        for k, label_id in schema_edge_labels.items():
            if int(k.split("_")[1]) == label:
                candidates.append(label_id)
        candidate_edge_labels.append(set(candidates))
    for vid, vl in vertices:
        if vl == -1:
            continue
        for (src, dst, _), candidate_labels in zip(edges, candidate_edge_labels):
            candidates_to_remove = []
            if src == vid:
                for cl in candidate_labels:
                    sl, _ = schema_edge_label_props[cl]
                    if sl != vl:
                        candidates_to_remove.append(cl)
            if dst == vid:
                for cl in candidate_labels:
                    _, tl = schema_edge_label_props[cl]
                    if tl != vl:
                        candidates_to_remove.append(cl)
            for cl in candidates_to_remove:
                candidate_labels.remove(cl)
    
    
        
    print(vertices)
    print(edges)
    print(candidate_edge_labels)
    for cel in candidate_edge_labels:
        print(len(cel))
    exit(0)


def convert_query_merged(path):
    vertices, edges = load_query(path)
    query = {
        "vertices": [{
            "tag_id": id,
            "label_id": 0
        } for id, _ in vertices],
        "edges": [{
            "tag_id": i,
            "src": src,
            "dst": dst,
            "label_id": label
        } for i, (src, dst, label) in enumerate(edges)]
    }
    return query


for i, entry in enumerate(sorted(os.scandir(args.input),
                                 key=lambda e: e.name)):
    if args.type == "regular":
        query = convert_query_regular(entry.path)
    else:
        query = convert_query_merged(entry.path)
    output_path = output_dir / f"{i}.json"
    with open(output_path, "w") as f:
        print(f"write {output_path}")
        json.dump(query, f, indent=4)
