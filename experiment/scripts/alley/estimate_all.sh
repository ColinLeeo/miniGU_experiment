#!/bin/bash
set -eu
set -o pipefail

dataset=${1:?usage: estimate_all.sh <dataset> [method=alley] [ratio=0.001] [repeat=30] [seed=0]}
method=${2:-alley}
ratio=${3:-0.001}
repeat=${4:-30}
seed=${5:-0}

workspace=$(realpath "$(dirname "$0")/../../")

eval "$("$workspace/scripts/alley/alley_utils.py" prepare --dataset "$dataset")"

bin="$workspace/baseline/alley-main/build/gcare_inter"
if [[ ! -x "$bin" ]]; then
  "$workspace/scripts/alley/build_catalog.sh" "$dataset" "$method" "$ratio" "$seed"
fi

catalog="${ALLEY_DATA_HEADER}.${method}.p${ratio}.s${seed}"
if [[ ! -e "$catalog" && ! -d "$catalog" ]]; then
  "$workspace/scripts/alley/build_catalog.sh" "$dataset" "$method" "$ratio" "$seed"
fi

out_dir="$workspace/result/baselines/alley/estimate"
mkdir -p "$out_dir"
raw="$out_dir/${ALLEY_DATASET}_${method}_p${ratio}_s${seed}.txt"
log="$out_dir/${ALLEY_DATASET}_${method}_p${ratio}_s${seed}.log"
csv="$out_dir/${ALLEY_DATASET}_${method}_p${ratio}_s${seed}.csv"

set +e
"$bin" -q -m "$method" -i "$ALLEY_QUERY_DIR" -d "$ALLEY_DATA_HEADER" -p "$ratio" -n "$repeat" -s "$seed" -o "$raw" > "$log" 2>&1
status=$?
set -e

"$workspace/scripts/alley/alley_utils.py" parse-estimates \
  --input "$raw" \
  --output "$csv" \
  --query-dir "$ALLEY_QUERY_DIR" \
  --dataset "$ALLEY_DATASET" \
  --method "$method"

echo "Raw output: $raw"
echo "Detail CSV: $csv"
echo "Summary CSV: ${csv%.csv}_summary.csv"
echo "Log: $log"
exit "$status"
