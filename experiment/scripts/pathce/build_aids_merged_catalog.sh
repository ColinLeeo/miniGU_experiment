#!/bin/bash
set -eu
set -o pipefail

threads=$1
k=$2
d=$3
m=$4

workspace=$(realpath $(dirname $0)/../../)
source "$workspace/scripts/common/build_metrics.sh"
output_dir=$workspace/catalogs/aids_merged/pathce
mkdir -p $output_dir
output=$output_dir/aids_merged_"$k"_"$d"_"$m"
log=$workspace/result/baselines/pathce/build-logs/aids_merged_"$k"_"$d"_"$m".log

run_build_with_metrics "$log" "PathCE" "aids_merged" "$threads" "$output" \
  "$workspace/baseline/pathce/target/release/pathce" analyze -s "$workspace/schemas/aids_merged/aids_merged_pathce_schema.json" -g "$workspace/graphs/aids_merged/pathce/aids_merged.bincode" --greedy -t "$threads" -o "$output" --buckets "$m" --max-path-length "$k" --max-star-degree "$d"
