#!/usr/bin/env python3
"""
Plot q-error bar charts from comparison CSV.

Usage:
    python plot_qerror.py [csv_path] [output_dir]

Defaults:
    csv_path:   experiment/result/comparison/comparison_sf1.csv
    output_dir: experiment/result/comparison/
"""
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
PROJECT_DIR = SCRIPT_DIR.parent.parent
DEFAULT_CSV = PROJECT_DIR / "experiment/result/comparison/comparison_sf1.csv"
DEFAULT_OUT = PROJECT_DIR / "experiment/result/comparison"

csv_path = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_CSV
output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else DEFAULT_OUT
output_dir.mkdir(parents=True, exist_ok=True)

df = pd.read_csv(csv_path)

# Separate predicate vs non-predicate patterns
df_tools = df[df["tool"] != "truth"].copy()
df_tools["has_pred"] = df_tools["pattern"].str.contains("_P")

df_base = df_tools[~df_tools["has_pred"]]
df_pred = df_tools[df_tools["has_pred"]]

# Tool display config
TOOL_COLORS = {
    "gcard":    "#E74C3C",
    "pathce":   "#3498DB",
    "gcare-wj": "#2ECC71",
    "color":    "#F39C12",
}
TOOL_ORDER = ["gcard", "pathce", "gcare-wj", "color"]

INF_CAP = 1e8  # Cap for inf values on the plot


def plot_qerror(ax, data, tools, title):
    """Plot grouped bar chart of q-error (log scale)."""
    patterns = data["pattern"].unique()
    patterns = sorted(patterns, key=lambda x: (int(x.split("_")[0][1:]), x))
    n_patterns = len(patterns)
    n_tools = len(tools)

    bar_width = 0.8 / n_tools
    x = np.arange(n_patterns)

    for i, tool in enumerate(tools):
        tool_data = data[data["tool"] == tool]
        qerrors = []
        is_inf = []
        for pat in patterns:
            row = tool_data[tool_data["pattern"] == pat]
            if len(row) == 0:
                qerrors.append(0)
                is_inf.append(False)
            else:
                val = row.iloc[0]["qerror"]
                if val == float("inf") or val == np.inf or str(val).strip() == "inf":
                    qerrors.append(INF_CAP)
                    is_inf.append(True)
                else:
                    qerrors.append(float(val))
                    is_inf.append(False)

        positions = x + (i - n_tools / 2 + 0.5) * bar_width
        bars = ax.bar(positions, qerrors, bar_width,
                      label=tool, color=TOOL_COLORS.get(tool, "#999"),
                      edgecolor="white", linewidth=0.5, zorder=3)

        # Add value labels and inf markers
        for j, (bar, q, inf_flag) in enumerate(zip(bars, qerrors, is_inf)):
            if q == 0:
                continue
            if inf_flag:
                ax.text(bar.get_x() + bar.get_width() / 2, bar.get_height(),
                        "∞", ha="center", va="bottom", fontsize=12,
                        fontweight="bold", color=TOOL_COLORS.get(tool, "#999"))
            elif q > 100:
                ax.text(bar.get_x() + bar.get_width() / 2, bar.get_height(),
                        f"{q:.0f}", ha="center", va="bottom", fontsize=7,
                        rotation=45)

    ax.set_yscale("log")
    ax.set_ylabel("Q-error (log scale)")
    ax.set_xticks(x)
    ax.set_xticklabels(patterns)
    ax.set_xlabel("Query Pattern")
    ax.set_title(title)
    ax.axhline(y=1, color="gray", linestyle="--", linewidth=0.8, zorder=1)
    ax.legend(loc="upper left")
    ax.grid(axis="y", alpha=0.3, zorder=0)
    ax.set_axisbelow(True)


# ── Figure 1: Non-predicate patterns ──────────────────────────────────────
fig1, ax1 = plt.subplots(figsize=(10, 5))
base_tools = [t for t in TOOL_ORDER if t in df_base["tool"].unique()]
plot_qerror(ax1, df_base, base_tools,
            "Q-error Comparison (Without Predicates, SF1)")
fig1.tight_layout()
out1 = output_dir / "qerror_no_predicate_sf1.pdf"
fig1.savefig(out1, bbox_inches="tight")
fig1.savefig(out1.with_suffix(".png"), bbox_inches="tight")
print(f"Saved: {out1}")

# ── Figure 2: Predicate patterns ─────────────────────────────────────────
fig2, ax2 = plt.subplots(figsize=(8, 5))
pred_tools = [t for t in TOOL_ORDER if t in df_pred["tool"].unique()]
plot_qerror(ax2, df_pred, pred_tools,
            "Q-error Comparison (With Predicates, SF1)")
fig2.tight_layout()
out2 = output_dir / "qerror_predicate_sf1.pdf"
fig2.savefig(out2, bbox_inches="tight")
fig2.savefig(out2.with_suffix(".png"), bbox_inches="tight")
print(f"Saved: {out2}")

plt.show()
