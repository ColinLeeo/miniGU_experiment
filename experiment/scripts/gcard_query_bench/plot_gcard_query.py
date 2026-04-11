#!/usr/bin/env python3
"""
Plot GCard query benchmark results: Q-error and timing bar charts.

Usage:
    python plot_gcard_query.py [results_csv] [true_card_csv] [output_dir]

Defaults:
    results_csv:  experiment/result/gcard_query/gcard_query_results.csv
    true_card_csv: experiment/result/gcard_query/true_card.csv
    output_dir:   experiment/result/gcard_query/
"""
import re
import sys
import pandas as pd
import matplotlib.pyplot as plt
import matplotlib
import numpy as np
from pathlib import Path

matplotlib.rcParams.update({
    "font.size": 13,
    "axes.labelsize": 14,
    "axes.titlesize": 15,
    "legend.fontsize": 11,
    "figure.dpi": 150,
})

SCRIPT_DIR = Path(__file__).resolve().parent
PROJECT_DIR = SCRIPT_DIR.parent.parent.parent
DEFAULT_RESULTS = PROJECT_DIR / "experiment/result/gcard_query/gcard_query_results.csv"
DEFAULT_TRUE    = PROJECT_DIR / "experiment/result/gcard_query/true_card.csv"
DEFAULT_OUT     = PROJECT_DIR / "experiment/result/gcard_query"

results_csv  = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_RESULTS
true_card_csv = Path(sys.argv[2]) if len(sys.argv) > 2 else DEFAULT_TRUE
output_dir   = Path(sys.argv[3]) if len(sys.argv) > 3 else DEFAULT_OUT
output_dir.mkdir(parents=True, exist_ok=True)

# ── Load results ─────────────────────────────────────────────────────────────
df = pd.read_csv(results_csv)

# ── Parse true_card.csv ──────────────────────────────────────────────────────
# Format:  "  L1                      1,662,673,006  (1.59s)"
true_card = {}
with open(true_card_csv) as f:
    for line in f:
        line = line.strip()
        m = re.match(r'^(\S+)\s+([\d,]+)\s*\(', line)
        if m:
            pat = m.group(1)
            val = int(m.group(2).replace(",", ""))
            true_card[pat] = val

# Attach true cardinality
df["true_card"] = df["pattern"].map(true_card)

# Compute Q-error: max(est/true, true/est), treat 0-true as NaN
def qerror(est, true):
    if pd.isna(true) or true == 0:
        return np.nan
    if est == 0:
        return np.inf
    return max(est / true, true / est)

df["qerror"] = df.apply(lambda r: qerror(r["cardinality"], r["true_card"]), axis=1)

# Sort patterns: L1, L1_PA, L2, L2_PB, ...
def pat_sort_key(p):
    m = re.match(r'L(\d+)', p)
    num = int(m.group(1)) if m else 99
    return (num, p)

# ── Helper: sort & split patterns ────────────────────────────────────────────
all_patterns = sorted(df["pattern"].unique(), key=pat_sort_key)
base_patterns = [p for p in all_patterns if "_P" not in p]
pred_patterns = [p for p in all_patterns if "_P" in p]

GCARD_COLOR   = "#E74C3C"
TRUE_COLOR    = "#3498DB"
QERROR_COLOR  = "#E74C3C"

EST_COLOR     = "#E74C3C"
TOTAL_COLOR   = "#95A5A6"
SAMPLE_COLOR  = "#3498DB"

# ══════════════════════════════════════════════════════════════════════════════
# Figure 1: Cardinality comparison (estimated vs true), log scale
# ══════════════════════════════════════════════════════════════════════════════
def plot_cardinality(ax, patterns, title):
    sub = df[df["pattern"].isin(patterns)].set_index("pattern")
    x = np.arange(len(patterns))
    w = 0.35

    est_vals  = [sub.loc[p, "cardinality"] if p in sub.index else 0 for p in patterns]
    true_vals = [sub.loc[p, "true_card"]   if p in sub.index else 0 for p in patterns]

    # Replace 0 with small value for log scale visibility
    est_vals  = [v if v > 0 else 1 for v in est_vals]
    true_vals = [v if v > 0 else 1 for v in true_vals]

    ax.bar(x - w/2, est_vals,  w, label="GCard (estimated)", color=GCARD_COLOR,
           edgecolor="white", linewidth=0.5, zorder=3)
    ax.bar(x + w/2, true_vals, w, label="True",              color=TRUE_COLOR,
           edgecolor="white", linewidth=0.5, zorder=3)

    ax.set_yscale("log")
    ax.set_xticks(x)
    ax.set_xticklabels(patterns, rotation=15, ha="right")
    ax.set_ylabel("Cardinality (log scale)")
    ax.set_title(title)
    ax.legend()
    ax.grid(axis="y", alpha=0.3, zorder=0)
    ax.set_axisbelow(True)


