#!/bin/bash
set -eu
set -o pipefail

sample_size=${1:-200000}

workspace=$(realpath $(dirname $0)/../../)
source "$workspace/scripts/common/build_metrics.sh"
python_bin=${BAYESCARD_PYTHON:-python3}
runner=$workspace/scripts/sql_baselines/run_bayescard.py
dataset=$workspace/datasets/aids_merged_predicates
output_dir=$workspace/result/sql_baselines/aids_merged_predicates_4baselines/bayescard
artifact=$output_dir/bayescard.pkl
log=$workspace/result/sql_baselines/aids_merged_predicates_4baselines/bayescard/build.log
mkdir -p "$output_dir"

run_build_with_metrics "$log" "BayesCard" "aids_merged_predicates" "1" "$artifact" \
    "$python_bin" "$runner" build \
    --data-dir "$dataset" \
    --model-dir "$output_dir" \
    --sample-size "$sample_size"
