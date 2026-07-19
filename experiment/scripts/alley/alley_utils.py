#!/usr/bin/env python3
from __future__ import annotations

import argparse
import csv
import json
import math
import shlex
import shutil
from pathlib import Path
from typing import Any


EXP = Path(__file__).resolve().parents[2]


ALIASES = {
    "aids": "aids_merged",
    "aids_merged": "aids_merged",
    "dblp": "dblp",
    "ldbc": "ldbc_sf1",
    "ldbc_sf1": "ldbc_sf1",
    "imdb": "imdb",
    "youtube": "youtube",
}


def natural_key(path: Path) -> list[int | str]:
    import re

    return [int(part) if part.isdigit() else part for part in re.split(r"(\d+)", path.as_posix())]


def canonical_dataset(name: str) -> str:
    key = name.lower()
    if key not in ALIASES:
        raise SystemExit(f"Unsupported Alley dataset: {name}")
    return ALIASES[key]


def dataset_config(name: str) -> dict[str, Any]:
    ds = canonical_dataset(name)
    if ds == "aids_merged":
        return {
            "dataset": ds,
            "dataset_dir": EXP / "datasets/aids_merged",
            "schema": EXP / "schemas/aids_merged/aids_merged_pathce_schema.json",
            "graph_source": EXP / "catalogs/aids_merged/gcare/aids_merged.txt",
            "graph_text": EXP / "catalogs/aids_merged/alley/aids_merged.txt",
            "data_header": EXP / "catalogs/aids_merged/alley/aids_merged",
            "query_roots": [("aids_merged", EXP / "patterns/gcare/aids_merged", "text")],
            "query_dir": EXP / "run_tmp/alley_queries/aids_merged",
        }
    if ds == "dblp":
        return {
            "dataset": ds,
            "dataset_dir": EXP / "datasets/dblp",
            "schema": None,
            "graph_source": EXP / "catalogs/dblp/color/dblp.graph",
            "graph_text": EXP / "catalogs/dblp/alley/dblp.txt",
            "data_header": EXP / "catalogs/dblp/alley/dblp",
            "query_roots": [("dblp", EXP / "patterns/gcare/dblp", "text")],
            "query_dir": EXP / "run_tmp/alley_queries/dblp",
        }
    if ds == "ldbc_sf1":
        return {
            "dataset": ds,
            "dataset_dir": EXP / "datasets/ldbc/sf1",
            "schema": EXP / "schemas/ldbc/ldbc_pathce_schema.json",
            "graph_source": EXP / "catalogs/ldbc/gcare/sf1.txt",
            "graph_text": EXP / "catalogs/ldbc/alley/sf1.txt",
            "data_header": EXP / "catalogs/ldbc/alley/sf1",
            "query_roots": [
                ("lsqb", EXP / "patterns/gcare/lsqb", "text"),
                ("glogs", EXP / "patterns/gcare/glogs", "text"),
            ],
            "query_dir": EXP / "run_tmp/alley_queries/ldbc_sf1",
        }
    if ds == "imdb":
        return {
            "dataset": ds,
            "dataset_dir": EXP / "datasets/imdb/imdb",
            "schema": EXP / "schemas/imdb/imdb_pathce_schema.json",
            "graph_source": None,
            "graph_text": EXP / "catalogs/imdb/alley/imdb.txt",
            "data_header": EXP / "catalogs/imdb/alley/imdb",
            "query_roots": [("imdb", EXP / "patterns/gcare/imdb", "text")],
            "query_dir": EXP / "run_tmp/alley_queries/imdb",
        }
    if ds == "youtube":
        return {
            "dataset": ds,
            "dataset_dir": EXP / "datasets/youtube",
            "schema": EXP / "schemas/youtube/youtube_pathce_schema.json",
            "graph_source": EXP / "catalogs/youtube/gcare/youtube.graph",
            "graph_text": EXP / "catalogs/youtube/alley/youtube.txt",
            "data_header": EXP / "catalogs/youtube/alley/youtube",
            "query_roots": [("youtube", EXP / "patterns/gcare/youtube", "text")],
            "query_dir": EXP / "run_tmp/alley_queries/youtube",
        }
    raise AssertionError(ds)


def load_manifest(dataset_dir: Path) -> dict[str, Any]:
    with (dataset_dir / "manifest.json").open(encoding="utf-8") as f:
        return json.load(f)


def manifest_label_maps(manifest: dict[str, Any]) -> tuple[dict[str, int], dict[str, int]]:
    vertex = {v["label"].lower(): i for i, v in enumerate(manifest["vertices"])}
    edge = {e["label"].lower(): i for i, e in enumerate(manifest["edges"])}
    return vertex, edge


def resolve_dataset_file(dataset_dir: Path, rel: str) -> Path:
    path = dataset_dir / rel
    if path.exists():
        return path
    path = (dataset_dir / rel).resolve()
    if path.exists():
        return path
    raise FileNotFoundError(f"Dataset file not found: {dataset_dir / rel}")


