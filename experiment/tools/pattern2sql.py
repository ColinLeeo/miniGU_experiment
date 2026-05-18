#!/usr/bin/env python
import sys
import json
import argparse

parser = argparse.ArgumentParser(
    prog=sys.argv[0],
    description="Convert an input pattern (in JSON format) to SQL")
parser.add_argument("-p",
                    "--pattern",
                    help="Specify the pattern path",
                    required=True)
parser.add_argument("-s",
                    "--schema",
                    help="Specify the gCard schema path",
                    required=True)
args = parser.parse_args()

edge_labels = {}
vertex_labels = {}
with open(args.schema) as f:
    schema = json.load(f)
    for k, v in schema["edge_labels"].items():
        edge_labels[v] = k
    for k, v in schema["vertex_labels"].items():
        vertex_labels[v] = k

with open(args.pattern) as f:
    pattern = json.load(f)
    tables = []
    conditions = []
    for v in pattern["vertices"]:
        vlabel = v["label_id"]
        vid = v["tag_id"]
        table = vertex_labels[vlabel]
        tables.append(f"{table} v{vid}")
    for e in pattern["edges"]:
        elabel = e["label_id"]
        eid = e["tag_id"]
        src = e["src"]
        dst = e["dst"]
        table = edge_labels[elabel]
        tables.append(f"{table} e{eid}")
        conditions.append(f"e{eid}.src = v{src}.id and e{eid}.dst = v{dst}.id")
    if len(tables) == 0:
        print("ERROR: empty pattern is not allowed.", file=sys.stderr)
        exit(1)
    from_clause = ", ".join(tables)
    where_clause = " and ".join(conditions)

sql = f"select count(*) from {from_clause} where {where_clause}" if len(
    where_clause) != 0 else f"select count(*) from {from_clause}"
print(sql)
