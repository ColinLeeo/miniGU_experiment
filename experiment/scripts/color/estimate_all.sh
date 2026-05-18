#!/bin/bash
set -eu
set -o pipefail

summary=$(realpath $1)
pattern_dir=$(realpath $2)
log_file=$(realpath $3)

workspace=$(realpath "$(dirname "$0")/../../")
: > "$log_file"

patterns=$(find "$pattern_dir" -name '*.gcare' -type f | sort)
for pattern in $patterns; do
    count_time=$("$workspace/scripts/color/estimate.sh" "$summary" "$pattern")
    IFS="," read -r count time <<< "$count_time"
    rel="${pattern#$pattern_dir/}"
    echo "$rel: $count, $time" | tee -a "$log_file"
done