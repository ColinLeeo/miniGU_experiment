#!/usr/bin/env python3
"""
Plot sweep results across all (k, max_graph, sample_size) combinations.

Figures produced:
  1. Cardinality grouped bar  – per pattern, bars grouped by k, hue=ss, style=mg
  2. Cardinality heatmap      – pattern × config (log10 scale, 0 → grey)
  3. Sample-time bar chart    – predicate patterns only, grouped by config

Usage:
    python plot_sweep_results.py [result_dir] [output_dir]

Defaults:
    result_dir : experiment/result/gcard_query/
    output_dir : experiment/result/gcard_query/
"""

import re
import sys
from itertools import product
from pathlib import Path

import matplotlib
import matplotlib.patches as mpatches
import matplotlib.pyplot as plt
import numpy as np
import pandas as pd

matplotlib.rcParams.update({
    "font.size": 11,
    "axes.labelsize": 12,
    "axes.titlesize": 13,
    "legend.fontsize": 9,
    "figure.dpi": 150,
})

SCRIPT_DIR = Path(__file__).resolve().parent
PROJECT_DIR = SCRIPT_DIR.parent.parent.parent
DEFAULT_RESULT_DIR = PROJECT_DIR / "experiment/result/gcard_query"

result_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_RESULT_DIR
output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else DEFAULT_RESULT_DIR
output_dir.mkdir(parents=True, exist_ok=True)

K_VALUES  = [1, 2, 3]
MG_VALUES = [5, 10]
SS_VALUES = [200, 500]

# ── Load all CSVs ─────────────────────────────────────────────────────────────
records = []
for k, mg, ss in product(K_VALUES, MG_VALUES, SS_VALUES):
    path = result_dir / f"gcard_query_results_k{k}_mg{mg}_ss{ss}.csv"
    if not path.exists():
        print(f"  [skip] {path.name}")
        continue
    tmp = pd.read_csv(path)
    tmp["k"]  = k
    tmp["mg"] = mg
    tmp["ss"] = ss
    tmp["config"] = f"k{k}_mg{mg}_ss{ss}"
    records.append(tmp)

if not records:
    print("No result CSVs found. Run sweep_gcard_query.sh first.")
    sys.exit(1)

df = pd.concat(records, ignore_index=True)
df["cardinality"] = pd.to_numeric(df["cardinality"], errors="coerce").fillna(0)

# ── Load true cardinalities ────────────────────────────────────────────────────
TRUE_CARD_PATH = result_dir / "true_card.csv"
true_card: dict[str, float] = {}
INF_PLACEHOLDER = 1e13   # sentinel value for "Inf" (too expensive to measure)

if TRUE_CARD_PATH.exists():
    with open(TRUE_CARD_PATH) as f:
        for line in f:
            # Normal:  L1_PA    438,799,026  (0.99s)
            m = re.match(r'^\s+(\S+)\s+([\d,]+)\s+\(', line)
            if m:
                pat = m.group(1)
                val = float(m.group(2).replace(",", ""))
                true_card[pat] = val
                continue
            # Inf:     L6       Inf
            m = re.match(r'^\s+(\S+)\s+Inf\s*$', line)
            if m:
                true_card[m.group(1)] = INF_PLACEHOLDER
    print(f"Loaded true cardinalities for {len(true_card)} patterns: {list(true_card.keys())}")
else:
    print(f"  [skip] true_card.csv not found at {TRUE_CARD_PATH}")


def qerror(est: float, true: float) -> float:
    """Q-error with the convention: treat 0 as 1 when it is the divisor."""
    e = max(est, 1.0)
    t = max(true, 1.0)
    return max(e / t, t / e)

# Pattern sort order
def pat_sort_key(p):
    m = re.match(r'L(\d+)', p)
    return (int(m.group(1)) if m else 99, p)

all_patterns  = sorted(df["pattern"].unique(), key=pat_sort_key)
pred_patterns = [p for p in all_patterns if "_P" in p]
base_patterns = [p for p in all_patterns if "_P" not in p]
all_configs   = sorted(df["config"].unique())

# ── Colour helpers ────────────────────────────────────────────────────────────
K_COLORS  = {1: "#3498DB", 2: "#E74C3C", 3: "#2ECC71"}
SS_HATCH  = {200: "", 500: "///"}
MG_ALPHA  = {5: 1.0, 10: 0.6}