fig1, (ax1a, ax1b) = plt.subplots(1, 2, figsize=(14, 5))
plot_cardinality(ax1a, base_patterns, "Cardinality: Without Predicates (SF1)")
plot_cardinality(ax1b, pred_patterns, "Cardinality: With Predicates (SF1)")
fig1.tight_layout()
out1 = output_dir / "gcard_cardinality_sf1.pdf"
fig1.savefig(out1, bbox_inches="tight")
fig1.savefig(out1.with_suffix(".png"), bbox_inches="tight")
print(f"Saved: {out1}")

# ══════════════════════════════════════════════════════════════════════════════
# Figure 2: Q-error bar chart
# ══════════════════════════════════════════════════════════════════════════════
INF_CAP = 1e8

def plot_qerror(ax, patterns, title):
    sub = df[df["pattern"].isin(patterns)].set_index("pattern")
    x = np.arange(len(patterns))

    qerrs = []
    is_inf = []
    is_nan = []
    for p in patterns:
        if p not in sub.index:
            qerrs.append(0); is_inf.append(False); is_nan.append(True)
            continue
        q = sub.loc[p, "qerror"]
        if pd.isna(q):
            qerrs.append(0); is_inf.append(False); is_nan.append(True)
        elif q == np.inf:
            qerrs.append(INF_CAP); is_inf.append(True); is_nan.append(False)
        else:
            qerrs.append(q); is_inf.append(False); is_nan.append(False)

    bars = ax.bar(x, qerrs, 0.6, color=QERROR_COLOR,
                  edgecolor="white", linewidth=0.5, zorder=3)

    for bar, q, inf_f, nan_f in zip(bars, qerrs, is_inf, is_nan):
        if nan_f or q == 0:
            continue
        if inf_f:
            ax.text(bar.get_x() + bar.get_width()/2, bar.get_height(),
                    "∞", ha="center", va="bottom", fontsize=13,
                    fontweight="bold", color=QERROR_COLOR)
        elif q >= 10:
            ax.text(bar.get_x() + bar.get_width()/2, bar.get_height() * 1.05,
                    f"{q:.1f}", ha="center", va="bottom", fontsize=9, rotation=45)

    ax.set_yscale("log")
    ax.set_ylabel("Q-error (log scale)")
    ax.set_xticks(x)
    ax.set_xticklabels(patterns, rotation=15, ha="right")
    ax.set_title(title)
    ax.axhline(y=1, color="gray", linestyle="--", linewidth=0.8, zorder=1)
    ax.grid(axis="y", alpha=0.3, zorder=0)
    ax.set_axisbelow(True)


fig2, (ax2a, ax2b) = plt.subplots(1, 2, figsize=(14, 5))
plot_qerror(ax2a, base_patterns, "Q-error: Without Predicates (SF1)")
plot_qerror(ax2b, pred_patterns, "Q-error: With Predicates (SF1)")
fig2.tight_layout()
out2 = output_dir / "gcard_qerror_sf1.pdf"
fig2.savefig(out2, bbox_inches="tight")
fig2.savefig(out2.with_suffix(".png"), bbox_inches="tight")
print(f"Saved: {out2}")

# ══════════════════════════════════════════════════════════════════════════════
# Figure 3: Timing breakdown (stacked: estimate + sample, vs total)
# ══════════════════════════════════════════════════════════════════════════════
def plot_timing(ax, patterns, title):
    sub = df[df["pattern"].isin(patterns)].set_index("pattern")
    x = np.arange(len(patterns))
    w = 0.5

    est_t    = [float(sub.loc[p, "estimate_time_s"]) if p in sub.index else 0 for p in patterns]
    sample_t = [float(sub.loc[p, "sample_time_s"])   if p in sub.index else 0 for p in patterns]
    build_t  = [float(sub.loc[p, "build_time_s"])    if p in sub.index else 0 for p in patterns]

    # Stacked bar: estimate (bottom) + sample (middle) + build (top, separate)
    ax.bar(x, est_t,    w, label="estimate",  color=EST_COLOR,    zorder=3)
    ax.bar(x, sample_t, w, label="sample",    color=SAMPLE_COLOR, bottom=est_t, zorder=3)
    ax.bar(x, build_t,  w, label="build",     color=TOTAL_COLOR,
           bottom=[e+s for e,s in zip(est_t, sample_t)], zorder=3, alpha=0.6)

    ax.set_yscale("log")
    ax.set_ylabel("Time (s, log scale)")
    ax.set_xticks(x)
    ax.set_xticklabels(patterns, rotation=15, ha="right")
    ax.set_title(title)
    ax.legend()
    ax.grid(axis="y", alpha=0.3, zorder=0)
    ax.set_axisbelow(True)


fig3, (ax3a, ax3b) = plt.subplots(1, 2, figsize=(14, 5))
plot_timing(ax3a, base_patterns, "Timing Breakdown: Without Predicates (SF1)")
plot_timing(ax3b, pred_patterns, "Timing Breakdown: With Predicates (SF1)")
fig3.tight_layout()
out3 = output_dir / "gcard_timing_sf1.pdf"
fig3.savefig(out3, bbox_inches="tight")
fig3.savefig(out3.with_suffix(".png"), bbox_inches="tight")
print(f"Saved: {out3}")

plt.show()
