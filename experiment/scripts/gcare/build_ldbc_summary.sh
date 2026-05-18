#!/bin/bash
set -eu
set -o pipefail

# methods: cs bsk cset impr sumrdf wj jsub

workspace=$(realpath $(dirname $0)/../../)
source "$workspace/scripts/common/build_metrics.sh"
method=$1
sf=${2:-1}
graph=$workspace/datasets/ldbc/sf$sf/ldbc_sf${sf}_gcare.txt
graph_dir=$workspace/datasets/ldbc/sf$sf
threads=1
log=$workspace/results/gcare/build-logs/ldbc_sf"$sf"_"$method".log
if [[ $method == "bsk" ]]; then
    summary_path=$graph_dir.$method.b4096.s0
else
    summary_path=$graph_dir.$method.p0.03.s0
fi

if [[ $method == "cs" ]] || [[ $method == "bsk" ]]; then
    run_build_with_metrics "$log" "GCare-$method" "ldbc_sf$sf" "$threads" "$summary_path" \
        env GCARE_BSK_BUDGET=4096 "$workspace/baseline/gcare/build/gcare_relation" -b -m "$method" -i "$graph" -d "$graph_dir"
else
    run_build_with_metrics "$log" "GCare-$method" "ldbc_sf$sf" "$threads" "$summary_path" \
        "$workspace/baseline/gcare/build/gcare_graph" -b -m "$method" -i "$graph" -d "$graph_dir"
fi
