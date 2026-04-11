#!/bin/bash
set -eu
set -o pipefail

summary=$(realpath $1)
pattern=$(realpath $2)

workspace=$(realpath $(dirname $0)/../../)

pattern_name=$(basename $pattern)

uuid=$(uuid)
dir=$workspace/color/color-$pattern_name-$uuid
mkdir $dir

# Transform pattern format
$workspace/tools/pattern2gcare.py -p $pattern -o $dir/pattern.txt

julia --project=$workspace/color $workspace/color/scripts/estimate.jl -q $dir/pattern.txt -s $summary

# Clean up
rm -rf $dir
