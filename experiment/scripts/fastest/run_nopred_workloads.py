#!/usr/bin/env python3
"""Run FaSTest on no-predicate AIDS, JOB-light IMDB, and STATS workloads."""

from __future__ import annotations

import argparse
import csv
import json
import math
import os
import re
import selectors
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
EXPERIMENT = ROOT / "experiment"
FASTEST_DIR = EXPERIMENT / "baseline" / "FaSTest"
FASTEST_BIN = FASTEST_DIR / "build" / "Fastest"
DUCKDB = EXPERIMENT / "tools" / "duckdb"


WORKLOADS = {
    "aids": {
        "dataset_name": "aids_merged",
        "gcare_text": EXPERIMENT / "catalogs" / "aids_merged" / "gcare" / "aids_merged.txt",
        "pattern_root": EXPERIMENT / "patterns" / "gcare" / "aids_merged",
        "truth": EXPERIMENT / "result" / "truecard" / "aids_merged_current" / "true_card.csv",
    },
    "job": {
        "dataset_name": "job_light_imdb_unique",
        "dataset_dir": EXPERIMENT / "datasets" / "imdb" / "job_light_unique",
        "pattern_root": EXPERIMENT / "patterns" / "gcard" / "job_light_imdb_unique",
    },
    "stats": {
        "dataset_name": "stats_ceb",
        "dataset_dir": EXPERIMENT / "datasets" / "stats_ceb",
        "pattern_root": EXPERIMENT / "patterns" / "gcard" / "stats_ceb",
    },
    "dblp": {
        "dataset_name": "dblp",
        "graph": EXPERIMENT / "catalogs" / "dblp" / "fastest" / "dblp_undirected.graph",
        "pattern_root": EXPERIMENT / "patterns" / "gcare" / "dblp",
        "truth": EXPERIMENT / "result" / "truecard" / "dblp" / "true_card.csv",
    },
}


def natural_key(path: Path) -> list[int | str]:
    return [int(part) if part.isdigit() else part for part in re.split(r"(\d+)", path.as_posix())]


def sql_quote(value: Path | str) -> str:
    return "'" + str(value).replace("'", "''") + "'"


def run(cmd: list[str], *, timeout: int | None = None, stdout=None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        check=True,
        text=True,
        stdout=stdout if stdout is not None else subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )


def duckdb_scalar(db_path: Path, sql: str, *, timeout: int | None = None) -> str:
    proc = run(
        [str(DUCKDB), str(db_path), "-csv", "-noheader", "-c", sql],
        timeout=timeout,
    )
    return proc.stdout.strip().splitlines()[-1].strip()


def duckdb_exec(db_path: Path, sql: str, *, timeout: int | None = None) -> None:
    run([str(DUCKDB), str(db_path), "-batch", "-bail", "-c", sql], timeout=timeout)


def duckdb_copy_file(db_path: Path, sql: str, path: Path) -> None:
    with path.open("w") as out:
        run([str(DUCKDB), str(db_path), "-batch", "-bail", "-csv", "-noheader", "-c", sql], stdout=out)


def append_duckdb_copy(db_path: Path, sql: str, out, part_path: Path) -> None:
    duckdb_copy_file(db_path, sql, part_path)
    with part_path.open() as part:
        shutil.copyfileobj(part, out)
    part_path.unlink()


def build_fastest() -> None:
    if FASTEST_BIN.exists():
        return
    print("[fastest] building FaSTest", file=sys.stderr)
    (FASTEST_DIR / "build").mkdir(parents=True, exist_ok=True)
    subprocess.run(["cmake", ".."], cwd=FASTEST_DIR / "build", check=True)
    subprocess.run(["make", "-j"], cwd=FASTEST_DIR / "build", check=True)


def qerror(estimate: float, truth: float) -> float:
    if truth <= 0:
        return math.inf
    if estimate <= 0:
        return truth
    return max(estimate / truth, truth / estimate)


