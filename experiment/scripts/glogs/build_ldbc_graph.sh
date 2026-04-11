#!/bin/bash
set -eu
set -o pipefail

# Build GLogS graph from LDBC SNB dataset
# Argument: sf 

sf=$1

workspace=$(realpath $(dirname $0)/../../)

storage_schema=$workspace/schemas/dummy_schema.json
schema=$workspace/schemas/ldbc/ldbc_glogs_schema.json
input=$workspace/datasets/ldbc/sf$sf
output=$workspace/graphs/ldbc/glogs/ldbc_sf$sf

GLOGS_BIN="${HOME}/glogs_build/ir/target/release"
if [[ ! -x "$GLOGS_BIN/build_graph" ]]; then
    GLOGS_BIN="$workspace/glogs/ir/target/release"
fi
$GLOGS_BIN/build_graph --schema1 $schema --schema2 $storage_schema -d $input -o $output