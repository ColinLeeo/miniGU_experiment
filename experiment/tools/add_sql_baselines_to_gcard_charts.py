#!/usr/bin/env python3
import argparse
import csv
import json
import math
import os
import shutil
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MINIGU_EXP = ROOT / "experiment"

DEFAULT_TRUTH = ROOT / "experiment/result/truecard/ldbc_sf1_unique_with_pred_10/truth.csv"
DEFAULT_GCARD_DETAIL = (
    ROOT
    / "experiment/result/gcard_query/lsqb_glogs_with_pred_10_unique_k2d5_rerun/detail_combined.csv"
)
DEFAULT_CHART_DIR = (
    ROOT
    / "experiment/result/gcard_query/lsqb_glogs_with_pred_10_unique_k2d5_rerun/charts"
)
DEFAULT_OUT = ROOT / "experiment/result/sql_baselines/lsqb_glogs_with_pred_10_unique"

SAFEB0UND_RUNNER = ROOT / "experiment/scripts/safebound/run_safebound.py"
SAFEB0UND_WORKSPACE = MINIGU_EXP
SAFEB0UND_DATASET = MINIGU_EXP / "datasets/ldbc/sf1"
BAYESCARD_SCRIPT = (
    MINIGU_EXP
    / "baseline/SafeBound/repro_bayescard_ldbc_sf1/scripts/06_eval_ldbc_with_pred.py"
)
BAYESCARD_PYTHON = (
    MINIGU_EXP / "baseline/SafeBound/repro_bayescard_ldbc_sf1/env/bin/python3.7"
)
BAYESCARD_MODEL_DIR = (
    MINIGU_EXP / "baseline/SafeBound/repro_bayescard_ldbc_sf1/models_2path_alias_local"
)
FACTORJOIN_RUNNER = ROOT / "experiment/scripts/factorjoin/run_factorjoin_ldbc.py"
FACTORJOIN_PYTHON = (
    MINIGU_EXP / "baseline/SafeBound/repro_bayescard_ldbc_sf1/env/bin/python3.7"
)

METHODS = ["inner", "scale", "safebound", "bayescard", "factorjoin"]
COLORS = {
    "inner": "#2563eb",
    "scale": "#dc2626",
    "safebound": "#059669",
    "bayescard": "#7c3aed",
    "factorjoin": "#ea580c",
}
LABELS = {
    "inner": "0 inner",
    "scale": "1 scale",
    "safebound": "SafeBound",
    "bayescard": "BayesCard",
    "factorjoin": "FactorJoin",
}


def read_truth(path: Path):
    rows = []
    with path.open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            row["true_cardinality"] = float(row["true_cardinality"])
            rows.append(row)
    return rows


def qerror(estimate, truth):
    estimate = max(float(estimate), 1.0)
    truth = max(float(truth), 1.0)
    return max(estimate / truth, truth / estimate)



def unwrap_value(value):
    if isinstance(value, dict):
        return next(iter(value.values()))
    return value


def sql_literal(value):
    value = unwrap_value(value)
    if isinstance(value, bool):
        return "1" if value else "0"
    if isinstance(value, (int, float)):
        return str(value)
    return "'" + str(value).replace("'", "''") + "'"


def sql_op(op):
    return {
        "eq": "=",
        "ne": "!=",
        "gt": ">",
        "ge": ">=",
        "lt": "<",
        "le": "<=",
    }[op.lower()]


