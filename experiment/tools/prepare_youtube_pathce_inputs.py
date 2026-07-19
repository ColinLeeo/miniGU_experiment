#!/usr/bin/env python3
from __future__ import annotations

import argparse
import csv
import json
import re
from pathlib import Path

EXP = Path(__file__).resolve().parents[1]


def natural_key(path: Path):
    return [int(x) if x.isdigit() else x for x in re.split(r"(\d+)", path.as_posix())]


def parse_pattern(path: Path):
    vertices = []
    edges = []
    labels = {}
    with path.open(encoding="utf-8") as f:
        for line in f:
            parts = line.split()
            if not parts:
                continue
            if parts[0] == "v":
                vid = int(parts[1]); lab = int(parts[2])
                vertices.append((vid, lab)); labels[vid] = lab
            elif parts[0] == "e":
                edges.append((int(parts[1]), int(parts[2]), int(parts[3])))
    return vertices, edges, labels


def copy_dataset(src: Path, dst: Path) -> list[tuple[int, int, str]]:
    dst.mkdir(parents=True, exist_ok=True)
    for v in sorted(src.glob("v*.csv"), key=natural_key):
        (dst / v.name).write_text(v.read_text(encoding="utf-8"), encoding="utf-8")
    edge_specs = []
    for e in sorted(src.glob("e0_v*_v*.csv"), key=natural_key):
        m = re.match(r"e0_v(\d+)_v(\d+)\.csv", e.name)
        if not m:
            continue
        sl, dl = int(m.group(1)), int(m.group(2))
        (dst / e.name).write_text(e.read_text(encoding="utf-8"), encoding="utf-8")
        edge_specs.append((sl, dl, e.stem))
    manifest = {
        "vertices": [
            {"label": f"v{i}", "file": {"path": f"v{i}.csv", "format": "csv"}, "properties": []}
            for i in range(25)
        ],
        "edges": [
            {"label": name, "src_label": f"v{sl}", "dst_label": f"v{dl}", "file": {"path": f"{name}.csv", "format": "csv"}, "properties": []}
            for sl, dl, name in edge_specs
        ],
    }
    (dst / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return edge_specs


def write_schema(path: Path, edge_specs: list[tuple[int, int, str]]) -> dict[str, int]:
    path.parent.mkdir(parents=True, exist_ok=True)
    edge_labels = {name: i for i, (_sl, _dl, name) in enumerate(edge_specs)}
    schema = {
        "vertex_labels": {f"v{i}": i for i in range(25)},
        "edge_labels": edge_labels,
        "vertices": [{"label": i, "discrete": False} for i in range(25)],
        "edges": [
            {"card": "ManyToMany", "from": sl, "label": edge_labels[name], "to": dl}
            for sl, dl, name in edge_specs
        ],
    }
    path.write_text(json.dumps(schema, indent=2) + "\n", encoding="utf-8")
    return edge_labels


def write_patterns(pattern_dir: Path, out_dir: Path, edge_labels: dict[str, int]) -> int:
    out_dir.mkdir(parents=True, exist_ok=True)
    count = 0
    for p in sorted(pattern_dir.glob("*.graph"), key=natural_key):
        vertices, edges, labels = parse_pattern(p)
        obj = {
            "vertices": [{"tag_id": vid, "label_id": label} for vid, label in vertices],
            "edges": [
                {"tag_id": i, "src": src, "dst": dst, "label_id": edge_labels[f"e0_v{labels[src]}_v{labels[dst]}"]}
                for i, (src, dst, _label) in enumerate(edges)
            ],
        }
        (out_dir / f"{p.stem}.json").write_text(json.dumps(obj, indent=2) + "\n", encoding="utf-8")
        count += 1
    return count


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--src-dataset", type=Path, default=EXP / "datasets/youtube_gcard")
    ap.add_argument("--out-dataset", type=Path, default=EXP / "datasets/youtube_pathce_pair")
    ap.add_argument("--schema", type=Path, default=EXP / "schemas/youtube/youtube_pathce_pair_schema.json")
    ap.add_argument("--patterns", type=Path, default=EXP / "patterns/gcare/youtube")
    ap.add_argument("--out-patterns", type=Path, default=EXP / "patterns/pathce/youtube")
    args = ap.parse_args()
    edge_specs = copy_dataset(args.src_dataset, args.out_dataset)
    edge_labels = write_schema(args.schema, edge_specs)
    n = write_patterns(args.patterns, args.out_patterns, edge_labels)
    print(f"dataset={args.out_dataset}")
    print(f"schema={args.schema}")
    print(f"edge_labels={len(edge_labels)}")
    print(f"patterns={n}")


if __name__ == "__main__":
    main()
