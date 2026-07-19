#!/bin/bash
set -eu
set -o pipefail

workspace=$(realpath "$(dirname "$0")/../../")
source "$workspace/scripts/common/build_metrics.sh"
graph=$workspace/catalogs/aids_merged/gcare/aids_merged.txt
output_dir=$workspace/catalogs/aids_merged/color
mkdir -p "$output_dir"
output=$output_dir/aids_merged_mix_6_50000.obj
threads=${JULIA_NUM_THREADS:-1}
log=$workspace/result/baselines/color/build-logs/aids_merged.log

julia_bin="${JULIA_BIN:-julia}"
if ! command -v "$julia_bin" >/dev/null 2>&1; then
  julia_bin="$HOME/.local/bin/julia"
fi

run_build_with_metrics "$log" "Color" "aids_merged" "$threads" "$output" \
  "$julia_bin" --project="$workspace/baseline/color" "$workspace/baseline/color/scripts/build.jl" -d "$graph" -o "$output"