def build_label_maps(manifest: dict) -> tuple[dict[str, int], dict[str, int], dict[str, int]]:
    vertex_map = {v["label"].lower(): i for i, v in enumerate(manifest["vertices"])}
    edge_map = {e["label"].lower(): i for i, e in enumerate(manifest["edges"])}
    edge_to_id = {e["label"].lower(): i for i, e in enumerate(manifest["edges"])}
    return vertex_map, edge_map, edge_to_id


def load_manifest(dataset_dir: Path) -> dict:
    with (dataset_dir / "manifest.json").open() as f:
        return json.load(f)


def line_count(path: Path) -> int:
    with path.open("rb") as f:
        return max(0, sum(1 for _ in f) - 1)


def ensure_graph_db(dataset_dir: Path, manifest: dict, db_path: Path) -> dict[str, int]:
    """Create id maps and edge tables used by graph export and exact truth SQL."""
    marker = db_path.with_suffix(".ready")
    if marker.exists():
        counts = json.loads(marker.read_text())
        return {k: int(v) for k, v in counts.items()}

    db_path.parent.mkdir(parents=True, exist_ok=True)
    if db_path.exists():
        db_path.unlink()

    vertex_counts: dict[str, int] = {}
    offset = 0
    for i, vertex in enumerate(manifest["vertices"]):
        label = vertex["label"]
        csv_path = dataset_dir / vertex["file"]["path"]
        count = line_count(csv_path)
        vertex_counts[label] = count
        print(f"[fastest] indexing vertices {label}: {count}", file=sys.stderr)
        sql = (
            f"CREATE TABLE vm{i} AS "
            f"SELECT id, row_number() OVER (ORDER BY id) - 1 + {offset} AS new_id "
            f"FROM read_csv_auto({sql_quote(csv_path)}, header=true);"
        )
        duckdb_exec(db_path, sql)
        offset += count

    for i, edge in enumerate(manifest["edges"]):
        label = edge["label"]
        csv_path = dataset_dir / edge["file"]["path"]
        print(f"[fastest] importing edges {label}", file=sys.stderr)
        sql = (
            f"CREATE TABLE e{i} AS "
            f"SELECT src, dst FROM read_csv_auto({sql_quote(csv_path)}, header=true);"
        )
        duckdb_exec(db_path, sql)

    counts = {"__total_vertices__": offset}
    counts.update(vertex_counts)
    marker.write_text(json.dumps(counts, indent=2))
    return counts


def export_gcard_graph(dataset_name: str, dataset_dir: Path, manifest: dict, work_dir: Path) -> Path:
    dataset_out = work_dir / "dataset" / dataset_name
    graph_path = dataset_out / f"{dataset_name}.graph"
    if graph_path.exists():
        return graph_path

    dataset_out.mkdir(parents=True, exist_ok=True)
    db_path = work_dir / "duckdb" / f"{dataset_name}.duckdb"
    counts = ensure_graph_db(dataset_dir, manifest, db_path)
    total_vertices = counts["__total_vertices__"]
    total_edges = sum(line_count(dataset_dir / edge["file"]["path"]) for edge in manifest["edges"])

    tmp_path = graph_path.with_suffix(".graph.tmp")
    print(f"[fastest] exporting FaSTest graph {graph_path}", file=sys.stderr)
    with tmp_path.open("w") as out:
        out.write(f"t {total_vertices} {total_edges}\n")
        for i, _vertex in enumerate(manifest["vertices"]):
            sql = f"COPY (SELECT 'v', new_id, {i} FROM vm{i} ORDER BY new_id) TO STDOUT (DELIMITER ' ', HEADER false);"
            append_duckdb_copy(db_path, sql, out, tmp_path.with_suffix(f".v{i}.part"))
        for i, edge in enumerate(manifest["edges"]):
            src_label = next(j for j, v in enumerate(manifest["vertices"]) if v["label"] == edge["src_label"])
            dst_label = next(j for j, v in enumerate(manifest["vertices"]) if v["label"] == edge["dst_label"])
            sql = (
                "COPY ("
                f"SELECT 'e', s.new_id, d.new_id, {i} "
                f"FROM e{i} e "
                f"JOIN vm{src_label} s ON e.src = s.id "
                f"JOIN vm{dst_label} d ON e.dst = d.id"
                ") TO STDOUT (DELIMITER ' ', HEADER false);"
            )
            append_duckdb_copy(db_path, sql, out, tmp_path.with_suffix(f".e{i}.part"))
    tmp_path.replace(graph_path)
    return graph_path