def flatten_pattern_to_sql(pattern_path):
    pattern = json.loads(Path(pattern_path).read_text(encoding="utf-8"))
    vertices = {v["id"]: v["label"].lower() for v in pattern["vertices"]}
    edges = pattern["edges"]
    predicates = pattern.get("predicates", [])

    seen_vertices = {}
    lines = ["SELECT COUNT(*)", f"FROM {edges[0]['label'].lower()} e{edges[0]['id']}"]
    seen_vertices[edges[0]["src"]] = (f"e{edges[0]['id']}", "src")
    seen_vertices[edges[0]["dst"]] = (f"e{edges[0]['id']}", "dst")

    for edge in edges[1:]:
        alias = f"e{edge['id']}"
        conditions = []
        for side in ("src", "dst"):
            vid = edge[side]
            if vid in seen_vertices:
                prev_alias, prev_col = seen_vertices[vid]
                conditions.append(f"{alias}.{side} = {prev_alias}.{prev_col}")
        if conditions:
            lines.append(
                f"JOIN {edge['label'].lower()} {alias} ON " + " AND ".join(conditions)
            )
        else:
            lines.append(f"CROSS JOIN {edge['label'].lower()} {alias}")
        seen_vertices.setdefault(edge["src"], (alias, "src"))
        seen_vertices.setdefault(edge["dst"], (alias, "dst"))

    vertex_aliases = {}
    for pred in predicates:
        if pred["target"] != "vertex":
            continue
        vid = pred["id"]
        if vid not in vertex_aliases:
            alias = f"v{vid}"
            vertex_aliases[vid] = alias
            pin_alias, pin_col = seen_vertices[vid]
            lines.append(f"JOIN {vertices[vid]} {alias} ON {alias}.id = {pin_alias}.{pin_col}")

    where = []
    for pred in predicates:
        alias = f"v{pred['id']}" if pred["target"] == "vertex" else f"e{pred['id']}"
        where.append(
            f"{alias}.{pred['property']} {sql_op(pred['op'])} {sql_literal(pred['value'])}"
        )
    if where:
        lines.append("WHERE " + " AND ".join(where))
    return "\n".join(lines) + ";\n"

def median(values):
    values = sorted(values)
    n = len(values)
    if n == 0:
        return math.nan
    mid = n // 2
    if n % 2:
        return values[mid]
    return (values[mid - 1] + values[mid]) / 2


