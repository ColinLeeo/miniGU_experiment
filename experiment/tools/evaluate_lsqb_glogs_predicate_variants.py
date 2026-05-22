#!/usr/bin/env python3
import argparse
import csv
import math
import re
import shutil
import subprocess
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MINIGU = ROOT / "target/release/minigu"
DEFAULT_DB = ROOT / "experiment/run_tmp/gcard_master_k2_arm1_d5_current_catalogs_retest/dbs/ldbc"
DEFAULT_GRAPH = "ldbc_sf1_retest_k2_arm1_d5_after_offset_fix"
DEFAULT_TRUTH = ROOT / "experiment/result/truecard/ldbc_sf1_unique_with_pred_10/truth.csv"
DEFAULT_OUT = ROOT / "experiment/result/gcard_query/lsqb_glogs_with_pred_10_master_predicate_modes"
DEFAULT_RUN_DB = ROOT / "experiment/run_tmp/lsqb_glogs_with_pred_10_master_predicate_modes/db"
DEFAULT_CATALOG = DEFAULT_DB / "catalog.json"
DEFAULT_FLATGRAPH = (
    ROOT
    / "experiment/run_tmp/master_flatgraph_ldbc/ldbc_sf1_retest_k2_arm1_d5_after_offset_fix.flatgraph.bin"
)
DEFAULT_STATISTIC = (
    ROOT
    / "experiment/run_tmp/gcard_master_k2_arm1_d5_current_catalogs_retest/dbs/ldbc/ldbc_sf1_retest_k2_arm1_d5_after_offset_fix.statistic.bin"
)

CARD_RE = re.compile(r",\s*([^,\s]+),\s*cardinality:\s*([0-9.eE+-]+)")
TIME_RE = re.compile(r"Time:\s*([0-9.]+)s")
MODE_NAME = {0: "inner", 1: "scale"}


def read_truth(path: Path):
    rows = []
    with path.open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            rows.append(
                {
                    "group": row["group"],
                    "base": row["base"],
                    "variant": row["variant"],
                    "predicate_count": int(row["predicate_count"]),
                    "true_cardinality": float(row["true_cardinality"]),
                    "gcard_path": row["gcard_path"],
                }
            )
    return rows


def prepare_db(args):
    args.db.mkdir(parents=True, exist_ok=True)
    catalog_dest = args.db / "catalog.json"
    if args.catalog != catalog_dest:
        shutil.copy2(args.catalog, catalog_dest)
    for suffix, source in [
        ("flatgraph.bin", args.flatgraph),
        ("statistic.bin", args.statistic),
    ]:
        dest = args.db / f"{args.graph}.{suffix}"
        if dest.exists() or dest.is_symlink():
            dest.unlink()
        dest.symlink_to(source)


def qerror(estimate: float, truth: float) -> float:
    estimate = max(estimate, 1.0)
    truth = max(truth, 1.0)
    return max(estimate / truth, truth / estimate)


def median(values):
    values = sorted(values)
    n = len(values)
    if n == 0:
        return math.nan
    mid = n // 2
    if n % 2:
        return values[mid]
    return (values[mid - 1] + values[mid]) / 2