def export_manifest_graph(dataset_dir: Path, out_path: Path, force: bool = False) -> None:
    if out_path.exists() and not force:
        return
    manifest = load_manifest(dataset_dir)
    vertex_map, edge_map = manifest_label_maps(manifest)

    id_remap: dict[tuple[str, str], int] = {}
    vertices: list[tuple[int, int]] = []
    next_id = 0
    for vertex in manifest["vertices"]:
        label = vertex["label"]
        label_id = vertex_map[label.lower()]
        csv_path = resolve_dataset_file(dataset_dir, vertex["file"]["path"])
        with csv_path.open(newline="", encoding="utf-8") as f:
            for row in csv.DictReader(f):
                old_id = row["id"]
                id_remap[(label, old_id)] = next_id
                vertices.append((next_id, label_id))
                next_id += 1

    edges: list[tuple[int, int, int]] = []
    for edge in manifest["edges"]:
        label_id = edge_map[edge["label"].lower()]
        src_label = edge["src_label"]
        dst_label = edge["dst_label"]
        csv_path = resolve_dataset_file(dataset_dir, edge["file"]["path"])
        with csv_path.open(newline="", encoding="utf-8") as f:
            for row in csv.DictReader(f):
                src = id_remap.get((src_label, row["src"]))
                dst = id_remap.get((dst_label, row["dst"]))
                if src is None or dst is None:
                    continue
                edges.append((src, dst, label_id))

    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.writer(f, delimiter=" ", lineterminator="\n")
        writer.writerow(["t", "#", len(vertices)])
        for vid, label_id in vertices:
            writer.writerow(["v", vid, label_id])
        for src, dst, label_id in edges:
            writer.writerow(["e", src, dst, label_id])


def normalize_text_graph(src: Path, dst: Path, *, query: bool, force: bool = False) -> None:
    if dst.exists() and not force:
        return
    dst.parent.mkdir(parents=True, exist_ok=True)
    with src.open(encoding="utf-8", errors="replace") as inp, dst.open("w", encoding="utf-8") as out:
        for line in inp:
            parts = line.strip().split()
            if not parts:
                continue
            if parts[0] == "t":
                out.write(" ".join(parts) + "\n")
            elif parts[0] == "v":
                if query and len(parts) == 3:
                    parts.append("-1")
                out.write(" ".join(parts[:4] if query else parts) + "\n")
            elif parts[0] == "e":
                out.write(" ".join(parts[:4]) + "\n")


def convert_gcard_pattern(src: Path, dst: Path, dataset_dir: Path, force: bool = False) -> None:
    if dst.exists() and not force:
        return
    manifest = load_manifest(dataset_dir)
    vertex_map, edge_map = manifest_label_maps(manifest)
    pattern = json.loads(src.read_text(encoding="utf-8"))
    vertices = pattern["vertices"]
    edges = pattern["edges"]
    id_map = {v["id"]: i for i, v in enumerate(vertices)}

    dst.parent.mkdir(parents=True, exist_ok=True)
    with dst.open("w", encoding="utf-8") as out:
        out.write(f"t {len(vertices)} {len(edges)}\n")
        for v in vertices:
            label_id = vertex_map[v["label"].lower()]
            out.write(f"v {id_map[v['id']]} {label_id} -1\n")
        for e in edges:
            label_id = edge_map[e["label"].lower()]
            out.write(f"e {id_map[e['src']]} {id_map[e['dst']]} {label_id}\n")


def prepare_graph(cfg: dict[str, Any], force: bool = False) -> None:
    graph_text = cfg["graph_text"]
    source = cfg["graph_source"]
    if source is not None and source.exists():
        normalize_text_graph(source, graph_text, query=False, force=force)
        return
    if cfg["schema"] is not None:
        # Use the repository's schema converter when no reusable G-CARE graph exists.
        if graph_text.exists() and not force:
            return
        import subprocess
        import sys

        graph_text.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            [
                sys.executable,
                str(EXP / "tools/convert_csv_to_gcare.py"),
                "-s",
                str(cfg["schema"]),
                "-d",
                str(cfg["dataset_dir"]),
                "-o",
                str(graph_text),
            ],
            check=True,
        )
        return
    export_manifest_graph(cfg["dataset_dir"], graph_text, force=force)


