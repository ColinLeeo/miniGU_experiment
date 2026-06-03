#!/bin/bash
#
# start_insert_impact.sh — 在后台启动 run_insert_impact.sh 实验
#
# 用法:
#   ./start_insert_impact.sh <db_path> <dataset_dir>
#
# 退出后实验仍会跑（nohup + disown）。监控命令打印在末尾。

set -euo pipefail

if [ $# -lt 2 ]; then
    echo "用法: $0 <db_path> <dataset_dir>"
    exit 1
fi

DB_PATH="$(cd "$1" && pwd)"
DATASET_DIR="$(cd "$2" && pwd)"

PROJ_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
MINIGU_BIN="${PROJ_ROOT}/target/release/minigu"
RUNNER="${PROJ_ROOT}/experiment/scripts/run_insert_impact.sh"
RESULT_DIR="${PROJ_ROOT}/experiment/result/insert_impact"
PID_FILE="${RESULT_DIR}/run.pid"
RUN_LOG="${RESULT_DIR}/run.log"

# ── 前置检查 ─────────────────────────────────────────────────────────
[ -x "${MINIGU_BIN}" ] || { echo "ERR: minigu 二进制不存在或不可执行: ${MINIGU_BIN}"; exit 1; }
[ -x "${RUNNER}" ]     || { echo "ERR: ${RUNNER} 不可执行 (chmod +x)"; exit 1; }
[ -d "${DB_PATH}" ]    || { echo "ERR: DB 目录不存在: ${DB_PATH}"; exit 1; }
[ -d "${DATASET_DIR}" ] || { echo "ERR: dataset 目录不存在: ${DATASET_DIR}"; exit 1; }

# 已有实例在跑则拒绝启动
if [ -f "${PID_FILE}" ] && kill -0 "$(cat "${PID_FILE}")" 2>/dev/null; then
    echo "ERR: 已有 run_insert_impact.sh 在跑 (PID=$(cat "${PID_FILE}"))"
    echo "     如需重启，先 kill 或删除 ${PID_FILE}"
    exit 1
fi

mkdir -p "${RESULT_DIR}"

# ── 启动 ─────────────────────────────────────────────────────────────
nohup bash "${RUNNER}" "${DB_PATH}" "${DATASET_DIR}" \
    > "${RUN_LOG}" 2>&1 &

PID=$!
echo "$PID" > "${PID_FILE}"
disown

echo "============================================================"
echo ">>> 已后台启动 run_insert_impact.sh"
echo "    PID:       ${PID}"
echo "    DB:        ${DB_PATH}"
echo "    Dataset:   ${DATASET_DIR}"
echo "    Result:    ${RESULT_DIR}"
echo ""
echo "监控:"
echo "  tail -f ${RUN_LOG}                              # 主进度"
echo "  tail -f ${RESULT_DIR}/stdout.log                # minigu 输出 (含抽象图候选)"
echo "  tail -f ${RESULT_DIR}/stderr.log                # 内部计时 + DuckDB 错误"
echo "  ps -p ${PID} && echo alive || echo done/dead   # 进程死活"
echo "  wc -l ${RESULT_DIR}/insert_impact_results.csv  # CSV 实时行数"
echo ""
echo "中止:"
echo "  kill ${PID}"
echo "============================================================"