def convert_gcare_graph_to_fastest(src: Path, dst: Path) -> None:
    vertices: list[list[str]] = []
    edges: list[list[str]] = []
    with src.open() as f:
        for line in f:
            parts = line.strip().split()
            if not parts:
                continue
            if parts[0] == "v":
                vertices.append(parts)
            elif parts[0] == "e":
                edges.append(parts)

    dst.parent.mkdir(parents=True, exist_ok=True)
    with dst.open("w") as f:
        f.write(f"t {len(vertices)} {len(edges)}\n")
        for parts in vertices:
            f.write(f"v {parts[1]} {parts[2]}\n")
        for parts in edges:
            label = parts[3] if len(parts) > 3 else "0"
            f.write(f"e {parts[1]} {parts[2]} {label}\n")


def collect_aids_patterns(
    cfg: dict, limit: int | None, query_filter: set[str] | None
) -> tuple[list[tuple[str, str, Path]], dict[tuple[str, str], str]]:
    patterns: list[tuple[str, str, Path]] = []
    truth: dict[tuple[str, str], str] = {}
    with cfg["truth"].open(newline="") as f:
        for row in csv.DictReader(f):
            if row.get("status") != "ok":
                continue
            rel = Path(row["pattern"])
            if query_filter is not None and rel.name not in query_filter and rel.as_posix() not in query_filter:
                continue
            key = (rel.parent.as_posix(), rel.name)
            pattern_path = cfg["pattern_root"] / rel.parent / f"{rel.name}.gcare"
            if not pattern_path.exists():
                continue
            truth[key] = str(int(float(row["true_cardinality"])))
            patterns.append((key[0], key[1], pattern_path))
            if limit is not None and len(patterns) >= limit:
                break
    return patterns, truth


def collect_gcard_patterns(
    pattern_root: Path, limit: int | None, query_filter: set[str] | None
) -> list[tuple[str, str, Path]]:
    paths = sorted(pattern_root.glob("*.json"), key=natural_key)
    if query_filter is not None:
        paths = [path for path in paths if path.stem in query_filter]
    if limit is not None:
        paths = paths[:limit]
    return [("gcard", path.stem, path) for path in paths]


def collect_flat_gcare_patterns(
    cfg: dict, limit: int | None, query_filter: set[str] | None
) -> tuple[list[tuple[str, str, Path]], dict[tuple[str, str], str]]:
    truth: dict[tuple[str, str], str] = {}
    with cfg["truth"].open(newline="") as f:
        for row in csv.DictReader(f):
            if row.get("status") and row.get("status") != "ok":
                continue
            query = row["query"]
            if query_filter is not None and query not in query_filter:
                continue
            card = row.get("true_cardinality", "")
            if card:
                truth[("gcare", query)] = str(int(float(card)))
    paths = sorted(cfg["pattern_root"].glob("*.graph"), key=natural_key)
    if query_filter is not None:
        paths = [path for path in paths if path.stem in query_filter]
    patterns = [("gcare", path.stem, path) for path in paths if ("gcare", path.stem) in truth]
    if limit is not None:
        patterns = patterns[:limit]
    return patterns, truth


def convert_gcard_query_to_fastest(src: Path, dst: Path, vertex_map: dict[str, int], edge_map: dict[str, int]) -> None:
    pattern = json.loads(src.read_text())
    id_map = {v["id"]: i for i, v in enumerate(pattern["vertices"])}
    dst.parent.mkdir(parents=True, exist_ok=True)
    with dst.open("w") as f:
        f.write(f"t {len(pattern['vertices'])} {len(pattern['edges'])}\n")
        for v in pattern["vertices"]:
            label_id = vertex_map[v["label"].lower()]
            f.write(f"v {id_map[v['id']]} {label_id}\n")
        for e in pattern["edges"]:
            label_id = edge_map[e["label"].lower()]
            f.write(f"e {id_map[e['src']]} {id_map[e['dst']]} {label_id}\n")


