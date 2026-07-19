#!/bin/bash
set -eu
set -o pipefail

sf=$1

workspace=$(realpath $(dirname $0)/../../)
source "$workspace/scripts/safebound/env.sh"
safebound_source=$workspace/baseline/SafeBound/Source
runner=$workspace/scripts/safebound/run_safebound.py
patterns=$workspace/patterns/sql/lsqb
stats=$workspace/catalogs/ldbc/safebound/ldbc_sf$sf.pkl
output_dir=$workspace/result/baselines/safebound/estimate
output=$output_dir/ldbc_sf$sf.csv

mkdir -p "$output_dir"

if ! ls "$safebound_source"/SafeBoundUtils*.so >/dev/null 2>&1; then
    (cd "$safebound_source" && run_safebound_python CythonBuild.py build_ext --inplace)
fi

run_safebound_python "$runner" query \
    --workspace "$workspace" \
    --pattern-dir "$patterns" \
    --stats-path "$stats" \
    --output-csv "$output"
