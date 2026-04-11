#!/usr/bin/env python
import sys
import json
import argparse
import csv

parser = argparse.ArgumentParser(
    prog=sys.argv[0],
    description="Convert an input pattern (in JSON format) to G-CARE format")
parser.add_argument("-p",
                    "--pattern",
                    help="Specify the pattern path",
                    required=True)
parser.add_argument("-o",
                    "--output",
                    help="Specify the output path",
                    required=True)
args = parser.parse_args()

with open(args.pattern) as f:
    pattern = json.load(f)

tag_id_map = {}
next_vertex_id = 0
for v in pattern["vertices"]:
    tag_id_map[v["tag_id"]] = next_vertex_id
    next_vertex_id += 1

with open(args.output, "w") as f:
    writer = csv.writer(f, delimiter=" ", lineterminator="\n")
    writer.writerow(["t", "#", "s", 123])
    for v in pattern["vertices"]:
        writer.writerow(["v", tag_id_map[v["tag_id"]], v["label_id"], -1])
    for e in pattern["edges"]:
        writer.writerow(
            ["e", tag_id_map[e["src"]], tag_id_map[e["dst"]], e["label_id"]])
