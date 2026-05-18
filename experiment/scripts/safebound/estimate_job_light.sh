#!/bin/bash
set -eu
set -o pipefail

workspace=$(realpath "$(dirname "$0")/../../")
source "$workspace/scripts/safebound/env.sh"
safebound_source=$workspace/baseline/SafeBound/Source
runner=$workspace/scripts/safebound/run_safebound.py
patterns=${JOB_LIGHT_SQL_PATTERN_DIR:-$workspace/patterns/sql/job_light}
stats=${JOB_LIGHT_SAFEBOUND_STATS:-$workspace/catalogs/job_light/safebound/job_light.pkl}
output_dir=$workspace/results/safebound/estimate
output=$output_dir/job_light.csv

mkdir -p "$output_dir"

if [[ ! -d "$patterns" ]]; then
  echo "pattern dir not found: $patterns" >&2
  exit 1
fi
if [[ ! -f "$stats" ]]; then
  echo "SafeBound stats not found: $stats" >&2
  echo "Build or set JOB_LIGHT_SAFEBOUND_STATS before running estimation." >&2
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
