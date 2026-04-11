#!/usr/bin/env python3
"""
Convert pathce RawPattern (LSQB format) to miniGU gcard_query pattern format.

Usage:
    python lsqb2minigu.py -s schema.json -p pattern.json -o output.json
    python lsqb2minigu.py -s schema.json -d pattern_dir/ -o output_dir/
"""
import sys
import json
import argparse
from pathlib import Path


def build_label_maps(schema):
    """Build reverse maps: label_id -> lowercase label name."""
    vertex_id_to_name = {v: k.lower() for k, v in schema["vertex_labels"].items()}
    edge_id_to_name = {v: k.lower() for k, v in schema["edge_labels"].items()}
    return vertex_id_to_name, edge_id_to_name


def convert_pattern(pathce_pattern, vertex_map, edge_map):
    """Convert a pathce RawPattern to miniGU gcard_query format."""
    vertices = []
    for v in pathce_pattern["vertices"]:
        tag_id = v["tag_id"]
        label_id = v["label_id"]
        label_name = vertex_map.get(label_id)
        if label_name is None:
            raise ValueError(f"Unknown vertex label_id: {label_id}")
        vertices.append({"id": tag_id + 1, "label": label_name})

    edges = []
    for e in pathce_pattern["edges"]:
        tag_id = e["tag_id"]
        label_id = e["label_id"]
        label_name = edge_map.get(label_id)
        if label_name is None:
            raise ValueError(f"Unknown edge label_id: {label_id}")
        edges.append({
            "id": tag_id + 1,
            "label": label_name,
            "src": e["src"] + 1,
            "dst": e["dst"] + 1,
        })

    return {"vertices": vertices, "edges": edges, "predicates": []}


def convert_file(schema, input_path, output_path):
    vertex_map, edge_map = build_label_maps(schema)
    with open(input_path) as f:
        pathce_pattern = json.load(f)
    minigu_pattern = convert_pattern(pathce_pattern, vertex_map, edge_map)
    with open(output_path, "w") as f:
        json.dump(minigu_pattern, f, indent=2)
    print(f"  {input_path} -> {output_path}")


def main():
    parser = argparse.ArgumentParser(description="Convert LSQB/pathce patterns to miniGU format")
    parser.add_argument("-s", "--schema", required=True, help="pathce schema JSON")
    parser.add_argument("-p", "--pattern", help="Single pattern file")
    parser.add_argument("-d", "--dir", help="Directory of pattern files")
    parser.add_argument("-o", "--output", required=True, help="Output file or directory")
    args = parser.parse_args()

    with open(args.schema) as f:
        schema = json.load(f)

    if args.pattern:
        convert_file(schema, args.pattern, args.output)
    elif args.dir:
        input_dir = Path(args.dir)
        output_dir = Path(args.output)
        output_dir.mkdir(parents=True, exist_ok=True)
        for p in sorted(input_dir.glob("*.json")):
            convert_file(schema, p, output_dir / p.name)
    else:
        print("Error: specify either -p (single file) or -d (directory)", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