def truth_sql_for_pattern(pattern_path: Path, edge_map: dict[str, int], manifest: dict) -> str:
    pattern = json.loads(pattern_path.read_text())
    if not pattern["edges"]:
        raise ValueError(f"Pattern has no edges: {pattern_path}")

    froms: list[str] = []
    vertex_refs: dict[int, list[str]] = {}
    for edge in pattern["edges"]:
        edge_id = edge_map[edge["label"].lower()]
        alias = f"e{edge['id']}"
        froms.append(f"e{edge_id} AS {alias}")
        vertex_refs.setdefault(edge["src"], []).append(f"{alias}.src")
        vertex_refs.setdefault(edge["dst"], []).append(f"{alias}.dst")

    predicates: list[str] = []
    for refs in vertex_refs.values():
        first = refs[0]
        predicates.extend(f"{ref} = {first}" for ref in refs[1:])

    sql = "SELECT COUNT(*) FROM " + ", ".join(froms)
    if predicates:
        sql += " WHERE " + " AND ".join(predicates)
    return sql + ";"


def compute_gcard_truths(
    dataset_dir: Path,
    manifest: dict,
    patterns: list[tuple[str, str, Path]],
    work_dir: Path,
    timeout_s: int,
) -> dict[tuple[str, str], str]:
    dataset_name = dataset_dir.name
    db_path = work_dir / "duckdb" / f"{dataset_name}.duckdb"
    ensure_graph_db(dataset_dir, manifest, db_path)
    _vertex_map, edge_map, _edge_to_id = build_label_maps(manifest)

    out_path = work_dir / "truth_nopred.csv"
    done: dict[tuple[str, str], str] = {}
    if out_path.exists():
        with out_path.open(newline="") as f:
            for row in csv.DictReader(f):
                if row.get("status") == "ok":
                    done[(row["group"], row["query"])] = row["true_cardinality"]

    with out_path.open("a", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=["group", "query", "true_cardinality", "status", "error"])
        if out_path.stat().st_size == 0:
            writer.writeheader()
        for group, query, path in patterns:
            key = (group, query)
            if key in done:
                continue
            print(f"[fastest] exact no-pred truth {query}", file=sys.stderr)
            try:
                card = duckdb_scalar(db_path, truth_sql_for_pattern(path, edge_map, manifest), timeout=timeout_s)
                card = str(int(float(card)))
                done[key] = card
                writer.writerow({"group": group, "query": query, "true_cardinality": card, "status": "ok", "error": ""})
            except Exception as exc:  # noqa: BLE001 - persist failures per query.
                writer.writerow(
                    {"group": group, "query": query, "true_cardinality": "", "status": "error", "error": str(exc)}
                )
            f.flush()
    return done


