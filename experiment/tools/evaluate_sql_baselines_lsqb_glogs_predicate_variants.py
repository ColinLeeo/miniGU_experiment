#!/usr/bin/env python3
import argparse
import csv
import json
import math
import os
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MINIGU_EXP = ROOT / "experiment"
TRUTH = ROOT / "experiment/result/truecard/ldbc_sf1_unique_with_pred_10/truth.csv"
GCARD_DETAIL = (
    ROOT
    / "experiment/result/gcard_query/lsqb_glogs_with_pred_10_master_predicate_modes/detail_combined.csv"
)
OUT = ROOT / "experiment/result/sql_baselines/lsqb_glogs_with_pred_10"
PATTERN_SQL_FLAT = OUT / "flat_sql"
SAFEB0UND_RUNNER = ROOT / "experiment/scripts/safebound/run_safebound.py"
SAFEB0UND_WORKSPACE = MINIGU_EXP
SAFEB0UND_DATASET = MINIGU_EXP / "datasets/ldbc/sf1"
SAFEB0UND_STATS = OUT / "safebound/lsqb_glogs_with_pred_10_sf1.pkl"
SAFEB0UND_OUTPUT = OUT / "safebound/estimate.csv"
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
BAYESCARD_OUTPUT = OUT / "bayescard/estimate.csv"


def read_truth(path):
    rows = []
    with path.open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            row["true_cardinality"] = float(row["true_cardinality"])
            row["predicate_count"] = int(row["predicate_count"])
            rows.append(row)
    return rows


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

    where = []
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

    for pred in predicates:
        alias = f"v{pred['id']}" if pred["target"] == "vertex" else f"e{pred['id']}"
        where.append(
            f"{alias}.{pred['property']} {sql_op(pred['op'])} {sql_literal(pred['value'])}"
        )
    if where:
        lines.append("WHERE " + " AND ".join(where))
    return "\n".join(lines) + ";\n"


