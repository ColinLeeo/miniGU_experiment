#!/usr/bin/env python3

import argparse
import os
import tempfile
from pathlib import Path

_tmp_cache = Path(tempfile.gettempdir()) / "codex_plot_cache"
os.environ.setdefault("MPLCONFIGDIR", str(_tmp_cache / "mplconfig"))
os.environ.setdefault("XDG_CACHE_HOME", str(_tmp_cache / "xdg"))

import matplotlib.pyplot as plt
import pandas as pd
import seaborn as sns


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Plot insert impact results per pattern without mean aggregation."
    )
    parser.add_argument(
        "--input",
        default="/Users/colin/Downloads/insert_impact_results.csv",
        help="Path to the input CSV file.",
    )
    parser.add_argument(
        "--outdir",
        default="result/insert_impact_plots",
        help="Directory to write generated plots.",
    )
    return parser.parse_args()


def prepare_dataframe(csv_path: Path) -> pd.DataFrame:
    df = pd.read_csv(csv_path)
    df["update_pct_label"] = (df["update_pct"] * 100).map(lambda x: f"{x:.0f}%")
    df["query_time_ms"] = df["query_time_s"] * 1000.0
    df["build_time_ms"] = df["build_time_s"] * 1000.0
    return df.sort_values(["pattern", "query_type", "position", "update_pct", "inserted_edges"])


def make_series_label(row: pd.Series) -> str:
    return f"{row['query_type']} | {row['position']} | {row['edge_label']}"


def save_pattern_plot(pattern_df: pd.DataFrame, outdir: Path) -> None:
    pattern = pattern_df["pattern"].iloc[0]
    plot_df = pattern_df.copy()
    plot_df["series"] = plot_df.apply(make_series_label, axis=1)

    fig, axes = plt.subplots(2, 2, figsize=(14, 9))

    sns.lineplot(
        data=plot_df,
        x="update_pct",
        y="qerror",
        hue="series",
        style="series",
        markers=True,
        dashes=False,
        ax=axes[0, 0],
    )
    axes[0, 0].set_title(f"{pattern}: Q-error vs Update Ratio")
    axes[0, 0].set_xlabel("Update ratio")
    axes[0, 0].set_ylabel("Q-error")
    axes[0, 0].set_yscale("log")

    sns.lineplot(
        data=plot_df,
        x="update_pct",
        y="query_time_ms",
        hue="series",
        style="series",
        markers=True,
        dashes=False,
        legend=False,
        ax=axes[0, 1],
    )
    axes[0, 1].set_title(f"{pattern}: Query Time vs Update Ratio")
    axes[0, 1].set_xlabel("Update ratio")
    axes[0, 1].set_ylabel("Query time (ms)")

    nonzero_df = plot_df[plot_df["inserted_edges"] > 0].copy()
    sns.lineplot(
        data=nonzero_df,
        x="inserted_edges",
        y="qerror",
        hue="series",
        style="series",
        markers=True,
        dashes=False,
        legend=False,
        ax=axes[1, 0],
    )
    axes[1, 0].set_title(f"{pattern}: Q-error vs Inserted Edges")
    axes[1, 0].set_xlabel("Inserted edges")
    axes[1, 0].set_ylabel("Q-error")
    axes[1, 0].set_xscale("log")
    axes[1, 0].set_yscale("log")

    sns.lineplot(
        data=nonzero_df,
        x="inserted_edges",
        y="insert_time_s",
        hue="series",
        style="series",
        markers=True,
        dashes=False,
        legend=False,
        ax=axes[1, 1],
    )
    axes[1, 1].set_title(f"{pattern}: Insert Time vs Inserted Edges")
    axes[1, 1].set_xlabel("Inserted edges")
    axes[1, 1].set_ylabel("Insert time (s)")
    axes[1, 1].set_xscale("log")

    for ax in axes.flat:
        ax.grid(True, alpha=0.3)

    handles, labels = axes[0, 0].get_legend_handles_labels()
    axes[0, 0].legend(handles, labels, title="Series", fontsize=9, title_fontsize=10)

    fig.suptitle(f"Insert Impact for {pattern}", fontsize=15, y=0.98)
    fig.tight_layout()
    fig.savefig(outdir / f"{pattern}_detail.png", dpi=220, bbox_inches="tight")
    plt.close(fig)


def print_pattern_summary(df: pd.DataFrame) -> None:
    print("=== Pattern summary ===")
    cols = [
        "pattern",
        "query_type",
        "position",
        "edge_label",
        "update_pct",
        "inserted_edges",
        "qerror",
        "insert_time_s",
        "query_time_ms",
    ]
    print(df[cols].to_string(index=False))


def main() -> None:
    args = parse_args()
    sns.set_theme(style="whitegrid")

    csv_path = Path(args.input)
    outdir = Path(args.outdir)
    outdir.mkdir(parents=True, exist_ok=True)

    df = prepare_dataframe(csv_path)
    print_pattern_summary(df)

    for _, pattern_df in df.groupby("pattern", sort=True):
        save_pattern_plot(pattern_df, outdir)

    print()
    print(f"Plots written to: {outdir.resolve()}")


if __name__ == "__main__":
    main()
