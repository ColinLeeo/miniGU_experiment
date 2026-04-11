#!/bin/bash
set -eu
set -o pipefail

sf=$1

workspace=$(realpath $(dirname $0)/../../)
graph=$workspace/datasets/ldbc/sf$sf.txt
output_dir=$workspace/catalogs/ldbc/color
mkdir -p $output_dir
output=$output_dir/ldbc_sf"$sf"_mix_6_50000.obj
julia --project=$workspace/color $workspace/color/scripts/build.jl -d $graph -o $output
