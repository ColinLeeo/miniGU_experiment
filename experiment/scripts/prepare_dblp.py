#!/usr/bin/env python3
import argparse
import csv
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC_GRAPH = ROOT / "catalogs/dblp/color/dblp.graph"
SRC_PATTERNS = ROOT / "patterns/gcare/dblp"
DATASET = ROOT / "datasets/dblp"
PATHCE_DATASET = ROOT / "datasets/dblp_pathce"
GCARD_PATTERNS = ROOT / "patterns/gcard/dblp"
SQL_PATTERNS = ROOT / "patterns/sql/dblp"
PARTITIONED_SQL_PATTERNS = ROOT / "patterns/sql/dblp_partitioned"
PATHCE_PATTERNS = ROOT / "patterns/pathce/dblp"
PARTITIONED_GCARD_PATTERNS = ROOT / "run_tmp/dblp_gcard_partitioned_patterns"
PARTITIONED_DATASET = ROOT / "run_tmp/dblp_gcard_partitioned_dataset"
SCHEMA = ROOT / "schemas/dblp/dblp_pathce_schema.json"
MANIFEST = ROOT / "result/dblp/workload_manifest.csv"


def parse_graph(path):
    vertices = []
    edges = []
    with path.open(encoding="utf-8") as f:
        for line in f:
            parts = line.strip().split()
            if not parts:
                continue
            if parts[0] == "v":
                vid = int(parts[1])
                label = int(parts[2])
                attr = int(parts[3]) if len(parts) > 3 else -1
                vertices.append((vid, label, attr))
            elif parts[0] == "e":
                edges.append((int(parts[1]), int(parts[2]), int(parts[3])))
    return vertices, edges


def parse_pattern(path):
    vertices = []
    edges = []
    with path.open(encoding="utf-8") as f:
        for line in f:
            parts = line.strip().split()
            if not parts:
                continue
            if parts[0] == "v":
                vertices.append((int(parts[1]), int(parts[2]), int(parts[3]) if len(parts) > 3 else -1))
            elif parts[0] == "e":
                edges.append((int(parts[1]), int(parts[2]), int(parts[3])))
    return vertices, edges


def query_sort_key(path):
    nums = [int(x) for x in re.findall(r"\d+", path.stem)]
    return (path.stem.split("_")[1] if "_" in path.stem else path.stem, nums)


def qident(name):
    return str(name)


def bidirectional_edges(edges):
    out = []
    seen = set()
    for src, dst, label in edges:
        for a, b in ((src, dst), (dst, src)):
            key = (a, b, label)
            if key not in seen:
                seen.add(key)
                out.append(key)
    return out


def rewrite_color_gcare_files(vertices, edges, force=False):
    # Color's wrapper loads these as G-Care format. For DBLP subgraph data,
    # query fourth-column data labels must be wildcard -1, and data vertices
    # should not carry the original subgraph-matching data-id field as a label.
    if force:
        with SRC_GRAPH.open("w", encoding="utf-8") as f:
            f.write(f"t {len(vertices)} {len(edges)}\n")
            for vid, label, _data_label in sorted(vertices):
                f.write(f"v {vid} {label}\n")
            for src, dst, label in edges:
                f.write(f"e {src} {dst} {label}\n")

        for path in sorted(SRC_PATTERNS.glob("*.graph"), key=query_sort_key):
            q_vertices, q_edges = parse_pattern(path)
            with path.open("w", encoding="utf-8") as f:
                f.write(f"t {len(q_vertices)} {len(q_edges)}\n")
                for vid, label, _data_label in sorted(q_vertices):
                    f.write(f"v {vid} {label} -1\n")
                for src, dst, label in q_edges:
                    f.write(f"e {src} {dst} {label}\n")


