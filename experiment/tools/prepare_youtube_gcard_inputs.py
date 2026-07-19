#!/usr/bin/env python3
from __future__ import annotations

import argparse
import csv
import json
import re
from collections import defaultdict
from pathlib import Path

EXP = Path(__file__).resolve().parents[1]


def natural_key(path: Path):
    return [int(x) if x.isdigit() else x for x in re.split(r"(\d+)", path.as_posix())]


def read_vertex_labels(graph: Path) -> dict[int, int]:
    labels = {}
    with graph.open(encoding="utf-8") as f:
        for line in f:
            parts = line.split()
            if not parts:
                continue
            if parts[0] == "v":
                labels[int(parts[1])] = int(parts[2])
    return labels


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
                vid = int(parts[1])
                label = int(parts[2])
                vertices.append((vid, label))
                labels[vid] = label
            elif parts[0] == "e":
                edges.append((int(parts[1]), int(parts[2]), int(parts[3])))
    return vertices, edges, labels


def write_vertex_tables(src_dataset: Path, out_dir: Path, labels: list[int]) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    for label in labels:
        src = src_dataset / f"v{label}.csv"
        dst = out_dir / f"v{label}.csv"
        dst.write_text(src.read_text(encoding="utf-8"), encoding="utf-8")


def write_edge_tables(graph: Path, out_dir: Path, vertex_labels: dict[int, int]) -> list[dict[str, object]]:
    writers = {}
    handles = {}
    counts = defaultdict(int)

    def writer_for(src_label: int, dst_label: int):
        key = (src_label, dst_label)
        if key not in writers:
            label = f"e0_v{src_label}_v{dst_label}"
            path = out_dir / f"{label}.csv"
            fh = path.open("w", newline="", encoding="utf-8")
            writer = csv.writer(fh)
            writer.writerow(["src", "dst"])
            handles[key] = fh
            writers[key] = writer
        return writers[key]

    try:
        with graph.open(encoding="utf-8") as f:
            for line in f:
                parts = line.split()
                if not parts or parts[0] != "e":
                    continue
                src = int(parts[1])
                dst = int(parts[2])
                sl = vertex_labels[src]
                dl = vertex_labels[dst]
                writer_for(sl, dl).writerow([src, dst])
                counts[(sl, dl)] += 1
                writer_for(dl, sl).writerow([dst, src])
                counts[(dl, sl)] += 1
    finally:
        for fh in handles.values():
            fh.close()

    edges = []
    for src_label, dst_label in sorted(counts):
        label = f"e0_v{src_label}_v{dst_label}"
        edges.append({
            "label": label,
            "src_label": f"v{src_label}",
            "dst_label": f"v{dst_label}",
            "file": {"path": f"{label}.csv", "format": "csv"},
            "properties": [],
        })
    return edges


def write_manifest(out_dir: Path, labels: list[int], edges: list[dict[str, object]]) -> None:
    manifest = {
        "vertices": [
            {"label": f"v{label}", "file": {"path": f"v{label}.csv", "format": "csv"}, "properties": []}
            for label in labels
        ],
        "edges": edges,
    }
    (out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


def write_patterns(pattern_dir: Path, gcard_out: Path, factorjoin_out: Path) -> int:
    gcard_out.mkdir(parents=True, exist_ok=True)
    factorjoin_out.mkdir(parents=True, exist_ok=True)
    count = 0
    for path in sorted(pattern_dir.glob("*.graph"), key=natural_key):
        vertices, edges, labels = parse_pattern(path)
        gcard = {
            "vertices": [{"id": vid + 1, "label": f"v{label}"} for vid, label in vertices],
            "edges": [
                {
                    "id": i + 1,
                    "label": f"e0_v{labels[src]}_v{labels[dst]}",
                    "src": src + 1,
                    "dst": dst + 1,
                }
                for i, (src, dst, _label) in enumerate(edges)
            ],
            "predicates": [],
        }
        factorjoin = {
            "vertices": [{"id": vid + 1, "label": f"v{label}"} for vid, label in vertices],
            "edges": [
                {"id": i + 1, "label": "e0", "src": src + 1, "dst": dst + 1}
                for i, (src, dst, _label) in enumerate(edges)
            ],
            "predicates": [],
        }
        (gcard_out / f"{path.stem}.json").write_text(json.dumps(gcard, indent=2) + "\n", encoding="utf-8")
        (factorjoin_out / f"{path.stem}.json").write_text(json.dumps(factorjoin, indent=2) + "\n", encoding="utf-8")
        count += 1
    return count


def main() -> None:
    ap = argparse.ArgumentParser(description="Prepare GCard-specific YouTube dataset and query patterns.")
    ap.add_argument("--graph", type=Path, default=EXP / "catalogs/youtube.graph")
    ap.add_argument("--src-dataset", type=Path, default=EXP / "datasets/youtube")
    ap.add_argument("--patterns", type=Path, default=EXP / "patterns/gcare/youtube")
    ap.add_argument("--out-dataset", type=Path, default=EXP / "datasets/youtube_gcard")
    ap.add_argument("--gcard-out", type=Path, default=EXP / "patterns/gcard/youtube")
    ap.add_argument("--factorjoin-out", type=Path, default=EXP / "patterns/factorjoin/youtube")
    args = ap.parse_args()

    vertex_labels = read_vertex_labels(args.graph)
    labels = sorted(set(vertex_labels.values()))
    args.out_dataset.mkdir(parents=True, exist_ok=True)
    write_vertex_tables(args.src_dataset, args.out_dataset, labels)
    edges = write_edge_tables(args.graph, args.out_dataset, vertex_labels)
    write_manifest(args.out_dataset, labels, edges)
    n = write_patterns(args.patterns, args.gcard_out, args.factorjoin_out)
    print(f"dataset={args.out_dataset}")
    print(f"edge_tables={len(edges)}")
    print(f"patterns={n}")
    print(f"gcard_patterns={args.gcard_out}")
    print(f"factorjoin_patterns={args.factorjoin_out}")


if __name__ == "__main__":
    main()
