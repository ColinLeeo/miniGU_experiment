#!/usr/bin/env python3
"""Run XDB/WanderJoin with intermediate reports and pick first rel_ci crossing."""

from __future__ import annotations

import argparse
import csv
import math
import time
from dataclasses import dataclass
from pathlib import Path

from xdb_wanderjoin_common import (
    DEFAULT_DB,
    DEFAULT_OUT,
    DEFAULT_HOST,
    DEFAULT_PORT,
    DEFAULT_PSQL,
    DEFAULT_USER,
    DatasetConfig,
    load_workload,
    qerror,
    run_psql,
    to_online_sql,
)


@dataclass
class OnlineReport:
    time_ms: float
    nsamples: int
    nrejected: int
    estimate: float
    rel_ci: float


def parse_float(value: str) -> float:
    value = value.strip()
    return math.nan if value.lower() == "nan" else float(value)


def parse_online_reports(output: str) -> list[OnlineReport]:
    reports: list[OnlineReport] = []
    for line in output.splitlines():
        line = line.strip()
        if not line:
            continue
        fields = line.split("\t")
        if len(fields) < 5:
            continue
        reports.append(
            OnlineReport(
                time_ms=float(fields[0]),
                nsamples=int(fields[1]),
                nrejected=int(fields[2]),
                estimate=parse_float(fields[3]),
                rel_ci=parse_float(fields[4]),
            )
        )
    if not reports:
        raise ValueError("empty ONLINE output")
    return reports


def report_reaches_target(report: OnlineReport, args: argparse.Namespace) -> bool:
    if report.nsamples < args.target_min_samples:
        return False
    if not math.isfinite(report.rel_ci):
        return False
    if args.allow_zero_rel_ci:
        return report.rel_ci < args.target_rel_ci
    return 0.0 < report.rel_ci < args.target_rel_ci


def target_label(value: float) -> str:
    return (f"{value:g}").replace(".", "p")


def output_paths(cfg: DatasetConfig, args: argparse.Namespace) -> tuple[Path, Path]:
    suffix = (
        f"{args.withtime_ms}ms_ri{args.report_interval_ms}ms_"
        f"relci{target_label(args.target_rel_ci)}"
    )
    if args.target_min_samples:
        suffix += f"_mins{args.target_min_samples}"
    if args.allow_zero_rel_ci:
        suffix += "_allowzero"
    result_path = args.out_dir / f"{cfg.name}_with_pred_xdb_wanderjoin_{suffix}_qerror_latency_long.csv"
    summary_path = args.out_dir / f"{cfg.name}_with_pred_xdb_wanderjoin_{suffix}_summary.csv"
    return result_path, summary_path


def make_online_sql(sql: str, cfg: DatasetConfig, args: argparse.Namespace) -> str:
    online_sql = to_online_sql(sql, cfg, args.withtime_ms, args.confidence)
    return online_sql.replace(
        f"REPORTINTERVAL {args.withtime_ms}",
        f"REPORTINTERVAL {args.report_interval_ms}",
    )


def percentile(values: list[float], p: float) -> float:
    if not values:
        return float("nan")
    xs = sorted(values)
    if len(xs) == 1:
        return xs[0]
    pos = (len(xs) - 1) * p / 100.0
    lo = math.floor(pos)
    hi = math.ceil(pos)
    if lo == hi:
        return xs[lo]
    frac = pos - lo
    return xs[lo] * (1 - frac) + xs[hi] * frac