def build_dataset(force=False):
    vertices, edges_raw = parse_graph(SRC_GRAPH)
    edges = bidirectional_edges(edges_raw)
    rewrite_color_gcare_files(vertices, edges, force=force)
    # Re-read after canonicalization so downstream generation sees wildcard query data labels.
    vertices, edges_raw = parse_graph(SRC_GRAPH)
    edges = edges_raw

    DATASET.mkdir(parents=True, exist_ok=True)
    PATHCE_DATASET.mkdir(parents=True, exist_ok=True)
    PARTITIONED_DATASET.mkdir(parents=True, exist_ok=True)
    vertex_labels = sorted({label for _, label, _ in vertices})
    edge_labels = sorted({label for _, _, label in edges})

    by_vlabel = {label: [] for label in vertex_labels}
    for vid, label, _data_label in vertices:
        by_vlabel[label].append((vid,))
    for label, rows in by_vlabel.items():
        out = DATASET / f"v{label}.csv"
        if force or not out.exists():
            with out.open("w", newline="", encoding="utf-8") as f:
                w = csv.writer(f)
                w.writerow(["id"])
                w.writerows(rows)

    by_elabel = {label: [] for label in edge_labels}
    for src, dst, label in edges:
        by_elabel[label].append((src, dst))
    for label, rows in by_elabel.items():
        out = DATASET / f"e{label}.csv"
        if force or not out.exists():
            with out.open("w", newline="", encoding="utf-8") as f:
                w = csv.writer(f)
                w.writerow(["src", "dst"])
                w.writerows(rows)

    by_vertex = {vid: label for vid, label, _ in vertices}
    by_partition = {}
    for src, dst, label in edges:
        by_partition.setdefault((label, by_vertex[src], by_vertex[dst]), []).append((src, dst))
    for label in edge_labels:
        for src_label in vertex_labels:
            for dst_label in vertex_labels:
                rows = by_partition.get((label, src_label, dst_label), [])
                out = DATASET / f"pe{label}_{src_label}_{dst_label}.csv"
                if force or not out.exists():
                    with out.open("w", newline="", encoding="utf-8") as f:
                        w = csv.writer(f)
                        w.writerow(["src", "dst"])
                        w.writerows(rows)

    manifest = {
        "vertices": [
            {"label": f"v{label}", "file": {"path": f"v{label}.csv", "format": "csv"}, "properties": []}
            for label in vertex_labels
        ],
        "edges": [
            {"label": f"e{label}", "src_label": "*", "dst_label": "*", "file": {"path": f"e{label}.csv", "format": "csv"}, "properties": []}
            for label in edge_labels
        ],
    }
    (DATASET / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

    partitioned_manifest = {
        "vertices": [
            {"label": f"v{label}", "file": {"path": f"../../datasets/dblp/v{label}.csv", "format": "csv"}, "properties": []}
            for label in vertex_labels
        ],
        "edges": [
            {
                "label": f"pe{label}_{src_label}_{dst_label}",
                "src_label": f"v{src_label}",
                "dst_label": f"v{dst_label}",
                "file": {"path": f"../../datasets/dblp/pe{label}_{src_label}_{dst_label}.csv", "format": "csv"},
                "properties": [],
            }
            for label in edge_labels
            for src_label in vertex_labels
            for dst_label in vertex_labels
        ],
    }
    (PARTITIONED_DATASET / "manifest.json").write_text(json.dumps(partitioned_manifest, indent=2) + "\n", encoding="utf-8")

    vertex_out = PATHCE_DATASET / "vertex.csv"
    if force or not vertex_out.exists():
        with vertex_out.open("w", newline="", encoding="utf-8") as f:
            w = csv.writer(f)
            w.writerow(["id"])
            for vid, _label, _data_label in vertices:
                w.writerow([vid])
    edge_out = PATHCE_DATASET / "e0.csv"
    if force or not edge_out.exists():
        with edge_out.open("w", newline="", encoding="utf-8") as f:
            w = csv.writer(f)
            w.writerow(["src", "dst"])
            for src, dst, _label in edges:
                w.writerow([src, dst])

    schema = {
        "vertex_labels": {"vertex": 0},
        "edge_labels": {"e0": 0},
        "vertices": [{"label": 0, "discrete": False}],
        "edges": [{"card": "ManyToMany", "from": 0, "label": 0, "to": 0}],
    }
    SCHEMA.parent.mkdir(parents=True, exist_ok=True)
    SCHEMA.write_text(json.dumps(schema, indent=2) + "\n", encoding="utf-8")
    (PATHCE_DATASET / "manifest.json").write_text(json.dumps({"source": "dblp", "label_mode": "collapsed", "note": "PathCE structural-only DBLP dataset"}, indent=2) + "\n", encoding="utf-8")
    return vertex_labels, edge_labels

def pattern_to_minigu(vertices, edges):
    vmap = {vid: vid + 1 for vid, _, _ in vertices}
    return {
        "vertices": [{"id": vmap[vid], "label": f"v{label}"} for vid, label, _ in sorted(vertices)],
        "edges": [{"id": idx + 1, "label": f"e{label}", "src": vmap[src], "dst": vmap[dst]} for idx, (src, dst, label) in enumerate(edges)],
        "predicates": [],
    }


def pattern_to_pathce(vertices, edges):
    return {
        "vertices": [{"tag_id": vid, "label_id": 0} for vid, _label, _attr in sorted(vertices)],
        "edges": [{"tag_id": idx, "src": src, "dst": dst, "label_id": 0} for idx, (src, dst, _label) in enumerate(edges)],
    }


def pattern_to_partitioned(pattern):
    vertices = {v["id"]: v["label"] for v in pattern["vertices"]}
    return {
        "vertices": pattern["vertices"],
        "edges": [
            {**e, "label": f"p{e['label']}_{vertices[e['src']][1:]}_{vertices[e['dst']][1:]}"}
            for e in pattern["edges"]
        ],
        "predicates": [],
    }


def pattern_to_partitioned_sql(pattern):
    from_terms = []
    where = []
    first_ref = {}
    for e in sorted(pattern["edges"], key=lambda x: x["id"]):
        alias = f"e{e['id']}"
        from_terms.append(f"{qident(e['label'])} {alias}")
        for side in ("src", "dst"):
            vid = e[side]
            ref = f"{alias}.{side}"
            if vid in first_ref:
                where.append(f"{ref} = {first_ref[vid]}")
            else:
                first_ref[vid] = ref
    return "SELECT COUNT(*) FROM " + ", ".join(from_terms) + (" WHERE " + " AND ".join(where) if where else "")


def pattern_to_sql(pattern, undirected=False):
    from_terms = []
    where = []
    for v in sorted(pattern["vertices"], key=lambda x: x["id"]):
        from_terms.append(f"{qident(v['label'])} v{v['id']}")
    for e in sorted(pattern["edges"], key=lambda x: x["id"]):
        alias = f"e{e['id']}"
        from_terms.append(f"{qident(e['label'])} {alias}")
        src = f"v{e['src']}.id"
        dst = f"v{e['dst']}.id"
        if undirected:
            where.append(f"(({alias}.src = {src} AND {alias}.dst = {dst}) OR ({alias}.src = {dst} AND {alias}.dst = {src}))")
        else:
            where.append(f"{alias}.src = {src}")
            where.append(f"{alias}.dst = {dst}")
    return "SELECT COUNT(*) FROM " + ", ".join(from_terms) + " WHERE " + " AND ".join(where)


def build_patterns(force=False, undirected=False):
    GCARD_PATTERNS.mkdir(parents=True, exist_ok=True)
    SQL_PATTERNS.mkdir(parents=True, exist_ok=True)
    PATHCE_PATTERNS.mkdir(parents=True, exist_ok=True)
    PARTITIONED_SQL_PATTERNS.mkdir(parents=True, exist_ok=True)
    PARTITIONED_GCARD_PATTERNS.mkdir(parents=True, exist_ok=True)
    MANIFEST.parent.mkdir(parents=True, exist_ok=True)
    rows = []
    for src in sorted(SRC_PATTERNS.glob("*.graph"), key=query_sort_key):
        vertices, edges = parse_pattern(src)
        pattern = pattern_to_minigu(vertices, edges)
        pathce = pattern_to_pathce(vertices, edges)
        partitioned = pattern_to_partitioned(pattern)
        qname = src.stem
        group = "dense" if "dense" in qname else "sparse" if "sparse" in qname else "unknown"
        gcard_out = GCARD_PATTERNS / f"{qname}.json"
        sql_out = SQL_PATTERNS / f"{qname}.sql"
        pathce_out = PATHCE_PATTERNS / f"{qname}.json"
        partitioned_sql_out = PARTITIONED_SQL_PATTERNS / f"{qname}.sql"
        partitioned_gcard_out = PARTITIONED_GCARD_PATTERNS / f"{qname}.json"
        if force or not gcard_out.exists():
            gcard_out.write_text(json.dumps(pattern, indent=2) + "\n", encoding="utf-8")
        if force or not sql_out.exists():
            sql_out.write_text(pattern_to_sql(pattern, undirected=undirected) + "\n", encoding="utf-8")
        if force or not pathce_out.exists():
            pathce_out.write_text(json.dumps(pathce, indent=2) + "\n", encoding="utf-8")
        if force or not partitioned_sql_out.exists():
            partitioned_sql_out.write_text(pattern_to_partitioned_sql(partitioned) + "\n", encoding="utf-8")
        if force or not partitioned_gcard_out.exists():
            partitioned_gcard_out.write_text(json.dumps(partitioned, indent=2) + "\n", encoding="utf-8")
        rows.append({"query": qname, "group": group, "gcare_path": str(src), "gcard_path": str(gcard_out), "sql_path": str(sql_out), "pathce_path": str(pathce_out), "partitioned_sql_path": str(partitioned_sql_out), "partitioned_gcard_path": str(partitioned_gcard_out), "num_vertices": len(vertices), "num_edges": len(edges), "undirected_sql": int(undirected)})
    with MANIFEST.open("w", newline="", encoding="utf-8") as f:
        fieldnames = ["query", "group", "gcare_path", "gcard_path", "sql_path", "pathce_path", "partitioned_sql_path", "partitioned_gcard_path", "num_vertices", "num_edges", "undirected_sql"]
        w = csv.DictWriter(f, fieldnames=fieldnames)
        w.writeheader()
        w.writerows(rows)
    return len(rows)


def main():
    ap = argparse.ArgumentParser(description="Prepare DBLP CSV dataset and pattern formats from GCare graph files.")
    ap.add_argument("--force", action="store_true")
    ap.add_argument("--undirected", action="store_true", help="Generate SQL truecard patterns with undirected edge matching.")
    args = ap.parse_args()
    vlabels, elabels = build_dataset(force=args.force)
    n = build_patterns(force=args.force, undirected=args.undirected)
    print(f"dataset={DATASET}")
    print(f"pathce_dataset={PATHCE_DATASET}")
    print(f"schema={SCHEMA}")
    print(f"gcard_patterns={GCARD_PATTERNS}")
    print(f"sql_patterns={SQL_PATTERNS}")
    print(f"pathce_patterns={PATHCE_PATTERNS}")
    print(f"partitioned_sql_patterns={PARTITIONED_SQL_PATTERNS}")
    print(f"partitioned_gcard_patterns={PARTITIONED_GCARD_PATTERNS}")
    print(f"manifest={MANIFEST}")
    print(f"vertex_labels={len(vlabels)} edge_labels={len(elabels)} patterns={n}")


if __name__ == "__main__":
    main()
