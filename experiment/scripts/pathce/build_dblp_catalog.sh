#!/bin/bash
set -eu
set -o pipefail

threads=${1:-64}
k=${2:-2}
d=${3:-5}
m=${4:-200}

workspace=$(realpath $(dirname $0)/../../)
source "$workspace/scripts/common/build_metrics.sh"
output_dir=$workspace/catalogs/dblp/pathce
mkdir -p "$output_dir"
output=$output_dir/dblp_"$k"_"$d"_"$m"
log=$workspace/result/baselines/pathce/build-logs/dblp_"$k"_"$d"_"$m".log

run_build_with_metrics "$log" "PathCE" "dblp" "$threads" "$output" \
  "$workspace/baseline/pathce/target/release/pathce" analyze -s "$workspace/schemas/dblp/dblp_pathce_schema.json" -g "$workspace/graphs/dblp/pathce/dblp.bincode" --greedy -t "$threads" -o "$output" --buckets "$m" --max-path-length "$k" --max-star-degree "$d"
