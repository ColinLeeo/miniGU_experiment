#!/bin/bash
set -eu
set -o pipefail

sf=${1:-1}

workspace=$(realpath "$(dirname "$0")/../../")
source "$workspace/scripts/safebound/env.sh"
safebound_source=$workspace/baseline/SafeBound/Source
runner=$workspace/scripts/safebound/run_safebound.py
patterns=${GLOGS_SAFEBOUND_PATTERN_DIR:-$workspace/patterns/sql/glogs}
stats=${GLOGS_SAFEBOUND_STATS_PATH:-$workspace/catalogs/ldbc/safebound/ldbc_sf$sf.pkl}
output_dir=$workspace/results/safebound/estimate
output=$output_dir/glogs_sf$sf.csv

mkdir -p "$output_dir"

if [[ ! -d "$patterns" ]]; then
    echo "pattern dir not found: $patterns" >&2
    exit 1
fi
if [[ ! -f "$stats" ]]; then
    echo "stats file not found: $stats" >&2
    echo "Build LDBC SafeBound stats first, or set GLOGS_SAFEBOUND_STATS_PATH." >&2
    exit 1
fi

if ! ls "$safebound_source"/SafeBoundUtils*.so >/dev/null 2>&1; then
    (cd "$safebound_source" && run_safebound_python CythonBuild.py build_ext --inplace)
fi

run_safebound_python "$runner" query \
    --workspace "$workspace" \
    --pattern-dir "$patterns" \
    --stats-path "$stats" \
    --output-csv "$output"
