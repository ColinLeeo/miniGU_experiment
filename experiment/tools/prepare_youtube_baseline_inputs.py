#!/usr/bin/env python3
from __future__ import annotations

import argparse
import csv
import json
import shutil
from collections import defaultdict
from pathlib import Path

EXP = Path(__file__).resolve().parents[1]


def natural_key(path: Path):
    import re
    return [int(x) if x.isdigit() else x for x in re.split(r"(\d+)", path.as_posix())]


def read_graph_header(graph: Path):
    vertex_labels = set()
    edge_labels = set()
    vertex_count = edge_count = 0
    with graph.open(encoding="utf-8") as f:
        for line in f:
            parts = line.split()
            if not parts:
                continue
            if parts[0] == "v":
                vertex_count += 1
                vertex_labels.add(int(parts[2]))
            elif parts[0] == "e":
                edge_count += 1
                edge_labels.add(int(parts[3]))
    return vertex_count, edge_count, sorted(vertex_labels), sorted(edge_labels)


def write_schema(schema_path: Path, vertex_labels: list[int], edge_labels: list[int]) -> None:
    schema_path.parent.mkdir(parents=True, exist_ok=True)
    schema = {
        "vertex_labels": {f"v{x}": x for x in vertex_labels},
        "edge_labels": {f"e{x}": x for x in edge_labels},
        "vertices": [{"label": x, "discrete": False} for x in vertex_labels],
        "edges": [
            {"card": "ManyToMany", "from": src, "label": e, "to": dst}
            for e in edge_labels
            for src in vertex_labels
            for dst in vertex_labels
            if src <= dst
        ],
    }
    schema_path.write_text(json.dumps(schema, indent=2) + "\n", encoding="utf-8")


