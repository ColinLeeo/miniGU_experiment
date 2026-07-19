#!/usr/bin/env python3
"""Run FaSTest and G-CARE WJ on the LDBC SF1 LSQB/GLogS base queries.

This runner reuses the existing G-CARE graph/query format and converts only the
header shape needed by FaSTest.  Outputs are written under experiment/result.
"""

from __future__ import annotations

import argparse
import csv
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
LOCAL_GCARE_BIN = EXPERIMENT / "baseline" / "gcare" / "build" / "gcare_graph"
GCARE_BIN = Path(os.environ.get("GCARE_GRAPH_BIN", str(LOCAL_GCARE_BIN)))
SCHEMA = EXPERIMENT / "schemas" / "ldbc" / "ldbc_pathce_schema.json"
CONVERT_GCARE = EXPERIMENT / "tools" / "convert_csv_to_gcare.py"


def run(cmd: list[str], *, cwd: Path | None = None, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=cwd,
        check=check,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


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


def qerror(estimate: float, truth: float) -> float:
    if truth <= 0:
        return math.inf
    if estimate <= 0:
        return truth
    return max(estimate / truth, truth / estimate)


def read_truth(path: Path) -> dict[tuple[str, str], float]:
    truth: dict[tuple[str, str], float] = {}
    with path.open(newline="") as f:
        reader = csv.DictReader(f)
        for row in reader:
            group = row.get("group", "").strip()
            query = row.get("query", row.get("base", "")).strip()
            card = row.get("true_cardinality", "").strip()
            if group and query and card:
                truth[(group, query)] = float(card)
    return truth


def collect_patterns(pattern_root: Path, truth: dict[tuple[str, str], float]) -> list[tuple[str, str, Path]]:
    patterns: list[tuple[str, str, Path]] = []
    for group_dir in sorted(p for p in pattern_root.iterdir() if p.is_dir()):
        group = group_dir.name
        for path in sorted(group_dir.glob("*.gcare")):
            query = path.stem
            if (group, query) in truth:
                patterns.append((group, query, path))
    return patterns


def ensure_gcare_text(sf: str, dataset_dir: Path, gcare_text: Path) -> None:
    if gcare_text.exists():
        return
    gcare_text.parent.mkdir(parents=True, exist_ok=True)
    print(f"[fastest] generating G-CARE text graph: {gcare_text}", file=sys.stderr)
    subprocess.run(
        [
            sys.executable,
            str(CONVERT_GCARE),
            "-s",
            str(SCHEMA),
            "-d",
            str(dataset_dir),
            "-o",
            str(gcare_text),
        ],
        check=True,
    )


def build_fastest() -> None:
    if FASTEST_BIN.exists():
        return
    print("[fastest] building FaSTest", file=sys.stderr)
    (FASTEST_DIR / "build").mkdir(parents=True, exist_ok=True)
    subprocess.run(["cmake", ".."], cwd=FASTEST_DIR / "build", check=True)
    subprocess.run(["make", "-j"], cwd=FASTEST_DIR / "build", check=True)


def build_gcare_if_needed(dataset_dir: Path, gcare_text: Path) -> None:
    marker = Path(f"{dataset_dir}.graph.meta")
    if marker.exists():
        return
    if not GCARE_BIN.exists():
        raise FileNotFoundError(f"G-CARE binary not found: {GCARE_BIN}")
    print("[fastest] building G-CARE WJ graph", file=sys.stderr)
    subprocess.run(
        [str(GCARE_BIN), "-b", "-m", "wj", "-i", str(gcare_text), "-d", str(dataset_dir)],
        check=True,
    )


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


def convert_gcare_query_to_fastest(src: Path, dst: Path) -> None:
    convert_gcare_graph_to_fastest(src, dst)


def prepare_fastest_dataset(
    dataset_name: str,
    gcare_text: Path,
    patterns: list[tuple[str, str, Path]],
    truth: dict[tuple[str, str], float],
    work_dir: Path,
) -> Path:
    dataset_dir = work_dir / "dataset" / dataset_name
    query_dir = dataset_dir / "query_graph"
    query_dir.mkdir(parents=True, exist_ok=True)

    graph_path = dataset_dir / f"{dataset_name}.graph"
    if not graph_path.exists():
        convert_gcare_graph_to_fastest(gcare_text, graph_path)

    ans_path = dataset_dir / f"{dataset_name}_ans.txt"
    with ans_path.open("w") as ans:
        for group, query, pattern_path in patterns:
            rel = Path(group) / f"{query}.graph"
            out = query_dir / rel
            if not out.exists():
                convert_gcare_query_to_fastest(pattern_path, out)
            ans.write(f"{rel.as_posix()} 0ms {truth[(group, query)]:.0f}\n")
    return ans_path


def parse_fastest_output(output: str) -> dict[tuple[str, str], dict[str, float | str]]:
    rows: dict[tuple[str, str], dict[str, float | str]] = {}
    current: tuple[str, str] | None = None
    for line in output.splitlines():
        if line.startswith("Start Processing "):
            path = Path(line.split("Start Processing ", 1)[1].strip())
            query = path.stem
            group = path.parent.name
            current = (group, query)
            rows[current] = {"status": "ok"}
            continue
        if current is None:
            continue
        m = re.search(r"\[Result\]\s+([A-Za-z#]+)\s*:\s*([-+0-9.eE]+)", line)
        if not m:
            continue
        key, value = m.group(1), float(m.group(2))
        if key == "Est":
            rows[current]["estimate"] = value
        elif key == "Truth":
            rows[current]["true_cardinality"] = value
        elif key == "QueryTime":
            rows[current]["latency_ms"] = value
    return rows


def run_fastest(
    dataset_name: str,
    ans_path: Path,
    work_dir: Path,
    structure: str,
    fastest_k: int,
    timeout_s: int,
) -> tuple[dict[tuple[str, str], dict[str, float | str]], str]:
    run_dir = work_dir / "run"
    run_dir.mkdir(parents=True, exist_ok=True)
    cmd = [
        str(FASTEST_BIN),
        "-d",
        dataset_name,
        "-q",
        os.path.relpath(ans_path, run_dir),
        "--STRUCTURE",
        structure,
        "-K",
        str(fastest_k),
    ]
    try:
        proc = subprocess.run(
            cmd,
            cwd=run_dir,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout_s,
        )
        raw = proc.stdout + proc.stderr
        return parse_fastest_output(raw), raw
    except subprocess.TimeoutExpired as exc:
        out = exc.stdout or ""
        err = exc.stderr or ""
        if isinstance(out, bytes):
            out = out.decode(errors="replace")
        if isinstance(err, bytes):
            err = err.decode(errors="replace")
        raw = out + err
        rows = parse_fastest_output(raw)
        for line in ans_path.read_text().splitlines():
            if not line.strip():
                continue
            rel = Path(line.split()[0])
            key = (rel.parent.name, rel.stem)
            row = rows.get(key)
            if row is None or "estimate" not in row:
                rows[key] = {"status": "timeout", "estimate": 0.0, "latency_s": float(timeout_s)}
        raw += f"\n[runner] FaSTest timed out after {timeout_s}s\n"
        return rows, raw
    except subprocess.CalledProcessError as exc:
        raw = (exc.stdout or "") + (exc.stderr or "")
        rows = parse_fastest_output(raw)
        for line in ans_path.read_text().splitlines():
            if not line.strip():
                continue
            rel = Path(line.split()[0])
            rows.setdefault((rel.parent.name, rel.stem), {"status": "error", "estimate": 0.0})
        return rows, raw


def ans_entries(ans_path: Path) -> list[tuple[Path, str, float]]:
    entries: list[tuple[Path, str, float]] = []
    for line in ans_path.read_text().splitlines():
        if not line.strip():
            continue
        parts = line.split()
        if len(parts) < 3:
            continue
        entries.append((Path(parts[0]), parts[1], float(parts[2])))
    return entries


def run_fastest_one_query(
    dataset_name: str,
    rel_query: Path,
    truth_value: float,
    work_dir: Path,
    structure: str,
    fastest_k: int,
    timeout_s: int,
    load_timeout_s: int,
) -> tuple[tuple[str, str], dict[str, float | str], str]:
    run_dir = work_dir / "run"
    ans_dir = run_dir / "single_ans"
    ans_dir.mkdir(parents=True, exist_ok=True)

    key = (rel_query.parent.name, rel_query.stem)
    ans_path = ans_dir / f"{key[0]}__{key[1]}.txt"
    ans_path.write_text(f"{rel_query.as_posix()} 0ms {truth_value:.0f}\n")

    cmd = [
        str(FASTEST_BIN),
        "-d",
        dataset_name,
        "-q",
        os.path.relpath(ans_path, run_dir),
        "--STRUCTURE",
        structure,
        "-K",
        str(fastest_k),
    ]
    returncode, raw, timeout_reason = run_fastest_command(
        cmd,
        cwd=run_dir,
        query_timeout_s=timeout_s,
        load_timeout_s=load_timeout_s,
    )
    rows = parse_fastest_output(raw)
    row = rows.get(key, {})
    if timeout_reason is not None:
        if "estimate" in row:
            row = {**row, "status": "ok"}
            raw += f"\n[runner] FaSTest process timeout raced after result for {rel_query.as_posix()}\n"
            return key, row, raw
        if timeout_reason == "query_timeout":
            status = "timeout"
            limit = timeout_s
        else:
            status = "load_timeout"
            limit = load_timeout_s
        row = {**row, "status": status, "estimate": float(row.get("estimate", 0.0)), "latency_s": float(limit)}
        raw += f"\n[runner] FaSTest {status} after {limit}s for {rel_query.as_posix()}\n"
        return key, row, raw
    if returncode != 0:
        row = {**row, "status": "error", "estimate": float(row.get("estimate", 0.0))}
        raw += f"\n[runner] FaSTest failed with exit code {returncode} for {rel_query.as_posix()}\n"
        return key, row, raw
    row = rows.get(key, {"status": "error", "estimate": 0.0, "latency_s": 0.0})
    if "estimate" not in row:
        row = {**row, "status": "error", "estimate": 0.0}
    return key, row, raw


def run_fastest_per_query(
    dataset_name: str,
    ans_path: Path,
    work_dir: Path,
    structure: str,
    fastest_k: int,
    timeout_s: int,
    load_timeout_s: int,
) -> tuple[dict[tuple[str, str], dict[str, float | str]], str]:
    rows: dict[tuple[str, str], dict[str, float | str]] = {}
    raw_parts = ["[runner] FaSTest per-query mode\n"]
    entries = ans_entries(ans_path)
    for i, (rel_query, _truth_time, truth_value) in enumerate(entries, start=1):
        print(f"[fastest] running {rel_query.as_posix()} ({i}/{len(entries)})", file=sys.stderr)
        key, row, raw = run_fastest_one_query(
            dataset_name, rel_query, truth_value, work_dir, structure, fastest_k, timeout_s, load_timeout_s
        )
        rows[key] = row
        raw_parts.append(f"\n[runner] === {rel_query.as_posix()} ===\n")
        raw_parts.append(raw)
        if raw and not raw.endswith("\n"):
            raw_parts.append("\n")
    return rows, "".join(raw_parts)


def run_gcare_wj(
    patterns: list[tuple[str, str, Path]],
    dataset_dir: Path,
    repeat: int,
    seed: int,
) -> dict[tuple[str, str], dict[str, float | str]]:
    rows: dict[tuple[str, str], dict[str, float | str]] = {}
    for group, query, pattern_path in patterns:
        proc = run(
            [
                str(GCARE_BIN),
                "-q",
                "-m",
                "wj",
                "-n",
                str(repeat),
                "-s",
                str(seed),
                "-i",
                str(pattern_path),
                "-d",
                str(dataset_dir),
            ],
            check=False,
        )
        raw = proc.stdout.strip().splitlines()
        parsed = None
        for line in reversed(raw):
            if "," in line:
                left, right = line.split(",", 1)
                try:
                    parsed = (float(left.strip()), float(right.strip()))
                    break
                except ValueError:
                    pass
        if parsed is None:
            rows[(group, query)] = {"status": "error", "estimate": 0.0, "latency_s": 0.0}
        else:
            rows[(group, query)] = {
                "status": "ok",
                "estimate": parsed[0],
                "latency_s": parsed[1],
            }
    return rows


def write_detail(
    path: Path,
    patterns: list[tuple[str, str, Path]],
    truth: dict[tuple[str, str], float],
    fastest_rows: dict[tuple[str, str], dict[str, float | str]],
    gcare_rows: dict[tuple[str, str], dict[str, float | str]],
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=[
                "method",
                "group",
                "query",
                "true_cardinality",
                "estimate",
                "q_error",
                "latency_s",
                "status",
            ],
        )
        writer.writeheader()
        for method, source in [("fastest", fastest_rows), ("gcare-wj", gcare_rows)]:
            for group, query, _ in patterns:
                key = (group, query)
                true_card = truth[key]
                row = source.get(key, {"status": "missing", "estimate": 0.0, "latency_s": 0.0})
                estimate = float(row.get("estimate", 0.0))
                latency_s = float(row.get("latency_s", float(row.get("latency_ms", 0.0)) / 1000.0))
                status = str(row.get("status", "ok" if estimate > 0 else "error"))
                writer.writerow(
                    {
                        "method": method,
                        "group": group,
                        "query": query,
                        "true_cardinality": f"{true_card:.0f}",
                        "estimate": f"{estimate:.6g}",
                        "q_error": f"{qerror(estimate, true_card):.6g}" if status == "ok" else "",
                        "latency_s": f"{latency_s:.6g}",
                        "status": status,
                    }
                )


def summarize_detail(detail: Path, summary: Path) -> None:
    by_method: dict[str, list[dict[str, str]]] = {}
    with detail.open(newline="") as f:
        for row in csv.DictReader(f):
            by_method.setdefault(row["method"], []).append(row)

    with summary.open("w", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=[
                "method",
                "total_count",
                "ok_count",
                "timeout_count",
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
        for method, rows in sorted(by_method.items()):
            ok_rows = [r for r in rows if r["status"] == "ok" and r["q_error"]]
            timeout_count = sum(1 for r in rows if r["status"] == "timeout")
            error_count = sum(1 for r in rows if r["status"] == "error")
            missing_count = sum(1 for r in rows if r["status"] == "missing")
            latencies = [float(r["latency_s"]) for r in rows if r["latency_s"]]
            if ok_rows:
                qerrs = [float(r["q_error"]) for r in ok_rows]
                mean_qerror = f"{statistics.fmean(qerrs):.6g}"
                median_qerror = f"{statistics.median(qerrs):.6g}"
                max_qerror = f"{max(qerrs):.6g}"
                accuracy_status = "defined"
            else:
                mean_qerror = median_qerror = max_qerror = ""
                accuracy_status = "undefined"
            writer.writerow(
                {
                    "method": method,
                    "total_count": len(rows),
                    "ok_count": len(ok_rows),
                    "timeout_count": timeout_count,
                    "error_count": error_count,
                    "missing_count": missing_count,
                    "mean_qerror": mean_qerror,
                    "median_qerror": median_qerror,
                    "max_qerror": max_qerror,
                    "mean_latency_s": f"{statistics.fmean(latencies):.6g}" if latencies else "",
                    "median_latency_s": f"{statistics.median(latencies):.6g}" if latencies else "",
                    "total_latency_s": f"{sum(latencies):.6g}" if latencies else "",
                    "accuracy_status": accuracy_status,
                }
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sf", default="1")
    parser.add_argument("--pattern-root", type=Path, default=EXPERIMENT / "patterns" / "gcare")
    parser.add_argument(
        "--truth",
        type=Path,
        default=EXPERIMENT / "result" / "truecard" / "ldbc_sf1_unique_no_pred" / "true_card.csv",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=EXPERIMENT / "result" / "fastest" / "ldbc_sf1_lsqb_glogs_no_pred",
    )
    parser.add_argument("--structure", default="4", choices=["X", "3", "4"])
    parser.add_argument("--fastest-k", type=int, default=1000)
    parser.add_argument("--fastest-timeout-s", type=int, default=60)
    parser.add_argument("--fastest-load-timeout-s", type=int, default=120)
    parser.add_argument("--fastest-run-mode", choices=["per-query", "batch"], default="per-query")
    parser.add_argument("--gcare-repeat", type=int, default=1)
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument("--gcare-bin", type=Path, default=GCARE_BIN)
    parser.add_argument("--gcare-data-dir", type=Path)
    parser.add_argument("--gcare-text", type=Path)
    parser.add_argument("--no-build", action="store_true")
    args = parser.parse_args()

    globals()["GCARE_BIN"] = args.gcare_bin

    dataset_dir = EXPERIMENT / "datasets" / "ldbc" / f"sf{args.sf}"
    fallback_gcare_data_dir = EXPERIMENT / "catalogs" / "ldbc" / "gcare" / f"sf{args.sf}"
    if args.gcare_data_dir is not None:
        gcare_data_dir = args.gcare_data_dir
    elif Path(f"{fallback_gcare_data_dir}.graph.meta").exists():
        gcare_data_dir = fallback_gcare_data_dir
    else:
        gcare_data_dir = dataset_dir
    gcare_text = args.gcare_text or EXPERIMENT / "catalogs" / "ldbc" / "gcare" / f"sf{args.sf}.txt"
    dataset_name = f"ldbc_sf{args.sf}"
    work_dir = args.out_dir / "work"

    truth = read_truth(args.truth)
    patterns = collect_patterns(args.pattern_root, truth)
    if not patterns:
        raise RuntimeError(f"No matching G-CARE patterns found under {args.pattern_root}")

    if not args.no_build:
        build_fastest()
    ensure_gcare_text(args.sf, dataset_dir, gcare_text)
    build_gcare_if_needed(gcare_data_dir, gcare_text)

    ans_path = prepare_fastest_dataset(dataset_name, gcare_text, patterns, truth, work_dir)
    if args.fastest_run_mode == "batch":
        fastest_rows, fastest_raw = run_fastest(
            dataset_name, ans_path, work_dir, args.structure, args.fastest_k, args.fastest_timeout_s
        )
    else:
        fastest_rows, fastest_raw = run_fastest_per_query(
            dataset_name,
            ans_path,
            work_dir,
            args.structure,
            args.fastest_k,
            args.fastest_timeout_s,
            args.fastest_load_timeout_s,
        )
    args.out_dir.mkdir(parents=True, exist_ok=True)
    (args.out_dir / "fastest_raw.log").write_text(fastest_raw)

    gcare_rows = run_gcare_wj(patterns, gcare_data_dir, args.gcare_repeat, args.seed)

    detail = args.out_dir / "detail.csv"
    summary = args.out_dir / "summary.csv"
    write_detail(detail, patterns, truth, fastest_rows, gcare_rows)
    summarize_detail(detail, summary)

    print(f"detail: {detail}")
    print(f"summary: {summary}")
    with summary.open() as f:
        print(f.read().strip())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
