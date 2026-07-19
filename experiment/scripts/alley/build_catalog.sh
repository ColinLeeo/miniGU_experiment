#!/bin/bash
set -eu
set -o pipefail

dataset=${1:?usage: build_catalog.sh <dataset> [method=alley] [ratio=0.001] [seed=0]}
method=${2:-alley}
ratio=${3:-0.001}
seed=${4:-0}

workspace=$(realpath "$(dirname "$0")/../../")
source "$workspace/scripts/common/build_metrics.sh"

eval "$("$workspace/scripts/alley/alley_utils.py" prepare --dataset "$dataset")"

bin="$workspace/baseline/alley-main/build/gcare_inter"
if [[ ! -x "$bin" ]]; then
  mkdir -p "$workspace/baseline/alley-main/build"
  cmake -S "$workspace/baseline/alley-main" -B "$workspace/baseline/alley-main/build"
  cmake --build "$workspace/baseline/alley-main/build" --target gcare_inter -j
fi

mkdir -p "$(dirname "$ALLEY_DATA_HEADER")" "$workspace/result/baselines/alley/build-logs"
log="$workspace/result/baselines/alley/build-logs/${ALLEY_DATASET}_${method}_p${ratio}_s${seed}.log"
summary_path="${ALLEY_DATA_HEADER}.${method}.p${ratio}.s${seed}"

run_build_with_metrics "$log" "Alley-$method" "$ALLEY_DATASET" 1 "$summary_path" \
  "$bin" -b -m "$method" -i "$ALLEY_GRAPH_TEXT" -d "$ALLEY_DATA_HEADER" -p "$ratio" -s "$seed" -o "$log.raw"

echo "Catalog prefix: $summary_path"
echo "Build log: $log"
