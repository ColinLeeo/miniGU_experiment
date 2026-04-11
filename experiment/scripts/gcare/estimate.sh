#!/bin/bash
set -eu
set -o pipefail

method=$1
graph_dir=$(realpath $2)
pattern=$(realpath $3)

workspace=$(realpath $(dirname $0)/../../)

pattern_name=$(basename $pattern)

uuid=$(uuid)
dir=$workspace/gcare/gcare-$method-$pattern_name-$uuid
mkdir $dir

# Transform pattern format
$workspace/tools/pattern2gcare.py -p $pattern -o $dir/pattern.txt

if [[ $method == "cs" ]] || [[ $method == "bsk" ]]; then
    GCARE_BSK_BUDGET=4096 $workspace/gcare/build/gcare_relation -q -m $method -n 1 -i $dir/pattern.txt -d $graph_dir
else
    $workspace/gcare/build/gcare_graph -q -m $method -n 1 -i $dir/pattern.txt -d $graph_dir
fi

# Clean up
rm -rf $dir
