#!/bin/bash
set -eu
set -o pipefail

sf=${1:-1}

workspace=$(realpath "$(dirname "$0")/../../")
source "$workspace/scripts/safebound/env.sh"

safebound_source=$workspace/baseline/SafeBound/Source
runner=$workspace/scripts/safebound/run_safebound.py
dataset=$workspace/datasets/ldbc/sf$sf
patterns=$workspace/patterns/sql/ldbc_with_pred_30
output_dir=$workspace/catalogs/ldbc/safebound
output=$output_dir/ldbc_with_pred_30_sf$sf.pkl
log_dir=$workspace/results/safebound/build-logs
log=$log_dir/ldbc_with_pred_30_sf$sf.log

num_cores=${SAFEBOUND_NUM_CORES:-8}

mkdir -p "$output_dir" "$log_dir"

if ! ls "$safebound_source"/SafeBoundUtils*.so >/dev/null 2>&1; then
    (cd "$safebound_source" && run_safebound_python CythonBuild.py build_ext --inplace)
fi

{
    echo "SafeBound LDBC with-predicate-30 build"
    echo "sf=$sf"
    echo "dataset=$dataset"
    echo "patterns=$patterns"
    echo "output=$output"
    echo "num_cores=$num_cores"
    date
    run_safebound_python "$runner" build \
        --workspace "$workspace" \
        --dataset-dir "$dataset" \
        --pattern-dir "$patterns" \
        --output-stats "$output" \
        --num-cores "$num_cores"
    date
} 2>&1 | tee "$log"

echo "Stats: $output"
echo "Log: $log"
