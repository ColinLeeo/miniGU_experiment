#!/usr/bin/env python3
import argparse
import csv
import json
import math
import re
from pathlib import Path


PROFILE_RE = re.compile(
    r"total:\s*(?P<total>\d+).*?"
    r"sample_time:\s*(?P<sample>[0-9.eE+-]+),\s*"
    r"estimate_time:\s*(?P<estimate_time>[0-9.eE+-]+),\s*"
    r"build_time:\s*(?P<build_time>[0-9.eE+-]+),\s*"
    r"(?P<query>[^,\r\n]+),\s*cardinality:\s*(?P<cardinality>[0-9.eE+-]+)"
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--guided", type=Path, required=True)
    parser.add_argument("--truth", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    return parser.parse_args()


def read_profiles(path: Path) -> dict[str, dict[str, float]]:
    profiles: dict[str, dict[str, float]] = {}
    for match in PROFILE_RE.finditer(path.read_text(encoding="utf-8")):
        query_id = match.group("query").strip()
        if query_id in profiles:
            raise ValueError(f"duplicate profile for {query_id} in {path}")
        profiles[query_id] = {
            "estimate": float(match.group("cardinality")),
            "latency_ms": (
                float(match.group("build_time"))
                + float(match.group("estimate_time"))
            )
            * 1000.0,
        }
    return profiles


def read_truth(path: Path) -> dict[str, float]:
    with path.open(newline="", encoding="utf-8") as handle:
        return {
            row["query_id"]: float(row["true_cardinality"])
            for row in csv.DictReader(handle)
        }


def qerror(estimate: float, truth: float) -> float:
    estimate = max(estimate, 1.0)
    truth = max(truth, 1.0)
    return max(estimate / truth, truth / estimate)


def percentile(values: list[float], quantile: float) -> float:
    ordered = sorted(values)
    position = (len(ordered) - 1) * quantile
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    fraction = position - lower
    return ordered[lower] * (1.0 - fraction) + ordered[upper] * fraction


def geomean(values: list[float]) -> float:
    return math.exp(sum(math.log(value) for value in values) / len(values))


def summarize(rows: list[dict[str, float]], prefix: str) -> dict[str, object]:
    qerrors = [row[f"{prefix}_qerror"] for row in rows]
    latencies = [row[f"{prefix}_latency_ms"] for row in rows]
    return {
        "count": len(rows),
        "qerror": {
            "p50": percentile(qerrors, 0.50),
            "geomean": geomean(qerrors),
            "p90": percentile(qerrors, 0.90),
            "p95": percentile(qerrors, 0.95),
            "p99": percentile(qerrors, 0.99),
            "max": max(qerrors),
            "le2": sum(value <= 2.0 for value in qerrors),
            "le10": sum(value <= 10.0 for value in qerrors),
        },
        "latency_ms": {
            "p50": percentile(latencies, 0.50),
            "geomean": geomean([max(value, 1e-9) for value in latencies]),
            "p95": percentile(latencies, 0.95),
            "p99": percentile(latencies, 0.99),
            "max": max(latencies),
        },
    }


def family(query_id: str) -> str:
    return re.sub(r"_\d+$", "", query_id)


def paired_summary(rows: list[dict[str, float]]) -> dict[str, object]:
    ratios = [row["qerror_ratio"] for row in rows]
    improved = sum(ratio < 1.0 and not math.isclose(ratio, 1.0) for ratio in ratios)
    equal = sum(math.isclose(ratio, 1.0) for ratio in ratios)
    worse = len(rows) - improved - equal
    return {
        "qerror_improved": improved,
        "qerror_equal": equal,
        "qerror_worse": worse,
        "estimate_changed": sum(
            row["baseline_estimate"] != row["guided_estimate"] for row in rows
        ),
        "median_qerror_ratio": percentile(ratios, 0.50),
        "geomean_qerror_ratio": geomean(ratios),
        "median_latency_ratio": percentile(
            [row["latency_ratio"] for row in rows], 0.50
        ),
    }


def compact_row(row: dict[str, float]) -> dict[str, object]:
    return {
        "query_id": row["query_id"],
        "qerror_ratio": row["qerror_ratio"],
        "baseline_qerror": row["baseline_qerror"],
        "guided_qerror": row["guided_qerror"],
        "baseline_estimate": row["baseline_estimate"],
        "guided_estimate": row["guided_estimate"],
        "true_cardinality": row["true_cardinality"],
        "latency_ratio": row["latency_ratio"],
    }


def main() -> int:
    args = parse_args()
    truth = read_truth(args.truth)
    baseline = read_profiles(args.baseline)
    guided = read_profiles(args.guided)
    missing_baseline = sorted(set(truth) - set(baseline))
    missing_guided = sorted(set(truth) - set(guided))
    if missing_baseline or missing_guided:
        raise ValueError(
            f"missing profiles: baseline={missing_baseline}, guided={missing_guided}"
        )

    rows: list[dict[str, float]] = []
    for query_id, true_cardinality in truth.items():
        baseline_profile = baseline[query_id]
        guided_profile = guided[query_id]
        baseline_qerror = qerror(baseline_profile["estimate"], true_cardinality)
        guided_qerror = qerror(guided_profile["estimate"], true_cardinality)
        rows.append(
            {
                "query_id": query_id,
                "family": family(query_id),
                "true_cardinality": true_cardinality,
                "baseline_estimate": baseline_profile["estimate"],
                "guided_estimate": guided_profile["estimate"],
                "baseline_qerror": baseline_qerror,
                "guided_qerror": guided_qerror,
                "qerror_ratio": guided_qerror / baseline_qerror,
                "baseline_latency_ms": baseline_profile["latency_ms"],
                "guided_latency_ms": guided_profile["latency_ms"],
                "latency_ratio": guided_profile["latency_ms"]
                / max(baseline_profile["latency_ms"], 1e-9),
            }
        )

    by_family: dict[str, dict[str, object]] = {}
    for name in sorted({row["family"] for row in rows}):
        family_rows = [row for row in rows if row["family"] == name]
        by_family[name] = {
            "baseline": summarize(family_rows, "baseline"),
            "guided": summarize(family_rows, "guided"),
            "paired": paired_summary(family_rows),
        }

    summary = {
        "baseline": summarize(rows, "baseline"),
        "guided": summarize(rows, "guided"),
        "paired": paired_summary(rows),
        "by_family": by_family,
        "top_improvements": [
            compact_row(row) for row in sorted(rows, key=lambda row: row["qerror_ratio"])[:10]
        ],
        "top_regressions": [
            compact_row(row)
            for row in sorted(rows, key=lambda row: row["qerror_ratio"], reverse=True)[:10]
        ],
    }

    args.output_dir.mkdir(parents=True, exist_ok=True)
    with (args.output_dir / "comparison.csv").open(
        "w", newline="", encoding="utf-8"
    ) as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    (args.output_dir / "summary.json").write_text(
        json.dumps(summary, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
