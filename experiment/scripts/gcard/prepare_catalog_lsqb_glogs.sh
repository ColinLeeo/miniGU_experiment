#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
MINIGU="${MINIGU_BIN:-${PROJECT_ROOT}/target/release/minigu}"
DB_PATH="${GCARD_LDBC_DB_PATH:-${PROJECT_ROOT}/experiment/run_tmp/gcard_master_k2_arm1_d5_current_catalogs_retest/dbs/ldbc}"
GRAPH="${GCARD_LDBC_GRAPH:-ldbc_sf1_retest_k2_arm1_d5_after_offset_fix}"
K="${1:-2}"
THREADS="${2:-32}"
SCAN_THREADS="${3:-$THREADS}"

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

cat >"$tmp" <<EOF
session set graph ${GRAPH}
call create_catalog("${GRAPH}", ${K}, ${THREADS}, ${SCAN_THREADS})
:quit
EOF

"$MINIGU" execute "$tmp" --path "$DB_PATH"