# ══════════════════════════════════════════════════════════════════════════════
# Figure 1 – Cardinality grouped bar (all patterns, grouped by k)
# Each k has two sub-bars: ss=200 (solid) / ss=500 (hatched)
# mg=5 full opacity, mg=10 lighter
# ══════════════════════════════════════════════════════════════════════════════
def fig1_cardinality_grouped(patterns, title_suffix, fname):
    n_pat = len(patterns)
    # sub-bars per pattern: k × mg × ss  = 3×2×2 = 12, but we group by k (3 groups of 4)
    n_groups = len(K_VALUES)           # 3 groups per pattern position
    n_sub    = len(MG_VALUES) * len(SS_VALUES)  # 4 sub-bars per group
    total_bars = n_groups * n_sub      # 12 bars per pattern

    bar_w   = 0.06
    group_w = n_sub * bar_w + 0.02
    pat_w   = n_groups * group_w + 0.05

    fig, ax = plt.subplots(figsize=(max(12, n_pat * 1.8), 5))

    x_centers = np.arange(n_pat) * pat_w

    legend_handles = {}

    for pi, pat in enumerate(patterns):
        cx = x_centers[pi]
        gi = 0
        for k in K_VALUES:
            g_start = cx - pat_w / 2 + gi * group_w + 0.01
            si = 0
            for mg in MG_VALUES:
                for ss in SS_VALUES:
                    sub = df[(df["pattern"] == pat) & (df["k"] == k) &
                             (df["mg"] == mg) & (df["ss"] == ss)]
                    val = float(sub["cardinality"].values[0]) if len(sub) else 0
                    bx  = g_start + si * bar_w + bar_w / 2
                    bar_val = max(val, 1)  # log scale floor

                    label = f"k={k}"
                    bar = ax.bar(
                        bx, bar_val, bar_w,
                        color=K_COLORS[k],
                        hatch=SS_HATCH[ss],
                        alpha=MG_ALPHA[mg],
                        edgecolor="white",
                        linewidth=0.4,
                        zorder=3,
                    )
                    if label not in legend_handles:
                        legend_handles[label] = bar[0]

                    # mark zero
                    if val == 0:
                        ax.text(bx, 1.5, "0", ha="center", va="bottom",
                                fontsize=6, color="grey", rotation=90)
                    si += 1
            gi += 1

    # True-cardinality reference lines (one horizontal segment per pattern)
    true_handle = None
    for pi, pat in enumerate(patterns):
        if pat in true_card:
            cx   = x_centers[pi]
            half = pat_w / 2 - 0.01
            tc   = max(true_card[pat], 1.0)
            line, = ax.plot([cx - half, cx + half], [tc, tc],
                            color="black", linewidth=1.5, linestyle="--",
                            zorder=5)
            if true_handle is None:
                true_handle = line
            if true_card[pat] >= INF_PLACEHOLDER:
                ax.text(cx, tc * 1.5, "Inf", ha="center", va="bottom",
                        fontsize=7, color="black", fontstyle="italic")

    # x-tick at centre of each pattern
    ax.set_xticks(x_centers)
    ax.set_xticklabels(patterns, rotation=20, ha="right")
    ax.set_yscale("log")
    ax.set_ylabel("Cardinality (log scale)")
    ax.set_title(f"Cardinality across configs – {title_suffix}")
    ax.grid(axis="y", alpha=0.3, zorder=0)
    ax.set_axisbelow(True)

    # Legend: k colours
    k_handles = [
        mpatches.Patch(color=K_COLORS[k], label=f"k={k}")
        for k in K_VALUES
    ]
    # ss hatches
    ss_handles = [
        mpatches.Patch(facecolor="grey", hatch=SS_HATCH[ss],
                       edgecolor="white", label=f"ss={ss}")
        for ss in SS_VALUES
    ]
    # mg alpha
    mg_handles = [
        mpatches.Patch(facecolor="grey", alpha=MG_ALPHA[mg],
                       label=f"mg={mg}")
        for mg in MG_VALUES
    ]
    extra = []
    if true_handle is not None:
        from matplotlib.lines import Line2D
        extra = [Line2D([0], [0], color="black", linewidth=1.5,
                        linestyle="--", label="true card")]
    ax.legend(handles=k_handles + ss_handles + mg_handles + extra,
              ncol=4, loc="upper right", fontsize=8)

    fig.tight_layout()
    out = output_dir / fname
    fig.savefig(out, bbox_inches="tight")
    fig.savefig(out.with_suffix(".png"), bbox_inches="tight")
    print(f"Saved: {out}")
    plt.close(fig)


fig1_cardinality_grouped(base_patterns, "No Predicates",   "sweep_cardinality_base.pdf")
fig1_cardinality_grouped(pred_patterns, "With Predicates", "sweep_cardinality_pred.pdf")

