#!/bin/bash
set -eu
set -o pipefail

sf=${1:-1}
pattern_dir=${2:-}

workspace=$(realpath "$(dirname "$0")/../../")
if [[ -z "$pattern_dir" ]]; then
  pattern_dir=$workspace/patterns/gcare/glogs
fi
pattern_dir=$(realpath "$pattern_dir")

summary=${GLOGS_COLOR_SUMMARY:-$workspace/catalogs/ldbc/color/ldbc_sf"$sf"_mix_6_50000.obj}
log_dir=$workspace/result/baselines/color/estimate
log_file=$log_dir/glogs_sf${sf}.log
mkdir -p "$log_dir"
: > "$log_file"

if [[ ! -f "$summary" ]]; then
  echo "summary file not found: $summary" >&2
  exit 1
fi
if [[ ! -d "$pattern_dir" ]]; then
  echo "pattern dir not found: $pattern_dir" >&2
  exit 1
fi

patterns=$(find "$pattern_dir" -name '*.gcare' -type f | sort -V)
if [[ -z "$patterns" ]]; then
  echo "no .gcare pattern files found under: $pattern_dir" >&2
  exit 1
fi

for pattern in $patterns; do
  rel="${pattern#$pattern_dir/}"
  count_time=$("$workspace/scripts/color/estimate.sh" "$summary" "$pattern")
  IFS="," read -r count time <<< "$count_time"
  echo "$rel: $count, $time" | tee -a "$log_file"
done

echo "Log: $log_file"