def prepare_dataset_and_ans(
    workload: str,
    cfg: dict,
    out_dir: Path,
    limit: int | None,
    truth_timeout_s: int,
    query_filter: set[str] | None,
) -> tuple[str, list[tuple[str, str, Path]], dict[tuple[str, str], str], Path]:
    dataset_name = cfg["dataset_name"]
    work_dir = out_dir / "work"
    dataset_out = work_dir / "dataset" / dataset_name
    query_dir = dataset_out / "query_graph"
    query_dir.mkdir(parents=True, exist_ok=True)

    if workload == "aids":
        patterns, truth = collect_aids_patterns(cfg, limit, query_filter)
        graph_path = dataset_out / f"{dataset_name}.graph"
        if not graph_path.exists():
            convert_gcare_graph_to_fastest(cfg["gcare_text"], graph_path)
        for group, query, src in patterns:
            dst = query_dir / group / f"{query}.graph"
            if not dst.exists():
                convert_gcare_graph_to_fastest(src, dst)
    elif workload == "dblp":
        patterns, truth = collect_flat_gcare_patterns(cfg, limit, query_filter)
        graph_path = dataset_out / f"{dataset_name}.graph"
        if not graph_path.exists():
            shutil.copyfile(cfg["graph"], graph_path)
        for group, query, src in patterns:
            dst = query_dir / group / f"{query}.graph"
            if not dst.exists():
                convert_gcare_graph_to_fastest(src, dst)
    else:
        dataset_dir = cfg["dataset_dir"]
        manifest = load_manifest(dataset_dir)
        vertex_map, edge_map, _edge_to_id = build_label_maps(manifest)
        patterns = collect_gcard_patterns(cfg["pattern_root"], limit, query_filter)
        export_gcard_graph(dataset_name, dataset_dir, manifest, work_dir)
        truth = compute_gcard_truths(dataset_dir, manifest, patterns, work_dir, truth_timeout_s)
        for group, query, src in patterns:
            dst = query_dir / group / f"{query}.graph"
            if not dst.exists():
                convert_gcard_query_to_fastest(src, dst, vertex_map, edge_map)

    runnable = [(group, query, path) for group, query, path in patterns if (group, query) in truth]
    ans_path = dataset_out / f"{dataset_name}_ans.txt"
    with ans_path.open("w") as ans:
        for group, query, _src in runnable:
            rel = Path(group) / f"{query}.graph"
            ans.write(f"{rel.as_posix()} 0ms {truth[(group, query)]}\n")
    return dataset_name, runnable, truth, ans_path


def run_fastest_command(
    cmd: list[str],
    *,
    cwd: Path,
    query_timeout_s: int,
    load_timeout_s: int,
) -> tuple[int | None, str, str | None]:
    proc = subprocess.Popen(
        cmd,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        bufsize=1,
    )
    assert proc.stdout is not None
    selector = selectors.DefaultSelector()
    selector.register(proc.stdout, selectors.EVENT_READ)
    raw_parts: list[str] = []
    start_time = time.monotonic()
    query_start_time: float | None = None

    while True:
        events = selector.select(timeout=0.2)
        for key, _mask in events:
            line = key.fileobj.readline()
            if line == "":
                try:
                    selector.unregister(key.fileobj)
                except KeyError:
                    pass
                break
            raw_parts.append(line)
            if line.startswith("Start Processing ") and query_start_time is None:
                query_start_time = time.monotonic()

        rc = proc.poll()
        if rc is not None:
            remainder = proc.stdout.read()
            if remainder:
                raw_parts.append(remainder)
            return rc, "".join(raw_parts), None

        now = time.monotonic()
        timeout_reason = None
        if query_start_time is None and now - start_time > load_timeout_s:
            timeout_reason = "load_timeout"
        elif query_start_time is not None and now - query_start_time > query_timeout_s:
            timeout_reason = "query_timeout"

        if timeout_reason is not None:
            proc.kill()
            remainder = proc.stdout.read()
            if remainder:
                raw_parts.append(remainder)
            proc.wait()
            return None, "".join(raw_parts), timeout_reason


def parse_fastest_output(output: str) -> dict[tuple[str, str], dict[str, float | str]]:
    rows: dict[tuple[str, str], dict[str, float | str]] = {}
    current: tuple[str, str] | None = None
    number = r"([+-]?(?:nan|inf|(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?))"
    for line in output.splitlines():
        if line.startswith("Start Processing "):
            path = Path(line.split("Start Processing ", 1)[1].strip())
            current = (path.parent.name, path.stem)
            rows[current] = {"status": "ok"}
            continue
        if current is None:
            continue
        m = re.search(r"\[Result\]\s+([A-Za-z#]+)\s*:\s*" + number, line, flags=re.IGNORECASE)
        if not m:
            continue
        key, value = m.group(1), float(m.group(2))
        if key == "Est":
            rows[current]["estimate"] = value
        elif key == "QueryTime":
            rows[current]["latency_ms"] = value
    return rows


def ans_entries(ans_path: Path) -> list[tuple[Path, str]]:
    entries: list[tuple[Path, str]] = []
    for line in ans_path.read_text().splitlines():
        if not line.strip():
            continue
        parts = line.split()
        entries.append((Path(parts[0]), parts[2]))
    return entries


