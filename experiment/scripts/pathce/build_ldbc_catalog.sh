#!/bin/bash
set -eu
set -o pipefail

if [ "$#" -lt 2 ]; then
  echo "Usage: $0 <sf> <threads> [k=2] [degree=5] [buckets=200]" >&2
  exit 1
fi

sf=$1
threads=$2
k=${3:-2}
d=${4:-5}
m=${5:-200}

workspace=$(realpath $(dirname $0)/../../)
output_dir=$workspace/catalogs/ldbc/pathce
mkdir -p $output_dir
output=$output_dir/ldbc_"sf$sf"_"$k"_"$d"_"$m"
log=$workspace/results/pathce/build-logs/ldbc_sf"$sf"_"$k"_"$d"_"$m".log
mkdir -p "$(dirname "$log")"

"$workspace/baseline/pathce/target/release/pathce" analyze \
  -s "$workspace/schemas/ldbc/ldbc_pathce_schema.json" \
  -g "$workspace/graphs/ldbc/pathce/ldbc_sf$sf.bincode" \
  --greedy \
  -t "$threads" \
  -o "$output" \
  --max-path-length "$k" \
  --max-star-degree "$d" \
  --buckets "$m" 2>&1 | tee "$log"