def run_mode(args, truth_rows, mode: int, out_dir: Path):
    mode_name = MODE_NAME[mode]
    gql = out_dir / f"run_{mode}_{mode_name}.gql"
    log = out_dir / f"raw_{mode}_{mode_name}.log"
    detail = out_dir / f"detail_{mode}_{mode_name}.csv"

    lines = [
        f"session set graph {args.graph}",
        f"call set_gcard_star_config({args.star_len}, {args.star_degree})",
    ]
    for row in truth_rows:
        lines.append(
            f':time call gcard_query("{row["gcard_path"]}", {args.k}, {args.sample_size}, '
            f"{mode}, false, {args.tree_num})"
        )
    lines.append(":quit")
    gql.write_text("\n".join(lines) + "\n", encoding="utf-8")

    expected = len(truth_rows)
    existing_estimates = []
    if log.exists():
        existing_estimates = [m.group(2) for m in CARD_RE.finditer(log.read_text(encoding="utf-8", errors="replace"))]

    elapsed = 0.0
    if len(existing_estimates) < expected:
        started = time.time()
        with log.open("w", encoding="utf-8") as out:
            proc = subprocess.Popen(
                [str(args.minigu), "execute", "--path", str(args.db), str(gql)],
                cwd=str(ROOT),
                stdout=out,
                stderr=subprocess.STDOUT,
                text=True,
            )
            while proc.poll() is None:
                try:
                    text = log.read_text(encoding="utf-8", errors="replace")
                    done = len(CARD_RE.findall(text))
                except FileNotFoundError:
                    done = 0
                if done >= expected:
                    time.sleep(1.0)
                    if proc.poll() is None:
                        proc.terminate()
                        try:
                            proc.wait(timeout=20)
                        except subprocess.TimeoutExpired:
                            proc.kill()
                            proc.wait()
                    break
                time.sleep(1.0)
            rc = proc.returncode
        elapsed = time.time() - started
        if rc not in (0, -15):
            raise SystemExit(f"minigu exited with {rc}; see {log}")

    text = log.read_text(encoding="utf-8", errors="replace")
    estimates_by_name = {m.group(1): float(m.group(2)) for m in CARD_RE.finditer(text)}
    times = [float(m.group(1)) for m in TIME_RE.finditer(text)]
    if len(estimates_by_name) != len(truth_rows):
        raise SystemExit(
            f"mode {mode}: got {len(estimates_by_name)} estimates, "
            f"expected {len(truth_rows)}; see {log}"
        )
    if len(times) < len(truth_rows):
        times.extend([0.0] * (len(truth_rows) - len(times)))

    rows = []
    with detail.open("w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(
            [
                "mode",
                "mode_name",
                "group",
                "base",
                "variant",
                "predicate_count",
                "true_cardinality",
                "estimate_raw",
                "estimate_for_qerror",
                "latency_s",
                "q_error",
            ]
        )
        for truth_row, latency in zip(truth_rows, times):
            variant = truth_row["variant"]
            estimate = estimates_by_name[variant]
            qe = qerror(estimate, truth_row["true_cardinality"])
            row = [
                mode,
                mode_name,
                truth_row["group"],
                truth_row["base"],
                variant,
                truth_row["predicate_count"],
                truth_row["true_cardinality"],
                estimate,
                max(estimate, 1.0),
                latency,
                qe,
            ]
            rows.append(row)
            writer.writerow(row)

    print(
        f"mode={mode} name={mode_name} queries={len(rows)} elapsed_s={elapsed:.2f} "
        f"detail={detail}"
    )
    return rows


