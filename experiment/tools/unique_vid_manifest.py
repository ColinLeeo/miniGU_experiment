#!/usr/bin/env python3
import argparse
import csv
import json
import os
from pathlib import Path


def parse_args():
    parser = argparse.ArgumentParser(
        description="Rewrite manifest-based graph CSV files so vertex ids are graph-wide unique."
    )
    parser.add_argument(
        "-d",
        "--dataset",
        required=True,
        help="Dataset directory containing manifest.json and CSV files.",
    )
    parser.add_argument(
        "--manifest",
        default="manifest.json",
        help="Manifest filename relative to dataset dir.",
    )
    return parser.parse_args()


def file_path(dataset_dir: Path, file_spec):
    if isinstance(file_spec, dict):
        return dataset_dir / file_spec["path"]
    return dataset_dir / file_spec


def rewrite_csv(path: Path, rows):
    tmp_path = path.with_suffix(path.suffix + ".tmp")
    with tmp_path.open("w", newline="") as out:
        writer = csv.writer(out)
        writer.writerows(rows)
    os.replace(tmp_path, path)


def main():
    args = parse_args()
    dataset_dir = Path(args.dataset)
    manifest_path = dataset_dir / args.manifest
    with manifest_path.open() as f:
        manifest = json.load(f)

    next_global_id = 0
    vertex_maps = {}

    for vertex in manifest.get("vertices", []):
        label = vertex["label"].lower()
        path = file_path(dataset_dir, vertex["file"])
        local_to_global = {}
        rewritten_rows = []

        with path.open(newline="") as f:
            reader = csv.reader(f)
            header = next(reader)
            rewritten_rows.append(header)
            for row in reader:
                if not row:
                    rewritten_rows.append(row)
                    continue
                local_id = int(row[0])
                if local_id in local_to_global:
                    raise ValueError(f"duplicate vertex id {local_id} in {path}")
                global_id = next_global_id
                next_global_id += 1
                local_to_global[local_id] = global_id
                row[0] = str(global_id)
                rewritten_rows.append(row)

        vertex_maps[label] = local_to_global
        rewrite_csv(path, rewritten_rows)
        print(f"vertex {label}: {len(local_to_global)} ids rewritten")

    for edge in manifest.get("edges", []):
        label = edge["label"].lower()
        src_label = edge["src_label"].lower()
        dst_label = edge["dst_label"].lower()
        src_map = vertex_maps[src_label]
        dst_map = vertex_maps[dst_label]
        path = file_path(dataset_dir, edge["file"])
        rewritten_rows = []
        edge_count = 0

        with path.open(newline="") as f:
            reader = csv.reader(f)
            header = next(reader)
            rewritten_rows.append(header)
            for row in reader:
                if not row:
                    rewritten_rows.append(row)
                    continue
                row[0] = str(src_map[int(row[0])])
                row[1] = str(dst_map[int(row[1])])
                rewritten_rows.append(row)
                edge_count += 1

        rewrite_csv(path, rewritten_rows)
        print(f"edge {label}: {edge_count} endpoints rewritten")

    print(f"done: {next_global_id} graph-wide vertex ids assigned")


if __name__ == "__main__":
    main()
