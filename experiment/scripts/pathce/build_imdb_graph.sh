#!/bin/bash
set -eu
set -o pipefail

workspace=$(realpath $(dirname $0)/../../)
dataset=$workspace/datasets/imdb/imdb
schema=$workspace/schemas/imdb/imdb_pathce_schema.json
output_dir=$workspace/graphs/imdb/pathce
mkdir -p $output_dir
output=$output_dir/imdb.bincode

$workspace/baseline/pathce/target/release/pathce serialize -i $dataset -s $schema -o $output
