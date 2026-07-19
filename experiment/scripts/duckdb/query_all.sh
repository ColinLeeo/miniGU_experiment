#!/bin/bash
set -eu
set -o pipefail

# Compute true cardinalities of all JSON patterns in a directory.
# Arguments: db, pathce_schema, pattern_dir, output_dir

db=$(realpath "$1")
schema=$(realpath "$2")
pattern_dir=$(realpath "$3")
mkdir -p "$4"
output_dir=$(realpath "$4")

workspace=$(realpath "$(dirname "$0")/../../")

patterns=$(find "$pattern_dir" -name '*.json' -type f | sort)
for pattern in $patterns; do
    count=$("$workspace/scripts/duckdb/query.sh" "$db" "$schema" "$pattern")
    echo "$pattern: $count"
    filename=$(basename "$pattern")
    jq ".count=$count" "$pattern" > "$output_dir/$filename.tmp"
    mv "$output_dir/$filename.tmp" "$output_dir/$filename"
done
