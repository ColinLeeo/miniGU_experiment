#!/bin/bash
set -eu
set -o pipefail

pattern_dir=${1:-}

workspace=$(realpath "$(dirname "$0")/../../")
if [[ -z "$pattern_dir" ]]; then
  pattern_dir=$workspace/patterns/gcare/aids_merged
fi
pattern_dir=$(realpath "$pattern_dir")

summary=$workspace/catalogs/aids_merged/color/aids_merged_mix_6_50000.obj
log_dir=$workspace/result/color/estimate
log_file=$log_dir/aids_merged.log
mkdir -p "$log_dir"
touch "$log_file"

patterns=$(find "$pattern_dir" -name '*.gcare' -type f | sort)
for pattern in $patterns; do
    rel="${pattern#$pattern_dir/}"
    if grep -Fq "$rel:" "$log_file"; then
        continue
    fi
    count_time=$("$workspace/scripts/color/estimate.sh" "$summary" "$pattern")
    IFS="," read -r count time <<< "$count_time"
    echo "$rel: $count, $time" | tee -a "$log_file"
done

echo "Log: $log_file"