def prepare_flat_sql(truth_rows, out_dir):
    out_dir.mkdir(parents=True, exist_ok=True)
    manifest = out_dir / "manifest.csv"
    with manifest.open("w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(["file", "group", "base", "variant", "true_cardinality"])
        for row in truth_rows:
            file_name = f"{row['variant']}.sql"
            (out_dir / file_name).write_text(
                flatten_pattern_to_sql(row["gcard_path"]), encoding="utf-8"
            )
            writer.writerow(
                [file_name, row["group"], row["base"], row["variant"], row["true_cardinality"]]
            )


def run_checked(cmd, cwd=None, env=None):
    print("+ " + " ".join(str(x) for x in cmd), flush=True)
    subprocess.run(cmd, cwd=str(cwd) if cwd else None, env=env, check=True)


def run_safebound(args):
    if SAFEB0UND_OUTPUT.exists() and not args.force:
        return
    SAFEB0UND_OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["PYTHONPATH"] = ""
    python = args.safebound_python
    if args.force and SAFEB0UND_STATS.exists():
        SAFEB0UND_STATS.unlink()
    if not SAFEB0UND_STATS.exists():
        run_checked(
            [
                python,
                str(SAFEB0UND_RUNNER),
                "build",
                "--workspace",
                str(SAFEB0UND_WORKSPACE),
                "--dataset-dir",
                str(SAFEB0UND_DATASET),
                "--pattern-dir",
                str(PATTERN_SQL_FLAT),
                "--output-stats",
                str(SAFEB0UND_STATS),
                "--num-cores",
                str(args.safebound_cores),
            ],
            cwd=ROOT,
            env=env,
        )
    run_checked(
        [
            python,
            str(SAFEB0UND_RUNNER),
            "query",
            "--workspace",
            str(SAFEB0UND_WORKSPACE),
            "--pattern-dir",
            str(PATTERN_SQL_FLAT),
            "--stats-path",
            str(SAFEB0UND_STATS),
            "--output-csv",
            str(SAFEB0UND_OUTPUT),
        ],
        cwd=ROOT,
        env=env,
    )


def run_bayescard(args):
    if BAYESCARD_OUTPUT.exists() and not args.force:
        return
    BAYESCARD_OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    run_checked(
        [
            str(BAYESCARD_PYTHON),
            str(BAYESCARD_SCRIPT),
            "--model-dir",
            str(BAYESCARD_MODEL_DIR),
            "--query-dir",
            str(PATTERN_SQL_FLAT),
            "--output",
            str(BAYESCARD_OUTPUT),
        ],
        cwd=BAYESCARD_SCRIPT.parent,
    )


def qerror(est, truth):
    est = max(float(est), 1.0)
    truth = max(float(truth), 1.0)
    return max(est / truth, truth / est)


def load_manifest():
    rows = {}
    with (PATTERN_SQL_FLAT / "manifest.csv").open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            row["true_cardinality"] = float(row["true_cardinality"])
            rows[row["file"]] = row
    return rows


def combine_results():
    manifest = load_manifest()
    rows = []

    with GCARD_DETAIL.open(newline="", encoding="utf-8") as f:
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

    with SAFEB0UND_OUTPUT.open(newline="", encoding="utf-8") as f:
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

    with BAYESCARD_OUTPUT.open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            meta = manifest[row["query_file"]]
            estimate_text = row["prediction"]
            status = row["status"]
            if estimate_text == "":
                estimate = 1.0
                qe = math.inf
            else:
                estimate = float(estimate_text)
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

    combined = OUT / "qerror_combined.csv"
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


def summarize(rows):
    summary = OUT / "summary_by_method.csv"
    groups = {}
    for row in rows:
        if not math.isfinite(row["q_error"]):
            continue
        groups.setdefault((row["method"], row["group"], row["base"]), []).append(row["q_error"])
        groups.setdefault((row["method"], "ALL", "ALL"), []).append(row["q_error"])
    with summary.open("w", newline="", encoding="utf-8") as f:
        writer = csv.writer(f)
        writer.writerow(["method", "group", "base", "count", "mean_qerror", "median_qerror", "max_qerror"])
        for key in sorted(groups):
            vals = sorted(groups[key])
            n = len(vals)
            med = vals[n // 2] if n % 2 else (vals[n // 2 - 1] + vals[n // 2]) / 2
            writer.writerow([*key, n, sum(vals) / n, med, max(vals)])
    return summary


def plot_boxplots(rows):
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    plot_dir = OUT / "boxplots"
    plot_dir.mkdir(parents=True, exist_ok=True)
    methods = ["inner", "scale", "safebound", "bayescard"]
    colors = ["#2563eb", "#dc2626", "#059669", "#7c3aed"]
    by_base = {}
    for row in rows:
        if math.isfinite(row["q_error"]):
            by_base.setdefault((row["group"], row["base"], row["method"]), []).append(row["q_error"])

    for group, base in sorted({(r["group"], r["base"]) for r in rows}):
        data = [by_base.get((group, base, method), []) for method in methods]
        fig, ax = plt.subplots(figsize=(7.2, 4.6))
        bp = ax.boxplot(data, labels=methods, patch_artist=True, showfliers=True)
        for patch, color in zip(bp["boxes"], colors):
            patch.set_facecolor(color)
            patch.set_alpha(0.35)
            patch.set_edgecolor(color)
        for element in ["whiskers", "caps", "medians"]:
            for item in bp[element]:
                item.set_color("#374151")
        ax.set_yscale("log")
        ax.set_ylabel("q-error (log scale)")
        ax.set_title(f"{group}/{base}: q-error boxplot")
        ax.grid(axis="y", which="both", alpha=0.25)
        fig.tight_layout()
        fig.savefig(plot_dir / f"{group}_{base}_boxplot.png", dpi=180)
        fig.savefig(plot_dir / f"{group}_{base}_boxplot.pdf")
        plt.close(fig)
    return plot_dir


def plot_overview(rows):
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from matplotlib.patches import Patch

    methods = ["inner", "scale", "safebound", "bayescard"]
    colors = {
        "inner": "#2563eb",
        "scale": "#dc2626",
        "safebound": "#059669",
        "bayescard": "#7c3aed",
    }
    bases = sorted({(r["group"], r["base"]) for r in rows})
    by = {}
    for row in rows:
        q = row["q_error"]
        if math.isfinite(q):
            by.setdefault((row["group"], row["base"], row["method"]), []).append(q)

    fig, ax = plt.subplots(figsize=(18, 7.2))
    positions = []
    data = []
    box_colors = []
    centers = []
    width = 0.16
    gap = 0.7
    for i, (group, base) in enumerate(bases):
        center = i * (len(methods) * width + gap)
        centers.append(center + width * 1.5)
        for j, method in enumerate(methods):
            positions.append(center + j * width)
            values = by.get((group, base, method), [])
            data.append(values if values else [math.nan])
            box_colors.append(colors[method])

    bp = ax.boxplot(
        data,
        positions=positions,
        widths=width * 0.82,
        patch_artist=True,
        showfliers=False,
        manage_ticks=False,
    )
    for patch, color in zip(bp["boxes"], box_colors):
        patch.set_facecolor(color)
        patch.set_alpha(0.42)
        patch.set_edgecolor(color)
    for element in ["whiskers", "caps", "medians"]:
        for item in bp[element]:
            item.set_color("#374151")

    ax.set_yscale("log")
    ax.set_ylabel("q-error (log scale)")
    ax.set_title("All 16 base patterns: q-error comparison")
    ax.set_xticks(centers)
    ax.set_xticklabels([f"{g}/{b}" for g, b in bases], rotation=35, ha="right")
    ax.grid(axis="y", which="both", alpha=0.25)
    ax.legend(
        handles=[Patch(facecolor=colors[m], alpha=0.42, label=m) for m in methods],
        loc="upper right",
        ncol=4,
        frameon=False,
    )
    fig.tight_layout()
    output = OUT / "all_methods_16_groups_boxplot"
    fig.savefig(output.with_suffix(".png"), dpi=200)
    fig.savefig(output.with_suffix(".pdf"))
    plt.close(fig)
    return output


def plot_signed_overview(rows):
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from matplotlib.patches import Patch

    methods = ["inner", "scale", "safebound", "bayescard"]
    colors = {
        "inner": "#2563eb",
        "scale": "#dc2626",
        "safebound": "#059669",
        "bayescard": "#7c3aed",
    }
    bases = sorted({(r["group"], r["base"]) for r in rows})
    by = {}
    for row in rows:
        q = row["q_error"]
        if not math.isfinite(q):
            continue
        estimate = float(row["estimate"])
        truth = float(row["true_cardinality"])
        if estimate == truth:
            signed = 0.0
        else:
            sign = 1.0 if estimate > truth else -1.0
            signed = sign * math.log10(max(q, 1.0))
        by.setdefault((row["group"], row["base"], row["method"]), []).append(signed)

    fig, ax = plt.subplots(figsize=(18, 7.4))
    positions = []
    data = []
    box_colors = []
    centers = []
    width = 0.16
    gap = 0.7
    for i, (group, base) in enumerate(bases):
        center = i * (len(methods) * width + gap)
        centers.append(center + width * 1.5)
        for j, method in enumerate(methods):
            positions.append(center + j * width)
            values = by.get((group, base, method), [])
            data.append(values if values else [math.nan])
            box_colors.append(colors[method])

    bp = ax.boxplot(
        data,
        positions=positions,
        widths=width * 0.82,
        patch_artist=True,
        showfliers=False,
        manage_ticks=False,
    )
    for patch, color in zip(bp["boxes"], box_colors):
        patch.set_facecolor(color)
        patch.set_alpha(0.42)
        patch.set_edgecolor(color)
    for element in ["whiskers", "caps", "medians"]:
        for item in bp[element]:
            item.set_color("#374151")

    max_abs = 0.0
    for values in by.values():
        if values:
            max_abs = max(max_abs, max(abs(v) for v in values))
    limit = max(1.0, math.ceil(max_abs))
    ax.set_ylim(-limit * 1.08, limit * 1.08)
    tick_candidates = [0, 1, 3, 6, 9, 12, 15]
    ticks = sorted({0, *[t for t in tick_candidates if t <= limit], *[-t for t in tick_candidates if t <= limit]})
    ax.set_yticks(ticks)
    ax.set_yticklabels([r"$10^0$" if t == 0 else rf"$10^{{{abs(int(t))}}}$" for t in ticks])
    ax.axhline(0, color="#111827", linewidth=1.0)
    ax.text(
        0.01,
        0.98,
        "overest.",
        transform=ax.transAxes,
        ha="left",
        va="top",
        fontsize=10,
        color="#374151",
    )
    ax.text(
        0.01,
        0.02,
        "underest.",
        transform=ax.transAxes,
        ha="left",
        va="bottom",
        fontsize=10,
        color="#374151",
    )
    ax.set_ylabel(r"underest. $\leftarrow$  $|q-error|$  $\rightarrow$ overest.")
    ax.set_title("All 16 base patterns: signed q-error comparison")
    ax.set_xticks(centers)
    ax.set_xticklabels([f"{g}/{b}" for g, b in bases], rotation=35, ha="right")
    ax.grid(axis="y", alpha=0.25)
    ax.legend(
        handles=[Patch(facecolor=colors[m], alpha=0.42, label=m) for m in methods],
        loc="upper right",
        ncol=4,
        frameon=False,
    )
    fig.tight_layout()
    output = OUT / "all_methods_16_groups_signed_qerror_boxplot"
    fig.savefig(output.with_suffix(".png"), dpi=220)
    fig.savefig(output.with_suffix(".pdf"))
    plt.close(fig)
    return output


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--safebound-python",
        default="/home/zxz/miniconda3/envs/TestEnv/bin/python3.10",
    )
    parser.add_argument("--safebound-cores", type=int, default=8)
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    OUT.mkdir(parents=True, exist_ok=True)
    truth_rows = read_truth(TRUTH)
    prepare_flat_sql(truth_rows, PATTERN_SQL_FLAT)
    run_safebound(args)
    run_bayescard(args)
    rows, combined = combine_results()
    summary = summarize(rows)
    plot_dir = plot_boxplots(rows)
    overview = plot_overview(rows)
    signed_overview = plot_signed_overview(rows)
    print(f"combined={combined}")
    print(f"summary={summary}")
    print(f"boxplots={plot_dir}")
    print(f"overview={overview}.png")
    print(f"signed_overview={signed_overview}.png")


if __name__ == "__main__":
    main()
