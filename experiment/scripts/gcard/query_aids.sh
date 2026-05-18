#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
MINIGU="${MINIGU_BIN:-${PROJECT_ROOT}/target/release/minigu}"
DB_PATH="${GCARD_AIDS_DB_PATH:-${PROJECT_ROOT}/experiment/run_tmp/gcard_master_k2_arm1_d5_current_catalogs_retest/dbs/aids}"
GRAPH="${GCARD_AIDS_GRAPH:-aids_merged_k2_arm1_d5_after_offset_fix}"
K="${1:-2}"
SAMPLE_SIZE="${2:-500}"
MAX_GRAPH="${3:-5}"
OUT_DIR="${GCARD_OUT_DIR:-${PROJECT_ROOT}/experiment/result/gcard_query}"
PATTERN_DIR="${PROJECT_ROOT}/experiment/patterns/gcard/aids_merged"

mkdir -p "$OUT_DIR"
out="$OUT_DIR/aids_k${K}_ss${SAMPLE_SIZE}.csv"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

cat >"$tmp" <<EOF
session set graph ${GRAPH}
call load_catalog("${GRAPH}")
EOF

find "$PATTERN_DIR" -name '*.json' -type f | sort -V |
while IFS= read -r pattern; do
  echo ":time call gcard_query(\"${pattern}\", ${K}, ${SAMPLE_SIZE}, 0, false, ${MAX_GRAPH})" >>"$tmp"
done
echo ":quit" >>"$tmp"

"$MINIGU" execute "$tmp" --path "$DB_PATH" | tee "$out"
echo "Log: $out"