def run_relci_dataset(cfg: DatasetConfig, args: argparse.Namespace) -> Path:
    args.out_dir.mkdir(parents=True, exist_ok=True)
    online_dir = args.out_dir / "online_sql" / cfg.name / f"relci{target_label(args.target_rel_ci)}"
    online_dir.mkdir(parents=True, exist_ok=True)
    result_path, summary_path = output_paths(cfg, args)

    fields = [
        "dataset",
        "query",
        "method",
        "true_cardinality",
        "estimate",
        "qerror",
        "latency_s",
        "wall_latency_s",
        "nsamples",
        "nrejected",
        "rel_ci",
        "withtime_ms",
        "report_interval_ms",
        "target_rel_ci",
        "target_min_samples",
        "allow_zero_rel_ci",
        "nreports",
        "reached_target",
        "status",
        "source_sql",
        "notes",
    ]

    rows: list[dict[str, object]] = []
    workload = load_workload(cfg)
    with result_path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields)
        writer.writeheader()
        for idx, (query, sql_path, true_card) in enumerate(workload, 1):
            online_sql = make_online_sql(sql_path.read_text(encoding="utf-8"), cfg, args)
            (online_dir / f"{query}.sql").write_text(online_sql, encoding="utf-8")
            started = time.perf_counter()
            status = "ok_rel_ci"
            notes = ""
            reached = False
            chosen = OnlineReport(math.nan, 0, 0, math.nan, math.nan)
            nreports = 0
            try:
                output = run_psql(args, online_sql, tuples_only=True)
                reports = parse_online_reports(output)
                nreports = len(reports)
                chosen = reports[-1]
                for report in reports:
                    if report_reaches_target(report, args):
                        chosen = report
                        reached = True
                        break
                if not reached:
                    status = "not_reached_rel_ci"
            except Exception as exc:  # noqa: BLE001
                status = "error"
                notes = str(exc).replace("\n", " ")[:500]

            row = {
                "dataset": cfg.name,
                "query": query,
                "method": "XDB-WanderJoin-relci",
                "true_cardinality": true_card,
                "estimate": chosen.estimate,
                "qerror": qerror(true_card, chosen.estimate),
                "latency_s": chosen.time_ms / 1000.0 if math.isfinite(chosen.time_ms) else math.nan,
                "wall_latency_s": time.perf_counter() - started,
                "nsamples": chosen.nsamples,
                "nrejected": chosen.nrejected,
                "rel_ci": chosen.rel_ci,
                "withtime_ms": args.withtime_ms,
                "report_interval_ms": args.report_interval_ms,
                "target_rel_ci": args.target_rel_ci,
                "target_min_samples": args.target_min_samples,
                "allow_zero_rel_ci": args.allow_zero_rel_ci,
                "nreports": nreports,
                "reached_target": reached,
                "status": status,
                "source_sql": str(sql_path),
                "notes": notes,
            }
            writer.writerow(row)
            rows.append(row)
            handle.flush()
            print(
                f"[{cfg.name}] {idx}/{len(workload)} {query} {status} "
                f"lat={row['latency_s']} rel_ci={row['rel_ci']} qerr={row['qerror']}",
                flush=True,
            )

    ok = [r for r in rows if r["status"] in {"ok_rel_ci", "not_reached_rel_ci"}]
    reached_rows = [r for r in rows if r["status"] == "ok_rel_ci"]
    lats = [float(r["latency_s"]) for r in reached_rows if math.isfinite(float(r["latency_s"]))]
    qerrs = [float(r["qerror"]) for r in reached_rows if math.isfinite(float(r["qerror"]))]
    all_lats = [float(r["latency_s"]) for r in ok if math.isfinite(float(r["latency_s"]))]
    all_qerrs = [float(r["qerror"]) for r in ok if math.isfinite(float(r["qerror"]))]
    with summary_path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(
            handle,
            fieldnames=[
                "dataset",
                "n",
                "reached",
                "not_reached",
                "errors",
                "target_rel_ci",
                "target_min_samples",
                "allow_zero_rel_ci",
                "withtime_ms",
                "report_interval_ms",
                "reached_latency_median_s",
                "reached_latency_p95_s",
                "reached_qerror_median",
                "reached_qerror_p95",
                "all_chosen_latency_median_s",
                "all_chosen_qerror_median",
            ],
        )
        writer.writeheader()
        writer.writerow(
            {
                "dataset": cfg.name,
                "n": len(rows),
                "reached": len(reached_rows),
                "not_reached": sum(1 for r in rows if r["status"] == "not_reached_rel_ci"),
                "errors": sum(1 for r in rows if r["status"] == "error"),
                "target_rel_ci": args.target_rel_ci,
                "target_min_samples": args.target_min_samples,
                "allow_zero_rel_ci": args.allow_zero_rel_ci,
                "withtime_ms": args.withtime_ms,
                "report_interval_ms": args.report_interval_ms,
                "reached_latency_median_s": percentile(lats, 50),
                "reached_latency_p95_s": percentile(lats, 95),
                "reached_qerror_median": percentile(qerrs, 50),
                "reached_qerror_p95": percentile(qerrs, 95),
                "all_chosen_latency_median_s": percentile(all_lats, 50),
                "all_chosen_qerror_median": percentile(all_qerrs, 50),
            }
        )
    print(f"wrote {result_path}", flush=True)
    print(f"wrote {summary_path}", flush=True)
    return result_path


def build_relci_arg_parser(description: str) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=description)
    parser.add_argument("--psql", type=Path, default=DEFAULT_PSQL)
    parser.add_argument("--host", default=DEFAULT_HOST)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    parser.add_argument("--user", default=DEFAULT_USER)
    parser.add_argument("--db", default=DEFAULT_DB)
    parser.add_argument("--out-dir", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--withtime-ms", type=int, default=1000)
    parser.add_argument("--report-interval-ms", type=int, default=50)
    parser.add_argument("--confidence", type=int, default=95)
    parser.add_argument("--target-rel-ci", type=float, default=0.1)
    parser.add_argument("--target-min-samples", type=int, default=0)
    parser.add_argument("--allow-zero-rel-ci", action="store_true")
    return parser
