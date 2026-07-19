#!/bin/bash
set -eu
set -o pipefail

# methods: cs bsk cset impr sumrdf wj jsub

workspace=$(realpath $(dirname $0)/../../)
source "$workspace/scripts/common/build_metrics.sh"
dataset=$workspace/datasets/imdb/imdb
schema=$workspace/schemas/imdb/imdb_pathce_schema.json
graph=$workspace/catalogs/imdb/gcare/imdb.txt
graph_dir=$workspace/catalogs/imdb/gcare/imdb
mkdir -p "$(dirname "$graph_dir")"
if [[ ! -f "$graph" ]]; then
    python3 "$workspace/tools/convert_csv_to_gcare.py" -s "$schema" -d "$dataset" -o "$graph"
fi
method=$1
threads=1
log=$workspace/result/baselines/gcare/build-logs/imdb_"$method".log
if [[ $method == "bsk" ]]; then
    summary_path=$graph_dir.$method.b4096.s0
else
    summary_path=$graph_dir.$method.p0.03.s0
fi

if [[ $method == "cs" ]] || [[ $method == "bsk" ]]; then
    run_build_with_metrics "$log" "GCare-$method" "imdb" "$threads" "$summary_path" \
        env GCARE_BSK_BUDGET=4096 "$workspace/baseline/gcare/build/gcare_relation" -b -m "$method" -i "$graph" -d "$graph_dir"
else
    run_build_with_metrics "$log" "GCare-$method" "imdb" "$threads" "$summary_path" \
        "$workspace/baseline/gcare/build/gcare_graph" -b -m "$method" -i "$graph" -d "$graph_dir"
fi
