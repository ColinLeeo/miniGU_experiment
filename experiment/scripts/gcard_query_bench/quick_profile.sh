#!/usr/bin/env bash
# Quick profiling run: only predicated patterns on sf1
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
MINIGU="$PROJECT_DIR/target/release/minigu"
PATTERN_DIR="$PROJECT_DIR/experiment/pattern/LDBC"
DB_PATH="$PROJECT_DIR/experiment/dataset/ldbc/sf1/minigu_db"

SAMPLE_SIZE=500
MAX_K=2

# Only predicated patterns
PATTERNS=(
    "$PATTERN_DIR/L1_path/L1_PA.json"
    "$PATTERN_DIR/L2_chain/L2_PB.json"
    "$PATTERN_DIR/L4_cycle/L4_PA.json"
    "$PATTERN_DIR/L6_cycle_branch/L6_PA.json"
)

TMP_SCRIPT=$(mktemp /tmp/prof_XXXXXX.gql)
cat > "$TMP_SCRIPT" <<EOF
session set graph ldbc
call load_catalog("ldbc")
EOF

for p in "${PATTERNS[@]}"; do
    echo ":time call gcard_query(\"${p}\", ${MAX_K}, ${SAMPLE_SIZE}, 0, false)" >> "$TMP_SCRIPT"
done

echo "=== Running profiling on sf1 ==="
"$MINIGU" execute "$TMP_SCRIPT" --path "$DB_PATH" 2>&1

rm -f "$TMP_SCRIPT"
