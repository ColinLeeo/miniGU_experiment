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
stats_dir=$workspace/catalogs/ldbc/safebound
stats=$stats_dir/ldbc_with_pred_30_sf$sf.pkl
output_dir=$workspace/result/baselines/safebound/estimate
output=$output_dir/ldbc_with_pred_30_sf$sf.csv

num_cores=${SAFEBOUND_NUM_CORES:-8}

mkdir -p "$stats_dir" "$output_dir"

if ! ls "$safebound_source"/SafeBoundUtils*.so >/dev/null 2>&1; then
    (cd "$safebound_source" && run_safebound_python CythonBuild.py build_ext --inplace)
fi

if [[ ! -s "$stats" || "${REBUILD_SAFEBOUND:-0}" == "1" ]]; then
    run_safebound_python "$runner" build \
        --workspace "$workspace" \
        --dataset-dir "$dataset" \
        --pattern-dir "$patterns" \
        --output-stats "$stats" \
        --num-cores "$num_cores"
fi

run_safebound_python "$runner" query \
    --workspace "$workspace" \
    --pattern-dir "$patterns" \
    --stats-path "$stats" \
    --output-csv "$output"

echo "Stats: $stats"
echo "Output: $output"
