#!/bin/bash
set -eu
set -o pipefail

# methods: cs bsk cset impr sumrdf wj jsub

workspace=$(realpath $(dirname $0)/../../)
method=$1
sf=$2
graph=$workspace/datasets/ldbc/sf$sf.txt
graph_dir=$workspace/datasets/ldbc/sf$sf

if [[ $method == "cs" ]] || [[ $method == "bsk" ]]; then
    GCARE_BSK_BUDGET=4096 $workspace/gcare/build/gcare_relation -b -m $method -i $graph -d $graph_dir
else
    $workspace/gcare/build/gcare_graph -b -m $method -i $graph -d $graph_dir
fi