def prepare_flat_sql(truth_rows, flat_dir: Path):
    flat_dir.mkdir(parents=True, exist_ok=True)
    manifest = flat_dir / "manifest.csv"
    with manifest.open("w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(["file", "group", "base", "variant", "true_cardinality"])
        for row in truth_rows:
            dest_name = f"{row['variant']}.sql"
            (flat_dir / dest_name).write_text(
                flatten_pattern_to_sql(row["gcard_path"]), encoding="utf-8"
            )
            writer.writerow(
                [dest_name, row["group"], row["base"], row["variant"], row["true_cardinality"]]
            )


def run_checked(cmd, cwd=None, env=None):
    print("+ " + " ".join(str(x) for x in cmd), flush=True)
    subprocess.run(cmd, cwd=str(cwd) if cwd else None, env=env, check=True)


def run_safebound(args, flat_dir: Path, out_dir: Path):
    stats = out_dir / "safebound/lsqb_glogs_with_pred_10_unique_sf1.pkl"
    output = out_dir / "safebound/estimate.csv"
    output.parent.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["PYTHONPATH"] = ""
    if args.force and stats.exists():
        stats.unlink()
    if args.force and output.exists():
        output.unlink()
    if not stats.exists():
        run_checked(
            [
                args.safebound_python,
                str(SAFEB0UND_RUNNER),
                "build",
                "--workspace",
                str(SAFEB0UND_WORKSPACE),
                "--dataset-dir",
                str(SAFEB0UND_DATASET),
                "--pattern-dir",
                str(flat_dir),
                "--output-stats",
                str(stats),
                "--num-cores",
                str(args.safebound_cores),
            ],
            cwd=ROOT,
            env=env,
        )
    if not output.exists():
        run_checked(
            [
                args.safebound_python,
                str(SAFEB0UND_RUNNER),
                "query",
                "--workspace",
                str(SAFEB0UND_WORKSPACE),
                "--pattern-dir",
                str(flat_dir),
                "--stats-path",
                str(stats),
                "--output-csv",
                str(output),
            ],
            cwd=ROOT,
            env=env,
        )
    return output


def run_bayescard(args, flat_dir: Path, out_dir: Path):
    output = out_dir / "bayescard/estimate.csv"
    output.parent.mkdir(parents=True, exist_ok=True)
    if args.force and output.exists():
        output.unlink()
    if not output.exists():
        run_checked(
            [
                str(BAYESCARD_PYTHON),
                str(BAYESCARD_SCRIPT),
                "--model-dir",
                str(BAYESCARD_MODEL_DIR),
                "--query-dir",
                str(flat_dir),
                "--output",
                str(output),
            ],
            cwd=BAYESCARD_SCRIPT.parent,
        )
    return output



def run_factorjoin(args, out_dir: Path):
    output = out_dir / "factorjoin/estimate.csv"
    model_dir = out_dir / "factorjoin/model"
    model = model_dir / f"model_ldbc_{args.factorjoin_bucket_method}_{args.factorjoin_bins}.pkl"
    output.parent.mkdir(parents=True, exist_ok=True)
    if args.force and output.exists():
        output.unlink()
    if args.force and model.exists():
        model.unlink()
    if not model.exists():
        run_checked(
            [
                str(args.factorjoin_python),
                str(FACTORJOIN_RUNNER),
                "build",
                "--model-dir",
                str(model_dir),
                "--n-dim-dist",
                str(args.factorjoin_n_dim_dist),
                "--n-bins",
                str(args.factorjoin_bins),
                "--bucket-method",
                args.factorjoin_bucket_method,
            ],
            cwd=ROOT,
        )
    if not output.exists():
        run_checked(
            [
                str(args.factorjoin_python),
                str(FACTORJOIN_RUNNER),
                "query",
                "--truth",
                str(args.truth),
                "--model-path",
                str(model),
                "--output-csv",
                str(output),
            ],
            cwd=ROOT,
        )
    return output

def load_manifest(flat_dir: Path):
    manifest = {}
    with (flat_dir / "manifest.csv").open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            row["true_cardinality"] = float(row["true_cardinality"])
            manifest[row["file"]] = row
    return manifest


def combine_results(gcard_detail: Path, flat_dir: Path, safebound_csv: Path, bayescard_csv: Path, factorjoin_csv: Path, out_dir: Path):
    manifest = load_manifest(flat_dir)
    rows = []
    with gcard_detail.open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            rows.append(
                {
                    "method": row["mode_name"],
                    "group": row["group"],
                    "base": row["base"],
                    "variant": row["variant"],
                    "true_cardinality": float(row["true_cardinality"]),
                    "estimate": float(row["estimate_raw"]),
                    "latency_s": float(row["latency_s"]),
                    "q_error": float(row["q_error"]),
                    "status": "ok",
                }
            )

    with safebound_csv.open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            file_name = Path(row["query"]).name
            meta = manifest[file_name]
            estimate = float(row["estimate"])
            rows.append(
                {
                    "method": "safebound",
                    "group": meta["group"],
                    "base": meta["base"],
                    "variant": meta["variant"],
                    "true_cardinality": meta["true_cardinality"],
                    "estimate": estimate,
                    "latency_s": float(row["inference_time_s"]),
                    "q_error": qerror(estimate, meta["true_cardinality"]),
                    "status": "ok",
                }
            )

    with bayescard_csv.open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            meta = manifest[row["query_file"]]
            status = row["status"]
            if row["prediction"] == "":
                estimate = 1.0
                qe = math.inf
            else:
                estimate = float(row["prediction"])
                qe = qerror(estimate, meta["true_cardinality"])
            rows.append(
                {
                    "method": "bayescard",
                    "group": meta["group"],
                    "base": meta["base"],
                    "variant": meta["variant"],
                    "true_cardinality": meta["true_cardinality"],
                    "estimate": estimate,
                    "latency_s": float(row["latency_ms"]) / 1000.0,
                    "q_error": qe,
                    "status": status,
                }
            )


    with factorjoin_csv.open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            meta = manifest[row["query_file"]]
            status = row["status"]
            if row["prediction"] == "":
                estimate = 1.0
                qe = math.inf
            else:
                estimate = float(row["prediction"])
                qe = qerror(estimate, meta["true_cardinality"])
            rows.append(
                {
                    "method": "factorjoin",
                    "group": meta["group"],
                    "base": meta["base"],
                    "variant": meta["variant"],
                    "true_cardinality": meta["true_cardinality"],
                    "estimate": estimate,
                    "latency_s": float(row["latency_s"]),
                    "q_error": qe,
                    "status": status,
                }
            )

    combined = out_dir / "qerror_combined.csv"
    with combined.open("w", newline="", encoding="utf-8") as f:
        fields = [
            "method",
            "group",
            "base",
            "variant",
            "true_cardinality",
            "estimate",
            "latency_s",
            "q_error",
            "status",
        ]
        writer = csv.DictWriter(f, fieldnames=fields)
        writer.writeheader()
        writer.writerows(rows)
    return rows, combined


def write_summary(rows, out_dir: Path):
    groups = {}
    for row in rows:
        if not math.isfinite(row["q_error"]):
            continue
        groups.setdefault((row["method"], row["group"], row["base"]), []).append(row["q_error"])
        groups.setdefault((row["method"], "ALL", "ALL"), []).append(row["q_error"])
    summary = out_dir / "summary_by_method.csv"
    with summary.open("w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(["method", "group", "base", "count", "mean_qerror", "median_qerror", "max_qerror"])
        for key in sorted(groups):
            vals = groups[key]
            writer.writerow([*key, len(vals), sum(vals) / len(vals), median(vals), max(vals)])
    return summary


def svg_escape(text):
    return (
        str(text)
        .replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
    )


def draw_base_chart(base_rows, out_path: Path):
    width, height = 1160, 440
    left, right, top, bottom = 76, 30, 42, 86
    plot_w = width - left - right
    plot_h = height - top - bottom
    variants = sorted({row["variant"] for row in base_rows})
    rows_by_key = {
        (row["method"], row["variant"]): row
        for row in base_rows
        if math.isfinite(row["q_error"])
    }
    values = {key: row["q_error"] for key, row in rows_by_key.items()}
    max_q = max(values.values()) if values else 1.0
    max_log = max(1.0, math.log10(max_q))

    def y_of(q):
        return top + plot_h - (math.log10(max(q, 1.0)) / max_log) * plot_h

    n = len(variants)
    slot = plot_w / max(n, 1)
    bar_w = min(12, slot * 0.15)
    group = base_rows[0]["group"]
    base = base_rows[0]["base"]
    title = f"{group} / {base} q-error by method"

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
        parts.append(f'<line x1="{left - 5}" y1="{y:.2f}" x2="{left + plot_w}" y2="{y:.2f}" stroke="#e5e7eb"/>')
        parts.append(f'<text x="{left - 10}" y="{y + 4:.2f}" text-anchor="end" font-family="Arial" font-size="11" fill="#374151">{tick:g}</text>')
    parts.append(f'<text x="16" y="{top + plot_h / 2:.2f}" transform="rotate(-90 16,{top + plot_h / 2:.2f})" font-family="Arial" font-size="12" fill="#374151">q-error (log10)</text>')

    offsets = {
        "inner": -2.0 * bar_w,
        "scale": -1.0 * bar_w,
        "safebound": 0.0 * bar_w,
        "bayescard": 1.0 * bar_w,
        "factorjoin": 2.0 * bar_w,
    }
    for i, variant in enumerate(variants):
        cx = left + slot * i + slot / 2
        for method in METHODS:
            if (method, variant) not in values:
                continue
            q = values[(method, variant)]
            y = y_of(q)
            h = top + plot_h - y
            x = cx + offsets[method] - bar_w / 2
            row = rows_by_key[(method, variant)]
            estimate = float(row["estimate"])
            truth = float(row["true_cardinality"])
            direction = "overestimate" if estimate > truth else "underestimate" if estimate < truth else "exact"
            parts.append(
                f'<rect x="{x:.2f}" y="{y:.2f}" width="{bar_w:.2f}" height="{max(h, 1):.2f}" '
                f'fill="{COLORS[method]}"><title>{LABELS[method]} {variant}: {q:.4g} ({direction})</title></rect>'
            )
            marker_x = x + bar_w / 2
            marker_y = max(top + 8, y - 8)
            if direction == "overestimate":
                points = f"{marker_x:.2f},{marker_y - 4:.2f} {marker_x - 4:.2f},{marker_y + 4:.2f} {marker_x + 4:.2f},{marker_y + 4:.2f}"
            elif direction == "underestimate":
                points = f"{marker_x:.2f},{marker_y + 4:.2f} {marker_x - 4:.2f},{marker_y - 4:.2f} {marker_x + 4:.2f},{marker_y - 4:.2f}"
            else:
                points = ""
            if points:
                parts.append(
                    f'<polygon points="{points}" fill="#111827" stroke="#ffffff" stroke-width="0.75">'
                    f'<title>{LABELS[method]} {variant}: {direction}</title></polygon>'
                )
        parts.append(f'<text x="{cx:.2f}" y="{top + plot_h + 18}" text-anchor="middle" font-family="Arial" font-size="11" fill="#374151">{svg_escape(variant.split("_")[-1])}</text>')

    legend_x = left
    legend_y = height - 38
    for method in METHODS:
        parts.append(f'<rect x="{legend_x}" y="{legend_y}" width="14" height="14" fill="{COLORS[method]}"/>')
        parts.append(f'<text x="{legend_x + 20}" y="{legend_y + 11}" font-family="Arial" font-size="12">{LABELS[method]}</text>')
        legend_x += 126
    marker_legend_x = legend_x + 8
    parts.append(f'<polygon points="{marker_legend_x},{legend_y + 2} {marker_legend_x - 5},{legend_y + 12} {marker_legend_x + 5},{legend_y + 12}" fill="#111827"/>')
    parts.append(f'<text x="{marker_legend_x + 12}" y="{legend_y + 11}" font-family="Arial" font-size="12">over</text>')
    marker_legend_x += 66
    parts.append(f'<polygon points="{marker_legend_x},{legend_y + 12} {marker_legend_x - 5},{legend_y + 2} {marker_legend_x + 5},{legend_y + 2}" fill="#111827"/>')
    parts.append(f'<text x="{marker_legend_x + 12}" y="{legend_y + 11}" font-family="Arial" font-size="12">under</text>')
    parts.append("</svg>")
    out_path.write_text("\n".join(parts) + "\n", encoding="utf-8")


def update_original_charts(rows, chart_dir: Path):
    chart_dir.mkdir(parents=True, exist_ok=True)
    by_base = {}
    for row in rows:
        by_base.setdefault((row["group"], row["base"]), []).append(row)
    for (group, base), base_rows in sorted(by_base.items()):
        draw_base_chart(base_rows, chart_dir / f"{group}_{base}_qerror.svg")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--truth", type=Path, default=DEFAULT_TRUTH)
    parser.add_argument("--gcard-detail", type=Path, default=DEFAULT_GCARD_DETAIL)
    parser.add_argument("--chart-dir", type=Path, default=DEFAULT_CHART_DIR)
    parser.add_argument("--out-dir", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--safebound-python", default="/home/zxz/miniconda3/envs/TestEnv/bin/python3.10")
    parser.add_argument("--safebound-cores", type=int, default=8)
    parser.add_argument("--force", action="store_true")
    parser.add_argument("--factorjoin-python", type=Path, default=FACTORJOIN_PYTHON)
    parser.add_argument("--factorjoin-n-dim-dist", type=int, default=2)
    parser.add_argument("--factorjoin-bins", type=int, default=200)
    parser.add_argument("--factorjoin-bucket-method", default="fixed_start_key")
    args = parser.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    truth_rows = read_truth(args.truth)
    flat_dir = args.out_dir / "flat_sql"
    prepare_flat_sql(truth_rows, flat_dir)
    safebound_csv = run_safebound(args, flat_dir, args.out_dir)
    bayescard_csv = run_bayescard(args, flat_dir, args.out_dir)
    factorjoin_csv = run_factorjoin(args, args.out_dir)
    rows, combined = combine_results(args.gcard_detail, flat_dir, safebound_csv, bayescard_csv, factorjoin_csv, args.out_dir)
    summary = write_summary(rows, args.out_dir)
    update_original_charts(rows, args.chart_dir)

    print(f"queries={len(truth_rows)}")
    print(f"combined={combined}")
    print(f"summary={summary}")
    print(f"updated_charts={args.chart_dir}")


if __name__ == "__main__":
    main()
