#!/bin/bash
set -eu
set -o pipefail

# Build duckdb database from LDBC SNB dataset.
# Argument: sf

sf=$1

workspace=$(realpath "$(dirname "$0")/../../")
duckdb=${DUCKDB_BIN:-$workspace/tools/duckdb}
if [[ ! -x "$duckdb" ]]; then
    duckdb=${DUCKDB_BIN:-duckdb}
fi

mkdir -p "$workspace/graphs/ldbc/duckdb"
cd "$workspace/datasets/ldbc/sf$sf"
rm -f "$workspace/graphs/ldbc/duckdb/ldbc_sf$sf.duckdb"
sql=$(mktemp)
{
    echo "begin;"
    for csv in *.csv; do
        table=${csv%.csv}
        printf "create table \"%s\" as from read_csv('%s');\n" "$table" "$csv"
    done
    echo "commit;"
} > "$sql"
"$duckdb" "$workspace/graphs/ldbc/duckdb/ldbc_sf$sf.duckdb" < "$sql"
rm -f "$sql"