def export_tables(graph: Path, dataset_dir: Path, pathce_dir: Path, vertex_labels: list[int], edge_labels: list[int]) -> None:
    dataset_dir.mkdir(parents=True, exist_ok=True)
    pathce_dir.mkdir(parents=True, exist_ok=True)

    vertex_files = {}
    vertex_writers = {}
    vertex_handles = {}
    for label in vertex_labels:
        for base in (dataset_dir, pathce_dir):
            path = base / f"v{label}.csv"
            fh = path.open("w", newline="", encoding="utf-8")
            writer = csv.writer(fh)
            writer.writerow(["id"])
            vertex_handles[(base, label)] = fh
            vertex_writers[(base, label)] = writer

    edge_files = {}
    edge_writers = {}
    edge_handles = {}
    for label in edge_labels:
        for base in (dataset_dir, pathce_dir):
            path = base / f"e{label}.csv"
            fh = path.open("w", newline="", encoding="utf-8")
            writer = csv.writer(fh)
            writer.writerow(["src", "dst"])
            edge_handles[(base, label)] = fh
            edge_writers[(base, label)] = writer

    try:
        with graph.open(encoding="utf-8") as f:
            for line in f:
                parts = line.split()
                if not parts:
                    continue
                if parts[0] == "v":
                    vid = int(parts[1])
                    label = int(parts[2])
                    for base in (dataset_dir, pathce_dir):
                        vertex_writers[(base, label)].writerow([vid])
                elif parts[0] == "e":
                    src = int(parts[1])
                    dst = int(parts[2])
                    label = int(parts[3])
                    for base in (dataset_dir, pathce_dir):
                        edge_writers[(base, label)].writerow([src, dst])
                        edge_writers[(base, label)].writerow([dst, src])
    finally:
        for fh in list(vertex_handles.values()) + list(edge_handles.values()):
            fh.close()

    manifest = {
        "vertices": [
            {"label": f"v{label}", "file": {"path": f"v{label}.csv", "format": "csv"}, "properties": []}
            for label in vertex_labels
        ],
        "edges": [
            {
                "label": f"e{label}",
                "src_label": "*",
                "dst_label": "*",
                "file": {"path": f"e{label}.csv", "format": "csv"},
                "src": "src",
                "dst": "dst",
                "properties": [],
            }
            for label in edge_labels
        ],
    }
    for base in (dataset_dir, pathce_dir):
        (base / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


def parse_gcare_pattern(path: Path):
    vertices = []
    edges = []
    with path.open(encoding="utf-8") as f:
        for line in f:
            parts = line.split()
            if not parts:
                continue
            if parts[0] == "v":
                vertices.append((int(parts[1]), int(parts[2])))
            elif parts[0] == "e":
                edges.append((int(parts[1]), int(parts[2]), int(parts[3])))
    return vertices, edges


def convert_patterns(pattern_dir: Path, pathce_out: Path, gcard_out: Path, sql_out: Path, safebound_sql_out: Path) -> int:
    for out in (pathce_out, gcard_out, sql_out, safebound_sql_out):
        out.mkdir(parents=True, exist_ok=True)
    count = 0
    for p in sorted(pattern_dir.glob("*.graph"), key=natural_key):
        vertices, edges = parse_gcare_pattern(p)
        pathce = {
            "vertices": [{"tag_id": vid, "label_id": label} for vid, label in vertices],
            "edges": [
                {"tag_id": i, "src": src, "dst": dst, "label_id": label}
                for i, (src, dst, label) in enumerate(edges)
            ],
        }
        (pathce_out / f"{p.stem}.json").write_text(json.dumps(pathce, indent=2) + "\n", encoding="utf-8")

        gcard = {
            "vertices": [{"id": vid + 1, "label": f"v{label}"} for vid, label in vertices],
            "edges": [
                {"id": i + 1, "label": f"e{label}", "src": src + 1, "dst": dst + 1}
                for i, (src, dst, label) in enumerate(edges)
            ],
            "predicates": [],
        }
        (gcard_out / f"{p.stem}.json").write_text(json.dumps(gcard, indent=2) + "\n", encoding="utf-8")

        sql = pattern_to_sql(edges, include_vertex_tables=True, vertices=vertices)
        (sql_out / f"{p.stem}.sql").write_text(sql + "\n", encoding="utf-8")
        (safebound_sql_out / f"{p.stem}.sql").write_text(sql + "\n", encoding="utf-8")
        count += 1
    return count


def pattern_to_sql(edges, include_vertex_tables=False, vertices=None) -> str:
    from_terms = []
    where = []
    if include_vertex_tables and vertices is not None:
        for vid, label in vertices:
            from_terms.append(f"v{label} v{vid}")
    for i, (src, dst, label) in enumerate(edges):
        alias = f"e{i}"
        from_terms.append(f"e{label} {alias}")
        if include_vertex_tables:
            where.append(f"{alias}.src = v{src}.id")
            where.append(f"{alias}.dst = v{dst}.id")
        else:
            # Edge tables already encode edge labels. Repeated query variables are expressed by equality between edge endpoints.
            pass
    first_expr = {}
    for i, (src, dst, _label) in enumerate(edges):
        for vid, expr in ((src, f"e{i}.src"), (dst, f"e{i}.dst")):
            if vid in first_expr:
                where.append(f"{expr} = {first_expr[vid]}")
            else:
                first_expr[vid] = expr
    if not from_terms:
        raise ValueError("empty pattern")
    sql = "SELECT COUNT(*) FROM " + ", ".join(from_terms)
    if where:
        sql += " WHERE " + " AND ".join(where)
    return sql


def write_normalized_undirected_graph(graph: Path, vertex_count: int, edge_count: int) -> None:
    targets = [
        EXP / "catalogs/youtube/gcare/youtube.graph",
        EXP / "catalogs/youtube/color/youtube.graph",
        EXP / "catalogs/youtube/alley/youtube.txt",
    ]
    handles = []
    try:
        for target in targets:
            target.parent.mkdir(parents=True, exist_ok=True)
            fh = target.open("w", encoding="utf-8")
            fh.write(f"t {vertex_count} {edge_count * 2}\n")
            handles.append(fh)
        with graph.open(encoding="utf-8") as f:
            for line in f:
                parts = line.split()
                if not parts:
                    continue
                if parts[0] == "v":
                    # YouTube truecard uses the first numeric label column (l1); l2 is not part of the graph label.
                    out = f"v {parts[1]} {parts[2]}\n"
                    for fh in handles:
                        fh.write(out)
                elif parts[0] == "e":
                    src, dst, label = parts[1], parts[2], parts[3]
                    for fh in handles:
                        fh.write(f"e {src} {dst} {label}\n")
                        fh.write(f"e {dst} {src} {label}\n")
    finally:
        for fh in handles:
            fh.close()



def main() -> None:
    ap = argparse.ArgumentParser(description="Prepare YouTube inputs for graph and SQL baselines from G-CARE graph/pattern files.")
    ap.add_argument("--graph", type=Path, default=EXP / "catalogs/youtube.graph")
    ap.add_argument("--patterns", type=Path, default=EXP / "patterns/gcare/youtube")
    ap.add_argument("--skip-tables", action="store_true", help="Only convert schema/patterns/shared graph; do not rewrite CSV tables")
    args = ap.parse_args()

    vc, ec, vlabels, elabels = read_graph_header(args.graph)
    schema_path = EXP / "schemas/youtube/youtube_pathce_schema.json"
    write_schema(schema_path, vlabels, elabels)
    write_normalized_undirected_graph(args.graph, vc, ec)
    if not args.skip_tables:
        export_tables(args.graph, EXP / "datasets/youtube", EXP / "datasets/youtube_pathce", vlabels, elabels)
    n = convert_patterns(
        args.patterns,
        EXP / "patterns/pathce/youtube",
        EXP / "patterns/gcard/youtube",
        EXP / "patterns/sql/youtube",
        EXP / "patterns/sql/youtube_scheme_b",
    )
    print(f"graph={args.graph}")
    print(f"vertices={vc} edges={ec} vertex_labels={len(vlabels)} edge_labels={len(elabels)}")
    print(f"schema={schema_path}")
    print("shared_graphs=catalogs/youtube/gcare/youtube.graph,catalogs/youtube/color/youtube.graph,catalogs/youtube/alley/youtube.txt")
    print("datasets=datasets/youtube,datasets/youtube_pathce")
    print("patterns=patterns/pathce/youtube,patterns/gcard/youtube,patterns/sql/youtube,patterns/sql/youtube_scheme_b")
    print(f"queries={n}")


if __name__ == "__main__":
    main()