# ══════════════════════════════════════════════════════════════════════════════
# Figure 2 – Cardinality heatmap  (pattern × config)
# ══════════════════════════════════════════════════════════════════════════════
pivot = df.pivot_table(index="pattern", columns="config",
                       values="cardinality", aggfunc="first")
pivot = pivot.reindex(index=all_patterns, columns=sorted(pivot.columns))

# log10, treat 0 as NaN for colouring
log_pivot = pivot.copy().astype(float)
log_pivot[log_pivot <= 0] = np.nan
log_pivot = np.log10(log_pivot)

fig2, ax = plt.subplots(figsize=(len(all_configs) * 0.85 + 2, len(all_patterns) * 0.55 + 1.5))

cmap = matplotlib.colormaps.get_cmap("YlOrRd").copy()
cmap.set_bad(color="#CCCCCC")   # grey for 0/NaN

im = ax.imshow(log_pivot.values, aspect="auto", cmap=cmap)

ax.set_xticks(range(len(pivot.columns)))
ax.set_xticklabels(pivot.columns, rotation=40, ha="right", fontsize=8)
ax.set_yticks(range(len(pivot.index)))
ax.set_yticklabels(pivot.index, fontsize=9)

# Annotate cells
for r in range(log_pivot.shape[0]):
    for c in range(log_pivot.shape[1]):
        raw = pivot.values[r, c]
        if pd.isna(raw) or raw == 0:
            txt, col = "0", "black"
        elif raw >= 1e9:
            txt = f"{raw/1e9:.1f}G"
        elif raw >= 1e6:
            txt = f"{raw/1e6:.1f}M"
        elif raw >= 1e3:
            txt = f"{raw/1e3:.0f}K"
        else:
            txt = str(int(raw))
        lv = log_pivot.values[r, c]
        col = "white" if (not np.isnan(lv) and lv > 7) else "black"
        ax.text(c, r, txt, ha="center", va="center", fontsize=6.5, color=col)

cb = fig2.colorbar(im, ax=ax, pad=0.01)
cb.set_label("log₁₀(cardinality)  [grey = 0]", fontsize=9)
ax.set_title("Cardinality heatmap – all patterns × configs", fontsize=13)

fig2.tight_layout()
out2 = output_dir / "sweep_heatmap.pdf"
fig2.savefig(out2, bbox_inches="tight")
fig2.savefig(out2.with_suffix(".png"), bbox_inches="tight")
print(f"Saved: {out2}")
plt.close(fig2)

# ══════════════════════════════════════════════════════════════════════════════
# Figure 3 – Sample-time for predicate patterns (grouped by pattern, hue=config)
# ══════════════════════════════════════════════════════════════════════════════
pred_df = df[df["pattern"].isin(pred_patterns)].copy()

n_pat  = len(pred_patterns)
n_cfg  = len(all_configs)
bar_w  = 0.7 / n_cfg
x_base = np.arange(n_pat)

# colour by k, hatch by ss, alpha by mg  (same convention as fig1)
fig3, ax3 = plt.subplots(figsize=(max(10, n_pat * 1.6), 5))

for ci, cfg in enumerate(sorted(all_configs)):
    k_  = int(re.search(r'k(\d+)', cfg).group(1))
    mg_ = int(re.search(r'mg(\d+)', cfg).group(1))
    ss_ = int(re.search(r'ss(\d+)', cfg).group(1))

    vals = []
    for pat in pred_patterns:
        sub = pred_df[(pred_df["pattern"] == pat) & (pred_df["config"] == cfg)]
        vals.append(float(sub["sample_time_s"].values[0]) if len(sub) else 0)

    xpos = x_base - 0.35 + ci * bar_w + bar_w / 2
    ax3.bar(xpos, vals, bar_w,
            color=K_COLORS[k_], hatch=SS_HATCH[ss_], alpha=MG_ALPHA[mg_],
            edgecolor="white", linewidth=0.4, zorder=3, label=cfg)

ax3.set_xticks(x_base)
ax3.set_xticklabels(pred_patterns, rotation=15, ha="right")
ax3.set_yscale("log")
ax3.set_ylabel("Sample time (s, log scale)")
ax3.set_title("Sampling time – predicate patterns (all configs)")
ax3.grid(axis="y", alpha=0.3, zorder=0)
ax3.set_axisbelow(True)

# Compact legend
k_h  = [mpatches.Patch(color=K_COLORS[k],  label=f"k={k}")  for k in K_VALUES]
ss_h = [mpatches.Patch(facecolor="grey", hatch=SS_HATCH[ss],
                        edgecolor="white", label=f"ss={ss}") for ss in SS_VALUES]
