#!/bin/bash
set -eu
set -o pipefail

sf=$1
threads=${2:-8}

workspace=$(realpath $(dirname $0)/../../)
source "$workspace/scripts/common/build_metrics.sh"
source "$workspace/scripts/safebound/env.sh"
safebound_source=$workspace/baseline/SafeBound/Source
python_bin=$(resolve_safebound_python)
runner=$workspace/scripts/safebound/run_safebound.py
dataset=$workspace/datasets/ldbc/sf$sf
patterns=$workspace/patterns/sql/lsqb
output_dir=$workspace/catalogs/ldbc/safebound
output=$output_dir/ldbc_sf$sf.pkl
log=$workspace/results/safebound/build-logs/ldbc_sf"$sf".log

mkdir -p "$output_dir"

if ! ls "$safebound_source"/SafeBoundUtils*.so >/dev/null 2>&1; then
    (cd "$safebound_source" && run_safebound_python CythonBuild.py build_ext --inplace)
fi

run_build_with_metrics "$log" "SafeBound" "ldbc_sf$sf" "$threads" "$output" \
    env -u VIRTUAL_ENV -u PYTHONHOME -u PYTHONPATH -u PYTHONUSERBASE \
    -u CONDA_PREFIX -u CONDA_DEFAULT_ENV -u CONDA_SHLVL -u CONDA_PYTHON_EXE \
    PATH="$(dirname "$python_bin"):/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    "$python_bin" "$runner" build \
    --workspace "$workspace" \
    --dataset-dir "$dataset" \
    --pattern-dir "$patterns" \
    --output-stats "$output" \
    --num-cores "$threads"
