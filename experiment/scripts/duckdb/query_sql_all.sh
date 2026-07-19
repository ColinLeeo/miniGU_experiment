#!/bin/bash
set -eu
set -o pipefail

# Compute true cardinalities of all SQL files in a directory.
# Arguments: db, sql_dir, output_dir

db=$(realpath "$1")
sql_dir=$(realpath "$2")
mkdir -p "$3"
output_dir=$(realpath "$3")

workspace=$(realpath "$(dirname "$0")/../../")
duckdb=${DUCKDB_BIN:-$workspace/tools/duckdb}
if [[ ! -x "$duckdb" ]]; then
    duckdb=${DUCKDB_BIN:-duckdb}
fi
threads=${DUCKDB_THREADS:-8}
memory_limit=${DUCKDB_MEMORY_LIMIT:-64GB}

queries=$(find "$sql_dir" -name '*.sql' -type f | sort -V)
for query in $queries; do
    filename=$(basename "$query" .sql)
    output="$output_dir/$filename.txt"
    if [[ -s "$output" ]]; then
        echo "$query: skip"
        continue
    fi
    count=$("$duckdb" "$db" -readonly -csv -noheader <<EOF
PRAGMA threads=$threads;
PRAGMA memory_limit='$memory_limit';
$(cat "$query")
EOF
)
    echo "$query: $count"
    printf "%s\n" "$count" > "$output.tmp"
    mv "$output.tmp" "$output"
done
