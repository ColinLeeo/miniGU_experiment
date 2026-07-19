#!/bin/bash
set -eu
set -o pipefail

sample_size=${1:-200000}

workspace=$(realpath $(dirname $0)/../../)
source "$workspace/scripts/common/build_metrics.sh"
python_bin=${BAYESCARD_PYTHON:-python3}
runner=$workspace/scripts/sql_baselines/run_bayescard.py
dataset=$workspace/datasets/dblp
output_dir=$workspace/catalogs/dblp/bayescard
artifact=$output_dir/bayescard.pkl
log=$workspace/result/baselines/bayescard/build-logs/dblp.log
mkdir -p "$output_dir"

run_build_with_metrics "$log" "BayesCard" "dblp" "1" "$artifact" \
    "$python_bin" "$runner" build \
    --data-dir "$dataset" \
    --model-dir "$output_dir" \
    --sample-size "$sample_size"
