#!/usr/bin/env python3
"""Run SF1 GCard catalog thread scaling with build-phase RSS deltas.

The memory metric intentionally excludes already-loaded input/FlatGraph memory:
RSS at the first `compute_threads=` line is used as the baseline, and the peak
before `GCard build excl input load time` is reported as the build delta.
"""

from __future__ import annotations

import csv
import os
import re
import subprocess
import sys
import time
from pathlib import Path


ROOT = Path("/home/zxz/miniGU_experiment_gcard-alley-sampling")
MINIGU = Path(os.environ.get("MINIGU_BIN", ROOT / "target/release/minigu"))
RESULT_DIR = ROOT / "experiment/result/scalability"
INPUT_DB = RESULT_DIR / "input_db"
LOG_DIR = RESULT_DIR / "logs"
GQL_DIR = RESULT_DIR / "gql"
GRAPH = "ldbc_sf1_thread_scalability"
MAX_K = 2
THREADS = [1, 8, 16, 24, 32]


PATTERNS = {
    "scan_time_s": re.compile(r"^Scan time: ([0-9.]+)s"),
    "scan_cache_mb": re.compile(r"^ScannedHops cache memory estimate: ([0-9.]+) MB"),
    "remap_time_s": re.compile(r"GCard_scan_remap_summary: .* remap_time=([0-9.]+)s"),
    "unique_remaps": re.compile(r"GCard_scan_remap_summary: .* unique_remaps=([0-9]+)"),
    "compute_time_s": re.compile(r"^Compute time: ([0-9.]+)s"),
    "catalog_build_time_s": re.compile(r"^Catalog build time: ([0-9.]+)s"),
    "statistic_pipeline_time_s": re.compile(r"^Statistic pipeline time: ([0-9.]+)s"),
    "build_excl_input_load_time_s": re.compile(
        r"^GCard build excl input load time: ([0-9.]+)s"
    ),
}


def rss_kb(pid: int) -> int | None:
    try:
        with open(f"/proc/{pid}/status", "r", encoding="utf-8") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except FileNotFoundError:
        return None
    return None


def write_gql(thread: int) -> Path:
    GQL_DIR.mkdir(parents=True, exist_ok=True)
    path = GQL_DIR / f"create_catalog_t{thread}_memory.gql"
    path.write_text(
        "\n".join(
            [
                f"session set graph {GRAPH}",
                f'call create_catalog("{GRAPH}", {MAX_K}, {thread})',
                ":quit",
                "",
            ]
        ),
        encoding="utf-8",
    )
    return path


def parse_metrics(text: str) -> dict[str, str]:
    metrics: dict[str, str] = {}
    for line in text.splitlines():
        for key, pat in PATTERNS.items():
            match = pat.search(line)
            if match:
                metrics[key] = match.group(1)
    return metrics


def run_one(thread: int) -> dict[str, object]:
    gql = write_gql(thread)
    log_path = LOG_DIR / f"create_catalog_sf1_t{thread}_memory_remap_reuse.log"
    LOG_DIR.mkdir(parents=True, exist_ok=True)

    env = os.environ.copy()
    env["GCARD_SAVE_STATISTIC"] = "0"
    env["GCARD_SCAN_HOP_PARALLELISM"] = str(thread)

    proc = subprocess.Popen(
        [str(MINIGU), "execute", str(gql), "--path", str(INPUT_DB)],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        env=env,
    )

    output: list[str] = []
    baseline = None
    peak = None
    samples = 0
    in_build = False

    assert proc.stdout is not None
    with log_path.open("w", encoding="utf-8") as log:
        while True:
            line = proc.stdout.readline()
            if line:
                output.append(line)
                log.write(line)
                log.flush()
                if "compute_threads=" in line and baseline is None:
                    baseline = rss_kb(proc.pid)
                    peak = baseline
                    in_build = True
                if "GCard build excl input load time" in line:
                    in_build = False
            elif proc.poll() is not None:
                break

            if in_build:
                current = rss_kb(proc.pid)
                if current is not None:
                    peak = current if peak is None else max(peak, current)
                    samples += 1
                time.sleep(0.02)

        rest = proc.stdout.read()
        if rest:
            output.append(rest)
            log.write(rest)

    return_code = proc.wait()
    text = "".join(output)
    metrics = parse_metrics(text)

    delta = None
    if baseline is not None and peak is not None:
        delta = max(0, peak - baseline)

    row: dict[str, object] = {
        "thread": thread,
        "return_code": return_code,
        "baseline_rss_kb": baseline if baseline is not None else "",
        "build_peak_rss_kb": peak if peak is not None else "",
        "delta_peak_rss_kb": delta if delta is not None else "",
        "delta_peak_rss_mb": round(delta / 1024, 2) if delta is not None else "",
        "delta_peak_rss_gib": round(delta / 1024 / 1024, 3) if delta is not None else "",
        "samples": samples,
        "log": str(log_path),
    }
    row.update(metrics)
    return row


def main() -> int:
    if not MINIGU.exists():
        print(f"missing minigu binary: {MINIGU}", file=sys.stderr)
        return 1
    if not (INPUT_DB / f"{GRAPH}.flatgraph.bin").exists():
        print(f"missing prepared input db under {INPUT_DB}", file=sys.stderr)
        return 1

    out = RESULT_DIR / "sf1_thread_memory_remap_reuse.csv"
    fields = [
        "thread",
        "return_code",
        "baseline_rss_kb",
        "build_peak_rss_kb",
        "delta_peak_rss_kb",
        "delta_peak_rss_mb",
        "delta_peak_rss_gib",
        "samples",
        "scan_time_s",
        "scan_cache_mb",
        "remap_time_s",
        "unique_remaps",
        "compute_time_s",
        "catalog_build_time_s",
        "statistic_pipeline_time_s",
        "build_excl_input_load_time_s",
        "log",
    ]

    rows = []
    for thread in THREADS:
        print(f"running thread={thread}", flush=True)
        row = run_one(thread)
        rows.append(row)
        print(
            "  build={build}s scan={scan}s mem_delta={mem}MB rc={rc}".format(
                build=row.get("build_excl_input_load_time_s", ""),
                scan=row.get("scan_time_s", ""),
                mem=row.get("delta_peak_rss_mb", ""),
                rc=row.get("return_code", ""),
            ),
            flush=True,
        )

        with out.open("w", newline="", encoding="utf-8") as f:
            writer = csv.DictWriter(f, fieldnames=fields)
            writer.writeheader()
            writer.writerows(rows)

    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
