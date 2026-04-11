#!/bin/bash
set -eu
set -o pipefail

sf=$1
threads=$2

workspace=$(realpath $(dirname $0)/../../)
output_dir=$workspace/catalogs/ldbc/glogs
mkdir -p $output_dir
output=$output_dir/ldbc_sf$sf.bincode

SCHEMA_PATH=$workspace/schemas/ldbc/ldbc_glogs_schema.json \
SAMPLE_PATH=$workspace/graphs/ldbc/glogs/ldbc_sf$sf \
GLOGS_BIN="${HOME}/glogs_build/ir/target/release"
if [[ ! -x "$GLOGS_BIN/build_catalog" ]]; then
    GLOGS_BIN="$workspace/glogs/ir/target/release"
fi
$GLOGS_BIN/build_catalog -m from_meta -t $threads -p $output