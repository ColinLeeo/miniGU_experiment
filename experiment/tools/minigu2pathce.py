#!/usr/bin/env python3
"""
Convert miniGU gcard_query patterns to pathce RawPattern format and G-CARE format.

Usage:
    # Single file
    python minigu2pathce.py -s schema.json -p pattern.json -o output.json
    python minigu2pathce.py -s schema.json -p pattern.json -o output.json --gcare output.txt

    # Directory
    python minigu2pathce.py -s schema.json -d pattern_dir/ -o output_dir/
    python minigu2pathce.py -s schema.json -d pattern_dir/ -o output_dir/ --gcare gcare_dir/
"""
import sys
import json
import csv
import argparse
from pathlib import Path


def build_label_maps(schema):
    """Build case-insensitive maps: lowercase name -> label_id."""
    vertex_name_to_id = {k.lower(): v for k, v in schema["vertex_labels"].items()}
    edge_name_to_id = {k.lower(): v for k, v in schema["edge_labels"].items()}
    return vertex_name_to_id, edge_name_to_id


def convert_to_pathce(minigu_pattern, vertex_map, edge_map):
    """Convert miniGU pattern to pathce RawPattern format."""
    # miniGU vertex ids are 1-based, pathce tag_ids are 0-based
    vertices = []
    for v in minigu_pattern["vertices"]:
        label_name = v["label"].lower()
        label_id = vertex_map.get(label_name)
        if label_id is None:
            raise ValueError(f"Unknown vertex label: {v['label']} (tried: {label_name})")
        vertices.append({
            "tag_id": v["id"] - 1,
            "label_id": label_id
        })

    edges = []
    for e in minigu_pattern["edges"]:
        label_name = e["label"].lower()
        label_id = edge_map.get(label_name)
        if label_id is None:
            raise ValueError(f"Unknown edge label: {e['label']} (tried: {label_name})")
        edges.append({
            "tag_id": e["id"] - 1,
            "src": e["src"] - 1,
            "dst": e["dst"] - 1,
            "label_id": label_id
        })

    return {"vertices": vertices, "edges": edges}


def write_gcare(pathce_pattern, output_path):
    """Write pathce pattern as G-CARE query format."""
    tag_id_map = {}
    next_id = 0
    for v in pathce_pattern["vertices"]:
        tag_id_map[v["tag_id"]] = next_id
        next_id += 1

    with open(output_path, "w") as f:
        writer = csv.writer(f, delimiter=" ", lineterminator="\n")
        writer.writerow(["t", "#", "s", 123])
        for v in pathce_pattern["vertices"]:
            writer.writerow(["v", tag_id_map[v["tag_id"]], v["label_id"], -1])
        for e in pathce_pattern["edges"]:
            writer.writerow(["e", tag_id_map[e["src"]], tag_id_map[e["dst"]], e["label_id"]])


def convert_file(schema, input_path, output_path, gcare_path=None):
    vertex_map, edge_map = build_label_maps(schema)
    with open(input_path) as f:
        minigu_pattern = json.load(f)

    pathce_pattern = convert_to_pathce(minigu_pattern, vertex_map, edge_map)

    with open(output_path, "w") as f:
        json.dump(pathce_pattern, f, indent=2)
    print(f"  pathce: {input_path} -> {output_path}")

    if gcare_path:
        write_gcare(pathce_pattern, gcare_path)
        print(f"  gcare:  {input_path} -> {gcare_path}")


def main():
    parser = argparse.ArgumentParser(description="Convert miniGU patterns to pathce/G-CARE format")
    parser.add_argument("-s", "--schema", required=True, help="pathce schema JSON")
    parser.add_argument("-p", "--pattern", help="Single pattern file")
    parser.add_argument("-d", "--dir", help="Directory of pattern files (recursive)")
    parser.add_argument("-o", "--output", required=True, help="Output file or directory")
    parser.add_argument("--gcare", help="Also output G-CARE format to this file or directory")
    args = parser.parse_args()

    with open(args.schema) as f:
        schema = json.load(f)

    if args.pattern:
        convert_file(schema, args.pattern, args.output, args.gcare)
    elif args.dir:
        input_dir = Path(args.dir)
        output_dir = Path(args.output)
        output_dir.mkdir(parents=True, exist_ok=True)
        gcare_dir = Path(args.gcare) if args.gcare else None
        if gcare_dir:
            gcare_dir.mkdir(parents=True, exist_ok=True)

        for p in sorted(input_dir.rglob("*.json")):
            # Preserve subdirectory structure
            rel = p.relative_to(input_dir)
            out = output_dir / rel
            out.parent.mkdir(parents=True, exist_ok=True)
            gcare_out = None
            if gcare_dir:
                gcare_out = gcare_dir / rel.with_suffix(".txt")
                gcare_out.parent.mkdir(parents=True, exist_ok=True)
            try:
                convert_file(schema, p, out, gcare_out)
            except ValueError as e:
                print(f"  SKIP {p}: {e}", file=sys.stderr)
    else:
        print("Error: specify either -p or -d", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
