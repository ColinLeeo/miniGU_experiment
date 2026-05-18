#!/bin/bash
set -eu
set -o pipefail

workspace=$(realpath $(dirname $0)/../../)
dataset=$workspace/datasets/aids_merged
schema=$workspace/schemas/aids_merged/aids_merged_pathce_schema.json
output_dir=$workspace/graphs/aids_merged/pathce
mkdir -p $output_dir
output=$output_dir/aids_merged.bincode

$workspace/baseline/pathce/target/release/pathce serialize -i $dataset -s $schema -o $output