def run_fastest_per_query(
    dataset_name: str,
    ans_path: Path,
    work_dir: Path,
    structure: str,
    fastest_k: int,
    query_timeout_s: int,
    load_timeout_s: int,
) -> tuple[dict[tuple[str, str], dict[str, float | str]], str]:
    run_dir = work_dir / "run"
    ans_dir = run_dir / "single_ans"
    ans_dir.mkdir(parents=True, exist_ok=True)
    rows: dict[tuple[str, str], dict[str, float | str]] = {}
    raw_parts = ["[runner] FaSTest no-predicate per-query mode\n"]
    entries = ans_entries(ans_path)

    for i, (rel_query, truth_value) in enumerate(entries, start=1):
        print(f"[fastest] running {dataset_name}/{rel_query.as_posix()} ({i}/{len(entries)})", file=sys.stderr)
        key = (rel_query.parent.name, rel_query.stem)
        single_ans = ans_dir / f"{key[0]}__{key[1]}.txt"
        single_ans.write_text(f"{rel_query.as_posix()} 0ms {truth_value}\n")
        cmd = [
            str(FASTEST_BIN),
            "-d",
            dataset_name,
            "-q",
            os.path.relpath(single_ans, run_dir),
            "--STRUCTURE",
            structure,
            "-K",
            str(fastest_k),
        ]
        returncode, raw, timeout_reason = run_fastest_command(
            cmd,
            cwd=run_dir,
            query_timeout_s=query_timeout_s,
            load_timeout_s=load_timeout_s,
        )
        parsed = parse_fastest_output(raw)
        row = parsed.get(key, {})
        if timeout_reason is not None:
            status = "timeout" if timeout_reason == "query_timeout" else "load_timeout"
            row = {
                **row,
                "status": status,
                "estimate": float(row.get("estimate", 0.0)),
                "latency_s": float(query_timeout_s if status == "timeout" else load_timeout_s),
            }
            raw += f"\n[runner] FaSTest {status} for {rel_query.as_posix()}\n"
        elif returncode != 0:
            row = {**row, "status": "error", "estimate": float(row.get("estimate", 0.0)), "latency_s": 0.0}
            raw += f"\n[runner] FaSTest exit code {returncode} for {rel_query.as_posix()}\n"
        elif "estimate" not in row:
            row = {**row, "status": "error", "estimate": 0.0, "latency_s": 0.0}
        rows[key] = row
        raw_parts.append(f"\n[runner] === {rel_query.as_posix()} ===\n{raw}")
        if raw and not raw.endswith("\n"):
            raw_parts.append("\n")
    return rows, "".join(raw_parts)


def write_detail(
    path: Path,
    patterns: list[tuple[str, str, Path]],
    truth: dict[tuple[str, str], str],
    rows: dict[tuple[str, str], dict[str, float | str]],
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=["method", "group", "query", "true_cardinality", "estimate", "q_error", "latency_s", "status"],
        )
        writer.writeheader()
        for group, query, _path in patterns:
            key = (group, query)
            true_card = float(truth[key])
            row = rows.get(key, {"status": "missing", "estimate": 0.0, "latency_s": 0.0})
            estimate = float(row.get("estimate", 0.0))
            latency_s = float(row.get("latency_s", float(row.get("latency_ms", 0.0)) / 1000.0))
            status = str(row.get("status", "ok" if estimate > 0 else "error"))
            if status == "ok" and not math.isfinite(estimate):
                status = "nan_estimate"
            writer.writerow(
                {
                    "method": "fastest",
                    "group": group,
                    "query": query,
                    "true_cardinality": truth[key],
                    "estimate": f"{estimate:.6g}",
                    "q_error": f"{qerror(estimate, true_card):.6g}" if status == "ok" else "",
                    "latency_s": f"{latency_s:.6g}",
                    "status": status,
                }
            )

