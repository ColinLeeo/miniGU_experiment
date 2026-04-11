#!/bin/bash
set -eu
set -o pipefail

# Estimate the cardinality of a given pattern
# Argument: catalog, pattern 

catalog=$(realpath $1)
pattern=$(realpath $2)

workspace=$(realpath $(dirname $0)/../../)

GLOGS_BIN="${HOME}/glogs_build/ir/target/release"
if [[ ! -x "$GLOGS_BIN/pattern_count" ]]; then
    GLOGS_BIN="$workspace/glogs/ir/target/release"
fi
$GLOGS_BIN/pattern_count -c $catalog -p $pattern