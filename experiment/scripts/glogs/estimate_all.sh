#!/bin/bash
set -eu
set -o pipefail

# Estimate the cardinality of all patterns in a given directory, and write the results into a new directory.
# Argument: catalog, pattern_dir, output_dir 

catalog=$(realpath $1)
pattern_dir=$(realpath $2)
mkdir -p $3
output_dir=$(realpath $3)

workspace=$(realpath $(dirname $0)/../../)

patterns=$(find $pattern_dir -name '*.json' -type f | sort)
for pattern in $patterns; do
    count_time=$($workspace/scripts/glogs/estimate.sh $catalog $pattern)
    IFS="," read -r count time <<< "$count_time"
    echo "$pattern: $count, $time"
    filename=$(basename $pattern)
    jq ".count=$count" $pattern > $output_dir/$filename.tmp
    mv $output_dir/$filename.tmp $output_dir/$filename
done