def summarize_detail(detail: Path, summary: Path) -> None:
    with detail.open(newline="") as f:
        rows = list(csv.DictReader(f))
    ok_rows = [r for r in rows if r["status"] == "ok" and r["q_error"]]
    timeout_count = sum(1 for r in rows if r["status"] == "timeout")
    load_timeout_count = sum(1 for r in rows if r["status"] == "load_timeout")
    error_count = sum(1 for r in rows if r["status"] in {"error", "nan_estimate"})
    missing_count = sum(1 for r in rows if r["status"] == "missing")
    latencies = [float(r["latency_s"]) for r in rows if r["latency_s"]]
    qerrs = [float(r["q_error"]) for r in ok_rows]

    with summary.open("w", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=[
                "method",
                "total_count",
                "ok_count",
                "timeout_count",
                "load_timeout_count",
                "error_count",
                "missing_count",
                "mean_qerror",
                "median_qerror",
                "max_qerror",
                "mean_latency_s",
                "median_latency_s",
                "total_latency_s",
                "accuracy_status",
            ],
        )
        writer.writeheader()
        writer.writerow(
            {
                "method": "fastest",
                "total_count": len(rows),
                "ok_count": len(ok_rows),
                "timeout_count": timeout_count,
                "load_timeout_count": load_timeout_count,
                "error_count": error_count,
                "missing_count": missing_count,
                "mean_qerror": f"{statistics.fmean(qerrs):.6g}" if qerrs else "",
                "median_qerror": f"{statistics.median(qerrs):.6g}" if qerrs else "",
                "max_qerror": f"{max(qerrs):.6g}" if qerrs else "",
                "mean_latency_s": f"{statistics.fmean(latencies):.6g}" if latencies else "",
                "median_latency_s": f"{statistics.median(latencies):.6g}" if latencies else "",
                "total_latency_s": f"{sum(latencies):.6g}" if latencies else "",
                "accuracy_status": "defined" if qerrs else "undefined",
            }
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--workload", choices=sorted(WORKLOADS), required=True)
    parser.add_argument("--out-dir", type=Path)
    parser.add_argument("--structure", default="X", choices=["X", "3", "4"])
    parser.add_argument("--fastest-k", type=int, default=100000)
    parser.add_argument("--query-timeout-s", type=int, default=60)
    parser.add_argument("--load-timeout-s", type=int, default=120)
    parser.add_argument("--truth-timeout-s", type=int, default=600)
    parser.add_argument("--max-queries", type=int)
    parser.add_argument("--query", action="append", help="Run only this query name; can be repeated")
    parser.add_argument("--no-build", action="store_true")
    args = parser.parse_args()

    if not args.no_build:
        build_fastest()

    cfg = WORKLOADS[args.workload]
    out_dir = args.out_dir or EXPERIMENT / "result" / "fastest" / f"{args.workload}_nopred_structure_{args.structure.lower()}"
    if FASTEST_DIR / "dataset" != out_dir / "work" / "dataset":
        dataset_link = FASTEST_DIR / "dataset"
        work_dataset = out_dir / "work" / "dataset"
        work_dataset.mkdir(parents=True, exist_ok=True)
        if dataset_link.exists() and not dataset_link.is_symlink():
            # FaSTest resolves ../dataset from the run directory in our layout, so this only matters
            # for direct runs from the upstream build tree.
            pass

    query_filter = set(args.query) if args.query else None
    dataset_name, patterns, truth, ans_path = prepare_dataset_and_ans(
        args.workload, cfg, out_dir, args.max_queries, args.truth_timeout_s, query_filter
    )
    if not patterns:
        raise RuntimeError(f"No runnable patterns for {args.workload}")

    fastest_rows, fastest_raw = run_fastest_per_query(
        dataset_name,
        ans_path,
        out_dir / "work",
        args.structure,
        args.fastest_k,
        args.query_timeout_s,
        args.load_timeout_s,
    )
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "fastest_raw.log").write_text(fastest_raw)
    detail = out_dir / "detail.csv"
    summary = out_dir / "summary.csv"
    write_detail(detail, patterns, truth, fastest_rows)
    summarize_detail(detail, summary)

    print(f"detail: {detail}")
    print(f"summary: {summary}")
    print(summary.read_text().strip())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
