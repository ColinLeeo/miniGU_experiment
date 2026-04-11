#!/usr/bin/env python3
"""
Merge graph baseline and SQL baseline results into a unified CSV.

Usage:
    python merge_results.py <sf>
    python merge_results.py 1

Merges:
    experiment/result/comparison/comparison_sf{SF}.csv       (graph baselines)
    experiment/result/comparison/comparison_sql_sf{SF}.csv   (SQL baselines)
  ->
    experiment/result/comparison/comparison_all_sf{SF}.csv   (merged)
"""
import sys
import os
import pandas as pd

def main():
    if len(sys.argv) < 2:
        print(__doc__.strip())
        sys.exit(1)

    sf = sys.argv[1]
    result_dir = os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "..", "result", "comparison"
    )

    graph_csv = os.path.join(result_dir, f"comparison_sf{sf}.csv")
    sql_csv = os.path.join(result_dir, f"comparison_sql_sf{sf}.csv")
    output_csv = os.path.join(result_dir, f"comparison_all_sf{sf}.csv")

    dfs = []

    if os.path.exists(graph_csv):
        df = pd.read_csv(graph_csv)
        dfs.append(df)
        print(f"Graph baselines: {len(df)} rows from {graph_csv}")
    else:
        print(f"WARN: {graph_csv} not found")

    if os.path.exists(sql_csv):
        df = pd.read_csv(sql_csv)
        dfs.append(df)
        print(f"SQL baselines: {len(df)} rows from {sql_csv}")
    else:
        print(f"WARN: {sql_csv} not found")

    if not dfs:
        print("ERROR: no result files found")
        sys.exit(1)

    merged = pd.concat(dfs, ignore_index=True)
    merged.to_csv(output_csv, index=False)
    print(f"Merged: {len(merged)} rows -> {output_csv}")

    # Summary
    print("\nTools found:")
    for tool, count in merged.groupby("tool").size().items():
        print(f"  {tool}: {count} entries")


if __name__ == "__main__":
    main()
