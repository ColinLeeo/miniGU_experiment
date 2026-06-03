#!/bin/bash
#
# start_backfill_true_card.sh — 后台运行 backfill_true_card.py
#
# 用法:
#   ./start_backfill_true_card.sh [<result_dir>] [<dataset_dir>]
#
# 默认:
#   result_dir  = experiment/result/insert_impact
#   dataset_dir = experiment/dataset/ldbc/sf1
#
# 中断后重跑同命令即可断点续传（脚本内置 resume）。

set -euo pipefail

PROJ_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

RESULT_DIR="${1:-${PROJ_ROOT}/experiment/result/insert_impact}"
DATASET_DIR="${2:-${PROJ_ROOT}/experiment/dataset/ldbc/sf1}"

# 转绝对路径
RESULT_DIR="$(cd "${RESULT_DIR}" && pwd)"
DATASET_DIR="$(cd "${DATASET_DIR}" && pwd)"

CSV_IN="${RESULT_DIR}/insert_impact_results.csv"
CSV_OUT="${RESULT_DIR}/insert_impact_results_with_truecard.csv"
LOG="${RESULT_DIR}/backfill.log"
PID_FILE="${RESULT_DIR}/backfill.pid"

PATTERN_NP_DIR="${PROJ_ROOT}/experiment/patterns/gcard/lsqb"
PATTERN_P_DIR="${PROJ_ROOT}/experiment/patterns/gcard/lsqb_glogs_with_pred_10/lsqb"
TRUE_CARD_SCRIPT="${PROJ_ROOT}/experiment/scripts/true_cardinality.py"
BACKFILL_SCRIPT="${PROJ_ROOT}/experiment/scripts/backfill_true_card.py"

# ── 前置检查 ─────────────────────────────────────────────────────────
[ -f "${CSV_IN}" ]            || { echo "ERR: 输入 CSV 不存在: ${CSV_IN}"; exit 1; }
[ -d "${DATASET_DIR}" ]       || { echo "ERR: dataset 目录不存在: ${DATASET_DIR}"; exit 1; }
[ -d "${PATTERN_NP_DIR}" ]    || { echo "ERR: ${PATTERN_NP_DIR}"; exit 1; }
[ -d "${PATTERN_P_DIR}" ]     || { echo "ERR: ${PATTERN_P_DIR}"; exit 1; }
[ -f "${TRUE_CARD_SCRIPT}" ]  || { echo "ERR: ${TRUE_CARD_SCRIPT}"; exit 1; }
[ -f "${BACKFILL_SCRIPT}" ]   || { echo "ERR: ${BACKFILL_SCRIPT}"; exit 1; }
python3 -c 'import duckdb' 2>/dev/null || { echo "ERR: duckdb 未安装 (pip install duckdb)"; exit 1; }

# 并行度（可通过环境变量覆盖）：N workers × M DuckDB 线程 ≈ 物理核数
WORKERS="${WORKERS:-4}"
DUCKDB_THREADS_VAR="${DUCKDB_THREADS:-4}"

# 防重复启动
if [ -f "${PID_FILE}" ] && kill -0 "$(cat "${PID_FILE}")" 2>/dev/null; then
    echo "ERR: 已有 backfill 在跑 (PID=$(cat "${PID_FILE}"))"
    echo "     若需重启，先 kill 或删除 ${PID_FILE}"
    exit 1
fi

# ── 启动 ─────────────────────────────────────────────────────────────
DUCKDB_THREADS="${DUCKDB_THREADS_VAR}" \
nohup python3 -u "${BACKFILL_SCRIPT}" \
    --csv "${CSV_IN}" \
    --dataset "${DATASET_DIR}" \
    --result-dir "${RESULT_DIR}" \
    --pattern-np-dir "${PATTERN_NP_DIR}" \
    --pattern-p-dir "${PATTERN_P_DIR}" \
    --true-card-script "${TRUE_CARD_SCRIPT}" \
    --out "${CSV_OUT}" \
    --workers "${WORKERS}" \
    > "${LOG}" 2>&1 &

PID=$!
echo "$PID" > "${PID_FILE}"
disown

echo "============================================================"
echo ">>> 已后台启动 backfill_true_card.py"
echo "    PID:         ${PID}"
echo "    Input CSV:   ${CSV_IN}"
echo "    Dataset:     ${DATASET_DIR}"
echo "    Output CSV:  ${CSV_OUT}"
echo "    Workers:     ${WORKERS}  (×${DUCKDB_THREADS_VAR} threads/DuckDB ≈ $((WORKERS * DUCKDB_THREADS_VAR)) cores)"
echo ""
echo "监控:"
echo "  tail -f ${LOG}                          # 进度（每行一条）"
echo "  wc -l ${CSV_OUT}                        # 已写行数"
echo "  ps -p ${PID} && echo alive || echo done"
echo ""
echo "中止:"
echo "  kill ${PID}"
echo ""
echo "重启（断点续传）:"
echo "  $0"
echo "============================================================"
