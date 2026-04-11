#!/bin/bash
set -eu
set -o pipefail

sf=$1

workspace=$(realpath $(dirname $0)/../../)
dataset=$workspace/datasets/ldbc/sf$sf
schema=$workspace/schemas/ldbc/ldbc_pathce_schema.json
output_dir=$workspace/graphs/ldbc/pathce
mkdir -p $output_dir
output=$output_dir/ldbc_sf$sf.bincode

$workspace/pathce/target/release/pathce serialize -i $dataset -s $schema -o $output
