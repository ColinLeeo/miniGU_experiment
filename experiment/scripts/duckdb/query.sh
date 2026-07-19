#!/bin/bash
set -eu
set -o pipefail

# Compute the true cardinality of a given JSON pattern.
# Arguments: db, pathce_schema, pattern

db=$(realpath "$1")
schema=$(realpath "$2")
pattern=$(realpath "$3")

workspace=$(realpath "$(dirname "$0")/../../")
duckdb=${DUCKDB_BIN:-$workspace/tools/duckdb}
if [[ ! -x "$duckdb" ]]; then
    duckdb=${DUCKDB_BIN:-duckdb}
fi

sql=$("$workspace/tools/pattern2sql.py" -p "$pattern" -s "$schema")
"$duckdb" "$db" -readonly -csv -noheader <<EOF
$sql
EOF
