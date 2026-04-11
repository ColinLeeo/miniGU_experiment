#!/usr/bin/env python3
"""
Plot GCard catalog build benchmark results for paper figures.

Reads: experiment/result/build_catalog/bench_catalog_results.csv
Outputs to: experiment/result/build_catalog/
  - catalog_stat_size.pdf        (Fig 1: raw & compressed stat size vs SF)
  - catalog_compute_time_sf.pdf  (Fig 2: compute time vs SF at 24 threads)
  - catalog_thread_scaling.pdf   (Fig 3: thread scaling on SF1)

Usage:
    python3 experiment/scripts/plot_catalog_bench.py
"""

import os
import sys
import pandas as pd
import matplotlib.pyplot as plt
import matplotlib
import numpy as np

matplotlib.rcParams.update({
    'font.size': 12,
    'axes.labelsize': 13,
    'axes.titlesize': 14,
    'xtick.labelsize': 11,
    'ytick.labelsize': 11,
    'legend.fontsize': 11,
    'figure.dpi': 150,
    'savefig.bbox': 'tight',
    'savefig.pad_inches': 0.1,
})

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
PROJECT_DIR = os.path.join(SCRIPT_DIR, '..', '..', '..')
RESULT_DIR = os.path.join(PROJECT_DIR, 'experiment', 'result', 'build_catalog')
CSV_PATH = os.path.join(RESULT_DIR, 'bench_catalog_results.csv')


def sf_to_num(s):
    return float(s.replace('sf', ''))


def load_data():
    df = pd.read_csv(CSV_PATH)
    df['sf_num'] = df['sf'].apply(sf_to_num)
    df = df.sort_values(['sf_num', 'threads', 'repeat'])
    return df


def plot_stat_size(df):
    """Fig 1: Raw and compressed catalog size vs Scale Factor (grouped bar)."""
    fig, ax = plt.subplots(figsize=(6, 4))

    sizes = df.drop_duplicates('sf').sort_values('sf_num')
    x = np.arange(len(sizes))
    w = 0.35

    raw_gb = sizes['stat_size_mb'].values / 1024
    comp_gb = sizes['compressed_size_mb'].values / 1024

    raw_bars = ax.bar(x - w/2, raw_gb, w,
                      label='Raw', color='#4C72B0', edgecolor='white', linewidth=0.5)
    comp_bars = ax.bar(x + w/2, comp_gb, w,
                       label='Compressed', color='#DD8452', edgecolor='white', linewidth=0.5)

    ax.set_xlabel('LDBC Scale Factor')
    ax.set_ylabel('Size (GB)')
    ax.set_title('Catalog Size')
    ax.set_xticks(x)
    ax.set_xticklabels(sizes['sf'].values)
    ax.legend()
    ax.grid(axis='y', alpha=0.3)

    # Value labels
    for bar in raw_bars:
        h = bar.get_height()
        label = f'{h:.2f}' if h < 1 else f'{h:.1f}'
        ax.text(bar.get_x() + bar.get_width()/2, h + 0.02,
                label, ha='center', va='bottom', fontsize=9)
    for bar in comp_bars:
        h = bar.get_height()
        label = f'{h:.2f}' if h < 1 else f'{h:.1f}'
        ax.text(bar.get_x() + bar.get_width()/2, h + 0.02,
                label, ha='center', va='bottom', fontsize=9)

    out = os.path.join(RESULT_DIR, 'catalog_stat_size.pdf')
    fig.savefig(out)
    plt.close(fig)
    print(f"Saved {out}")


def plot_compute_time_sf(agg):
    """Fig 2: Compute time vs Scale Factor at 24 threads (bar chart)."""
    fig, ax = plt.subplots(figsize=(6, 4))

    t24 = agg[agg['threads'] == 24].sort_values('sf_num')
    x = np.arange(len(t24))

    bars = ax.bar(x, t24['compute_median'],
                  yerr=[t24['compute_median'] - t24['compute_min'],
                        t24['compute_max'] - t24['compute_median']],
                  capsize=4, color='#55A868', edgecolor='white', linewidth=0.5,
                  error_kw={'linewidth': 1.2})

    for i, row in enumerate(t24.itertuples()):
        offset = (row.compute_max - row.compute_median) + 0.3
        ax.text(i, row.compute_median + offset,
                f'{row.compute_median:.2f}s',
                ha='center', va='bottom', fontsize=10, fontweight='bold')

    ax.set_xlabel('LDBC Scale Factor')
    ax.set_ylabel('Compute Time (s)')
    ax.set_title('Degree Sequence Compute Time (24 threads)')
    ax.set_xticks(x)
    ax.set_xticklabels(t24['sf'].values)
    ax.grid(axis='y', alpha=0.3)
    # Leave room for top labels (label sits at compute_max + 0.3, text extends above)
    ymax = (t24['compute_max'].max() + 0.3) * 1.30
    ax.set_ylim(bottom=0, top=ymax)

    out = os.path.join(RESULT_DIR, 'catalog_compute_time_sf.pdf')
    fig.savefig(out)
    plt.close(fig)
    print(f"Saved {out}")


