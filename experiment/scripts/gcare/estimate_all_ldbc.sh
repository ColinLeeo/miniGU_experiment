#!/bin/bash
set -u
set -o pipefail

method=$1
sf=$2
pattern_dir=$(realpath $3)
mkdir -p $4
output_dir=$(realpath $4)

workspace=$(realpath $(dirname $0)/../../)

graph_dir=$workspace/datasets/ldbc/sf$sf

patterns=$(find $pattern_dir -name '*.json' -type f | sort)
for pattern in $patterns; do
    count_time=$(timeout -v 5m $workspace/scripts/gcare/estimate.sh $method $graph_dir $pattern)
    IFS="," read -r count time <<< "$count_time"
    echo "$pattern: $count, $time"
    filename=$(basename $pattern)
    jq ".count=$count" $pattern > $output_dir/$filename.tmp
    mv $output_dir/$filename.tmp $output_dir/$filename
done