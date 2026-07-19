#!/bin/bash
set -eu
set -o pipefail

workspace=$(realpath $(dirname $0)/../../)
source "$workspace/scripts/safebound/env.sh"
safebound_source=$workspace/baseline/SafeBound/Source
runner=$workspace/scripts/safebound/run_safebound.py
patterns=$workspace/patterns/sql/imdb
stats=$workspace/catalogs/imdb/safebound/imdb.pkl
output_dir=$workspace/result/baselines/safebound/estimate
output=$output_dir/imdb.csv

mkdir -p "$output_dir"

if ! ls "$safebound_source"/SafeBoundUtils*.so >/dev/null 2>&1; then
    (cd "$safebound_source" && run_safebound_python CythonBuild.py build_ext --inplace)
fi

run_safebound_python "$runner" query \
    --workspace "$workspace" \
    --pattern-dir "$patterns" \
    --stats-path "$stats" \
    --output-csv "$output"