mg_h = [mpatches.Patch(facecolor="grey", alpha=MG_ALPHA[mg], label=f"mg={mg}")
        for mg in MG_VALUES]
ax3.legend(handles=k_h + ss_h + mg_h, ncol=3, fontsize=8)

fig3.tight_layout()
out3 = output_dir / "sweep_sample_time.pdf"
fig3.savefig(out3, bbox_inches="tight")
fig3.savefig(out3.with_suffix(".png"), bbox_inches="tight")
print(f"Saved: {out3}")
plt.close(fig3)

# ══════════════════════════════════════════════════════════════════════════════
# Figure 4 – Q-error grouped bar (same layout as Figure 1, requires true_card)
# ══════════════════════════════════════════════════════════════════════════════
def fig4_qerror_grouped(patterns, title_suffix, fname):
    if not true_card:
        print(f"  [skip] {fname} – no true_card.csv")
        return

    # Filter to patterns that have a true cardinality entry
    patterns = [p for p in patterns if p in true_card]
    if not patterns:
        print(f"  [skip] {fname} – no matching patterns in true_card")
        return

    n_pat    = len(patterns)
    n_sub    = len(MG_VALUES) * len(SS_VALUES)
    bar_w    = 0.06
    group_w  = n_sub * bar_w + 0.02
    pat_w    = len(K_VALUES) * group_w + 0.05

    fig, ax = plt.subplots(figsize=(max(12, n_pat * 1.8), 5))
    x_centers = np.arange(n_pat) * pat_w

    for pi, pat in enumerate(patterns):
        cx = x_centers[pi]
        tc = true_card[pat]
        gi = 0
        for k in K_VALUES:
            g_start = cx - pat_w / 2 + gi * group_w + 0.01
            si = 0
            for mg in MG_VALUES:
                for ss in SS_VALUES:
                    sub = df[(df["pattern"] == pat) & (df["k"] == k) &
                             (df["mg"] == mg) & (df["ss"] == ss)]
                    est = float(sub["cardinality"].values[0]) if len(sub) else 0.0
                    qe  = qerror(est, tc)
                    bx  = g_start + si * bar_w + bar_w / 2
                    # For Inf true cardinality: mark bar top with "Inf"
                    if tc >= INF_PLACEHOLDER and si == 0:
                        ax.text(bx + (n_sub * bar_w) / 2, max(qe, 1.0) * 1.3,
                                "true=Inf", ha="center", va="bottom",
                                fontsize=6, color="dimgrey", fontstyle="italic")
                    ax.bar(bx, max(qe, 1.0), bar_w,
                           color=K_COLORS[k],
                           hatch=SS_HATCH[ss],
                           alpha=MG_ALPHA[mg],
                           edgecolor="white",
                           linewidth=0.4,
                           zorder=3)
                    si += 1
            gi += 1

    # Reference line at q-error = 1 (perfect estimate)
    ax.axhline(1.0, color="black", linewidth=1.0, linestyle="--", zorder=4)

    ax.set_xticks(x_centers)
    ax.set_xticklabels(patterns, rotation=20, ha="right")
    ax.set_yscale("log")
    ax.set_ylabel("Q-error (log scale,  1 = perfect)")
    ax.set_title(f"Q-error across configs – {title_suffix}")
    ax.grid(axis="y", alpha=0.3, zorder=0)
    ax.set_axisbelow(True)

    k_h  = [mpatches.Patch(color=K_COLORS[k],  label=f"k={k}")  for k in K_VALUES]
    ss_h = [mpatches.Patch(facecolor="grey", hatch=SS_HATCH[ss],
                            edgecolor="white", label=f"ss={ss}") for ss in SS_VALUES]
    mg_h = [mpatches.Patch(facecolor="grey", alpha=MG_ALPHA[mg], label=f"mg={mg}")
            for mg in MG_VALUES]
    from matplotlib.lines import Line2D
    ref_h = [Line2D([0], [0], color="black", linewidth=1.0, linestyle="--", label="q-error=1")]
    ax.legend(handles=k_h + ss_h + mg_h + ref_h, ncol=4, loc="upper right", fontsize=8)

    fig.tight_layout()
    out = output_dir / fname
    fig.savefig(out, bbox_inches="tight")
    fig.savefig(out.with_suffix(".png"), bbox_inches="tight")
    print(f"Saved: {out}")
    plt.close(fig)


fig4_qerror_grouped(base_patterns, "No Predicates",   "sweep_qerror_base.pdf")
fig4_qerror_grouped(pred_patterns, "With Predicates", "sweep_qerror_pred.pdf")

print("\nDone. All figures saved to:", output_dir)