def write_summary(rows, path: Path):
    groups = {}
    for row in rows:
        key = (row[0], row[1], row[2], row[3])
        groups.setdefault(key, []).append(float(row[-1]))

    with path.open("w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(["mode", "mode_name", "group", "base", "count", "mean_qerror", "median_qerror", "max_qerror"])
        all_by_mode = {}
        for key in sorted(groups):
            values = groups[key]
            writer.writerow([*key, len(values), sum(values) / len(values), median(values), max(values)])
            all_by_mode.setdefault(key[:2], []).extend(values)
        for key in sorted(all_by_mode):
            values = all_by_mode[key]
            writer.writerow([key[0], key[1], "ALL", "ALL", len(values), sum(values) / len(values), median(values), max(values)])


def svg_escape(text):
    return (
        str(text)
        .replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
    )


def draw_base_chart(base_rows, out_path: Path):
    width, height = 980, 420
    left, right, top, bottom = 72, 28, 42, 78
    plot_w = width - left - right
    plot_h = height - top - bottom
    variants = sorted({row[4] for row in base_rows})
    values = {(row[0], row[4]): float(row[-1]) for row in base_rows}
    max_q = max(values.values()) if values else 1.0
    max_log = max(1.0, math.log10(max_q))

    def y_of(q):
        return top + plot_h - (math.log10(max(q, 1.0)) / max_log) * plot_h

    n = len(variants)
    slot = plot_w / max(n, 1)
    bar_w = min(24, slot * 0.32)
    title = f"{base_rows[0][2]} / {base_rows[0][3]} q-error by predicate mode"
    colors = {0: "#2563eb", 1: "#dc2626"}

    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">',
        '<rect width="100%" height="100%" fill="#ffffff"/>',
        f'<text x="{left}" y="24" font-family="Arial" font-size="18" font-weight="700">{svg_escape(title)}</text>',
        f'<line x1="{left}" y1="{top + plot_h}" x2="{left + plot_w}" y2="{top + plot_h}" stroke="#444"/>',
        f'<line x1="{left}" y1="{top}" x2="{left}" y2="{top + plot_h}" stroke="#444"/>',
    ]
    ticks = sorted({1.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0, 1000000.0, max_q})
    for tick in ticks:
        if tick > max_q * 1.001:
            continue
        y = y_of(tick)
        label = f"{tick:g}"
        parts.append(f'<line x1="{left - 5}" y1="{y:.2f}" x2="{left + plot_w}" y2="{y:.2f}" stroke="#e5e7eb"/>')
        parts.append(f'<text x="{left - 10}" y="{y + 4:.2f}" text-anchor="end" font-family="Arial" font-size="11" fill="#374151">{label}</text>')
    parts.append(f'<text x="16" y="{top + plot_h / 2:.2f}" transform="rotate(-90 16,{top + plot_h / 2:.2f})" font-family="Arial" font-size="12" fill="#374151">q-error (log10)</text>')

    for i, variant in enumerate(variants):
        cx = left + slot * i + slot / 2
        for mode, dx in [(0, -bar_w / 2), (1, bar_w / 2)]:
            q = values.get((mode, variant), 1.0)
            y = y_of(q)
            h = top + plot_h - y
            x = cx + dx - bar_w / 2
            parts.append(
                f'<rect x="{x:.2f}" y="{y:.2f}" width="{bar_w:.2f}" height="{max(h, 1):.2f}" '
                f'fill="{colors[mode]}"><title>{MODE_NAME[mode]} {variant}: {q:.4g}</title></rect>'
            )
        parts.append(f'<text x="{cx:.2f}" y="{top + plot_h + 18}" text-anchor="middle" font-family="Arial" font-size="11" fill="#374151">{svg_escape(variant.split("_")[-1])}</text>')
    parts.extend(
        [
            f'<rect x="{left}" y="{height - 34}" width="14" height="14" fill="{colors[0]}"/>',
            f'<text x="{left + 20}" y="{height - 23}" font-family="Arial" font-size="12">0 inner</text>',
            f'<rect x="{left + 96}" y="{height - 34}" width="14" height="14" fill="{colors[1]}"/>',
            f'<text x="{left + 116}" y="{height - 23}" font-family="Arial" font-size="12">1 scale</text>',
            "</svg>",
        ]
    )
    out_path.write_text("\n".join(parts) + "\n", encoding="utf-8")


def write_charts(rows, chart_dir: Path):
    chart_dir.mkdir(parents=True, exist_ok=True)
    by_base = {}
    for row in rows:
        by_base.setdefault((row[2], row[3]), []).append(row)
    for (group, base), base_rows in sorted(by_base.items()):
        draw_base_chart(base_rows, chart_dir / f"{group}_{base}_qerror.svg")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--truth", type=Path, default=DEFAULT_TRUTH)
    parser.add_argument("--out-dir", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--db", type=Path, default=DEFAULT_RUN_DB)
    parser.add_argument("--catalog", type=Path, default=DEFAULT_CATALOG)
    parser.add_argument("--flatgraph", type=Path, default=DEFAULT_FLATGRAPH)
    parser.add_argument("--statistic", type=Path, default=DEFAULT_STATISTIC)
    parser.add_argument("--graph", default=DEFAULT_GRAPH)
    parser.add_argument("--minigu", type=Path, default=MINIGU)
    parser.add_argument("--k", type=int, default=2)
    parser.add_argument("--sample-size", type=int, default=500)
    parser.add_argument("--tree-num", type=int, default=10)
    parser.add_argument("--star-len", type=int, default=1)
    parser.add_argument("--star-degree", type=int, default=5)
    args = parser.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    prepare_db(args)
    truth_rows = read_truth(args.truth)
    all_rows = []
    for mode in (0, 1):
        all_rows.extend(run_mode(args, truth_rows, mode, args.out_dir))

    combined = args.out_dir / "detail_combined.csv"
    with combined.open("w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(
            [
                "mode",
                "mode_name",
                "group",
                "base",
                "variant",
                "predicate_count",
                "true_cardinality",
                "estimate_raw",
                "estimate_for_qerror",
                "latency_s",
                "q_error",
            ]
        )
        writer.writerows(all_rows)

    summary = args.out_dir / "summary_by_base.csv"
    write_summary(all_rows, summary)
    charts = args.out_dir / "charts"
    write_charts(all_rows, charts)

    print(f"combined={combined}")
    print(f"summary={summary}")
    print(f"charts={charts}")


if __name__ == "__main__":
    main()
