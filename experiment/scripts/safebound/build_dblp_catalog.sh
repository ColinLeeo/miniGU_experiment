#!/bin/bash
set -eu
set -o pipefail

threads=${1:-8}

workspace=$(realpath $(dirname $0)/../../)
source "$workspace/scripts/common/build_metrics.sh"
source "$workspace/scripts/safebound/env.sh"
safebound_source=$workspace/baseline/SafeBound/Source
python_bin=$(resolve_safebound_python)
runner=$workspace/scripts/safebound/run_safebound.py
dataset=$workspace/datasets/dblp
patterns=$workspace/patterns/sql/dblp_partitioned_scheme_b
output_dir=$workspace/catalogs/dblp/safebound
output=$output_dir/dblp_partitioned.pkl
log=$workspace/result/baselines/safebound/build-logs/dblp_partitioned.log

mkdir -p "$output_dir"
"$workspace/scripts/safebound/prepare_dblp_scheme_b.py" --force

if ! ls "$safebound_source"/SafeBoundUtils*.so >/dev/null 2>&1; then
    (cd "$safebound_source" && run_safebound_python CythonBuild.py build_ext --inplace)
fi

run_build_with_metrics "$log" "SafeBound" "dblp" "$threads" "$output" \
    env -u VIRTUAL_ENV -u PYTHONHOME -u PYTHONPATH -u PYTHONUSERBASE \
    -u CONDA_PREFIX -u CONDA_DEFAULT_ENV -u CONDA_SHLVL -u CONDA_PYTHON_EXE \
    PATH="$(dirname "$python_bin"):/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    "$python_bin" "$runner" build \
    --workspace "$workspace" \
    --dataset-dir "$dataset" \
    --pattern-dir "$patterns" \
    --output-stats "$output" \
    --num-cores "$threads" \
    --dblp-partitioned-fk