def plot_thread_scaling(agg):
    """Fig 3: Thread scaling — compute time + speedup."""
    fig, (ax_time, ax_speedup) = plt.subplots(1, 2, figsize=(11, 4.5))

    sf1 = agg[agg['sf'] == 'sf1'].sort_values('threads')
    threads = sf1['threads'].values
    t1_median = sf1[sf1['threads'] == 1]['compute_median'].values[0]
    speedup = t1_median / sf1['compute_median'].values

    # --- Left: Compute time ---
    ax_time.errorbar(threads, sf1['compute_median'],
                     yerr=[sf1['compute_median'] - sf1['compute_min'],
                           sf1['compute_max'] - sf1['compute_median']],
                     fmt='o-', color='#4C72B0', capsize=4, linewidth=2, markersize=6)

    # Bandwidth wall line
    bw_time = sf1[sf1['threads'] >= 8]['compute_median'].min()
    ax_time.axhline(y=bw_time, color='#C44E52', linestyle='--', alpha=0.6, linewidth=1)

    ax_time.set_xlabel('Number of Threads')
    ax_time.set_ylabel('Compute Time (s)')
    ax_time.set_title('Compute Time vs Threads')
    ax_time.set_xticks(threads)
    ax_time.grid(alpha=0.3)

    # --- Right: Speedup ---
    ax_speedup.plot(threads, speedup, 'o-', color='#DD8452', linewidth=2, markersize=6,
                    label='Measured', zorder=3)
    ax_speedup.plot([1, threads[-1]], [1, threads[-1]], '--', color='gray', alpha=0.4,
                    linewidth=1, label='Ideal linear')

    # Fill the gap between ideal and measured
    ax_speedup.fill_between(threads, speedup, threads, alpha=0.08, color='gray')

    ax_speedup.set_xlabel('Number of Threads')
    ax_speedup.set_ylabel('Speedup (T$_1$ / T$_n$)')
    ax_speedup.set_title('Parallel Speedup')
    ax_speedup.set_xticks(threads)
    ax_speedup.legend(loc='upper left')
    ax_speedup.grid(alpha=0.3)

    # Annotate key points
    for t, s in zip(threads, speedup):
        if t in [1, 4, 8, 12, 24]:
            ax_speedup.annotate(f'{s:.1f}x', (t, s), textcoords="offset points",
                                xytext=(8, -4 if t > 4 else 8), fontsize=9,
                                fontweight='bold', color='#DD8452')

    fig.tight_layout(w_pad=3)
    out = os.path.join(RESULT_DIR, 'catalog_thread_scaling.pdf')
    fig.savefig(out)
    plt.close(fig)
    print(f"Saved {out}")


def main():
    df = load_data()
    print(f"Loaded {len(df)} rows: SFs={sorted(df['sf'].unique(), key=sf_to_num)}, "
          f"threads={sorted(df['threads'].unique())}")

    # Aggregate: median/min/max compute time per (sf, threads)
    agg = df.groupby(['sf', 'sf_num', 'threads']).agg(
        compute_median=('compute_time_s', 'median'),
        compute_min=('compute_time_s', 'min'),
        compute_max=('compute_time_s', 'max'),
        stat_size=('stat_size_mb', 'first'),
        compressed_size=('compressed_size_mb', 'first'),
    ).reset_index().sort_values(['sf_num', 'threads'])

#     plot_stat_size(df)
#     plot_compute_time_sf(agg)
    plot_thread_scaling(agg)

    print(f"\nAll figures saved to {RESULT_DIR}")


if __name__ == '__main__':
    main()
