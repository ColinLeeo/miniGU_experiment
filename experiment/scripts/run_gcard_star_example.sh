#!/usr/bin/env bash
set -euo pipefail

# Demonstrates how to enable GCard star statistics at catalog build time and
# how to enable star matching at query time.
#
# Usage:
#   experiment/scripts/run_gcard_star_example.sh DB_PATH GRAPH QUERY_JSON [K] [STAR_ARM_LEN] [STAR_DEGREE] [THREADS] [BUCKETS]
#
# Example:
#   experiment/scripts/run_gcard_star_example.sh \
#     experiment/run_tmp/my_db \
#     my_graph \
#     experiment/pattern_gcard/imdb/q1.json \
#     2 1 5 32 500

if [[ $# -lt 3 || $# -gt 8 ]]; then
  echo "usage: $0 DB_PATH GRAPH QUERY_JSON [K] [STAR_ARM_LEN] [STAR_DEGREE] [THREADS] [BUCKETS]" >&2
  exit 2
fi

DB_PATH="$1"
GRAPH="$2"
QUERY_JSON="$3"
K="${4:-2}"
STAR_ARM_LEN="${5:-1}"
STAR_DEGREE="${6:-5}"
THREADS="${7:-32}"
BUCKETS="${8:-500}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
MINIGU="${PROJECT_ROOT}/target/release/minigu"
TMP_DIR="${PROJECT_ROOT}/experiment/run_tmp/gcard_star_example"

mkdir -p "${TMP_DIR}"

BUILD_GQL="${TMP_DIR}/build_star_catalog.gql"
QUERY_GQL="${TMP_DIR}/query_with_star.gql"

cat >"${BUILD_GQL}" <<EOF
session set graph ${GRAPH}
call create_catalog("${GRAPH}", ${K}, ${THREADS})
:quit
EOF

cat >"${QUERY_GQL}" <<EOF
session set graph ${GRAPH}
call set_gcard_star_config(${STAR_ARM_LEN}, ${STAR_DEGREE})
:time call gcard_query("${QUERY_JSON}", ${K}, ${BUCKETS}, 2, false, 5)
:quit
EOF

echo "[1/2] building catalog with star stats: arm_len=${STAR_ARM_LEN}, max_degree=${STAR_DEGREE}"
GCARD_MAX_STAR_LENGTH="${STAR_ARM_LEN}" \
GCARD_MAX_STAR_DEGREE="${STAR_DEGREE}" \
"${MINIGU}" execute --path "${DB_PATH}" "${BUILD_GQL}"

echo "[2/2] querying with star matching enabled"
"${MINIGU}" execute --path "${DB_PATH}" "${QUERY_GQL}"
