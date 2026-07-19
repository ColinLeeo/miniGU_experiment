#!/bin/bash
set -eu
set -o pipefail

workspace=$(realpath $(dirname $0)/../../)
dataset=$workspace/datasets/dblp_pathce
schema=$workspace/schemas/dblp/dblp_pathce_schema.json
output_dir=$workspace/graphs/dblp/pathce
mkdir -p "$output_dir"
output=$output_dir/dblp.bincode

$workspace/baseline/pathce/target/release/pathce serialize -i "$dataset" -s "$schema" -o "$output"