def prepare_queries(cfg: dict[str, Any], force: bool = False) -> None:
    query_dir = cfg["query_dir"]
    if force and query_dir.exists():
        shutil.rmtree(query_dir)
    query_dir.mkdir(parents=True, exist_ok=True)
    for group, root, kind in cfg["query_roots"]:
        if not root.exists():
            continue
        if kind == "text":
            paths = [p for p in root.rglob("*") if p.suffix in {".gcare", ".graph", ".txt"}]
            for src in sorted(paths, key=natural_key):
                rel = src.relative_to(root)
                if rel.parent == Path("."):
                    dst = query_dir / group / f"{src.stem}.txt"
                else:
                    dst = query_dir / group / rel.parent / f"{src.stem}.txt"
                normalize_text_graph(src, dst, query=True, force=force)
        elif kind == "gcard":
            for src in sorted(root.rglob("*.json"), key=natural_key):
                rel = src.relative_to(root)
                if rel.parent == Path("."):
                    dst = query_dir / group / f"{src.stem}.txt"
                else:
                    dst = query_dir / group / rel.parent / f"{src.stem}.txt"
                convert_gcard_pattern(src, dst, cfg["dataset_dir"], force=force)
        else:
            raise ValueError(kind)


def prepare(args: argparse.Namespace) -> None:
    cfg = dataset_config(args.dataset)
    prepare_graph(cfg, force=args.force)
    prepare_queries(cfg, force=args.force)

    values = {
        "ALLEY_DATASET": cfg["dataset"],
        "ALLEY_GRAPH_TEXT": str(cfg["graph_text"]),
        "ALLEY_DATA_HEADER": str(cfg["data_header"]),
        "ALLEY_QUERY_DIR": str(cfg["query_dir"]),
    }
    for key, value in values.items():
        print(f"{key}={shlex.quote(value)}")


def parse_estimates(args: argparse.Namespace) -> None:
    rows: list[dict[str, str]] = []
    raw = args.input
    err = Path(str(raw) + ".err")
    timeouts = Path(str(raw) + ".timeout")
    timeout_groups = set()
    if timeouts.exists():
        timeout_groups = {line.strip() for line in timeouts.read_text().splitlines() if line.strip()}
    error_paths = set()
    if err.exists():
        for line in err.read_text(errors="replace").splitlines():
            if " error " in line:
                error_paths.add(line.split(" error ", 1)[0].strip())

    with raw.open(encoding="utf-8", errors="replace") as f:
        for line in f:
            parts = line.strip().split()
            if len(parts) != 4:
                continue
            path = Path(parts[0])
            try:
                estimate = float(parts[1])
                latency_ms = float(parts[2])
                variance = float(parts[3])
            except ValueError:
                continue
            if not (math.isfinite(estimate) and math.isfinite(latency_ms) and math.isfinite(variance)):
                status = "nan"
            else:
                status = "ok"
            try:
                rel = path.relative_to(args.query_dir)
            except ValueError:
                rel = path
            rows.append(
                {
                    "method": args.method,
                    "dataset": args.dataset,
                    "query": rel.with_suffix("").as_posix(),
                    "estimate": f"{estimate:.12g}",
                    "latency_ms": f"{latency_ms:.12g}",
                    "latency_s": f"{latency_ms / 1000.0:.12g}",
                    "variance": f"{variance:.12g}",
                    "status": status,
                    "source": str(path),
                }
            )

    existing_sources = {row["source"] for row in rows}
    for path in sorted(error_paths - existing_sources):
        try:
            rel = Path(path).relative_to(args.query_dir).with_suffix("").as_posix()
        except ValueError:
            rel = Path(path).with_suffix("").as_posix()
        rows.append(
            {
                "method": args.method,
                "dataset": args.dataset,
                "query": rel,
                "estimate": "",
                "latency_ms": "",
                "latency_s": "",
                "variance": "",
                "status": "error",
                "source": path,
            }
        )

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", newline="", encoding="utf-8") as f:
        fieldnames = ["method", "dataset", "query", "estimate", "latency_ms", "latency_s", "variance", "status", "source"]
        writer = csv.DictWriter(f, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(rows)

    summary = args.output.with_name(args.output.stem + "_summary.csv")
    ok = [row for row in rows if row["status"] == "ok"]
    with summary.open("w", newline="", encoding="utf-8") as f:
        fieldnames = ["method", "dataset", "total_count", "ok_count", "error_count", "timeout_group_count", "mean_latency_s", "total_latency_s"]
        writer = csv.DictWriter(f, fieldnames=fieldnames)
        writer.writeheader()
        latencies = [float(row["latency_s"]) for row in ok if row["latency_s"]]
        writer.writerow(
            {
                "method": args.method,
                "dataset": args.dataset,
                "total_count": len(rows),
                "ok_count": len(ok),
                "error_count": len(rows) - len(ok),
                "timeout_group_count": len(timeout_groups),
                "mean_latency_s": f"{sum(latencies) / len(latencies):.12g}" if latencies else "",
                "total_latency_s": f"{sum(latencies):.12g}" if latencies else "",
            }
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("prepare")
    p.add_argument("--dataset", required=True)
    p.add_argument("--force", action="store_true")
    p.set_defaults(func=prepare)

    p = sub.add_parser("parse-estimates")
    p.add_argument("--input", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--query-dir", type=Path, required=True)
    p.add_argument("--dataset", required=True)
    p.add_argument("--method", required=True)
    p.set_defaults(func=parse_estimates)

    args = parser.parse_args()
    args.func(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
